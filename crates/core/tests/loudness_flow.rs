//! Controller-level loudness normalization flow: live setting toggles, Smart
//! Album/Track selection, gapless preload updates, restored-queue backfill,
//! TrackInfo diagnostics, and legacy server fallback.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, Command, CoreConfig, Engine, EngineEnd, EngineEvent, EngineLoudness, Event,
    LoudnessMode, LoudnessSettings, LoudnessTarget, MemorySecrets, PeakProtection, Settings,
    TrackList, TrackSource,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct LoudnessFakeEngine {
    loaded: Mutex<Vec<(String, EngineLoudness)>>,
    appended: Mutex<Vec<(String, EngineLoudness)>>,
    live_updates: Mutex<Vec<EngineLoudness>>,
}

impl Engine for LoudnessFakeEngine {
    fn load(&self, _: &str) -> Result<(), String> {
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
    fn set_loudness(&self, loudness: EngineLoudness) -> Result<(), String> {
        self.live_updates.lock().unwrap().push(loudness);
        Ok(())
    }
    fn load_with_loudness(
        &self,
        url: &str,
        _: bool,
        loudness: EngineLoudness,
    ) -> Result<(), String> {
        self.loaded.lock().unwrap().push((url.to_owned(), loudness));
        Ok(())
    }
    fn append_with_loudness(
        &self,
        url: &str,
        _: bool,
        loudness: EngineLoudness,
    ) -> Result<(), String> {
        self.appended
            .lock()
            .unwrap()
            .push((url.to_owned(), loudness));
        Ok(())
    }
}

fn wait_for<T>(rx: &mpsc::Receiver<Event>, mut pred: impl FnMut(&Event) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = rx.recv_timeout(left).expect("timed out waiting for event");
        if let Some(found) = pred(&event) {
            return found;
        }
    }
}

fn make_track_json(
    id: i64,
    album_id: i64,
    title: &str,
    lufs: Option<f64>,
    peak: Option<f64>,
    album_lufs: Option<f64>,
    album_peak: Option<f64>,
) -> Value {
    let mut obj = json!({
        "id": id,
        "albumId": album_id,
        "title": title,
        "album": format!("Album {album_id}"),
        "artist": "Artist",
        "durationMillis": 180_000,
        "streamUrl": format!("/api/v1/tracks/{id}/stream"),
        "artworkUrl": "/api/v1/artwork/1",
    });
    let map = obj.as_object_mut().unwrap();
    if let Some(v) = lufs {
        map.insert("loudnessLufs".into(), json!(v));
    }
    if let Some(v) = peak {
        map.insert("truePeakDb".into(), json!(v));
    }
    if let Some(v) = album_lufs {
        map.insert("albumLoudnessLufs".into(), json!(v));
    }
    if let Some(v) = album_peak {
        map.insert("albumTruePeakDb".into(), json!(v));
    }
    obj
}

#[tokio::test(flavor = "multi_thread")]
async fn live_loudness_smart_mode_preload_and_track_info() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1",
            "apiRevision": 3,
            "features": { "browse": true, "loudness": true },
            "loudness": {
                "available": true,
                "analyzedTracks": 2,
                "totalTracks": 2,
                "libraryMedianLufs": -10.0
            },
            "playback": { "clientKinds": ["desktop"] },
            "limits": { "maxPageSize": 500 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/1/stream"))
        .respond_with(ResponseTemplate::new(206))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/browse/albums"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [], "limit": 500, "offset": 0, "total": 0 })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/playlists"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [], "limit": 500, "offset": 0, "total": 0 })),
        )
        .mount(&server)
        .await;
    // Two tracks from the same album (album 10) with distinct track loudness (-8 vs -14)
    // and shared album loudness (-9.5 / -0.5).
    let tracks = vec![
        make_track_json(
            1,
            10,
            "Loud One",
            Some(-8.0),
            Some(0.2),
            Some(-9.5),
            Some(0.2),
        ),
        make_track_json(
            2,
            10,
            "Quiet Two",
            Some(-14.0),
            Some(-1.0),
            Some(-9.5),
            Some(0.2),
        ),
    ];
    Mock::given(method("GET"))
        .and(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": tracks,
            "limit": 500,
            "offset": 0,
            "total": 2
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [make_track_json(1, 10, "Loud One", Some(-8.0), Some(0.2), Some(-9.5), Some(0.2))],
            "limit": 1,
            "offset": 0,
            "total": 1
        })))
        .mount(&server)
        .await;

    let dir = std::env::temp_dir().join(format!(
        "032player-loudness-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let engine = Arc::new(LoudnessFakeEngine::default());
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let settings = Settings {
        volume: 100.0,
        loudness: LoudnessSettings {
            enabled: true,
            target: LoudnessTarget::Loud,
            mode: LoudnessMode::Smart,
            peak_mode: PeakProtection::SoftLimiter,
        },
        ..Settings::default()
    };
    let core = client_core::start(
        engine.clone(),
        CoreConfig {
            data_dir: dir.clone(),
            settings,
            secrets: Arc::new(MemorySecrets::default()),
        },
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        },
    );

    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "api-tok".into(),
        media_token: "media-tok".into(),
    });
    let features = wait_for(&rx, |e| match e {
        Event::LoggedIn { features, .. } => Some(features.clone()),
        _ => None,
    });
    assert!(features.loudness);
    assert_eq!(features.loudness_median_lufs, Some(-10.0));

    wait_for(&rx, |e| match e {
        Event::TracksAppended { items, .. } if items.len() == 2 => Some(()),
        _ => None,
    });

    // Play track 0 of the 2-track album sequentially -> Smart mode picks Album loudness (-9.5 -> -1.5 dB).
    core.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    wait_for(&rx, |e| match e {
        Event::NowPlaying(Some(np)) if np.track_id == 1 => Some(()),
        _ => None,
    });

    let loaded = engine.loaded.lock().unwrap().clone();
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].1.enabled);
    assert!((loaded[0].1.gain_db - (-1.5)).abs() < 1e-6);
    assert!(!loaded[0].1.use_limiter);

    // Finish loading so track 2 is preloaded with the same Album loudness (-1.5 dB).
    let expected_url = loaded[0].0.clone();
    core.engine_event(EngineEvent::FileLoaded {
        path: Some(expected_url),
    });
    // Wait briefly for preload append to settle.
    let deadline = Instant::now() + Duration::from_secs(2);
    while engine.appended.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let appended = engine.appended.lock().unwrap().clone();
    assert_eq!(appended.len(), 1);
    assert!((appended[0].1.gain_db - (-1.5)).abs() < 1e-6);

    // Toggle to Track mode live -> track 1 switches immediately to -8.0 -> -3.0 dB,
    // and preloaded track 2 refreshes to -14.0 -> +3.0 dB with soft limiter!
    core.send(Command::SetLoudness(LoudnessSettings {
        enabled: true,
        target: LoudnessTarget::Loud,
        mode: LoudnessMode::Track,
        peak_mode: PeakProtection::SoftLimiter,
    }));
    let deadline = Instant::now() + Duration::from_secs(2);
    while engine.appended.lock().unwrap().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let live = engine.live_updates.lock().unwrap().clone();
    assert!(
        live.iter()
            .any(|l| l.enabled && (l.gain_db - (-3.0)).abs() < 1e-6 && !l.use_limiter),
        "expected live update to track gain -3.0 dB, got {live:?}"
    );
    let appended = engine.appended.lock().unwrap().clone();
    let last_append = appended.last().unwrap().1;
    assert!(last_append.enabled);
    assert!((last_append.gain_db - 3.0).abs() < 1e-6);
    assert!(last_append.use_limiter);

    // Request NowTrackInfo -> includes loudness diagnostics rows.
    core.send(Command::NowTrackInfo);
    let facts = wait_for(&rx, |e| match e {
        Event::TrackInfo { facts, .. } => Some(facts.clone()),
        _ => None,
    });
    assert!(
        facts
            .iter()
            .any(|(k, v)| k == "集成响度" && v.contains("-8.0 LUFS"))
    );
    assert!(
        facts
            .iter()
            .any(|(k, v)| k == "响度补偿" && v.contains("-3.0 dB"))
    );

    // Gapless EOF to track 2 keeps active loudness updated without reloading.
    core.engine_event(EngineEvent::EndFile {
        reason: EngineEnd::Eof,
        error: None,
    });
    wait_for(&rx, |e| match e {
        Event::NowPlaying(Some(np)) if np.track_id == 2 => Some(()),
        _ => None,
    });

    // Turning loudness off sends bypass immediately.
    core.send(Command::SetLoudness(LoudnessSettings {
        enabled: false,
        ..LoudnessSettings::default()
    }));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !engine
        .live_updates
        .lock()
        .unwrap()
        .last()
        .is_some_and(|l| !l.enabled && l.gain_db == 0.0 && !l.use_limiter)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        engine
            .live_updates
            .lock()
            .unwrap()
            .last()
            .is_some_and(|l| !l.enabled && !l.use_limiter)
    );

    core.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
