//! OS integration: credential store, media controls, tray, single instance,
//! file dialogs.

pub mod dialog;
pub mod instance;
pub mod media;
pub mod secrets;
#[cfg(windows)]
pub mod tray;
