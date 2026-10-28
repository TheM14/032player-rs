//! 032player: native desktop client for 032 Music Server.
//!
//! The console stays visible in every build during M0 so logs (startup
//! timing, errors) can be collected; packaged builds hide it in M4 with
//! `windows_subsystem = "windows"`.

mod albums;
mod engine;
mod ui;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use client_core::{CoreConfig, Engine, EngineEvent};
use slint::ComponentHandle;

/// Code generated from `ui/*.slint`.
#[allow(clippy::all, clippy::todo)]
mod generated {
    slint::include_modules!();
}
pub use generated::*;

type Forward = Box<dyn Fn(EngineEvent) + Send + Sync>;

fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "032player").map_or_else(
        || std::env::temp_dir().join("032player"),
        |d| d.data_dir().to_path_buf(),
    )
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_owned()
}

fn main() -> Result<(), slint::PlatformError> {
    let started = Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu=warn,naga=warn".into()),
        )
        .init();

    let window = AppWindow::new()?;
    let data_dir = data_dir();
    tracing::info!(data_dir = %data_dir.display(), "starting");

    // The player callback runs on mpv's event thread; it forwards into the
    // core once the core exists.
    let forward: Arc<OnceLock<Forward>> = Arc::new(OnceLock::new());
    let player_forward = forward.clone();
    let (engine, engine_error): (Arc<dyn Engine>, Option<String>) =
        match player::Player::new(&player::PlayerOptions::default(), move |event| {
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
        },
        |event| {
            let _ = slint::invoke_from_event_loop(move || ui::apply(event));
        },
    );
    let _ = forward.set(Box::new(core.engine_sender()));

    let server_file = data_dir.join("last_server.txt");
    ui::install(&window, core.commander(), {
        let server_file = server_file.clone();
        move |url| {
            let _ = std::fs::create_dir_all(server_file.parent().unwrap_or(&server_file));
            let _ = std::fs::write(&server_file, url);
        }
    });

    // M0: tokens come from the environment (keyring storage is M1).
    let server = Some(env("M032_SERVER_URL"))
        .filter(|s| !s.is_empty())
        .or_else(|| std::fs::read_to_string(&server_file).ok())
        .unwrap_or_default();
    window.set_login_server(server.trim().into());
    window.set_login_api_token(env("M032_API_TOKEN").into());
    window.set_login_media_token(env("M032_MEDIA_TOKEN").into());
    if !window.get_login_server().is_empty()
        && !window.get_login_api_token().is_empty()
        && !window.get_login_media_token().is_empty()
    {
        window.invoke_login(
            window.get_login_server(),
            window.get_login_api_token(),
            window.get_login_media_token(),
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
    window.run()?;

    tracing::info!("shutting down");
    core.shutdown();
    drop(engine);
    Ok(())
}
