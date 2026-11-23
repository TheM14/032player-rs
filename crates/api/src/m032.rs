//! 032-specific endpoints: home, works, series, daily mixes, playback
//! history, track works and lyrics search / translation.
//!
//! Sources: `032music-server/docs/api/{home,works,series}.md`,
//! `docs/api/README.md` and the Electron client's `music032-types.ts`.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::error::ApiError;
use crate::query::Query;
use crate::types::{Album, Items, Lyrics, Page, Playlist, Track};

// ---------------------------------------------------------------- types

/// How a card expands into tracks: `album`, `work`, `series`, `playlist`, `track`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayRef {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: i64,
}

/// Songs per use (op / ed / insert ...) of a work.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleCount {
    pub role: String,
    pub count: i64,
}

/// `GET /works` item and the base of `GET /works/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Work {
    pub id: i64,
    pub title: String,
    pub reading_title: String,
    pub translated_title: String,
    /// anime / drama / movie / game / commercial / other
    #[serde(rename = "type")]
    pub kind: String,
    pub year: i32,
    /// The stored (usually remote) poster; `GET /works/{id}` has the local
    /// route in `poster_api_url`.
    pub poster_url: String,
    pub track_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// `series` of `GET /works/{id}` (capability `workSeriesInfo`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkSeriesRef {
    pub id: i64,
    pub title: String,
    pub title_source: String,
    pub member_count: i64,
    /// 1-based position among the members.
    pub position: i64,
    pub source: String,
}

/// `GET /works/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkDetail {
    #[serde(flatten)]
    pub work: Work,
    /// `/api/v1/works/{id}/poster?v=...`; empty until the poster is cached.
    pub poster_api_url: String,
    pub bangumi_subject_id: String,
    pub series: Option<WorkSeriesRef>,
    pub series_locked: bool,
}

/// `GET /works/{id}/albums` item (also in series album lists).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AlbumWork {
    pub album_id: i64,
    pub work_id: i64,
    pub title: String,
    pub role: String,
    /// Album-level relation (`ost` / `other`).
    pub album_role: String,
    /// Track-level uses of the work on this album, by priority.
    pub track_roles: Vec<String>,
    pub source: String,
    pub season: i32,
    pub artist: String,
    pub year: i32,
    pub album_type: String,
    pub disc_count: i32,
    pub track_count: i64,
    pub artwork_url: String,
}

/// `GET /works/{id}/tracks` item: a track and how it belongs to the work.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkTrack {
    #[serde(flatten)]
    pub track: Track,
    pub role: String,
    pub season: i32,
    /// OP1 / ED2 numbering; 0 shows no number.
    pub sequence: i32,
    /// album / manual / bangumi / auto
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TypeCount {
    #[serde(rename = "type")]
    pub kind: String,
    pub count: i64,
}

/// `GET /series` item and `series` of the detail.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Series {
    pub id: i64,
    pub title: String,
    /// `manual`: renamed by the user.
    pub title_source: String,
    pub member_count: i64,
    pub type_counts: Vec<TypeCount>,
    pub representative_work_id: i64,
    pub poster_url: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeriesMember {
    pub id: i64,
    pub title: String,
    pub reading_title: String,
    pub translated_title: String,
    pub work_type: String,
    pub year: i32,
    pub track_count: i64,
    pub poster_url: String,
    pub source: String,
    pub play: PlayRef,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeriesStats {
    pub member_count: i64,
    pub year_from: i32,
    pub year_to: i32,
    pub album_count: i64,
    pub track_count: i64,
}

/// `GET /series/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeriesDetail {
    pub series: Series,
    /// Earliest first.
    pub members: Vec<SeriesMember>,
    pub stats: SeriesStats,
    pub play: PlayRef,
}

/// `GET /series/{id}/albums` item and `spotlight.series.albums[]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeriesAlbumEntry {
    pub work_id: i64,
    pub work_title: String,
    pub album: AlbumWork,
    pub play: PlayRef,
}

/// `GET /playback/history` item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaybackRecord {
    pub track_id: i64,
    pub position_millis: i64,
    pub duration_millis: i64,
    pub play_count: i64,
    pub skip_count: i64,
    pub last_played_at: String,
    /// playing / paused / completed / skipped / interrupted / ...
    pub state: String,
    pub track: Track,
}

/// Album tab of the home spotlight.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomeAlbumPane {
    /// recent / listening / random
    pub focus: String,
    pub play: PlayRef,
    pub album: Album,
    /// At most 60; the album has `album.track_count`.
    pub tracks: Vec<Track>,
    pub preview_duration_millis: i64,
    /// recent: added; listening / random: last played ("" = never).
    pub note_time: String,
    /// listening only: the track to continue.
    pub resume: Option<PlaybackRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomeSeriesWork {
    pub work: Work,
    pub role_counts: Vec<RoleCount>,
}

/// Works tab of the home spotlight: a series, or the work with the most
/// linked tracks when the library has no series.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomeSeriesPane {
    pub is_series: bool,
    pub series_id: i64,
    pub title: String,
    pub play: PlayRef,
    pub work_ids: Vec<i64>,
    pub works: Vec<HomeSeriesWork>,
    pub albums: Vec<SeriesAlbumEntry>,
    pub album_count: i64,
    pub track_total: i64,
    pub year_from: i32,
    pub year_to: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomeSpotlight {
    /// recent, listening, series, random (in display order).
    pub tabs: Vec<String>,
    pub default_tab: String,
    pub recent: Option<HomeAlbumPane>,
    pub listening: Option<HomeAlbumPane>,
    pub random: Option<HomeAlbumPane>,
    pub series: Option<HomeSeriesPane>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IdTitle {
    pub id: i64,
    pub title: String,
}

/// A work in the daily works rotation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomeWorkCard {
    pub id: i64,
    pub title: String,
    pub year: i32,
    pub track_count: i64,
    pub work_type: String,
    pub poster_url: String,
    pub cover_url: String,
    /// The poster when cached, else the first album cover.
    pub artwork_url: String,
    pub series: Option<IdTitle>,
    pub role_counts: Vec<RoleCount>,
    pub play: PlayRef,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSongWork {
    pub id: i64,
    pub title: String,
    pub year: i32,
}

/// A theme / insert song in the daily songs rotation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSongRow {
    pub track: Track,
    pub role: String,
    pub work: HomeSongWork,
    pub play: PlayRef,
}

/// One page of a daily rotation. Turning pages sends `generation` back;
/// `renewed`: it had expired and this is page 0 of the new one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RotationPage<T> {
    /// Works only: all / anime / game / movie.
    #[serde(rename = "type")]
    pub kind: String,
    pub generation: String,
    pub batch: u32,
    pub pages: u32,
    pub count: i64,
    pub renewed: bool,
    pub items: Vec<T>,
}

impl<T> Default for RotationPage<T> {
    fn default() -> Self {
        Self {
            kind: String::new(),
            generation: String::new(),
            batch: 0,
            pages: 0,
            count: 0,
            renewed: false,
            items: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct YearCount {
    pub year: i32,
    pub count: i64,
}

/// Albums of one release year (newest added first, at most 12).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chronicle {
    pub year: i32,
    pub total: i64,
    pub albums: Vec<Album>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomeChronicle {
    /// Ascending.
    pub years: Vec<YearCount>,
    pub selected_year: i32,
    pub result: Chronicle,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomeStats {
    pub albums: i64,
    pub tracks: i64,
    pub works: i64,
    pub last_scan_at: String,
    pub pending_review_count: i64,
}

/// `GET /home`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Home {
    /// No albums yet (first run): only `stats` is filled.
    pub empty: bool,
    pub generated_at: String,
    pub rotation_day: String,
    pub spotlight: Option<HomeSpotlight>,
    /// Unfinished tracks, most recent first (at most 5).
    pub resume: Vec<PlaybackRecord>,
    pub recent_albums: Vec<Album>,
    pub works: RotationPage<HomeWorkCard>,
    pub songs: RotationPage<HomeSongRow>,
    pub chronicle: Option<HomeChronicle>,
    pub playlists: Vec<Playlist>,
    pub stats: HomeStats,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DailyMixTag {
    /// mood / style
    pub kind: String,
    pub tag: String,
    pub label: String,
}

/// A daily mix (today's or an older one).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DailyMix {
    pub id: i64,
    pub day: String,
    pub slot: i32,
    /// active / replaced / ...
    pub status: String,
    pub title: String,
    pub description: String,
    pub theme_key: String,
    pub theme_type: String,
    pub theme_type_label: String,
    pub tags: Vec<DailyMixTag>,
    pub track_count: i64,
    pub duration_millis: i64,
    /// At most four album covers.
    pub cover_urls: Vec<String>,
    /// 1 like, -1 dislike, 0 none.
    pub rating: i32,
    pub rating_reasons: Vec<String>,
    pub saved_playlist_id: Option<i64>,
    pub created_at: String,
}

/// `GET /daily-mixes`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DailyMixList {
    pub day: String,
    /// ready / generating (HTTP 202) / not_ready / disabled
    pub status: String,
    pub message: Option<String>,
    pub replacements_left: i64,
    pub items: Vec<DailyMix>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DailyMixTrack {
    /// 0-based.
    pub position: i64,
    /// familiar / explore (hardly played yet).
    pub bucket: String,
    /// Marked "不再推荐".
    pub blocked: bool,
    pub track: Track,
}

/// `GET /daily-mixes/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DailyMixDetail {
    pub mix: DailyMix,
    pub tracks: Vec<DailyMixTrack>,
}

/// Dislike reasons the server accepts.
pub const DAILY_MIX_REASONS: [&str; 5] = ["repetitive", "theme", "tracks", "flow", "mismatch"];

/// `GET /tracks/{id}/works` item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TrackWork {
    pub id: i64,
    pub title: String,
    pub translated_title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub season: i32,
    pub series_id: Option<i64>,
}

/// `GET /albums/{id}/works` item: one row per work linked to the album (the
/// album-level link, else the first linked track).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AlbumWorkLink {
    pub id: i64,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub source: String,
    pub track_id: Option<i64>,
    pub track_title: String,
}

/// `GET /lyrics/search` result; `score` 0 is a perfect match.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LyricsSearchResult {
    pub source: String,
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    /// `None`: unknown.
    pub synced: Option<bool>,
    pub score: f64,
}

/// `POST /tracks/{id}/lyrics/translation`: one entry per lyrics line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LyricsTranslation {
    pub lines: Vec<String>,
    pub model: String,
    pub cached: bool,
    /// `ai`, or the lyrics source whose own translation was used.
    pub source: String,
    pub target_language: String,
}

/// Sort of `GET /works`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkSort {
    #[default]
    Title,
    Year,
    Updated,
}

impl WorkSort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Year => "year",
            Self::Updated => "updated",
        }
    }
}

/// Filters of `GET /works`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkListQuery {
    pub search: String,
    /// Empty: every type.
    pub kind: String,
    pub sort: WorkSort,
}

// ---------------------------------------------------------------- endpoints

impl Client {
    pub async fn home(&self) -> Result<Home, ApiError> {
        self.get("/home", &Query::new()).await
    }

    /// A page of the works rotation (`kind`: all / anime / game / movie).
    pub async fn home_works(
        &self,
        kind: &str,
        batch: u32,
        generation: &str,
    ) -> Result<RotationPage<HomeWorkCard>, ApiError> {
        let mut q = Query::new();
        q.push("type", kind).push("batch", batch);
        if !generation.is_empty() {
            q.push("generation", generation);
        }
        self.get("/home/works", &q).await
    }

    pub async fn home_songs(
        &self,
        batch: u32,
        generation: &str,
    ) -> Result<RotationPage<HomeSongRow>, ApiError> {
        let mut q = Query::new();
        q.push("batch", batch);
        if !generation.is_empty() {
            q.push("generation", generation);
        }
        self.get("/home/songs", &q).await
    }

    pub async fn home_chronicle(&self, year: i32) -> Result<Chronicle, ApiError> {
        let mut q = Query::new();
        q.push("year", year);
        self.get("/home/chronicle", &q).await
    }

    /// Another random album for the spotlight, avoiding `exclude`.
    pub async fn home_spotlight_random(&self, exclude: i64) -> Result<HomeAlbumPane, ApiError> {
        let mut q = Query::new();
        if exclude > 0 {
            q.push("exclude", exclude);
        }
        self.get("/home/spotlight/random", &q).await
    }

    pub async fn works(
        &self,
        query: &WorkListQuery,
        offset: u32,
        limit: u32,
    ) -> Result<Page<Work>, ApiError> {
        let mut q = Query::new();
        let search = query.search.trim();
        if !search.is_empty() {
            q.push("q", search);
        }
        if !query.kind.is_empty() {
            q.push("type", &query.kind);
        }
        q.push("sort", query.sort.as_str()).page(offset, limit);
        self.get("/works", &q).await
    }

    pub async fn work(&self, work_id: i64) -> Result<WorkDetail, ApiError> {
        self.get(&format!("/works/{work_id}"), &Query::new()).await
    }

    pub async fn work_albums(&self, work_id: i64) -> Result<Vec<AlbumWork>, ApiError> {
        let res: Items<AlbumWork> = self
            .get(&format!("/works/{work_id}/albums"), &Query::new())
            .await?;
        Ok(res.items)
    }

    pub async fn work_tracks(&self, work_id: i64) -> Result<Vec<WorkTrack>, ApiError> {
        let res: Items<WorkTrack> = self
            .get(&format!("/works/{work_id}/tracks"), &Query::new())
            .await?;
        Ok(res.items)
    }

    /// Series sorted by name; `limit` 1..=200.
    pub async fn series_list(
        &self,
        search: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Page<Series>, ApiError> {
        let mut q = Query::new();
        let search = search.trim();
        if !search.is_empty() {
            q.push("q", search);
        }
        q.page(offset, limit.clamp(1, 200));
        self.get("/series", &q).await
    }

    pub async fn series(&self, series_id: i64) -> Result<SeriesDetail, ApiError> {
        self.get(&format!("/series/{series_id}"), &Query::new())
            .await
    }

    pub async fn series_albums(&self, series_id: i64) -> Result<Vec<SeriesAlbumEntry>, ApiError> {
        let res: Items<SeriesAlbumEntry> = self
            .get(&format!("/series/{series_id}/albums"), &Query::new())
            .await?;
        Ok(res.items)
    }

    /// Every track of the series, members in order, without duplicates.
    pub async fn series_tracks(&self, series_id: i64) -> Result<Vec<WorkTrack>, ApiError> {
        let res: Items<WorkTrack> = self
            .get(&format!("/series/{series_id}/tracks"), &Query::new())
            .await?;
        Ok(res.items)
    }

    /// Works the track belongs to (capability `trackWorks`).
    pub async fn track_works(&self, track_id: i64) -> Result<Vec<TrackWork>, ApiError> {
        let res: Items<TrackWork> = self
            .get(&format!("/tracks/{track_id}/works"), &Query::new())
            .await?;
        Ok(res.items)
    }

    /// Works linked to an album; empty on servers without the endpoint.
    pub async fn album_works(&self, album_id: i64) -> Result<Vec<AlbumWorkLink>, ApiError> {
        match self
            .get::<Items<AlbumWorkLink>>(&format!("/albums/{album_id}/works"), &Query::new())
            .await
        {
            Ok(res) => Ok(res.items),
            Err(e) if e.status() == Some(404) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Tracks with playback records, most recent first.
    pub async fn playback_history(
        &self,
        offset: u32,
        limit: u32,
    ) -> Result<Page<PlaybackRecord>, ApiError> {
        let mut q = Query::new();
        q.page(offset, limit);
        self.get("/playback/history", &q).await
    }

    /// Clears every play count, resume position and session.
    pub async fn clear_playback_history(&self) -> Result<(), ApiError> {
        self.send_empty::<()>(Method::DELETE, "/playback/history", None)
            .await
    }

    /// Today's mixes; `status` `generating` (HTTP 202) while being built.
    pub async fn daily_mixes(&self) -> Result<DailyMixList, ApiError> {
        self.get("/daily-mixes", &Query::new()).await
    }

    pub async fn daily_mix(&self, mix_id: i64) -> Result<DailyMixDetail, ApiError> {
        self.get(&format!("/daily-mixes/{mix_id}"), &Query::new())
            .await
    }

    /// `value`: 1 like, -1 dislike (with `reasons`), 0 clears.
    pub async fn rate_daily_mix(
        &self,
        mix_id: i64,
        value: i32,
        reasons: &[String],
    ) -> Result<DailyMix, ApiError> {
        let reasons: &[String] = if value == -1 { reasons } else { &[] };
        let body = serde_json::json!({ "value": value, "reasons": reasons });
        self.send_json(Method::PUT, &format!("/daily-mixes/{mix_id}/rating"), &body)
            .await
    }

    /// 换一个: a new theme in the same slot; 429 when today's replacements are used up.
    pub async fn regenerate_daily_mix(&self, mix_id: i64) -> Result<DailyMix, ApiError> {
        self.send_json(
            Method::POST,
            &format!("/daily-mixes/{mix_id}/regenerate"),
            &serde_json::json!({}),
        )
        .await
    }

    /// Saves the mix as a normal playlist.
    pub async fn save_daily_mix(&self, mix_id: i64) -> Result<Playlist, ApiError> {
        #[derive(Deserialize)]
        struct Saved {
            playlist: Playlist,
        }
        let saved: Saved = self
            .send_json(
                Method::POST,
                &format!("/daily-mixes/{mix_id}/save"),
                &serde_json::json!({}),
            )
            .await?;
        Ok(saved.playlist)
    }

    /// 不再推荐 (`blocked`) or recommend again.
    pub async fn set_recommendation_block(
        &self,
        track_id: i64,
        blocked: bool,
    ) -> Result<(), ApiError> {
        let path = format!("/tracks/{track_id}/recommendation-block");
        if blocked {
            self.send_empty(Method::PUT, &path, Some(&serde_json::json!({})))
                .await
        } else {
            self.send_empty::<()>(Method::DELETE, &path, None).await
        }
    }

    /// Online lyrics search (`duration_ms` helps ranking).
    pub async fn search_lyrics(
        &self,
        name: &str,
        artist: &str,
        duration_ms: Option<i64>,
    ) -> Result<Vec<LyricsSearchResult>, ApiError> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Results {
            results: Vec<LyricsSearchResult>,
        }
        let mut q = Query::new();
        q.push("name", name.trim()).push("artist", artist.trim());
        q.push_opt("durationMs", duration_ms.filter(|d| *d > 0));
        let res: Results = self.get("/lyrics/search", &q).await?;
        Ok(res.results)
    }

    /// Preview of one search result; `None` when the source has none.
    pub async fn remote_lyrics(&self, source: &str, id: &str) -> Result<Option<Lyrics>, ApiError> {
        let path = format!("/lyrics/remote/{}/{}", url_segment(source), url_segment(id));
        match self.get(&path, &Query::new()).await {
            Ok(lyrics) => Ok(Some(lyrics)),
            Err(e) if e.status() == Some(404) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Uses a search result as the track's lyrics on the server (every client).
    pub async fn set_lyrics_source(
        &self,
        track_id: i64,
        pick: &LyricsSearchResult,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({
            "source": pick.source,
            "id": pick.id,
            "name": pick.name,
            "artist": pick.artist,
        });
        self.send_empty(
            Method::PUT,
            &format!("/tracks/{track_id}/lyrics/source"),
            Some(&body),
        )
        .await
    }

    /// Drops the online lyrics and stops automatic lookups (local lyrics stay).
    pub async fn clear_lyrics_source(&self, track_id: i64) -> Result<(), ApiError> {
        self.send_empty::<()>(
            Method::DELETE,
            &format!("/tracks/{track_id}/lyrics/source"),
            None,
        )
        .await
    }

    /// Translates the track's lyrics; 409 `translation_disabled` when off.
    pub async fn translate_lyrics(
        &self,
        track_id: i64,
        target_language: &str,
    ) -> Result<LyricsTranslation, ApiError> {
        self.send_json(
            Method::POST,
            &format!("/tracks/{track_id}/lyrics/translation"),
            &serde_json::json!({ "targetLanguage": target_language }),
        )
        .await
    }
}

/// Local poster route of a work: the API route when known, else the
/// default one (older servers list the remote poster address).
pub fn work_poster_path(work_id: i64, poster_url: &str) -> String {
    if poster_url.starts_with('/')
        && !poster_url.starts_with("//")
        && !poster_url.starts_with("/\\")
    {
        poster_url.to_owned()
    } else {
        format!("/api/v1/works/{work_id}/poster")
    }
}

fn url_segment(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
