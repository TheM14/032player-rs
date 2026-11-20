//! Client state and the glue between api, session and the audio engine.
//! No UI and no libmpv: the engine comes in through [`Engine`].

pub mod browse;
pub mod cast;
mod controller;
mod engine;
pub mod format;
mod images;
pub mod m032;
mod queue;
mod reporter;
mod secrets;
mod settings;
pub mod shortcuts;
mod storage;

pub use api::{
    AlbumSort, ArtistRef, ArtistRole, ArtistSort, FavoriteKind, SearchCategory, SortOrder,
    TrackSort, Transcode, WorkListQuery, WorkSort,
};
pub use browse::{
    AlbumFilter, AlbumItem, AlbumView, ArtistCreditRole, ArtistFilter, ArtistItem, ArtistView,
    CreditPart, DetailView, PlaylistFilter, PlaylistItem, PlaylistSort, PlaylistView, ReleaseGroup,
    SearchBestMatch, TrackFilter, TrackItem, TrackList, release_groups,
};
pub use controller::{
    CastRoom, Command, Commander, CoreConfig, CoreHandle, Event, LoginFailure, LyricRow,
    LyricsResultItem, LyricsTools, NowPlaying, PlayingSources, QueueEntry, QueuePosition,
    SavedLogin, ServerFeatures, SignOutReason, SleepStatus, SleepTimer, TrackSource, start,
};
pub use engine::{AudioDevice, Engine, EngineEnd, EngineEvent};
pub use images::{ArtworkFit, Rgba, backdrop, decode, decode_fit, fitted_size, server_size};
pub use queue::{Queue, QueueItem, Repeat, Step};
pub use secrets::{MemorySecrets, SecretStore};
pub use settings::{
    CastHiRes, CastOpus, CastRoomSetting, CastSettings, ExclusiveMode, LyricsSettings, RightPanel,
    Settings, ThemeMode, TranslationMode, WindowState,
};
pub use storage::FileStorage;
