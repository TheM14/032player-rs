//! List queries for albums and tracks.
//!
//! Servers with `features.browse` are read through `/browse/*`, which honours
//! `order`, `yearFrom`/`yearTo` and adds `sort=random&seed=`. Legacy
//! `/albums` and `/tracks` have a fixed direction per sort key and ignore
//! sort values they never understood (see `ALBUM_SORT` / `SONG_SORT` in
//! `music032-controller.ts`).

use std::fmt::Display;

use crate::client::MAX_PAGE_SIZE;

/// Ordered query parameters. Empty values are skipped, like `toParams`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query(Vec<(String, String)>);

impl Query {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, key: &str, value: impl Display) -> &mut Self {
        let value = value.to_string();
        if !value.is_empty() {
            self.0.push((key.to_owned(), value));
        }
        self
    }

    pub fn push_opt(&mut self, key: &str, value: Option<impl Display>) -> &mut Self {
        if let Some(value) = value {
            self.push(key, value);
        }
        self
    }

    /// `limit` (clamped to 1..=500) and `offset`.
    pub fn page(&mut self, offset: u32, limit: u32) -> &mut Self {
        self.push("limit", limit.clamp(1, MAX_PAGE_SIZE));
        self.push("offset", offset)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

impl SortOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlbumSort {
    Added,
    RecentlyPlayed,
    Date,
    Year,
    /// Browse only.
    Title,
    /// Browse only.
    Random,
}

impl AlbumSort {
    fn browse(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::RecentlyPlayed => "recentlyPlayed",
            Self::Date => "date",
            Self::Year => "year",
            Self::Title => "title",
            Self::Random => "random",
        }
    }

    fn legacy(self) -> Option<&'static str> {
        match self {
            Self::Title | Self::Random => None,
            other => Some(other.browse()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackSort {
    Title,
    Plays,
    RecentlyPlayed,
    Date,
    Year,
    /// Browse only.
    Album,
    /// Browse only.
    Duration,
    /// Browse only.
    Random,
    /// Browse only.
    Added,
}

impl TrackSort {
    fn browse(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Plays => "plays",
            Self::RecentlyPlayed => "recentlyPlayed",
            Self::Date => "date",
            Self::Year => "year",
            Self::Album => "album",
            Self::Duration => "duration",
            Self::Random => "random",
            Self::Added => "added",
        }
    }

    fn legacy(self) -> Option<&'static str> {
        match self {
            Self::Album | Self::Duration | Self::Random | Self::Added => None,
            other => Some(other.browse()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumListQuery {
    pub artist_id: Option<i64>,
    pub genre: Option<String>,
    pub search: Option<String>,
    pub sort: Option<AlbumSort>,
    pub order: Option<SortOrder>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// Seed of `sort=random`; keep it for every page of one list.
    pub seed: Option<i64>,
}

impl AlbumListQuery {
    /// Endpoint path and parameters (without paging).
    pub fn source(&self, browse: bool) -> (&'static str, Query) {
        let mut q = Query::new();
        q.push_opt("artist", self.artist_id);
        q.push_opt("genre", self.genre.as_deref());
        q.push_opt("q", self.search.as_deref());
        if !browse {
            q.push_opt("sort", self.sort.and_then(AlbumSort::legacy));
            // /albums only filters an exact year.
            if self.year_from.is_some_and(|y| y > 0) && self.year_from == self.year_to {
                q.push_opt("year", self.year_from);
            }
            return ("/albums", q);
        }
        let sort = self.sort.map(AlbumSort::browse);
        if sort.is_some() {
            q.push_opt("order", self.order.map(SortOrder::as_str));
        }
        q.push_opt("sort", sort);
        q.push_opt("yearFrom", self.year_from.filter(|y| *y > 0));
        q.push_opt("yearTo", self.year_to.filter(|y| *y > 0));
        if self.sort == Some(AlbumSort::Random) {
            q.push_opt("seed", self.seed);
        }
        ("/browse/albums", q)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackListQuery {
    pub album_id: Option<i64>,
    pub artist_id: Option<i64>,
    pub genre: Option<String>,
    pub search: Option<String>,
    pub sort: Option<TrackSort>,
    pub order: Option<SortOrder>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// Seed of `sort=random`; keep it for every page of one list.
    pub seed: Option<i64>,
}

impl TrackListQuery {
    /// Endpoint path and parameters (without paging).
    pub fn source(&self, browse: bool) -> (&'static str, Query) {
        let mut q = Query::new();
        q.push_opt("album", self.album_id);
        q.push_opt("artist", self.artist_id);
        q.push_opt("genre", self.genre.as_deref());
        q.push_opt("q", self.search.as_deref());
        if !browse {
            q.push_opt("sort", self.sort.and_then(TrackSort::legacy));
            q.push_opt("yearFrom", self.year_from.filter(|y| *y > 0));
            q.push_opt("yearTo", self.year_to.filter(|y| *y > 0));
            return ("/tracks", q);
        }
        let sort = self.sort.map(TrackSort::browse);
        q.push_opt("sort", sort);
        q.push_opt("yearFrom", self.year_from.filter(|y| *y > 0));
        q.push_opt("yearTo", self.year_to.filter(|y| *y > 0));
        if sort.is_some() {
            q.push_opt("order", self.order.map(SortOrder::as_str));
        }
        if self.sort == Some(TrackSort::Random) {
            q.push_opt("seed", self.seed);
        }
        ("/browse/tracks", q)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArtistSort {
    Name,
    /// Album count.
    Albums,
    /// Track count.
    Tracks,
    /// Browse only.
    Random,
}

impl ArtistSort {
    fn browse(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Albums => "albums",
            Self::Tracks => "tracks",
            Self::Random => "random",
        }
    }

    fn legacy(self) -> Option<&'static str> {
        match self {
            Self::Albums | Self::Tracks => Some(self.browse()),
            Self::Name | Self::Random => None,
        }
    }
}

/// `role` of the artist lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ArtistRole {
    /// Album artists (the artist page of the sidebar).
    #[default]
    Album,
    All,
    Credit,
    Lyricist,
    Composer,
    Arranger,
    Producer,
}

impl ArtistRole {
    /// True for backstage credit roles (`credit` / `lyricist` / `composer` / `arranger` / `producer`).
    #[must_use]
    pub const fn is_credit_role(self) -> bool {
        matches!(
            self,
            Self::Credit | Self::Lyricist | Self::Composer | Self::Arranger | Self::Producer
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtistListQuery {
    pub role: ArtistRole,
    /// Only favorites (`/artists` honours `favorite=true`).
    pub favorite: bool,
    pub search: Option<String>,
    pub sort: Option<ArtistSort>,
    pub order: Option<SortOrder>,
    pub seed: Option<i64>,
}

impl ArtistListQuery {
    /// Endpoint path and parameters (without paging).
    pub fn source(&self, browse: bool) -> (&'static str, Query) {
        let mut q = Query::new();
        q.push_opt("favorite", self.favorite.then_some("true"));
        q.push_opt("q", self.search.as_deref());
        q.push(
            "role",
            match self.role {
                ArtistRole::Album => "album",
                ArtistRole::All => "all",
                ArtistRole::Credit => "credit",
                ArtistRole::Lyricist => "lyricist",
                ArtistRole::Composer => "composer",
                ArtistRole::Arranger => "arranger",
                ArtistRole::Producer => "producer",
            },
        );
        if !browse {
            q.push_opt("sort", self.sort.and_then(ArtistSort::legacy));
            return ("/artists", q);
        }
        let sort = self.sort.map(ArtistSort::browse);
        if sort.is_some() {
            q.push_opt("order", self.order.map(SortOrder::as_str));
        }
        q.push_opt("sort", sort);
        if self.sort == Some(ArtistSort::Random) {
            q.push_opt("seed", self.seed);
        }
        ("/browse/artists", q)
    }
}

/// Seeds for `sort=random`, like `withRandomSeed` in the Electron client:
/// one seed per list query (without paging) so every page of a list comes
/// from the same shuffle; loading the list from offset 0 again draws a new
/// one. At most [`RandomSeeds::MAX`] queries are remembered.
#[derive(Debug, Default)]
pub struct RandomSeeds {
    /// Oldest first.
    seeds: Vec<(String, i64)>,
}

impl RandomSeeds {
    pub const MAX: usize = 50;

    pub fn new() -> Self {
        Self::default()
    }

    /// Seed for the page at `offset` of the list identified by `key`.
    pub fn seed(&mut self, key: &str, offset: u32, draw: impl FnOnce() -> i64) -> i64 {
        let found = self.seeds.iter().position(|(k, _)| k == key);
        if offset > 0
            && let Some(i) = found
        {
            return self.seeds[i].1;
        }
        if let Some(i) = found {
            self.seeds.remove(i);
        }
        let seed = draw();
        self.seeds.push((key.to_owned(), seed));
        if self.seeds.len() > Self::MAX {
            self.seeds.remove(0);
        }
        seed
    }

    pub fn len(&self) -> usize {
        self.seeds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seeds.is_empty()
    }
}

/// Key of a list query for [`RandomSeeds`]: resource and parameters
/// without paging and seed.
pub fn seed_key(resource: &str, query: &Query) -> String {
    let mut key = resource.to_owned();
    for (k, v) in query
        .pairs()
        .filter(|(k, _)| !matches!(*k, "seed" | "limit" | "offset"))
    {
        key.push('&');
        key.push_str(k);
        key.push('=');
        key.push_str(v);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_stable_per_query_and_rotate_on_offset_zero() {
        let mut seeds = RandomSeeds::new();
        let mut next = 0;
        let mut draw = || {
            next += 1;
            next
        };
        let a = seeds.seed("albums", 0, &mut draw);
        assert_eq!(seeds.seed("albums", 500, &mut draw), a);
        let b = seeds.seed("tracks", 0, &mut draw);
        assert_ne!(a, b);
        // Loading from the top again reshuffles.
        let a2 = seeds.seed("albums", 0, &mut draw);
        assert_ne!(a2, a);
        assert_eq!(seeds.seed("albums", 1000, &mut draw), a2);
        // A later page of an unknown list still gets a seed.
        let c = seeds.seed("artists", 500, &mut draw);
        assert_eq!(seeds.seed("artists", 1000, &mut draw), c);
    }

    #[test]
    fn seeds_keep_at_most_fifty_queries() {
        let mut seeds = RandomSeeds::new();
        for i in 0..60 {
            seeds.seed(&format!("q{i}"), 0, || i);
        }
        assert_eq!(seeds.len(), RandomSeeds::MAX);
        // The oldest were dropped: a later page draws anew.
        assert_eq!(seeds.seed("q0", 500, || 99), 99);
        assert_eq!(seeds.seed("q59", 500, || 99), 59);
    }

    #[test]
    fn seed_key_ignores_paging() {
        let mut q = Query::new();
        q.push("sort", "random").push("seed", 5).page(500, 500);
        assert_eq!(seed_key("albums", &q), "albums&sort=random");
    }

    #[test]
    fn artist_queries() {
        let query = ArtistListQuery {
            sort: Some(ArtistSort::Random),
            order: Some(SortOrder::Desc),
            seed: Some(7),
            search: Some("花".into()),
            ..ArtistListQuery::default()
        };
        let (path, q) = query.source(true);
        assert_eq!(path, "/browse/artists");
        assert_eq!(q.get("sort"), Some("random"));
        assert_eq!(q.get("seed"), Some("7"));
        assert_eq!(q.get("role"), Some("album"));
        assert_eq!(q.get("q"), Some("花"));
        let (path, q) = query.source(false);
        assert_eq!(path, "/artists");
        assert_eq!(q.get("sort"), None);
        assert_eq!(q.get("seed"), None);
        let fav = ArtistListQuery {
            favorite: true,
            sort: Some(ArtistSort::Albums),
            ..ArtistListQuery::default()
        };
        let (_, q) = fav.source(false);
        assert_eq!(q.get("favorite"), Some("true"));
        assert_eq!(q.get("sort"), Some("albums"));
    }
}
