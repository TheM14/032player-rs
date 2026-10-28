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

use crate::engine::{Engine, EngineEnd, EngineEvent};
use crate::format;
use crate::images::{Images, Rgba};
use crate::queue::Queue;
use crate::reporter::{Reporter, ReporterMsg};
use crate::storage::FileStorage;

/// "Previous" restarts the track instead after this much playback.
const RESTART_THRESHOLD_MS: i64 = 10_000;
/// Position updates to the UI and the reporter are coarsened to this step.
const POSITION_STEP_MS: i64 = 250;
const PAGE: u32 = api::MAX_PAGE_SIZE;

#[derive(Clone, Debug)]
pub struct CoreConfig {
    /// Where the playback resume snapshot lives.
    pub data_dir: PathBuf,
}

#[derive(Debug)]
pub enum Command {
    Login {
        server_url: String,
        api_token: String,
        media_token: String,
    },
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

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    LoginFailed(LoginFailure),
    LoggedIn {
        server_url: String,
    },
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
    Reporter(ReporterMsg),
    LoggedIn(Box<(Client, Capabilities)>),
    LoginFailed(LoginFailure),
    TracksPage {
        generation: u64,
        tracks: Vec<Track>,
        total: usize,
    },
    AlbumTracks(Result<Vec<Track>, String>),
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

/// Bridges reporter messages into the internal channel.
fn reporter_channel(internal: &UnboundedSender<Internal>) -> UnboundedSender<ReporterMsg> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let internal = internal.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if internal.send(Internal::Reporter(msg)).is_err() {
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

struct Core {
    engine: Arc<dyn Engine>,
    config: CoreConfig,
    sink: Sink,
    internal: UnboundedSender<Internal>,
    library: Option<Library>,
    reporter: Option<Reporter>,
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
}

impl Core {
    fn new(
        engine: Arc<dyn Engine>,
        config: CoreConfig,
        sink: Sink,
        internal: UnboundedSender<Internal>,
    ) -> Self {
        Self {
            engine,
            config,
            sink,
            internal,
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
        }
    }

    fn emit(&self, event: Event) {
        (self.sink)(event);
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
        if let Some(reporter) = self.reporter.as_mut() {
            reporter.close(Duration::from_secs(2)).await;
        }
        let _ = self.engine.stop();
    }

    // ------------------------------------------------------------ commands

    fn command(&mut self, command: Command) {
        match command {
            Command::Login {
                server_url,
                api_token,
                media_token,
            } => {
                let tx = self.internal.clone();
                tokio::spawn(async move {
                    let msg = match api::authenticate(&server_url, &api_token, &media_token).await {
                        Ok(ok) => Internal::LoggedIn(Box::new(ok)),
                        Err(e) => Internal::LoginFailed(e.into()),
                    };
                    let _ = tx.send(msg);
                });
            }
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
                    let result = fetch_all_tracks(&client, browse, &query)
                        .await
                        .map_err(|e| e.to_string());
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
            EngineEvent::Volume(volume) => self.emit(Event::Volume(volume)),
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
            Internal::Reporter(msg) => {
                if let Some(r) = self.reporter.as_mut() {
                    r.handle(msg);
                }
            }
            Internal::LoginFailed(failure) => self.emit(Event::LoginFailed(failure)),
            Internal::LoggedIn(ok) => {
                let (client, capabilities) = *ok;
                self.logged_in(client, capabilities);
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
            Internal::AlbumTracks(Err(e)) => self.emit(Event::Notice(format!("加载专辑失败：{e}"))),
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
        }
    }

    fn logged_in(&mut self, client: Client, capabilities: Capabilities) {
        let storage = FileStorage::open(self.config.data_dir.join("playback.json"));
        let reporter_tx = reporter_channel(&self.internal);
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

        self.list_generation += 1;
        let generation = self.list_generation;
        self.emit(Event::AlbumsReset);
        self.emit(Event::TracksReset);

        let sink = self.sink.clone();
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
                        sink(Event::AlbumsAppended { items, total });
                        offset += n;
                        if n == 0 || i64::from(offset) >= page.total {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Internal::Notice(format!("加载专辑失败：{e}")));
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
                        let _ = tx.send(Internal::Notice(format!("加载曲目失败：{e}")));
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
