//! Views of the 032-specific pages (home, works, series, daily mixes,
//! playback history) and their labels, mapped from the API types. Labels
//! follow `features/home/music032/lib/format.ts` and the zh-Hans strings.

use api::{
    AlbumWork, DailyMix, DailyMixDetail, Home, HomeAlbumPane, HomeSeriesPane, HomeSongRow,
    HomeWorkCard, PlaybackRecord, RoleCount, RotationPage, Series, SeriesAlbumEntry, SeriesDetail,
    SeriesMember, Track, Work, WorkDetail, WorkTrack, work_poster_path,
};

use crate::browse::{AlbumItem, PlaylistItem, TrackItem, album_item, playlist_item, track_item};
use crate::format;

// ---------------------------------------------------------------- labels

/// Short label of a track use (op / ed / insert ...).
pub fn role_label(role: &str) -> String {
    match role {
        "character" => "角色歌".into(),
        "ed" => "ED".into(),
        "image_song" => "印象曲".into(),
        "insert" => "插曲".into(),
        "op" => "OP".into(),
        "theme" => "主题曲".into(),
        other => other.to_uppercase(),
    }
}

/// OP / ED / theme tags use the accent color.
pub fn is_primary_role(role: &str) -> bool {
    matches!(role, "op" | "ed" | "theme")
}

/// "OP1" / "ED"; sequence 0 shows no number.
pub fn role_sequence_label(role: &str, sequence: i32) -> String {
    if sequence > 0 {
        format!("{}{sequence}", role_label(role))
    } else {
        role_label(role)
    }
}

/// Work type of the home filters (anime / game / movie, else "其他").
pub fn work_type_label(kind: &str) -> &'static str {
    match kind {
        "anime" => "动画",
        "game" => "游戏",
        "movie" => "电影",
        _ => "其他",
    }
}

/// Every work type (works and series pages).
pub fn work_type_long_label(kind: &str) -> &'static str {
    match kind {
        "commercial" => "广告",
        "drama" => "电视剧",
        other => work_type_label(other),
    }
}

/// Work types of the works page filter, in menu order.
pub const WORK_TYPES: [&str; 6] = ["anime", "drama", "movie", "game", "commercial", "other"];

fn album_type_label(kind: &str) -> &'static str {
    match kind {
        "album" => "专辑",
        "bootleg" => "非官方发行",
        "compilation" => "合辑",
        "ep" => "EP",
        "live" => "现场",
        "single" => "单曲",
        "soundtrack" => "原声",
        _ => "其他",
    }
}

fn source_label(source: &str) -> &'static str {
    match source {
        "album" => "整张专辑",
        "bangumi" => "Bangumi",
        "manual" => "手动",
        _ => "自动",
    }
}

/// Relation of an album to a work (`workAlbumRelationLabel`): "原声集 OST",
/// "OP · ED 单曲", "相关专辑".
pub fn album_relation_label(album: &AlbumWork) -> String {
    let album_role = if album.album_role.is_empty() {
        album.role.as_str()
    } else {
        album.album_role.as_str()
    };
    let is_ost = album_role == "ost";
    let meaningful: Vec<&str> = album
        .track_roles
        .iter()
        .map(String::as_str)
        .filter(|r| !r.is_empty() && *r != "other" && !(*r == "ost" && is_ost))
        .collect();
    if is_ost && meaningful.is_empty() {
        return "原声集 OST".into();
    }
    let suffix = album_type_label(if album.album_type.is_empty() {
        "album"
    } else {
        &album.album_type
    });
    let mut parts: Vec<String> = Vec::new();
    if is_ost {
        parts.push("OST".into());
    }
    for role in meaningful {
        let short = role_label(role);
        if !parts.contains(&short) {
            parts.push(short);
        }
    }
    if parts.is_empty() {
        format!("相关{suffix}")
    } else {
        format!("{} {suffix}", parts.join(" · "))
    }
}

fn role_counts_label(counts: &[RoleCount]) -> String {
    counts
        .iter()
        .map(|c| format!("{} {}", role_label(&c.role), c.count))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn type_year(kind: &str, year: i32, long: bool) -> String {
    let kind = if long {
        work_type_long_label(kind)
    } else {
        work_type_label(kind)
    };
    if year > 0 {
        format!("{kind} · {year}")
    } else {
        kind.to_owned()
    }
}

/// "1 小时 22 分" / "48 分钟" (`formatMixDuration`).
pub fn mix_duration(millis: i64) -> String {
    let minutes = millis.max(0) / 60_000;
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 {
        format!("{h} 小时 {m} 分")
    } else {
        format!("{m} 分钟")
    }
}

/// Mood · style (or the theme type) · N 首 · duration.
pub fn mix_subtitle(mix: &DailyMix) -> String {
    let mut parts: Vec<String> = mix.tags.iter().map(|t| t.label.clone()).collect();
    if parts.is_empty() && !mix.theme_type_label.is_empty() {
        parts.push(mix.theme_type_label.clone());
    }
    parts.push(format!("{} 首", mix.track_count));
    parts.push(mix_duration(mix.duration_millis));
    parts.join(" · ")
}

/// Dislike reasons, in the order offered.
pub fn mix_reason_label(code: &str) -> &str {
    match code {
        "repetitive" => "太重复",
        "theme" => "不喜欢这个主题",
        "tracks" => "歌不好听",
        "flow" => "风格太跳",
        "mismatch" => "歌和主题不符",
        other => other,
    }
}

/// Playback history states.
pub fn record_state_label(state: &str) -> String {
    match state {
        "playing" => "播放中".into(),
        "buffering" => "缓冲中".into(),
        "paused" => "已暂停".into(),
        "interrupted" => "已中断".into(),
        "completed" => "播放完成".into(),
        "skipped" => "已跳过".into(),
        "error" => "播放错误".into(),
        "stopped" => "已停止".into(),
        other => other.to_owned(),
    }
}

// ---------------------------------------------------------------- rows

/// A work as a poster card (works page, series members, home rotation).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkItem {
    pub id: i64,
    pub title: String,
    /// Translated or reading title.
    pub subtitle: String,
    /// "动画 · 2023".
    pub kind: String,
    pub artwork: String,
    pub track_count: i64,
    /// "OP 1 · ED 2" (home rotation).
    pub roles: String,
    pub series_id: i64,
    pub series_title: String,
}

pub fn work_item(work: &Work) -> WorkItem {
    WorkItem {
        id: work.id,
        title: work.title.clone(),
        subtitle: first_nonempty(&work.translated_title, &work.reading_title),
        kind: type_year(&work.kind, work.year, true),
        artwork: work_poster_path(work.id, &work.poster_url),
        track_count: work.track_count,
        ..WorkItem::default()
    }
}

pub fn member_item(member: &SeriesMember) -> WorkItem {
    WorkItem {
        id: member.id,
        title: member.title.clone(),
        subtitle: first_nonempty(&member.translated_title, &member.reading_title),
        kind: type_year(&member.work_type, member.year, true),
        artwork: work_poster_path(member.id, &member.poster_url),
        track_count: member.track_count,
        ..WorkItem::default()
    }
}

pub fn work_card_item(card: &HomeWorkCard) -> WorkItem {
    WorkItem {
        id: card.id,
        title: card.title.clone(),
        subtitle: String::new(),
        kind: type_year(&card.work_type, card.year, false),
        artwork: card.artwork_url.clone(),
        track_count: card.track_count,
        roles: role_counts_label(&card.role_counts),
        series_id: card.series.as_ref().map_or(0, |s| s.id),
        series_title: card
            .series
            .as_ref()
            .map(|s| s.title.clone())
            .unwrap_or_default(),
    }
}

fn first_nonempty(a: &str, b: &str) -> String {
    if a.is_empty() { b } else { a }.to_owned()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeriesItem {
    pub id: i64,
    pub title: String,
    pub renamed: bool,
    /// "4 部作品 · 动画 3 · 游戏 1".
    pub meta: String,
    pub poster: String,
}

pub fn series_item(series: &Series) -> SeriesItem {
    let mut meta = format!("{} 部作品", series.member_count);
    for tc in &series.type_counts {
        meta.push_str(&format!(
            " · {} {}",
            work_type_long_label(&tc.kind),
            tc.count
        ));
    }
    SeriesItem {
        id: series.id,
        title: series.title.clone(),
        renamed: series.title_source == "manual",
        meta,
        poster: if series.poster_url.is_empty() {
            String::new()
        } else {
            work_poster_path(series.representative_work_id, &series.poster_url)
        },
    }
}

/// A track with its playback record (接着听, playback history).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordItem {
    pub track: TrackItem,
    pub artwork: String,
    pub position_ms: i64,
    pub percent: i32,
    /// "1:04 / 4:00".
    pub progress: String,
    pub when: String,
    pub state: String,
    pub play_count: i64,
}

pub fn record_item(record: &PlaybackRecord) -> RecordItem {
    let duration = record.duration_millis;
    let percent = if duration > 0 {
        i32::try_from((record.position_millis.max(0) * 100 + duration / 2) / duration)
            .unwrap_or(100)
            .min(100)
    } else {
        0
    };
    RecordItem {
        track: track_item(&record.track),
        artwork: record.track.artwork_url.clone(),
        position_ms: record.position_millis.max(0),
        percent,
        progress: format!(
            "{} / {}",
            format::duration(record.position_millis),
            format::duration(duration)
        ),
        when: format::date_time(&record.last_played_at),
        state: record_state_label(&record.state),
        play_count: record.play_count,
    }
}

/// A recently played track (no progress).
pub fn recent_item(track: &Track) -> RecordItem {
    RecordItem {
        track: track_item(track),
        artwork: track.artwork_url.clone(),
        when: format::date_time(track.last_played_at.as_deref().unwrap_or("")),
        ..RecordItem::default()
    }
}

/// A theme / insert song of the home rotation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SongItem {
    pub track: TrackItem,
    pub artwork: String,
    pub role: String,
    pub primary: bool,
    pub work_id: i64,
    pub work_title: String,
}

pub fn song_item(row: &HomeSongRow) -> SongItem {
    SongItem {
        track: track_item(&row.track),
        artwork: row.track.artwork_url.clone(),
        role: role_label(&row.role),
        primary: is_primary_role(&row.role),
        work_id: row.work.id,
        work_title: row.work.title.clone(),
    }
}

/// A daily mix row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixItem {
    pub id: i64,
    pub title: String,
    pub subtitle: String,
    /// Four covers, or one filling the tile.
    pub covers: Vec<String>,
    pub rating: i32,
}

pub fn mix_item(mix: &DailyMix) -> MixItem {
    let covers = if mix.cover_urls.len() >= 4 {
        mix.cover_urls[..4].to_vec()
    } else {
        mix.cover_urls.iter().take(1).cloned().collect()
    };
    MixItem {
        id: mix.id,
        title: mix.title.clone(),
        subtitle: mix_subtitle(mix),
        covers,
        rating: mix.rating,
    }
}

// ---------------------------------------------------------------- home

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SpotlightTab {
    #[default]
    Recent,
    Listening,
    Series,
    Random,
}

impl SpotlightTab {
    pub fn parse(tab: &str) -> Option<Self> {
        Some(match tab {
            "recent" => Self::Recent,
            "listening" => Self::Listening,
            "series" => Self::Series,
            "random" => Self::Random,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Recent => "最新入库",
            Self::Listening => "最近在听",
            Self::Series => "作品聚焦",
            Self::Random => "随机重温",
        }
    }
}

/// An album tab of the spotlight; its preview tracks are the list
/// `TrackList::Spotlight(tab)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlbumPane {
    pub tab: SpotlightTab,
    pub album: AlbumItem,
    pub track_count: i64,
    /// Year, "24 首", "2 Disc", duration.
    pub facts: Vec<String>,
    pub spec: String,
    /// "入库于 3 天前" / "上次播放 …" / "从未播放".
    pub note: String,
    /// Listening: "从 1:35 继续".
    pub resume: String,
    pub tracks: Vec<TrackItem>,
    /// "查看全部 24 首" when the preview is shorter than the album.
    pub more: String,
}

pub fn album_pane(tab: SpotlightTab, pane: &HomeAlbumPane) -> AlbumPane {
    let album = &pane.album;
    let mut facts = Vec::new();
    if album.year > 0 {
        facts.push(album.year.to_string());
    }
    facts.push(format!("{} 首", album.track_count));
    if album.disc_count > 1 {
        facts.push(format!("{} Disc", album.disc_count));
    }
    if pane.preview_duration_millis > 0 {
        facts.push(format::duration(pane.preview_duration_millis));
    }
    let note = if pane.focus == "recent" {
        if pane.note_time.is_empty() {
            String::new()
        } else {
            format!("入库于 {}", format::relative_time(&pane.note_time))
        }
    } else if pane.note_time.is_empty() {
        "从未播放".into()
    } else {
        format!("上次播放 {}", format::relative_time(&pane.note_time))
    };
    let shown = i64::try_from(pane.tracks.len()).unwrap_or(i64::MAX);
    AlbumPane {
        tab,
        album: album_item(album),
        track_count: album.track_count,
        facts,
        spec: format::spec_label(&pane.tracks).unwrap_or_default(),
        note,
        resume: pane
            .resume
            .as_ref()
            .filter(|_| pane.focus == "listening")
            .map(|r| format!("从 {} 继续", format::duration(r.position_millis)))
            .unwrap_or_default(),
        tracks: pane.tracks.iter().map(track_item).collect(),
        more: if shown < album.track_count {
            format!("查看全部 {} 首", album.track_count)
        } else {
            "专辑详情".into()
        },
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpotlightAlbum {
    pub id: i64,
    pub title: String,
    /// "MyGO!!!!! · 24 首".
    pub subtitle: String,
    pub year: String,
    pub artwork: String,
}

/// The works tab: a series, or the work with the most linked tracks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeriesPane {
    pub is_series: bool,
    pub series_id: i64,
    /// Opened by the title when not a series.
    pub first_work_id: i64,
    pub title: String,
    /// "系列 · 2 部作品".
    pub by: String,
    pub facts: Vec<String>,
    /// (work id, title, "动画 · 2023").
    pub works: Vec<(i64, String, String)>,
    pub albums: Vec<SpotlightAlbum>,
    pub album_count: i64,
    /// Up to three covers fanned out.
    pub fan: Vec<String>,
}

pub fn series_pane(pane: &HomeSeriesPane) -> SeriesPane {
    let year = match (pane.year_from, pane.year_to) {
        (a, b) if a > 0 && b > 0 && a == b => Some(a.to_string()),
        (a, b) if a > 0 && b > 0 => Some(format!("{a}-{b}")),
        _ => None,
    };
    let mut facts = vec![
        format!("{} 张关联专辑", pane.album_count),
        format!("{} 首关联歌曲", pane.track_total),
    ];
    facts.extend(year);
    SeriesPane {
        is_series: pane.is_series,
        series_id: pane.series_id,
        first_work_id: pane.works.first().map_or(0, |w| w.work.id),
        title: pane.title.clone(),
        by: format!(
            "{} · {} 部作品",
            if pane.is_series { "系列" } else { "作品" },
            pane.works.len()
        ),
        facts,
        works: pane
            .works
            .iter()
            .map(|w| {
                (
                    w.work.id,
                    w.work.title.clone(),
                    type_year(&w.work.kind, w.work.year, false),
                )
            })
            .collect(),
        albums: pane.albums.iter().map(spotlight_album).collect(),
        album_count: pane.album_count,
        fan: pane
            .albums
            .iter()
            .map(|e| e.album.artwork_url.clone())
            .filter(|u| !u.is_empty())
            .take(3)
            .collect(),
    }
}

fn spotlight_album(entry: &SeriesAlbumEntry) -> SpotlightAlbum {
    SpotlightAlbum {
        id: entry.album.album_id,
        title: entry.album.title.clone(),
        subtitle: format!("{} · {} 首", entry.work_title, entry.album.track_count),
        year: if entry.album.year > 0 {
            entry.album.year.to_string()
        } else {
            String::new()
        },
        artwork: entry.album.artwork_url.clone(),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spotlight {
    pub tabs: Vec<SpotlightTab>,
    pub default_tab: SpotlightTab,
    pub albums: Vec<AlbumPane>,
    pub series: Option<SeriesPane>,
}

/// A page of a daily rotation as shown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rotation<T> {
    pub kind: String,
    pub batch: u32,
    pub pages: u32,
    pub count: i64,
    pub items: Vec<T>,
}

pub fn rotation<A, T>(page: &RotationPage<A>, map: impl Fn(&A) -> T) -> Rotation<T> {
    Rotation {
        kind: if page.kind.is_empty() {
            "all".into()
        } else {
            page.kind.clone()
        },
        batch: page.batch,
        pages: page.pages,
        count: page.count,
        items: page.items.iter().map(map).collect(),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct YearBar {
    pub year: i32,
    pub count: i64,
    /// Bar height, 3..=100 (% of the busiest year).
    pub height: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChronicleView {
    pub years: Vec<YearBar>,
    pub year: i32,
    pub total: i64,
    pub albums: Vec<AlbumItem>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub albums: i64,
    pub tracks: i64,
    pub works: i64,
    /// "上次扫描 …" or empty.
    pub last_scan: String,
    pub pending_review: i64,
}

/// The 032 home. Track rows are the lists `Spotlight(tab)`, `HomeResume`
/// and `HomeSongs`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeView {
    pub empty: bool,
    pub spotlight: Option<Spotlight>,
    pub resume: Vec<RecordItem>,
    pub recent_albums: Vec<AlbumItem>,
    pub works: Rotation<WorkItem>,
    pub songs: Rotation<SongItem>,
    pub chronicle: Option<ChronicleView>,
    pub playlists: Vec<PlaylistItem>,
    pub stats: Stats,
}

/// Rows of the 接着听 block (the newest two).
pub const RESUME_SHOWN: usize = 2;

pub fn chronicle_view(years: &[api::YearCount], result: &api::Chronicle) -> ChronicleView {
    let max = years.iter().map(|y| y.count).max().unwrap_or(1).max(1);
    ChronicleView {
        years: years
            .iter()
            .map(|y| YearBar {
                year: y.year,
                count: y.count,
                height: i32::try_from((y.count * 100 + max / 2) / max)
                    .unwrap_or(100)
                    .clamp(3, 100),
            })
            .collect(),
        year: result.year,
        total: result.total,
        albums: result.albums.iter().map(album_item).collect(),
    }
}

pub fn home_view(home: &Home) -> HomeView {
    let spotlight = home.spotlight.as_ref().map(|s| {
        let tabs: Vec<SpotlightTab> = s
            .tabs
            .iter()
            .filter_map(|t| SpotlightTab::parse(t))
            .collect();
        let albums = [
            (SpotlightTab::Recent, &s.recent),
            (SpotlightTab::Listening, &s.listening),
            (SpotlightTab::Random, &s.random),
        ]
        .into_iter()
        .filter_map(|(tab, pane)| pane.as_ref().map(|p| album_pane(tab, p)))
        .collect();
        Spotlight {
            tabs,
            default_tab: SpotlightTab::parse(&s.default_tab).unwrap_or_default(),
            albums,
            series: s.series.as_ref().map(series_pane),
        }
    });
    HomeView {
        empty: home.empty,
        spotlight,
        resume: home
            .resume
            .iter()
            .take(RESUME_SHOWN)
            .map(record_item)
            .collect(),
        recent_albums: home.recent_albums.iter().map(album_item).collect(),
        works: rotation(&home.works, work_card_item),
        songs: rotation(&home.songs, song_item),
        chronicle: home
            .chronicle
            .as_ref()
            .map(|c| chronicle_view(&c.years, &c.result)),
        playlists: home.playlists.iter().map(playlist_item).collect(),
        stats: Stats {
            albums: home.stats.albums,
            tracks: home.stats.tracks,
            works: home.stats.works,
            last_scan: if home.stats.last_scan_at.is_empty() {
                String::new()
            } else {
                format!("上次扫描 {}", format::date_time(&home.stats.last_scan_at))
            },
            pending_review: home.stats.pending_review_count,
        },
    }
}

/// Recently played rows shown under (or instead of) 接着听: played tracks
/// only, without those 接着听 already lists.
pub fn recent_rows(tracks: &[Track], exclude: &[i64], count: usize) -> Vec<Track> {
    tracks
        .iter()
        .filter(|t| {
            t.last_played_at.as_deref().is_some_and(|p| !p.is_empty()) && !exclude.contains(&t.id)
        })
        .take(count)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------- details

/// An album linked to a work, with its track-level uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkAlbum {
    pub album_id: i64,
    pub title: String,
    pub artist: String,
    pub artwork: String,
    /// "2011 · 原声 · 自动".
    pub meta: String,
    pub relation: String,
    /// (track title, "OP1", source label).
    pub usage: Vec<(String, String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkView {
    pub id: i64,
    pub title: String,
    pub translated: String,
    pub reading: String,
    /// Type, year, "N 张整张关联专辑", "N 首曲目".
    pub facts: Vec<String>,
    pub poster: String,
    pub bangumi_url: String,
    pub series_id: i64,
    pub series_title: String,
    /// "共 4 部作品 · 当前：第 2 部".
    pub series_meta: String,
    pub albums: Vec<WorkAlbum>,
    /// Rows of the "收录于精选集 / 原创专辑" list (`TrackList::Detail`).
    pub collected: usize,
}

/// The work page: linked albums with their track uses, and the tracks
/// collected on other albums (returned separately, with role tags).
pub fn work_view(
    work: &WorkDetail,
    albums: &[AlbumWork],
    tracks: &[WorkTrack],
) -> (WorkView, Vec<Track>, Vec<String>) {
    let linked: std::collections::HashSet<i64> = albums.iter().map(|a| a.album_id).collect();
    let views = albums
        .iter()
        .map(|a| {
            let mut meta = Vec::new();
            if a.year > 0 {
                meta.push(a.year.to_string());
            }
            meta.push(album_type_label(&a.album_type).to_owned());
            meta.push(source_label(&a.source).to_owned());
            WorkAlbum {
                album_id: a.album_id,
                title: a.title.clone(),
                artist: a.artist.clone(),
                artwork: a.artwork_url.clone(),
                meta: meta.join(" · "),
                relation: album_relation_label(a),
                usage: tracks
                    .iter()
                    .filter(|t| t.track.album_id == a.album_id && t.source != "album")
                    .map(|t| {
                        (
                            t.track.title.clone(),
                            role_sequence_label(&t.role, t.sequence),
                            source_label(&t.source).to_owned(),
                        )
                    })
                    .collect(),
            }
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    let collected: Vec<&WorkTrack> = tracks
        .iter()
        .filter(|t| !linked.contains(&t.track.album_id) && seen.insert(t.track.id))
        .collect();
    let w = &work.work;
    let mut facts = Vec::new();
    if !w.kind.is_empty() {
        facts.push(work_type_long_label(&w.kind).to_owned());
    }
    if w.year > 0 {
        facts.push(w.year.to_string());
    }
    facts.push(format!("{} 张整张关联专辑", albums.len()));
    facts.push(format!("{} 首曲目", w.track_count));
    let view = WorkView {
        id: w.id,
        title: w.title.clone(),
        translated: w.translated_title.clone(),
        reading: w.reading_title.clone(),
        facts,
        poster: work_poster_path(
            w.id,
            if work.poster_api_url.is_empty() {
                &w.poster_url
            } else {
                &work.poster_api_url
            },
        ),
        bangumi_url: if work.bangumi_subject_id.is_empty() {
            String::new()
        } else {
            format!("https://bgm.tv/subject/{}", work.bangumi_subject_id)
        },
        series_id: work.series.as_ref().map_or(0, |s| s.id),
        series_title: work
            .series
            .as_ref()
            .map(|s| s.title.clone())
            .unwrap_or_default(),
        series_meta: work
            .series
            .as_ref()
            .map(|s| format!("共 {} 部作品 · 当前：第 {} 部", s.member_count, s.position))
            .unwrap_or_default(),
        albums: views,
        collected: collected.len(),
    };
    let tags = collected
        .iter()
        .map(|t| role_sequence_label(&t.role, t.sequence))
        .collect();
    (
        view,
        collected.into_iter().map(|t| t.track.clone()).collect(),
        tags,
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeriesAlbum {
    pub album_id: i64,
    pub title: String,
    pub artist: String,
    pub artwork: String,
    /// "Fate/stay night · 2004".
    pub meta: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeriesView {
    pub id: i64,
    pub title: String,
    pub renamed: bool,
    pub facts: Vec<String>,
    pub poster: String,
    /// Members grouped by type in member order; the label is empty when
    /// there is only one type.
    pub groups: Vec<(String, Vec<WorkItem>)>,
    pub member_count: usize,
    pub albums: Vec<SeriesAlbum>,
}

pub fn series_view(detail: &SeriesDetail, albums: &[SeriesAlbumEntry]) -> SeriesView {
    let mut groups: Vec<(String, Vec<WorkItem>)> = Vec::new();
    let mut kinds: Vec<&str> = Vec::new();
    for member in &detail.members {
        let item = member_item(member);
        match kinds.iter().position(|k| *k == member.work_type) {
            Some(i) => groups[i].1.push(item),
            None => {
                kinds.push(&member.work_type);
                groups.push((
                    work_type_long_label(&member.work_type).to_owned(),
                    vec![item],
                ));
            }
        }
    }
    if groups.len() == 1 {
        groups[0].0.clear();
    }
    let s = &detail.series;
    let stats = &detail.stats;
    let mut facts = vec![format!(
        "{} 部作品",
        if stats.member_count > 0 {
            stats.member_count
        } else {
            s.member_count
        }
    )];
    if stats.year_from > 0 {
        facts.push(format!("{} - {}", stats.year_from, stats.year_to));
    }
    facts.push(format!(
        "{} 张专辑 · {} 首曲目",
        stats.album_count, stats.track_count
    ));
    SeriesView {
        id: s.id,
        title: s.title.clone(),
        renamed: s.title_source == "manual",
        facts,
        poster: series_item(s).poster,
        groups,
        member_count: detail.members.len(),
        albums: albums
            .iter()
            .map(|e| SeriesAlbum {
                album_id: e.album.album_id,
                title: e.album.title.clone(),
                artist: e.album.artist.clone(),
                artwork: e.album.artwork_url.clone(),
                meta: if e.album.year > 0 {
                    format!("{} · {}", e.work_title, e.album.year)
                } else {
                    e.work_title.clone()
                },
            })
            .collect(),
    }
}

/// One row of a daily mix.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixRow {
    pub position: i64,
    pub explore: bool,
    pub blocked: bool,
    pub missing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixView {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub theme: String,
    /// "今日推荐" / "2026-10-27 的推荐" / "… · 已被替换".
    pub context: String,
    /// (label, is a style tag).
    pub tags: Vec<(String, bool)>,
    pub subtitle: String,
    pub covers: Vec<String>,
    pub rating: i32,
    pub reasons: Vec<String>,
    /// "已记录：太重复。再点一次可撤销。" when disliked.
    pub disliked_note: String,
    /// Today's active mix: can be replaced.
    pub current: bool,
    pub replacements_left: i64,
    pub saved_playlist_id: i64,
    pub rows: Vec<MixRow>,
    pub others: Vec<MixItem>,
}

/// The daily mix page; `list` is today's mixes (for "换一个" and the others).
pub fn mix_view(detail: &DailyMixDetail, list: Option<&api::DailyMixList>) -> MixView {
    let mix = &detail.mix;
    let current = list.is_some_and(|l| mix.status == "active" && mix.day == l.day);
    let context = if current {
        "今日推荐".to_owned()
    } else if mix.status == "replaced" {
        format!("{} 的推荐 · 已被替换", mix.day)
    } else {
        format!("{} 的推荐", mix.day)
    };
    let reasons: Vec<&str> = mix
        .rating_reasons
        .iter()
        .map(|r| mix_reason_label(r))
        .collect();
    MixView {
        id: mix.id,
        title: mix.title.clone(),
        description: mix.description.clone(),
        theme: mix.theme_type_label.clone(),
        context,
        tags: mix
            .tags
            .iter()
            .map(|t| (t.label.clone(), t.kind == "style"))
            .collect(),
        subtitle: mix_subtitle(mix),
        covers: mix_item(mix).covers,
        rating: mix.rating,
        reasons: mix.rating_reasons.clone(),
        disliked_note: if mix.rating == -1 {
            let text = if reasons.is_empty() {
                "不喜欢".to_owned()
            } else {
                reasons.join("、")
            };
            format!("已记录：{text}。再点一次可撤销。")
        } else {
            String::new()
        },
        current,
        replacements_left: list.map_or(0, |l| l.replacements_left),
        saved_playlist_id: mix.saved_playlist_id.unwrap_or(0),
        rows: detail
            .tracks
            .iter()
            .map(|t| MixRow {
                position: t.position,
                explore: t.bucket == "explore",
                blocked: t.blocked,
                missing: t.track.missing,
            })
            .collect(),
        others: if current {
            list.map(|l| {
                l.items
                    .iter()
                    .filter(|m| m.id != mix.id)
                    .map(mix_item)
                    .collect()
            })
            .unwrap_or_default()
        } else {
            Vec::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use api::{DailyMixTag, DailyMixTrack, PlayRef, SeriesStats};

    use super::*;

    #[test]
    fn relation_labels_match_the_web() {
        let a = |role: &str, roles: &[&str], kind: &str| AlbumWork {
            album_role: role.into(),
            track_roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            album_type: kind.into(),
            ..AlbumWork::default()
        };
        assert_eq!(
            album_relation_label(&a("ost", &[], "soundtrack")),
            "原声集 OST"
        );
        assert_eq!(album_relation_label(&a("ost", &["ost"], "")), "原声集 OST");
        assert_eq!(
            album_relation_label(&a("ost", &["op", "ed"], "soundtrack")),
            "OST · OP · ED 原声"
        );
        assert_eq!(
            album_relation_label(&a("other", &["op"], "single")),
            "OP 单曲"
        );
        assert_eq!(
            album_relation_label(&a("other", &["other"], "")),
            "相关专辑"
        );
        assert_eq!(role_sequence_label("op", 2), "OP2");
        assert_eq!(role_sequence_label("insert", 0), "插曲");
        assert_eq!(role_label("ost"), "OST");
        assert_eq!(mix_duration(82 * 60_000), "1 小时 22 分");
        assert_eq!(mix_duration(48 * 60_000 + 59_000), "48 分钟");
    }

    #[test]
    fn work_page_splits_linked_albums_and_collected_tracks() {
        let wt = |id, album_id, role: &str, seq, source: &str| WorkTrack {
            track: Track {
                id,
                album_id,
                title: format!("t{id}"),
                ..Track::default()
            },
            role: role.into(),
            sequence: seq,
            source: source.into(),
            ..WorkTrack::default()
        };
        let work = WorkDetail {
            work: Work {
                id: 15,
                kind: "anime".into(),
                year: 2011,
                track_count: 4,
                ..Work::default()
            },
            poster_api_url: "/api/v1/works/15/poster?v=1".into(),
            bangumi_subject_id: "10380".into(),
            series: Some(api::WorkSeriesRef {
                id: 7,
                title: "Fate".into(),
                member_count: 4,
                position: 2,
                ..api::WorkSeriesRef::default()
            }),
            ..WorkDetail::default()
        };
        let albums = [AlbumWork {
            album_id: 5,
            album_role: "ost".into(),
            album_type: "soundtrack".into(),
            year: 2011,
            source: "auto".into(),
            ..AlbumWork::default()
        }];
        let tracks = [
            wt(1, 5, "ost", 0, "album"),
            wt(2, 5, "op", 1, "bangumi"),
            wt(3, 9, "ed", 2, "manual"),
            wt(3, 9, "insert", 0, "auto"),
        ];
        let (view, collected, tags) = work_view(&work, &albums, &tracks);
        assert_eq!(view.facts, ["动画", "2011", "1 张整张关联专辑", "4 首曲目"]);
        assert_eq!(view.poster, "/api/v1/works/15/poster?v=1");
        assert_eq!(view.bangumi_url, "https://bgm.tv/subject/10380");
        assert_eq!(view.series_meta, "共 4 部作品 · 当前：第 2 部");
        assert_eq!(view.albums[0].meta, "2011 · 原声 · 自动");
        assert_eq!(
            view.albums[0].usage,
            [("t2".to_owned(), "OP1".to_owned(), "Bangumi".to_owned())]
        );
        assert_eq!(collected.iter().map(|t| t.id).collect::<Vec<_>>(), [3]);
        assert_eq!(tags, ["ED2"]);
    }

    #[test]
    fn series_members_group_by_type_in_order() {
        let m = |id, kind: &str| SeriesMember {
            id,
            work_type: kind.into(),
            play: PlayRef::default(),
            ..SeriesMember::default()
        };
        let detail = SeriesDetail {
            series: Series {
                id: 7,
                title_source: "manual".into(),
                ..Series::default()
            },
            members: vec![m(1, "game"), m(2, "anime"), m(3, "game")],
            stats: SeriesStats {
                member_count: 3,
                year_from: 2004,
                year_to: 2011,
                album_count: 5,
                track_count: 26,
            },
            ..SeriesDetail::default()
        };
        let view = series_view(&detail, &[]);
        assert!(view.renamed);
        assert_eq!(view.groups.len(), 2);
        assert_eq!(view.groups[0].0, "游戏");
        assert_eq!(
            view.groups[0].1.iter().map(|w| w.id).collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(
            view.facts,
            ["3 部作品", "2004 - 2011", "5 张专辑 · 26 首曲目"]
        );
        let single = SeriesDetail {
            members: vec![m(1, "anime")],
            ..detail
        };
        assert_eq!(series_view(&single, &[]).groups[0].0, "");
    }

    #[test]
    fn mix_page_states() {
        let mix = DailyMix {
            id: 4,
            day: "2026-10-28".into(),
            status: "active".into(),
            tags: vec![DailyMixTag {
                kind: "style".into(),
                tag: "pop".into(),
                label: "流行".into(),
            }],
            track_count: 2,
            duration_millis: 8 * 60_000,
            rating: -1,
            rating_reasons: vec!["theme".into(), "flow".into()],
            cover_urls: vec!["a".into(), "b".into()],
            ..DailyMix::default()
        };
        let detail = DailyMixDetail {
            mix: mix.clone(),
            tracks: vec![DailyMixTrack {
                position: 0,
                bucket: "explore".into(),
                ..DailyMixTrack::default()
            }],
        };
        let other = DailyMix {
            id: 5,
            ..mix.clone()
        };
        let list = api::DailyMixList {
            day: "2026-10-28".into(),
            replacements_left: 2,
            items: vec![mix, other],
            ..api::DailyMixList::default()
        };
        let view = mix_view(&detail, Some(&list));
        assert!(view.current);
        assert_eq!(view.context, "今日推荐");
        assert_eq!(view.subtitle, "流行 · 2 首 · 8 分钟");
        assert_eq!(view.covers, ["a"]);
        assert_eq!(
            view.disliked_note,
            "已记录：不喜欢这个主题、风格太跳。再点一次可撤销。"
        );
        assert!(view.rows[0].explore);
        assert_eq!(view.others.len(), 1);
        let past = mix_view(&detail, None);
        assert!(!past.current && past.others.is_empty());
        assert_eq!(past.context, "2026-10-28 的推荐");
    }

    #[test]
    fn chronicle_bars_and_recent_rows() {
        let years = [
            api::YearCount {
                year: 2019,
                count: 1,
            },
            api::YearCount {
                year: 2023,
                count: 40,
            },
        ];
        let view = chronicle_view(&years, &api::Chronicle::default());
        assert_eq!(view.years[0].height, 3);
        assert_eq!(view.years[1].height, 100);
        let t = |id, played: bool| Track {
            id,
            last_played_at: played.then(|| "2026-10-28T00:00:00Z".to_owned()),
            ..Track::default()
        };
        let rows = recent_rows(&[t(1, true), t(2, false), t(3, true), t(4, true)], &[3], 2);
        assert_eq!(rows.iter().map(|t| t.id).collect::<Vec<_>>(), [1, 4]);
    }
}
