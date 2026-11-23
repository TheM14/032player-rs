//! The core loop: owns all client state, runs on tokio, takes [`Command`]s
//! from the UI and [`EngineEvent`]s from the audio engine, and pushes
//! [`Event`]s to a sink. The UI never blocks on the network.

mod auth;
mod cast;
mod fade;
mod library;
mod lyrics;
mod m032;
mod output;
mod playback;
mod radio;
mod sleep;

pub use cast::CastRoom;
pub use m032::PlayingSources;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::{
    AuthError, Capabilities, Client, FavoriteKind, Feature, Playlist, SearchCategory, Track,
    Transcode,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::browse::{
    AlbumFilter, AlbumItem, ArtistFilter, ArtistItem, DetailView, PlaylistFilter, PlaylistItem,
    SearchBestMatch, TrackFilter, TrackItem, TrackList,
};
use crate::engine::{
    AsioCapabilities, AsioChannelPair, AsioDriver, AudioDevice, Engine, EngineEvent,
};
use crate::format;
use crate::images::{Images, Rgba};
use crate::m032::{
    AlbumPane, ChronicleView, HomeView, MixItem, RecordItem, Rotation, SeriesItem, SongItem,
    WorkItem,
};
use crate::queue::{Queue, QueueSnapshot, Repeat};
use crate::reporter::{Reporter, ReporterMsg};
use crate::secrets::SecretStore;
use crate::settings::{ExclusiveMode, LyricsSettings, Settings};
use crate::storage::FileStorage;

/// "Previous" restarts the track instead after this much playback.
const RESTART_THRESHOLD_MS: i64 = 10_000;
/// Position updates to the UI and the reporter are coarsened to this step.
const POSITION_STEP_MS: i64 = 250;

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
    #[serde(default)]
    radio: crate::radio::RadioSnapshot,
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
    /// Filter the open artist's backstage credited tracks by role ("" = all).
    FilterArtistCredits {
        artist_id: i64,
        role: String,
    },
    OpenPlaylist(i64),
    /// Other versions / recordings with the same title.
    OpenVersions(TrackList, usize),
    /// Other versions / recordings of a track id (used by navigation history).
    OpenVersionsById(i64),
    /// Play one disc of the open album.
    PlayAlbumDisc(i32),
    /// Unified search overview (`GET /api/v1/search`), answered with [`Event::SearchResults`].
    Search(String),
    /// Single-category paginated search, answered with [`Event::SearchCategoryReset`]
    /// and [`Event::SearchCategoryAppended`].
    SearchCategory {
        query: String,
        category: SearchCategory,
    },
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
    /// Answered with [`Event::AudioDevices`] and [`Event::AsioState`] without
    /// opening ASIO hardware.
    RefreshAudioDevices,
    /// Refreshes audio devices / ASIO drivers when Settings opens and probes
    /// the selected ASIO driver on demand (unless cached or actively streaming).
    RefreshAudioSettings,
    /// `None` = system default. Answered with [`Event::AudioDevice`].
    SetAudioDevice(Option<String>),
    /// Answered with [`Event::Exclusive`] (the old value on failure).
    SetExclusive(bool),
    /// Answered with [`Event::OutputMode`] (old state on failure).
    SetExclusiveMode(ExclusiveMode),
    /// Selects an installed ASIO driver by CLSID and probes its ports.
    SetAsioDriver(String),
    /// Selects a stereo output channel pair `(left, right)` for the active ASIO driver.
    SetAsioChannelPair(usize, usize),
    /// Refreshes installed ASIO drivers and re-probes the selected driver.
    RefreshAsioDriver,
    /// Opens the ASIO driver control panel when supported.
    OpenAsioControlPanel,
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
    /// Load artwork `path` for a `width` x `height` box (physical pixels)
    /// using `fit`; answered with [`Event::Image`] carrying `key`.
    LoadImage {
        key: String,
        path: String,
        width: u32,
        height: u32,
        fit: crate::ArtworkFit,
    },
    /// Like [`Command::LoadImage`], but downloads the original artwork (no
    /// `size`; the full-screen player).
    LoadOriginalImage {
        key: String,
        path: String,
        width: u32,
        height: u32,
        fit: crate::ArtworkFit,
    },
    /// Details of a row ([`Event::TrackInfo`]).
    TrackInfo(TrackList, usize),
    /// Toggle queue auto-continue similar tracks (`∞`, synced with settings).
    SetAutoPlaySimilar(bool),
    /// Start or restart Track Radio from the currently playing track without
    /// interrupting playback.
    StartSimilarRadioCurrent,
    /// Start Track Radio from a track list row (plays immediately + appends).
    StartSimilarRadioTrack(TrackList, usize),
    /// Start Track Radio from a queue item (current = no interruption, other = play).
    StartSimilarRadioQueueItem(u64),
    /// Replace unplayed auto-similar tracks with a fresh batch ("换一批").
    RefreshSimilarRadio,
    /// Append another batch of similar tracks ("再加一批" / retry).
    LoadMoreSimilarRadio,
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
    /// Look for Sonos rooms ([`Event::CastRooms`]).
    DiscoverCast,
    /// Play on a Sonos room (its id) or on this computer (`None`); the
    /// current listen moves along. Answered with [`Event::CastOutput`].
    SetCastOutput(Option<String>),
    /// Sonos transcoding and the speaker addresses to probe.
    SetCastOptions {
        hires: crate::settings::CastHiRes,
        opus: crate::settings::CastOpus,
        hosts: String,
    },
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
    /// One entry currently in the play queue, identified by its UID.
    QueueItem(u64),
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
    /// Server `/browse/artists` supports backstage credit role filters (`artistCreditRoles`).
    pub artist_credit_roles: bool,
    /// Server supports `GET /tracks/{id}/similar` (`similarTracks`).
    pub similar_tracks: bool,
    /// Advertised similarity method (`hybrid` vs `metadata`).
    pub similarity_method: crate::radio::SimilarityMethod,
}

/// A queue row for the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueEntry {
    pub uid: u64,
    pub track_id: i64,
    pub album_id: i64,
    pub artist_id: i64,
    pub artists: Vec<api::ArtistRef>,
    pub title: String,
    pub artist: String,
    pub duration: String,
    /// Server-relative artwork path.
    pub artwork: String,
    /// Whether this entry lies in the auto-continued similar zone.
    pub is_auto: bool,
    /// Highest-priority match reason label (for auto-continued tracks).
    pub reason: Option<String>,
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
    pub artists: Vec<api::ArtistRef>,
    pub album: String,
    pub album_id: i64,
    pub year: i32,
    pub favorite: bool,
    pub artwork: String,
    /// Local cached cover file path for system media controls (never carries
    /// `mediaToken`). May start as `None` and arrive via [`Event::MediaCover`].
    pub artwork_path: Option<PathBuf>,
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
        best_match: Option<SearchBestMatch>,
        artists: Vec<ArtistItem>,
        artists_total: usize,
        albums: Vec<AlbumItem>,
        albums_total: usize,
        tracks: Vec<TrackItem>,
        tracks_total: usize,
        composers: Vec<ArtistItem>,
        composers_total: usize,
    },
    SearchCategoryReset {
        category: SearchCategory,
    },
    SearchCategoryAppended {
        category: SearchCategory,
        artists: Vec<ArtistItem>,
        albums: Vec<AlbumItem>,
        tracks: Vec<TrackItem>,
        total: usize,
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
    /// Loading the image for `key` failed (lets the UI drop pending bindings and retry later).
    ImageFailed {
        key: String,
    },
    /// Local cached cover file path for system media controls (SMTC / MPRIS).
    MediaCover(Option<PathBuf>),
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
    /// Effective output mode, ASIO device availability, and Opus fallback state.
    OutputMode {
        mode: ExclusiveMode,
        asio_available: bool,
        asio_fallback: bool,
    },
    /// Snapshot of discovered ASIO drivers, selected target, and probed capabilities.
    AsioState(AsioStateView),
    /// Lines of the current track (empty: none). `synced` false = plain text.
    Lyrics {
        lines: Vec<LyricRow>,
        synced: bool,
        source: Option<String>,
    },
    LyricsActive(Option<usize>),
    /// Lyrics scroll should return to the start (for example restarting via Previous).
    LyricsReset,
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
    /// The open artist's backstage credited songs (`TrackList::ArtistCredits`).
    ArtistCredits {
        artist_id: i64,
        active_role: String,
        tracks: Vec<TrackItem>,
    },
    /// Track Radio and Queue Auto-Continue status.
    RadioState(crate::radio::RadioStatus),
    /// Sonos rooms found (`searching`: a search is running).
    CastRooms {
        searching: bool,
        rooms: Vec<CastRoom>,
        error: Option<String>,
    },
    /// Playback goes to this Sonos room (`None`: this computer).
    CastOutput(Option<CastRoom>),
    /// Something the user should see (load failure, playback error).
    Notice(String),
}

/// UI-facing snapshot of installed ASIO drivers and the selected driver's probed ports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AsioStateView {
    pub drivers: Vec<AsioDriver>,
    pub selected_clsid: Option<String>,
    pub channel_pairs: Vec<AsioChannelPair>,
    pub selected_channels: (usize, usize),
    pub supported_sample_rates: Vec<u32>,
    pub status: String,
    pub ready: bool,
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
    /// "NetEase - 123 - 同步".
    pub meta: String,
    /// "98.00%".
    pub score: String,
}

/// Messages from spawned tasks back into the loop.
enum Internal {
    Reporter(u64, ReporterMsg),
    /// From the Sonos room of this generation.
    Cast(u64, cast::CastMsg),
    CastRooms(Result<Vec<sonos::Room>, String>),
    LoggedIn(u64, Box<(Client, Capabilities)>),
    LoginFailed(u64, LoginFailure),
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
    Fetched {
        generation: u64,
        api_token: String,
        play_seq: Option<u64>,
        result: Result<Vec<Track>, api::ApiError>,
        intent: Intent,
    },
    FavoriteSet {
        generation: u64,
        kind: FavoriteKind,
        id: i64,
        favorite: bool,
    },
    /// A playlist edit finished: reload the playlists (and the open one).
    PlaylistsChanged {
        generation: u64,
        notice: Option<String>,
        deleted: Option<i64>,
    },
    AudioDevicesLoaded {
        devices: Result<Vec<AudioDevice>, String>,
        drivers: Vec<AsioDriver>,
        probe_asio: bool,
    },
    AsioDriversRefreshed(Vec<AsioDriver>),
    AsioDriverProbed {
        clsid: String,
        seq: u64,
        result: Result<AsioCapabilities, String>,
        handover: bool,
        rollback_clsid: Option<String>,
    },
    AudioDeviceSet(Option<String>, Result<(), String>),
    ExclusiveSet(bool, Result<(), String>),
    ExclusiveModeSet {
        mode: ExclusiveMode,
        auto_clsid: Option<String>,
        probed: Option<(String, AsioCapabilities)>,
        result: Result<(), String>,
    },
    AsioControlPanelOpened(Result<(), String>),
    SaveSettings(u64),
    SaveQueue(u64),
    SleepTick(u64),
    Lyrics {
        seq: u64,
        track_id: i64,
        lyrics: Option<api::Lyrics>,
    },
    Translation {
        seq: u64,
        api_token: String,
        track_id: i64,
        lines: Result<Vec<String>, api::ApiError>,
    },
    LyricsResults {
        seq: u64,
        track_id: i64,
        results: Vec<api::LyricsSearchResult>,
    },
    LyricsPreview(u64, Vec<String>),
    /// The track's lyrics source changed on the server: reload them.
    LyricsChanged {
        generation: u64,
        track_id: i64,
    },
    /// Tracks of an expanded work / series.
    SourceTracks {
        generation: u64,
        key: m032::SourceKey,
        ids: std::collections::HashSet<i64>,
    },
    TrackWorks {
        generation: u64,
        track_id: i64,
        works: Vec<api::TrackWork>,
    },
    PollMixes {
        list_gen: u64,
        poll_gen: u64,
    },
    HistoryCleared(u64),
    MixChanged {
        generation: u64,
        id: i64,
        notice: Option<String>,
        replaced_by: Option<i64>,
    },
    MixBlocked {
        generation: u64,
        mix_id: i64,
        index: usize,
        blocked: bool,
    },
    MediaCoverReady {
        seq: u64,
        source: String,
        path: PathBuf,
    },
    Notice {
        generation: u64,
        text: String,
    },
    RadioFetched {
        list_gen: u64,
        radio_gen: u64,
        api_token: String,
        kind: radio::RadioFetchKind,
        excluded_ids: std::collections::HashSet<i64>,
        limit: usize,
        result: Result<Vec<api::SimilarTrack>, api::ApiError>,
    },
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
    Lyrics,
    LyricsTranslation,
    LyricsResults,
    LyricsPreview,
    ArtistFavorites,
}

#[derive(Default)]
pub(super) struct SearchPreviewData {
    pub query: String,
    pub best_match: Option<SearchBestMatch>,
    pub artists: Vec<ArtistItem>,
    pub artists_total: usize,
    pub albums: Vec<AlbumItem>,
    pub albums_total: usize,
    pub tracks: Vec<Track>,
    pub tracks_total: usize,
    pub composers: Vec<ArtistItem>,
    pub composers_total: usize,
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
    Detail(
        Box<DetailView>,
        Vec<Track>,
        Vec<String>,
        Option<ArtistCredits>,
    ),
    SearchPreview(Box<SearchPreviewData>),
    SearchCategoryPage {
        category: SearchCategory,
        artists: Vec<ArtistItem>,
        albums: Vec<AlbumItem>,
        tracks: Vec<Track>,
        total: usize,
    },
    Home(Box<api::Home>),
    HomeUnavailable,
    HomeRecent(Vec<Track>),
    DailyMixes(api::DailyMixList),
    DailyMixesFailed,
    HomeWorks(api::RotationPage<api::HomeWorkCard>),
    HomeSongs(api::RotationPage<api::HomeSongRow>),
    HomeChronicle(api::Chronicle),
    HomeSpotlight(Box<api::HomeAlbumPane>),
    WorksPage(Vec<WorkItem>, usize),
    SeriesPage(Vec<SeriesItem>, usize),
    HistoryPage(Vec<api::PlaybackRecord>, usize),
    /// Works linked to the open album.
    AlbumWorks(i64, Vec<crate::m032::AlbumWorkEntry>),
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
    favorite_tracks: Arc<Mutex<library::FavoriteTracks>>,
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
    Versions(i64),
    Work(i64),
    Series(i64),
    DailyMix(i64),
}

use auth::unauthorized;

struct ArtistCredits {
    artist_id: i64,
    tracks: Vec<Track>,
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
    auth: auth::AuthState,
    queue: Queue,
    pb: playback::PlaybackState,
    radio: radio::RadioState,
    lyr: lyrics::LyricsState,
    settings: Settings,
    /// Bumped per settings change; `saved_revision` is the last written.
    settings_revision: u64,
    saved_revision: u64,
    output: output::OutputState,
    /// Bumped per queue change; the debounced save writes the latest.
    queue_revision: u64,
    saved_queue_revision: u64,
    /// Serialized background writer for `queue.json` (flushed and joined on shutdown).
    queue_writer_tx: Option<UnboundedSender<(PathBuf, Vec<u8>)>>,
    queue_writer_task: Option<tokio::task::JoinHandle<()>>,
    sleep: sleep::SleepState,
    /// Request counter; `latest` holds the newest request per target.
    seq: u64,
    latest: HashMap<Target, u64>,
    /// Active paginated tasks by target, aborted when superseded or on sign-out.
    target_tasks: HashMap<Target, tokio::task::AbortHandle>,
    /// Filters of the loaded lists (reloaded after favorite changes).
    track_filter: TrackFilter,
    favorite_albums: Option<AlbumFilter>,
    favorite_tracks: Option<TrackFilter>,
    favorite_artists: Option<ArtistFilter>,
    playlist_filter: PlaylistFilter,
    playlists: Vec<Playlist>,
    open_detail: Option<DetailKey>,
    /// The home page was loaded (partial reloads use its state).
    home: Option<m032::HomeState>,
    open_mix: Option<m032::OpenMix>,
    /// Works / series expanded into tracks this session.
    sources: m032::SourceTracks,
    /// Works of a track (`/tracks/{id}/works`).
    track_works: Option<(i64, Vec<api::TrackWork>)>,
    playing_sources: PlayingSources,
    /// Album ids per release group of the open artist page.
    artist_groups: Option<(i64, Vec<Vec<i64>>)>,
    /// Index of the "幕后参与" release group on the open artist page.
    artist_credit_group: Option<(i64, usize)>,
    /// All backstage credited tracks of the open artist page.
    artist_credit_all: Option<ArtistCredits>,
    /// Playback goes to this Sonos room instead of the engine.
    cast: Option<cast::Cast>,
    /// Bumped per attached room; messages of older rooms are dropped.
    cast_generation: u64,
    cast_rooms: Vec<sonos::Room>,
    cast_discovering: bool,
    /// Monotonic counter for local SMTC / MPRIS cover file caching.
    media_cover_seq: u64,
    media_cover_source: Option<String>,
    media_cover_path: Option<PathBuf>,
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
            auth: auth::AuthState::default(),
            queue,
            pb: playback::PlaybackState::default(),
            radio: radio::RadioState::default(),
            lyr: lyrics::LyricsState::default(),
            settings,
            settings_revision: 0,
            saved_revision: 0,
            output: output::OutputState::default(),
            queue_revision: 0,
            saved_queue_revision: 0,
            queue_writer_tx: None,
            queue_writer_task: None,
            sleep: sleep::SleepState::default(),
            seq: 0,
            latest: HashMap::new(),
            target_tasks: HashMap::new(),
            track_filter: TrackFilter::default(),
            favorite_albums: None,
            favorite_tracks: None,
            favorite_artists: None,
            playlist_filter: PlaylistFilter::default(),
            playlists: Vec::new(),
            open_detail: None,
            home: None,
            open_mix: None,
            sources: m032::SourceTracks::default(),
            track_works: None,
            playing_sources: PlayingSources::default(),
            artist_groups: None,
            artist_credit_group: None,
            artist_credit_all: None,
            cast: None,
            cast_generation: 0,
            cast_rooms: Vec::new(),
            cast_discovering: false,
            media_cover_seq: 0,
            media_cover_source: None,
            media_cover_path: None,
        }
    }

    fn emit(&self, event: Event) {
        (self.sink)(event);
    }

    pub(super) fn begin_play_intent(&mut self) -> u64 {
        self.pb.play_intent_seq += 1;
        self.pb.play_intent_seq
    }

    pub(super) fn track_task(&mut self, target: Target, handle: tokio::task::AbortHandle) {
        if let Some(prev) = self.target_tasks.insert(target, handle) {
            prev.abort();
        }
    }

    pub(super) fn abort_task(&mut self, target: Target) {
        if let Some(prev) = self.target_tasks.remove(&target) {
            prev.abort();
        }
    }

    fn abort_all_tasks(&mut self) {
        for (_, handle) in self.target_tasks.drain() {
            handle.abort();
        }
    }

    fn enqueue_queue_write(&mut self, path: PathBuf, bytes: Vec<u8>) {
        if self.queue_writer_tx.is_none() {
            let (tx, mut rx) = mpsc::unbounded_channel::<(PathBuf, Vec<u8>)>();
            let handle = tokio::spawn(async move {
                while let Some(mut item) = rx.recv().await {
                    while let Ok(newer) = rx.try_recv() {
                        item = newer;
                    }
                    let (p, b) = item;
                    let _ = tokio::task::spawn_blocking(move || write_atomic(&p, &b)).await;
                }
            });
            self.queue_writer_tx = Some(tx);
            self.queue_writer_task = Some(handle);
        }
        if let Some(tx) = &self.queue_writer_tx {
            let _ = tx.send((path, bytes));
        }
    }

    fn now_playing(&self, track: &Track) -> NowPlaying {
        NowPlaying {
            track_id: track.id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            artist_id: crate::browse::track_item(track).artist_id,
            artists: crate::browse::track_artists(track).to_vec(),
            album: track.album.clone(),
            album_id: track.album_id,
            year: track.year,
            favorite: track.is_favorite,
            artwork: track.artwork_url.clone(),
            artwork_path: self.media_cover_path.clone(),
            duration_ms: track.duration_millis,
            quality: format::audio_quality(track),
        }
    }

    /// Emits [`Event::NowPlaying`] and caches a local cover file (without
    /// `mediaToken`) for system media controls when the artwork path changes.
    pub(super) fn emit_now_playing(&mut self, track: Option<&Track>) {
        let Some(track) = track else {
            self.media_cover_seq += 1;
            self.media_cover_source = None;
            self.media_cover_path = None;
            self.emit(Event::NowPlaying(None));
            return;
        };
        let same_cached = !track.artwork_url.is_empty()
            && self.media_cover_source.as_deref() == Some(track.artwork_url.as_str())
            && self.media_cover_path.as_ref().is_some_and(|p| p.exists());
        if !same_cached {
            self.media_cover_seq += 1;
            let seq = self.media_cover_seq;
            self.media_cover_source =
                (!track.artwork_url.is_empty()).then(|| track.artwork_url.clone());
            self.media_cover_path = None;
            if !track.artwork_url.is_empty()
                && let Some(library) = &self.library
                && let Some(url) = library.client.image_url(&track.artwork_url, Some(512))
            {
                let images = library.images.clone();
                let data_dir = self.config.data_dir.clone();
                let source = track.artwork_url.clone();
                let tx = self.internal.clone();
                tokio::spawn(async move {
                    let bytes = match images.fetch_bytes(url).await {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::debug!("media cover {source}: {e}");
                            return;
                        }
                    };
                    let path = tokio::task::spawn_blocking(move || {
                        write_media_cover(&data_dir, seq, &bytes)
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some(path) = path {
                        let _ = tx.send(Internal::MediaCoverReady { seq, source, path });
                    }
                });
            }
        }
        let now = self.now_playing(track);
        self.emit(Event::NowPlaying(Some(now)));
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
        self.sleep.cancel();
        self.abort_all_tasks();
        self.home = None;
        self.save_settings();
        self.save_position();
        if self.queue_revision != self.saved_queue_revision
            && let Some((path, bytes)) = self.queue_file()
        {
            self.saved_queue_revision = self.queue_revision;
            self.enqueue_queue_write(path, bytes);
        }
        self.queue_writer_tx = None;
        if let Some(handle) = self.queue_writer_task.take() {
            let _ = handle.await;
        }
        if let Some(reporter) = self.reporter.as_mut() {
            reporter.close(Duration::from_secs(2)).await;
        }
        cleanup_media_covers(&self.config.data_dir, None);
        let _ = self.engine.stop();
    }

    // ------------------------------------------------------------ commands

    fn command(&mut self, command: Command) {
        match command {
            Command::Restore => self.restore_login(),
            Command::Login {
                server_url,
                api_token,
                media_token,
            } => self.login(server_url, api_token, media_token),
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
                if let Some(from) = self.queue.position_of_uid(uid)
                    && self.queue.move_to(uid, to)
                {
                    let to = to.min(self.queue.len().saturating_sub(1));
                    self.radio_on_track_moved(from, to);
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
            Command::FilterArtistCredits { artist_id, role } => {
                self.filter_artist_credits(artist_id, &role);
            }
            Command::OpenPlaylist(id) => self.open_detail(DetailKey::Playlist(id)),
            Command::OpenVersions(list, index) => {
                let id = self
                    .library
                    .as_ref()
                    .and_then(|l| l.list(list).get(index))
                    .map(|t| t.id);
                if let Some(id) = id {
                    self.open_detail(DetailKey::Versions(id));
                }
            }
            Command::OpenVersionsById(id) => self.open_detail(DetailKey::Versions(id)),
            Command::PlayAlbumDisc(disc) => {
                let tracks: Vec<api::Track> = self
                    .library
                    .as_ref()
                    .map(|l| {
                        l.list(TrackList::Detail)
                            .iter()
                            .filter(|t| t.disc_number.max(1) == disc)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if !tracks.is_empty() {
                    self.begin_play_intent();
                    self.fetched(tracks, Intent::play_at(0));
                }
            }
            Command::Search(query) => self.search(query),
            Command::SearchCategory { query, category } => self.search_category(query, category),
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
                // Keep `self.preloaded` intact so `sync_preload` sees URL / backend
                // differences and calls `clear_upcoming()` even when `want` becomes `None`.
                self.sync_preload();
            }
            Command::PlayQueueItem(uid) => {
                if self.queue.go_to_uid(uid).is_some() {
                    self.radio.pending_play_after_end = false;
                    self.begin_play_intent();
                    self.play_current(true);
                    self.emit_queue_position();
                }
            }
            Command::RemoveFromQueue(uid) => self.remove_from_queue(uid),
            Command::ClearQueue => {
                self.begin_play_intent();
                self.stop_playback();
                self.queue.clear();
                self.radio_reset();
                self.pb.resume_at = None;
                self.show_current_stopped();
                self.queue_changed();
            }
            Command::SetRepeat(repeat) => {
                self.update_settings(|s| s.repeat = repeat);
                self.emit(Event::Repeat(repeat));
                self.emit_radio_state();
                self.sync_preload();
                self.cast_play_mode();
                if repeat == Repeat::Off {
                    self.maybe_trigger_auto_continue(false);
                }
            }
            Command::SetShuffle(shuffle) => {
                self.radio.manual_queue_count = self
                    .queue
                    .set_shuffle_with_boundary(shuffle, self.radio.manual_queue_count);
                self.update_settings(|s| s.shuffle = shuffle);
                self.emit(Event::Shuffle(shuffle));
                self.queue_changed();
                self.emit_radio_state();
            }
            Command::TogglePause => {
                if self.queue.current().is_none() {
                    return;
                }
                if self.pb.idle {
                    self.play_current(false);
                    return;
                }
                let result = if self.effective_paused() {
                    self.unpause_engine(self.pb.paused)
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
                if self.pb.position_ms > RESTART_THRESHOLD_MS && !self.pb.idle {
                    // Like mediaPrevious: a seek in the same session, not a new play.
                    self.restart_current();
                } else {
                    let step = self.queue.manual_previous(self.settings.repeat);
                    self.step(step, true);
                }
            }
            Command::Seek { position_ms } => self.seek(position_ms),
            Command::SetVolume(volume) => {
                if self.casting() {
                    self.cast_volume(volume);
                } else if let Err(e) = self.engine.set_volume(volume) {
                    self.emit(Event::Notice(e));
                }
            }
            Command::RefreshAudioDevices => self.refresh_audio_devices(false),
            Command::RefreshAudioSettings => self.refresh_audio_devices(true),
            Command::SetAudioDevice(device) => self.set_audio_device(device),
            Command::SetExclusive(enabled) => self.set_exclusive(enabled),
            Command::SetExclusiveMode(mode) => self.set_exclusive_mode(mode),
            Command::SetAsioDriver(clsid) => self.set_asio_driver(clsid),
            Command::SetAsioChannelPair(left, right) => self.set_asio_channel_pair(left, right),
            Command::RefreshAsioDriver => self.refresh_asio_driver(),
            Command::OpenAsioControlPanel => self.open_asio_control_panel(),
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
                if !on {
                    if self.pb.fade.target() == Some(true) {
                        // A fade-out to pause was already underway (and UI shows paused):
                        // pause immediately before restoring full fade gain.
                        let _ = self.engine.set_paused(true);
                        self.restore_gain();
                    } else if !self.pb.paused {
                        self.restore_gain();
                    }
                }
            }
            Command::SetSleepTimer(timer) => self.set_sleep_timer(timer),
            Command::Stop => {
                self.radio.pending_play_after_end = false;
                if !self.pb.idle {
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
            Command::SetAutoPlaySimilar(enabled) => self.set_auto_play_similar(enabled),
            Command::StartSimilarRadioCurrent => self.start_radio_from_current(),
            Command::StartSimilarRadioTrack(list, index) => {
                self.start_radio_from_list(list, index);
            }
            Command::StartSimilarRadioQueueItem(uid) => self.start_radio_from_queue_item(uid),
            Command::RefreshSimilarRadio => self.refresh_auto_similar(),
            Command::LoadMoreSimilarRadio => self.load_more_auto_similar(),
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
                fit,
            } => self.load_image(key, path, (width, height), fit, false),
            Command::LoadOriginalImage {
                key,
                path,
                width,
                height,
                fit,
            } => self.load_image(key, path, (width, height), fit, true),
            Command::DiscoverCast => self.discover_cast(),
            Command::SetCastOutput(id) => self.set_cast_output(id),
            Command::SetCastOptions { hires, opus, hosts } => {
                let changed = hires != self.settings.cast.hires || opus != self.settings.cast.opus;
                self.update_settings(|s| {
                    s.cast.hires = hires;
                    s.cast.opus = opus;
                    s.cast.hosts = hosts;
                });
                if changed && let Some(cast) = self.cast.as_mut() {
                    // The next play builds the speaker queue with the new plan.
                    cast.invalidate();
                }
            }
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
    fn load_image(
        &self,
        key: String,
        path: String,
        (width, height): (u32, u32),
        fit: crate::ArtworkFit,
        original: bool,
    ) {
        let Some(library) = &self.library else {
            self.emit(Event::ImageFailed { key });
            return;
        };
        let images = library.images.clone();
        let url = if original {
            images.original_url(&path)
        } else {
            images.url(&path, width, height)
        };
        let Some(url) = url else {
            self.emit(Event::ImageFailed { key });
            return;
        };
        let sink = self.sink.clone();
        tokio::spawn(async move {
            match images.load(url, width, height, fit).await {
                Ok(image) => sink(Event::Image { key, image }),
                Err(e) => {
                    tracing::debug!("artwork {path}: {e}");
                    sink(Event::ImageFailed { key });
                }
            }
        });
    }

    // ------------------------------------------------------------ tasks

    fn internal(&mut self, msg: Internal) {
        match msg {
            Internal::Cast(generation, msg) => self.cast_msg(generation, msg),
            Internal::CastRooms(result) => self.cast_rooms_found(result),
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
            Internal::LoginFailed(auth_seq, failure) => {
                if auth_seq == self.auth.current() {
                    self.emit(Event::LoginFailed(failure));
                }
            }
            Internal::LoggedIn(auth_seq, ok) => {
                if auth_seq != self.auth.current() {
                    return;
                }
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
            Internal::Fetched {
                generation,
                api_token,
                play_seq,
                result,
                intent,
            } => match result {
                Ok(tracks) => {
                    if generation != self.list_generation {
                        return;
                    }
                    if let Some(seq) = play_seq
                        && seq != self.pb.play_intent_seq
                    {
                        return;
                    }
                    self.fetched(tracks, intent);
                }
                Err(e) => {
                    if e.is_unauthorized() {
                        self.internal(Internal::Unauthorized { api_token });
                    } else if generation == self.list_generation
                        && self.library.is_some()
                        && play_seq.is_none_or(|seq| seq == self.pb.play_intent_seq)
                    {
                        self.emit(Event::Notice(format!("加载曲目失败：{e}")));
                    }
                }
            },
            Internal::FavoriteSet {
                generation,
                kind,
                id,
                favorite,
            } => {
                if generation == self.list_generation {
                    self.favorite_set(kind, id, favorite);
                }
            }
            Internal::PlaylistsChanged {
                generation,
                notice,
                deleted,
            } => {
                if generation != self.list_generation || self.library.is_none() {
                    return;
                }
                if let Some(notice) = notice {
                    self.emit(Event::Notice(notice));
                }
                if let Some(id) = deleted {
                    if self.open_detail == Some(DetailKey::Playlist(id)) {
                        self.open_detail = None;
                        self.artist_groups = None;
                        self.artist_credit_group = None;
                        self.artist_credit_all = None;
                    }
                    self.emit(Event::PlaylistDeleted(id));
                }
                self.load_playlists();
                if let Some(key @ DetailKey::Playlist(_)) = self.open_detail {
                    self.open_detail(key);
                }
            }
            Internal::Lyrics {
                seq,
                track_id,
                lyrics,
            } => self.lyrics_loaded(seq, track_id, lyrics),
            Internal::Translation {
                seq,
                api_token,
                track_id,
                lines,
            } => self.translation_loaded(seq, api_token, track_id, lines),
            Internal::LyricsResults {
                seq,
                track_id,
                results,
            } => {
                self.lyrics_results(seq, track_id, results);
            }
            Internal::LyricsPreview(seq, lines) => self.lyrics_preview(seq, lines),
            Internal::LyricsChanged {
                generation,
                track_id,
            } => {
                if generation == self.list_generation {
                    self.lyrics_changed(track_id);
                }
            }
            Internal::SourceTracks {
                generation,
                key,
                ids,
            } => {
                if generation == self.list_generation {
                    self.sources.record(key, ids);
                    self.update_playing_sources();
                }
            }
            Internal::TrackWorks {
                generation,
                track_id,
                works,
            } => {
                if generation == self.list_generation
                    && self
                        .track_works
                        .as_ref()
                        .is_some_and(|(id, _)| *id == track_id)
                {
                    self.track_works = Some((track_id, works));
                    self.update_playing_sources();
                }
            }
            Internal::PollMixes { list_gen, poll_gen } => {
                if list_gen == self.list_generation
                    && self
                        .home
                        .as_ref()
                        .is_some_and(|h| h.mix_poll_gen() == poll_gen)
                {
                    self.load_mixes();
                }
            }
            Internal::HistoryCleared(generation) => {
                if generation == self.list_generation {
                    self.history_cleared();
                }
            }
            Internal::MixChanged {
                generation,
                id,
                notice,
                replaced_by,
            } => {
                if generation == self.list_generation {
                    self.mix_changed(id, notice, replaced_by);
                }
            }
            Internal::MixBlocked {
                generation,
                mix_id,
                index,
                blocked,
            } => {
                if generation == self.list_generation {
                    self.mix_blocked(mix_id, index, blocked);
                }
            }
            Internal::Notice { generation, text } => {
                if generation == self.list_generation && self.library.is_some() {
                    self.emit(Event::Notice(text));
                }
            }
            Internal::RadioFetched {
                list_gen,
                radio_gen,
                api_token,
                kind,
                excluded_ids,
                limit,
                result,
            } => {
                self.handle_radio_fetched(
                    list_gen,
                    radio_gen,
                    api_token,
                    kind,
                    excluded_ids,
                    limit,
                    result,
                );
            }
            Internal::AudioDevicesLoaded {
                devices,
                drivers,
                probe_asio,
            } => {
                self.handle_audio_devices_loaded(devices, drivers, probe_asio);
            }
            Internal::AsioDriversRefreshed(drivers) => self.handle_asio_drivers_refreshed(drivers),
            Internal::AsioDriverProbed {
                clsid,
                seq,
                result,
                handover,
                rollback_clsid,
            } => self.handle_asio_driver_probed(clsid, seq, result, handover, rollback_clsid),
            Internal::AudioDeviceSet(device, result) => {
                self.handle_audio_device_set(device, result)
            }
            Internal::ExclusiveSet(enabled, result) => self.handle_exclusive_set(enabled, result),
            Internal::ExclusiveModeSet {
                mode,
                auto_clsid,
                probed,
                result,
            } => self.handle_exclusive_mode_set(mode, auto_clsid, probed, result),
            Internal::AsioControlPanelOpened(result) => {
                if let Err(e) = result {
                    self.emit(Event::Notice(format!("打开 ASIO 控制面板失败：{e}")));
                }
            }
            Internal::SleepTick(generation) => self.on_sleep_tick(generation),
            Internal::SaveQueue(revision) => {
                if revision != self.queue_revision {
                    return;
                }
                if let Some((path, bytes)) = self.queue_file() {
                    self.saved_queue_revision = revision;
                    self.enqueue_queue_write(path, bytes);
                }
            }
            Internal::SaveSettings(revision) => {
                if revision == self.settings_revision {
                    self.save_settings();
                }
            }
            Internal::MediaCoverReady { seq, source, path } => {
                if seq == self.media_cover_seq
                    && self.media_cover_source.as_deref() == Some(source.as_str())
                {
                    self.media_cover_path = Some(path.clone());
                    self.emit(Event::MediaCover(Some(path)));
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// Writes via a unique temp file and rename, so concurrent writes or crashes
/// never collide or leave half a file.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("queue.json");
    let tmp = path.with_file_name(format!("{file_name}.{}.{seq}.tmp", std::process::id()));
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("writing {}: {e}", path.display());
    }
}

fn detect_image_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "jpg"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else {
        "jpg"
    }
}

fn media_cover_prefix() -> String {
    format!("media-cover-{}-", std::process::id())
}

/// Persists cover bytes into `<data_dir>/cache/media-cover-<pid>-<seq>.<ext>`
/// with private permissions on Unix so SMTC / MPRIS receive a local file path
/// instead of a URL carrying `mediaToken`.
fn write_media_cover(data_dir: &std::path::Path, seq: u64, bytes: &[u8]) -> Option<PathBuf> {
    use std::io::Write as _;

    let cache_dir = data_dir.join("cache");
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        tracing::debug!("creating cover cache dir {}: {e}", cache_dir.display());
        return None;
    }
    let prefix = media_cover_prefix();
    let ext = detect_image_ext(bytes);
    let final_path = cache_dir.join(format!("{prefix}{seq}.{ext}"));
    let tmp_path = cache_dir.join(format!("{prefix}{seq}.tmp"));

    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let write_res = opts
        .open(&tmp_path)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.flush()))
        .and_then(|()| std::fs::rename(&tmp_path, &final_path));
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        tracing::debug!("writing media cover {}: {e}", final_path.display());
        return None;
    }
    cleanup_media_covers(data_dir, Some(&final_path));
    Some(final_path)
}

/// Removes temporary cover files created by this process (optionally keeping `keep`).
fn cleanup_media_covers(data_dir: &std::path::Path, keep: Option<&std::path::Path>) {
    let cache_dir = data_dir.join("cache");
    let Ok(entries) = std::fs::read_dir(&cache_dir) else {
        return;
    };
    let prefix = media_cover_prefix();
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.is_some_and(|k| k == path) {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}
