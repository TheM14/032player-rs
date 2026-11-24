//! Integration tests for Similar Track Radio and Queue Auto-Continue.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, Command, CoreConfig, CoreHandle, Engine, EngineEnd, EngineEvent, Event,
    MemorySecrets, QueueEntry, QueuePosition, RadioPauseReason, RadioStatus, Repeat, Settings,
    SimilarityMethod, SleepTimer, TrackList, TrackSource,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct FakeEngine {
    loads: Mutex<Vec<String>>,
}

impl FakeEngine {
    fn loads(&self) -> Vec<String> {
        self.loads.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.loads.lock().unwrap().clear();
    }
}

impl Engine for FakeEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.loads.lock().unwrap().push(url.to_owned());
        Ok(())
    }
    fn append(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn clear_upcoming(&self) -> Result<(), String> {
        Ok(())
    }
    fn stop(&self) -> Result<(), String> {
        Ok(())
    }
    fn set_paused(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn seek(&self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn set_volume(&self, _: f64) -> Result<(), String> {
        Ok(())
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

fn track(id: i64, album_id: i64, title: &str, artist: &str, artist_id: i64) -> Value {
    json!({
        "id": id,
        "albumId": album_id,
        "title": title,
        "artist": artist,
        "artists": [{ "id": artist_id, "name": artist }],
        "album": format!("Album {album_id}"),
        "durationMillis": 200_000,
        "streamUrl": format!("/api/v1/tracks/{id}/stream"),
        "trackNumber": id,
    })
}

fn page(items: Vec<Value>) -> Value {
    let total = items.len();
    json!({ "items": items, "limit": 500, "offset": 0, "total": total })
}

struct Harness {
    core: CoreHandle,
    engine: Arc<FakeEngine>,
    rx: mpsc::Receiver<Event>,
    last_queue: Mutex<Vec<QueueEntry>>,
    last_radio: Mutex<RadioStatus>,
}

impl Harness {
    async fn start(server: &MockServer, settings: Settings, dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let engine = Arc::new(FakeEngine::default());
        let (tx, rx) = mpsc::channel();
        let config = CoreConfig {
            data_dir: dir,
            secrets: Arc::new(MemorySecrets::default()),
            settings,
        };
        let core = client_core::start(engine.clone(), config, move |e| {
            let _ = tx.send(e);
        });
        core.send(Command::Login {
            server_url: server.uri(),
            api_token: "api".into(),
            media_token: "media".into(),
        });
        let h = Self {
            core,
            engine,
            rx,
            last_queue: Mutex::new(Vec::new()),
            last_radio: Mutex::new(RadioStatus::default()),
        };
        h.wait("logged in", |e| matches!(e, Event::LoggedIn { .. }));
        h.wait("tracks loaded", |e| {
            matches!(e, Event::TracksAppended { .. })
        });
        h
    }

    fn send(&self, cmd: Command) {
        self.core.send(cmd);
    }

    fn wait<F>(&self, label: &str, mut pred: F) -> Event
    where
        F: FnMut(&Event) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(rem) = deadline.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(rem) {
                Ok(event) => {
                    if let Event::Queue { items, .. } = &event {
                        *self.last_queue.lock().unwrap() = items.clone();
                    }
                    if let Event::RadioState(ref status) = event {
                        *self.last_radio.lock().unwrap() = status.clone();
                    }
                    if pred(&event) {
                        return event;
                    }
                }
                Err(_) => break,
            }
        }
        panic!("timed out waiting for {label}");
    }

    fn queue(&self) -> Vec<QueueEntry> {
        self.last_queue.lock().unwrap().clone()
    }

    fn radio(&self) -> RadioStatus {
        self.last_radio.lock().unwrap().clone()
    }

    fn shutdown(self) {
        self.core.shutdown();
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "032player-radio-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn radio_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1",
            "apiRevision": 3,
            "features": { "browse": true, "similarTracks": true },
            "similarity": { "method": "hybrid", "distanceRange": [0, 1] },
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/playlists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            track(1, 10, "群青", "YOASOBI", 100),
            track(2, 10, "優しい彗星", "YOASOBI", 100),
            track(3, 10, "群青 (Off Vocal)", "YOASOBI", 100),
            track(4, 20, "Ghost Pop", "ZUTOMAYO", 200),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/1/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "track": track(101, 30, "星座になれたら", "結束バンド", 300), "score": 0.92, "distance": 0.08, "reasons": ["soundsSimilar"] },
                { "track": track(102, 31, "藍二乗", "ヨルシカ", 301), "score": 0.88, "distance": 0.12, "reasons": ["sameWork"] },
                { "track": track(103, 32, "秒針を噛む", "ZUTOMAYO", 200), "score": 0.84, "distance": 0.16, "reasons": ["coPlaylist"] },
                { "track": track(104, 33, "光るなら", "Goose house", 302), "score": 0.80, "distance": 0.20, "reasons": ["sharedCredit"] },
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/2/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "track": track(201, 40, "花に亡霊", "ヨルシカ", 301), "score": 0.90, "distance": 0.10, "reasons": ["soundsSimilar"] },
                { "track": track(202, 41, "廻廻奇譚", "Eve", 303), "score": 0.85, "distance": 0.15, "reasons": ["genre"] },
            ]
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn radio_from_track_dual_zone_edits_refresh_and_persist() {
    let server = radio_server().await;
    let dir = temp_dir("flow");
    let settings = Settings {
        repeat: Repeat::All,
        shuffle: true,
        auto_play_similar: true,
        ..Settings::default()
    };
    let h = Harness::start(&server, settings, dir.clone()).await;

    // Starting radio from track 1 plays immediately, clears repeat & shuffle,
    // and appends similar tracks in the auto zone with reasons.
    h.send(Command::StartSimilarRadioTrack(TrackList::Library, 0));
    h.wait(
        "radio appended",
        |e| matches!(e, Event::Queue { items, .. } if items.len() == 5),
    );
    let q = h.queue();
    assert_eq!(q[0].track_id, 1);
    assert!(!q[0].is_auto);
    assert!(q[1..].iter().all(|i| i.is_auto));
    let auto_reasons: std::collections::HashMap<i64, Option<&str>> = q[1..]
        .iter()
        .map(|i| (i.track_id, i.reason.as_deref()))
        .collect();
    assert_eq!(auto_reasons.get(&101), Some(&Some("听感相似")));
    assert_eq!(auto_reasons.get(&102), Some(&Some("同作品")));
    assert_eq!(auto_reasons.get(&103), Some(&Some("共现歌单")));
    assert_eq!(auto_reasons.get(&104), Some(&Some("同作者")));
    let r = h.radio();
    assert!(r.supported);
    assert_eq!(r.similarity_method, SimilarityMethod::Hybrid);
    assert_eq!(r.manual_queue_count, 1);
    assert_eq!(r.radio_seed.as_ref().map(|s| s.track_id), Some(1));

    // Manual "add to end" inserts at end of manual zone (index 1), BEFORE auto zone.
    h.send(Command::Enqueue {
        source: TrackSource::Track(TrackList::Library, 3),
        at: QueuePosition::Last,
    });
    h.wait(
        "manual enqueued",
        |e| matches!(e, Event::Queue { items, .. } if items.len() == 6),
    );
    let q = h.queue();
    assert_eq!(q[1].track_id, 4);
    assert!(!q[1].is_auto);
    assert!(q[2].is_auto);
    assert_eq!(h.radio().manual_queue_count, 2);

    // Dragging an auto track (index 2) into manual zone (index 1) promotes it.
    let auto_uid = q[2].uid;
    h.send(Command::MoveQueueItem {
        uid: auto_uid,
        to: 1,
    });
    h.wait("promoted", |e| {
        matches!(e, Event::Queue { items, .. } if items.get(1).is_some_and(|x| x.uid == auto_uid && !x.is_auto))
    });
    assert_eq!(h.radio().manual_queue_count, 3);

    // Shut down and restart to verify queue.json restores radio boundary & seed.
    h.shutdown();
    let h2 = Harness::start(&server, Settings::default(), dir.clone()).await;
    let restored_q = h2.queue();
    assert_eq!(restored_q.len(), 6);
    assert!(!restored_q[2].is_auto);
    assert!(restored_q[3].is_auto);
    assert_eq!(h2.radio().manual_queue_count, 3);
    assert_eq!(h2.radio().radio_seed.as_ref().map(|s| s.track_id), Some(1));

    // Turning off auto-play trims unplayed auto-similar tracks and exits radio mode.
    h2.send(Command::SetAutoPlaySimilar(false));
    h2.wait(
        "auto-play disabled",
        |e| matches!(e, Event::RadioState(r) if !r.auto_play_enabled),
    );
    assert_eq!(h2.queue().len(), 3);
    assert_eq!(h2.radio().manual_queue_count, 3);
    assert!(h2.radio().radio_seed.is_none());
    assert!(!h2.radio().auto_play_enabled);

    h2.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn current_radio_does_not_interrupt_and_rolling_seed_skips_accompaniment() {
    let server = radio_server().await;
    let dir = temp_dir("rolling");
    // Start with auto-play off so initial manual play does not prefetch immediately.
    let settings = Settings {
        auto_play_similar: false,
        ..Settings::default()
    };
    let h = Harness::start(&server, settings, dir.clone()).await;

    // Play tracks [1, 2, 3 ("群青 (Off Vocal)"), 4] from index 1 ("優しい彗星").
    h.send(Command::Play(TrackSource::Track(TrackList::Library, 1)));
    h.wait(
        "playing track 2",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 2),
    );
    h.engine.clear();

    // Start radio from current track (2): must NOT reload engine, keeps [1, 2], replaces [3, 4] with similar(2).
    h.send(Command::StartSimilarRadioCurrent);
    h.wait("replaced after current", |e| {
        matches!(e, Event::Queue { items, .. } if items.len() == 4 && items[2].is_auto && items[3].is_auto)
    });
    assert!(
        h.engine.loads().is_empty(),
        "current track radio must not interrupt active playback"
    );
    let q_ids: Vec<i64> = h.queue().iter().map(|i| i.track_id).collect();
    assert_eq!(&q_ids[..2], &[1, 2]);
    let mut auto_ids = q_ids[2..].to_vec();
    auto_ids.sort_unstable();
    assert_eq!(auto_ids, [201, 202]);

    // Turn off auto-play, queue [track 2, track 3 "(Off Vocal)"] and enable auto-continue:
    // rolling seed must skip track 3 "(Off Vocal)" and query /tracks/2/similar!
    h.send(Command::SetAutoPlaySimilar(false));
    h.wait(
        "off",
        |e| matches!(e, Event::RadioState(r) if !r.auto_play_enabled),
    );
    h.send(Command::Play(TrackSource::Track(TrackList::Library, 2))); // starts on track 3 (Off Vocal)
    h.wait(
        "library queue loaded",
        |e| matches!(e, Event::Queue { items, current: Some(2) } if items.len() == 4),
    );
    // Remove track 4 so queue ends on [1, 2, 3 (Off Vocal)].
    let uid4 = h.queue()[3].uid;
    h.send(Command::RemoveFromQueue(uid4));
    h.wait(
        "removed 4",
        |e| matches!(e, Event::Queue { items, .. } if items.len() == 3),
    );

    // Enable auto-play: rolling seed skips track 3 (Off Vocal) and picks track 2!
    h.send(Command::SetAutoPlaySimilar(true));
    h.wait("rolling appended from track 2", |e| {
        matches!(e, Event::RadioState(r) if r.last_auto_seed_title.as_deref() == Some("優しい彗星"))
    });
    assert_eq!(h.queue().len(), 5);
    let mut rolled_auto: Vec<i64> = h.queue()[3..].iter().map(|i| i.track_id).collect();
    rolled_auto.sort_unstable();
    assert_eq!(rolled_auto, [201, 202]);
    assert_eq!(
        h.radio().last_auto_seed_title.as_deref(),
        Some("優しい彗星")
    );

    // SleepTimer::EndOfAlbum blocks auto-continue at last track of album 10.
    h.send(Command::SetSleepTimer(Some(SleepTimer::EndOfAlbum)));
    h.wait(
        "sleep album pause reason",
        |e| matches!(e, Event::RadioState(r) if r.pause_reason == RadioPauseReason::SleepAlbum),
    );

    // RepeatOne overrides with RepeatOne pause reason.
    h.send(Command::SetRepeat(Repeat::One));
    h.wait(
        "repeat pause reason",
        |e| matches!(e, Event::RadioState(r) if r.pause_reason == RadioPauseReason::RepeatOne),
    );

    h.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn eof_at_last_track_waits_for_auto_continue_and_plays_appended() {
    let server = radio_server().await;
    let dir = temp_dir("eof");
    // Slow down /tracks/1/similar slightly so EOF arrives while fetch is in flight.
    let slow_server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1",
            "apiRevision": 3,
            "features": { "browse": true, "similarTracks": true },
        })))
        .mount(&slow_server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&slow_server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&slow_server)
        .await;
    Mock::given(path("/api/v1/playlists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&slow_server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![track(1, 10, "群青", "YOASOBI", 100)])),
        )
        .mount(&slow_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/1/similar"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_json(json!({
                    "items": [
                        { "track": track(101, 30, "星座になれたら", "結束バンド", 300), "score": 0.92, "distance": 0.08, "reasons": ["soundsSimilar"] },
                    ]
                })),
        )
        .mount(&slow_server)
        .await;

    let h = Harness::start(&slow_server, Settings::default(), dir.clone()).await;
    h.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    h.wait(
        "playing track 1",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 1),
    );
    let loaded_url = h.engine.loads().last().cloned().unwrap();
    h.core.engine_event(EngineEvent::FileLoaded {
        path: Some(loaded_url),
    });
    // Trigger EOF while /tracks/1/similar is still delayed in flight.
    h.core.engine_event(EngineEvent::EndFile {
        reason: EngineEnd::Eof,
        error: None,
    });
    h.wait(
        "auto-continued into 101 after EOF",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 101),
    );

    h.shutdown();
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_load_more_and_queue_item_radio() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1",
            "apiRevision": 3,
            "features": { "browse": true, "similarTracks": true },
            "similarity": { "method": "metadata" },
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/playlists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![
            track(1, 10, "群青", "YOASOBI", 100),
            track(2, 10, "優しい彗星", "YOASOBI", 100),
        ])))
        .mount(&server)
        .await;
    // First call returns [101, 102]; after refresh (101/102 dismissed) returns [103, 104].
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/1/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "track": track(101, 30, "A", "ArtistA", 301), "score": 0.9, "distance": 0.1, "reasons": ["sameArtist"] },
                { "track": track(102, 31, "B", "ArtistB", 302), "score": 0.85, "distance": 0.15, "reasons": ["genre"] },
            ]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/1/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "track": track(101, 30, "A", "ArtistA", 301), "score": 0.9, "distance": 0.1, "reasons": ["sameArtist"] },
                { "track": track(103, 32, "C", "ArtistC", 303), "score": 0.82, "distance": 0.18, "reasons": ["era"] },
                { "track": track(104, 33, "D", "ArtistD", 304), "score": 0.80, "distance": 0.20, "reasons": ["coPlaylist"] },
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/104/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "track": track(105, 34, "E", "ArtistE", 305), "score": 0.88, "distance": 0.12, "reasons": ["sameWork"] },
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/103/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "track": track(201, 40, "F", "ArtistF", 306), "score": 0.91, "distance": 0.09, "reasons": ["soundsSimilar"] },
            ]
        })))
        .mount(&server)
        .await;

    let dir = temp_dir("refresh");
    let h = Harness::start(&server, Settings::default(), dir.clone()).await;

    // Start radio from track 1 -> [1, 101, 102]
    h.send(Command::StartSimilarRadioTrack(TrackList::Library, 0));
    h.wait(
        "initial radio",
        |e| matches!(e, Event::Queue { items, .. } if items.len() == 3),
    );
    assert_eq!(h.radio().similarity_method, SimilarityMethod::Metadata);

    // Refresh ("换一批"): 101 & 102 dismissed (so 101 is filtered out client-side), replaced by {103, 104}.
    h.send(Command::RefreshSimilarRadio);
    h.wait("refreshed batch", |e| {
        matches!(e, Event::Queue { items, .. } if items.len() == 3 && items[1].track_id != 101 && items[2].track_id != 101)
    });
    let mut refreshed_auto: Vec<i64> = h.queue()[1..].iter().map(|i| i.track_id).collect();
    refreshed_auto.sort_unstable();
    assert_eq!(refreshed_auto, [103, 104]);

    // Load more ("再加一批"): uses rolling tail seed (104 -> 105, or 103 -> 201).
    let expected_more = if h.queue()[2].track_id == 104 {
        105
    } else {
        201
    };
    h.send(Command::LoadMoreSimilarRadio);
    h.wait(
        "loaded more",
        |e| matches!(e, Event::Queue { items, .. } if items.len() == 4 && items[3].track_id == expected_more),
    );

    // StartSimilarRadioQueueItem on non-current queue item 103 plays 103 and seeds radio -> [103, 201].
    let uid103 = h
        .queue()
        .iter()
        .find(|i| i.track_id == 103)
        .expect("track 103 in queue")
        .uid;
    h.send(Command::StartSimilarRadioQueueItem(uid103));
    h.wait("queue item radio", |e| {
        matches!(e, Event::Queue { items, .. } if items.len() == 2 && items[0].track_id == 103 && items[1].track_id == 201)
    });
    assert_eq!(h.radio().radio_seed.as_ref().map(|s| s.track_id), Some(103));

    h.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
