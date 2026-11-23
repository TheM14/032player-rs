use api::{
    AlbumListQuery, AlbumSort, AuthError, Client, Feature, SortOrder, TrackListQuery, TrackSort,
    authenticate,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "api-token";
const MEDIA: &str = "media-token";

fn capabilities(revision: i64) -> Value {
    json!({
        "apiVersion": "v1",
        "apiRevision": revision,
        "features": { "browse": true, "lyrics": true, "unknownFlag": true },
        "playback": { "clientKinds": ["web", "android", "desktop"], "contextTypes": ["dailyMix"] },
        "limits": { "maxPageSize": 500, "maxPlaylistTracks": 5000 },
        "extra": { "ignored": true },
    })
}

fn track_json(id: i64) -> Value {
    json!({
        "id": id, "albumId": 3, "title": "曲", "album": "アルバム", "artist": "A",
        "durationMillis": 240000, "streamUrl": format!("/api/v1/tracks/{id}/stream"),
        "artworkUrl": "/api/v1/artwork/9", "codec": "flac", "sampleRate": 48000, "bitDepth": 24,
        "artists": [{ "id": 1, "name": "A", "imageUrl": "", "albumCount": 2 }],
    })
}

async fn mock_caps(server: &MockServer, revision: i64) {
    Mock::given(method("GET"))
        .and(path("/api/v1/capabilities"))
        .and(header("authorization", format!("Bearer {API}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(capabilities(revision)))
        .mount(server)
        .await;
}

async fn mock_first_track(server: &MockServer, items: Value) {
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": items, "limit": 1, "offset": 0, "total": 1
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn authenticate_validates_revision_and_probes_the_media_token() {
    let server = MockServer::start().await;
    mock_caps(&server, 3).await;
    mock_first_track(&server, json!([track_json(7)])).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/7/stream"))
        .and(query_param("mediaToken", MEDIA))
        .and(header("range", "bytes=0-0"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(vec![0u8]))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/", server.uri());
    let (client, caps) = authenticate(&url, &format!(" {API} "), MEDIA)
        .await
        .unwrap();
    assert_eq!(client.base_url(), server.uri());
    assert_eq!(client.api_token(), API);
    assert!(caps.has(Feature::Browse));
    assert!(!caps.has(Feature::DailyMixes));
    assert_eq!(caps.client_kind(), "desktop");
    assert_eq!(caps.page_size(), 500);
}

#[tokio::test]
async fn authenticate_rejects_bad_input_before_any_request() {
    let r = authenticate("http://127.0.0.1:9", "", MEDIA).await;
    assert!(matches!(r, Err(AuthError::CredentialsRequired)));
    let r = authenticate("http://127.0.0.1:9", "same", "same").await;
    assert!(matches!(r, Err(AuthError::TokensMustDiffer)));
    let r = authenticate("not a url", API, MEDIA).await;
    assert!(matches!(r, Err(AuthError::Api(api::ApiError::Url(_)))));
}

#[tokio::test]
async fn authenticate_maps_401_to_invalid_api_token() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "code": "unauthorized", "message": "bad token" }
        })))
        .mount(&server)
        .await;
    let r = authenticate(&server.uri(), API, MEDIA).await;
    assert!(matches!(r, Err(AuthError::ApiTokenInvalid)), "{r:?}");
}

#[tokio::test]
async fn authenticate_rejects_other_revisions() {
    let server = MockServer::start().await;
    mock_caps(&server, 2).await;
    let r = authenticate(&server.uri(), API, MEDIA).await;
    assert!(matches!(r, Err(AuthError::UnsupportedRevision(2))), "{r:?}");
}

#[tokio::test]
async fn authenticate_detects_an_invalid_media_token() {
    let server = MockServer::start().await;
    mock_caps(&server, 3).await;
    mock_first_track(&server, json!([track_json(7)])).await;
    Mock::given(path("/api/v1/tracks/7/stream"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let r = authenticate(&server.uri(), API, MEDIA).await;
    assert!(matches!(r, Err(AuthError::MediaTokenInvalid)), "{r:?}");
}

#[tokio::test]
async fn authenticate_skips_the_media_probe_on_an_empty_library() {
    let server = MockServer::start().await;
    mock_caps(&server, 3).await;
    mock_first_track(&server, json!([])).await;
    authenticate(&server.uri(), API, MEDIA).await.unwrap();
}

#[tokio::test]
async fn error_bodies_are_parsed_and_fall_back_to_a_generic_message() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "code": "invalid_filter", "message": "sort=nope" }
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>"))
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), API, MEDIA).unwrap();

    let e = client
        .albums(true, &AlbumListQuery::default(), 0, 10)
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(400));
    assert_eq!(e.code(), Some("invalid_filter"));
    assert_eq!(e.to_string(), "sort=nope");

    let e = client
        .tracks(true, &TrackListQuery::default(), 0, 10)
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(502));
    assert_eq!(e.code(), None);
    assert_eq!(e.to_string(), "032 Music Server request failed (502)");
    assert!(!e.is_unauthorized());
}

#[tokio::test]
async fn browse_albums_sends_order_year_range_and_seed() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/browse/albums"))
        .and(query_param("sort", "random"))
        .and(query_param("order", "desc"))
        .and(query_param("seed", "42"))
        .and(query_param("yearFrom", "1990"))
        .and(query_param("yearTo", "1999"))
        .and(query_param("limit", "500"))
        .and(query_param("offset", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": 3, "title": "T", "artists": [{ "id": 1, "name": "A" }] }],
            "limit": 500, "offset": 1000, "total": 1001
        })))
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), API, MEDIA).unwrap();
    let query = AlbumListQuery {
        sort: Some(AlbumSort::Random),
        order: Some(SortOrder::Desc),
        seed: Some(42),
        year_from: Some(1990),
        year_to: Some(1999),
        ..AlbumListQuery::default()
    };
    // limit is clamped to the server maximum.
    let page = client.albums(true, &query, 1000, 10_000).await.unwrap();
    assert_eq!(page.total, 1001);
    assert_eq!(page.items[0].artists[0].name, "A");
    // Missing fields decode to defaults.
    assert_eq!(page.items[0].track_count, 0);
}

#[test]
fn legacy_album_list_drops_what_it_does_not_understand() {
    let query = AlbumListQuery {
        sort: Some(AlbumSort::Random),
        order: Some(SortOrder::Desc),
        seed: Some(42),
        year_from: Some(1990),
        year_to: Some(1999),
        ..AlbumListQuery::default()
    };
    let (path, q) = query.source(false);
    assert_eq!(path, "/albums");
    assert!(q.is_empty(), "{q:?}");

    let exact_year = AlbumListQuery {
        sort: Some(AlbumSort::Added),
        year_from: Some(2001),
        year_to: Some(2001),
        ..AlbumListQuery::default()
    };
    let (_, q) = exact_year.source(false);
    assert_eq!(q.get("sort"), Some("added"));
    assert_eq!(q.get("year"), Some("2001"));
}

#[test]
fn track_list_order_is_only_sent_with_a_sort() {
    let only_order = TrackListQuery {
        order: Some(SortOrder::Asc),
        ..TrackListQuery::default()
    };
    assert_eq!(only_order.source(true).1.get("order"), None);

    let sorted = TrackListQuery {
        sort: Some(TrackSort::Duration),
        order: Some(SortOrder::Asc),
        seed: Some(1),
        ..TrackListQuery::default()
    };
    let (path, q) = sorted.source(true);
    assert_eq!(path, "/browse/tracks");
    assert_eq!(q.get("sort"), Some("duration"));
    assert_eq!(q.get("order"), Some("asc"));
    assert_eq!(q.get("seed"), None, "seed only rides on sort=random");

    let (path, q) = sorted.source(false);
    assert_eq!(path, "/tracks");
    assert_eq!(q.get("sort"), None);
}

#[tokio::test]
async fn legacy_tracks_endpoint_is_used_without_browse() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/tracks"))
        .and(query_param("sort", "plays"))
        .and(query_param_is_missing("order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [track_json(1)], "limit": 50, "offset": 0, "total": 1
        })))
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), API, MEDIA).unwrap();
    let query = TrackListQuery {
        sort: Some(TrackSort::Plays),
        order: Some(SortOrder::Desc),
        ..TrackListQuery::default()
    };
    let page = client.tracks(false, &query, 0, 50).await.unwrap();
    let t = &page.items[0];
    assert_eq!(t.title, "曲");
    assert_eq!(t.sample_rate, Some(48000));
    assert_eq!(t.bit_depth, Some(24));
    assert_eq!(t.channels, None);
}

#[tokio::test]
async fn lyrics_404_is_none() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/tracks/1/lyrics"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "code": "not_found", "message": "no lyrics" }
        })))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/tracks/2/lyrics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lines": [{ "text": "一行目", "timeMs": 1200 }], "synced": true, "sourceName": "LRCLIB"
        })))
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), API, MEDIA).unwrap();
    assert_eq!(client.lyrics(1).await.unwrap(), None);
    let lyrics = client.lyrics(2).await.unwrap().unwrap();
    assert!(lyrics.synced);
    assert_eq!(lyrics.lines[0].time_ms, 1200);
    assert_eq!(lyrics.source_name.as_deref(), Some("LRCLIB"));
}

#[test]
fn media_urls_carry_the_media_token_never_the_api_token() {
    let client = Client::new("https://music.example.com/", API, "m t&").unwrap();
    let stream = client.stream_url(5);
    assert_eq!(
        stream.as_str(),
        "https://music.example.com/api/v1/tracks/5/stream?mediaToken=m+t%26"
    );
    assert!(!stream.as_str().contains(API));

    let art = client
        .image_url("/api/v1/artwork/9?v=2", Some(512))
        .unwrap();
    assert_eq!(
        art.as_str(),
        "https://music.example.com/api/v1/artwork/9?v=2&mediaToken=m+t%26&size=512"
    );
    // size 0 means "original": the server rejects size=0.
    let original = client.image_url("/api/v1/artwork/9", Some(0)).unwrap();
    assert!(!original.as_str().contains("size="));
    assert!(client.image_url("", Some(256)).is_none());

    // An existing mediaToken is replaced, not duplicated.
    let replaced = client
        .media_url("/api/v1/artwork/9?mediaToken=old", &[])
        .unwrap();
    assert_eq!(
        replaced.as_str(),
        "https://music.example.com/api/v1/artwork/9?mediaToken=m+t%26"
    );
    assert_eq!(
        client.artist_image_url(4, None).as_str(),
        "https://music.example.com/api/v1/artists/4/image?mediaToken=m+t%26"
    );

    // Cross-origin and protocol-relative URLs never receive the media token,
    // and non-HTTP(S) schemes are rejected.
    let cross = client
        .media_url(
            "https://evil.example/poster.jpg?mediaToken=old&x=1",
            &[("size", "512".into())],
        )
        .unwrap();
    assert_eq!(cross.as_str(), "https://evil.example/poster.jpg?x=1");
    let proto_rel = client.media_url("//evil.example/poster.jpg", &[]).unwrap();
    assert_eq!(proto_rel.as_str(), "https://evil.example/poster.jpg");
    let downgraded = client
        .media_url("http://music.example.com/api/v1/artwork/9", &[])
        .unwrap();
    assert!(!downgraded.as_str().contains("mediaToken"));
    assert!(client.media_url("file:///etc/passwd", &[]).is_none());

    // Client Debug output redacts both tokens.
    let dbg = format!("{client:?}");
    assert!(!dbg.contains(API));
    assert!(!dbg.contains("m t&"));
    assert!(dbg.contains("<redacted>"));

    // Log redaction strips mediaToken and Bearer secrets while preserving UTF-8.
    assert_eq!(
        api::redact_media_token(
            "https://music.example.com/api/v1/tracks/5/stream?mediaToken=m+t%26&bitrate=320"
        ),
        "https://music.example.com/api/v1/tracks/5/stream?mediaToken=<redacted>&bitrate=320"
    );
    assert_eq!(
        api::redact_media_token("Authorization: Bearer secret-api 曲名"),
        "Authorization: Bearer <redacted> 曲名"
    );
}

#[tokio::test]
async fn network_errors_do_not_leak_media_token() {
    let client = Client::new("http://127.0.0.1:1", API, "super-secret-media-token").unwrap();
    let url = client.stream_url(5);
    let err = client.fetch_media(url).await.unwrap_err();
    let display = err.to_string();
    let debug = format!("{err:?}");
    assert!(!display.contains("super-secret-media-token"), "{display}");
    assert!(!debug.contains("super-secret-media-token"), "{debug}");
}

#[tokio::test]
async fn latest_release_reads_tag_and_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/lux032/032player-rs/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/lux032/032player-rs/releases/tag/v9.9.9",
            "draft": false,
            "prerelease": false,
        })))
        .mount(&server)
        .await;
    let release = api::latest_release(&server.uri(), api::RELEASE_REPO)
        .await
        .unwrap();
    assert_eq!(release.tag, "v9.9.9");
    assert!(release.url.ends_with("/v9.9.9"));
    assert!(api::is_newer(&release.tag, "0.1.6"));

    let missing = api::latest_release(&server.uri(), "nobody/none").await;
    assert_eq!(missing.unwrap_err().status(), Some(404));
}

#[tokio::test]
async fn requests_and_decodes_gzip_json() {
    // gzip of {"items":[],"limit":500,"offset":0,"total":7}
    const BODY: [u8; 62] = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xab, 0x56, 0xca, 0x2c, 0x49,
        0xcd, 0x2d, 0x56, 0xb2, 0x8a, 0x8e, 0xd5, 0x51, 0xca, 0xc9, 0xcc, 0xcd, 0x2c, 0x51, 0xb2,
        0x32, 0x35, 0x30, 0xd0, 0x51, 0xca, 0x4f, 0x4b, 0x2b, 0x4e, 0x05, 0x72, 0x80, 0xcc, 0x92,
        0xfc, 0x92, 0xc4, 0x1c, 0x25, 0x2b, 0xf3, 0x5a, 0x00, 0xf5, 0xea, 0xb7, 0x0f, 0x2d, 0x00,
        0x00, 0x00,
    ];
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks"))
        .and(header("accept-encoding", "gzip"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-encoding", "gzip")
                .insert_header("content-type", "application/json")
                .set_body_bytes(BODY.to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri(), API, MEDIA).unwrap();
    let page: api::Page<Value> = client.get("/tracks", &api::Query::new()).await.unwrap();
    assert_eq!(page.total, 7);
}

#[tokio::test]
async fn unified_search_overview_and_category_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("q", "星街彗星"))
        .and(query_param_is_missing("category"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "query": "星街彗星",
            "category": "",
            "bestMatch": {
                "type": "artists",
                "id": 42,
                "title": "星街すいせい",
                "subtitle": "",
                "imageUrl": "/api/v1/artists/42/image"
            },
            "artists": { "items": [{ "id": 42, "name": "星街すいせい", "albumCount": 3, "trackCount": 20 }], "total": 1, "limit": 5, "offset": 0 },
            "albums": { "items": [], "total": 0, "limit": 6, "offset": 0 },
            "tracks": { "items": [track_json(7)], "total": 12, "limit": 10, "offset": 0 },
            "composers": { "items": [], "total": 0, "limit": 5, "offset": 0 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("q", "星街彗星"))
        .and(query_param("category", "tracks"))
        .and(query_param("offset", "10"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "query": "星街彗星",
            "category": "tracks",
            "bestMatch": null,
            "artists": { "items": [], "total": 0, "limit": 5, "offset": 0 },
            "albums": { "items": [], "total": 0, "limit": 6, "offset": 0 },
            "tracks": { "items": [track_json(8)], "total": 12, "limit": 100, "offset": 10 },
            "composers": { "items": [], "total": 0, "limit": 5, "offset": 0 }
        })))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri(), API, MEDIA).unwrap();
    let overview = client.search("  星街彗星  ", None, 0, 0).await.unwrap();
    assert_eq!(overview.best_match.as_ref().unwrap().id, 42);
    assert_eq!(overview.artists.total, 1);
    assert_eq!(overview.tracks.total, 12);

    let page = client
        .search("星街彗星", Some(api::SearchCategory::Tracks), 10, 100)
        .await
        .unwrap();
    assert_eq!(page.category, "tracks");
    assert_eq!(page.tracks.items[0].id, 8);
    assert_eq!(page.tracks.total, 12);
}

#[test]
fn normalize_base_url_strips_query_and_fragment_and_media_url_preserves_subpath() {
    assert_eq!(
        api::normalize_base_url("  https://host.example/music/?tab=home#albums ").unwrap(),
        "https://host.example/music"
    );
    assert_eq!(
        api::normalize_base_url("https://host.example/music#top").unwrap(),
        "https://host.example/music"
    );

    let client = Client::new("https://host.example/music/?x=1#y", API, "tok").unwrap();
    assert_eq!(client.base_url(), "https://host.example/music");
    assert_eq!(
        client.stream_url(5).as_str(),
        "https://host.example/music/api/v1/tracks/5/stream?mediaToken=tok"
    );
    assert_eq!(
        client
            .image_url("/api/v1/artwork/9?v=2", Some(512))
            .unwrap()
            .as_str(),
        "https://host.example/music/api/v1/artwork/9?v=2&mediaToken=tok&size=512"
    );
    // Already-prefixed subpath is not duplicated.
    assert_eq!(
        client
            .image_url("/music/api/v1/artwork/9", Some(256))
            .unwrap()
            .as_str(),
        "https://host.example/music/api/v1/artwork/9?mediaToken=tok&size=256"
    );
    // Protocol-relative and backslash-prefixed paths remain cross-origin without token.
    assert_eq!(
        client
            .media_url("//evil.example/poster.jpg", &[])
            .unwrap()
            .as_str(),
        "https://evil.example/poster.jpg"
    );
    assert_eq!(
        client
            .media_url("/\\evil.example/poster.jpg", &[])
            .unwrap()
            .as_str(),
        "https://evil.example/poster.jpg"
    );
    // Root-relative path looking like a scheme stays same-origin under subpath.
    assert_eq!(
        client
            .media_url("/https://evil.example/a", &[])
            .unwrap()
            .as_str(),
        "https://host.example/music/https://evil.example/a?mediaToken=tok"
    );
}

#[tokio::test]
async fn warm_media_does_not_buffer_full_200_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut req = [0u8; 1024];
        let _ = socket.read(&mut req).await;
        // Advertise a 10 MiB 200 OK body, send only the first 16 bytes, and hold
        // the connection open. If warm_media tried to buffer the full body this
        // test would time out.
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10485760\r\n\r\n0123456789abcdef")
            .await;
        let _ = socket.flush().await;
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    });

    let client = Client::new(&format!("http://{addr}"), API, MEDIA).unwrap();
    let url = client.stream_url(1);
    let (status, retry) =
        tokio::time::timeout(std::time::Duration::from_secs(2), client.warm_media(url))
            .await
            .expect("warm_media must not wait for full 200 OK body")
            .unwrap();
    assert_eq!(status, 200);
    assert_eq!(retry, None);
    server.abort();
}

#[tokio::test]
async fn post_playback_event_propagates_truncated_body_as_network_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut req = [0u8; 2048];
        let _ = socket.read(&mut req).await;
        // Truncated 409 response: declares 120 bytes but closes after 10 bytes.
        let _ = socket
            .write_all(b"HTTP/1.1 409 Conflict\r\nContent-Length: 120\r\n\r\n{\"error\":{")
            .await;
        let _ = socket.shutdown().await;
    });

    let client = Client::new(&format!("http://{addr}"), API, MEDIA).unwrap();
    let err = client
        .post_playback_event(&json!({ "type": "heartbeat" }), false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, api::ApiError::Network(_)),
        "expected network error on truncated body, got {err:?}"
    );
}
