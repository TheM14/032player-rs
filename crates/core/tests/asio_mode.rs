//! Generic ASIO driver discovery, multi-channel port selection, hot-switching,
//! and Opus WASAPI fallback in core.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AsioCapabilities, AsioChannelPair, AsioDriver, AsioTarget, AudioDevice, Command, CoreConfig,
    Engine, EngineEnd, EngineEvent, Event, ExclusiveMode, MemorySecrets, Settings, TrackList,
    TrackSource,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLSID_MINIFUSE: &str = "{D323B478-A475-B063-6A36-197FAF1D0689}";
const CLSID_TOPPING: &str = "{6D241B5E-CF73-4043-A85F-EF11D4670955}";

struct AsioFakeEngine {
    calls: Mutex<Vec<String>>,
    drivers_installed: AtomicBool,
    sole_driver_only: AtomicBool,
    topping_ready: AtomicBool,
    target: Mutex<Option<AsioTarget>>,
}

impl Default for AsioFakeEngine {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            drivers_installed: AtomicBool::new(false),
            sole_driver_only: AtomicBool::new(false),
            topping_ready: AtomicBool::new(true),
            target: Mutex::new(None),
        }
    }
}

impl AsioFakeEngine {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn push(&self, call: String) -> Result<(), String> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl Engine for AsioFakeEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.push(format!("load wasapi {url}"))
    }
    fn load_for(&self, url: &str, use_asio: bool) -> Result<(), String> {
        if use_asio {
            let target = self
                .target
                .lock()
                .unwrap()
                .as_ref()
                .map(|t| format!("{}[{}:{}]", t.clsid, t.left_channel, t.right_channel))
                .unwrap_or_else(|| "none".into());
            self.push(format!("load asio {target} {url}"))
        } else {
            self.push(format!("load wasapi {url}"))
        }
    }
    fn append(&self, url: &str) -> Result<(), String> {
        self.push(format!("append wasapi {url}"))
    }
    fn append_for(&self, url: &str, use_asio: bool) -> Result<(), String> {
        let tag = if use_asio { "asio" } else { "wasapi" };
        self.push(format!("append {tag} {url}"))
    }
    fn clear_upcoming(&self) -> Result<(), String> {
        self.push("clear_upcoming".into())
    }
    fn stop(&self) -> Result<(), String> {
        self.push("stop".into())
    }
    fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.push(format!("pause {paused}"))
    }
    fn seek(&self, seconds: f64) -> Result<(), String> {
        self.push(format!("seek {seconds:.3}"))
    }
    fn set_volume(&self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        Ok(vec![AudioDevice {
            name: "wasapi/{realtek}".into(),
            description: "扬声器 (Realtek)".into(),
        }])
    }
    fn set_audio_device(&self, name: &str) -> Result<(), String> {
        self.push(format!("device {name}"))
    }
    fn set_exclusive(&self, enabled: bool) -> Result<(), String> {
        self.push(format!("exclusive {enabled}"))
    }
    fn asio_drivers(&self) -> Vec<AsioDriver> {
        if !self.drivers_installed.load(Ordering::Relaxed) {
            return Vec::new();
        }
        if self.sole_driver_only.load(Ordering::Relaxed) {
            return vec![AsioDriver {
                clsid: CLSID_TOPPING.into(),
                name: "Topping USB Audio Device".into(),
            }];
        }
        vec![
            AsioDriver {
                clsid: CLSID_MINIFUSE.into(),
                name: "MiniFuse ASIO Driver".into(),
            },
            AsioDriver {
                clsid: CLSID_TOPPING.into(),
                name: "Topping USB Audio Device".into(),
            },
        ]
    }
    fn probe_asio_driver(&self, clsid: &str) -> Result<AsioCapabilities, String> {
        let _ = self.push(format!("probe {clsid}"));
        if clsid.eq_ignore_ascii_case(CLSID_MINIFUSE) {
            Ok(AsioCapabilities {
                output_channels: 4,
                channel_pairs: vec![
                    AsioChannelPair {
                        left: 0,
                        right: 1,
                        name: "1-2 · MAIN Left / MAIN Right".into(),
                    },
                    AsioChannelPair {
                        left: 2,
                        right: 3,
                        name: "3-4 · LOOPBACK Left / LOOPBACK Right".into(),
                    },
                ],
                supported_sample_rates: vec![44_100, 48_000, 88_200, 96_000, 176_400, 192_000],
                sample_format: "Int32Lsb".into(),
                buffer_size: 256,
            })
        } else if clsid.eq_ignore_ascii_case(CLSID_TOPPING) {
            if !self.topping_ready.load(Ordering::Relaxed) {
                return Err("ASIO error (-1000): IASIO::init failed".into());
            }
            Ok(AsioCapabilities {
                output_channels: 2,
                channel_pairs: vec![AsioChannelPair {
                    left: 0,
                    right: 1,
                    name: "1-2 · Analogue 1 / Analogue 2".into(),
                }],
                supported_sample_rates: vec![44_100, 48_000, 88_200, 96_000, 176_400, 192_000],
                sample_format: "Int32Lsb".into(),
                buffer_size: 512,
            })
        } else {
            Err(format!("unknown ASIO CLSID {clsid}"))
        }
    }
    fn set_asio_target(&self, target: Option<AsioTarget>) -> Result<(), String> {
        *self.target.lock().unwrap() = target;
        Ok(())
    }
    fn set_exclusive_mode(&self, mode: ExclusiveMode) -> Result<(), String> {
        self.push(format!("mode {mode:?}"))
    }
}

fn track(id: i64, codec: &str) -> Value {
    json!({
        "id": id,
        "title": format!("曲{id}"),
        "artist": "歌手",
        "album": "专辑",
        "durationMillis": 240_000,
        "streamUrl": format!("/api/v1/tracks/{id}/stream"),
        "codec": codec,
        "bitDepth": 24,
        "sampleRate": 48_000,
    })
}

async fn setup_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1",
            "apiRevision": 3,
            "features": { "browse": true },
            "playback": { "clientKinds": ["web", "desktop"] },
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track(1, "flac")],
            "limit": 1,
            "offset": 0,
            "total": 3,
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track(1, "flac"), track(2, "opus"), track(3, "flac")],
            "limit": 500,
            "offset": 0,
            "total": 3,
        })))
        .mount(&server)
        .await;
    for id in [1, 2, 3] {
        Mock::given(path(format!("/api/v1/tracks/{id}/lyrics")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/api/v1/playback/events"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "applied": true, "state": "playing" })),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "limit": 50, "offset": 0, "total": 0
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/playlists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "playlists": [] })))
        .mount(&server)
        .await;
    server
}

fn wait(rx: &mpsc::Receiver<Event>, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(e) if pred(&e) => return e,
            Ok(e) => seen.push(format!("{e:?}")),
            Err(_) => panic!("timed out waiting for {what}; seen={seen:?}"),
        }
    }
}

async fn wait_call(engine: &AsioFakeEngine, call: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !engine.calls().iter().any(|c| c == call) {
        assert!(
            Instant::now() < deadline,
            "{call} not sent: {:?}",
            engine.calls()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_asio_selection_port_switch_and_opus_fallback() {
    let server = setup_server().await;
    let dir = std::env::temp_dir().join(format!("032player-asio-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Arc::new(AsioFakeEngine::default());
    let (tx, rx) = mpsc::channel();
    let config = CoreConfig {
        data_dir: dir.clone(),
        secrets: Arc::new(MemorySecrets::default()),
        settings: Settings::default(),
    };
    let core = client_core::start(engine.clone(), config, move |e| {
        let _ = tx.send(e);
    });

    // Without installed ASIO drivers, ASIO mode is unavailable and rejected.
    core.send(Command::RefreshAudioDevices);
    wait(&rx, "asio unavailable", |e| {
        matches!(
            e,
            Event::OutputMode {
                asio_available: false,
                ..
            }
        )
    });
    core.send(Command::SetExclusiveMode(ExclusiveMode::Asio));
    wait(
        &rx,
        "reject asio without drivers",
        |e| matches!(e, Event::Notice(n) if n.contains("未检测到")),
    );

    // Discover multiple installed ASIO drivers: switching to ASIO requires user choice first.
    engine.drivers_installed.store(true, Ordering::Relaxed);
    core.send(Command::RefreshAudioDevices);
    wait(&rx, "asio available", |e| {
        matches!(
            e,
            Event::OutputMode {
                asio_available: true,
                ..
            }
        )
    });
    core.send(Command::SetExclusiveMode(ExclusiveMode::Asio));
    wait(
        &rx,
        "prompt select driver",
        |e| matches!(e, Event::Notice(n) if n.contains("选择要使用的 ASIO 驱动")),
    );

    // User selects MiniFuse ASIO Driver -> probes on demand and discovers 2 stereo ports.
    core.send(Command::SetAsioDriver(CLSID_MINIFUSE.into()));
    wait(&rx, "minifuse probed", |e| {
        matches!(
            e,
            Event::AsioState(s)
                if s.ready
                    && s.selected_clsid.as_deref() == Some(CLSID_MINIFUSE)
                    && s.channel_pairs.len() == 2
        )
    });

    core.send(Command::SetExclusiveMode(ExclusiveMode::Wasapi));
    wait(&rx, "wasapi mode", |e| {
        matches!(
            e,
            Event::OutputMode {
                mode: ExclusiveMode::Wasapi,
                ..
            }
        )
    });

    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    wait(
        &rx,
        "tracks loaded",
        |e| matches!(e, Event::TracksAppended { items, .. } if items.len() == 3),
    );

    // Play track 1 (FLAC) in WASAPI mode, advance to 42s, then hot-switch to ASIO (MiniFuse 0:1).
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    let url1 = format!("{}/api/v1/tracks/1/stream?mediaToken=media", server.uri());
    let url2 = format!("{}/api/v1/tracks/2/stream?mediaToken=media", server.uri());
    let url3 = format!("{}/api/v1/tracks/3/stream?mediaToken=media", server.uri());
    wait(&rx, "wasapi playing track 1", |e| {
        matches!(e, Event::Paused(false))
    });
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url1.clone()),
    });
    core.engine_event(EngineEvent::TimePos(42.0));
    wait(
        &rx,
        "pos 42s",
        |e| matches!(e, Event::Position { position_ms, .. } if *position_ms >= 42_000),
    );

    core.send(Command::SetExclusiveMode(ExclusiveMode::Asio));
    wait(&rx, "asio mode", |e| {
        matches!(
            e,
            Event::OutputMode {
                mode: ExclusiveMode::Asio,
                asio_fallback: false,
                ..
            }
        )
    });
    wait_call(&engine, &format!("load asio {CLSID_MINIFUSE}[0:1] {url1}")).await;
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url1.clone()),
    });
    wait_call(&engine, "seek 42.000").await;

    // Hot-switch ASIO output port on MiniFuse to channels 2:3 while playing.
    core.send(Command::SetAsioChannelPair(2, 3));
    wait_call(&engine, &format!("load asio {CLSID_MINIFUSE}[2:3] {url1}")).await;
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url1.clone()),
    });

    // Hot-switch to a completely different ASIO driver (Topping) while playing.
    core.send(Command::SetAsioDriver(CLSID_TOPPING.into()));
    wait_call(&engine, &format!("load asio {CLSID_TOPPING}[0:1] {url1}")).await;
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url1.clone()),
    });

    // Next track is Opus -> must NOT preload into ASIO.
    assert!(
        !engine
            .calls()
            .iter()
            .any(|c| c.starts_with("append asio ") && c.contains("/tracks/2/")),
        "opus track must not be preloaded onto ASIO: {:?}",
        engine.calls()
    );

    // End track 1 -> track 2 (Opus) loads on WASAPI fallback.
    core.engine_event(EngineEvent::EndFile {
        reason: EngineEnd::Eof,
        error: None,
    });
    wait(&rx, "opus fallback active", |e| {
        matches!(
            e,
            Event::OutputMode {
                mode: ExclusiveMode::Asio,
                asio_available: true,
                asio_fallback: true,
            }
        )
    });
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url2.clone()),
    });

    // Next track is FLAC (ASIO) -> must NOT preload onto WASAPI.
    assert!(
        !engine
            .calls()
            .iter()
            .any(|c| c.starts_with("append ") && c.contains("/tracks/3/")),
        "FLAC track must not be preloaded onto WASAPI fallback: {:?}",
        engine.calls()
    );

    // End track 2 -> track 3 (FLAC) resumes ASIO on Topping.
    core.engine_event(EngineEvent::EndFile {
        reason: EngineEnd::Eof,
        error: None,
    });
    wait(&rx, "asio resumed on track 3", |e| {
        matches!(
            e,
            Event::OutputMode {
                mode: ExclusiveMode::Asio,
                asio_available: true,
                asio_fallback: false,
            }
        )
    });
    wait_call(&engine, &format!("load asio {CLSID_TOPPING}[0:1] {url3}")).await;

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_audio_devices_hands_over_when_saved_asio_driver_appears() {
    let server = setup_server().await;
    let engine = Arc::new(AsioFakeEngine::default());
    engine.drivers_installed.store(false, Ordering::Relaxed);

    let dir = std::env::temp_dir().join(format!("032player-asio-hotplug-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings {
                exclusive: true,
                asio: true,
                asio_driver: Some(CLSID_TOPPING.into()),
                ..Settings::default()
            },
        },
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        },
    );

    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    wait(
        &rx,
        "tracks loaded",
        |e| matches!(e, Event::TracksAppended { items, .. } if items.len() == 3),
    );

    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    let url1 = format!("{}/api/v1/tracks/1/stream?mediaToken=media", server.uri());
    wait_call(&engine, &format!("load wasapi {url1}")).await;
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url1.clone()),
    });
    core.engine_event(EngineEvent::TimePos(12.0));
    wait(
        &rx,
        "pos 12s",
        |e| matches!(e, Event::Position { position_ms, .. } if *position_ms >= 12_000),
    );

    // Saved ASIO driver becomes discovered on refresh -> hands over active FLAC to ASIO.
    engine.drivers_installed.store(true, Ordering::Relaxed);
    core.send(Command::RefreshAudioDevices);
    wait(&rx, "asio available", |e| {
        matches!(
            e,
            Event::OutputMode {
                mode: ExclusiveMode::Asio,
                asio_available: true,
                asio_fallback: false,
            }
        )
    });
    wait_call(&engine, &format!("load asio {CLSID_TOPPING}[0:1] {url1}")).await;

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_refresh_avoids_probing_until_settings_opened() {
    let engine = Arc::new(AsioFakeEngine::default());
    engine.drivers_installed.store(true, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("032player-asio-lazy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (tx, rx) = mpsc::channel();
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings {
                exclusive: false,
                asio: false,
                asio_driver: Some(CLSID_MINIFUSE.into()),
                ..Settings::default()
            },
        },
        move |e| {
            let _ = tx.send(e);
        },
    );

    // Startup refresh enumerates drivers without opening ASIO hardware.
    core.send(Command::RefreshAudioDevices);
    wait(&rx, "startup asio state", |e| {
        matches!(
            e,
            Event::AsioState(s)
                if s.drivers.len() == 2
                    && s.selected_clsid.as_deref() == Some(CLSID_MINIFUSE)
                    && !s.ready
        )
    });
    assert!(
        !engine.calls().iter().any(|c| c.starts_with("probe ")),
        "startup refresh must not probe hardware: {:?}",
        engine.calls()
    );

    // Entering Settings triggers an on-demand probe once.
    core.send(Command::RefreshAudioSettings);
    wait(&rx, "settings probed minifuse", |e| {
        matches!(
            e,
            Event::AsioState(s)
                if s.ready
                    && s.selected_clsid.as_deref() == Some(CLSID_MINIFUSE)
                    && s.channel_pairs.len() == 2
        )
    });
    assert_eq!(
        engine
            .calls()
            .iter()
            .filter(|c| c.as_str() == format!("probe {CLSID_MINIFUSE}"))
            .count(),
        1
    );

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_driver_switch_while_playing_rolls_back_and_preserves_ports() {
    let server = setup_server().await;
    let engine = Arc::new(AsioFakeEngine::default());
    engine.drivers_installed.store(true, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("032player-asio-rollback-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (tx, rx) = mpsc::channel();
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings {
                exclusive: true,
                asio: true,
                asio_driver: Some(CLSID_MINIFUSE.into()),
                ..Settings::default()
            },
        },
        move |e| {
            let _ = tx.send(e);
        },
    );

    core.send(Command::RefreshAudioSettings);
    wait(
        &rx,
        "minifuse probed",
        |e| matches!(e, Event::AsioState(s) if s.ready && s.channel_pairs.len() == 2),
    );
    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    wait(
        &rx,
        "tracks loaded",
        |e| matches!(e, Event::TracksAppended { items, .. } if items.len() == 3),
    );

    let url1 = format!("{}/api/v1/tracks/1/stream?mediaToken=media", server.uri());
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    wait_call(&engine, &format!("load asio {CLSID_MINIFUSE}[0:1] {url1}")).await;
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url1.clone()),
    });

    // Attempt to switch to a disconnected Topping DAC while MiniFuse is actively streaming.
    engine.topping_ready.store(false, Ordering::Relaxed);
    core.send(Command::SetAsioDriver(CLSID_TOPPING.into()));
    wait(
        &rx,
        "switch failure notice",
        |e| matches!(e, Event::Notice(n) if n.contains("切换 ASIO 驱动失败")),
    );
    // Selection and cached stereo output ports roll back cleanly to MiniFuse.
    let target = engine.target.lock().unwrap().clone();
    assert_eq!(
        target.as_ref().map(|t| t.clsid.as_str()),
        Some(CLSID_MINIFUSE)
    );
    core.send(Command::RefreshAudioSettings);
    wait(&rx, "minifuse ports preserved", |e| {
        matches!(
            e,
            Event::AsioState(s)
                if s.ready
                    && s.selected_clsid.as_deref() == Some(CLSID_MINIFUSE)
                    && s.channel_pairs.len() == 2
        )
    });

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_asio_migrates_sole_driver_and_idle_switch_probes_once() {
    let engine = Arc::new(AsioFakeEngine::default());
    engine.drivers_installed.store(true, Ordering::Relaxed);
    engine.sole_driver_only.store(true, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("032player-asio-sole-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (tx, rx) = mpsc::channel();
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings {
                exclusive: true,
                asio: true,
                asio_driver: None,
                ..Settings::default()
            },
        },
        move |e| {
            let _ = tx.send(e);
        },
    );

    // Legacy v0.2.27 `exclusive: true, asio: true` without `asio_driver` auto-migrates
    // when a single ASIO driver is installed, without probing hardware on startup.
    core.send(Command::RefreshAudioDevices);
    wait(&rx, "migrated sole driver", |e| {
        matches!(
            e,
            Event::OutputMode {
                mode: ExclusiveMode::Asio,
                asio_available: true,
                ..
            }
        )
    });
    assert!(
        !engine.calls().iter().any(|c| c.starts_with("probe ")),
        "migration must not probe hardware on startup: {:?}",
        engine.calls()
    );
    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();

    // Idle switch to ASIO with an unselected sole driver that is disconnected must not
    // persist the driver selection; once connected it probes exactly once.
    let engine2 = Arc::new(AsioFakeEngine::default());
    engine2.drivers_installed.store(true, Ordering::Relaxed);
    engine2.sole_driver_only.store(true, Ordering::Relaxed);
    engine2.topping_ready.store(false, Ordering::Relaxed);
    let (tx2, rx2) = mpsc::channel();
    let core2 = client_core::start(
        engine2.clone(),
        CoreConfig {
            data_dir: dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings::default(),
        },
        move |e| {
            let _ = tx2.send(e);
        },
    );
    core2.send(Command::RefreshAudioDevices);
    wait(
        &rx2,
        "sole driver discovered",
        |e| matches!(e, Event::AsioState(s) if s.drivers.len() == 1 && s.selected_clsid.is_none()),
    );
    core2.send(Command::SetExclusiveMode(ExclusiveMode::Asio));
    wait(
        &rx2,
        "idle switch failed notice",
        |e| matches!(e, Event::Notice(n) if n.contains("切换输出模式失败")),
    );
    wait(
        &rx2,
        "idle switch failed without mutating selection",
        |e| matches!(e, Event::AsioState(s) if s.selected_clsid.is_none()),
    );

    engine2.topping_ready.store(true, Ordering::Relaxed);
    core2.send(Command::SetExclusiveMode(ExclusiveMode::Asio));
    wait(&rx2, "idle switch succeeded", |e| {
        matches!(
            e,
            Event::OutputMode {
                mode: ExclusiveMode::Asio,
                ..
            }
        )
    });
    wait(&rx2, "caps populated from single probe", |e| {
        matches!(
            e,
            Event::AsioState(s)
                if s.ready
                    && s.selected_clsid.as_deref() == Some(CLSID_TOPPING)
                    && s.channel_pairs.len() == 1
        )
    });
    // One failed probe + one successful probe = 2 total (not 3 from double-probing).
    assert_eq!(
        engine2
            .calls()
            .iter()
            .filter(|c| c.as_str() == format!("probe {CLSID_TOPPING}"))
            .count(),
        2,
        "{:?}",
        engine2.calls()
    );

    tokio::task::spawn_blocking(move || core2.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
