//! OS integration: credential store, media controls, tray, single instance,
//! file dialogs, the browser, the system proxy.

pub mod browser;
pub mod dialog;
pub mod instance;
pub mod media;
pub mod proxy;
pub mod secrets;
#[cfg(windows)]
pub mod tray;
