//! M3 against a mock server: the 032 home, 接着听 resume positions, daily
//! mix playback contexts, playing badges, playback history and lyrics tools.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, Command, CoreConfig, CoreHandle, DetailView, Engine, EngineEvent, Event,
    LyricsSettings, MemorySecrets, PlayingSources, Settings, TrackList, TrackSource,
    TranslationMode, m032::SpotlightTab,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[derive(Default)]
struct FakeEngine {
    calls: Mutex<Vec<String>>,
}

impl FakeEngine {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
    fn push(&self, call: String) -> Result<(), String> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

fn short(url: &str) -> String {
    let rest = url.split("/api/v1").nth(1).unwrap_or(url);
    rest.replace("?mediaToken=media", "")
}

impl Engine for FakeEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.push(format!("load {}", short(url)))
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
    fn seek(&self, seconds: f64) -> Result<(), String> {
        self.push(format!("seek {seconds}"))
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

fn track(id: i64) -> Value {
    json!({
        "id": id, "title": format!("t{id}"), "artist": "歌手", "albumId": 10,
        "durationMillis": 240000, "streamUrl": format!("/api/v1/tracks/{id}/stream"),
    })
}

fn played(id: i64) -> Value {
    let mut t = track(id);
    t["lastPlayedAt"] = json!("2026-10-28T01:00:00.000Z");
    t
}

fn page(items: Vec<Value>, total: usize) -> Value {
    json!({ "items": items, "limit": 500, "offset": 0, "total": total })
}

fn record(id: i64, position: i64) -> Value {
    json!({ "trackId": id, "positionMillis": position, "durationMillis": 240000,
            "playCount": 1, "state": "interrupted", "lastPlayedAt": "2026-10-28T01:00:00.000Z",
            "track": played(id) })
}

async fn get(server: &MockServer, p: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn server(features: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "apiRevision": 3, "features": features,
            "playback": { "clientKinds": ["web", "desktop"], "contextTypes": ["dailyMix"] },
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .and(query_param("sort", "recentlyPlayed"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![played(1), track(2), played(3), played(4)], 4)),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    get(&server, "/api/v1/tracks", page(vec![], 0)).await;
    get(&server, "/api/v1/albums", page(vec![], 0)).await;
    get(&server, "/api/v1/playlists", page(vec![], 0)).await;
    get(
        &server,
        "/api/v1/home",
        json!({
            "empty": false,
            "spotlight": {
                "tabs": ["recent", "listening"], "defaultTab": "recent",
                "recent": { "focus": "recent", "album": { "id": 10, "title": "A", "trackCount": 9 },
                            "tracks": [track(1), track(2)], "previewDurationMillis": 480000,
                            "noteTime": "" },
                "listening": { "focus": "listening", "album": { "id": 11, "trackCount": 1 },
                               "tracks": [], "noteTime": "", "resume": record(7, 30000) }
            },
            "resume": [record(3, 95000), record(5, 1000), record(6, 1000)],
            "recentAlbums": [{ "id": 10, "title": "A" }],
            "works": { "type": "all", "generation": "g1", "batch": 0, "pages": 3, "count": 30,
                       "items": [{ "id": 15, "title": "W", "workType": "anime", "year": 2011,
                                   "roleCounts": [{ "role": "op", "count": 1 }], "artworkUrl": "/a" }] },
            "songs": { "generation": "g1", "batch": 0, "pages": 1, "count": 1,
                       "items": [{ "track": track(8), "role": "ed", "work": { "id": 15, "title": "W" } }] },
            "chronicle": { "years": [{ "year": 2011, "count": 2 }], "selectedYear": 2011,
                           "result": { "year": 2011, "total": 2, "albums": [{ "id": 10 }] } },
            "playlists": [],
            "stats": { "albums": 1, "tracks": 9, "works": 1, "lastScanAt": "", "pendingReviewCount": 0 }
        }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/home/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "all", "generation": "g2", "batch": 0, "pages": 3, "count": 30, "renewed": true, "items": []
        })))
        .mount(&server)
        .await;
    get(
        &server,
        "/api/v1/works/15/tracks",
        json!({ "items": [track(1), track(2), track(1)] }),
    )
    .await;
    get(
        &server,
        "/api/v1/tracks/1/works",
        json!({ "items": [{ "id": 15, "title": "W", "seriesId": 7 }] }),
    )
    .await;
    get(
        &server,
        "/api/v1/daily-mixes",
        json!({ "day": "2026-10-28", "status": "ready", "replacementsLeft": 1,
                "items": [{ "id": 4, "title": "深夜", "status": "active", "day": "2026-10-28", "trackCount": 3 }] }),
    )
    .await;
    get(
        &server,
        "/api/v1/daily-mixes/4",
        json!({
            "mix": { "id": 4, "title": "深夜", "status": "active", "day": "2026-10-28" },
            "tracks": [
                { "position": 0, "bucket": "familiar", "track": { "id": 20, "missing": true } },
                { "position": 1, "bucket": "explore", "track": track(21) },
                { "position": 2, "bucket": "familiar", "track": track(22) }
            ]
        }),
    )
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

struct Harness {
    core: Option<CoreHandle>,
    events: mpsc::Receiver<Event>,
    engine: Arc<FakeEngine>,
}

impl Harness {
    fn start(server: &MockServer) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let data_dir =
            std::env::temp_dir().join(format!("032player-m032-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        let engine = Arc::new(FakeEngine::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let config = CoreConfig {
            settings: Settings::default(),
            data_dir,
            secrets: Arc::new(MemorySecrets::default()),
        };
        let core = client_core::start(engine.clone(), config, move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let h = Self {
            core: Some(core),
            events: rx,
            engine,
        };
        h.send(Command::Login {
            server_url: server.uri(),
            api_token: "api".into(),
            media_token: "media".into(),
        });
        h.wait("login", |e| matches!(e, Event::LoggedIn { .. }));
        h
    }

    fn send(&self, command: Command) {
        self.core.as_ref().unwrap().send(command);
    }

    fn engine_event(&self, event: EngineEvent) {
        self.core.as_ref().unwrap().engine_event(event);
    }

    async fn wait_call(&self, call: &str) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut calls = Vec::new();
        loop {
            calls.extend(self.engine.take());
            if calls.iter().any(|c| c == call) {
                return calls;
            }
            assert!(Instant::now() < deadline, "no {call}: {calls:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn wait(&self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(e) if pred(&e) => return e,
                Ok(_) => {}
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    async fn shutdown(mut self) {
        let core = self.core.take().unwrap();
        tokio::task::spawn_blocking(move || core.shutdown())
            .await
            .unwrap();
    }
}

async fn requests(server: &MockServer, verb: &str, p: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == p)
        .collect()
}

/// Bodies of playback events of `type`, polled until one arrives.
async fn events_of(server: &MockServer, kind: &str) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found: Vec<Value> = requests(server, "POST", "/api/v1/playback/events")
            .await
            .into_iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .filter(|b| b["type"] == kind)
            .collect();
        if !found.is_empty() {
            return found;
        }
        assert!(Instant::now() < deadline, "no {kind} event");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn home_resume_and_rotations() {
    let server = server(json!({ "home": true })).await;
    let h = Harness::start(&server);
    h.send(Command::LoadHome);
    let Event::Home(home) = h.wait("home", |e| matches!(e, Event::Home(_))) else {
        unreachable!()
    };
    let spotlight = home.spotlight.as_ref().unwrap();
    assert_eq!(
        spotlight.tabs,
        [SpotlightTab::Recent, SpotlightTab::Listening]
    );
    assert_eq!(spotlight.albums[0].more, "查看全部 9 首");
    assert_eq!(spotlight.albums[1].resume, "从 0:30 继续");
    assert_eq!(home.resume.len(), 2);
    assert_eq!(home.resume[0].progress, "1:35 / 4:00");
    assert_eq!(home.works.items[0].roles, "OP 1");
    assert_eq!(home.songs.items[0].role, "ED");
    // 最近播放: played tracks without 接着听's rows (3 and 5).
    let Event::HomeRecent(recent) = h.wait(
        "recent",
        |e| matches!(e, Event::HomeRecent(r) if !r.is_empty()),
    ) else {
        unreachable!()
    };
    let ids: Vec<i64> = recent.iter().map(|r| r.track.id).collect();
    assert_eq!(ids, [1, 4]);

    // 接着听 starts at the saved position.
    h.send(Command::PlayTrack(TrackList::HomeResume, 0));
    h.wait_call("load /tracks/3/stream").await;
    h.engine_event(EngineEvent::FileLoaded { path: None });
    h.wait_call("seek 95").await;
    // Like the Electron client: a new play from 0, then a seek.
    let start = events_of(&server, "start").await;
    assert_eq!(start[0]["trackId"], 3);
    assert_eq!(start[0]["positionMillis"], 0);
    let seek = events_of(&server, "seek").await;
    assert_eq!(seek[0]["positionMillis"], 95000);

    // 从 0:30 继续 on 最近在听.
    h.send(Command::ResumeListening);
    h.wait_call("load /tracks/7/stream").await;
    h.engine_event(EngineEvent::FileLoaded { path: None });
    h.wait_call("seek 30").await;

    // The next works batch carries the generation; renewed shows a notice.
    h.send(Command::HomeWorks {
        kind: None,
        next: true,
    });
    h.wait(
        "renewed",
        |e| matches!(e, Event::Notice(n) if n == "今日推荐已更新"),
    );
    let r = requests(&server, "GET", "/api/v1/home/works").await;
    let q: Vec<(String, String)> = r[0].url.query_pairs().into_owned().collect();
    assert!(q.contains(&("batch".into(), "1".into())));
    assert!(q.contains(&("generation".into(), "g1".into())));
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn home_falls_back_without_the_endpoint() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/home"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "error": { "code": "not_found" } })),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "apiRevision": 3, "features": {}
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![], 0)))
        .mount(&server)
        .await;
    get(&server, "/api/v1/albums", page(vec![], 0)).await;
    get(&server, "/api/v1/playlists", page(vec![], 0)).await;
    let h = Harness::start(&server);
    h.send(Command::LoadHome);
    h.wait("unavailable", |e| *e == Event::HomeUnavailable);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daily_mix_plays_with_its_context_and_queues_without() {
    let server = server(json!({ "home": true, "dailyMixes": true, "playbackContext": true })).await;
    let h = Harness::start(&server);
    h.send(Command::LoadHome);
    let Event::DailyMixes { items, generating } =
        h.wait("mixes", |e| matches!(e, Event::DailyMixes { .. }))
    else {
        unreachable!()
    };
    assert!(!generating);
    assert_eq!(items[0].title, "深夜");
    // Next to daily mixes 最近播放 shows max(2, 7 - 1) rows, none excluded.
    h.wait(
        "recent",
        |e| matches!(e, Event::HomeRecent(r) if r.len() == 3),
    );

    h.send(Command::OpenDailyMix(4));
    let Event::Detail { view, tracks } = h.wait("mix", |e| {
        matches!(
            e,
            Event::Detail {
                view: DetailView::DailyMix(_),
                ..
            }
        )
    }) else {
        unreachable!()
    };
    let DetailView::DailyMix(mix) = view else {
        unreachable!()
    };
    assert!(mix.current && mix.rows[0].missing && mix.rows[1].explore);
    assert_eq!(tracks.len(), 3);

    // Row 2 (position 2) of the open mix; the missing row is skipped.
    h.send(Command::Play(TrackSource::Track(TrackList::Detail, 2)));
    h.wait_call("load /tracks/22/stream").await;
    let start = events_of(&server, "start").await;
    assert_eq!(
        start[0]["context"],
        json!({ "type": "dailyMix", "id": 4, "position": 2 })
    );
    h.wait("badge", |e| {
        matches!(
            e,
            Event::PlayingSources(PlayingSources { mix: Some(4), .. })
        )
    });
    let Event::Queue { items, .. } = h.wait("queue", |e| matches!(e, Event::Queue { .. })) else {
        unreachable!()
    };
    assert_eq!(items.len(), 2);

    // Queued from the home row: no context.
    h.send(Command::Play(TrackSource::DailyMix(4)));
    h.wait_call("load /tracks/21/stream").await;
    h.send(Command::Enqueue {
        source: TrackSource::DailyMix(4),
        at: client_core::QueuePosition::Next,
    });
    h.wait(
        "queued",
        |e| matches!(e, Event::Queue { items, .. } if items.len() == 4),
    );
    h.send(Command::Next);
    h.wait_call("load /tracks/21/stream").await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let starts = events_of(&server, "start").await;
        if starts.len() >= 3 {
            assert_eq!(starts[1]["context"]["position"], 1);
            assert!(starts[2].get("context").is_none(), "{:?}", starts[2]);
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn works_play_and_light_up() {
    let server = server(json!({ "trackWorks": true })).await;
    let h = Harness::start(&server);
    h.send(Command::Play(TrackSource::Work(15)));
    h.wait_call("load /tracks/1/stream").await;
    let Event::Queue { items, .. } = h.wait("queue", |e| matches!(e, Event::Queue { .. })) else {
        unreachable!()
    };
    assert_eq!(items.len(), 2, "duplicates dropped");
    h.wait(
        "badges",
        |e| matches!(e, Event::PlayingSources(s) if s.works == [15] && s.series == [7]),
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_pages_and_clears() {
    let server = server(json!({})).await;
    get(
        &server,
        "/api/v1/playback/history",
        page(vec![record(3, 1000)], 1),
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/playback/history"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let h = Harness::start(&server);
    h.send(Command::QueryHistory);
    h.wait("reset", |e| *e == Event::HistoryReset);
    let Event::HistoryAppended { items, total } =
        h.wait("history", |e| matches!(e, Event::HistoryAppended { .. }))
    else {
        unreachable!()
    };
    assert_eq!((items[0].state.as_str(), total), ("已中断", 1));
    h.send(Command::PlayTrack(TrackList::History, 0));
    h.wait_call("load /tracks/3/stream").await;
    h.send(Command::ClearHistory);
    h.wait(
        "cleared",
        |e| matches!(e, Event::Notice(n) if n == "已清空播放历史"),
    );
    h.wait("reload", |e| *e == Event::HistoryReset);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lyrics_translation_offset_and_search() {
    let server = server(json!({ "lyricsOnline": true, "lyricsTranslation": true })).await;
    get(
        &server,
        "/api/v1/tracks/1/lyrics",
        json!({ "synced": true, "source": "netease", "sourceName": "网易云",
                "lines": [{ "timeMs": 1000, "text": "一" }, { "timeMs": 5000, "text": "二" }] }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tracks/1/lyrics/translation"))
        .and(body_json(json!({ "targetLanguage": "zh-Hans" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lines": ["one", "二"], "source": "ai"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/lyrics/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": [
            { "source": "lrclib", "id": "9", "name": "b", "artist": "x", "score": 0.5, "synced": false },
            { "source": "netease", "id": "3", "name": "a", "artist": "x", "score": 0.02, "synced": true }
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/tracks/1/lyrics/source"))
        .and(body_json(
            json!({ "source": "netease", "id": "3", "name": "a", "artist": "x" }),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let h = Harness::start(&server);
    h.send(Command::Play(TrackSource::Work(15)));
    h.wait_call("load /tracks/1/stream").await;
    h.wait(
        "lyrics",
        |e| matches!(e, Event::Lyrics { lines, .. } if lines.len() == 2),
    );
    h.wait(
        "tools",
        |e| matches!(e, Event::LyricsTools(t) if t.can_translate && t.can_search),
    );
    h.engine_event(EngineEvent::FileLoaded { path: None });

    // Bilingual: the translation under lines that differ.
    h.send(Command::ToggleTranslation);
    let Event::Lyrics { lines, .. } = h.wait("translated", |e| {
        matches!(e, Event::Lyrics { lines, .. } if lines.iter().any(|l| !l.translation.is_empty()))
    }) else {
        unreachable!()
    };
    assert_eq!(lines[0].translation, "one");
    assert_eq!(lines[1].translation, "");
    // Translation only.
    h.send(Command::SetLyricsSettings(LyricsSettings {
        translation_mode: TranslationMode::Translated,
        ..LyricsSettings::default()
    }));
    h.wait("translated only", |e| {
        matches!(e, Event::Lyrics { lines, .. } if lines.first().is_some_and(|l| l.text == "one"))
    });

    // An offset of +1 s shows the first line at 0 s.
    h.engine_event(EngineEvent::TimePos(0.2));
    h.send(Command::SetLyricsOffset(1000));
    h.wait("active", |e| *e == Event::LyricsActive(Some(0)));
    h.send(Command::SeekToLyric(1));
    h.wait_call("seek 4").await;

    // Search sorts by score; applying reloads the lyrics.
    h.send(Command::SearchLyrics {
        name: "t1".into(),
        artist: "x".into(),
    });
    let Event::LyricsSearchResults(results) =
        h.wait("results", |e| matches!(e, Event::LyricsSearchResults(_)))
    else {
        unreachable!()
    };
    assert_eq!(results[0].meta, "NetEase - 3 - 同步");
    assert_eq!(results[0].score, "98.00%");
    h.send(Command::ApplyLyrics(0));
    h.wait(
        "reloaded",
        |e| matches!(e, Event::Lyrics { lines, .. } if lines.is_empty()),
    );
    h.wait(
        "tools after reload",
        |e| matches!(e, Event::LyricsTools(t) if !t.translation_shown && t.offset_ms == 1000),
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_lyrics_dropped_and_unsynced_clears_timeline() {
    let server = server(json!({ "lyricsOnline": true })).await;
    // Track 1 returns slow synced lyrics; track 2 returns fast unsynced lyrics.
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/1/lyrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_json(json!({
                    "synced": true,
                    "source": "netease",
                    "lines": [{ "timeMs": 1000, "text": "synced-1" }]
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/2/lyrics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "synced": false,
            "source": "local",
            "lines": [{ "text": "plain-2" }]
        })))
        .mount(&server)
        .await;
    get(
        &server,
        "/api/v1/albums/10",
        json!({
            "id": 10,
            "title": "A",
            "trackCount": 2,
            "tracks": [track(1), track(2)]
        }),
    )
    .await;

    let h = Harness::start(&server);
    h.send(Command::Play(TrackSource::Album(10)));
    h.wait_call("load /tracks/1/stream").await;
    // Immediately switch to track 2 while track 1's lyrics request is still in flight.
    h.send(Command::Next);
    h.wait_call("load /tracks/2/stream").await;
    h.wait("track 2 unsynced lyrics", |e| {
        matches!(
            e,
            Event::Lyrics {
                synced: false,
                lines,
                ..
            } if lines.first().is_some_and(|l| l.text == "plain-2")
        )
    });
    // Wait past track 1's delay; stale synced lyrics must be dropped.
    tokio::time::sleep(Duration::from_millis(350)).await;
    h.engine_event(EngineEvent::TimePos(2.0));
    tokio::time::sleep(Duration::from_millis(50)).await;
    while let Ok(ev) = h.events.try_recv() {
        assert!(
            !matches!(ev, Event::Lyrics { synced: true, .. }),
            "stale synced lyrics leaked into track 2: {ev:?}"
        );
        assert!(
            !matches!(ev, Event::LyricsActive(Some(_))),
            "unsynced lyrics should not activate timeline lines: {ev:?}"
        );
    }
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replace_daily_mix_invalidates_cached_mixes() {
    let server = server(json!({ "home": true, "dailyMixes": true })).await;
    let mixes_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_clone = mixes_hits.clone();
    Mock::given(method("GET"))
        .and(path("/api/v1/daily-mixes"))
        .respond_with(move |_req: &Request| {
            let n = hits_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let id = if n == 0 { 101 } else { 102 };
            ResponseTemplate::new(200).set_body_json(json!({
                "day": "2026-11-23",
                "status": "ready",
                "replacementsLeft": 1,
                "items": [{
                    "id": id,
                    "day": "2026-11-23",
                    "status": "active",
                    "title": format!("Mix {id}"),
                    "trackCount": 1
                }]
            }))
        })
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/daily-mixes/101/regenerate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 102,
            "day": "2026-11-23",
            "status": "active",
            "title": "Mix 102",
            "trackCount": 1
        })))
        .mount(&server)
        .await;
    get(
        &server,
        "/api/v1/daily-mixes/102",
        json!({
            "mix": { "id": 102, "day": "2026-11-23", "status": "active", "title": "Mix 102" },
            "tracks": [{ "position": 0, "bucket": "familiar", "track": track(1) }]
        }),
    )
    .await;

    let h = Harness::start(&server);
    h.send(Command::LoadHome);
    h.wait(
        "initial mixes",
        |e| matches!(e, Event::DailyMixes { items, .. } if items.first().is_some_and(|m| m.id == 101)),
    );
    h.send(Command::ReplaceDailyMix(101));
    h.wait("replaced mix event", |e| {
        matches!(e, Event::DailyMixReplaced { from: 101, to: 102 })
    });
    h.wait(
        "reloaded mixes",
        |e| matches!(e, Event::DailyMixes { items, .. } if items.first().is_some_and(|m| m.id == 102)),
    );
    assert!(
        mixes_hits.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "daily mixes cache must be invalidated on replace"
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daily_mix_missing_tracks_are_filtered_on_enqueue() {
    let server = server(json!({ "home": true, "dailyMixes": true })).await;
    let mut missing_track = track(1);
    missing_track["missing"] = json!(true);
    get(
        &server,
        "/api/v1/daily-mixes/201",
        json!({
            "mix": { "id": 201, "day": "2026-11-23", "status": "active", "title": "Mix Missing" },
            "tracks": [
                { "position": 0, "bucket": "familiar", "track": missing_track },
                { "position": 1, "bucket": "explore", "track": track(2) }
            ]
        }),
    )
    .await;

    let h = Harness::start(&server);
    h.send(Command::OpenDailyMix(201));
    h.wait(
        "daily mix detail",
        |e| matches!(e, Event::Detail { view: DetailView::DailyMix(m), .. } if m.id == 201),
    );
    h.send(Command::Enqueue {
        source: TrackSource::List(TrackList::Detail),
        at: client_core::QueuePosition::Last,
    });
    h.wait(
        "queue excludes missing row",
        |e| matches!(e, Event::Queue { items, .. } if items.len() == 1 && items[0].title == "t2"),
    );
    h.shutdown().await;
}
