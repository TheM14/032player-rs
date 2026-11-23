//! M3 endpoints: home, works, series, daily mixes, history, lyrics tools.

use api::{Client, LyricsSearchResult, WorkListQuery, WorkSort, work_poster_path};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client(server: &MockServer) -> Client {
    Client::new(&server.uri(), "api-token", "media-token").unwrap()
}

async fn mount(server: &MockServer, verb: &str, route: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn home_decodes_the_documented_example() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/v1/home",
        200,
        json!({
            "empty": false,
            "rotationDay": "2026-10-12",
            "spotlight": {
                "tabs": ["recent", "listening", "series", "random"],
                "defaultTab": "recent",
                "recent": {
                    "focus": "recent",
                    "play": { "type": "album", "id": 412 },
                    "album": { "id": 412, "title": "OST", "trackCount": 24, "discCount": 1 },
                    "tracks": [{ "id": 9001, "albumId": 412, "title": "春日影", "codec": "flac", "bitDepth": 24, "sampleRate": 96000 }],
                    "previewDurationMillis": 4820000,
                    "noteTime": "2026-10-11T14:02:11.000Z",
                    "resume": null
                },
                "listening": {
                    "focus": "listening",
                    "play": { "type": "album", "id": 120 },
                    "album": { "id": 120 },
                    "tracks": [],
                    "previewDurationMillis": 0,
                    "noteTime": "",
                    "resume": { "trackId": 1503, "positionMillis": 95000, "durationMillis": 240000, "playCount": 3, "lastPlayedAt": "x", "track": { "id": 1503 } }
                },
                "series": {
                    "isSeries": true, "seriesId": 7, "title": "BanG Dream!",
                    "play": { "type": "series", "id": 7 },
                    "workIds": [31, 32],
                    "works": [{ "work": { "id": 31, "title": "MyGO!!!!!", "type": "anime", "year": 2023, "trackCount": 40 }, "roleCounts": [{ "role": "op", "count": 1 }] }],
                    "albums": [{ "workId": 31, "workTitle": "MyGO!!!!!", "album": { "albumId": 412, "workId": 31, "title": "…", "artworkUrl": "/api/v1/artwork/903", "trackCount": 24, "year": 2023 }, "play": { "type": "album", "id": 412 } }],
                    "albumCount": 9, "trackTotal": 112, "yearFrom": 2017, "yearTo": 2025
                },
                "random": null
            },
            "resume": [{ "trackId": 1503, "positionMillis": 95000, "durationMillis": 240000, "playCount": 3, "lastPlayedAt": "x", "track": { "id": 1503 } }],
            "recentAlbums": [{ "id": 412 }],
            "works": {
                "type": "all", "generation": "2026-10-12.k3x9a", "batch": 0, "pages": 4, "count": 87, "renewed": false,
                "items": [{ "id": 31, "title": "MyGO!!!!!", "workType": "anime", "year": 2023, "trackCount": 40, "posterUrl": "", "coverUrl": "/api/v1/artwork/903", "artworkUrl": "/api/v1/artwork/903", "series": { "id": 7, "title": "BanG Dream!" }, "roleCounts": [{ "role": "op", "count": 1 }], "play": { "type": "work", "id": 31 } }]
            },
            "songs": {
                "generation": "2026-10-12.k3x9a", "batch": 0, "pages": 6, "count": 112, "renewed": false,
                "items": [{ "track": { "id": 9001, "title": "春日影" }, "role": "op", "work": { "id": 31, "title": "MyGO!!!!!", "year": 2023 }, "play": { "type": "track", "id": 9001 } }]
            },
            "chronicle": {
                "years": [{ "year": 2019, "count": 12 }, { "year": 2023, "count": 40 }],
                "selectedYear": 2023,
                "result": { "year": 2023, "total": 40, "albums": [{ "id": 412 }] }
            },
            "playlists": [{ "id": 3, "name": "通勤", "itemCount": 52 }],
            "stats": { "albums": 812, "tracks": 10342, "works": 87, "lastScanAt": "", "pendingReviewCount": 3 }
        }),
    )
    .await;

    let home = client(&server).await.home().await.unwrap();
    let spotlight = home.spotlight.unwrap();
    assert_eq!(spotlight.tabs.len(), 4);
    assert_eq!(spotlight.recent.unwrap().tracks[0].bit_depth, Some(24));
    assert_eq!(
        spotlight.listening.unwrap().resume.unwrap().position_millis,
        95000
    );
    let series = spotlight.series.unwrap();
    assert_eq!(series.play.kind, "series");
    assert_eq!(series.works[0].work.kind, "anime");
    assert_eq!(series.albums[0].album.album_id, 412);
    assert!(spotlight.random.is_none());
    assert_eq!(home.works.pages, 4);
    assert_eq!(home.works.items[0].series.as_ref().unwrap().id, 7);
    assert_eq!(home.songs.items[0].work.title, "MyGO!!!!!");
    assert_eq!(home.chronicle.unwrap().result.albums[0].id, 412);
    assert_eq!(home.playlists[0].item_count, 52);
    assert_eq!(home.stats.pending_review_count, 3);
}

#[tokio::test]
async fn empty_home_has_no_spotlight() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/v1/home",
        200,
        json!({
            "empty": true, "spotlight": null, "resume": [], "recentAlbums": [],
            "works": { "type": "all", "generation": "", "batch": 0, "pages": 0, "count": 0, "renewed": false, "items": [] },
            "songs": { "generation": "", "batch": 0, "pages": 0, "count": 0, "renewed": false, "items": [] },
            "chronicle": null, "playlists": [],
            "stats": { "albums": 0, "tracks": 0, "works": 0, "lastScanAt": "", "pendingReviewCount": 0 }
        }),
    )
    .await;
    let home = client(&server).await.home().await.unwrap();
    assert!(home.empty);
    assert!(home.spotlight.is_none() && home.chronicle.is_none());
}

#[tokio::test]
async fn rotation_pages_carry_the_generation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/home/works"))
        .and(query_param("type", "anime"))
        .and(query_param("batch", "2"))
        .and(query_param("generation", "g1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "anime", "generation": "g2", "batch": 0, "pages": 3, "count": 30, "renewed": true, "items": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/home/songs"))
        .and(query_param("batch", "0"))
        .and(query_param_is_missing("generation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "generation": "g2", "batch": 0, "pages": 1, "count": 2, "renewed": false, "items": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/home/chronicle"))
        .and(query_param("year", "2019"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "year": 2019, "total": 0, "albums": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/home/spotlight/random"))
        .and(query_param("exclude", "77"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "focus": "random", "album": { "id": 78 }, "tracks": []
        })))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let works = c.home_works("anime", 2, "g1").await.unwrap();
    assert!(works.renewed);
    assert_eq!(works.generation, "g2");
    assert_eq!(c.home_songs(0, "").await.unwrap().count, 2);
    assert_eq!(c.home_chronicle(2019).await.unwrap().total, 0);
    assert_eq!(c.home_spotlight_random(77).await.unwrap().album.id, 78);
}

#[tokio::test]
async fn works_and_series() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/works"))
        .and(query_param("q", "fate"))
        .and(query_param("type", "game"))
        .and(query_param("sort", "year"))
        .and(query_param("offset", "60"))
        .and(query_param("limit", "60"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": 12, "title": "Fate/stay night", "type": "game", "year": 2004, "posterUrl": "https://lain.bgm.tv/x.jpg" }],
            "total": 61, "limit": 60, "offset": 60
        })))
        .mount(&server)
        .await;
    mount(
        &server,
        "GET",
        "/api/v1/works/15",
        200,
        json!({
            "id": 15, "title": "Fate/Zero", "type": "anime", "year": 2011, "trackCount": 9,
            "posterUrl": "https://lain.bgm.tv/pic.jpg",
            "posterApiUrl": "/api/v1/works/15/poster?v=1",
            "bangumiSubjectId": "10380",
            "series": { "id": 7, "title": "Fate系列", "titleSource": "manual", "memberCount": 4, "position": 2, "source": "manual" },
            "seriesLocked": false
        }),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/v1/works/15/tracks",
        200,
        json!({ "items": [
            { "id": 1, "albumId": 5, "title": "oath sign", "role": "op", "sequence": 1, "source": "bangumi" }
        ]}),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/v1/works/15/albums",
        200,
        json!({ "items": [
            { "albumId": 5, "workId": 15, "title": "OST", "albumRole": "ost", "trackRoles": ["op"], "albumType": "soundtrack" }
        ]}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/series"))
        .and(query_param("limit", "50"))
        .and(query_param_is_missing("q"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": 7, "title": "Fate系列", "titleSource": "manual", "memberCount": 4,
                        "typeCounts": [{ "type": "anime", "count": 3 }], "representativeWorkId": 12, "posterUrl": "" }],
            "total": 1, "limit": 50, "offset": 0
        })))
        .mount(&server)
        .await;
    mount(
        &server,
        "GET",
        "/api/v1/series/7",
        200,
        json!({
            "series": { "id": 7, "title": "Fate系列" },
            "members": [{ "id": 12, "title": "Fate/stay night", "workType": "game", "year": 2004, "play": { "type": "work", "id": 12 } }],
            "stats": { "memberCount": 1, "yearFrom": 2004, "yearTo": 2004, "albumCount": 5, "trackCount": 26 },
            "play": { "type": "series", "id": 7 }
        }),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/v1/series/7/tracks",
        200,
        json!({ "items": [{ "id": 1, "title": "a", "role": "op" }, { "id": 2, "title": "b" }] }),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/v1/series/7/albums",
        200,
        json!({ "items": [{ "workId": 12, "workTitle": "Fate/stay night", "album": { "albumId": 301, "title": "OST" }, "play": { "type": "album", "id": 301 } }] }),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/v1/tracks/1/works",
        200,
        json!({ "items": [{ "id": 15, "title": "Fate/Zero", "type": "anime", "role": "op", "seriesId": 7 }] }),
    )
    .await;

    let c = client(&server).await;
    let query = WorkListQuery {
        search: " fate ".into(),
        kind: "game".into(),
        sort: WorkSort::Year,
    };
    let works = c.works(&query, 60, 60).await.unwrap();
    assert_eq!(works.total, 61);
    assert_eq!(works.items[0].kind, "game");
    assert_eq!(
        work_poster_path(12, &works.items[0].poster_url),
        "/api/v1/works/12/poster"
    );

    let work = c.work(15).await.unwrap();
    assert_eq!(work.work.title, "Fate/Zero");
    assert_eq!(work.series.as_ref().unwrap().position, 2);
    assert_eq!(
        work_poster_path(15, &work.poster_api_url),
        "/api/v1/works/15/poster?v=1"
    );
    assert_eq!(
        work_poster_path(15, "//evil.example/poster.jpg"),
        "/api/v1/works/15/poster"
    );
    assert_eq!(
        work_poster_path(15, r"/\evil.example/poster.jpg"),
        "/api/v1/works/15/poster"
    );
    let tracks = c.work_tracks(15).await.unwrap();
    assert_eq!(tracks[0].track.title, "oath sign");
    assert_eq!((tracks[0].role.as_str(), tracks[0].sequence), ("op", 1));
    assert_eq!(c.work_albums(15).await.unwrap()[0].track_roles, ["op"]);

    let list = c.series_list("", 0, 50).await.unwrap();
    assert_eq!(list.items[0].type_counts[0].count, 3);
    let detail = c.series(7).await.unwrap();
    assert_eq!(detail.members[0].work_type, "game");
    assert_eq!(detail.stats.track_count, 26);
    assert_eq!(c.series_tracks(7).await.unwrap().len(), 2);
    assert_eq!(c.series_albums(7).await.unwrap()[0].album.album_id, 301);
    assert_eq!(c.track_works(1).await.unwrap()[0].series_id, Some(7));
}

#[tokio::test]
async fn daily_mixes() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/v1/daily-mixes",
        202,
        json!({ "day": "2026-10-28", "status": "generating", "replacementsLeft": 3, "items": [] }),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/v1/daily-mixes/4",
        200,
        json!({
            "mix": { "id": 4, "title": "深夜", "status": "active", "day": "2026-10-28", "rating": 0,
                     "tags": [{ "kind": "mood", "tag": "calm", "label": "安静" }], "coverUrls": ["/api/v1/artwork/1"] },
            "tracks": [
                { "position": 0, "bucket": "explore", "track": { "id": 10 } },
                { "position": 1, "bucket": "familiar", "blocked": true, "track": { "id": 11, "missing": true } }
            ]
        }),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/daily-mixes/4/rating"))
        .and(body_json(json!({ "value": -1, "reasons": ["theme"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 4, "rating": -1, "ratingReasons": ["theme"]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/daily-mixes/4/rating"))
        .and(body_json(json!({ "value": 1, "reasons": [] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 4, "rating": 1 })))
        .mount(&server)
        .await;
    mount(
        &server,
        "POST",
        "/api/v1/daily-mixes/4/regenerate",
        429,
        json!({ "error": { "code": "replacement_limit", "message": "used up" } }),
    )
    .await;
    mount(
        &server,
        "POST",
        "/api/v1/daily-mixes/4/save",
        200,
        json!({ "playlist": { "id": 9, "name": "深夜" } }),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/tracks/10/recommendation-block"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tracks/10/recommendation-block"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server).await;
    let list = c.daily_mixes().await.unwrap();
    assert_eq!(list.status, "generating");
    let detail = c.daily_mix(4).await.unwrap();
    assert_eq!(detail.mix.tags[0].label, "安静");
    assert!(detail.tracks[1].blocked && detail.tracks[1].track.missing);
    let mix = c.rate_daily_mix(4, -1, &["theme".into()]).await.unwrap();
    assert_eq!(mix.rating_reasons, ["theme"]);
    // Reasons only go with a dislike.
    let mix = c.rate_daily_mix(4, 1, &["theme".into()]).await.unwrap();
    assert_eq!(mix.rating, 1);
    let err = c.regenerate_daily_mix(4).await.unwrap_err();
    assert_eq!(err.status(), Some(429));
    assert_eq!(c.save_daily_mix(4).await.unwrap().id, 9);
    c.set_recommendation_block(10, true).await.unwrap();
    c.set_recommendation_block(10, false).await.unwrap();
}

#[tokio::test]
async fn playback_history() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/playback/history"))
        .and(query_param("offset", "0"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "trackId": 3, "positionMillis": 1000, "durationMillis": 4000, "playCount": 2,
                        "state": "interrupted", "lastPlayedAt": "2026-10-28T01:00:00.000Z", "track": { "id": 3, "title": "x" } }],
            "total": 1, "limit": 100, "offset": 0
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/playback/history"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server).await;
    let page = c.playback_history(0, 100).await.unwrap();
    assert_eq!(page.items[0].state, "interrupted");
    assert_eq!(page.items[0].track.title, "x");
    c.clear_playback_history().await.unwrap();
}

#[tokio::test]
async fn lyrics_tools() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/lyrics/search"))
        .and(query_param("name", "春日影"))
        .and(query_param("artist", "MyGO"))
        .and(query_param("durationMs", "271000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "source": "netease", "id": "a b", "name": "春日影", "artist": "MyGO", "synced": true, "score": 0.1 }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/lyrics/remote/netease/a%20b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "synced": true, "lines": [{ "timeMs": 0, "text": "x" }]
        })))
        .mount(&server)
        .await;
    mount(
        &server,
        "GET",
        "/api/v1/lyrics/remote/lrclib/9",
        404,
        json!({ "error": { "code": "not_found" } }),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/tracks/5/lyrics/source"))
        .and(body_json(
            json!({ "source": "netease", "id": "a b", "name": "春日影", "artist": "MyGO" }),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "synced": true, "lines": [] })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tracks/5/lyrics/source"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tracks/5/lyrics/translation"))
        .and(body_json(json!({ "targetLanguage": "zh-Hans" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lines": ["一", ""], "model": "m", "cached": true, "source": "ai", "targetLanguage": "zh-Hans"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tracks/6/lyrics/translation"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": { "code": "translation_disabled", "message": "off" }
        })))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let results: Vec<LyricsSearchResult> = c
        .search_lyrics("春日影", "MyGO", Some(271_000))
        .await
        .unwrap();
    assert_eq!(results[0].synced, Some(true));
    let preview = c.remote_lyrics("netease", "a b").await.unwrap().unwrap();
    assert_eq!(preview.lines[0].text, "x");
    assert!(c.remote_lyrics("lrclib", "9").await.unwrap().is_none());
    c.set_lyrics_source(5, &results[0]).await.unwrap();
    c.clear_lyrics_source(5).await.unwrap();
    let t = c.translate_lyrics(5, "zh-Hans").await.unwrap();
    assert_eq!(t.lines, ["一", ""]);
    let err = c.translate_lyrics(6, "en").await.unwrap_err();
    assert_eq!(err.code(), Some("translation_disabled"));
}

#[tokio::test]
async fn album_works_and_missing_endpoint() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/v1/albums/5/works",
        200,
        json!({ "items": [
            { "id": 31, "title": "CLANNAD", "type": "anime", "role": "ost", "source": "album" },
            { "id": 32, "title": "AS", "type": "anime", "role": "op", "source": "manual", "trackId": 9, "trackTitle": "TORCH" }
        ]}),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/v1/albums/6/works",
        404,
        json!({ "error": { "code": "not_found", "message": "x" } }),
    )
    .await;
    let c = client(&server).await;
    let works = c.album_works(5).await.unwrap();
    assert_eq!(works.len(), 2);
    assert_eq!((works[0].kind.as_str(), works[0].track_id), ("anime", None));
    assert_eq!(works[1].track_id, Some(9));
    assert!(c.album_works(6).await.unwrap().is_empty());
}
