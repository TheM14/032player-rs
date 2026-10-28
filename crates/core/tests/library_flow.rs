//! M2 browsing against a mock server: list queries, favorites, details,
//! playlists, search, similar tracks, transcoding and queue drag and drop.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AlbumFilter, AlbumSort, AudioDevice, Command, CoreConfig, CoreHandle, DetailView, Engine,
    EngineEvent, Event, FavoriteKind, MemorySecrets, Settings, SortOrder, TrackFilter, TrackList,
    TrackSort, TrackSource, Transcode,
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

/// Keeps the path and query after `/api/v1`, without the media token.
fn short(url: &str) -> String {
    let rest = url.split("/api/v1").nth(1).unwrap_or(url);
    rest.replace("mediaToken=media&", "")
        .replace("?mediaToken=media", "")
        .replace("&mediaToken=media", "")
}

impl Engine for FakeEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.push(format!("load {}", short(url)))
    }
    fn append(&self, url: &str) -> Result<(), String> {
        self.push(format!("append {}", short(url)))
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

fn track(id: i64, title: &str) -> Value {
    json!({
        "id": id, "title": title, "artist": "歌手", "album": "专辑", "albumId": 10,
        "artists": [{ "id": 5, "name": "歌手" }],
        "durationMillis": 240000, "streamUrl": format!("/api/v1/tracks/{id}/stream"),
        "codec": "flac", "bitDepth": 16, "sampleRate": 44100, "trackNumber": id,
    })
}

fn page(items: Vec<Value>, total: usize) -> Value {
    json!({ "items": items, "limit": 500, "offset": 0, "total": total })
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "apiRevision": 3,
            "features": { "browse": true, "playlistArtwork": true },
            "transcode": { "available": true, "formats": { "mp3": { "available": true } } },
        })))
        .mount(&server)
        .await;
    // Search (with q) before the media token probe.
    Mock::given(path("/api/v1/tracks"))
        .and(query_param("q", "夜"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![track(3, "夜")], 1)))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/artists"))
        .and(query_param("q", "夜"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![json!({ "id": 5, "name": "歌手", "albumCount": 1 })],
            1,
        )))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/albums"))
        .and(query_param("q", "夜"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![], 0)))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![], 0)))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![], 0)))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![track(1, "a"), track(2, "b"), track(3, "c")], 3)),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/playlists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                json!({ "id": 2, "name": "夜", "itemCount": 3, "revision": 7, "durationMillis": 720000 }),
                json!({ "id": 3, "name": "A", "itemCount": 0, "revision": 1 }),
            ],
            2,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/playlists/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "playlist": { "id": 2, "name": "夜", "itemCount": 3, "revision": 7 },
            "tracks": [track(1, "a"), track(2, "b"), track(3, "c")],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/albums/10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "album": { "id": 10, "title": "专辑", "trackCount": 3, "discCount": 1,
                       "artists": [{ "id": 5, "name": "歌手" }] },
            "tracks": [track(1, "a"), track(2, "b"), track(3, "c")],
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

struct Harness {
    core: Option<CoreHandle>,
    events: mpsc::Receiver<Event>,
    engine: Arc<FakeEngine>,
}

impl Harness {
    async fn start(server: &MockServer, settings: Settings) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let data_dir =
            std::env::temp_dir().join(format!("032player-library-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        let engine = Arc::new(FakeEngine::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let config = CoreConfig {
            settings,
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
        h.wait("tracks", |e| {
            matches!(e, Event::TracksAppended { total: 3, .. })
        });
        h
    }

    fn send(&self, command: Command) {
        self.core.as_ref().unwrap().send(command);
    }

    fn engine_event(&self, event: EngineEvent) {
        self.core.as_ref().unwrap().engine_event(event);
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

    fn queue_titles(&self) -> Vec<String> {
        match self.wait("queue", |e| matches!(e, Event::Queue { .. })) {
            Event::Queue { items, .. } => items.into_iter().map(|i| i.title).collect(),
            _ => unreachable!(),
        }
    }

    async fn shutdown(mut self) {
        let core = self.core.take().unwrap();
        tokio::task::spawn_blocking(move || core.shutdown())
            .await
            .unwrap();
    }
}

async fn requests(server: &MockServer, method: &str, p: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == method && r.url.path() == p)
        .collect()
}

fn param(r: &Request, key: &str) -> Option<String> {
    r.url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn is_playlist_detail(e: &Event) -> bool {
    matches!(
        e,
        Event::Detail {
            view: DetailView::Playlist(_),
            ..
        }
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn random_lists_keep_one_seed_per_load() {
    let server = server().await;
    let albums = |offset: usize, n: usize| {
        (offset..offset + n)
            .map(|i| json!({ "id": i, "title": format!("al{i}") }))
            .collect::<Vec<_>>()
    };
    Mock::given(path("/api/v1/browse/albums"))
        .and(query_param("sort", "random"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(albums(0, 500), 501)))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/albums"))
        .and(query_param("sort", "random"))
        .and(query_param("offset", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(albums(500, 1), 501)))
        .with_priority(1)
        .mount(&server)
        .await;
    let h = Harness::start(&server, Settings::default()).await;
    let filter = AlbumFilter {
        sort: AlbumSort::Random,
        year_from: Some(1990),
        year_to: Some(2000),
        ..AlbumFilter::default()
    };
    for _ in 0..2 {
        h.send(Command::QueryAlbums {
            favorites: false,
            filter: filter.clone(),
        });
        h.wait("reset", |e| *e == Event::AlbumsReset);
        h.wait(
            "page 1",
            |e| matches!(e, Event::AlbumsAppended { items, .. } if items.len() == 500),
        );
        h.wait(
            "page 2",
            |e| matches!(e, Event::AlbumsAppended { items, .. } if items.len() == 1),
        );
    }
    let random: Vec<Request> = requests(&server, "GET", "/api/v1/browse/albums")
        .await
        .into_iter()
        .filter(|r| param(r, "sort").as_deref() == Some("random"))
        .collect();
    assert_eq!(random.len(), 4);
    let seeds: Vec<String> = random.iter().map(|r| param(r, "seed").unwrap()).collect();
    assert_eq!(seeds[0], seeds[1], "pages of one load share a seed");
    assert_eq!(seeds[2], seeds[3]);
    assert_ne!(seeds[0], seeds[2], "loading again reshuffles");
    assert_eq!(param(&random[0], "yearFrom").as_deref(), Some("1990"));
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn favorites_are_sorted_locally_and_follow_changes() {
    let server = server().await;
    Mock::given(path("/api/v1/favorites/tracks"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![track(1, "b"), track(2, "a"), track(3, "c")], 3)),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tracks/2/favorite"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let h = Harness::start(&server, Settings::default()).await;
    let filter = TrackFilter {
        sort: TrackSort::Title,
        order: SortOrder::Asc,
        ..TrackFilter::default()
    };
    h.send(Command::QueryTracks {
        favorites: true,
        filter,
    });
    let Event::FavoriteTracks(items) =
        h.wait("favorites", |e| matches!(e, Event::FavoriteTracks(_)))
    else {
        unreachable!()
    };
    let titles: Vec<&str> = items.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["a", "b", "c"]);

    // A row plays its whole list from there.
    h.send(Command::Play(TrackSource::Track(TrackList::Favorites, 1)));
    h.wait(
        "playing",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 1),
    );
    assert_eq!(h.queue_titles(), ["a", "b", "c"]);

    h.send(Command::SetFavorite {
        kind: FavoriteKind::Track,
        id: 2,
        favorite: false,
    });
    h.wait("favorite", |e| {
        *e == Event::Favorite {
            kind: FavoriteKind::Track,
            id: 2,
            favorite: false,
        }
    });
    // The favorites page reloads.
    h.wait("refresh", |e| matches!(e, Event::FavoriteTracks(_)));
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn album_detail_search_and_similar() {
    let server = server().await;
    Mock::given(path("/api/v1/tracks/2/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "track": track(3, "c"), "score": 0.9 },
        ]})))
        .mount(&server)
        .await;
    let h = Harness::start(&server, Settings::default()).await;

    h.send(Command::OpenAlbum(10));
    let Event::Detail { view, tracks } = h.wait("album", |e| matches!(e, Event::Detail { .. }))
    else {
        unreachable!()
    };
    let DetailView::Album(album) = view else {
        panic!("not an album: {view:?}")
    };
    assert_eq!(album.quality.as_deref(), Some("FLAC / 16bit / 44.1kHz"));
    assert_eq!(album.artist_id, 5);
    assert_eq!(tracks.len(), 3);
    h.send(Command::Play(TrackSource::Track(TrackList::Detail, 1)));
    h.wait(
        "playing",
        |e| matches!(e, Event::NowPlaying(Some(n)) if n.track_id == 2),
    );

    h.send(Command::Search(" 夜 ".into()));
    let Event::SearchResults {
        query,
        artists,
        albums,
        tracks,
    } = h.wait("search", |e| matches!(e, Event::SearchResults { .. }))
    else {
        unreachable!()
    };
    assert_eq!(query, "夜");
    assert_eq!((artists.len(), albums.len(), tracks.len()), (1, 0, 1));

    h.send(Command::OpenSimilar(TrackList::Library, 1));
    let Event::Detail { view, tracks } = h.wait("similar", |e| matches!(e, Event::Detail { .. }))
    else {
        unreachable!()
    };
    assert_eq!(
        view,
        DetailView::Similar {
            track_id: 2,
            title: "b".into()
        }
    );
    assert_eq!(tracks[0].id, 3);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn playlist_edits() {
    let server = server().await;
    // The first remove meets a concurrent edit; the retry reads the revision again.
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists/2/items/remove"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": { "code": "playlist_conflict", "message": "stale" }
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists/2/items/remove"))
        .and(body_json(json!({ "trackIds": [2], "expectedRevision": 7 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "removed": 1 })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/playlists/2/order"))
        .and(body_json(
            json!({ "trackIds": [2, 3, 1], "expectedRevision": 7 }),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists/3/items/append"))
        .and(body_json(json!({ "albumId": 10 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "added": 3 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists"))
        .and(body_json(
            json!({ "name": "新", "description": "", "trackIds": [1, 2, 3] }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9, "name": "新" })))
        .expect(1)
        .mount(&server)
        .await;
    let h = Harness::start(&server, Settings::default()).await;
    h.send(Command::OpenPlaylist(2));
    h.wait("playlist", is_playlist_detail);

    h.send(Command::RemoveFromPlaylist {
        playlist_id: 2,
        index: 1,
    });
    // Reloads the playlists and the open playlist (in either order).
    let mut choices = None;
    let mut reloaded = false;
    while choices.is_none() || !reloaded {
        match h.wait("reloads", |e| {
            matches!(e, Event::PlaylistChoices(_)) || is_playlist_detail(e)
        }) {
            Event::PlaylistChoices(c) => choices = Some(c),
            _ => reloaded = true,
        }
    }
    let choices = choices.unwrap();
    let names: Vec<&str> = choices.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["A", "夜"]);
    assert_eq!(choices[1].duration.as_deref(), Some("12 分钟"));
    assert_eq!(
        requests(&server, "POST", "/api/v1/playlists/2/items/remove")
            .await
            .len(),
        2
    );

    h.send(Command::MovePlaylistItem {
        playlist_id: 2,
        from: 0,
        to: 2,
    });
    h.wait("reload", is_playlist_detail);

    h.send(Command::AddToPlaylist {
        playlist_id: 3,
        source: TrackSource::Album(10),
    });
    h.wait("added", |e| *e == Event::Notice("已添加 3 首到歌单".into()));

    h.send(Command::CreatePlaylist {
        name: " 新 ".into(),
        description: String::new(),
        tracks: Some(TrackSource::List(TrackList::Detail)),
    });
    h.wait("created", |e| {
        *e == Event::Notice("已创建歌单「新」".into())
    });
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lossy_transcode_seeks_by_restarting_the_stream() {
    let server = server().await;
    let settings = Settings {
        transcode: Some(Transcode::Mp3(320)),
        ..Settings::default()
    };
    let h = Harness::start(&server, settings).await;
    h.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    h.wait("playing", |e| matches!(e, Event::NowPlaying(Some(_))));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.engine.take(),
        ["load /tracks/1/transcode.mp3?bitrate=320"]
    );
    h.engine_event(EngineEvent::FileLoaded {
        path: Some(format!(
            "{}/api/v1/tracks/1/transcode.mp3?mediaToken=media&bitrate=320",
            server.uri()
        )),
    });
    h.send(Command::Seek {
        position_ms: 60_000,
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let calls = h.engine.take();
    assert!(
        calls.contains(&"load /tracks/1/transcode.mp3?bitrate=320&offsetMs=60000".to_owned()),
        "{calls:?}"
    );
    h.engine_event(EngineEvent::FileLoaded {
        path: Some(format!(
            "{}/api/v1/tracks/1/transcode.mp3?mediaToken=media&bitrate=320&offsetMs=60000",
            server.uri()
        )),
    });
    // Engine time is relative to the restarted stream.
    h.engine_event(EngineEvent::TimePos(1.0));
    h.wait("position", |e| {
        matches!(
            e,
            Event::Position {
                position_ms: 61_000,
                ..
            }
        )
    });
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_drag_and_drop() {
    let server = server().await;
    let h = Harness::start(&server, Settings::default()).await;
    h.send(Command::Play(TrackSource::Track(TrackList::Library, 0)));
    assert_eq!(h.queue_titles(), ["a", "b", "c"]);
    // The first queue's uids are 1..=3.
    h.send(Command::MoveQueueItem { uid: 3, to: 0 });
    assert_eq!(h.queue_titles(), ["c", "a", "b"]);
    h.shutdown().await;
}
