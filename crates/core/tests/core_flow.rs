//! Core end to end against a mock server and a fake engine.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, Command, CoreConfig, Engine, EngineEnd, EngineEvent, Event, LoginFailure,
    MemorySecrets, Settings, TrackList, TrackSource,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct FakeEngine {
    calls: Mutex<Vec<String>>,
    /// Supports the fade gain (recorded as `gain {g}`).
    fade: bool,
}

impl FakeEngine {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn push(&self, call: String) -> Result<(), String> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl Engine for FakeEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.push(format!("load {url}"))
    }
    fn append(&self, url: &str) -> Result<(), String> {
        self.push(format!("append {url}"))
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
        self.push(format!("seek {seconds}"))
    }
    fn set_volume(&self, volume: f64) -> Result<(), String> {
        self.push(format!("volume {volume}"))
    }
    fn set_fade_gain(&self, gain: f64) -> Result<(), String> {
        if !self.fade {
            return Err("unsupported".into());
        }
        self.push(format!("gain {gain:.3}"))
    }
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        Ok(Vec::new())
    }
    fn set_audio_device(&self, name: &str) -> Result<(), String> {
        self.push(format!("device {name}"))
    }
    fn set_exclusive(&self, enabled: bool) -> Result<(), String> {
        self.push(format!("exclusive {enabled}"))
    }
}

fn track(id: i64) -> Value {
    json!({
        "id": id, "title": format!("曲{id}"), "artist": "歌手", "album": "专辑", "year": 2024,
        "durationMillis": 240000, "streamUrl": format!("/api/v1/tracks/{id}/stream"),
        "codec": "flac", "bitDepth": 16, "sampleRate": 44100,
    })
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "apiRevision": 3,
            "features": { "browse": true },
            "playback": { "clientKinds": ["web", "desktop"] },
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track(1)], "limit": 1, "offset": 0, "total": 2
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks/1/stream"))
        .respond_with(ResponseTemplate::new(206))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": 9, "title": "A", "artist": "B", "artworkUrl": "/api/v1/artwork/1" }],
            "limit": 500, "offset": 0, "total": 1
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track(1), track(2)], "limit": 500, "offset": 0, "total": 2
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks/1/lyrics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lines": [{ "text": "一", "timeMs": 0 }, { "text": "二", "timeMs": 5000 }],
            "synced": true, "sourceName": "LRCLIB"
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks/2/lyrics"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playback/events"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "applied": true, "state": "playing" })),
        )
        .mount(&server)
        .await;
    server
}

struct Events(mpsc::Receiver<Event>);

impl Events {
    /// Waits for the first event matching `pred`, skipping others.
    fn wait(&self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.0.recv_timeout(left) {
                Ok(e) if pred(&e) => return e,
                Ok(_) => {}
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }
}

async fn playback_events(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/playback/events")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

async fn wait_for_events(server: &MockServer, n: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = playback_events(server).await;
        if events.len() >= n || Instant::now() > deadline {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn login_play_auto_advance_and_report() {
    let server = server().await;
    let engine = Arc::new(FakeEngine::default());
    let data_dir = std::env::temp_dir().join(format!("032player-core-{}", std::process::id()));
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let config = CoreConfig {
        data_dir,
        secrets: Arc::new(MemorySecrets::default()),
        settings: Settings::default(),
    };
    let core = client_core::start(engine.clone(), config, move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    let events = Events(rx);

    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    events.wait("login", |e| matches!(e, Event::LoggedIn { .. }));
    // Albums and tracks load concurrently: accept either order.
    let (mut albums, mut tracks) = (false, false);
    while !(albums && tracks) {
        match events.wait("lists", |e| {
            matches!(
                e,
                Event::AlbumsAppended { .. } | Event::TracksAppended { .. }
            )
        }) {
            Event::AlbumsAppended { items, total } => {
                assert_eq!((items[0].title.as_str(), total), ("A", 1));
                albums = true;
            }
            Event::TracksAppended { items, total } => {
                assert_eq!((items.len(), total), (2, 2));
                tracks = true;
            }
            _ => unreachable!(),
        }
    }

    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    let now = events.wait("now playing", |e| matches!(e, Event::NowPlaying(Some(_))));
    let Event::NowPlaying(Some(now)) = now else {
        unreachable!()
    };
    assert_eq!(now.title, "曲1");
    assert_eq!(now.year, 2024);
    assert_eq!(now.quality.as_deref(), Some("FLAC / 16bit / 44.1kHz"));
    events.wait(
        "lyrics",
        |e| matches!(e, Event::Lyrics { lines, synced: true, .. } if lines.len() == 2),
    );
    events.wait("first line", |e| *e == Event::LyricsActive(Some(0)));

    let calls = engine.calls();
    assert!(
        calls[0].starts_with("load ")
            && calls[0].ends_with("/api/v1/tracks/1/stream?mediaToken=media"),
        "{calls:?}"
    );

    // Stale signals of the previous file before the new one is loaded.
    core.engine_event(EngineEvent::TimePos(200.0));
    core.engine_event(EngineEvent::EndFile {
        reason: EngineEnd::Eof,
        error: None,
    });
    core.engine_event(EngineEvent::FileLoaded { path: None });
    core.engine_event(EngineEvent::TimePos(6.0));
    events.wait("second line", |e| *e == Event::LyricsActive(Some(1)));
    core.engine_event(EngineEvent::TimePos(239.0));
    events.wait("position", |e| {
        matches!(
            e,
            Event::Position {
                position_ms: 239_000,
                duration_ms: 240_000
            }
        )
    });
    core.engine_event(EngineEvent::EndFile {
        reason: EngineEnd::Eof,
        error: None,
    });
    events.wait(
        "next track",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 2),
    );

    let sent = wait_for_events(&server, 3).await;
    let kinds: Vec<String> = sent
        .iter()
        .map(|e| format!("{}:{}", e["type"].as_str().unwrap(), e["trackId"]))
        .collect();
    assert_eq!(kinds, ["start:1", "end:1", "start:2"]);
    assert_eq!(sent[0]["clientKind"], "desktop");
    assert_eq!(sent[1]["endReason"], "completed");
    assert_eq!(sent[1]["positionMillis"], 239_000);

    // Pause comes back from the engine as a property change.
    core.send(Command::TogglePause);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !engine.calls().contains(&"pause true".to_owned()) {
        assert!(Instant::now() < deadline, "pause not sent to the engine");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    core.engine_event(EngineEvent::Paused(true));
    events.wait("paused", |e| *e == Event::Paused(true));
    let sent = wait_for_events(&server, 4).await;
    assert_eq!(sent[3]["type"], "pause");

    // Close: client_closed over the keepalive path (media token in the URL).
    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let closed = requests
        .iter()
        .rfind(|r| r.url.path() == "/api/v1/playback/events")
        .unwrap();
    assert_eq!(closed.url.query(), Some("mediaToken=media"));
    let body: Value = serde_json::from_slice(&closed.body).unwrap();
    assert_eq!(body["endReason"], "client_closed");
    assert!(!closed.headers.contains_key("authorization"));
}

#[tokio::test(flavor = "multi_thread")]
async fn login_failures_are_reported() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let core = client_core::start(
        Arc::new(FakeEngine::default()),
        CoreConfig {
            data_dir: std::env::temp_dir(),
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings::default(),
        },
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        },
    );
    let events = Events(rx);
    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    events.wait("failure", |e| {
        *e == Event::LoginFailed(LoginFailure::ApiTokenInvalid)
    });
    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "same".into(),
        media_token: "same".into(),
    });
    events.wait("failure", |e| {
        *e == Event::LoginFailed(LoginFailure::TokensMustDiffer)
    });
    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
}

/// Fade gains (in call order) and the position of `call` among all calls.
fn gains(calls: &[String]) -> Vec<f64> {
    calls
        .iter()
        .filter_map(|c| c.strip_prefix("gain ")?.parse().ok())
        .collect()
}

async fn wait_call(engine: &FakeEngine, call: &str) {
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
async fn pause_and_play_fade_without_touching_volume() {
    let server = server().await;
    let engine = Arc::new(FakeEngine {
        fade: true,
        ..FakeEngine::default()
    });
    let data_dir = std::env::temp_dir().join(format!("032player-core-fade-{}", std::process::id()));
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir,
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings::default(),
        },
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        },
    );
    let events = Events(rx);
    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    events.wait("tracks", |e| matches!(e, Event::TracksAppended { .. }));

    // Starting from stopped fades in from silence.
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    events.wait("now playing", |e| matches!(e, Event::NowPlaying(Some(_))));
    core.engine_event(EngineEvent::FileLoaded { path: None });
    wait_call(&engine, "gain 1.000").await;
    let calls = engine.calls();
    let g = gains(&calls);
    assert_eq!(g.first().copied(), Some(0.0), "{calls:?}");
    assert!(g.windows(2).all(|w| w[0] <= w[1]), "{g:?}");

    // Pause: the icon flips at once, the engine pauses after the fade-out.
    engine.calls.lock().unwrap().clear();
    core.send(Command::TogglePause);
    events.wait("paused", |e| *e == Event::Paused(true));
    wait_call(&engine, "pause true").await;
    let calls = engine.calls();
    let g = gains(&calls);
    assert!(g.len() > 5 && g.windows(2).all(|w| w[0] >= w[1]), "{g:?}");
    assert_eq!(g.last().copied(), Some(0.0));
    assert_eq!(calls.last().map(String::as_str), Some("pause true"));
    core.engine_event(EngineEvent::Paused(true));
    // Let core take the engine signal before the next command.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Play: unpause at silence, then fade in.
    engine.calls.lock().unwrap().clear();
    core.send(Command::TogglePause);
    wait_call(&engine, "gain 1.000").await;
    let calls = engine.calls();
    assert_eq!(calls[0], "gain 0.000", "{calls:?}");
    assert_eq!(calls[1], "pause false", "{calls:?}");
    core.engine_event(EngineEvent::Paused(false));
    // Let core take the engine signal before the next command.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Toggling back mid fade-out never pauses and ends at full level.
    engine.calls.lock().unwrap().clear();
    core.send(Command::TogglePause);
    tokio::time::sleep(Duration::from_millis(60)).await;
    core.send(Command::TogglePause);
    events.wait("resumed", |e| *e == Event::Paused(false));
    wait_call(&engine, "gain 1.000").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let calls = engine.calls();
    assert!(!calls.iter().any(|c| c == "pause true"), "{calls:?}");
    assert!(!calls.iter().any(|c| c.starts_with("volume")), "{calls:?}");

    // Switched off: plain pause, no gain changes.
    core.send(Command::SetFadeOnPause(false));
    engine.calls.lock().unwrap().clear();
    core.send(Command::TogglePause);
    wait_call(&engine, "pause true").await;
    assert_eq!(engine.calls(), ["pause true"]);

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn previous_restart_resets_lyrics() {
    let server = server().await;
    let engine = Arc::new(FakeEngine::default());
    let data_dir = std::env::temp_dir().join(format!("032player-prev-{}", std::process::id()));
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let config = CoreConfig {
        data_dir,
        secrets: Arc::new(MemorySecrets::default()),
        settings: Settings::default(),
    };
    let core = client_core::start(engine, config, move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    let events = Events(rx);

    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    events.wait("login", |e| matches!(e, Event::LoggedIn { .. }));
    events.wait("tracks", |e| matches!(e, Event::TracksAppended { .. }));

    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    events.wait(
        "lyrics",
        |e| matches!(e, Event::Lyrics { lines, synced: true, .. } if lines.len() == 2),
    );
    core.engine_event(EngineEvent::FileLoaded { path: None });
    core.engine_event(EngineEvent::TimePos(12.0));
    events.wait("second line", |e| *e == Event::LyricsActive(Some(1)));

    // Clicking Previous past 10s seeks to 0 and resets lyrics scroll/follow.
    core.send(Command::Previous);
    events.wait("first line", |e| *e == Event::LyricsActive(Some(0)));
    events.wait("lyrics reset", |e| *e == Event::LyricsReset);

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn media_cover_is_cached_locally_without_media_token() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "apiRevision": 3,
            "features": { "browse": true },
            "playback": { "clientKinds": ["web", "desktop"] },
        })))
        .mount(&server)
        .await;
    let track_with_art = json!({
        "id": 1, "title": "曲1", "artist": "歌手", "album": "专辑", "year": 2024,
        "durationMillis": 240000, "streamUrl": "/api/v1/tracks/1/stream",
        "artworkUrl": "/api/v1/artwork/9",
        "codec": "flac", "bitDepth": 16, "sampleRate": 44100,
    });
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track_with_art], "limit": 1, "offset": 0, "total": 1
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks/1/stream"))
        .respond_with(ResponseTemplate::new(206))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "limit": 500, "offset": 0, "total": 0
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track_with_art], "limit": 500, "offset": 0, "total": 1
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/playlists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "limit": 500, "offset": 0, "total": 0
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks/1/lyrics"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playback/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "applied": true, "sessionId": "s1", "state": "active"
        })))
        .mount(&server)
        .await;
    // Minimal PNG header + payload.
    let fake_png = b"\x89PNG\r\n\x1a\nfake-png-bytes".to_vec();
    Mock::given(path("/api/v1/artwork/9"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(fake_png.clone()))
        .mount(&server)
        .await;

    let engine = Arc::new(FakeEngine::default());
    let data_dir = std::env::temp_dir().join(format!(
        "032player-media-cover-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_dir_all(&data_dir);
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let config = CoreConfig {
        data_dir: data_dir.clone(),
        secrets: Arc::new(MemorySecrets::default()),
        settings: Settings::default(),
    };
    let core = client_core::start(engine, config, move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    let events = Events(rx);

    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "secret-media-tok".into(),
    });
    events.wait("login", |e| matches!(e, Event::LoggedIn { .. }));
    events.wait("tracks", |e| matches!(e, Event::TracksAppended { .. }));

    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    let cover_ev = events.wait("media cover", |e| matches!(e, Event::MediaCover(Some(_))));
    let Event::MediaCover(Some(cover_path)) = cover_ev else {
        unreachable!()
    };
    assert!(cover_path.exists());
    assert_eq!(cover_path.extension().and_then(|s| s.to_str()), Some("png"));
    assert!(!cover_path.to_string_lossy().contains("secret-media-tok"));
    assert_eq!(std::fs::read(&cover_path).unwrap(), fake_png);

    // Shutdown cleans up temporary cover files.
    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    assert!(!cover_path.exists());
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn fade_disable_mid_fade_out_pauses_loading_seek_waits_and_clear_queue_resets_lyrics() {
    let server = server().await;
    let engine = Arc::new(FakeEngine {
        fade: true,
        ..FakeEngine::default()
    });
    let data_dir = std::env::temp_dir().join(format!(
        "032player-p2-11-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: data_dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings: Settings::default(),
        },
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        },
    );
    let events = Events(rx);
    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api".into(),
        media_token: "media".into(),
    });
    events.wait("tracks", |e| matches!(e, Event::TracksAppended { .. }));

    // 1. Seek while loading is deferred until FileLoaded.
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    events.wait("now playing", |e| matches!(e, Event::NowPlaying(Some(_))));
    engine.calls.lock().unwrap().clear();
    core.send(Command::Seek {
        position_ms: 45_000,
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        !engine.calls().iter().any(|c| c.starts_with("seek ")),
        "must not seek engine before FileLoaded: {:?}",
        engine.calls()
    );
    core.engine_event(EngineEvent::FileLoaded { path: None });
    wait_call(&engine, "seek 45").await;
    wait_call(&engine, "gain 1.000").await;

    // 2. Buffering clears on Stop.
    core.engine_event(EngineEvent::Buffering(true));
    events.wait("buffering on", |e| *e == Event::Buffering(true));
    core.send(Command::Stop);
    events.wait("buffering off", |e| *e == Event::Buffering(false));

    // 3. Disabling fade_on_pause mid fade-out still pauses engine immediately.
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    events.wait("now playing 2", |e| matches!(e, Event::NowPlaying(Some(_))));
    core.engine_event(EngineEvent::FileLoaded { path: None });
    wait_call(&engine, "gain 1.000").await;
    engine.calls.lock().unwrap().clear();
    core.send(Command::TogglePause);
    tokio::time::sleep(Duration::from_millis(30)).await;
    core.send(Command::SetFadeOnPause(false));
    wait_call(&engine, "pause true").await;
    wait_call(&engine, "gain 1.000").await;

    // 4. ClearQueue clears lyrics.
    core.send(Command::ClearQueue);
    events.wait(
        "lyrics cleared",
        |e| matches!(e, Event::Lyrics { lines, .. } if lines.is_empty()),
    );

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}
