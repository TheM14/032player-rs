//! 032 playback reporting (apiRevision 3): the session state machine and the
//! adapter that maps player signals onto it. Pure logic, no IO.

mod adapter;
mod env;
mod session;
mod types;

pub use adapter::{AdapterStatus, AdapterTrack, NEAR_END_MS, NowFn, PlaybackAdapter};
pub use env::{KeyValueStorage, LiveProbe, MemoryStorage, Probe, Scheduler, TimerId};
pub use session::{ClientKindFn, PlaybackSession, SessionOptions, UuidFn};
pub use types::*;
