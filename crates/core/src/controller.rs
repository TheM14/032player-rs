//! The core loop: owns all client state, runs on tokio, takes [`Command`]s
//! from the UI and [`EngineEvent`]s from the audio engine, and pushes
//! [`Event`]s to a sink. The UI never blocks on the network.

mod fade;
mod library;
mod lyrics;
mod m032;

pub use m032::PlayingSources;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::{
    AuthError, Capabilities, Client, FavoriteKind, Feature, Playlist, RandomSeeds, Track, Transcode,
};
use session::{AdapterStatus, AdapterTrack};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::browse::{
    AlbumFilter, AlbumItem, ArtistFilter, ArtistItem, DetailView, PlaylistFilter, PlaylistItem,
    TrackFilter, TrackItem, TrackList,
};
use crate::engine::{AudioDevice, Engine, EngineEnd, EngineEvent};
use crate::format;
use crate::images::{Images, Rgba};
use crate::m032::{
    AlbumPane, ChronicleView, HomeView, MixItem, RecordItem, Rotation, SeriesItem, SongItem,
    WorkItem,
};
use crate::queue::{Queue, QueueSnapshot, Repeat, Step};
use crate::reporter::{Reporter, ReporterMsg};
use crate::secrets::{SecretStore, TokenKind, token_key};
use crate::settings::{LyricsSettings, Settings};
use crate::storage::FileStorage;
use session::KeyValueStorage;

/// "Previous" restarts the track instead after this much playback.
const RESTART_THRESHOLD_MS: i64 = 10_000;
/// Position updates to the UI and the reporter are coarsened to this step.
const POSITION_STEP_MS: i64 = 250;
const PAGE: u32 = api::MAX_PAGE_SIZE;

/// The timed sleep timer counts down in steps of this many ms of playback.
const SLEEP_TICK_MS: i64 = 1_000;
/// Settings changes are written this long after the last one (volume drags).
const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(500);

/// Key of the last signed-in server in `session.json`.
const SERVER_KEY: &str = "server";
/// Playback position (ms) of the saved queue's current entry, in `session.json`.
const POSITION_KEY: &str = "positionMs";
/// The queue file is written this long after the last change.
const QUEUE_SAVE_DELAY: Duration = Duration::from_secs(1);
/// While playing, the position is saved every this many ms.
const POSITION_SAVE_STEP_MS: i64 = 10_000;
const QUEUE_FILE: &str = "queue.json";

/// `queue.json`: the queue of one server.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedQueue {
    server: String,
    queue: QueueSnapshot,
}

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
    /// Replace the queue with `source` and play it (a list row plays the
    /// whole list from that row).
    Play(TrackSource),
    /// Like `Play`, in random order (the shuffle mode is not changed).
    ShufflePlay(TrackSource),
    /// Play random tracks of the library ("随机播放全部"), within the
    /// year range of the tracks page.
    ShuffleAll,
    /// Insert tracks after the current entry or at the end of the queue.
    Enqueue {
        source: TrackSource,
        at: QueuePosition,
    },
    PlayQueueItem(u64),
    RemoveFromQueue(u64),
    /// Drag and drop in the queue: move to this play-order position.
    MoveQueueItem {
        uid: u64,
        to: usize,
    },
    /// Reload the albums page (`favorites`: the favorites page) with a filter.
    QueryAlbums {
        favorites: bool,
        filter: AlbumFilter,
    },
    QueryTracks {
        favorites: bool,
        filter: TrackFilter,
    },
    QueryArtists {
        favorites: bool,
        filter: ArtistFilter,
    },
    QueryPlaylists(PlaylistFilter),
    /// Answered with [`Event::Detail`].
    OpenAlbum(i64),
    OpenArtist(i64),
    OpenPlaylist(i64),
    /// Tracks similar to a list row.
    OpenSimilar(TrackList, usize),
    /// Answered with [`Event::SearchResults`].
    Search(String),
    /// Answered with [`Event::Favorite`].
    SetFavorite {
        kind: FavoriteKind,
        id: i64,
        favorite: bool,
    },
    /// New playlist, optionally with tracks.
    CreatePlaylist {
        name: String,
        description: String,
        tracks: Option<TrackSource>,
    },
    UpdatePlaylist {
        id: i64,
        name: String,
        description: String,
    },
    DeletePlaylist(i64),
    AddToPlaylist {
        playlist_id: i64,
        source: TrackSource,
    },
    /// Remove the row `index` of the open playlist.
    RemoveFromPlaylist {
        playlist_id: i64,
        index: usize,
    },
    /// Move a row of the open playlist.
    MovePlaylistItem {
        playlist_id: i64,
        from: usize,
        to: usize,
    },
    /// Upload (`Some`, image file bytes) or reset the playlist artwork.
    SetPlaylistArtwork {
        playlist_id: i64,
        image: Option<Vec<u8>>,
    },
    /// Stream a transcode (`None`: the original file).
    SetTranscode(Option<Transcode>),
    /// Stops playback and empties the queue.
    ClearQueue,
    SetRepeat(Repeat),
    SetShuffle(bool),
    TogglePause,
    /// Stop (keeps the queue; play starts the current entry over).
    Stop,
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
    /// Layout of the right panel (queue / lyrics).
    SetRightPanel(crate::settings::RightPanel),
    SetShortcuts(crate::shortcuts::Shortcuts),
    /// Left sidebar width (logical px, clamped).
    SetSidebarWidth(f32),
    /// Main window size, sent when the app quits.
    SetWindow(crate::settings::WindowState),
    SetCloseToTray(bool),
    /// Volume fade on play / pause.
    SetFadeOnPause(bool),
    SetTheme(crate::settings::ThemeMode),
    /// Looks for a newer GitHub release ([`Event::UpdateAvailable`]).
    CheckUpdate,
    /// Start (`Some`) or cancel the sleep timer.
    SetSleepTimer(Option<SleepTimer>),
    /// Load artwork `path` for a `width` x `height` box (physical pixels,
    /// cover-fit and cropped to exactly that size); answered with
    /// [`Event::Image`] carrying `key`.
    LoadImage {
        key: String,
        path: String,
        width: u32,
        height: u32,
    },
    /// Like [`Command::LoadImage`], but downloads the original artwork (no
    /// `size`; the full-screen player).
    LoadOriginalImage {
        key: String,
        path: String,
        width: u32,
        height: u32,
    },
    /// Songs similar to track `id` ([`Event::Related`], `TrackList::Related`).
    LoadRelated(i64),
    /// Details of a row ([`Event::TrackInfo`]).
    TrackInfo(TrackList, usize),
    /// Details of the current track ([`Event::TrackInfo`]).
    NowTrackInfo,
    /// Play just this row (接着听 rows start at their saved position).
    PlayTrack(TrackList, usize),
    /// The 032 home ([`Event::Home`] or [`Event::HomeUnavailable`]), its
    /// 最近播放 and today's daily mixes.
    LoadHome,
    /// Works rotation: another type (`kind`, from the first batch) or the
    /// next batch (`next`) of the shown one.
    HomeWorks {
        kind: Option<String>,
        next: bool,
    },
    /// Next batch of 主题曲与插曲.
    HomeSongsNext,
    /// Albums of another year in 编年.
    HomeChronicle(i32),
    /// 换一张 on the 随机重温 spotlight tab.
    HomeReroll,
    /// 从 x 继续 on the 最近在听 spotlight tab.
    ResumeListening,
    /// Reload the works page.
    QueryWorks(api::WorkListQuery),
    /// Reload the series page (search text).
    QuerySeries(String),
    QueryHistory,
    ClearHistory,
    OpenWork(i64),
    OpenSeries(i64),
    OpenDailyMix(i64),
    /// 1 like, -1 dislike (with reasons), 0 clears.
    RateDailyMix {
        id: i64,
        value: i32,
        reasons: Vec<String>,
    },
    /// 换一个: answered with [`Event::DailyMixReplaced`].
    ReplaceDailyMix(i64),
    SaveDailyMix(i64),
    /// 不再推荐 a row of the open daily mix (or recommend it again).
    BlockRecommendation {
        index: usize,
        blocked: bool,
    },
    /// Online lyrics search for the current track.
    SearchLyrics {
        name: String,
        artist: String,
    },
    /// Show a search result's lyrics ([`Event::LyricsPreview`]).
    PreviewLyrics(usize),
    /// Use a search result as the current track's lyrics (every client).
    ApplyLyrics(usize),
    /// Drop the online lyrics of the current track.
    ClearLyrics,
    /// Translate (first time) or show / hide the translation.
    ToggleTranslation,
    /// Lyrics offset of the current track for this session (positive:
    /// lyrics earlier).
    SetLyricsOffset(i64),
    /// Seek to a lyrics line.
    SeekToLyric(usize),
    SetLyricsSettings(LyricsSettings),
}

/// When playback stops by itself (the Electron sleep timer modes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepTimer {
    /// At the next change of track (the current one ends or is skipped).
    EndOfTrack,
    /// After the last track of the current album.
    EndOfAlbum,
    /// After this many minutes of playback.
    Minutes(u32),
}

/// The running sleep timer, for the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepStatus {
    pub timer: SleepTimer,
    /// Playback time left (`Minutes` only).
    pub remaining_ms: Option<i64>,
}

struct Sleep {
    timer: SleepTimer,
    remaining_ms: i64,
    /// `EndOfAlbum`: the album to finish.
    album_id: Option<i64>,
}

/// Tracks to play, queue or add to a playlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackSource {
    /// One row of a track list (`Play`: the whole list from that row).
    Track(TrackList, usize),
    /// A whole track list.
    List(TrackList),
    Album(i64),
    /// Every track of the artist in album order.
    Artist(i64),
    /// The albums of a release group of the open artist page (artist id,
    /// group index in [`crate::release_groups`] order), in page order.
    ArtistGroup(i64, usize),
    Playlist(i64),
    /// Tracks linked to a work.
    Work(i64),
    /// Tracks of every member of a series.
    Series(i64),
    /// Playable tracks of a daily mix (playing it reports the mix).
    DailyMix(i64),
    /// The play queue in play order ("另存为歌单" of the queue panel).
    Queue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePosition {
    /// Right after the current entry ("play next").
    Next,
    Last,
}

/// What to do with fetched tracks.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Intent {
    /// Replace the queue and play from this index.
    Play {
        start: usize,
        shuffle: bool,
        /// Playback contexts per track (daily mix); may be shorter.
        contexts: Vec<Option<session::PlaybackContext>>,
        /// The first entry starts here (接着听).
        start_ms: i64,
    },
    At(QueuePosition),
    AddToPlaylist(i64),
    NewPlaylist {
        name: String,
        description: String,
    },
}

impl Intent {
    fn play_at(start: usize) -> Self {
        Self::Play {
            start,
            shuffle: false,
            contexts: Vec::new(),
            start_ms: 0,
        }
    }

    fn starting_at(self, ms: i64) -> Self {
        match self {
            Self::Play {
                start,
                shuffle,
                contexts,
                ..
            } => Self::Play {
                start,
                shuffle,
                contexts,
                start_ms: ms,
            },
            other => other,
        }
    }

    fn with_contexts(self, contexts: Vec<Option<session::PlaybackContext>>) -> Self {
        match self {
            Self::Play {
                start,
                shuffle,
                start_ms,
                ..
            } => Self::Play {
                start,
                shuffle,
                contexts,
                start_ms,
            },
            other => other,
        }
    }

    fn shuffled(self) -> Self {
        match self {
            Self::Play {
                contexts, start_ms, ..
            } => Self::Play {
                start: 0,
                shuffle: true,
                contexts,
                start_ms,
            },
            other => other,
        }
    }
}

/// Server abilities the UI adapts to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerFeatures {
    /// `/browse/*`: sort order, random, year ranges.
    pub browse: bool,
    pub playlist_artwork: bool,
    /// Transcode formats the server offers (`mp3`, `ogg`, `flac`).
    pub transcode: Vec<String>,
    /// `/home`, the works pages, `/series`, daily mixes.
    pub home: bool,
    pub works: bool,
    pub series: bool,
    pub daily_mixes: bool,
    /// Server lyrics translation (`lyricsTranslation`).
    pub lyrics_translation: bool,
}

/// A queue row for the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueEntry {
    pub uid: u64,
    pub title: String,
    pub artist: String,
    pub duration: String,
    /// Server-relative artwork path.
    pub artwork: String,
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
pub struct NowPlaying {
    pub track_id: i64,
    pub title: String,
    pub artist: String,
    /// 0: unknown.
    pub artist_id: i64,
    pub album: String,
    pub album_id: i64,
    pub favorite: bool,
    pub artwork: String,
    /// Absolute artwork URL (with the media token) for the system media
    /// controls.
    pub artwork_url: Option<String>,
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
        features: ServerFeatures,
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
    ArtistsReset,
    ArtistsAppended {
        items: Vec<ArtistItem>,
        total: usize,
    },
    FavoriteAlbums(Vec<AlbumItem>),
    FavoriteTracks(Vec<TrackItem>),
    FavoriteArtists(Vec<ArtistItem>),
    /// The playlists page (searched and sorted).
    Playlists(Vec<PlaylistItem>),
    /// Every playlist by name, for "add to playlist" menus.
    PlaylistChoices(Vec<PlaylistItem>),
    /// The playlist is gone (leave its page).
    PlaylistDeleted(i64),
    /// The opened (or refreshed) detail page; `tracks` is [`TrackList::Detail`].
    Detail {
        view: DetailView,
        tracks: Vec<TrackItem>,
    },
    /// Works linked to the open album (after [`Event::Detail`]); `tieups`:
    /// per track, the first use label and the number of works.
    AlbumWorks {
        album_id: i64,
        works: Vec<crate::m032::AlbumWorkEntry>,
        tieups: Vec<(i64, String, usize)>,
    },
    SearchResults {
        query: String,
        artists: Vec<ArtistItem>,
        albums: Vec<AlbumItem>,
        tracks: Vec<TrackItem>,
    },
    /// A favorite flag changed on the server.
    Favorite {
        kind: FavoriteKind,
        id: i64,
        favorite: bool,
    },
    Image {
        key: String,
        image: Rgba,
    },
    NowPlaying(Option<NowPlaying>),
    /// The whole queue in play order, and the current row.
    Queue {
        items: Vec<QueueEntry>,
        current: Option<usize>,
    },
    /// Only the current row moved.
    QueuePosition(Option<usize>),
    Repeat(Repeat),
    Shuffle(bool),
    SleepTimer(Option<SleepStatus>),
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
        lines: Vec<LyricRow>,
        synced: bool,
        source: Option<String>,
    },
    LyricsActive(Option<usize>),
    /// State of the lyrics toolbar.
    LyricsTools(LyricsTools),
    /// Online lyrics search results, best first.
    LyricsSearchResults(Vec<LyricsResultItem>),
    /// Lines of a previewed search result (empty: none).
    LyricsPreview(Vec<String>),
    Home(Box<HomeView>),
    /// The server has no home endpoint.
    HomeUnavailable,
    HomeWorks(Rotation<WorkItem>),
    HomeSongs(Rotation<SongItem>),
    HomeChronicle(ChronicleView),
    /// A new pane for a spotlight tab (换一张).
    HomeSpotlightPane(AlbumPane),
    /// 最近播放 rows (`TrackList::HomeRecent`).
    HomeRecent(Vec<RecordItem>),
    /// Today's daily mixes.
    DailyMixes {
        generating: bool,
        items: Vec<MixItem>,
    },
    /// 换一个 succeeded: leave the old mix's page for the new one.
    DailyMixReplaced {
        from: i64,
        to: i64,
    },
    /// 不再推荐 changed on a row of the open daily mix.
    MixRowBlocked {
        index: usize,
        blocked: bool,
    },
    WorksReset,
    WorksAppended {
        items: Vec<WorkItem>,
        total: usize,
    },
    SeriesReset,
    SeriesAppended {
        items: Vec<SeriesItem>,
        total: usize,
    },
    HistoryReset,
    /// Rows of `TrackList::History`.
    HistoryAppended {
        items: Vec<RecordItem>,
        total: usize,
    },
    /// Works / series / mix the current track belongs to.
    PlayingSources(PlayingSources),
    /// A newer release than this build is published.
    UpdateAvailable {
        /// Tag, e.g. `v0.1.7`.
        version: String,
        /// Release page.
        url: String,
    },
    /// The track info dialog: title and (label, value) rows.
    TrackInfo {
        title: String,
        facts: Vec<(String, String)>,
    },
    /// The open artist's favorite songs (`TrackList::ArtistFavorites`).
    ArtistFavorites {
        artist_id: i64,
        tracks: Vec<TrackItem>,
    },
    /// Songs similar to `track_id` (`TrackList::Related`).
    Related {
        track_id: i64,
        tracks: Vec<TrackItem>,
    },
    /// Something the user should see (load failure, playback error).
    Notice(String),
}

/// A lyrics line as shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LyricRow {
    pub text: String,
    /// The translation under the original (bilingual).
    pub translation: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LyricsTools {
    pub has_lyrics: bool,
    /// Online search / pick / clear (`lyricsOnline`).
    pub can_search: bool,
    /// Server translation (`lyricsTranslation`).
    pub can_translate: bool,
    pub translation_shown: bool,
    pub translating: bool,
    pub offset_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LyricsResultItem {
    pub title: String,
    pub artist: String,
    /// "NetEase — 123 — 同步".
    pub meta: String,
    /// "98.00%".
    pub score: String,
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
    /// A list or page result tagged with its request (`seq`); only the
    /// latest request of each [`Target`] is applied.
    Loaded(Target, u64, Loaded),
    /// A request failed; `what` prefixes the notice.
    Failed {
        api_token: String,
        what: &'static str,
        error: api::ApiError,
    },
    Fetched(Result<Vec<Track>, api::ApiError>, Intent),
    FavoriteSet {
        kind: FavoriteKind,
        id: i64,
        favorite: bool,
    },
    /// A playlist edit finished: reload the playlists (and the open one).
    PlaylistsChanged {
        notice: Option<String>,
        deleted: Option<i64>,
    },
    AudioDeviceSet(Option<String>, Result<(), String>),
    ExclusiveSet(bool, Result<(), String>),
    SaveSettings(u64),
    SaveQueue(u64),
    SleepTick(u64),
    Lyrics {
        track_id: i64,
        lyrics: Option<api::Lyrics>,
    },
    Translation {
        track_id: i64,
        lines: Result<Vec<String>, api::ApiError>,
    },
    LyricsResults {
        track_id: i64,
        results: Vec<api::LyricsSearchResult>,
    },
    LyricsPreview(u64, Vec<String>),
    /// The track's lyrics source changed on the server: reload them.
    LyricsChanged(i64),
    /// Tracks of an expanded work / series.
    SourceTracks(m032::SourceKey, std::collections::HashSet<i64>),
    TrackWorks(i64, Vec<api::TrackWork>),
    PollMixes(u64),
    HistoryCleared,
    MixChanged {
        id: i64,
        notice: Option<String>,
        replaced_by: Option<i64>,
    },
    MixBlocked {
        mix_id: i64,
        index: usize,
        blocked: bool,
    },
    Notice(String),
}

/// What a background request fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Target {
    Albums,
    Tracks,
    Artists,
    FavoriteAlbums,
    FavoriteTracks,
    FavoriteArtists,
    Playlists,
    Detail,
    Search,
    Home,
    HomeRecent,
    HomeWorks,
    HomeSongs,
    HomeChronicle,
    HomeSpotlight,
    DailyMixes,
    Works,
    SeriesList,
    History,
    LyricsPreview,
    Related,
    ArtistFavorites,
}

enum Loaded {
    AlbumsPage(Vec<AlbumItem>, usize),
    TracksPage(Vec<Track>, usize),
    ArtistsPage(Vec<ArtistItem>, usize),
    FavoriteAlbums(Vec<AlbumItem>),
    FavoriteTracks(Vec<Track>),
    FavoriteArtists(Vec<ArtistItem>),
    Playlists(Vec<Playlist>),
    /// The view, its tracks and their tags.
    Detail(Box<DetailView>, Vec<Track>, Vec<String>),
    Search {
        query: String,
        artists: Vec<ArtistItem>,
        albums: Vec<AlbumItem>,
        tracks: Vec<Track>,
    },
    Home(Box<api::Home>),
    HomeUnavailable,
    HomeRecent(Vec<Track>),
    DailyMixes(api::DailyMixList),
    HomeWorks(api::RotationPage<api::HomeWorkCard>),
    HomeSongs(api::RotationPage<api::HomeSongRow>),
    HomeChronicle(api::Chronicle),
    HomeSpotlight(Box<api::HomeAlbumPane>),
    WorksPage(Vec<WorkItem>, usize),
    SeriesPage(Vec<SeriesItem>, usize),
    HistoryPage(Vec<api::PlaybackRecord>, usize),
    /// Works linked to the open album.
    AlbumWorks(i64, Vec<crate::m032::AlbumWorkEntry>),
    /// Similar songs of a track.
    Related(i64, Vec<Track>),
    /// Favorite songs of an artist.
    ArtistFavorites(i64, Vec<Track>),
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
    /// Rows of the playable track lists.
    lists: HashMap<TrackList, Vec<Track>>,
    seeds: Arc<Mutex<RandomSeeds>>,
}

impl Library {
    fn list(&self, list: TrackList) -> &[Track] {
        self.lists.get(&list).map_or(&[], Vec::as_slice)
    }

    fn browse(&self) -> bool {
        self.capabilities.has(Feature::Browse)
    }
}

/// The open detail page, refreshed after edits and favorites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailKey {
    Album(i64),
    Artist(i64),
    Playlist(i64),
    Similar(i64),
    Work(i64),
    Series(i64),
    DailyMix(i64),
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
    lyr: lyrics::LyricsState,
    settings: Settings,
    /// Bumped per settings change; `saved_revision` is the last written.
    settings_revision: u64,
    saved_revision: u64,
    /// An output device or exclusive mode change is running.
    output_busy: bool,
    /// Queue entry appended to the engine for gapless playback.
    preloaded: Option<u64>,
    /// URL of the file being loaded; a different `FileLoaded` is a stale preload.
    expected_path: Option<String>,
    /// Entry of the last started play (repeat-one passes start it again).
    started_uid: Option<u64>,
    /// Restored queue entry and the position to continue it at.
    resume_at: Option<(u64, i64)>,
    /// Seek to this once the loading file is loaded (resumed position).
    pending_seek_ms: Option<i64>,
    /// 接着听: the play starts at 0 for the server and then seeks here
    /// (reported once the file is loaded), like the Electron client.
    report_seek_ms: Option<i64>,
    /// Bumped per queue change; the debounced save writes the latest.
    queue_revision: u64,
    saved_queue_revision: u64,
    saved_position_step: i64,
    sleep: Option<Sleep>,
    /// Bumped per sleep timer start / cancel; stale ticks are ignored.
    sleep_generation: u64,
    /// Request counter; `latest` holds the newest request per target.
    seq: u64,
    latest: HashMap<Target, u64>,
    /// Filters of the loaded lists (reloaded after favorite changes).
    track_filter: TrackFilter,
    favorite_albums: Option<AlbumFilter>,
    favorite_tracks: Option<TrackFilter>,
    favorite_artists: Option<ArtistFilter>,
    playlist_filter: PlaylistFilter,
    playlists: Vec<Playlist>,
    open_detail: Option<DetailKey>,
    /// Lossy transcodes cannot seek by range: the stream starts here and
    /// engine positions are relative to it.
    stream_offset_ms: i64,
    /// The home page was loaded (partial reloads use its state).
    home: Option<m032::HomeState>,
    open_mix: Option<m032::OpenMix>,
    /// Works / series expanded into tracks this session.
    sources: m032::SourceTracks,
    /// Works of a track (`/tracks/{id}/works`).
    track_works: Option<(i64, Vec<api::TrackWork>)>,
    playing_sources: PlayingSources,
    fade: fade::Fader,
    /// Album ids per release group of the open artist page.
    artist_groups: Option<(i64, Vec<Vec<i64>>)>,
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
        let mut queue = Queue::default();
        queue.set_shuffle(settings.shuffle);
        Self {
            engine,
            config,
            sink,
            internal,
            session_file,
            library: None,
            reporter: None,
            list_generation: 0,
            queue,
            idle: true,
            loading: false,
            paused: false,
            position_ms: 0,
            reported_step: -1,
            engine_duration_ms: 0,
            lyr: lyrics::LyricsState::default(),
            settings,
            settings_revision: 0,
            saved_revision: 0,
            output_busy: false,
            preloaded: None,
            expected_path: None,
            started_uid: None,
            resume_at: None,
            pending_seek_ms: None,
            report_seek_ms: None,
            queue_revision: 0,
            saved_queue_revision: 0,
            saved_position_step: -1,
            sleep: None,
            sleep_generation: 0,
            seq: 0,
            latest: HashMap::new(),
            track_filter: TrackFilter::default(),
            favorite_albums: None,
            favorite_tracks: None,
            favorite_artists: None,
            playlist_filter: PlaylistFilter::default(),
            playlists: Vec::new(),
            open_detail: None,
            stream_offset_ms: 0,
            home: None,
            open_mix: None,
            sources: m032::SourceTracks::default(),
            track_works: None,
            playing_sources: PlayingSources::default(),
            fade: fade::Fader::default(),
            artist_groups: None,
        }
    }

    fn emit(&self, event: Event) {
        (self.sink)(event);
    }

    fn now_playing(&self, track: &Track) -> NowPlaying {
        let artwork_url = self.library.as_ref().and_then(|l| {
            l.client
                .image_url(&track.artwork_url, Some(512))
                .map(|u| u.to_string())
        });
        NowPlaying {
            track_id: track.id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            artist_id: crate::browse::track_item(track).artist_id,
            album: track.album.clone(),
            album_id: track.album_id,
            favorite: track.is_favorite,
            artwork: track.artwork_url.clone(),
            artwork_url,
            duration_ms: track.duration_millis,
            quality: format::audio_quality(track),
        }
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
            // Biased: commands sent right before shutdown (the window
            // size on quit) are handled first.
            tokio::select! {
                biased;
                Some(command) = commands.recv() => self.command(command),
                Some(event) = engine.recv() => self.engine_event(event),
                Some(msg) = internal.recv() => self.internal(msg),
                _ = &mut shutdown => break,
            }
        }
        self.save_settings();
        self.save_position();
        if self.queue_revision != self.saved_queue_revision
            && let Some((path, bytes)) = self.queue_file()
        {
            write_atomic(&path, &bytes);
        }
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
            Command::Play(source) => {
                let start = match source {
                    TrackSource::Track(_, index) => index,
                    _ => 0,
                };
                // A row plays its whole list from there.
                let source = match source {
                    TrackSource::Track(list, _) => TrackSource::List(list),
                    other => other,
                };
                self.with_tracks(source, Intent::play_at(start));
            }
            Command::ShufflePlay(source) => {
                self.with_tracks(source, Intent::play_at(0).shuffled());
            }
            Command::ShuffleAll => self.shuffle_all(),
            Command::Enqueue { source, at } => self.with_tracks(source, Intent::At(at)),
            Command::MoveQueueItem { uid, to } => {
                if self.queue.move_to(uid, to) {
                    self.queue_changed();
                }
            }
            Command::QueryAlbums { favorites, filter } => self.query_albums(favorites, filter),
            Command::QueryTracks { favorites, filter } => self.query_tracks(favorites, filter),
            Command::QueryArtists { favorites, filter } => self.query_artists(favorites, filter),
            Command::QueryPlaylists(filter) => {
                self.playlist_filter = filter;
                self.emit_playlists();
            }
            Command::OpenAlbum(id) => self.open_detail(DetailKey::Album(id)),
            Command::OpenArtist(id) => self.open_detail(DetailKey::Artist(id)),
            Command::OpenPlaylist(id) => self.open_detail(DetailKey::Playlist(id)),
            Command::OpenSimilar(list, index) => {
                let id = self
                    .library
                    .as_ref()
                    .and_then(|l| l.list(list).get(index))
                    .map(|t| t.id);
                if let Some(id) = id {
                    self.open_detail(DetailKey::Similar(id));
                }
            }
            Command::Search(query) => self.search(query),
            Command::SetFavorite { kind, id, favorite } => self.set_favorite(kind, id, favorite),
            Command::CreatePlaylist {
                name,
                description,
                tracks,
            } => self.create_playlist(name, description, tracks),
            Command::UpdatePlaylist {
                id,
                name,
                description,
            } => self.update_playlist(id, name, description),
            Command::DeletePlaylist(id) => self.delete_playlist(id),
            Command::AddToPlaylist {
                playlist_id,
                source,
            } => self.add_to_playlist(playlist_id, source),
            Command::RemoveFromPlaylist { playlist_id, index } => {
                self.remove_from_playlist(playlist_id, index);
            }
            Command::MovePlaylistItem {
                playlist_id,
                from,
                to,
            } => self.move_playlist_item(playlist_id, from, to),
            Command::SetPlaylistArtwork { playlist_id, image } => {
                self.set_playlist_artwork(playlist_id, image);
            }
            Command::SetTranscode(transcode) => {
                self.update_settings(|s| s.transcode = transcode);
                // The next file (and the preload) uses the new stream.
                self.preloaded = None;
                self.sync_preload();
            }
            Command::PlayQueueItem(uid) => {
                if self.queue.go_to_uid(uid).is_some() {
                    self.play_current(true);
                    self.emit_queue_position();
                }
            }
            Command::RemoveFromQueue(uid) => self.remove_from_queue(uid),
            Command::ClearQueue => {
                self.stop_playback();
                self.queue.clear();
                self.emit(Event::NowPlaying(None));
                self.queue_changed();
            }
            Command::SetRepeat(repeat) => {
                self.update_settings(|s| s.repeat = repeat);
                self.emit(Event::Repeat(repeat));
                self.sync_preload();
            }
            Command::SetShuffle(shuffle) => {
                self.queue.set_shuffle(shuffle);
                self.update_settings(|s| s.shuffle = shuffle);
                self.emit(Event::Shuffle(shuffle));
                self.queue_changed();
            }
            Command::TogglePause => {
                if self.queue.current().is_none() {
                    return;
                }
                if self.idle {
                    self.play_current(false);
                    return;
                }
                let result = if self.effective_paused() {
                    self.unpause_engine(self.paused)
                } else {
                    self.pause_engine()
                };
                if let Err(e) = result {
                    self.emit(Event::Notice(e));
                }
            }
            Command::Next => {
                let step = self.queue.manual_next(self.settings.repeat);
                self.step(step, true);
            }
            Command::Previous => {
                if self.queue.current().is_none() {
                    return;
                }
                if self.position_ms > RESTART_THRESHOLD_MS && !self.idle {
                    // Like mediaPrevious: a seek in the same session, not a new play.
                    self.seek(0);
                } else {
                    let step = self.queue.manual_previous(self.settings.repeat);
                    self.step(step, true);
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
            Command::SetRightPanel(panel) => self.update_settings(|s| s.right_panel = panel),
            Command::SetShortcuts(keys) => self.update_settings(|s| s.shortcuts = keys),
            Command::SetWindow(mut window) => {
                window.sanitize();
                self.update_settings(|s| s.window = window);
            }
            Command::SetSidebarWidth(width) => {
                self.update_settings(|s| s.sidebar_width = crate::settings::sidebar_width(width));
            }
            Command::SetCloseToTray(on) => self.update_settings(|s| s.close_to_tray = on),
            Command::SetTheme(theme) => self.update_settings(|s| s.theme = theme),
            Command::CheckUpdate => {
                let sink = self.sink.clone();
                tokio::spawn(async move {
                    match api::latest_release(api::GITHUB_API, api::RELEASE_REPO).await {
                        Ok(release) if api::is_newer(&release.tag, env!("CARGO_PKG_VERSION")) => {
                            tracing::info!(tag = %release.tag, "update available");
                            sink(Event::UpdateAvailable {
                                version: release.tag,
                                url: release.url,
                            });
                        }
                        Ok(release) => tracing::debug!(tag = %release.tag, "up to date"),
                        Err(e) => tracing::debug!("update check: {e}"),
                    }
                });
            }
            Command::SetFadeOnPause(on) => {
                self.update_settings(|s| s.fade_on_pause = on);
                if !on && !self.paused {
                    self.restore_gain();
                }
            }
            Command::SetSleepTimer(timer) => self.set_sleep_timer(timer),
            Command::Stop => {
                if !self.idle {
                    self.stop_playback();
                }
            }
            Command::PlayTrack(list, index) => self.play_track(list, index),
            Command::LoadHome => self.load_home(),
            Command::HomeWorks { kind, next } => self.home_works(kind, next),
            Command::HomeSongsNext => self.home_songs_next(),
            Command::HomeChronicle(year) => self.home_chronicle(year),
            Command::HomeReroll => self.home_reroll(),
            Command::ResumeListening => self.resume_listening(),
            Command::QueryWorks(query) => self.query_works(query),
            Command::QuerySeries(search) => self.query_series(search),
            Command::QueryHistory => self.query_history(),
            Command::ClearHistory => self.clear_history(),
            Command::OpenWork(id) => self.open_detail(DetailKey::Work(id)),
            Command::OpenSeries(id) => self.open_detail(DetailKey::Series(id)),
            Command::OpenDailyMix(id) => self.open_detail(DetailKey::DailyMix(id)),
            Command::RateDailyMix { id, value, reasons } => self.rate_mix(id, value, reasons),
            Command::ReplaceDailyMix(id) => self.replace_mix(id),
            Command::SaveDailyMix(id) => self.save_mix(id),
            Command::BlockRecommendation { index, blocked } => {
                self.block_recommendation(index, blocked);
            }
            Command::SearchLyrics { name, artist } => self.search_lyrics(name, artist),
            Command::PreviewLyrics(index) => self.preview_lyrics(index),
            Command::ApplyLyrics(index) => self.apply_lyrics(index),
            Command::ClearLyrics => self.clear_lyrics_source(),
            Command::ToggleTranslation => self.toggle_translation(),
            Command::SetLyricsOffset(ms) => self.set_lyrics_offset(ms),
            Command::SeekToLyric(index) => self.seek_to_lyric(index),
            Command::SetLyricsSettings(lyrics) => self.set_lyrics_settings(lyrics),
            Command::LoadRelated(id) => self.load_related(id),
            Command::TrackInfo(list, index) => {
                let track = self
                    .library
                    .as_ref()
                    .and_then(|l| l.list(list).get(index))
                    .cloned();
                self.emit_track_info(track.as_ref());
            }
            Command::NowTrackInfo => {
                let track = self.queue.current().map(|i| i.track.clone());
                self.emit_track_info(track.as_ref());
            }
            Command::LoadImage {
                key,
                path,
                width,
                height,
            } => self.load_image(key, path, (width, height), false),
            Command::LoadOriginalImage {
                key,
                path,
                width,
                height,
            } => self.load_image(key, path, (width, height), true),
        }
    }

    fn emit_track_info(&self, track: Option<&Track>) {
        if let Some(track) = track {
            self.emit(Event::TrackInfo {
                title: track.title.clone(),
                facts: crate::browse::track_facts(track),
            });
        }
    }

    /// Loads artwork for a `(width, height)` box; `original`: the uploaded
    /// file rather than a server-rendered size.
    fn load_image(&self, key: String, path: String, (width, height): (u32, u32), original: bool) {
        let Some(library) = &self.library else { return };
        let images = library.images.clone();
        let url = if original {
            images.original_url(&path)
        } else {
            images.url(&path, width, height)
        };
        let Some(url) = url else {
            return;
        };
        let sink = self.sink.clone();
        tokio::spawn(async move {
            match images.load(url, width, height).await {
                Ok(image) => sink(Event::Image { key, image }),
                Err(e) => tracing::debug!("artwork {path}: {e}"),
            }
        });
    }

    // ------------------------------------------------------------ playback

    fn seek(&mut self, position_ms: i64) {
        let position_ms = position_ms.max(0);
        if self.idle
            && let Some((uid, _)) = self.resume_at
        {
            // Restored, not started yet: move where playing will start.
            self.resume_at = Some((uid, position_ms));
            self.position_ms = position_ms;
            self.emit_position();
            self.update_active_line();
            return;
        }
        if self.idle || self.queue.current().is_none() {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let seconds = position_ms as f64 / 1000.0;
        if self.lossy_transcode() {
            // No byte ranges: restart the stream at the new position.
            if !self.reload_at(position_ms) {
                return;
            }
        } else if let Err(e) = self.engine.seek(seconds) {
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
            context: item.context.clone(),
            duration_millis: u64::try_from(item.track.duration_millis).unwrap_or(0),
            track_id: item.track.id,
            unique_id: item.uid.to_string(),
        })
    }

    /// Applies a queue move: play the target, restart it when it is the
    /// entry already playing, or stop at the start of the queue.
    fn step(&mut self, step: Option<Step>, user_navigated: bool) {
        match step {
            None => {}
            Some(Step::Play(pos)) if Some(pos) == self.queue.position() && !self.idle => {
                self.seek(0);
            }
            Some(Step::Play(pos)) if self.stops_at_track_change() => {
                // "End of track" stops at the next change of track, skips included.
                self.stop_playback();
                self.queue.go(pos);
                self.show_current_stopped();
                self.emit_queue_position();
                self.set_sleep_timer(None);
            }
            Some(Step::Play(pos)) => {
                self.queue.go(pos);
                self.play_current(user_navigated);
                self.emit_queue_position();
            }
            Some(Step::Stop(pos)) => {
                self.stop_playback();
                self.queue.go(pos);
                self.show_current_stopped();
                self.emit_queue_position();
            }
        }
    }

    /// Stops the engine and ends the reported play; the queue is untouched.
    fn stop_playback(&mut self) {
        if !self.idle {
            let _ = self.engine.stop();
            if let Some(r) = self.reporter.as_mut() {
                r.act(|a| a.on_status(AdapterStatus::Stopped));
            }
        }
        self.idle = true;
        self.loading = false;
        self.paused = true;
        self.preloaded = None;
        self.expected_path = None;
        self.position_ms = 0;
        self.pending_seek_ms = None;
        self.save_position();
        self.emit(Event::Paused(true));
        self.emit_position();
    }

    /// Shows the current entry (if any) without playing it.
    fn show_current_stopped(&mut self) {
        let adapter_track = self.adapter_track();
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| a.on_track_change(adapter_track));
        }
        self.started_uid = None;
        let now = self
            .queue
            .current()
            .map(|item| self.now_playing(&item.track));
        self.emit(Event::NowPlaying(now));
        self.emit_position();
        match self.queue.current().map(|i| i.track.id) {
            Some(id) => self.load_lyrics(id),
            None => self.clear_lyrics(),
        }
        self.update_playing_sources();
    }

    /// The transcode in use, if the server offers it.
    fn transcode(&self) -> Option<Transcode> {
        let transcode = self.settings.transcode?;
        let library = self.library.as_ref()?;
        library
            .capabilities
            .can_transcode(transcode.format())
            .then_some(transcode)
    }

    /// MP3 / Opus transcodes are generated live and cannot seek by range.
    fn lossy_transcode(&self) -> bool {
        matches!(
            self.transcode(),
            Some(Transcode::Mp3(_) | Transcode::Ogg(_))
        )
    }

    /// Stream URL of a track, starting at `offset_ms` (lossy transcodes).
    fn track_url(&self, track_id: i64, offset_ms: i64) -> Option<String> {
        let library = self.library.as_ref()?;
        Some(
            match self.transcode() {
                Some(t) => library.client.transcode_url(track_id, t, offset_ms),
                None => library.client.stream_url(track_id),
            }
            .to_string(),
        )
    }

    /// Restarts the current lossy transcode at `position_ms`.
    fn reload_at(&mut self, position_ms: i64) -> bool {
        let Some(id) = self.queue.current().map(|i| i.track.id) else {
            return false;
        };
        let Some(url) = self.track_url(id, position_ms) else {
            return false;
        };
        if let Err(e) = self.engine.load(&url) {
            self.emit(Event::Notice(e));
            return false;
        }
        if self.paused {
            let _ = self.engine.set_paused(true);
        }
        self.loading = true;
        self.preloaded = None;
        self.expected_path = Some(url);
        self.stream_offset_ms = position_ms;
        true
    }

    fn enqueue(&mut self, tracks: Vec<Track>, at: QueuePosition) {
        if tracks.is_empty() {
            return;
        }
        let n = tracks.len();
        match at {
            QueuePosition::Next => self.queue.play_next(tracks),
            QueuePosition::Last => self.queue.add_last(tracks),
        }
        self.queue_changed();
        let what = if n == 1 {
            "1 首".to_owned()
        } else {
            format!("{n} 首")
        };
        self.emit(Event::Notice(match at {
            QueuePosition::Next => format!("已添加 {what}到下一首播放"),
            QueuePosition::Last => format!("已添加 {what}到队列末尾"),
        }));
    }

    fn remove_from_queue(&mut self, uid: u64) {
        let Some(was_current) = self.queue.remove(uid) else {
            return;
        };
        if was_current {
            let playing = !self.idle && !self.paused;
            self.stop_playback();
            if playing && self.queue.current().is_some() {
                self.play_current(true);
            } else {
                self.show_current_stopped();
            }
        }
        self.queue_changed();
    }

    /// The queue's entries or order changed: tell the UI, refresh the preload.
    fn queue_changed(&mut self) {
        let items = self
            .queue
            .in_play_order()
            .map(|item| QueueEntry {
                uid: item.uid,
                title: item.track.title.clone(),
                artist: item.track.artist.clone(),
                duration: format::duration(item.track.duration_millis),
                artwork: item.track.artwork_url.clone(),
            })
            .collect();
        self.emit(Event::Queue {
            items,
            current: self.queue.position(),
        });
        self.sync_preload();
        self.schedule_queue_save();
    }

    fn emit_queue_position(&mut self) {
        self.emit(Event::QueuePosition(self.queue.position()));
        self.schedule_queue_save();
    }

    fn set_sleep_timer(&mut self, timer: Option<SleepTimer>) {
        self.sleep_generation += 1;
        self.sleep = timer.map(|timer| Sleep {
            timer,
            remaining_ms: match timer {
                SleepTimer::Minutes(m) => i64::from(m) * 60_000,
                _ => 0,
            },
            album_id: self.queue.current().map(|i| i.track.album_id),
        });
        if let Some(SleepTimer::Minutes(_)) = timer {
            let generation = self.sleep_generation;
            let tx = self.internal.clone();
            tokio::spawn(async move {
                let mut tick =
                    tokio::time::interval(Duration::from_millis(SLEEP_TICK_MS.unsigned_abs()));
                tick.tick().await;
                loop {
                    tick.tick().await;
                    if tx.send(Internal::SleepTick(generation)).is_err() {
                        break;
                    }
                }
            });
        }
        self.emit_sleep();
        // Whether the engine may continue into the next file changed.
        self.sync_preload();
    }

    fn emit_sleep(&self) {
        let status = self.sleep.as_ref().map(|s| SleepStatus {
            timer: s.timer,
            remaining_ms: matches!(s.timer, SleepTimer::Minutes(_)).then_some(s.remaining_ms),
        });
        self.emit(Event::SleepTimer(status));
    }

    fn stops_at_track_change(&self) -> bool {
        self.sleep
            .as_ref()
            .is_some_and(|s| s.timer == SleepTimer::EndOfTrack)
    }

    /// Playback stops when the current file ends (sleep timer).
    fn pause_at_end(&self) -> bool {
        let Some(sleep) = &self.sleep else {
            return false;
        };
        match sleep.timer {
            SleepTimer::EndOfTrack => true,
            SleepTimer::EndOfAlbum => {
                let next_album = match self.queue.auto_next(self.settings.repeat) {
                    Some(Step::Play(pos)) => self.queue.at(pos).map(|i| i.track.album_id),
                    _ => None,
                };
                // Last track of the album: the next one (if any) is elsewhere.
                // Repeat-one replays the same track and so never ends the album.
                self.settings.repeat != Repeat::One && next_album != sleep.album_id
            }
            SleepTimer::Minutes(_) => false,
        }
    }

    fn schedule_queue_save(&mut self) {
        self.queue_revision += 1;
        let revision = self.queue_revision;
        let tx = self.internal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(QUEUE_SAVE_DELAY).await;
            let _ = tx.send(Internal::SaveQueue(revision));
        });
    }

    /// `queue.json` path and contents for the signed-in server.
    fn queue_file(&self) -> Option<(PathBuf, Vec<u8>)> {
        let library = self.library.as_ref()?;
        let saved = SavedQueue {
            server: library.client.base_url().to_owned(),
            queue: self.queue.snapshot(),
        };
        let bytes = serde_json::to_vec(&saved).ok()?;
        Some((self.config.data_dir.join(QUEUE_FILE), bytes))
    }

    fn save_position(&mut self) {
        if self.library.is_some() {
            self.session_file
                .set(POSITION_KEY, &self.position_ms.max(0).to_string());
        }
    }

    /// Brings back the queue saved for this server, stopped at the saved
    /// position; playing it resumes the persisted reporting chain.
    fn restore_queue(&mut self) {
        let Some(library) = &self.library else { return };
        let path = self.config.data_dir.join(QUEUE_FILE);
        let Some(saved) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SavedQueue>(&b).ok())
        else {
            return;
        };
        if saved.server != library.client.base_url() || saved.queue.tracks.is_empty() {
            return;
        }
        self.queue.restore(saved.queue);
        self.queue_changed();
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        let position_ms = self
            .session_file
            .get(POSITION_KEY)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .clamp(0, item.track.duration_millis.max(0));
        self.resume_at = Some((item.uid, position_ms));
        self.position_ms = position_ms;
        let adapter_track = self.adapter_track();
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| {
                a.on_queue_restored();
                a.on_track_change(adapter_track);
            });
            #[allow(clippy::cast_precision_loss)]
            r.act(|a| a.on_progress(position_ms as f64 / 1000.0));
        }
        self.emit(Event::NowPlaying(Some(self.now_playing(&item.track))));
        self.emit(Event::Paused(true));
        self.emit_position();
        self.load_lyrics(item.track.id);
        self.update_playing_sources();
    }

    /// Keeps the engine's next file equal to the queue's automatic next
    /// entry, so the engine continues into it without a gap.
    fn sync_preload(&mut self) {
        if self.idle || self.loading {
            // Done once the current file has loaded.
            return;
        }
        if self.library.is_none() {
            return;
        }
        let next = match self.queue.auto_next(self.settings.repeat) {
            // The sleep timer stops after this file: the engine must not go on.
            _ if self.pause_at_end() => None,
            Some(Step::Play(pos)) => self.queue.at(pos),
            _ => None,
        };
        let want = next
            .map(|item| (item.uid, item.track.id))
            .and_then(|(uid, id)| Some((uid, self.track_url(id, 0)?)));
        if want.as_ref().map(|(uid, _)| *uid) == self.preloaded {
            return;
        }
        self.preloaded = None;
        if let Err(e) = self.engine.clear_upcoming() {
            tracing::warn!("clearing the preloaded file: {e}");
            return;
        }
        if let Some((uid, url)) = want {
            match self.engine.append(&url) {
                Ok(()) => self.preloaded = Some(uid),
                Err(e) => tracing::warn!("preloading the next file: {e}"),
            }
        }
    }

    /// Loads and starts the current queue entry.
    fn play_current(&mut self, user_navigated: bool) {
        self.start_current(user_navigated, true);
    }

    /// Starts the current queue entry: `load` it, or (gapless) it is the
    /// preloaded file the engine has just moved on to.
    fn start_current(&mut self, user_navigated: bool, load: bool) {
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        // 接着听: the entry starts at its saved position (once).
        let start_ms = if load {
            self.queue.take_current_start()
        } else {
            0
        };
        // A restored entry continues where it was left.
        let restored =
            matches!(self.resume_at, Some((uid, ms)) if uid == item.uid && ms > 0 && load);
        let resume_ms = match self.resume_at {
            Some((uid, ms)) if uid == item.uid && ms > 0 && load => Some(ms),
            _ => (start_ms > 0
                && (item.track.duration_millis <= 0 || start_ms < item.track.duration_millis))
                .then_some(start_ms),
        };
        // Lossy transcodes start the stream there instead of seeking.
        let offset_ms = if self.lossy_transcode() {
            resume_ms.unwrap_or(0)
        } else {
            0
        };
        let Some(url) = self.track_url(item.track.id, offset_ms) else {
            return;
        };
        let from_silence = self.idle || self.paused;
        if load
            && let Err(e) = self
                .engine
                .load(&url)
                .and_then(|()| self.unpause_engine(from_silence))
        {
            self.emit(Event::Notice(e));
            return;
        }
        self.resume_at = None;
        self.idle = false;
        self.loading = true;
        self.paused = false;
        self.preloaded = None;
        self.expected_path = Some(url);
        self.stream_offset_ms = offset_ms;
        self.pending_seek_ms = resume_ms.filter(|_| offset_ms == 0);
        self.report_seek_ms = resume_ms.filter(|_| !restored);
        self.position_ms = resume_ms.unwrap_or(0);
        self.reported_step = self.position_ms / POSITION_STEP_MS;
        self.engine_duration_ms = 0;
        let position_ms = self.position_ms;

        if let Some(sleep) = &self.sleep
            && sleep.timer == SleepTimer::EndOfAlbum
            && sleep.album_id != Some(item.track.album_id)
        {
            // Moved on to another album: the timer no longer applies.
            self.set_sleep_timer(None);
        }

        // The same entry again without a user action: a repeat-one pass.
        let repeated = !user_navigated && self.started_uid == Some(item.uid);
        self.started_uid = Some(item.uid);
        let adapter_track = self.adapter_track();
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| {
                if repeated {
                    a.on_repeated();
                }
                #[allow(clippy::cast_precision_loss)]
                a.on_progress(position_ms as f64 / 1000.0);
                a.on_track_change(adapter_track);
                if user_navigated {
                    a.on_user_navigate();
                }
                a.on_status(AdapterStatus::Playing);
            });
        }

        let track = &item.track;
        self.emit(Event::NowPlaying(Some(self.now_playing(track))));
        self.emit(Event::Paused(false));
        self.emit_position();
        self.save_position();
        self.load_lyrics(track.id);
        self.update_playing_sources();
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

    fn engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::TimePos(seconds) => {
                if self.idle || self.loading {
                    return;
                }
                #[allow(clippy::cast_possible_truncation)]
                let ms = (seconds * 1000.0).round() as i64 + self.stream_offset_ms;
                #[allow(clippy::cast_precision_loss)]
                let seconds = ms as f64 / 1000.0;
                self.position_ms = ms.max(0);
                let save_step = self.position_ms / POSITION_SAVE_STEP_MS;
                if save_step != self.saved_position_step {
                    self.saved_position_step = save_step;
                    self.save_position();
                }
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
                self.fade.engine_paused(paused);
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
                if paused {
                    self.save_position();
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
            EngineEvent::FileLoaded { path } => {
                if !self.loading {
                    return;
                }
                if let (Some(path), Some(expected)) = (&path, &self.expected_path)
                    && path != expected
                {
                    // The engine moved on to a stale preload (the queue
                    // changed right at the track boundary): load the right one.
                    tracing::debug!(%path, %expected, "unexpected file, reloading");
                    if let Err(e) = self.engine.load(expected) {
                        self.emit(Event::Notice(e));
                    }
                    return;
                }
                self.loading = false;
                if let Some(ms) = self.pending_seek_ms.take() {
                    #[allow(clippy::cast_precision_loss)]
                    if let Err(e) = self.engine.seek(ms as f64 / 1000.0) {
                        tracing::warn!("seeking to the resumed position: {e}");
                    }
                }
                if let Some(ms) = self.report_seek_ms.take()
                    && let Some(r) = self.reporter.as_mut()
                {
                    #[allow(clippy::cast_precision_loss)]
                    r.act(|a| a.on_seek(ms as f64 / 1000.0));
                }
                self.sync_preload();
            }
            EngineEvent::EndFile { reason, error } => match reason {
                EngineEnd::Stop => {}
                EngineEnd::Eof => {
                    if self.idle || self.loading {
                        return;
                    }
                    match self.queue.auto_next(self.settings.repeat) {
                        Some(Step::Play(pos)) if self.pause_at_end() => {
                            // Sleep timer: on to the next entry, stopped.
                            self.stop_playback();
                            self.queue.go(pos);
                            self.show_current_stopped();
                            self.emit_queue_position();
                            self.set_sleep_timer(None);
                        }
                        Some(Step::Play(pos)) => {
                            let gapless = self.preloaded.is_some()
                                && self.queue.at(pos).map(|i| i.uid) == self.preloaded;
                            self.queue.go(pos);
                            // Gapless: the engine already moved on to the preload.
                            self.start_current(false, !gapless);
                            self.emit_queue_position();
                        }
                        step @ Some(Step::Stop(_)) => self.step(step, false),
                        None => {}
                    }
                }
                EngineEnd::Error => {
                    if self.idle {
                        return;
                    }
                    // Keeps the engine from moving on into the preload.
                    let _ = self.engine.stop();
                    self.preloaded = None;
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
            Internal::Loaded(target, seq, loaded) => {
                if self.latest.get(&target) == Some(&seq) {
                    self.loaded(loaded);
                }
            }
            Internal::Failed {
                api_token,
                what,
                error,
            } => {
                if error.is_unauthorized() {
                    self.internal(Internal::Unauthorized { api_token });
                } else if self
                    .library
                    .as_ref()
                    .is_some_and(|l| l.client.api_token() == api_token)
                {
                    self.emit(Event::Notice(format!("{what}失败：{error}")));
                }
            }
            Internal::Fetched(Ok(tracks), intent) => self.fetched(tracks, intent),
            Internal::Fetched(Err(e), _) => {
                if e.is_unauthorized() {
                    self.sign_out(SignOutReason::Expired);
                } else if self.library.is_some() {
                    self.emit(Event::Notice(format!("加载曲目失败：{e}")));
                }
            }
            Internal::FavoriteSet { kind, id, favorite } => {
                self.favorite_set(kind, id, favorite);
            }
            Internal::PlaylistsChanged { notice, deleted } => {
                if self.library.is_none() {
                    return;
                }
                if let Some(notice) = notice {
                    self.emit(Event::Notice(notice));
                }
                if let Some(id) = deleted {
                    if self.open_detail == Some(DetailKey::Playlist(id)) {
                        self.open_detail = None;
                    }
                    self.emit(Event::PlaylistDeleted(id));
                }
                self.load_playlists();
                if let Some(key @ DetailKey::Playlist(_)) = self.open_detail {
                    self.open_detail(key);
                }
            }
            Internal::Lyrics { track_id, lyrics } => self.lyrics_loaded(track_id, lyrics),
            Internal::Translation { track_id, lines } => self.translation_loaded(track_id, lines),
            Internal::LyricsResults { track_id, results } => {
                self.lyrics_results(track_id, results);
            }
            Internal::LyricsPreview(seq, lines) => self.lyrics_preview(seq, lines),
            Internal::LyricsChanged(track_id) => self.lyrics_changed(track_id),
            Internal::SourceTracks(key, ids) => {
                self.sources.record(key, ids);
                self.update_playing_sources();
            }
            Internal::TrackWorks(track_id, works) => {
                if self
                    .track_works
                    .as_ref()
                    .is_some_and(|(id, _)| *id == track_id)
                {
                    self.track_works = Some((track_id, works));
                    self.update_playing_sources();
                }
            }
            Internal::PollMixes(generation) => {
                if generation == self.list_generation && self.home.is_some() {
                    self.load_mixes();
                }
            }
            Internal::HistoryCleared => self.history_cleared(),
            Internal::MixChanged {
                id,
                notice,
                replaced_by,
            } => self.mix_changed(id, notice, replaced_by),
            Internal::MixBlocked {
                mix_id,
                index,
                blocked,
            } => self.mix_blocked(mix_id, index, blocked),
            Internal::Notice(text) => {
                if self.library.is_some() {
                    self.emit(Event::Notice(text));
                }
            }
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
            Internal::SleepTick(generation) => {
                if generation != self.sleep_generation {
                    return;
                }
                let playing = !self.idle && !self.paused;
                let Some(sleep) = self.sleep.as_mut() else {
                    return;
                };
                if !playing {
                    return;
                }
                sleep.remaining_ms -= SLEEP_TICK_MS;
                if sleep.remaining_ms > 0 {
                    self.emit_sleep();
                    return;
                }
                self.set_sleep_timer(None);
                if let Err(e) = self.pause_engine() {
                    self.emit(Event::Notice(e));
                }
            }
            Internal::SaveQueue(revision) => {
                if revision != self.queue_revision {
                    return;
                }
                if let Some((path, bytes)) = self.queue_file() {
                    self.saved_queue_revision = revision;
                    tokio::task::spawn_blocking(move || write_atomic(&path, &bytes));
                }
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
        self.queue.clear();
        self.idle = true;
        self.loading = false;
        self.paused = true;
        self.preloaded = None;
        self.expected_path = None;
        self.started_uid = None;
        self.resume_at = None;
        self.pending_seek_ms = None;
        self.sleep = None;
        self.sleep_generation += 1;
        self.position_ms = 0;
        self.engine_duration_ms = 0;
        self.stream_offset_ms = 0;
        self.home = None;
        self.open_mix = None;
        self.sources = m032::SourceTracks::default();
        self.track_works = None;
        // Results of requests still running are dropped.
        self.latest.clear();
        self.track_filter = TrackFilter::default();
        self.favorite_albums = None;
        self.favorite_tracks = None;
        self.favorite_artists = None;
        self.playlists.clear();
        self.open_detail = None;

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
        self.clear_lyrics();
        self.update_playing_sources();
        self.emit(Event::AlbumsReset);
        self.emit(Event::TracksReset);
        self.emit(Event::ArtistsReset);
        self.emit(Event::FavoriteAlbums(Vec::new()));
        self.emit(Event::FavoriteTracks(Vec::new()));
        self.emit(Event::FavoriteArtists(Vec::new()));
        self.emit(Event::Playlists(Vec::new()));
        self.emit(Event::PlaylistChoices(Vec::new()));
        self.emit(Event::Queue {
            items: Vec::new(),
            current: None,
        });
        self.emit(Event::SleepTimer(None));
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
        let features = ServerFeatures {
            browse,
            playlist_artwork: capabilities.has(Feature::PlaylistArtwork),
            transcode: ["mp3", "ogg", "flac"]
                .into_iter()
                .filter(|f| capabilities.can_transcode(f))
                .map(str::to_owned)
                .collect(),
            home: capabilities.has(Feature::Home),
            works: capabilities.has(Feature::Works),
            series: capabilities.has(Feature::Series),
            daily_mixes: capabilities.has(Feature::DailyMixes),
            lyrics_translation: capabilities.has(Feature::LyricsTranslation),
        };
        self.library = Some(Library {
            images: Arc::new(Images::new(client.clone())),
            client: client.clone(),
            capabilities,
            lists: HashMap::new(),
            seeds: Arc::new(Mutex::new(RandomSeeds::new())),
        });
        self.emit(Event::LoggedIn {
            server_url: client.base_url().to_owned(),
            features,
        });

        self.restore_queue();
        self.query_albums(false, AlbumFilter::default());
        self.query_tracks(false, TrackFilter::initial(browse));
        self.load_playlists();
    }
}

/// Writes via a temp file and rename, so a crash never leaves half a file.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("writing {}: {e}", path.display());
    }
}
