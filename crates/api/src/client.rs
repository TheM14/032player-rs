use std::time::Duration;

use reqwest::{Method, StatusCode, header};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{ApiError, AuthError};
use crate::query::{AlbumListQuery, ArtistListQuery, Query, TrackListQuery};
use crate::types::{
    Album, AlbumDetail, Artist, ArtistCreditsPage, ArtistDetail, Capabilities, CreditRoleCount,
    ErrorBody, Items, Lyrics, Page, Playlist, PlaylistArtwork, PlaylistDetail, PlaylistItemResult,
    SimilarTrack, Track, TrackVersionsResponse,
};

/// What can be marked as favorite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FavoriteKind {
    Album,
    Track,
    Artist,
}

impl FavoriteKind {
    fn segment(self) -> &'static str {
        match self {
            Self::Album => "albums",
            Self::Track => "tracks",
            Self::Artist => "artists",
        }
    }
}

/// Sort of `/artists/{id}/tracks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtistTrackSort {
    /// Year, album, disc, track.
    Album,
    Plays,
}

/// Lossy transcode targets (`/tracks/{id}/transcode.{format}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Transcode {
    /// MP3 at this bitrate (128, 192, 256, 320).
    Mp3(u32),
    /// Opus in Ogg at this bitrate (64 .. 256).
    Ogg(u32),
    /// FLAC limited to the 44.1 / 48 kHz family.
    Flac48,
}

impl Transcode {
    const MP3: [u32; 4] = [128, 192, 256, 320];
    const OGG: [u32; 6] = [64, 96, 128, 160, 192, 256];

    pub fn format(self) -> &'static str {
        match self {
            Self::Mp3(_) => "mp3",
            Self::Ogg(_) => "ogg",
            Self::Flac48 => "flac",
        }
    }

    /// The highest allowed bitrate not above the requested one (like
    /// `transcodeUrl`), or the lowest.
    fn bitrate(self) -> Option<u32> {
        let (allowed, want): (&[u32], u32) = match self {
            Self::Mp3(b) => (&Self::MP3, b),
            Self::Ogg(b) => (&Self::OGG, b),
            Self::Flac48 => return None,
        };
        Some(
            allowed
                .iter()
                .rev()
                .find(|b| **b <= want)
                .copied()
                .unwrap_or(allowed[0]),
        )
    }
}

/// Largest page the 032 list endpoints accept (`capabilities.limits.maxPageSize`).
pub const MAX_PAGE_SIZE: u32 = 500;

/// apiRevision this client implements (playback session protocol).
pub const API_REVISION: i64 = 3;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const PLAYBACK_EVENT_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const WARM_MEDIA_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const TRANSLATE_LYRICS_TIMEOUT: Duration = Duration::from_secs(120);

/// HTTP client for one 032 Music Server.
///
/// The API token goes in the `Authorization` header only; media URLs
/// (stream, artwork) carry the separate media token as a query parameter.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_token: String,
    media_token: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("api_token", &"<redacted>")
            .field("media_token", &"<redacted>")
            .finish()
    }
}

/// Redacts `mediaToken=...` query values and `Bearer ...` credentials from text
/// before logging or displaying diagnostics.
#[must_use]
pub fn redact_media_token(input: &str) -> String {
    fn is_delim(b: u8) -> bool {
        matches!(
            b,
            b'&' | b'#'
                | b'\''
                | b'"'
                | b')'
                | b']'
                | b'>'
                | b','
                | b';'
                | b' '
                | b'\t'
                | b'\r'
                | b'\n'
        )
    }

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &input[i..];
        let prev = i.checked_sub(1).map(|p| bytes[p]);
        if rest.len() >= 11
            && rest.as_bytes()[..11].eq_ignore_ascii_case(b"mediatoken=")
            && prev.is_none_or(|b| matches!(b, b'?' | b'&' | b' ' | b'\t' | b'\'' | b'"' | b'('))
        {
            out.push_str(&rest[..11]);
            out.push_str("<redacted>");
            i += 11;
            while i < bytes.len() && !is_delim(bytes[i]) {
                i += 1;
            }
        } else if rest.len() >= 7
            && rest.as_bytes()[..7].eq_ignore_ascii_case(b"bearer ")
            && prev.is_none_or(|b| matches!(b, b':' | b' ' | b'\t' | b'\'' | b'"'))
        {
            out.push_str(&rest[..7]);
            out.push_str("<redacted>");
            i += 7;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            while i < bytes.len() && !is_delim(bytes[i]) {
                i += 1;
            }
        } else if let Some(ch) = rest.chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

/// Turns a user-entered server URL into the base URL (no query, fragment, or trailing slash).
pub fn normalize_base_url(url: &str) -> Result<String, ApiError> {
    let trimmed = url.trim();
    let mut parsed = Url::parse(trimmed).map_err(|e| ApiError::Url(e.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err(ApiError::Url(format!("not an http(s) URL: {trimmed}")));
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

fn remove_param(url: &mut Url, key: &str) {
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !k.eq_ignore_ascii_case(key))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
}

fn set_param(url: &mut Url, key: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut found = false;
    pairs.retain_mut(|(k, v)| {
        if k != key {
            return true;
        }
        if found {
            return false;
        }
        found = true;
        value.clone_into(v);
        true
    });
    if !found {
        pairs.push((key.to_owned(), value.to_owned()));
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
}

impl Client {
    pub fn new(
        base_url: &str,
        api_token: impl Into<String>,
        media_token: impl Into<String>,
    ) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("032player-rs/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()?;
        Self::with_http(http, base_url, api_token, media_token)
    }

    pub fn with_http(
        http: reqwest::Client,
        base_url: &str,
        api_token: impl Into<String>,
        media_token: impl Into<String>,
    ) -> Result<Self, ApiError> {
        Ok(Self {
            http,
            base_url: normalize_base_url(base_url)?,
            api_token: api_token.into(),
            media_token: media_token.into(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_token(&self) -> &str {
        &self.api_token
    }

    pub fn media_token(&self) -> &str {
        &self.media_token
    }

    // ------------------------------------------------------------ requests

    fn api_url(&self, path: &str, query: &Query) -> Result<Url, ApiError> {
        let mut url = Url::parse(&format!("{}/api/v1{path}", self.base_url))
            .map_err(|e| ApiError::Url(e.to_string()))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.pairs());
        }
        Ok(url)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &Query,
        body: Option<&(dyn erased::Body + Sync)>,
    ) -> Result<reqwest::Response, ApiError> {
        self.send_with_timeout(method, path, query, body, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    async fn send_with_timeout(
        &self,
        method: Method,
        path: &str,
        query: &Query,
        body: Option<&(dyn erased::Body + Sync)>,
        timeout: Duration,
    ) -> Result<reqwest::Response, ApiError> {
        let url = self.api_url(path, query)?;
        let mut req = self
            .http
            .request(method, url)
            .timeout(timeout)
            .header(header::ACCEPT, "application/json")
            .bearer_auth(&self.api_token);
        if let Some(body) = body {
            req = req
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.to_json()?);
        }
        let res = req.send().await?;
        if res.status().is_success() {
            return Ok(res);
        }
        Err(error_from_response(res).await)
    }

    /// `GET /api/v1{path}` decoding a JSON body.
    pub async fn get<T: DeserializeOwned>(&self, path: &str, query: &Query) -> Result<T, ApiError> {
        let res = self.send(Method::GET, path, query, None).await?;
        decode(res).await
    }

    /// Request whose answer body is not needed (204, favorites).
    pub async fn send_empty<B: Serialize + Sync>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<(), ApiError> {
        let json = body.map(erased::Json);
        self.send(
            method,
            path,
            &Query::new(),
            json.as_ref().map(|j| j as &(dyn erased::Body + Sync)),
        )
        .await?;
        Ok(())
    }

    /// Every page of a list endpoint.
    pub async fn get_all<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &Query,
    ) -> Result<Vec<T>, ApiError> {
        let mut out = Vec::new();
        loop {
            let mut q = query.clone();
            q.page(u32::try_from(out.len()).unwrap_or(u32::MAX), MAX_PAGE_SIZE);
            let page: Page<T> = self.get(path, &q).await?;
            let n = page.items.len();
            out.extend(page.items);
            if n == 0 || i64::try_from(out.len()).unwrap_or(i64::MAX) >= page.total {
                return Ok(out);
            }
        }
    }

    /// Request with a JSON body, decoding a JSON response.
    pub async fn send_json<B: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        self.send_json_with_timeout(method, path, body, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    pub(crate) async fn send_json_with_timeout<B: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        timeout: Duration,
    ) -> Result<T, ApiError> {
        let res = self
            .send_with_timeout(
                method,
                path,
                &Query::new(),
                Some(&erased::Json(body)),
                timeout,
            )
            .await?;
        decode(res).await
    }

    // ------------------------------------------------------------ endpoints

    pub async fn capabilities(&self) -> Result<Capabilities, ApiError> {
        self.get("/capabilities", &Query::new()).await
    }

    /// Albums via `/browse/albums`, or legacy `/albums` when `browse` is false.
    pub async fn albums(
        &self,
        browse: bool,
        query: &AlbumListQuery,
        offset: u32,
        limit: u32,
    ) -> Result<Page<Album>, ApiError> {
        let (path, mut q) = query.source(browse);
        q.page(offset, limit);
        self.get(path, &q).await
    }

    /// Tracks via `/browse/tracks`, or legacy `/tracks` when `browse` is false.
    pub async fn tracks(
        &self,
        browse: bool,
        query: &TrackListQuery,
        offset: u32,
        limit: u32,
    ) -> Result<Page<Track>, ApiError> {
        let (path, mut q) = query.source(browse);
        q.page(offset, limit);
        self.get(path, &q).await
    }

    /// Artists via `/browse/artists`, or legacy `/artists` when `browse` is false.
    pub async fn artists(
        &self,
        browse: bool,
        query: &ArtistListQuery,
        offset: u32,
        limit: u32,
    ) -> Result<Page<Artist>, ApiError> {
        let (path, mut q) = query.source(browse);
        q.page(offset, limit);
        self.get(path, &q).await
    }

    /// Album with its tracks; albums with more tracks than embedded (500)
    /// are completed through `/tracks?album=`.
    pub async fn album(&self, album_id: i64) -> Result<AlbumDetail, ApiError> {
        let mut detail: AlbumDetail = self
            .get(&format!("/albums/{album_id}"), &Query::new())
            .await?;
        if detail.album.track_count > i64::try_from(detail.tracks.len()).unwrap_or(i64::MAX) {
            let mut q = Query::new();
            q.push("album", album_id);
            detail.tracks = self.get_all("/tracks", &q).await?;
        }
        Ok(detail)
    }

    pub async fn artist(&self, artist_id: i64) -> Result<ArtistDetail, ApiError> {
        self.get(&format!("/artists/{artist_id}"), &Query::new())
            .await
    }

    /// Unified local-library search (`GET /api/v1/search`).
    pub async fn search(
        &self,
        query: &str,
        category: Option<crate::SearchCategory>,
        offset: u32,
        limit: u32,
    ) -> Result<crate::SearchResults, ApiError> {
        let trimmed: String = query.trim().chars().take(200).collect();
        let mut q = Query::new();
        if !trimmed.is_empty() {
            q.push("q", &trimmed);
        }
        if let Some(cat) = category {
            q.push("category", cat.as_str());
            q.page(offset, limit.max(1));
        }
        self.get("/search", &q).await
    }

    /// Every track of an artist (performed or album artist).
    pub async fn artist_tracks(
        &self,
        artist_id: i64,
        sort: ArtistTrackSort,
    ) -> Result<Vec<Track>, ApiError> {
        let mut q = Query::new();
        q.push(
            "sort",
            match sort {
                ArtistTrackSort::Album => "album",
                ArtistTrackSort::Plays => "plays",
            },
        );
        self.get_all(&format!("/artists/{artist_id}/tracks"), &q)
            .await
    }

    /// Backstage credited tracks of an artist (`GET /artists/{id}/credits`),
    /// optionally filtered by `role` (`lyricist` / `composer` / `arranger` /
    /// `producer`), together with per-role track counts.
    pub async fn artist_credits(
        &self,
        artist_id: i64,
        role: Option<&str>,
    ) -> Result<(Vec<Track>, Vec<CreditRoleCount>), ApiError> {
        let mut out = Vec::new();
        let mut roles = Vec::new();
        let path = format!("/artists/{artist_id}/credits");
        loop {
            let mut q = Query::new();
            if let Some(role) = role.filter(|r| !r.is_empty()) {
                q.push("role", role);
            }
            q.page(u32::try_from(out.len()).unwrap_or(u32::MAX), MAX_PAGE_SIZE);
            let page: ArtistCreditsPage = self.get(&path, &q).await?;
            if roles.is_empty() && !page.roles.is_empty() {
                roles = page.roles;
            }
            let n = page.items.len();
            out.extend(page.items);
            if n == 0 || i64::try_from(out.len()).unwrap_or(i64::MAX) >= page.total {
                return Ok((out, roles));
            }
        }
    }

    pub async fn set_favorite(
        &self,
        kind: FavoriteKind,
        id: i64,
        favorite: bool,
    ) -> Result<(), ApiError> {
        let method = if favorite {
            Method::PUT
        } else {
            Method::DELETE
        };
        self.send_empty::<()>(method, &format!("/{}/{id}/favorite", kind.segment()), None)
            .await
    }

    /// All favorite albums (`/albums` ignores `favorite=true`).
    pub async fn favorite_albums(&self) -> Result<Vec<Album>, ApiError> {
        self.get_all("/favorites/albums", &Query::new()).await
    }

    pub async fn favorite_tracks(&self) -> Result<Vec<Track>, ApiError> {
        self.get_all("/favorites/tracks", &Query::new()).await
    }

    pub async fn favorite_artists(&self) -> Result<Vec<Artist>, ApiError> {
        self.get_all("/favorites/artists", &Query::new()).await
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>, ApiError> {
        self.get_all("/playlists", &Query::new()).await
    }

    pub async fn playlist(&self, playlist_id: i64) -> Result<PlaylistDetail, ApiError> {
        self.get(&format!("/playlists/{playlist_id}"), &Query::new())
            .await
    }

    pub async fn create_playlist(
        &self,
        name: &str,
        description: &str,
        track_ids: &[i64],
    ) -> Result<Playlist, ApiError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            name: &'a str,
            description: &'a str,
            #[serde(skip_serializing_if = "<[i64]>::is_empty")]
            track_ids: &'a [i64],
        }
        let body = Body {
            name,
            description,
            track_ids,
        };
        self.send_json(Method::POST, "/playlists", &body).await
    }

    /// Rename / change the description.
    pub async fn update_playlist(
        &self,
        playlist_id: i64,
        name: &str,
        description: &str,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({ "name": name, "description": description });
        self.send_empty(
            Method::PATCH,
            &format!("/playlists/{playlist_id}"),
            Some(&body),
        )
        .await
    }

    pub async fn delete_playlist(&self, playlist_id: i64) -> Result<(), ApiError> {
        self.send_empty::<()>(Method::DELETE, &format!("/playlists/{playlist_id}"), None)
            .await
    }

    /// Appends tracks (duplicates are skipped by the server).
    pub async fn append_to_playlist(
        &self,
        playlist_id: i64,
        track_ids: &[i64],
    ) -> Result<PlaylistItemResult, ApiError> {
        let body = serde_json::json!({ "trackIds": track_ids });
        self.send_json(
            Method::POST,
            &format!("/playlists/{playlist_id}/items/append"),
            &body,
        )
        .await
    }

    /// Appends a whole album in disc / track order.
    pub async fn append_album_to_playlist(
        &self,
        playlist_id: i64,
        album_id: i64,
    ) -> Result<PlaylistItemResult, ApiError> {
        let body = serde_json::json!({ "albumId": album_id });
        self.send_json(
            Method::POST,
            &format!("/playlists/{playlist_id}/items/append"),
            &body,
        )
        .await
    }

    /// Removes tracks; 409 `playlist_conflict` when `revision` is stale.
    pub async fn remove_from_playlist(
        &self,
        playlist_id: i64,
        track_ids: &[i64],
        revision: i64,
    ) -> Result<PlaylistItemResult, ApiError> {
        let body = serde_json::json!({ "trackIds": track_ids, "expectedRevision": revision });
        self.send_json(
            Method::POST,
            &format!("/playlists/{playlist_id}/items/remove"),
            &body,
        )
        .await
    }

    /// New order of every track; 409 `playlist_conflict` when `revision` is stale.
    pub async fn order_playlist(
        &self,
        playlist_id: i64,
        track_ids: &[i64],
        revision: i64,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({ "trackIds": track_ids, "expectedRevision": revision });
        self.send_empty(
            Method::PUT,
            &format!("/playlists/{playlist_id}/order"),
            Some(&body),
        )
        .await
    }

    /// Replaces the whole content (last writer wins).
    pub async fn replace_playlist(
        &self,
        playlist_id: i64,
        track_ids: &[i64],
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({ "trackIds": track_ids });
        self.send_empty(
            Method::PUT,
            &format!("/playlists/{playlist_id}/items"),
            Some(&body),
        )
        .await
    }

    /// Uploads custom artwork (JPEG / PNG / WebP, at most 10 MiB) as the
    /// multipart field `image`.
    pub async fn upload_playlist_artwork(
        &self,
        playlist_id: i64,
        image: Vec<u8>,
    ) -> Result<PlaylistArtwork, ApiError> {
        let boundary = "032player-artwork-7d4a1e9c3b5f";
        let mut body = Vec::with_capacity(image.len() + 256);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"artwork\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&image);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = self.api_url(&format!("/playlists/{playlist_id}/artwork"), &Query::new())?;
        let res = self
            .http
            .put(url)
            .timeout(UPLOAD_TIMEOUT)
            .bearer_auth(&self.api_token)
            .header(header::ACCEPT, "application/json")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(error_from_response(res).await);
        }
        decode(res).await
    }

    /// Back to the automatic artwork.
    pub async fn reset_playlist_artwork(
        &self,
        playlist_id: i64,
    ) -> Result<PlaylistArtwork, ApiError> {
        let res = self
            .send(
                Method::DELETE,
                &format!("/playlists/{playlist_id}/artwork"),
                &Query::new(),
                None,
            )
            .await?;
        decode(res).await
    }

    /// Metadata-similar tracks (`limit` 1..=100). The seed track itself is
    /// left out.
    pub async fn similar_tracks(&self, track_id: i64, limit: u32) -> Result<Vec<Track>, ApiError> {
        let mut q = Query::new();
        q.push("limit", limit.clamp(1, 100));
        let res: Items<SimilarTrack> = self.get(&format!("/tracks/{track_id}/similar"), &q).await?;
        Ok(res
            .items
            .into_iter()
            .map(|s| s.track)
            .filter(|t| t.id != track_id)
            .collect())
    }

    /// `POST /api/v1/playback/events`. Any HTTP status is a result (the
    /// session interprets it); only network failures are errors.
    /// `keepalive` (app close) authenticates with the media token in the
    /// query, like the web client's `sendBeacon`.
    pub async fn post_playback_event<B: Serialize + Sync>(
        &self,
        body: &B,
        keepalive: bool,
    ) -> Result<(u16, serde_json::Value), ApiError> {
        let mut url = self.api_url("/playback/events", &Query::new())?;
        let req = if keepalive {
            set_param(&mut url, "mediaToken", &self.media_token);
            self.http.post(url)
        } else {
            self.http.post(url).bearer_auth(&self.api_token)
        };
        let body = serde_json::to_vec(body).map_err(|e| ApiError::Decode(e.to_string()))?;
        let res = req
            .timeout(PLAYBACK_EVENT_TIMEOUT)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?;
        let status = res.status().as_u16();
        let json = res
            .bytes()
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(serde_json::Value::Null);
        Ok((status, json))
    }

    /// One track (`GET /tracks/{id}`).
    pub async fn track(&self, track_id: i64) -> Result<Track, ApiError> {
        self.get(&format!("/tracks/{track_id}"), &Query::new())
            .await
    }

    /// Other versions / recordings with the same title (`GET /tracks/{id}/versions`).
    pub async fn track_versions(&self, track_id: i64) -> Result<TrackVersionsResponse, ApiError> {
        self.get(&format!("/tracks/{track_id}/versions"), &Query::new())
            .await
    }

    /// Lyrics of a track; `None` when the server has none (404).
    pub async fn lyrics(&self, track_id: i64) -> Result<Option<Lyrics>, ApiError> {
        match self
            .get(&format!("/tracks/{track_id}/lyrics"), &Query::new())
            .await
        {
            Ok(lyrics) => Ok(Some(lyrics)),
            Err(e) if e.status() == Some(404) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // ------------------------------------------------------------ media URLs

    /// Turns a server-relative media path (artwork, artist image, stream)
    /// into an absolute URL carrying the media token. Empty paths or non-HTTP(S)
    /// schemes give `None`. The media token is only attached to same-origin URLs
    /// so third-party or protocol-relative paths never receive credentials.
    pub fn media_url(&self, path: &str, params: &[(&str, String)]) -> Option<Url> {
        if path.is_empty() {
            return None;
        }
        let base = Url::parse(&format!("{}/", self.base_url)).ok()?;
        let mut url =
            if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") {
                let base_prefix = base.path().trim_end_matches('/');
                if !base_prefix.is_empty()
                    && (path == base_prefix
                        || path.starts_with(&format!("{base_prefix}/"))
                        || path.starts_with(&format!("{base_prefix}?"))
                        || path.starts_with(&format!("{base_prefix}#")))
                {
                    base.join(path).ok()?
                } else {
                    Url::parse(&format!("{}{path}", self.base_url)).ok()?
                }
            } else {
                base.join(path).ok()?
            };
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        if url.origin() == base.origin() {
            set_param(&mut url, "mediaToken", &self.media_token);
            for (key, value) in params {
                set_param(&mut url, key, value);
            }
        } else {
            remove_param(&mut url, "mediaToken");
        }
        Some(url)
    }

    /// Original file stream (what makes bit-perfect output possible).
    pub fn stream_url(&self, track_id: i64) -> Url {
        self.media_url(&format!("/api/v1/tracks/{track_id}/stream"), &[])
            .expect("base URL was validated")
    }

    /// Artwork / image URL. `size` `None` or `0` requests the original:
    /// the server rejects `size=0`.
    pub fn image_url(&self, path: &str, size: Option<u32>) -> Option<Url> {
        match size {
            Some(size) if size > 0 => self.media_url(path, &[("size", size.to_string())]),
            _ => self.media_url(path, &[]),
        }
    }

    /// Transcoded stream; `offset_ms` starts lossy formats later (they do
    /// not support byte ranges).
    pub fn transcode_url(&self, track_id: i64, transcode: Transcode, offset_ms: i64) -> Url {
        let mut params = Vec::new();
        match transcode.bitrate() {
            Some(bitrate) => {
                params.push(("bitrate", bitrate.to_string()));
                if offset_ms > 0 {
                    params.push(("offsetMs", offset_ms.to_string()));
                }
            }
            None => params.push(("maxSampleRate", "48000".to_owned())),
        }
        self.media_url(
            &format!("/api/v1/tracks/{track_id}/transcode.{}", transcode.format()),
            &params,
        )
        .expect("base URL was validated")
    }

    pub fn artist_image_url(&self, artist_id: i64, size: Option<u32>) -> Url {
        self.image_url(&format!("/api/v1/artists/{artist_id}/image"), size)
            .expect("base URL was validated")
    }

    pub fn playlist_artwork_url(&self, playlist_id: i64, size: Option<u32>) -> Url {
        self.image_url(&format!("/api/v1/playlists/{playlist_id}/artwork"), size)
            .expect("base URL was validated")
    }

    /// Fetches a media URL with the shared HTTP client (images, probes).
    pub async fn fetch_media(&self, url: Url) -> Result<reqwest::Response, ApiError> {
        Ok(self
            .http
            .get(url)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .send()
            .await?)
    }

    /// A server-side transcode cache entry (FLAC) is ready: `HEAD` answers
    /// 200 with its length. `HEAD` never starts an encoder.
    pub async fn media_cached(&self, url: Url) -> Result<bool, ApiError> {
        let res = self.http.head(url).timeout(PROBE_TIMEOUT).send().await?;
        Ok(res.status() == StatusCode::OK && res.headers().contains_key(header::CONTENT_LENGTH))
    }

    /// Requests the first byte of a media URL (builds a FLAC cache entry and
    /// waits for it). Returns the status and `Retry-After` seconds (503 while
    /// the server's encoders are busy).
    pub async fn warm_media(&self, url: Url) -> Result<(u16, Option<u64>), ApiError> {
        let res = self
            .http
            .get(url)
            .timeout(WARM_MEDIA_TIMEOUT)
            .header(header::RANGE, "bytes=0-0")
            .send()
            .await?;
        let status = res.status().as_u16();
        let retry = res
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        let _ = res.bytes().await;
        Ok((status, retry))
    }

    // ------------------------------------------------------------ auth

    /// Probes the media token: Range-requests the first byte of one track.
    /// An empty library cannot be probed and passes.
    async fn verify_media_token(&self) -> Result<(), AuthError> {
        let mut q = Query::new();
        q.push("limit", 1);
        let page: Page<Track> = self.get("/tracks", &q).await?;
        let Some(track) = page.items.first() else {
            return Ok(());
        };
        let Some(url) = self.media_url(&track.stream_url, &[]) else {
            return Ok(());
        };
        let res = self
            .http
            .get(url)
            .timeout(PROBE_TIMEOUT)
            .header(header::RANGE, "bytes=0-0")
            .send()
            .await
            .map_err(ApiError::from)?;
        if res.status() == StatusCode::UNAUTHORIZED {
            return Err(AuthError::MediaTokenInvalid);
        }
        Ok(())
    }
}

/// Signs in: validates the credentials, the API revision and the media token.
pub async fn authenticate(
    server_url: &str,
    api_token: &str,
    media_token: &str,
) -> Result<(Client, Capabilities), AuthError> {
    let client = Client::new(server_url, api_token.trim(), media_token.trim())?;
    authenticate_client(client).await
}

/// [`authenticate`] with a preconfigured client (custom HTTP settings, tests).
pub async fn authenticate_client(client: Client) -> Result<(Client, Capabilities), AuthError> {
    if client.api_token.is_empty() || client.media_token.is_empty() {
        return Err(AuthError::CredentialsRequired);
    }
    if client.api_token == client.media_token {
        return Err(AuthError::TokensMustDiffer);
    }
    let capabilities = match client.capabilities().await {
        Ok(c) => c,
        Err(e) if e.is_unauthorized() => return Err(AuthError::ApiTokenInvalid),
        Err(e) => return Err(e.into()),
    };
    if capabilities.api_revision != API_REVISION {
        return Err(AuthError::UnsupportedRevision(capabilities.api_revision));
    }
    client.verify_media_token().await?;
    Ok((client, capabilities))
}

async fn decode<T: DeserializeOwned>(res: reqwest::Response) -> Result<T, ApiError> {
    let bytes = res.bytes().await?;
    serde_json::from_slice(&bytes).map_err(|e| ApiError::Decode(e.to_string()))
}

async fn error_from_response(res: reqwest::Response) -> ApiError {
    let status = res.status().as_u16();
    let body: ErrorBody = res
        .bytes()
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let detail = body.error.unwrap_or_default();
    let message = detail
        .message
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("032 Music Server request failed ({status})"));
    ApiError::Http {
        status,
        code: detail.code,
        message,
    }
}

/// Object-safe JSON body so `send` is not generic.
mod erased {
    use crate::error::ApiError;

    pub trait Body {
        fn to_json(&self) -> Result<Vec<u8>, ApiError>;
    }

    pub struct Json<'a, T>(pub &'a T);

    impl<T: serde::Serialize> Body for Json<'_, T> {
        fn to_json(&self) -> Result<Vec<u8>, ApiError> {
            serde_json::to_vec(self.0).map_err(|e| ApiError::Decode(e.to_string()))
        }
    }
}
