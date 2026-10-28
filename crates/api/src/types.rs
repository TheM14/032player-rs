//! Wire types for 032 Music Server API v1 (apiRevision 3).
//!
//! Source of truth: `032music-server/internal/storage/*.go` and
//! `internal/http/*.go`; mirrored from `music032-types.ts`. Every field has a
//! default so older servers that omit fields still decode.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistRef {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Album {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub artists: Vec<ArtistRef>,
    pub genres: String,
    pub artwork_url: String,
    pub year: i32,
    pub disc_count: i32,
    pub track_count: i64,
    pub performed_by: String,
    pub album_type: String,
    pub release_kind: String,
    pub version: String,
    pub release_date: String,
    pub original_release_date: String,
    pub label: String,
    pub catalog_number: String,
    pub country: String,
    pub review: String,
    pub compilation: bool,
    pub live: bool,
    pub bootleg: bool,
    pub formats: String,
    pub total_bytes: i64,
    /// Human-readable `total_bytes`, e.g. "1.2 GB".
    pub total_size: String,
    pub added_at: String,
    pub updated_at: String,
    pub last_played_at: Option<String>,
    pub is_favorite: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackCredit {
    pub role: String,
    pub artists: Vec<ArtistRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Track {
    pub id: i64,
    pub album_id: i64,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub artists: Vec<ArtistRef>,
    /// Credited album artists; only `/browse/tracks` returns them.
    pub album_artists: Vec<ArtistRef>,
    pub credits: Vec<TrackCredit>,
    pub genres: String,
    pub composer: String,
    pub lyricist: String,
    pub arranger: String,
    pub track_type: String,
    pub container: String,
    pub mime_type: String,
    pub relative_path: String,
    pub artwork_url: String,
    pub year: i32,
    pub disc_number: i32,
    pub track_number: i32,
    pub file_size: i64,
    pub duration_millis: i64,
    pub stream_url: String,
    pub added_at: String,
    pub updated_at: String,
    pub last_played_at: Option<String>,
    pub position_millis: i64,
    pub play_count: i64,
    pub is_favorite: bool,
    pub missing: bool,
    pub codec: Option<String>,
    pub sample_rate: Option<i32>,
    pub bit_depth: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    pub channels: Option<i32>,
}

/// List entry of `/artists`, `/browse/artists`, `/favorites/artists`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Artist {
    pub id: i64,
    pub name: String,
    pub image_url: String,
    pub album_count: i64,
    pub track_count: i64,
    pub is_favorite: bool,
}

/// `artist` of `GET /artists/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArtistInfo {
    pub id: i64,
    pub name: String,
    pub image_url: String,
    pub is_favorite: bool,
    pub album_count: i64,
    pub track_count: i64,
    pub biography: String,
    pub biography_source: String,
    pub country: String,
    pub artist_type: String,
    pub aliases: Vec<String>,
    /// The requested id when it was merged into this artist.
    pub merged_from: i64,
}

/// A release on the artist page; `relation` says how the artist is on it
/// (own, collaboration, appearance, ...).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistRelease {
    #[serde(flatten)]
    pub album: Album,
    pub relation: String,
}

/// `GET /artists/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArtistDetail {
    pub artist: ArtistInfo,
    pub albums: Vec<Album>,
    pub releases: Vec<ArtistRelease>,
    /// Most played (`top_tracks_are_popular`) or the first ten in album order.
    pub top_tracks: Vec<Track>,
    pub top_tracks_are_popular: bool,
    pub tracks_total: i64,
}

/// `GET /albums/{id}`: embeds at most 500 tracks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlbumDetail {
    pub album: Album,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub item_count: i64,
    /// Missing on servers older than the playlist-duration change.
    pub duration_millis: Option<i64>,
    pub artwork_url: String,
    pub has_custom_artwork: bool,
    /// Bumped per edit; remove / order send it as `expectedRevision`.
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// `GET /playlists/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

/// Result of append / insert / remove on playlist items.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaylistItemResult {
    pub added: i64,
    pub removed: i64,
    pub skipped_duplicate: i64,
    pub skipped_invalid: i64,
    pub total: i64,
    pub revision: i64,
}

/// Answer of playlist artwork upload / reset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaylistArtwork {
    pub artwork_url: String,
    pub revision: i64,
    pub has_custom_artwork: bool,
}

/// One entry of `GET /tracks/{id}/similar` (metadata similarity).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SimilarTrack {
    pub track: Track,
    pub distance: f64,
    pub score: f64,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Items<T> {
    pub items: Vec<T>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub total: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaybackCapabilities {
    /// Accepted clientKind values; servers before C4 omit it and only accept web/android.
    pub client_kinds: Vec<String>,
    pub context_types: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Limits {
    pub max_page_size: u32,
    pub max_playlist_tracks: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_page_size: crate::MAX_PAGE_SIZE,
            max_playlist_tracks: 5000,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscodeFormat {
    pub available: bool,
    pub bitrates: Vec<u32>,
    pub default: Option<u32>,
    pub max_sample_rates: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscodeCapabilities {
    pub available: bool,
    pub formats: HashMap<String, TranscodeFormat>,
}

/// Feature flags the client checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Browse,
    Lyrics,
    LyricsOnline,
    LyricsTranslation,
    PlaylistArtwork,
    TrackWorks,
    DailyMixes,
    PlaybackContext,
    Series,
    Home,
}

impl Feature {
    pub fn key(self) -> &'static str {
        match self {
            Self::Browse => "browse",
            Self::Lyrics => "lyrics",
            Self::LyricsOnline => "lyricsOnline",
            Self::LyricsTranslation => "lyricsTranslation",
            Self::PlaylistArtwork => "playlistArtwork",
            Self::TrackWorks => "trackWorks",
            Self::DailyMixes => "dailyMixes",
            Self::PlaybackContext => "playbackContext",
            Self::Series => "series",
            Self::Home => "home",
        }
    }
}

/// `GET /api/v1/capabilities`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Capabilities {
    pub api_version: String,
    pub api_revision: i64,
    pub features: HashMap<String, bool>,
    pub playback: PlaybackCapabilities,
    pub transcode: Option<TranscodeCapabilities>,
    pub limits: Limits,
}

impl Capabilities {
    pub fn has(&self, feature: Feature) -> bool {
        self.features.get(feature.key()).copied().unwrap_or(false)
    }

    /// The server can transcode to `format` (`mp3`, `ogg`, `flac`).
    pub fn can_transcode(&self, format: &str) -> bool {
        self.transcode
            .as_ref()
            .is_some_and(|t| t.available && t.formats.get(format).is_some_and(|f| f.available))
    }

    /// clientKind to report: `desktop` when the server accepts it, else `web`.
    pub fn client_kind(&self) -> &'static str {
        if self.playback.client_kinds.iter().any(|k| k == "desktop") {
            "desktop"
        } else {
            "web"
        }
    }

    /// Page size to request, clamped to the server limit.
    pub fn page_size(&self) -> u32 {
        match self.limits.max_page_size {
            0 => crate::MAX_PAGE_SIZE,
            n => n.min(crate::MAX_PAGE_SIZE),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LyricsLine {
    pub text: String,
    pub time_ms: i64,
}

/// `GET /tracks/{id}/lyrics`. The server resolves the source (manual pick,
/// sidecar .lrc / embedded tag, then online lyrics).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Lyrics {
    pub lines: Vec<LyricsLine>,
    pub manual: bool,
    pub remote_artist: Option<String>,
    pub remote_title: Option<String>,
    /// file | embedded | netease | lrclib
    pub source: Option<String>,
    pub source_name: Option<String>,
    pub synced: bool,
}

/// `{"error":{"code","message"}}`.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct ErrorBody {
    #[serde(default)]
    pub error: Option<ErrorDetail>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct ErrorDetail {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}
