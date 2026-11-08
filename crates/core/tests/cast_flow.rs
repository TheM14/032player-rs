//! Sonos output end to end: a wiremock 032 server and a simulated speaker.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, CastSettings, Command, CoreConfig, Engine, Event, MemorySecrets, QueuePosition,
    Settings, TrackList, TrackSource,
};
use serde_json::{Value, json};
use sonos::fake::{FakeSpeaker, UUID};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct FakeEngine {
    calls: Mutex<Vec<String>>,
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
        Ok(())
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
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        Ok(Vec::new())
    }
    fn set_audio_device(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn set_exclusive(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
}

fn track(id: i64, codec: &str) -> Value {
    json!({
        "id": id, "title": format!("曲{id}"), "artist": "歌手", "album": "专辑",
        "durationMillis": 240000, "streamUrl": format!("/api/v1/tracks/{id}/stream"),
        "codec": codec, "sampleRate": 48000, "artworkUrl": "/api/v1/artwork/1",
    })
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "apiRevision": 3,
            "features": { "browse": true },
            "playback": { "clientKinds": ["web", "desktop"] },
            "transcode": { "available": true, "formats": { "mp3": { "available": true } } },
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track(1, "flac")], "limit": 1, "offset": 0, "total": 3
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
            "items": [track(1, "flac"), track(2, "mp3"), track(3, "opus")],
            "limit": 500, "offset": 0, "total": 3
        })))
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
    fn wait(&self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(10);
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

async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn playback_events(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/playback/events")
        .map(|r| {
            let e: Value = serde_json::from_slice(&r.body).unwrap();
            let mut s = format!("{}:{}", e["type"].as_str().unwrap(), e["trackId"]);
            if let Some(reason) = e["endReason"].as_str() {
                s.push_str(&format!(":{reason}"));
            }
            s
        })
        .collect()
}

async fn wait_reported(server: &MockServer, what: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let events = playback_events(server).await;
        if events.iter().any(|e| e == what) {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "not reported: {what} in {events:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plays_through_a_sonos_room() {
    let server = server().await;
    let fake = FakeSpeaker::start().await;
    let engine = Arc::new(FakeEngine::default());
    let data_dir = std::env::temp_dir().join(format!("032player-cast-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let settings = Settings {
        cast: CastSettings {
            hosts: fake.host(),
            ..CastSettings::default()
        },
        ..Settings::default()
    };
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: data_dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings,
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
    events.wait("login", |e| matches!(e, Event::LoggedIn { .. }));
    events.wait("tracks", |e| matches!(e, Event::TracksAppended { .. }));

    // Discovery through the configured address, then the room.
    core.send(Command::DiscoverCast);
    let rooms = events.wait("rooms", |e| {
        matches!(
            e,
            Event::CastRooms {
                searching: false,
                ..
            }
        )
    });
    let Event::CastRooms { rooms, .. } = rooms else {
        unreachable!()
    };
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].name, "测试房间");
    core.send(Command::SetCastOutput(Some(UUID.into())));
    events.wait(
        "output",
        |e| matches!(e, Event::CastOutput(Some(r)) if r.id == UUID),
    );

    // Play the list: the whole queue goes to the speaker, nothing to mpv.
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    until("speaker playing", || {
        let s = fake.state();
        s.transport == "PLAYING" && s.queue.len() == 3 && s.track == 1
    })
    .await;
    let state = fake.state();
    assert!(
        state.queue[0]
            .0
            .ends_with("/api/v1/tracks/1/stream?mediaToken=media")
    );
    assert!(
        state.queue[1]
            .0
            .ends_with("/api/v1/tracks/2/stream?mediaToken=media")
    );
    // Opus: Sonos cannot decode it, the server transcodes live.
    assert!(
        state.queue[2]
            .0
            .contains("/api/v1/tracks/3/transcode.mp3?mediaToken=media&bitrate=320"),
        "{:?}",
        state.queue[2]
    );
    assert_eq!(state.mode, "NORMAL");
    assert!(!engine.calls().iter().any(|c| c.starts_with("load")));
    events.wait("playing", |e| *e == Event::Paused(false));
    wait_reported(&server, "start:1").await;

    // The speaker advances by itself near the end: the core follows.
    fake.update(|s| s.position_ms = 236_000);
    events.wait(
        "near the end",
        |e| matches!(e, Event::Position { position_ms, .. } if *position_ms >= 236_000),
    );
    fake.finish_track();
    events.wait(
        "next track",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 2),
    );
    let reported = wait_reported(&server, "start:2").await;
    assert!(
        reported.contains(&"end:1:completed".to_owned()),
        "{reported:?}"
    );

    // Pause and seek go to the speaker.
    fake.clear_log();
    core.send(Command::TogglePause);
    events.wait("paused", |e| *e == Event::Paused(true));
    until("pause sent", || fake.state().transport == "PAUSED_PLAYBACK").await;
    core.send(Command::Seek {
        position_ms: 30_000,
    });
    until("seek sent", || {
        fake.log().contains(&"Seek REL_TIME 0:00:30".to_owned())
    })
    .await;
    core.send(Command::TogglePause);
    until("resumed", || fake.state().transport == "PLAYING").await;

    // Next: a jump inside the speaker queue (no new queue).
    fake.clear_log();
    core.send(Command::Next);
    events.wait(
        "track 3",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 3),
    );
    until("jumped", || fake.state().track == 3).await;
    assert!(!fake.log().contains(&"RemoveAllTracksFromQueue".to_owned()));
    wait_reported(&server, "end:2:skipped").await;

    // A live MP3 seeks by re-opening the entry at the offset.
    fake.clear_log();
    core.send(Command::Seek {
        position_ms: 60_000,
    });
    until("live seek", || {
        fake.state().queue[2].0.contains("offsetMs=60000") && fake.state().track == 3
    })
    .await;
    assert!(fake.log().contains(&"RemoveTrackFromQueue 3".to_owned()));
    // The position counts from the offset.
    fake.update(|s| s.position_ms = 5_000);
    events.wait("offset position", |e| {
        matches!(e, Event::Position { position_ms, .. } if (65_000..67_000).contains(position_ms))
    });

    // Queue edits are mirrored.
    core.send(Command::Enqueue {
        source: TrackSource::Track(TrackList::Library, 0),
        at: QueuePosition::Last,
    });
    until("appended", || fake.state().queue.len() == 4).await;

    // Volume goes to the room.
    core.send(Command::SetVolume(42.0));
    until("volume", || fake.state().volume == 42).await;
    assert!(!engine.calls().iter().any(|c| c.starts_with("volume")));

    // Back to this computer: the speaker pauses, mpv continues the same listen.
    let before = playback_events(&server).await.len();
    core.send(Command::SetCastOutput(None));
    events.wait("local", |e| *e == Event::CastOutput(None));
    until("speaker paused", || {
        fake.state().transport == "PAUSED_PLAYBACK"
    })
    .await;
    until("mpv loads", || {
        engine
            .calls()
            .iter()
            .any(|c| c.starts_with("load ") && c.contains("/api/v1/tracks/3/stream"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = playback_events(&server).await;
    assert!(
        !after[before..].iter().any(|e| e.starts_with("start:")),
        "a new play was counted: {:?}",
        &after[before..]
    );

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let saved = Settings::load(&data_dir);
    assert_eq!(saved.cast.room, None);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn adopts_the_queue_a_speaker_plays() {
    let server = server().await;
    for id in [2, 3] {
        Mock::given(path(format!("/api/v1/tracks/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(track(id, if id == 3 { "opus" } else { "mp3" })),
            )
            .mount(&server)
            .await;
    }
    let fake = FakeSpeaker::start().await;
    // Pushed by the web player with a LAN address and this media token.
    fake.update(|s| {
        s.queue = vec![
            ("http://192.168.1.5:4533/api/v1/tracks/2/stream?mediaToken=media".into(), "曲2".into()),
            (
                "http://192.168.1.5:4533/api/v1/tracks/3/transcode.mp3?mediaToken=media&bitrate=320&offsetMs=20000"
                    .into(),
                "曲3".into(),
            ),
        ];
        s.queue_source = true;
        s.track = 2;
        s.transport = "PLAYING".into();
        s.position_ms = 10_000;
        s.mode = "REPEAT_ALL".into();
    });
    let engine = Arc::new(FakeEngine::default());
    let data_dir = std::env::temp_dir().join(format!("032player-adopt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let settings = Settings {
        cast: CastSettings {
            room: Some(client_core::CastRoomSetting {
                id: UUID.into(),
                name: "测试房间".into(),
                base_url: fake.server.uri(),
            }),
            hosts: fake.host(),
            ..CastSettings::default()
        },
        ..Settings::default()
    };
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: data_dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings,
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
    events.wait(
        "output",
        |e| matches!(e, Event::CastOutput(Some(r)) if r.id == UUID),
    );
    // The speaker's play mode comes along.
    events.wait("repeat", |e| *e == Event::Repeat(client_core::Repeat::All));
    let now = events.wait("adopted", |e| matches!(e, Event::NowPlaying(Some(_))));
    let Event::NowPlaying(Some(now)) = now else {
        unreachable!()
    };
    assert_eq!(now.track_id, 3);
    // The live entry's offset counts.
    events.wait(
        "position",
        |e| matches!(e, Event::Position { position_ms, .. } if *position_ms >= 30_000),
    );
    events.wait(
        "queue",
        |e| matches!(e, Event::Queue { items, current: Some(1) } if items.len() == 2),
    );
    wait_reported(&server, "start:3").await;
    assert!(
        !fake
            .log()
            .iter()
            .any(|l| l.starts_with("RemoveAllTracksFromQueue"))
    );
    assert!(!engine.calls().iter().any(|c| c.starts_with("load")));

    // Past 10 s "previous" restarts: the live entry re-opens at 0.
    core.send(Command::Previous);
    until("restart", || {
        let s = fake.state();
        s.track == 2 && !s.queue[1].0.contains("offsetMs")
    })
    .await;
    events.wait(
        "restarted",
        |e| matches!(e, Event::Position { position_ms, .. } if *position_ms < 5_000),
    );
    // Then a jump inside the adopted queue, which stays.
    core.send(Command::Previous);
    until("previous", || fake.state().track == 1).await;
    assert!(
        !fake
            .log()
            .iter()
            .any(|l| l.starts_with("RemoveAllTracksFromQueue"))
    );

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_at_the_end_of_the_queue() {
    let server = server().await;
    let fake = FakeSpeaker::start().await;
    let engine = Arc::new(FakeEngine::default());
    let data_dir = std::env::temp_dir().join(format!("032player-cast-end-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let settings = Settings {
        cast: CastSettings {
            hosts: fake.host(),
            ..CastSettings::default()
        },
        ..Settings::default()
    };
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: data_dir.clone(),
            secrets: Arc::new(MemorySecrets::default()),
            settings,
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
    core.send(Command::DiscoverCast);
    events.wait(
        "rooms",
        |e| matches!(e, Event::CastRooms { searching: false, rooms, .. } if rooms.len() == 1),
    );
    core.send(Command::SetCastOutput(Some(UUID.into())));
    // From the last row: the speaker starts at entry 3.
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 2)));
    until("speaker playing", || {
        let s = fake.state();
        s.transport == "PLAYING" && s.track == 3
    })
    .await;
    fake.update(|s| s.position_ms = 238_000);
    events.wait(
        "near the end",
        |e| matches!(e, Event::Position { position_ms, .. } if *position_ms >= 238_000),
    );
    // The speaker finishes the queue: stopped, back at entry 1.
    fake.finish_track();
    events.wait("stopped", |e| *e == Event::Paused(true));
    events.wait(
        "first entry",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 1),
    );
    let reported = wait_reported(&server, "end:3:completed").await;
    assert!(!reported.contains(&"start:1".to_owned()), "{reported:?}");

    // Play again: a jump, the queue is still on the speaker.
    fake.clear_log();
    core.send(Command::TogglePause);
    until("playing again", || {
        let s = fake.state();
        s.transport == "PLAYING" && s.track == 1
    })
    .await;
    assert!(
        !fake
            .log()
            .iter()
            .any(|l| l.starts_with("RemoveAllTracksFromQueue"))
    );
    wait_reported(&server, "start:1").await;

    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    // The room is remembered for the next start.
    assert_eq!(Settings::load(&data_dir).cast.room.unwrap().id, UUID);
    let _ = std::fs::remove_dir_all(&data_dir);
}
