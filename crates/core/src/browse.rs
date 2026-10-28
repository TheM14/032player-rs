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
}

impl Default for ArtistFilter {
    fn default() -> Self {
        Self {
            sort: ArtistSort::Name,
            order: SortOrder::Asc,
            search: String::new(),
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
    }
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
    }
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
