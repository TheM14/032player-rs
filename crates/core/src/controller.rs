//! The core loop: owns all client state, runs on tokio, takes [`Command`]s
//! from the UI and [`EngineEvent`]s from the audio engine, and pushes
//! [`Event`]s to a sink. The UI never blocks on the network.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use api::{
    AlbumListQuery, AlbumSort, AuthError, Capabilities, Client, Feature, SortOrder, Track,
    TrackListQuery, TrackSort,
};
use session::{AdapterStatus, AdapterTrack};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::engine::{AudioDevice, Engine, EngineEnd, EngineEvent};
use crate::format;
use crate::images::{Images, Rgba};
use crate::queue::Queue;
use crate::reporter::{Reporter, ReporterMsg};
use crate::secrets::{SecretStore, TokenKind, token_key};
use crate::settings::Settings;
use crate::storage::FileStorage;
use session::KeyValueStorage;

/// "Previous" restarts the track instead after this much playback.
const RESTART_THRESHOLD_MS: i64 = 10_000;
/// Position updates to the UI and the reporter are coarsened to this step.
const POSITION_STEP_MS: i64 = 250;
const PAGE: u32 = api::MAX_PAGE_SIZE;

/// Settings changes are written this long after the last one (volume drags).
const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(500);

/// Key of the last signed-in server in `session.json`.
const SERVER_KEY: &str = "server";

#[derive(Clone)]
pub struct CoreConfig {
    /// Where the playback resume snapshot and `session.json` live.
    pub data_dir: PathBuf,
    /// API token and media token per server.
    pub secrets: Arc<dyn SecretStore>,
    /// Loaded by the app with [`Settings::load`] (the engine is created
    /// with them); the core saves changes to `settings.json`.
    pub settings: Settings,
}

#[derive(Debug)]
pub enum Command {
    /// Announce the saved sign-in ([`Event::SavedLogin`]) and sign in with
    /// it when both tokens are saved. Sent once at startup.
    Restore,
    /// Sign in; an empty token falls back to the one saved for the server.
    /// On success the server and both tokens are saved.
    Login {
        server_url: String,
        api_token: String,
        media_token: String,
    },
    /// Sign out and forget both saved tokens (the server stays prefilled).
    Logout,
    /// Play the loaded track list from `index`.
    PlayTracks {
        index: usize,
    },
    PlayAlbum {
        album_id: i64,
    },
    TogglePause,
    Next,
    Previous,
    Seek {
        position_ms: i64,
    },
    SetVolume(f64),
    /// Answered with [`Event::AudioDevices`].
    RefreshAudioDevices,
    /// `None` = system default. Answered with [`Event::AudioDevice`].
    SetAudioDevice(Option<String>),
    /// Answered with [`Event::Exclusive`] (the old value on failure).
    SetExclusive(bool),
    SetShowLyrics(bool),
    /// Load artwork `path` for display at `px` physical pixels (long side);
    /// answered with [`Event::Image`] carrying `key`.
    LoadImage {
        key: String,
        path: String,
        px: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginFailure {
    CredentialsRequired,
    TokensMustDiffer,
    ApiTokenInvalid,
    UnsupportedRevision(i64),
    MediaTokenInvalid,
    InvalidUrl,
    Other(String),
}

impl From<AuthError> for LoginFailure {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::CredentialsRequired => Self::CredentialsRequired,
            AuthError::TokensMustDiffer => Self::TokensMustDiffer,
            AuthError::ApiTokenInvalid => Self::ApiTokenInvalid,
            AuthError::UnsupportedRevision(r) => Self::UnsupportedRevision(r),
            AuthError::MediaTokenInvalid => Self::MediaTokenInvalid,
            AuthError::Api(api::ApiError::Url(_)) => Self::InvalidUrl,
            AuthError::Api(e) => Self::Other(e.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumItem {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub year: i32,
    pub artwork: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackItem {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub track_id: i64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork: String,
    pub duration_ms: i64,
    pub quality: Option<String>,
}

/// What the sign-in form can reuse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedLogin {
    /// Last signed-in server; empty when none.
    pub server_url: String,
    pub has_api_token: bool,
    pub has_media_token: bool,
    /// Signing in with the saved tokens now (`LoggedIn` / `LoginFailed` follows).
    pub signing_in: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignOutReason {
    /// The user signed out.
    User,
    /// The server rejected the API token (rotated): enter a new one; the
    /// media token is kept.
    Expired,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    SavedLogin(SavedLogin),
    LoginFailed(LoginFailure),
    LoggedIn {
        server_url: String,
    },
    /// Back to the sign-in form; playback stopped and lists cleared.
    /// A [`Event::SavedLogin`] with what is left saved follows.
    SignedOut(SignOutReason),
    AlbumsReset,
    AlbumsAppended {
        items: Vec<AlbumItem>,
        total: usize,
    },
    TracksReset,
    TracksAppended {
        items: Vec<TrackItem>,
        total: usize,
    },
    Image {
        key: String,
        image: Rgba,
    },
    NowPlaying(Option<NowPlaying>),
    Position {
        position_ms: i64,
        duration_ms: i64,
    },
    Paused(bool),
    Buffering(bool),
    Volume(f64),
    AudioDevices(Vec<AudioDevice>),
    /// Selected output device (`None` = system default).
    AudioDevice(Option<String>),
    Exclusive(bool),
    /// Lines of the current track (empty: none). `synced` false = plain text.
    Lyrics {
        lines: Vec<String>,
        synced: bool,
        source: Option<String>,
    },
    LyricsActive(Option<usize>),
    /// Something the user should see (load failure, playback error).
    Notice(String),
}

/// Messages from spawned tasks back into the loop.
enum Internal {
    Reporter(u64, ReporterMsg),
    LoggedIn(Box<(Client, Capabilities)>),
    LoginFailed(LoginFailure),
    /// A request with this API token got 401.
    Unauthorized {
        api_token: String,
    },
    AlbumsPage {
        generation: u64,
        items: Vec<AlbumItem>,
        total: usize,
    },
    TracksPage {
        generation: u64,
        tracks: Vec<Track>,
        total: usize,
    },
    AlbumTracks(Result<Vec<Track>, api::ApiError>),
    AudioDeviceSet(Option<String>, Result<(), String>),
    ExclusiveSet(bool, Result<(), String>),
    SaveSettings(u64),
    Lyrics {
        track_id: i64,
        lyrics: Option<api::Lyrics>,
    },
    Notice(String),
}

/// Handle to the running core. Dropping it does not stop the core; call
/// [`CoreHandle::shutdown`].
pub struct CoreHandle {
    commands: UnboundedSender<Command>,
    engine: UnboundedSender<EngineEvent>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Cloneable command sender for UI callbacks.
#[derive(Clone, Debug)]
pub struct Commander(UnboundedSender<Command>);

impl Commander {
    pub fn send(&self, command: Command) {
        let _ = self.0.send(command);
    }
}

impl CoreHandle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commander(&self) -> Commander {
        Commander(self.commands.clone())
    }

    /// Feed an engine signal (callable from any thread).
    pub fn engine_event(&self, event: EngineEvent) {
        let _ = self.engine.send(event);
    }

    /// A cloneable sender for engine signals.
    pub fn engine_sender(&self) -> impl Fn(EngineEvent) + Send + Sync + 'static {
        let tx = self.engine.clone();
        move |event| {
            let _ = tx.send(event);
        }
    }

    /// Reports `client_closed`, stops the engine and joins the core thread.
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Sink = Arc<dyn Fn(Event) + Send + Sync>;

/// Starts the core on its own tokio runtime thread.
pub fn start(
    engine: Arc<dyn Engine>,
    config: CoreConfig,
    sink: impl Fn(Event) + Send + Sync + 'static,
) -> CoreHandle {
    let (commands, commands_rx) = mpsc::unbounded_channel();
    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let sink: Sink = Arc::new(sink);
    let thread = std::thread::Builder::new()
        .name("core".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            runtime.block_on(async move {
                let (internal_tx, internal_rx) = mpsc::unbounded_channel();
                let mut core = Core::new(engine, config, sink, internal_tx);
                core.run(commands_rx, engine_rx, internal_rx, shutdown_rx)
                    .await;
            });
            runtime.shutdown_timeout(Duration::from_millis(500));
        })
        .expect("core thread");
    CoreHandle {
        commands,
        engine: engine_tx,
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
    }
}

/// Bridges reporter messages into the internal channel, tagged with the
/// sign-in generation so a dropped reporter's timers are ignored.
fn reporter_channel(
    internal: &UnboundedSender<Internal>,
    generation: u64,
) -> UnboundedSender<ReporterMsg> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let internal = internal.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if internal.send(Internal::Reporter(generation, msg)).is_err() {
                break;
            }
        }
    });
    tx
}

struct Library {
    client: Client,
    capabilities: Capabilities,
    images: Arc<Images>,
    tracks: Vec<Track>,
}

/// Runs a blocking secret-store call off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

async fn read_token(secrets: &Arc<dyn SecretStore>, server: &str, kind: TokenKind) -> String {
    let secrets = secrets.clone();
    let key = token_key(server, kind);
    match blocking(move || secrets.get(&key)).await {
        Some(Ok(value)) => value.unwrap_or_default(),
        Some(Err(e)) => {
            tracing::warn!("reading saved {kind:?} token: {e}");
            String::new()
        }
        None => String::new(),
    }
}

/// Saves (`Some`) or deletes (`None`) a token.
async fn write_token(
    secrets: &Arc<dyn SecretStore>,
    server: &str,
    kind: TokenKind,
    value: Option<String>,
) {
    let secrets = secrets.clone();
    let key = token_key(server, kind);
    let result = blocking(move || match value {
        Some(value) => secrets.set(&key, &value),
        None => secrets.delete(&key),
    })
    .await;
    if let Some(Err(e)) = result {
        tracing::warn!("saving {kind:?} token: {e}");
    }
}

/// Signs in; empty tokens come from the store. Saves both tokens on
/// success; forgets a saved token the server rejected.
async fn sign_in(
    secrets: Arc<dyn SecretStore>,
    sink: Sink,
    server_url: String,
    api_token: String,
    media_token: String,
) -> Internal {
    let Ok(server) = api::normalize_base_url(&server_url) else {
        return Internal::LoginFailed(LoginFailure::InvalidUrl);
    };
    let api_saved = api_token.is_empty();
    let api_token = if api_saved {
        read_token(&secrets, &server, TokenKind::Api).await
    } else {
        api_token
    };
    let media_saved = media_token.is_empty();
    let media_token = if media_saved {
        read_token(&secrets, &server, TokenKind::Media).await
    } else {
        media_token
    };
    match api::authenticate(&server, &api_token, &media_token).await {
        Ok(ok) => {
            write_token(&secrets, &server, TokenKind::Api, Some(api_token)).await;
            write_token(&secrets, &server, TokenKind::Media, Some(media_token)).await;
            Internal::LoggedIn(Box::new(ok))
        }
        Err(e) => {
            let failure = LoginFailure::from(e);
            let rejected = match failure {
                LoginFailure::ApiTokenInvalid if api_saved && !api_token.is_empty() => {
                    Some(TokenKind::Api)
                }
                LoginFailure::MediaTokenInvalid if media_saved && !media_token.is_empty() => {
                    Some(TokenKind::Media)
                }
                _ => None,
            };
            if let Some(kind) = rejected {
                write_token(&secrets, &server, kind, None).await;
                sink(Event::SavedLogin(SavedLogin {
                    server_url: server,
                    has_api_token: kind != TokenKind::Api && !api_token.is_empty(),
                    has_media_token: kind != TokenKind::Media && !media_token.is_empty(),
                    signing_in: false,
                }));
            }
            Internal::LoginFailed(failure)
        }
    }
}

/// The message to send when `e` means the API token was rejected.
fn unauthorized(client: &Client, e: &api::ApiError) -> Option<Internal> {
    e.is_unauthorized().then(|| Internal::Unauthorized {
        api_token: client.api_token().to_owned(),
    })
}

struct Core {
    engine: Arc<dyn Engine>,
    config: CoreConfig,
    sink: Sink,
    internal: UnboundedSender<Internal>,
    /// `session.json`: the last signed-in server.
    session_file: FileStorage,
    library: Option<Library>,
    reporter: Option<Reporter>,
    /// Bumped on every sign-in and sign-out; results of older background
    /// work (list pages, reporter timers) are dropped.
    list_generation: u64,
    queue: Queue,
    /// Nothing loaded in the engine (before the first play, after the end
    /// of the queue or a failure): play reloads the current entry.
    idle: bool,
    /// Between `load` and the new file's `FileLoaded`: progress, duration
    /// and end-of-file signals still belong to the previous file.
    loading: bool,
    paused: bool,
    position_ms: i64,
    reported_step: i64,
    engine_duration_ms: i64,
    lyrics: Option<lyrics::SyncedLyrics>,
    active_line: Option<usize>,
    settings: Settings,
    /// Bumped per settings change; `saved_revision` is the last written.
    settings_revision: u64,
    saved_revision: u64,
    /// An output device or exclusive mode change is running.
    output_busy: bool,
}

impl Core {
    fn new(
        engine: Arc<dyn Engine>,
        config: CoreConfig,
        sink: Sink,
        internal: UnboundedSender<Internal>,
    ) -> Self {
        let session_file = FileStorage::open(config.data_dir.join("session.json"));
        let settings = config.settings.clone();
        Self {
            engine,
            config,
            sink,
            internal,
            session_file,
            library: None,
            reporter: None,
            list_generation: 0,
            queue: Queue::default(),
            idle: true,
            loading: false,
            paused: false,
            position_ms: 0,
            reported_step: -1,
            engine_duration_ms: 0,
            lyrics: None,
            active_line: None,
            settings,
            settings_revision: 0,
            saved_revision: 0,
            output_busy: false,
        }
    }

    fn emit(&self, event: Event) {
        (self.sink)(event);
    }

    /// Applies `f` to the settings and schedules a save if anything changed.
    fn update_settings(&mut self, f: impl FnOnce(&mut Settings)) {
        let before = self.settings.clone();
        f(&mut self.settings);
        if self.settings == before {
            return;
        }
        self.settings_revision += 1;
        let revision = self.settings_revision;
        let tx = self.internal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SETTINGS_SAVE_DELAY).await;
            let _ = tx.send(Internal::SaveSettings(revision));
        });
    }

    fn save_settings(&mut self) {
        if self.saved_revision == self.settings_revision {
            return;
        }
        match self.settings.save(&self.config.data_dir) {
            Ok(()) => self.saved_revision = self.settings_revision,
            Err(e) => tracing::warn!("saving settings: {e}"),
        }
    }

    /// Runs an output change (device, exclusive) on a blocking thread.
    fn change_output(
        &mut self,
        change: impl FnOnce(&dyn Engine) -> Result<(), String> + Send + 'static,
        done: impl FnOnce(Result<(), String>) -> Internal + Send + 'static,
    ) {
        self.output_busy = true;
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || change(engine.as_ref()))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(done(result));
        });
    }

    async fn run(
        &mut self,
        mut commands: UnboundedReceiver<Command>,
        mut engine: UnboundedReceiver<EngineEvent>,
        mut internal: UnboundedReceiver<Internal>,
        mut shutdown: tokio::sync::oneshot::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                Some(command) = commands.recv() => self.command(command),
                Some(event) = engine.recv() => self.engine_event(event),
                Some(msg) = internal.recv() => self.internal(msg),
                _ = &mut shutdown => break,
            }
        }
        self.save_settings();
        if let Some(reporter) = self.reporter.as_mut() {
            reporter.close(Duration::from_secs(2)).await;
        }
        let _ = self.engine.stop();
    }

    // ------------------------------------------------------------ commands

    fn command(&mut self, command: Command) {
        match command {
            Command::Restore => {
                let server = self.session_file.get(SERVER_KEY).unwrap_or_default();
                if server.is_empty() {
                    self.emit(Event::SavedLogin(SavedLogin::default()));
                    return;
                }
                let secrets = self.config.secrets.clone();
                let sink = self.sink.clone();
                let tx = self.internal.clone();
                tokio::spawn(async move {
                    let api = read_token(&secrets, &server, TokenKind::Api).await;
                    let media = read_token(&secrets, &server, TokenKind::Media).await;
                    let signing_in = !api.is_empty() && !media.is_empty();
                    sink(Event::SavedLogin(SavedLogin {
                        server_url: server.clone(),
                        has_api_token: !api.is_empty(),
                        has_media_token: !media.is_empty(),
                        signing_in,
                    }));
                    if signing_in {
                        // Empty tokens: sign_in reads (and may forget) the saved ones.
                        let msg =
                            sign_in(secrets, sink, server, String::new(), String::new()).await;
                        let _ = tx.send(msg);
                    }
                });
            }
            Command::Login {
                server_url,
                api_token,
                media_token,
            } => {
                if self.library.is_some() {
                    return;
                }
                let secrets = self.config.secrets.clone();
                let sink = self.sink.clone();
                let tx = self.internal.clone();
                tokio::spawn(async move {
                    let msg = sign_in(secrets, sink, server_url, api_token, media_token).await;
                    let _ = tx.send(msg);
                });
            }
            Command::Logout => self.sign_out(SignOutReason::User),
            Command::PlayTracks { index } => {
                let Some(library) = &self.library else { return };
                let tracks = library.tracks.clone();
                if self.queue.replace(tracks, index).is_some() {
                    self.play_current(true);
                }
            }
            Command::PlayAlbum { album_id } => {
                let Some(library) = &self.library else { return };
                let client = library.client.clone();
                let browse = library.capabilities.has(Feature::Browse);
                let tx = self.internal.clone();
                tokio::spawn(async move {
                    let query = TrackListQuery {
                        album_id: Some(album_id),
                        ..TrackListQuery::default()
                    };
                    let result = fetch_all_tracks(&client, browse, &query).await;
                    let _ = tx.send(Internal::AlbumTracks(result));
                });
            }
            Command::TogglePause => {
                if self.queue.current().is_none() {
                    return;
                }
                if self.idle {
                    self.play_current(false);
                } else if let Err(e) = self.engine.set_paused(!self.paused) {
                    self.emit(Event::Notice(e));
                }
            }
            Command::Next => {
                if self.queue.has_next() {
                    self.queue.advance();
                    self.play_current(true);
                }
            }
            Command::Previous => {
                if self.queue.current().is_none() {
                    return;
                }
                let restart = self.position_ms > RESTART_THRESHOLD_MS;
                if !restart && self.queue.has_previous() {
                    self.queue.back();
                    self.play_current(true);
                } else if self.idle {
                    self.play_current(true);
                } else {
                    // Like mediaPrevious: a seek in the same session, not a new play.
                    self.seek(0);
                }
            }
            Command::Seek { position_ms } => self.seek(position_ms),
            Command::SetVolume(volume) => {
                if let Err(e) = self.engine.set_volume(volume) {
                    self.emit(Event::Notice(e));
                }
            }
            Command::RefreshAudioDevices => {
                let engine = self.engine.clone();
                let sink = self.sink.clone();
                tokio::spawn(async move {
                    match tokio::task::spawn_blocking(move || engine.audio_devices()).await {
                        Ok(Ok(devices)) => sink(Event::AudioDevices(devices)),
                        Ok(Err(e)) => tracing::warn!("audio devices: {e}"),
                        Err(e) => tracing::warn!("audio devices: {e}"),
                    }
                });
            }
            Command::SetAudioDevice(device) => {
                let device = device.filter(|d| !d.is_empty() && d != "auto");
                if self.output_busy || device == self.settings.audio_device {
                    self.emit(Event::AudioDevice(self.settings.audio_device.clone()));
                    return;
                }
                let name = device.clone().unwrap_or_else(|| "auto".into());
                self.change_output(
                    move |e| e.set_audio_device(&name),
                    move |r| Internal::AudioDeviceSet(device, r),
                );
            }
            Command::SetExclusive(enabled) => {
                if self.output_busy || enabled == self.settings.exclusive {
                    self.emit(Event::Exclusive(self.settings.exclusive));
                    return;
                }
                self.change_output(
                    move |e| e.set_exclusive(enabled),
                    move |r| Internal::ExclusiveSet(enabled, r),
                );
            }
            Command::SetShowLyrics(show) => self.update_settings(|s| s.show_lyrics = show),
            Command::LoadImage { key, path, px } => {
                let Some(library) = &self.library else { return };
                let images = library.images.clone();
                let Some(url) = images.url(&path, px) else {
                    return;
                };
                let sink = self.sink.clone();
                tokio::spawn(async move {
                    match images.load(url, px).await {
                        Ok(image) => sink(Event::Image { key, image }),
                        Err(e) => tracing::debug!("artwork {path}: {e}"),
                    }
                });
            }
        }
    }

    // ------------------------------------------------------------ playback

    fn seek(&mut self, position_ms: i64) {
        if self.idle || self.queue.current().is_none() {
            return;
        }
        let position_ms = position_ms.max(0);
        #[allow(clippy::cast_precision_loss)]
        let seconds = position_ms as f64 / 1000.0;
        if let Err(e) = self.engine.seek(seconds) {
            self.emit(Event::Notice(e));
            return;
        }
        self.position_ms = position_ms;
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| a.on_seek(seconds));
        }
        self.emit_position();
        self.update_active_line();
    }

    fn adapter_track(&self) -> Option<AdapterTrack> {
        self.queue.current().map(|item| AdapterTrack {
            context: None,
            duration_millis: u64::try_from(item.track.duration_millis).unwrap_or(0),
            track_id: item.track.id,
            unique_id: item.uid.to_string(),
        })
    }

    /// Loads and starts the current queue entry.
    fn play_current(&mut self, user_navigated: bool) {
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        let Some(library) = &self.library else { return };
        let url = library.client.stream_url(item.track.id);
        let client = library.client.clone();
        if let Err(e) = self
            .engine
            .load(url.as_str())
            .and_then(|()| self.engine.set_paused(false))
        {
            self.emit(Event::Notice(e));
            return;
        }
        self.idle = false;
        self.loading = true;
        self.paused = false;
        self.position_ms = 0;
        self.reported_step = 0;
        self.engine_duration_ms = 0;

        let adapter_track = self.adapter_track();
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| {
                a.on_progress(0.0);
                a.on_track_change(adapter_track);
                if user_navigated {
                    a.on_user_navigate();
                }
                a.on_status(AdapterStatus::Playing);
            });
        }

        let track = &item.track;
        self.emit(Event::NowPlaying(Some(NowPlaying {
            track_id: track.id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            artwork: track.artwork_url.clone(),
            duration_ms: track.duration_millis,
            quality: format::audio_quality(track),
        })));
        self.emit(Event::Paused(false));
        self.emit_position();

        self.lyrics = None;
        self.active_line = None;
        self.emit(Event::Lyrics {
            lines: Vec::new(),
            synced: false,
            source: None,
        });
        let tx = self.internal.clone();
        let track_id = track.id;
        tokio::spawn(async move {
            let lyrics = match client.lyrics(track_id).await {
                Ok(lyrics) => lyrics,
                Err(e) => {
                    tracing::debug!("lyrics {track_id}: {e}");
                    if let Some(msg) = unauthorized(&client, &e) {
                        let _ = tx.send(msg);
                    }
                    None
                }
            };
            let _ = tx.send(Internal::Lyrics { track_id, lyrics });
        });
    }

    fn duration_ms(&self) -> i64 {
        self.queue
            .current()
            .map(|i| i.track.duration_millis)
            .filter(|d| *d > 0)
            .unwrap_or(self.engine_duration_ms)
    }

    fn emit_position(&self) {
        self.emit(Event::Position {
            position_ms: self.position_ms,
            duration_ms: self.duration_ms(),
        });
    }

    fn update_active_line(&mut self) {
        let active = self
            .lyrics
            .as_ref()
            .and_then(|l| l.active_index(self.position_ms, 0));
        if active != self.active_line {
            self.active_line = active;
            self.emit(Event::LyricsActive(active));
        }
    }

    fn engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::TimePos(seconds) => {
                if self.idle || self.loading {
                    return;
                }
                #[allow(clippy::cast_possible_truncation)]
                let ms = (seconds * 1000.0).round() as i64;
                self.position_ms = ms.max(0);
                let step = self.position_ms / POSITION_STEP_MS;
                if step != self.reported_step {
                    self.reported_step = step;
                    if let Some(r) = self.reporter.as_mut() {
                        r.act(|a| a.on_progress(seconds));
                    }
                    self.emit_position();
                }
                self.update_active_line();
            }
            EngineEvent::Duration(seconds) => {
                if self.loading {
                    return;
                }
                #[allow(clippy::cast_possible_truncation)]
                let ms = (seconds * 1000.0).round() as i64;
                self.engine_duration_ms = ms;
                self.emit_position();
            }
            EngineEvent::Paused(paused) => {
                if self.idle || paused == self.paused {
                    return;
                }
                self.paused = paused;
                if let Some(r) = self.reporter.as_mut() {
                    let status = if paused {
                        AdapterStatus::Paused
                    } else {
                        AdapterStatus::Playing
                    };
                    r.act(|a| a.on_status(status));
                }
                self.emit(Event::Paused(paused));
            }
            EngineEvent::Buffering(buffering) => {
                if let Some(r) = self.reporter.as_mut() {
                    r.act(|a| a.on_buffering(buffering));
                }
                self.emit(Event::Buffering(buffering));
            }
            EngineEvent::Volume(volume) => {
                self.update_settings(|s| s.volume = volume.clamp(0.0, 100.0));
                self.emit(Event::Volume(volume));
            }
            EngineEvent::FileLoaded => self.loading = false,
            EngineEvent::EndFile { reason, error } => match reason {
                EngineEnd::Stop => {}
                EngineEnd::Eof => {
                    if self.idle || self.loading {
                        return;
                    }
                    if self.queue.has_next() {
                        self.queue.advance();
                        self.play_current(false);
                    } else {
                        // End of the queue: keep the last track shown, stopped.
                        self.idle = true;
                        self.paused = true;
                        if let Some(r) = self.reporter.as_mut() {
                            r.act(|a| a.on_status(AdapterStatus::Stopped));
                        }
                        self.position_ms = 0;
                        self.emit(Event::Paused(true));
                        self.emit_position();
                    }
                }
                EngineEnd::Error => {
                    if self.idle {
                        return;
                    }
                    self.idle = true;
                    self.loading = false;
                    self.paused = true;
                    if let Some(r) = self.reporter.as_mut() {
                        r.act(|a| {
                            a.on_error();
                            a.on_status(AdapterStatus::Paused);
                        });
                    }
                    self.emit(Event::Paused(true));
                    self.emit(Event::Notice(format!(
                        "播放失败：{}",
                        error.unwrap_or_else(|| "未知错误".into())
                    )));
                }
            },
        }
    }

    // ------------------------------------------------------------ tasks

    fn internal(&mut self, msg: Internal) {
        match msg {
            Internal::Reporter(generation, msg) => {
                if generation != self.list_generation {
                    return;
                }
                if let ReporterMsg::Unauthorized(api_token) = msg {
                    self.internal(Internal::Unauthorized { api_token });
                } else if let Some(r) = self.reporter.as_mut() {
                    r.handle(msg);
                }
            }
            Internal::LoginFailed(failure) => self.emit(Event::LoginFailed(failure)),
            Internal::LoggedIn(ok) => {
                let (client, capabilities) = *ok;
                if self.library.is_none() {
                    self.logged_in(client, capabilities);
                }
            }
            Internal::Unauthorized { api_token } => {
                if self
                    .library
                    .as_ref()
                    .is_some_and(|l| l.client.api_token() == api_token)
                {
                    self.sign_out(SignOutReason::Expired);
                }
            }
            Internal::AlbumsPage {
                generation,
                items,
                total,
            } => {
                if generation == self.list_generation {
                    self.emit(Event::AlbumsAppended { items, total });
                }
            }
            Internal::TracksPage {
                generation,
                tracks,
                total,
            } => {
                if generation != self.list_generation {
                    return;
                }
                let Some(library) = self.library.as_mut() else {
                    return;
                };
                let items = tracks.iter().map(track_item).collect();
                library.tracks.extend(tracks);
                self.emit(Event::TracksAppended { items, total });
            }
            Internal::AlbumTracks(Ok(tracks)) => {
                if self.queue.replace(tracks, 0).is_some() {
                    self.play_current(true);
                }
            }
            Internal::AlbumTracks(Err(e)) => {
                if e.is_unauthorized() {
                    self.sign_out(SignOutReason::Expired);
                } else {
                    self.emit(Event::Notice(format!("加载专辑失败：{e}")));
                }
            }
            Internal::Lyrics { track_id, lyrics } => {
                if self.queue.current().map(|i| i.track.id) != Some(track_id) {
                    return;
                }
                let Some(lyrics) = lyrics.filter(|l| !l.lines.is_empty()) else {
                    return;
                };
                let lines = lyrics.lines.iter().map(|l| l.text.clone()).collect();
                let source = lyrics.source_name.clone().or(lyrics.source.clone());
                self.emit(Event::Lyrics {
                    lines,
                    synced: lyrics.synced,
                    source,
                });
                if lyrics.synced {
                    self.lyrics = Some(lyrics::SyncedLyrics::new(
                        lyrics
                            .lines
                            .into_iter()
                            .map(|l| lyrics::Line {
                                time_ms: l.time_ms,
                                text: l.text,
                            })
                            .collect(),
                    ));
                    self.active_line = None;
                    self.update_active_line();
                }
            }
            Internal::Notice(text) => self.emit(Event::Notice(text)),
            Internal::AudioDeviceSet(device, result) => {
                self.output_busy = false;
                match result {
                    Ok(()) => self.update_settings(|s| s.audio_device.clone_from(&device)),
                    Err(e) => self.emit(Event::Notice(format!("切换输出设备失败：{e}"))),
                }
                self.emit(Event::AudioDevice(self.settings.audio_device.clone()));
            }
            Internal::ExclusiveSet(enabled, result) => {
                self.output_busy = false;
                match result {
                    Ok(()) => self.update_settings(|s| s.exclusive = enabled),
                    Err(e) => self.emit(Event::Notice(format!("切换独占模式失败：{e}"))),
                }
                self.emit(Event::Exclusive(self.settings.exclusive));
            }
            Internal::SaveSettings(revision) => {
                if revision == self.settings_revision {
                    self.save_settings();
                }
            }
        }
    }

    /// Stops playback, drops the signed-in state and forgets the rejected
    /// (expired) or all (user) saved tokens.
    fn sign_out(&mut self, reason: SignOutReason) {
        let Some(library) = self.library.take() else {
            return;
        };
        self.list_generation += 1;
        if let Some(mut reporter) = self.reporter.take()
            && !self.idle
        {
            // Ends the server-side session; with an expired token this
            // is rejected and the server times the session out.
            reporter.act(|a| a.on_status(AdapterStatus::Stopped));
        }
        let _ = self.engine.stop();
        self.queue = Queue::default();
        self.idle = true;
        self.loading = false;
        self.paused = true;
        self.position_ms = 0;
        self.engine_duration_ms = 0;
        self.lyrics = None;
        self.active_line = None;

        let server = library.client.base_url().to_owned();
        let secrets = self.config.secrets.clone();
        let has_media_token = reason == SignOutReason::Expired;
        {
            let server = server.clone();
            tokio::spawn(async move {
                write_token(&secrets, &server, TokenKind::Api, None).await;
                if !has_media_token {
                    write_token(&secrets, &server, TokenKind::Media, None).await;
                }
            });
        }

        self.emit(Event::NowPlaying(None));
        self.emit(Event::Paused(true));
        self.emit_position();
        self.emit(Event::Lyrics {
            lines: Vec::new(),
            synced: false,
            source: None,
        });
        self.emit(Event::AlbumsReset);
        self.emit(Event::TracksReset);
        self.emit(Event::SignedOut(reason));
        self.emit(Event::SavedLogin(SavedLogin {
            server_url: server,
            has_api_token: false,
            has_media_token,
            signing_in: false,
        }));
    }

    fn logged_in(&mut self, client: Client, capabilities: Capabilities) {
        self.session_file.set(SERVER_KEY, client.base_url());
        self.list_generation += 1;
        let generation = self.list_generation;
        let storage = FileStorage::open(self.config.data_dir.join("playback.json"));
        let reporter_tx = reporter_channel(&self.internal, generation);
        self.reporter = Some(Reporter::new(
            client.clone(),
            &capabilities,
            Box::new(storage),
            reporter_tx,
        ));
        let browse = capabilities.has(Feature::Browse);
        self.library = Some(Library {
            images: Arc::new(Images::new(client.clone())),
            client: client.clone(),
            capabilities,
            tracks: Vec::new(),
        });
        self.emit(Event::LoggedIn {
            server_url: client.base_url().to_owned(),
        });

        self.emit(Event::AlbumsReset);
        self.emit(Event::TracksReset);

        let albums_client = client.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let query = AlbumListQuery {
                sort: Some(AlbumSort::Added),
                order: Some(SortOrder::Desc),
                ..AlbumListQuery::default()
            };
            let mut offset = 0u32;
            loop {
                match albums_client.albums(browse, &query, offset, PAGE).await {
                    Ok(page) => {
                        let n = u32::try_from(page.items.len()).unwrap_or(u32::MAX);
                        let total = usize::try_from(page.total).unwrap_or(0);
                        let items = page.items.iter().map(album_item).collect();
                        let msg = Internal::AlbumsPage {
                            generation,
                            items,
                            total,
                        };
                        offset += n;
                        if tx.send(msg).is_err() || n == 0 || i64::from(offset) >= page.total {
                            break;
                        }
                    }
                    Err(e) => {
                        let msg = unauthorized(&albums_client, &e)
                            .unwrap_or_else(|| Internal::Notice(format!("加载专辑失败：{e}")));
                        let _ = tx.send(msg);
                        break;
                    }
                }
            }
        });

        let tx = self.internal.clone();
        tokio::spawn(async move {
            let query = TrackListQuery {
                sort: Some(TrackSort::Added),
                order: Some(SortOrder::Desc),
                ..TrackListQuery::default()
            };
            let mut offset = 0u32;
            loop {
                match client.tracks(browse, &query, offset, PAGE).await {
                    Ok(page) => {
                        let n = u32::try_from(page.items.len()).unwrap_or(u32::MAX);
                        let total = usize::try_from(page.total).unwrap_or(0);
                        let done = n == 0 || i64::from(offset + n) >= page.total;
                        offset += n;
                        let msg = Internal::TracksPage {
                            generation,
                            tracks: page.items,
                            total,
                        };
                        if tx.send(msg).is_err() || done {
                            break;
                        }
                    }
                    Err(e) => {
                        let msg = unauthorized(&client, &e)
                            .unwrap_or_else(|| Internal::Notice(format!("加载曲目失败：{e}")));
                        let _ = tx.send(msg);
                        break;
                    }
                }
            }
        });
    }
}

async fn fetch_all_tracks(
    client: &Client,
    browse: bool,
    query: &TrackListQuery,
) -> Result<Vec<Track>, api::ApiError> {
    let mut out = Vec::new();
    loop {
        let offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let page = client.tracks(browse, query, offset, PAGE).await?;
        let n = page.items.len();
        out.extend(page.items);
        if n == 0 || i64::try_from(out.len()).unwrap_or(i64::MAX) >= page.total {
            return Ok(out);
        }
    }
}

fn album_item(album: &api::Album) -> AlbumItem {
    AlbumItem {
        id: album.id,
        title: album.title.clone(),
        artist: album.artist.clone(),
        year: album.year,
        artwork: album.artwork_url.clone(),
    }
}

fn track_item(track: &Track) -> TrackItem {
    TrackItem {
        id: track.id,
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        duration: format::duration(track.duration_millis),
    }
}
