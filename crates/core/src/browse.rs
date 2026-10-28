//! Library browsing: list filters, rows and detail views for the UI, and
//! the client-side filtering / sorting of favorites and playlists
//! (`favoriteAlbums` / `favoriteSongs` / `sortPlaylists` in the Electron
//! controller: `/favorites/*` and `/playlists` only page).

use std::cmp::Ordering;
use std::collections::HashSet;

use api::{Album, AlbumSort, Artist, ArtistSort, Playlist, SortOrder, Track, TrackSort};

use crate::format;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumFilter {
    pub sort: AlbumSort,
    pub order: SortOrder,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub artist_id: Option<i64>,
    pub search: String,
}

impl Default for AlbumFilter {
    fn default() -> Self {
        Self {
            sort: AlbumSort::Added,
            order: SortOrder::Desc,
            year_from: None,
            year_to: None,
            artist_id: None,
            search: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackFilter {
    pub sort: TrackSort,
    pub order: SortOrder,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub artist_id: Option<i64>,
    pub search: String,
}

impl Default for TrackFilter {
    fn default() -> Self {
        Self {
            sort: TrackSort::Added,
            order: SortOrder::Desc,
            year_from: None,
            year_to: None,
            artist_id: None,
            search: String::new(),
        }
    }
}

impl TrackFilter {
    /// The tracks page at sign-in: newest first, or by title on servers
    /// without `/browse` (they cannot sort by date added).
    pub fn initial(browse: bool) -> Self {
        if browse {
            Self::default()
        } else {
            Self {
                sort: TrackSort::Title,
                order: SortOrder::Asc,
                ..Self::default()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistFilter {
    pub sort: ArtistSort,
    pub order: SortOrder,
    pub search: String,
    /// Everyone credited on a track (`role=all`: performers, composers,
    /// lyricists, arrangers, producers) instead of album artists only
    /// (Electron 「艺术家」 / 「专辑艺术家」).
    pub all_roles: bool,
}

impl Default for ArtistFilter {
    fn default() -> Self {
        Self {
            sort: ArtistSort::Name,
            order: SortOrder::Asc,
            search: String::new(),
            all_roles: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaylistSort {
    #[default]
    Name,
    Duration,
    TrackCount,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistFilter {
    pub sort: PlaylistSort,
    pub order: SortOrder,
    pub search: String,
}

/// Track lists the UI can play from or queue out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackList {
    /// The tracks page.
    Library,
    /// Favorite tracks.
    Favorites,
    /// The open album / artist (top tracks) / playlist / similar page.
    Detail,
    /// Search results.
    Search,
    /// Preview tracks of a home spotlight album tab.
    Spotlight(crate::m032::SpotlightTab),
    /// 接着听 on the home page (each row resumes at its position).
    HomeResume,
    /// 最近播放 on the home page.
    HomeRecent,
    /// 主题曲与插曲 on the home page.
    HomeSongs,
    /// The playback history page.
    History,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlbumItem {
    pub id: i64,
    pub title: String,
    pub artist: String,
    /// First credited album artist (0: none).
    pub artist_id: i64,
    pub year: i32,
    pub artwork: String,
    pub favorite: bool,
    /// On an artist page: own / collaboration / appearance / credit.
    pub relation: String,
    /// `releaseKind` (manual edits and inference folded in), else
    /// `albumType`; lower case, empty when unknown.
    pub kind: String,
    /// When it entered the library (ISO time, may be empty).
    pub added_at: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackItem {
    pub id: i64,
    pub title: String,
    pub artist: String,
    /// First track artist, else album artist (0: none).
    pub artist_id: i64,
    pub album: String,
    pub album_id: i64,
    pub duration: String,
    pub disc_number: i32,
    pub track_number: i32,
    pub favorite: bool,
    /// Extra label of the row (the use on a work page, e.g. "OP1").
    pub tag: String,
    /// "作词 A · 作曲 B · 编曲 C · 制作 D" in pieces (album pages show it
    /// under the title, like the 032 web album page; names link to artists).
    pub credits: Vec<CreditPart>,
}

/// A piece of a credits line: a label / separator (`artist_id` 0) or a name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditPart {
    pub text: String,
    /// The credited artist; 0 for labels, separators and names known only
    /// from the file tags.
    pub artist_id: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtistItem {
    pub id: i64,
    pub name: String,
    pub image: String,
    pub album_count: i64,
    pub track_count: i64,
    pub favorite: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistItem {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub track_count: i64,
    /// Hidden when the server does not report durations.
    pub duration: Option<String>,
    pub artwork: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlbumView {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub artist_id: i64,
    pub year: i32,
    pub artwork: String,
    pub track_count: i64,
    pub duration: String,
    pub quality: Option<String>,
    pub genres: String,
    pub label: String,
    pub release_date: String,
    pub favorite: bool,
    /// Several discs: the track list shows disc numbers.
    pub multi_disc: bool,
    /// Inspector "发行信息" (label, value): 类型, 演出者, 发行日期, ...
    pub release_facts: Vec<(String, String)>,
    /// Inspector "媒体信息": 格式, 容量.
    pub media_facts: Vec<(String, String)>,
    pub people: Vec<AlbumPerson>,
    /// Release kind label ("专辑", "单曲", ...; empty when unknown).
    pub kind: String,
    /// The album review / comment.
    pub review: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtistView {
    pub id: i64,
    pub name: String,
    pub image: String,
    pub album_count: i64,
    pub track_count: i64,
    pub favorite: bool,
    pub biography: String,
    /// Where the biography comes from (Last.fm, ...).
    pub biography_source: String,
    /// "个人" / "组合" ... and the country, aliases.
    pub artist_type: String,
    pub country: String,
    pub aliases: Vec<String>,
    pub releases: Vec<AlbumItem>,
    /// Top tracks are the most played (else the first in album order).
    pub top_tracks_popular: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistView {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub track_count: i64,
    pub duration: Option<String>,
    pub artwork: String,
    pub has_custom_artwork: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DetailView {
    Album(AlbumView),
    Artist(ArtistView),
    Playlist(PlaylistView),
    /// Tracks similar to `title`.
    Similar {
        track_id: i64,
        title: String,
    },
    /// Tracks: those collected on other albums, tagged with their use.
    Work(crate::m032::WorkView),
    /// No tracks.
    Series(crate::m032::SeriesView),
    /// Tracks: every row of the mix (see `rows`).
    DailyMix(crate::m032::MixView),
}

// ---------------------------------------------------------------- rows

pub fn album_item(album: &Album) -> AlbumItem {
    AlbumItem {
        id: album.id,
        title: album.title.clone(),
        artist: album.artist.clone(),
        artist_id: album.artists.first().map_or(0, |a| a.id),
        year: album.year,
        artwork: album.artwork_url.clone(),
        favorite: album.is_favorite,
        relation: String::new(),
        kind: if album.release_kind.trim().is_empty() {
            &album.album_type
        } else {
            &album.release_kind
        }
        .trim()
        .to_lowercase(),
        added_at: album.added_at.clone(),
    }
}

/// A titled group of an artist's releases.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReleaseGroup {
    pub title: &'static str,
    pub items: Vec<AlbumItem>,
}

/// Groups of the artist page in the 032 server's order
/// (`groupArtistDiscography` in `internal/http/identity.go`, Electron
/// `MUSIC032_GROUPS`): the artist's own releases by kind, then
/// collaborations, appearances and releases reached only through credits
/// (composer, lyricist, ...). Empty groups are left out.
pub fn release_groups(releases: &[AlbumItem]) -> Vec<ReleaseGroup> {
    const TITLES: [&str; 7] = [
        "专辑",
        "单曲与 EP",
        "合辑与现场",
        "其他发行",
        "合作发行",
        "参与作品",
        "幕后参与",
    ];
    let mut groups: Vec<Vec<AlbumItem>> = vec![Vec::new(); TITLES.len()];
    for album in releases {
        let slot = match album.relation.as_str() {
            "collaboration" => 4,
            "appearance" => 5,
            "credit" => 6,
            _ => match album.kind.as_str() {
                "single" | "ep" => 1,
                "compilation" | "live" => 2,
                "soundtrack" | "bootleg" | "other" => 3,
                _ => 0,
            },
        };
        groups[slot].push(album.clone());
    }
    TITLES
        .iter()
        .zip(groups)
        .filter(|(_, items)| !items.is_empty())
        .map(|(title, items)| ReleaseGroup { title, items })
        .collect()
}

pub fn track_item(track: &Track) -> TrackItem {
    TrackItem {
        id: track.id,
        title: track.title.clone(),
        artist: track.artist.clone(),
        artist_id: track
            .artists
            .first()
            .or(track.album_artists.first())
            .map_or(0, |a| a.id),
        album: track.album.clone(),
        album_id: track.album_id,
        duration: format::duration(track.duration_millis),
        disc_number: track.disc_number,
        track_number: track.track_number,
        favorite: track.is_favorite,
        tag: String::new(),
        credits: credit_parts(track),
    }
}

/// Credit roles in the order of the web template "track-credits"
/// (album.html), with the plain tag text as fallback.
const CREDIT_ROLES: [(&str, &str); 4] = [
    ("lyricist", "作词"),
    ("composer", "作曲"),
    ("arranger", "编曲"),
    ("producer", "制作"),
];

/// "作词 A · 作曲 B, C · 编曲 D" in pieces; credited artists carry their
/// id, names from the file tags (no artist record) do not.
pub fn credit_parts(track: &Track) -> Vec<CreditPart> {
    let part = |text: &str, artist_id: i64| CreditPart {
        text: text.to_owned(),
        artist_id,
    };
    let mut parts = Vec::new();
    for (role, label) in CREDIT_ROLES {
        let names: Vec<(&str, i64)> = match track.credits.iter().find(|c| c.role == *role) {
            Some(c) if !c.artists.is_empty() => {
                c.artists.iter().map(|a| (a.name.as_str(), a.id)).collect()
            }
            _ => {
                let tag = match role {
                    "lyricist" => track.lyricist.trim(),
                    "composer" => track.composer.trim(),
                    "arranger" => track.arranger.trim(),
                    _ => "",
                };
                if tag.is_empty() {
                    vec![]
                } else {
                    vec![(tag, 0)]
                }
            }
        };
        if names.is_empty() {
            continue;
        }
        if !parts.is_empty() {
            parts.push(part(" · ", 0));
        }
        parts.push(part(&format!("{label} "), 0));
        for (i, (name, id)) in names.into_iter().enumerate() {
            if i > 0 {
                parts.push(part(", ", 0));
            }
            parts.push(part(name, id));
        }
    }
    parts
}

/// Someone performing on or credited on an album's tracks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumPerson {
    pub id: i64,
    pub name: String,
    /// "演唱 · 作曲".
    pub roles: String,
    pub track_count: usize,
}

/// `albumPeople` of the 032 server (`internal/http/album_people.go`):
/// performers and credited people, most tracks first, performers first on
/// ties, then first appearance.
pub fn album_people(tracks: &[Track]) -> Vec<AlbumPerson> {
    struct Entry {
        id: i64,
        name: String,
        roles: Vec<&'static str>,
        performer: bool,
        tracks: std::collections::HashSet<i64>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut add = |id: i64, name: &str, role: &'static str, track: i64| {
        if id == 0 || name.trim().is_empty() {
            return;
        }
        let i = match entries.iter().position(|e| e.id == id) {
            Some(i) => i,
            None => {
                entries.push(Entry {
                    id,
                    name: name.to_owned(),
                    roles: Vec::new(),
                    performer: false,
                    tracks: std::collections::HashSet::new(),
                });
                entries.len() - 1
            }
        };
        let e = &mut entries[i];
        if role == "演唱" {
            e.performer = true;
        }
        if !e.roles.contains(&role) {
            e.roles.push(role);
        }
        e.tracks.insert(track);
    };
    for track in tracks {
        for a in &track.artists {
            add(a.id, &a.name, "演唱", track.id);
        }
        for (role, label) in [
            ("composer", "作曲"),
            ("lyricist", "作词"),
            ("arranger", "编曲"),
            ("producer", "制作"),
        ] {
            if let Some(credit) = track.credits.iter().find(|c| c.role == role) {
                for a in &credit.artists {
                    add(a.id, &a.name, label, track.id);
                }
            }
        }
    }
    // Stable sort keeps the first-appearance order on ties.
    entries.sort_by(|a, b| {
        b.tracks
            .len()
            .cmp(&a.tracks.len())
            .then(b.performer.cmp(&a.performer))
    });
    entries
        .into_iter()
        .map(|e| AlbumPerson {
            id: e.id,
            name: e.name,
            roles: e.roles.join(" · "),
            track_count: e.tracks.len(),
        })
        .collect()
}

/// `albumSpecLabel` of the 032 server (`internal/http/audio_spec.go`):
/// "FLAC / 24bit / 96kHz"; mixed albums show ranges ("16–24bit").
pub fn album_spec_label(tracks: &[Track], fallback: &str) -> String {
    let mut codecs: Vec<String> = Vec::new();
    let (mut depth, mut rate) = ((0, 0), (0, 0));
    let widen = |range: &mut (i32, i32), v: i32| {
        if v > 0 {
            range.0 = if range.0 == 0 { v } else { range.0.min(v) };
            range.1 = range.1.max(v);
        }
    };
    for t in tracks {
        let codec = t
            .codec
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or(&t.container)
            .trim()
            .to_uppercase();
        if !codec.is_empty() && !codecs.contains(&codec) {
            codecs.push(codec);
        }
        widen(&mut depth, t.bit_depth.unwrap_or(0));
        widen(&mut rate, t.sample_rate.unwrap_or(0));
    }
    let range = |(lo, hi): (i32, i32), fmt: &dyn Fn(i32) -> String, unit: &str| {
        if hi <= 0 {
            String::new()
        } else if lo == hi {
            format!("{}{unit}", fmt(hi))
        } else {
            format!("{}–{}{unit}", fmt(lo), fmt(hi))
        }
    };
    let khz = |hz: i32| {
        let v = f64::from(hz) / 1000.0;
        if v.fract() == 0.0 {
            format!("{v:.0}")
        } else {
            format!("{v}")
        }
    };
    let label = [
        codecs.join("/"),
        range(depth, &|d| d.to_string(), "bit"),
        range(rate, &khz, "kHz"),
    ]
    .into_iter()
    .filter(|p| !p.is_empty())
    .collect::<Vec<_>>()
    .join(" / ");
    if label.is_empty() {
        fallback.to_owned()
    } else {
        label
    }
}

pub fn artist_item(artist: &Artist) -> ArtistItem {
    ArtistItem {
        id: artist.id,
        name: artist.name.clone(),
        image: artist.image_url.clone(),
        album_count: artist.album_count,
        track_count: artist.track_count,
        favorite: artist.is_favorite,
    }
}

pub fn playlist_item(playlist: &Playlist) -> PlaylistItem {
    PlaylistItem {
        id: playlist.id,
        name: playlist.name.clone(),
        description: playlist.description.clone(),
        track_count: playlist.item_count,
        duration: playlist.duration_millis.map(format::total_duration),
        artwork: playlist.artwork_url.clone(),
    }
}

pub fn album_view(album: &Album, tracks: &[Track]) -> AlbumView {
    AlbumView {
        id: album.id,
        title: album.title.clone(),
        artist: album.artist.clone(),
        artist_id: album.artists.first().map_or(0, |a| a.id),
        year: album.year,
        artwork: album.artwork_url.clone(),
        track_count: album
            .track_count
            .max(i64::try_from(tracks.len()).unwrap_or(0)),
        duration: format::total_duration(tracks.iter().map(|t| t.duration_millis.max(0)).sum()),
        quality: format::album_quality(tracks),
        genres: album.genres.clone(),
        label: album.label.clone(),
        release_date: album.release_date.clone(),
        favorite: album.is_favorite,
        multi_disc: album.disc_count > 1 || tracks.iter().any(|t| t.disc_number > 1),
        release_facts: release_facts(album),
        media_facts: {
            let mut facts = Vec::new();
            let spec = album_spec_label(tracks, &album.formats);
            if !spec.is_empty() {
                facts.push(("格式".to_owned(), spec));
            }
            if !album.total_size.is_empty() {
                facts.push(("容量".to_owned(), album.total_size.clone()));
            }
            facts
        },
        people: album_people(tracks),
        kind: {
            let kind = album_item(album).kind;
            if kind.is_empty() {
                String::new()
            } else {
                crate::m032::album_type_label(&kind).to_owned()
            }
        },
        review: album.review.trim().to_owned(),
    }
}

/// "发行信息" of the album inspector (`Music032AlbumInspector`).
fn release_facts(album: &Album) -> Vec<(String, String)> {
    let mut facts = Vec::new();
    let mut push = |label: &str, value: &str| {
        if !value.trim().is_empty() {
            facts.push((label.to_owned(), value.trim().to_owned()));
        }
    };
    let kind = album_item(album).kind;
    if !kind.is_empty() {
        push("类型", crate::m032::album_type_label(&kind));
    }
    let performers = if album.artists.is_empty() {
        if album.performed_by.is_empty() {
            album.artist.clone()
        } else {
            album.performed_by.clone()
        }
    } else {
        album
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join("、")
    };
    push("演出者", &performers);
    push("发行日期", &album.release_date);
    push("原始发行", &album.original_release_date);
    push("厂牌", &album.label);
    push("目录号", &album.catalog_number);
    push("国家/地区", &album.country);
    push("流派", &album.genres);
    facts
}

pub fn playlist_view(playlist: &Playlist) -> PlaylistView {
    PlaylistView {
        id: playlist.id,
        name: playlist.name.clone(),
        description: playlist.description.clone(),
        track_count: playlist.item_count,
        duration: playlist.duration_millis.map(format::total_duration),
        artwork: playlist.artwork_url.clone(),
        has_custom_artwork: playlist.has_custom_artwork,
    }
}

// ---------------------------------------------------------------- sorting

fn matches_text(needle: &str, haystack: &[&str]) -> bool {
    let needle = needle.trim().to_lowercase();
    needle.is_empty() || haystack.iter().any(|h| h.to_lowercase().contains(&needle))
}

fn in_year_range(year: i32, from: Option<i32>, to: Option<i32>) -> bool {
    from.is_none_or(|f| f <= 0 || year >= f) && to.is_none_or(|t| t <= 0 || year <= t)
}

/// Compares two keys; "missing" keys (empty, 0, never played) sort last in
/// both directions, like the server's browse lists.
fn by_key<K: Ord>(a: Option<K>, b: Option<K>, order: SortOrder) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => match order {
            SortOrder::Asc => a.cmp(&b),
            SortOrder::Desc => b.cmp(&a),
        },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn text(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_lowercase())
}

fn positive(n: i32) -> Option<i32> {
    (n > 0).then_some(n)
}

/// Fisher-Yates with xorshift64* (favorites in random order).
pub(crate) fn shuffle<T>(items: &mut [T], mut seed: u64) {
    seed |= 1;
    for i in (1..items.len()).rev() {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        let r = seed.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let j = usize::try_from(r % (i as u64 + 1)).unwrap_or(0);
        items.swap(i, j);
    }
}

/// Favorite albums filtered and sorted on the client. `artist_albums`:
/// albums of the artist filter (`/albums?artist=`).
pub fn favorite_albums(
    mut items: Vec<Album>,
    filter: &AlbumFilter,
    artist_albums: Option<&HashSet<i64>>,
    seed: u64,
) -> Vec<Album> {
    items.retain(|a| {
        artist_albums.is_none_or(|ids| ids.contains(&a.id))
            && in_year_range(a.year, filter.year_from, filter.year_to)
            && matches_text(&filter.search, &[&a.title, &a.artist])
    });
    let order = filter.order;
    match filter.sort {
        AlbumSort::Random => shuffle(&mut items, seed),
        AlbumSort::Title => items.sort_by(|a, b| by_key(text(&a.title), text(&b.title), order)),
        AlbumSort::Added => {
            items.sort_by(|a, b| by_key(text(&a.added_at), text(&b.added_at), order))
        }
        AlbumSort::RecentlyPlayed => items.sort_by(|a, b| {
            by_key(
                a.last_played_at.as_deref().and_then(text),
                b.last_played_at.as_deref().and_then(text),
                order,
            )
        }),
        AlbumSort::Date => {
            items.sort_by(|a, b| by_key(text(&a.release_date), text(&b.release_date), order))
        }
        AlbumSort::Year => items.sort_by(|a, b| by_key(positive(a.year), positive(b.year), order)),
    }
    items
}

/// Favorite tracks filtered and sorted on the client.
pub fn favorite_tracks(mut items: Vec<Track>, filter: &TrackFilter, seed: u64) -> Vec<Track> {
    items.retain(|t| {
        filter.artist_id.is_none_or(|id| {
            t.artists
                .iter()
                .chain(&t.album_artists)
                .chain(t.credits.iter().flat_map(|c| &c.artists))
                .any(|a| a.id == id)
        }) && in_year_range(t.year, filter.year_from, filter.year_to)
            && matches_text(&filter.search, &[&t.title, &t.album, &t.artist])
    });
    let order = filter.order;
    match filter.sort {
        TrackSort::Random => shuffle(&mut items, seed),
        TrackSort::Title => items.sort_by(|a, b| by_key(text(&a.title), text(&b.title), order)),
        TrackSort::Album => items.sort_by(|a, b| {
            by_key(text(&a.album), text(&b.album), order)
                .then(a.disc_number.cmp(&b.disc_number))
                .then(a.track_number.cmp(&b.track_number))
        }),
        TrackSort::Plays => items.sort_by(|a, b| {
            by_key(
                (a.play_count > 0).then_some(a.play_count),
                (b.play_count > 0).then_some(b.play_count),
                order,
            )
        }),
        TrackSort::RecentlyPlayed => items.sort_by(|a, b| {
            by_key(
                a.last_played_at.as_deref().and_then(text),
                b.last_played_at.as_deref().and_then(text),
                order,
            )
        }),
        TrackSort::Date | TrackSort::Year => {
            items.sort_by(|a, b| by_key(positive(a.year), positive(b.year), order));
        }
        TrackSort::Duration => items.sort_by(|a, b| {
            by_key(
                (a.duration_millis > 0).then_some(a.duration_millis),
                (b.duration_millis > 0).then_some(b.duration_millis),
                order,
            )
        }),
        TrackSort::Added => {
            items.sort_by(|a, b| by_key(text(&a.added_at), text(&b.added_at), order))
        }
    }
    items
}

/// Playlists searched by name and sorted (`sortPlaylists`).
pub fn sort_playlists(mut items: Vec<Playlist>, filter: &PlaylistFilter) -> Vec<Playlist> {
    items.retain(|p| matches_text(&filter.search, &[&p.name]));
    let order = filter.order;
    items.sort_by(|a, b| match filter.sort {
        PlaylistSort::Name => by_key(
            Some(a.name.to_lowercase()),
            Some(b.name.to_lowercase()),
            order,
        ),
        PlaylistSort::Duration => by_key(
            Some(a.duration_millis.unwrap_or(0)),
            Some(b.duration_millis.unwrap_or(0)),
            order,
        ),
        PlaylistSort::TrackCount => by_key(Some(a.item_count), Some(b.item_count), order),
    });
    items
}

#[cfg(test)]
mod tests {
    use api::ArtistRef;

    use super::*;

    fn album(id: i64, title: &str, year: i32, artist: &str) -> Album {
        Album {
            id,
            title: title.into(),
            year,
            artist: artist.into(),
            ..Album::default()
        }
    }

    fn ids<T>(items: &[T], id: impl Fn(&T) -> i64) -> Vec<i64> {
        items.iter().map(id).collect()
    }

    #[test]
    fn favorite_albums_filter_and_sort() {
        let items = vec![
            album(1, "b", 2001, "X"),
            album(2, "A", 0, "Y"),
            album(3, "c", 1999, "X"),
        ];
        let by_year = AlbumFilter {
            sort: AlbumSort::Year,
            order: SortOrder::Asc,
            ..AlbumFilter::default()
        };
        // No year sorts last in both directions.
        let r = favorite_albums(items.clone(), &by_year, None, 1);
        assert_eq!(ids(&r, |a| a.id), [3, 1, 2]);
        let desc = AlbumFilter {
            order: SortOrder::Desc,
            ..by_year.clone()
        };
        let r = favorite_albums(items.clone(), &desc, None, 1);
        assert_eq!(ids(&r, |a| a.id), [1, 3, 2]);

        let title = AlbumFilter {
            sort: AlbumSort::Title,
            order: SortOrder::Asc,
            year_from: Some(2000),
            ..AlbumFilter::default()
        };
        let r = favorite_albums(items.clone(), &title, None, 1);
        assert_eq!(ids(&r, |a| a.id), [1]);

        let search = AlbumFilter {
            sort: AlbumSort::Title,
            order: SortOrder::Asc,
            search: "x".into(),
            ..AlbumFilter::default()
        };
        let only: HashSet<i64> = [3].into();
        let r = favorite_albums(items.clone(), &search, Some(&only), 1);
        assert_eq!(ids(&r, |a| a.id), [3]);

        let random = AlbumFilter {
            sort: AlbumSort::Random,
            ..AlbumFilter::default()
        };
        let mut r = ids(&favorite_albums(items, &random, None, 7), |a| a.id);
        r.sort_unstable();
        assert_eq!(r, [1, 2, 3]);
    }

    #[test]
    fn album_people_and_credits() {
        let r = |id: i64, name: &str| api::ArtistRef {
            id,
            name: name.into(),
        };
        let credit = |role: &str, artists: Vec<api::ArtistRef>| api::TrackCredit {
            role: role.into(),
            artists,
        };
        let tracks = vec![
            Track {
                id: 1,
                artists: vec![r(10, "ikura")],
                credits: vec![credit("composer", vec![r(20, "Ayase")])],
                lyricist: "Ayase".into(),
                ..Track::default()
            },
            Track {
                id: 2,
                artists: vec![r(10, "ikura")],
                credits: vec![
                    credit("composer", vec![r(20, "Ayase")]),
                    credit("arranger", vec![r(30, "A"), r(31, "B")]),
                ],
                ..Track::default()
            },
            Track {
                id: 3,
                artists: vec![r(20, "Ayase")],
                ..Track::default()
            },
        ];
        let line = |t: &Track| {
            credit_parts(t)
                .iter()
                .map(|p| format!("{}{}", p.text, if p.artist_id > 0 { "#" } else { "" }))
                .collect::<String>()
        };
        // Tag-only names do not link; credited artists do.
        assert_eq!(line(&tracks[0]), "作词 Ayase · 作曲 Ayase#");
        assert_eq!(line(&tracks[1]), "作曲 Ayase# · 编曲 A#, B#");
        let people = album_people(&tracks);
        let shape: Vec<(&str, &str, usize)> = people
            .iter()
            .map(|p| (p.name.as_str(), p.roles.as_str(), p.track_count))
            .collect();
        assert_eq!(
            shape,
            [
                ("Ayase", "作曲 · 演唱", 3),
                ("ikura", "演唱", 2),
                ("A", "编曲", 1),
                ("B", "编曲", 1),
            ]
        );
    }

    #[test]
    fn spec_label_shows_ranges() {
        let t = |codec: &str, depth: i32, rate: i32| Track {
            codec: Some(codec.into()),
            bit_depth: Some(depth),
            sample_rate: Some(rate),
            ..Track::default()
        };
        assert_eq!(
            album_spec_label(&[t("flac", 24, 48000)], ""),
            "FLAC / 24bit / 48kHz"
        );
        assert_eq!(
            album_spec_label(&[t("flac", 16, 44100), t("flac", 24, 96000)], ""),
            "FLAC / 16–24bit / 44.1–96kHz"
        );
        assert_eq!(album_spec_label(&[], "FLAC"), "FLAC");
    }

    #[test]
    fn groups_releases_like_the_server() {
        let album = |id: i64, relation: &str, kind: &str| AlbumItem {
            id,
            relation: relation.into(),
            kind: kind.into(),
            ..AlbumItem::default()
        };
        let groups = release_groups(&[
            album(1, "personal", "album"),
            album(2, "personal", "single"),
            album(3, "credit", ""),
            album(4, "personal", ""),
            album(5, "collaboration", "album"),
            album(6, "personal", "soundtrack"),
            album(7, "appearance", "compilation"),
            album(8, "personal", "live"),
            album(9, "personal", "ep"),
        ]);
        let shape: Vec<(&str, Vec<i64>)> = groups
            .iter()
            .map(|g| (g.title, g.items.iter().map(|a| a.id).collect()))
            .collect();
        assert_eq!(
            shape,
            [
                ("专辑", vec![1, 4]),
                ("单曲与 EP", vec![2, 9]),
                ("合辑与现场", vec![8]),
                ("其他发行", vec![6]),
                ("合作发行", vec![5]),
                ("参与作品", vec![7]),
                ("幕后参与", vec![3]),
            ]
        );
    }

    #[test]
    fn favorite_tracks_match_any_artist_role() {
        let t = |id: i64, artists: Vec<i64>, credit: Vec<i64>, title: &str| Track {
            id,
            title: title.into(),
            artists: artists
                .into_iter()
                .map(|id| ArtistRef {
                    id,
                    name: String::new(),
                })
                .collect(),
            credits: vec![api::TrackCredit {
                role: "composer".into(),
                artists: credit
                    .into_iter()
                    .map(|id| ArtistRef {
                        id,
                        name: String::new(),
                    })
                    .collect(),
            }],
            ..Track::default()
        };
        let items = vec![
            t(1, vec![5], vec![], "b"),
            t(2, vec![6], vec![5], "a"),
            t(3, vec![6], vec![], "c"),
        ];
        let filter = TrackFilter {
            sort: TrackSort::Title,
            order: SortOrder::Asc,
            artist_id: Some(5),
            ..TrackFilter::default()
        };
        let r = favorite_tracks(items, &filter, 1);
        assert_eq!(ids(&r, |t| t.id), [2, 1]);
    }

    #[test]
    fn playlists_sort_and_search() {
        let p = |id, name: &str, count, duration| Playlist {
            id,
            name: name.into(),
            item_count: count,
            duration_millis: duration,
            ..Playlist::default()
        };
        let items = vec![
            p(1, "b", 3, Some(10)),
            p(2, "A", 5, None),
            p(3, "ab", 1, Some(99)),
        ];
        let r = sort_playlists(items.clone(), &PlaylistFilter::default());
        assert_eq!(ids(&r, |p| p.id), [2, 3, 1]);
        let r = sort_playlists(
            items.clone(),
            &PlaylistFilter {
                sort: PlaylistSort::TrackCount,
                order: SortOrder::Desc,
                search: "b".into(),
            },
        );
        assert_eq!(ids(&r, |p| p.id), [1, 3]);
        let r = sort_playlists(
            items,
            &PlaylistFilter {
                sort: PlaylistSort::Duration,
                order: SortOrder::Asc,
                search: String::new(),
            },
        );
        assert_eq!(ids(&r, |p| p.id), [2, 1, 3]);
    }

    #[test]
    fn rows_carry_artist_ids() {
        let track = Track {
            album_artists: vec![ArtistRef {
                id: 4,
                name: "AA".into(),
            }],
            duration_millis: 61_000,
            ..Track::default()
        };
        let item = track_item(&track);
        assert_eq!(item.artist_id, 4);
        assert_eq!(item.duration, "1:01");
        let playlist = Playlist {
            duration_millis: None,
            ..Playlist::default()
        };
        assert_eq!(playlist_item(&playlist).duration, None);
    }
}
