//! Client state and the glue between api, session and the audio engine.
//! No UI and no libmpv: the engine comes in through [`Engine`].

pub mod browse;
mod controller;
mod engine;
pub mod format;
mod images;
mod queue;
mod reporter;
mod secrets;
mod settings;
mod storage;

pub use api::{AlbumSort, ArtistSort, FavoriteKind, SortOrder, TrackSort, Transcode};
pub use browse::{
    AlbumFilter, AlbumItem, AlbumView, ArtistFilter, ArtistItem, ArtistView, DetailView,
    PlaylistFilter, PlaylistItem, PlaylistSort, PlaylistView, TrackFilter, TrackItem, TrackList,
};
pub use controller::{
    Command, Commander, CoreConfig, CoreHandle, Event, LoginFailure, NowPlaying, QueueEntry,
    QueuePosition, SavedLogin, ServerFeatures, SignOutReason, SleepStatus, SleepTimer, TrackSource,
    start,
};
pub use engine::{AudioDevice, Engine, EngineEnd, EngineEvent};
pub use images::{Rgba, decode, server_size};
pub use queue::{Queue, QueueItem, Repeat, Step};
pub use secrets::{MemorySecrets, SecretStore};
pub use settings::Settings;
pub use storage::FileStorage;
