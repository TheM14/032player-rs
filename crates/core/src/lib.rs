//! Client state and the glue between api, session and the audio engine.
//! No UI and no libmpv: the engine comes in through [`Engine`].

mod controller;
mod engine;
pub mod format;
mod images;
mod queue;
mod reporter;
mod storage;

pub use controller::{
    AlbumItem, Command, Commander, CoreConfig, CoreHandle, Event, LoginFailure, NowPlaying,
    TrackItem, start,
};
pub use engine::{Engine, EngineEnd, EngineEvent};
pub use images::{Rgba, decode, server_size};
pub use queue::{Queue, QueueItem};
pub use storage::FileStorage;
