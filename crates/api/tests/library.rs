//! M2 endpoints: details, favorites, playlists, similar, transcode.

use api::{ArtistTrackSort, Client, FavoriteKind, Transcode};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "api-token";
const MEDIA: &str = "media-token";

fn track(id: i64, album_id: i64) -> Value {
    json!({ "id": id, "albumId": album_id, "title": format!("t{id}"), "durationMillis": 1000 })
}

fn page(items: Vec<Value>, offset: usize, total: usize) -> Value {
    json!({ "items": items, "limit": 500, "offset": offset, "total": total })
}

async fn client(server: &MockServer) -> Client {
    Client::new(&server.uri(), API, MEDIA).unwrap()
}

#[tokio::test]
async fn album_detail_completes_tracks_beyond_the_embedded_500() {
    let server = MockServer::start().await;
    let embedded: Vec<Value> = (0..500).map(|i| track(i, 3)).collect();
    Mock::given(method("GET"))
        .and(path("/api/v1/albums/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "album": { "id": 3, "title": "Big", "trackCount": 502 },
            "tracks": embedded,
        })))
        .mount(&server)
        .await;
    let first: Vec<Value> = (0..500).map(|i| track(i, 3)).collect();
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks"))
        .and(query_param("album", "3"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(first, 0, 502)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks"))
        .and(query_param("album", "3"))
        .and(query_param("offset", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![track(500, 3), track(501, 3)],
            500,
            502,
        )))
        .mount(&server)
        .await;

    let detail = client(&server).await.album(3).await.unwrap();
    assert_eq!(detail.album.title, "Big");
    assert_eq!(detail.tracks.len(), 502);
    assert_eq!(detail.tracks[501].id, 501);
}

#[tokio::test]
async fn artist_detail_and_tracks_decode() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/artists/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artist": { "id": 5, "name": "茶太", "imageUrl": "/api/v1/artists/5/image",
                        "isFavorite": true, "albumCount": 2, "trackCount": 30,
                        "biography": "bio", "aliases": [], "mergedFrom": 0 },
            "albums": [],
            "releases": [{ "id": 9, "title": "R", "relation": "own", "year": 2004 }],
            "topTracks": [track(1, 9)],
            "topTracksArePopular": true,
            "tracksTotal": 30,
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/artists/5/tracks"))
        .and(query_param("sort", "album"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![track(1, 9)], 0, 1)))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let detail = c.artist(5).await.unwrap();
    assert_eq!(detail.artist.name, "茶太");
    assert!(detail.artist.is_favorite);
    assert_eq!(detail.releases[0].relation, "own");
    assert_eq!(detail.releases[0].album.year, 2004);
    assert_eq!(detail.top_tracks.len(), 1);
    assert!(detail.top_tracks_are_popular);
    let tracks = c.artist_tracks(5, ArtistTrackSort::Album).await.unwrap();
    assert_eq!(tracks.len(), 1);
}

#[tokio::test]
async fn favorites_put_and_delete_accept_empty_answers() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/albums/3/favorite"))
        .and(header("authorization", format!("Bearer {API}").as_str()))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tracks/4/favorite"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/artists/6/favorite"))
        .respond_with(ResponseTemplate::new(404).set_body_json(
            json!({ "error": { "code": "not_found", "message": "no such artist" } }),
        ))
        .mount(&server)
        .await;
    let c = client(&server).await;
    c.set_favorite(FavoriteKind::Album, 3, true).await.unwrap();
    c.set_favorite(FavoriteKind::Track, 4, false).await.unwrap();
    let e = c
        .set_favorite(FavoriteKind::Artist, 6, true)
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(404));
}

#[tokio::test]
async fn playlist_item_operations_send_the_revision() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists/2/items/append"))
        .and(body_json(json!({ "trackIds": [1, 2] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "added": 1, "removed": 0, "skippedDuplicate": 1, "skippedInvalid": 0,
            "total": 5, "revision": 8,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists/2/items/append"))
        .and(body_json(json!({ "albumId": 9 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "added": 12 })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists/2/items/remove"))
        .and(body_json(json!({ "trackIds": [2], "expectedRevision": 8 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "removed": 1 })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/playlists/2/order"))
        .and(body_json(
            json!({ "trackIds": [3, 1], "expectedRevision": 9 }),
        ))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": { "code": "playlist_conflict", "message": "stale" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/playlists"))
        .and(body_json(json!({ "name": "夜", "description": "" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 11, "name": "夜" })))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/playlists/2"))
        .and(body_json(json!({ "name": "新", "description": "d" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 2 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/playlists/2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server).await;
    let r = c.append_to_playlist(2, &[1, 2]).await.unwrap();
    assert_eq!((r.added, r.skipped_duplicate, r.revision), (1, 1, 8));
    assert_eq!(c.append_album_to_playlist(2, 9).await.unwrap().added, 12);
    assert_eq!(c.remove_from_playlist(2, &[2], 8).await.unwrap().removed, 1);
    let e = c.order_playlist(2, &[3, 1], 9).await.unwrap_err();
    assert_eq!(e.code(), Some("playlist_conflict"));
    let created = c.create_playlist("夜", "", &[]).await.unwrap();
    assert_eq!(created.id, 11);
    c.update_playlist(2, "新", "d").await.unwrap();
    c.delete_playlist(2).await.unwrap();
}

#[tokio::test]
async fn playlist_artwork_is_uploaded_as_multipart_image() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/playlists/2/artwork"))
        .and(header("authorization", format!("Bearer {API}").as_str()))
        .and(body_string_contains("name=\"image\""))
        .and(body_string_contains("PNGDATA"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artworkUrl": "/api/v1/playlists/2/artwork?v=ab", "revision": 3, "hasCustomArtwork": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/playlists/2/artwork"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artworkUrl": "/api/v1/artwork/1", "revision": 4, "hasCustomArtwork": false
        })))
        .mount(&server)
        .await;
    let c = client(&server).await;
    let art = c
        .upload_playlist_artwork(2, b"PNGDATA".to_vec())
        .await
        .unwrap();
    assert!(art.has_custom_artwork);
    let reset = c.reset_playlist_artwork(2).await.unwrap();
    assert!(!reset.has_custom_artwork);
}

#[tokio::test]
async fn similar_tracks_leave_out_the_seed_and_clamp_the_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/7/similar"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "track": track(7, 1), "score": 1.0, "distance": 0.0, "reasons": [] },
            { "track": track(8, 1), "score": 0.8, "distance": 0.2, "reasons": ["artist"] },
        ]})))
        .mount(&server)
        .await;
    let tracks = client(&server).await.similar_tracks(7, 500).await.unwrap();
    assert_eq!(tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [8]);
}

#[tokio::test]
async fn similar_tracks_detailed_sends_diverse_exclude_and_falls_back_on_400() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/7/similar"))
        .and(query_param("limit", "24"))
        .and(query_param("diverse", "1"))
        .and(query_param("exclude", "10,20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "track": track(9, 2), "score": 0.91, "distance": 0.09, "reasons": [" soundsSimilar ", ""] },
        ]})))
        .mount(&server)
        .await;
    let items = client(&server)
        .await
        .similar_tracks_detailed(
            7,
            api::SimilarTracksOptions {
                limit: 24,
                exclude_ids: &[0, -5, 10, 10, 20],
                diverse: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].track.id, 9);
    assert_eq!(items[0].reasons, ["soundsSimilar"]);

    // Older server rejecting exclude/diverse with 400 falls back to ?limit=.
    let legacy = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/7/similar"))
        .and(query_param("diverse", "1"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(1)
        .mount(&legacy)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/7/similar"))
        .and(query_param("limit", "24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "track": track(11, 3), "score": 0.7, "distance": 0.3, "reasons": ["sameArtist"] },
        ]})))
        .mount(&legacy)
        .await;
    let fallback = client(&legacy)
        .await
        .similar_tracks_detailed(
            7,
            api::SimilarTracksOptions {
                limit: 24,
                exclude_ids: &[10],
                diverse: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(fallback.len(), 1);
    assert_eq!(fallback[0].track.id, 11);
}

#[tokio::test]
async fn similar_tracks_detailed_retries_once_on_503() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/7/similar"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tracks/7/similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "track": track(12, 4), "score": 0.85, "distance": 0.15, "reasons": ["sameWork"] },
        ]})))
        .mount(&server)
        .await;
    let items = client(&server)
        .await
        .similar_tracks_detailed(
            7,
            api::SimilarTracksOptions {
                limit: 12,
                exclude_ids: &[],
                diverse: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].track.id, 12);
}

#[test]
fn capabilities_parse_similarity_method_and_feature() {
    let caps: api::Capabilities = serde_json::from_value(json!({
        "apiVersion": "v1",
        "apiRevision": 3,
        "features": { "similarTracks": true },
        "similarity": { "method": "hybrid", "distanceRange": [0, 1] }
    }))
    .unwrap();
    assert!(caps.has(api::Feature::SimilarTracks));
    assert_eq!(caps.similarity_method(), Some("hybrid"));
}

#[test]
fn transcode_urls_pick_an_allowed_bitrate() {
    let c = Client::new("https://m.example.com", API, MEDIA).unwrap();
    let url = c.transcode_url(3, Transcode::Mp3(300), 0);
    assert_eq!(url.path(), "/api/v1/tracks/3/transcode.mp3");
    let q: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    assert!(q.contains(&("bitrate".into(), "256".into())));
    assert!(q.contains(&("mediaToken".into(), MEDIA.into())));
    let url = c.transcode_url(3, Transcode::Ogg(10), 1500);
    assert!(url.query().unwrap().contains("bitrate=64"));
    assert!(url.query().unwrap().contains("offsetMs=1500"));
    let url = c.transcode_url(3, Transcode::Flac48, 1500);
    assert!(url.query().unwrap().contains("maxSampleRate=48000"));
    assert!(!url.query().unwrap().contains("offsetMs"));
}
