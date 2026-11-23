//! ASIO output mode, Opus WASAPI fallback, and hot-switching in core.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, Command, CoreConfig, Engine, EngineEnd, EngineEvent, Event, ExclusiveMode,
    MemorySecrets, Settings, TrackList, TrackSource,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct AsioFakeEngine {
    calls: Mutex<Vec<String>>,
    fiio_connected: AtomicBool,
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
        let tag = if use_asio { "asio" } else { "wasapi" };
        self.push(format!("load {tag} {url}"))
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
        if self.fiio_connected.load(Ordering::Relaxed) {
            Ok(vec![
                AudioDevice {
                    name: "wasapi/{fiio}".into(),
                    description: "扬声器 (FiiO Q series)".into(),
                },
                AudioDevice {
                    name: "wasapi/{realtek}".into(),
                    description: "扬声器 (Realtek)".into(),
                },
            ])
        } else {
            Ok(vec![AudioDevice {
                name: "wasapi/{realtek}".into(),
                description: "扬声器 (Realtek)".into(),
            }])
        }
    }
    fn set_audio_device(&self, name: &str) -> Result<(), String> {
        self.push(format!("device {name}"))
    }
    fn set_exclusive(&self, enabled: bool) -> Result<(), String> {
        self.push(format!("exclusive {enabled}"))
    }
    fn supports_asio(&self, devices: &[AudioDevice], selected: Option<&str>) -> bool {
        if !self.fiio_connected.load(Ordering::Relaxed) {
            return false;
        }
        match selected {
            Some("wasapi/{fiio}") => true,
            Some(_) => false,
            None => devices.iter().any(|d| d.name == "wasapi/{fiio}"),
        }
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
async fn asio_opus_fallback_skips_cross_backend_gapless_and_hot_switches() {
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

    // Without FiiO device, ASIO is unavailable and rejected.
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
        "reject asio",
        |e| matches!(e, Event::Notice(n) if n.contains("不支持 ASIO")),
    );

    // Connect FiiO device -> ASIO becomes available; start in WASAPI exclusive first.
    engine.fiio_connected.store(true, Ordering::Relaxed);
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

    // Play track 1 (FLAC) in WASAPI mode, advance to 42s, then hot-switch to ASIO.
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

    // Hot-switch WASAPI -> ASIO: should hand over at 42s and load track 1 on ASIO + seek.
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
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(url1.clone()),
    });
    wait_call(&engine, "seek 42.000").await;

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

    // End track 2 -> track 3 (FLAC) resumes ASIO.
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

    let calls = engine.calls();
    assert!(calls.contains(&format!("load wasapi {url1}")), "{calls:?}");
    assert!(calls.contains(&format!("load asio {url1}")), "{calls:?}");
    assert!(calls.contains(&format!("load wasapi {url2}")), "{calls:?}");
    assert!(calls.contains(&format!("load asio {url3}")), "{calls:?}");

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_audio_devices_hands_over_when_asio_becomes_available() {
    let server = setup_server().await;
    let engine = Arc::new(AsioFakeEngine::default());
    // Start with FiiO disconnected while user preference is ExclusiveMode::Asio.
    engine.fiio_connected.store(false, Ordering::Relaxed);

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

    // Plug FiiO DAC in and refresh devices -> should hand over active FLAC to ASIO.
    engine.fiio_connected.store(true, Ordering::Relaxed);
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
    wait_call(&engine, &format!("load asio {url1}")).await;

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
