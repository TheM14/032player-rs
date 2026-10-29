//! 032player: native desktop client for 032 Music Server.
//!
//! Release builds on Windows have no console window; the log goes to
//! `<data dir>/logs/032player.log` (settings page: "打开日志文件夹") and,
//! in debug builds, to the console as well.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cards;
mod engine;
mod fonts;
mod system;
mod ui;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use client_core::{Command, CoreConfig, Engine, EngineEvent, SecretStore, Settings};
use slint::ComponentHandle;

/// Code generated from `ui/*.slint`.
#[allow(clippy::all, clippy::todo)]
mod generated {
    slint::include_modules!();
}
pub use generated::*;

type Forward = Box<dyn Fn(EngineEvent) + Send + Sync>;

/// Log directory (also opened from the settings page).
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Log to `logs/032player.log` (appended; moved to `032player.old.log`
/// past 2 MB) and to stderr; panics are logged too.
fn init_logging(data_dir: &std::path::Path) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,wgpu=warn,naga=warn".into());
    let dir = data_dir.join("logs");
    let path = dir.join("032player.log");
    let file = std::fs::create_dir_all(&dir).ok().and_then(|()| {
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
            // Fails while another instance writes it; then keep appending.
            let _ = std::fs::rename(&path, dir.join("032player.old.log"));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
    });
    let file_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .init();
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
    }));
}

fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "032player").map_or_else(
        || std::env::temp_dir().join("032player"),
        |d| d.data_dir().to_path_buf(),
    )
}

/// Tokens in the system credential store. The store opens lazily on the
/// first call (off the UI thread); if it is missing, every call fails and
/// the core logs it, so tokens are simply not remembered.
struct SystemSecrets(os::secrets::Keyring);

impl SecretStore for SystemSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        self.0.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.0.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.0.delete(key)
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_owned()
}

fn main() -> Result<(), slint::PlatformError> {
    let started = Instant::now();
    let data_dir = data_dir();
    init_logging(&data_dir);
    tracing::info!(data_dir = %data_dir.display(), "starting");
    // A second start shows the running instance's window and exits.
    let _instance = match os::instance::acquire(&data_dir, system::activate) {
        os::instance::Instance::Primary(guard) => guard,
        os::instance::Instance::Secondary { notified } => {
            tracing::info!(notified, "already running");
            return Ok(());
        }
    };

    // The font collection belongs to the platform: select it first, then
    // register the bundled fonts before any text is measured.
    slint::BackendSelector::new().select()?;
    fonts::register();
    let window = AppWindow::new()?;
    let settings = Settings::load(&data_dir);
    let player_options = player::PlayerOptions {
        audio_device: settings.audio_device.clone(),
        exclusive: settings.exclusive,
        volume: settings.volume,
        ..player::PlayerOptions::default()
    };

    // The player callback runs on mpv's event thread; it forwards into the
    // core once the core exists.
    let forward: Arc<OnceLock<Forward>> = Arc::new(OnceLock::new());
    let player_forward = forward.clone();
    let (engine, engine_error): (Arc<dyn Engine>, Option<String>) =
        match player::Player::new(&player_options, move |event| {
            if let (Some(f), Some(e)) = (player_forward.get(), engine::map_event(event)) {
                f(e);
            }
        }) {
            Ok(p) => (Arc::new(engine::MpvEngine(p)), None),
            Err(e) => {
                let msg = format!("mpv 初始化失败：{e}");
                tracing::error!("{msg}");
                (Arc::new(engine::NullEngine(msg.clone())), Some(msg))
            }
        };

    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: data_dir.clone(),
            secrets: Arc::new(SystemSecrets(os::secrets::Keyring)),
            settings: settings.clone(),
        },
        |event| {
            let _ = slint::invoke_from_event_loop(move || {
                system::on_event(&event);
                ui::apply(event);
            });
        },
    );
    let _ = forward.set(Box::new(core.engine_sender()));

    ui::install(&window, core.commander(), &settings);

    // Development: a complete set of M032_* variables signs in with them;
    // otherwise the saved sign-in is restored.
    let (server, api_token, media_token) = (
        env("M032_SERVER_URL"),
        env("M032_API_TOKEN"),
        env("M032_MEDIA_TOKEN"),
    );
    if server.is_empty() || api_token.is_empty() || media_token.is_empty() {
        core.send(Command::Restore);
    } else {
        window.set_login_server(server.into());
        window.invoke_login(
            window.get_login_server(),
            api_token.into(),
            media_token.into(),
        );
    }
    if let Some(msg) = engine_error {
        window.set_notice(msg.into());
    }

    let _ = slint::invoke_from_event_loop(move || {
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            "window shown (first event loop turn)"
        );
    });
    // Last size; maximized again after the window exists.
    window.window().set_size(slint::LogicalSize::new(
        settings.window.width,
        settings.window.height,
    ));
    window.show()?;
    if settings.window.maximized {
        window.window().set_maximized(true);
    }
    system::install(&window, core.commander());
    // Runs until quit (closing may only hide the window to the tray).
    slint::run_event_loop_until_quit()?;
    if let Some(state) = ui::window_state() {
        core.send(Command::SetWindow(state));
    }
    let _ = window.hide();

    tracing::info!("shutting down");
    core.shutdown();
    drop(engine);
    Ok(())
}
