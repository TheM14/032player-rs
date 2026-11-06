//! Library browsing on the core loop: list queries, detail pages, search,
//! favorites and playlist edits. Requests run on tokio tasks and come back
//! as [`Internal::Loaded`] tagged with a sequence number; only the newest
//! request per [`Target`] is applied.

use std::collections::HashSet;

use api::{
    Album, AlbumListQuery, ApiError, Artist, ArtistListQuery, ArtistTrackSort, Client,
    FavoriteKind, Page, Query, Track, TrackListQuery, TrackSort, seed_key,
};
use serde::de::DeserializeOwned;

use super::m032::SourceKey;
use super::{Core, DetailKey, Event, Intent, Internal, Loaded, PAGE, Target, TrackSource};
use crate::browse::{
    self, AlbumFilter, ArtistFilter, DetailView, TrackFilter, TrackList, album_item, artist_item,
    playlist_item, track_item,
};

/// Tracks of "随机播放全部" (the Electron dialog's default).
const SHUFFLE_ALL_COUNT: u32 = 100;
const SIMILAR_COUNT: u32 = 50;
const SEARCH_ARTISTS: u32 = 20;
const SEARCH_ALBUMS: u32 = 40;
const SEARCH_TRACKS: u32 = 100;

/// A seed for `sort=random` (0 .. 2^31, like `Math.random() * 2 ** 31`).
pub(crate) fn random_seed() -> i64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut x = u64::from(nanos) ^ COUNTER.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    i64::try_from(x & 0x7fff_ffff).unwrap_or(0)
}

/// Reads every page of a list, one message per page. Random lists take
/// their seed from `seeds` per page (stable across pages, new from offset 0).
pub(super) async fn load_pages<T: DeserializeOwned>(
    client: &Client,
    path: &str,
    base: &Query,
    page_size: u32,
    seeds: Option<&std::sync::Mutex<api::RandomSeeds>>,
    mut each: impl FnMut(Vec<T>, usize) -> bool,
) -> Result<(), ApiError> {
    let key = seed_key(path, base);
    let mut offset = 0u32;
    loop {
        let mut q = base.clone();
        if let Some(seeds) = seeds {
            let seed = seeds
                .lock()
                .map_or_else(|_| random_seed(), |mut s| s.seed(&key, offset, random_seed));
            q.push("seed", seed);
        }
        q.page(offset, page_size);
        let page: Page<T> = client.get(path, &q).await?;
        let n = u32::try_from(page.items.len()).unwrap_or(u32::MAX);
        let total = usize::try_from(page.total).unwrap_or(0);
        offset = offset.saturating_add(n);
        let done = n == 0 || i64::from(offset) >= page.total;
        if !each(page.items, total) || done {
            return Ok(());
        }
    }
}

fn year(y: Option<i32>) -> Option<i32> {
    y.filter(|y| *y > 0)
}

impl Core {
    /// Starts a request for `target`; older ones of the same target are dropped.
    pub(super) fn next_seq(&mut self, target: Target) -> u64 {
        self.seq += 1;
        self.latest.insert(target, self.seq);
        self.seq
    }

    /// Client, internal sender and API token for a background request.
    pub(super) fn request(&self) -> Option<(Client, tokio::sync::mpsc::UnboundedSender<Internal>)> {
        let library = self.library.as_ref()?;
        Some((library.client.clone(), self.internal.clone()))
    }

    pub(super) fn failed(client: &Client, what: &'static str, error: ApiError) -> Internal {
        Internal::Failed {
            api_token: client.api_token().to_owned(),
            what,
            error,
        }
    }

    // ------------------------------------------------------------ lists

    pub(super) fn query_albums(&mut self, favorites: bool, filter: AlbumFilter) {
        let Some(library) = &self.library else { return };
        let browse = library.browse();
        let seeds = library.seeds.clone();
        let Some((client, tx)) = self.request() else {
            return;
        };
        if favorites {
            self.favorite_albums = Some(filter.clone());
            let seq = self.next_seq(Target::FavoriteAlbums);
            tokio::spawn(async move {
                let result = async {
                    let all = client.favorite_albums().await?;
                    let artist_albums = match filter.artist_id {
                        Some(id) => {
                            let mut q = Query::new();
                            q.push("artist", id);
                            let albums: Vec<Album> = client.get_all("/albums", &q).await?;
                            Some(albums.into_iter().map(|a| a.id).collect::<HashSet<_>>())
                        }
                        None => None,
                    };
                    #[allow(clippy::cast_sign_loss)]
                    let seed = random_seed() as u64;
                    Ok::<_, ApiError>(browse::favorite_albums(
                        all,
                        &filter,
                        artist_albums.as_ref(),
                        seed,
                    ))
                }
                .await;
                let msg = match result {
                    Ok(albums) => Internal::Loaded(
                        Target::FavoriteAlbums,
                        seq,
                        Loaded::FavoriteAlbums(albums.iter().map(album_item).collect()),
                    ),
                    Err(e) => Self::failed(&client, "加载收藏专辑", e),
                };
                let _ = tx.send(msg);
            });
            return;
        }
        let seq = self.next_seq(Target::Albums);
        self.emit(Event::AlbumsReset);
        let random = browse && filter.sort == api::AlbumSort::Random;
        let query = AlbumListQuery {
            artist_id: filter.artist_id,
            search: Some(filter.search.trim().to_owned()).filter(|s| !s.is_empty()),
            sort: Some(filter.sort),
            order: Some(filter.order),
            year_from: year(filter.year_from),
            year_to: year(filter.year_to),
            ..AlbumListQuery::default()
        };
        let (path, q) = query.source(browse);
        tokio::spawn(async move {
            let result = load_pages::<Album>(
                &client,
                path,
                &q,
                PAGE,
                random.then_some(&*seeds),
                |items, total| {
                    let items = items.iter().map(album_item).collect();
                    tx.send(Internal::Loaded(
                        Target::Albums,
                        seq,
                        Loaded::AlbumsPage(items, total),
                    ))
                    .is_ok()
                },
            )
            .await;
            if let Err(e) = result {
                let _ = tx.send(Self::failed(&client, "加载专辑", e));
            }
        });
    }

    pub(super) fn query_tracks(&mut self, favorites: bool, filter: TrackFilter) {
        let Some(library) = &self.library else { return };
        let browse = library.browse();
        let seeds = library.seeds.clone();
        let Some((client, tx)) = self.request() else {
            return;
        };
        if favorites {
            self.favorite_tracks = Some(filter.clone());
            let seq = self.next_seq(Target::FavoriteTracks);
            tokio::spawn(async move {
                let msg = match client.favorite_tracks().await {
                    Ok(all) => {
                        #[allow(clippy::cast_sign_loss)]
                        let seed = random_seed() as u64;
                        Internal::Loaded(
                            Target::FavoriteTracks,
                            seq,
                            Loaded::FavoriteTracks(browse::favorite_tracks(all, &filter, seed)),
                        )
                    }
                    Err(e) => Self::failed(&client, "加载收藏曲目", e),
                };
                let _ = tx.send(msg);
            });
            return;
        }
        self.track_filter = filter.clone();
        let seq = self.next_seq(Target::Tracks);
        if let Some(library) = self.library.as_mut() {
            library.lists.remove(&TrackList::Library);
        }
        self.emit(Event::TracksReset);
        let random = browse && filter.sort == TrackSort::Random;
        let query = TrackListQuery {
            artist_id: filter.artist_id,
            search: Some(filter.search.trim().to_owned()).filter(|s| !s.is_empty()),
            sort: Some(filter.sort),
            order: Some(filter.order),
            year_from: year(filter.year_from),
            year_to: year(filter.year_to),
            ..TrackListQuery::default()
        };
        let (path, q) = query.source(browse);
        tokio::spawn(async move {
            let result = load_pages::<Track>(
                &client,
                path,
                &q,
                PAGE,
                random.then_some(&*seeds),
                |items, total| {
                    tx.send(Internal::Loaded(
                        Target::Tracks,
                        seq,
                        Loaded::TracksPage(items, total),
                    ))
                    .is_ok()
                },
            )
            .await;
            if let Err(e) = result {
                let _ = tx.send(Self::failed(&client, "加载曲目", e));
            }
        });
    }

    pub(super) fn query_artists(&mut self, favorites: bool, filter: ArtistFilter) {
        let Some(library) = &self.library else { return };
        let browse = library.browse();
        let seeds = library.seeds.clone();
        let Some((client, tx)) = self.request() else {
            return;
        };
        let target = if favorites {
            self.favorite_artists = Some(filter.clone());
            Target::FavoriteArtists
        } else {
            Target::Artists
        };
        let seq = self.next_seq(target);
        if !favorites {
            self.emit(Event::ArtistsReset);
        }
        let random = browse && filter.sort == api::ArtistSort::Random;
        let query = ArtistListQuery {
            role: if filter.all_roles {
                api::ArtistRole::All
            } else {
                api::ArtistRole::Album
            },
            favorite: favorites,
            search: Some(filter.search.trim().to_owned()).filter(|s| !s.is_empty()),
            sort: Some(filter.sort),
            order: Some(filter.order),
            ..ArtistListQuery::default()
        };
        let (path, q) = query.source(browse);
        tokio::spawn(async move {
            let mut all = Vec::new();
            let result = load_pages::<Artist>(
                &client,
                path,
                &q,
                PAGE,
                random.then_some(&*seeds),
                |items, total| {
                    let items: Vec<_> = items.iter().map(artist_item).collect();
                    if favorites {
                        all.extend(items);
                        return true;
                    }
                    tx.send(Internal::Loaded(
                        target,
                        seq,
                        Loaded::ArtistsPage(items, total),
                    ))
                    .is_ok()
                },
            )
            .await;
            let msg = match result {
                Err(e) => Self::failed(&client, "加载艺人", e),
                Ok(()) if favorites => Internal::Loaded(target, seq, Loaded::FavoriteArtists(all)),
                Ok(()) => return,
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn load_playlists(&mut self) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::Playlists);
        tokio::spawn(async move {
            let msg = match client.playlists().await {
                Ok(all) => Internal::Loaded(Target::Playlists, seq, Loaded::Playlists(all)),
                Err(e) => Self::failed(&client, "加载歌单", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn emit_playlists(&self) {
        let shown = browse::sort_playlists(self.playlists.clone(), &self.playlist_filter);
        self.emit(Event::Playlists(shown.iter().map(playlist_item).collect()));
    }

    // ------------------------------------------------------------ details

    pub(super) fn open_detail(&mut self, key: DetailKey) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        self.open_detail = Some(key);
        let seq = self.next_seq(Target::Detail);
        let similar_title = match key {
            DetailKey::Similar(id) => self
                .library
                .as_ref()
                .and_then(|l| l.lists.values().flatten().find(|t| t.id == id))
                .map(|t| t.title.clone())
                .or_else(|| {
                    self.queue
                        .in_play_order()
                        .find(|i| i.track.id == id)
                        .map(|i| i.track.title.clone())
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        let mixes_known = self.home.as_ref().and_then(|h| h.mixes()).cloned();
        tokio::spawn(async move {
            let result: Result<(DetailView, Vec<Track>, Vec<String>), ApiError> = async {
                Ok(match key {
                    DetailKey::Album(id) => {
                        let detail = client.album(id).await?;
                        let view = browse::album_view(&detail.album, &detail.tracks);
                        (DetailView::Album(view), detail.tracks, Vec::new())
                    }
                    DetailKey::Artist(id) => {
                        let mut q = Query::new();
                        q.push("artist", id);
                        let (detail, matching) =
                            tokio::join!(client.artist(id), client.get_all::<Album>("/albums", &q));
                        let detail = detail?;
                        // /albums?artist= matches any role: albums only reached
                        // through credits stay visible as relation "credit".
                        let mut releases: Vec<_> = detail
                            .releases
                            .iter()
                            .map(|r| browse::AlbumItem {
                                relation: r.relation.clone(),
                                ..album_item(&r.album)
                            })
                            .collect();
                        let seen: HashSet<i64> = releases.iter().map(|a| a.id).collect();
                        releases.extend(matching?.iter().filter(|a| !seen.contains(&a.id)).map(
                            |a| browse::AlbumItem {
                                relation: "credit".into(),
                                ..album_item(a)
                            },
                        ));
                        let a = &detail.artist;
                        let view = browse::ArtistView {
                            id: a.id,
                            name: a.name.clone(),
                            image: a.image_url.clone(),
                            album_count: a.album_count,
                            track_count: a.track_count.max(detail.tracks_total),
                            favorite: a.is_favorite,
                            biography: a.biography.clone(),
                            biography_source: a.biography_source.clone(),
                            artist_type: a.artist_type.clone(),
                            country: a.country.clone(),
                            aliases: a.aliases.clone(),
                            releases,
                            top_tracks_popular: detail.top_tracks_are_popular,
                        };
                        (DetailView::Artist(view), detail.top_tracks, Vec::new())
                    }
                    DetailKey::Playlist(id) => {
                        let detail = client.playlist(id).await?;
                        let view = browse::playlist_view(&detail.playlist);
                        (DetailView::Playlist(view), detail.tracks, Vec::new())
                    }
                    DetailKey::Similar(id) => {
                        let tracks = client.similar_tracks(id, SIMILAR_COUNT).await?;
                        (
                            DetailView::Similar {
                                track_id: id,
                                title: similar_title,
                            },
                            tracks,
                            Vec::new(),
                        )
                    }
                    DetailKey::Work(id) => {
                        let (work, albums, tracks) = tokio::join!(
                            client.work(id),
                            client.work_albums(id),
                            client.work_tracks(id)
                        );
                        let (view, collected, tags) =
                            crate::m032::work_view(&work?, &albums?, &tracks?);
                        (DetailView::Work(view), collected, tags)
                    }
                    DetailKey::Series(id) => {
                        let (detail, albums) =
                            tokio::join!(client.series(id), client.series_albums(id));
                        let view = crate::m032::series_view(&detail?, &albums?);
                        (DetailView::Series(view), Vec::new(), Vec::new())
                    }
                    DetailKey::DailyMix(id) => {
                        let (detail, list) = match mixes_known {
                            Some(list) => (client.daily_mix(id).await, Ok(list)),
                            None => tokio::join!(client.daily_mix(id), client.daily_mixes()),
                        };
                        let detail = detail?;
                        let view = crate::m032::mix_view(&detail, list.ok().as_ref());
                        let tracks = detail.tracks.into_iter().map(|t| t.track).collect();
                        (DetailView::DailyMix(view), tracks, Vec::new())
                    }
                })
            }
            .await;
            // Album pages also show the works linked to the album.
            let album = match (key, &result) {
                (DetailKey::Album(id), Ok((_, tracks, _))) => Some((id, tracks.clone())),
                _ => None,
            };
            let msg = match result {
                Ok((view, tracks, tags)) => Internal::Loaded(
                    Target::Detail,
                    seq,
                    Loaded::Detail(Box::new(view), tracks, tags),
                ),
                Err(e) => Self::failed(&client, "加载详情", e),
            };
            let _ = tx.send(msg);
            if let Some((id, tracks)) = album {
                let works = album_works(&client, id, &tracks).await;
                if !works.is_empty() {
                    let _ = tx.send(Internal::Loaded(
                        Target::Detail,
                        seq,
                        Loaded::AlbumWorks(id, works),
                    ));
                }
            }
        });
    }

    pub(super) fn search(&mut self, query: String) {
        let query = query.trim().to_owned();
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::Search);
        if query.is_empty() {
            self.loaded(Loaded::Search {
                query,
                artists: Vec::new(),
                albums: Vec::new(),
                tracks: Vec::new(),
            });
            return;
        }
        tokio::spawn(async move {
            let q = |limit: u32| {
                let mut q = Query::new();
                q.push("q", &query).page(0, limit);
                q
            };
            let (qa, qb, qt) = (q(SEARCH_ARTISTS), q(SEARCH_ALBUMS), q(SEARCH_TRACKS));
            let (artists, albums, tracks) = tokio::join!(
                client.get::<Page<Artist>>("/artists", &qa),
                client.get::<Page<Album>>("/albums", &qb),
                client.get::<Page<Track>>("/tracks", &qt),
            );
            let msg = match (artists, albums, tracks) {
                (Ok(artists), Ok(albums), Ok(tracks)) => Internal::Loaded(
                    Target::Search,
                    seq,
                    Loaded::Search {
                        query,
                        artists: artists.items.iter().map(artist_item).collect(),
                        albums: albums.items.iter().map(album_item).collect(),
                        tracks: tracks.items,
                    },
                ),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    Self::failed(&client, "搜索", e)
                }
            };
            let _ = tx.send(msg);
        });
    }

    /// Applies the newest result of a target.
    pub(super) fn loaded(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::AlbumsPage(items, total) => self.emit(Event::AlbumsAppended { items, total }),
            Loaded::TracksPage(tracks, total) => {
                let Some(library) = self.library.as_mut() else {
                    return;
                };
                let items = tracks.iter().map(track_item).collect();
                library
                    .lists
                    .entry(TrackList::Library)
                    .or_default()
                    .extend(tracks);
                self.emit(Event::TracksAppended { items, total });
            }
            Loaded::ArtistsPage(items, total) => {
                self.emit(Event::ArtistsAppended { items, total });
            }
            Loaded::FavoriteAlbums(items) => self.emit(Event::FavoriteAlbums(items)),
            Loaded::FavoriteTracks(tracks) => {
                let items = tracks.iter().map(track_item).collect();
                self.set_list(TrackList::Favorites, tracks);
                self.emit(Event::FavoriteTracks(items));
            }
            Loaded::FavoriteArtists(items) => self.emit(Event::FavoriteArtists(items)),
            Loaded::Playlists(all) => {
                self.playlists = all;
                let mut choices = self.playlists.clone();
                choices.sort_by_key(|p| p.name.to_lowercase());
                self.emit(Event::PlaylistChoices(
                    choices.iter().map(playlist_item).collect(),
                ));
                self.emit_playlists();
            }
            Loaded::Detail(view, tracks, tags) => {
                let view = *view;
                let mut items: Vec<_> = tracks.iter().map(track_item).collect();
                for (item, tag) in items.iter_mut().zip(tags) {
                    item.tag = tag;
                }
                self.open_mix = match &view {
                    DetailView::DailyMix(m) => Some(super::m032::OpenMix {
                        id: m.id,
                        rows: m.rows.clone(),
                    }),
                    _ => None,
                };
                self.set_list(TrackList::Detail, tracks);
                self.artist_groups = match &view {
                    DetailView::Artist(a) => Some((
                        a.id,
                        browse::release_groups(&a.releases)
                            .iter()
                            .map(|g| g.items.iter().map(|i| i.id).collect())
                            .collect(),
                    )),
                    _ => None,
                };
                if let DetailView::Artist(a) = &view {
                    self.load_artist_favorites(a.id);
                }
                self.emit(Event::Detail {
                    view,
                    tracks: items,
                });
            }
            Loaded::AlbumWorks(album_id, works) => {
                let tieups = crate::m032::track_tieups(&works);
                self.emit(Event::AlbumWorks {
                    album_id,
                    works,
                    tieups,
                });
            }
            Loaded::Search {
                query,
                artists,
                albums,
                tracks,
            } => {
                let items = tracks.iter().map(track_item).collect();
                self.set_list(TrackList::Search, tracks);
                self.emit(Event::SearchResults {
                    query,
                    artists,
                    albums,
                    tracks: items,
                });
            }
            Loaded::WorksPage(items, total) => self.emit(Event::WorksAppended { items, total }),
            Loaded::SeriesPage(items, total) => {
                self.emit(Event::SeriesAppended { items, total });
            }
            Loaded::HistoryPage(records, total) => self.history_page(records, total),
            Loaded::ArtistFavorites(artist_id, tracks) => {
                if self.open_detail != Some(DetailKey::Artist(artist_id))
                    && self.artist_groups.as_ref().map(|g| g.0) != Some(artist_id)
                {
                    return;
                }
                let items = tracks.iter().map(track_item).collect();
                self.set_list(TrackList::ArtistFavorites, tracks);
                self.emit(Event::ArtistFavorites {
                    artist_id,
                    tracks: items,
                });
            }
            Loaded::Related(track_id, tracks) => {
                let items = tracks.iter().map(track_item).collect();
                self.set_list(TrackList::Related, tracks);
                self.emit(Event::Related {
                    track_id,
                    tracks: items,
                });
            }
            home => self.home_loaded(home),
        }
    }

    /// The artist page's "收藏的歌曲": favorite tracks crediting the artist
    /// (Electron `FAVORITE_SONGS`), filtered here like the favorites page.
    pub(super) fn load_artist_favorites(&mut self, artist_id: i64) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::ArtistFavorites);
        tokio::spawn(async move {
            let msg = match client.favorite_tracks().await {
                Ok(all) => {
                    let filter = TrackFilter {
                        artist_id: Some(artist_id),
                        ..TrackFilter::default()
                    };
                    let tracks = browse::favorite_tracks(all, &filter, 0);
                    Internal::Loaded(
                        Target::ArtistFavorites,
                        seq,
                        Loaded::ArtistFavorites(artist_id, tracks),
                    )
                }
                Err(e) => Self::failed(&client, "加载收藏的歌曲", e),
            };
            let _ = tx.send(msg);
        });
    }

    /// Similar songs of the current track for the full-screen player.
    pub(super) fn load_related(&mut self, id: i64) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::Related);
        tokio::spawn(async move {
            let msg = match client.similar_tracks(id, SIMILAR_COUNT).await {
                Ok(tracks) => Internal::Loaded(Target::Related, seq, Loaded::Related(id, tracks)),
                Err(e) => Self::failed(&client, "加载相关歌曲", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn set_list(&mut self, list: TrackList, tracks: Vec<Track>) {
        if let Some(library) = self.library.as_mut() {
            library.lists.insert(list, tracks);
        }
    }

    // ------------------------------------------------------------ favorites

    pub(super) fn set_favorite(&mut self, kind: FavoriteKind, id: i64, favorite: bool) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        tokio::spawn(async move {
            let msg = match client.set_favorite(kind, id, favorite).await {
                Ok(()) => Internal::FavoriteSet { kind, id, favorite },
                Err(e) => Self::failed(&client, "收藏", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn favorite_set(&mut self, kind: FavoriteKind, id: i64, favorite: bool) {
        if self.library.is_none() {
            return;
        }
        if kind == FavoriteKind::Track {
            if let Some(library) = self.library.as_mut() {
                for track in library.lists.values_mut().flatten() {
                    if track.id == id {
                        track.is_favorite = favorite;
                    }
                }
            }
            self.queue.set_favorite(id, favorite);
            self.schedule_queue_save();
        }
        self.emit(Event::Favorite { kind, id, favorite });
        // The favorites page shows the new set.
        match kind {
            FavoriteKind::Album => {
                if let Some(filter) = self.favorite_albums.clone() {
                    self.query_albums(true, filter);
                }
            }
            FavoriteKind::Track => {
                if let Some(filter) = self.favorite_tracks.clone() {
                    self.query_tracks(true, filter);
                }
                if let Some(DetailKey::Artist(id)) = self.open_detail {
                    self.load_artist_favorites(id);
                }
            }
            FavoriteKind::Artist => {
                if let Some(filter) = self.favorite_artists.clone() {
                    self.query_artists(true, filter);
                }
            }
        }
    }

    // ------------------------------------------------------------ tracks

    /// Resolves `source` to tracks (fetching albums, artists, playlists,
    /// works, series, daily mixes) and applies `intent` to them.
    pub(super) fn with_tracks(&mut self, source: TrackSource, intent: Intent) {
        let Some(library) = &self.library else { return };
        let with_context = library.capabilities.has(api::Feature::PlaybackContext);
        // Playing the open daily mix: its playable rows, attributed to it.
        if let (TrackSource::List(TrackList::Detail), Intent::Play { start, shuffle, .. }) =
            (source, &intent)
            && let Some((tracks, play)) = self.mix_playable(*start, true)
        {
            let play = if *shuffle { play.shuffled() } else { play };
            self.fetched(tracks, play);
            return;
        }
        let tracks = match source {
            TrackSource::Track(list, index) => {
                library.list(list).get(index).cloned().map(|t| vec![t])
            }
            TrackSource::List(list) => Some(library.list(list).to_vec()),
            TrackSource::Queue => Some(
                self.queue
                    .in_play_order()
                    .map(|item| item.track.clone())
                    .collect(),
            ),
            _ => None,
        };
        if let Some(tracks) = tracks {
            self.fetched(tracks, intent);
            return;
        }
        let Some((client, tx)) = self.request() else {
            return;
        };
        let group_albums = match (source, &self.artist_groups) {
            (TrackSource::ArtistGroup(artist, group), Some((open, groups))) if artist == *open => {
                groups.get(group).cloned().unwrap_or_default()
            }
            _ => Vec::new(),
        };
        if let (TrackSource::Album(album_id), Intent::AddToPlaylist(playlist_id)) =
            (source, &intent)
        {
            // The server appends the album in disc / track order.
            let playlist_id = *playlist_id;
            tokio::spawn(async move {
                let msg = match client.append_album_to_playlist(playlist_id, album_id).await {
                    Ok(r) => added_notice(&r),
                    Err(e) => Self::failed(&client, "添加到歌单", e),
                };
                let _ = tx.send(msg);
            });
            return;
        }
        tokio::spawn(async move {
            let mut intent = intent;
            let result = match source {
                TrackSource::Album(id) => client.album(id).await.map(|d| d.tracks),
                TrackSource::Artist(id) => client.artist_tracks(id, ArtistTrackSort::Album).await,
                TrackSource::Playlist(id) => client.playlist(id).await.map(|d| d.tracks),
                TrackSource::Work(id) => client.work_tracks(id).await.map(|items| {
                    let tracks = unique_tracks(items);
                    let ids = tracks.iter().map(|t| t.id).collect();
                    let _ = tx.send(Internal::SourceTracks(SourceKey::Work(id), ids));
                    tracks
                }),
                TrackSource::Series(id) => client.series_tracks(id).await.map(|items| {
                    let tracks = unique_tracks(items);
                    let ids = tracks.iter().map(|t| t.id).collect();
                    let _ = tx.send(Internal::SourceTracks(SourceKey::Series(id), ids));
                    tracks
                }),
                TrackSource::DailyMix(id) => client.daily_mix(id).await.map(|detail| {
                    // "加入队列" songs belong to the queue, not the mix: only
                    // playing the mix carries its context.
                    let playable: Vec<_> = detail
                        .tracks
                        .into_iter()
                        .filter(|t| !t.track.missing)
                        .collect();
                    if let Intent::Play { contexts, .. } = &mut intent
                        && with_context
                        && id > 0
                    {
                        *contexts = playable
                            .iter()
                            .map(|t| Some(session::PlaybackContext::daily_mix(id, t.position)))
                            .collect();
                    }
                    playable.into_iter().map(|t| t.track).collect()
                }),
                TrackSource::ArtistGroup(..) => {
                    let mut tracks = Vec::new();
                    let mut result = Ok(());
                    for id in group_albums {
                        match client.album(id).await {
                            Ok(detail) => tracks.extend(detail.tracks),
                            Err(e) => {
                                result = Err(e);
                                break;
                            }
                        }
                    }
                    result.map(|()| tracks)
                }
                TrackSource::Track(..) | TrackSource::List(_) | TrackSource::Queue => {
                    Ok(Vec::new())
                }
            };
            let _ = tx.send(Internal::Fetched(result, intent));
        });
    }

    pub(super) fn fetched(&mut self, tracks: Vec<Track>, intent: Intent) {
        if self.library.is_none() {
            return;
        }
        match intent {
            Intent::Play {
                start,
                shuffle,
                contexts,
                start_ms,
            } => {
                if tracks.is_empty() {
                    self.emit(Event::Notice("没有可播放的歌曲".into()));
                    return;
                }
                let mut contexts = contexts;
                contexts.resize(tracks.len(), None);
                let mut entries: Vec<_> = tracks.into_iter().zip(contexts).collect();
                if shuffle {
                    #[allow(clippy::cast_sign_loss)]
                    browse::shuffle(&mut entries, random_seed() as u64);
                }
                let (tracks, contexts): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
                let started = self.queue.replace(tracks, start).is_some();
                self.queue.set_contexts(contexts);
                if started {
                    if start_ms > 0 {
                        self.queue.set_current_start(start_ms);
                    }
                    self.play_current(true);
                }
                self.queue_changed();
            }
            Intent::At(at) => self.enqueue(tracks, at),
            Intent::AddToPlaylist(playlist_id) => {
                let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
                if ids.is_empty() {
                    return;
                }
                let Some((client, tx)) = self.request() else {
                    return;
                };
                tokio::spawn(async move {
                    let msg = match client.append_to_playlist(playlist_id, &ids).await {
                        Ok(r) => added_notice(&r),
                        Err(e) => Self::failed(&client, "添加到歌单", e),
                    };
                    let _ = tx.send(msg);
                });
            }
            Intent::NewPlaylist { name, description } => {
                let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
                let Some((client, tx)) = self.request() else {
                    return;
                };
                tokio::spawn(async move {
                    let msg = match client.create_playlist(&name, &description, &ids).await {
                        Ok(_) => Internal::PlaylistsChanged {
                            notice: Some(format!("已创建歌单「{name}」")),
                            deleted: None,
                        },
                        Err(e) => Self::failed(&client, "创建歌单", e),
                    };
                    let _ = tx.send(msg);
                });
            }
        }
    }

    /// Random tracks of the library (browse: uniform `sort=random`; older
    /// servers: a random window of the list, shuffled).
    pub(super) fn shuffle_all(&mut self) {
        let Some(library) = &self.library else { return };
        let browse = library.browse();
        let Some((client, tx)) = self.request() else {
            return;
        };
        let mut q = Query::new();
        q.push_opt("yearFrom", year(self.track_filter.year_from));
        q.push_opt("yearTo", year(self.track_filter.year_to));
        tokio::spawn(async move {
            let result = async {
                if browse {
                    q.push("sort", "random").push("seed", random_seed());
                    q.page(0, SHUFFLE_ALL_COUNT);
                    let page: Page<Track> = client.get("/browse/tracks", &q).await?;
                    return Ok(page.items);
                }
                let mut count = q.clone();
                count.push("limit", 1);
                let total: Page<Track> = client.get("/tracks", &count).await?;
                let total = u32::try_from(total.total).unwrap_or(0);
                let window = total.min(PAGE);
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let offset = (random_seed() as u32) % (total - window + 1).max(1);
                q.page(offset, window.max(1));
                let page: Page<Track> = client.get("/tracks", &q).await?;
                let mut items = page.items;
                #[allow(clippy::cast_sign_loss)]
                browse::shuffle(&mut items, random_seed() as u64);
                items.truncate(SHUFFLE_ALL_COUNT as usize);
                Ok::<_, ApiError>(items)
            }
            .await;
            let _ = tx.send(Internal::Fetched(result, Intent::play_at(0)));
        });
    }

    // ------------------------------------------------------------ playlists

    pub(super) fn create_playlist(
        &mut self,
        name: String,
        description: String,
        tracks: Option<TrackSource>,
    ) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        match tracks {
            Some(source) => self.with_tracks(source, Intent::NewPlaylist { name, description }),
            None => self.fetched(Vec::new(), Intent::NewPlaylist { name, description }),
        }
    }

    pub(super) fn update_playlist(&mut self, id: i64, name: String, description: String) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        let Some((client, tx)) = self.request() else {
            return;
        };
        tokio::spawn(async move {
            let msg = match client.update_playlist(id, &name, &description).await {
                Ok(()) => Internal::PlaylistsChanged {
                    notice: None,
                    deleted: None,
                },
                Err(e) => Self::failed(&client, "修改歌单", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn delete_playlist(&mut self, id: i64) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        tokio::spawn(async move {
            let msg = match client.delete_playlist(id).await {
                Ok(()) => Internal::PlaylistsChanged {
                    notice: Some("已删除歌单".into()),
                    deleted: Some(id),
                },
                Err(e) => Self::failed(&client, "删除歌单", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn add_to_playlist(&mut self, playlist_id: i64, source: TrackSource) {
        self.with_tracks(source, Intent::AddToPlaylist(playlist_id));
    }

    /// Track id of a row of the open playlist.
    fn open_playlist_track(&self, playlist_id: i64, index: usize) -> Option<i64> {
        if self.open_detail != Some(DetailKey::Playlist(playlist_id)) {
            return None;
        }
        Some(
            self.library
                .as_ref()?
                .list(TrackList::Detail)
                .get(index)?
                .id,
        )
    }

    pub(super) fn remove_from_playlist(&mut self, playlist_id: i64, index: usize) {
        let Some(track_id) = self.open_playlist_track(playlist_id, index) else {
            return;
        };
        let Some((client, tx)) = self.request() else {
            return;
        };
        tokio::spawn(async move {
            let remove = || async {
                let revision = client.playlist(playlist_id).await?.playlist.revision;
                client
                    .remove_from_playlist(playlist_id, &[track_id], revision)
                    .await
            };
            let mut result = remove().await;
            // Edited elsewhere in between: read the revision once more.
            if result
                .as_ref()
                .is_err_and(|e| e.code() == Some("playlist_conflict"))
            {
                result = remove().await;
            }
            let msg = match result {
                Ok(_) => Internal::PlaylistsChanged {
                    notice: None,
                    deleted: None,
                },
                Err(e) => Self::failed(&client, "从歌单移除", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn move_playlist_item(&mut self, playlist_id: i64, from: usize, to: usize) {
        if from == to {
            return;
        }
        let Some(moved) = self.open_playlist_track(playlist_id, from) else {
            return;
        };
        // Show the new order right away; the reload confirms it.
        if let Some(library) = self.library.as_mut()
            && let Some(list) = library.lists.get_mut(&TrackList::Detail)
        {
            let track = list.remove(from);
            list.insert(to.min(list.len()), track);
        }
        let Some((client, tx)) = self.request() else {
            return;
        };
        tokio::spawn(async move {
            let result = async {
                let detail = client.playlist(playlist_id).await?;
                let mut ids: Vec<i64> = detail.tracks.iter().map(|t| t.id).collect();
                let Some(at) = ids.iter().position(|id| *id == moved) else {
                    return Ok(());
                };
                ids.remove(at);
                ids.insert(to.min(ids.len()), moved);
                client
                    .order_playlist(playlist_id, &ids, detail.playlist.revision)
                    .await
            }
            .await;
            let msg = match result {
                Ok(()) => Internal::PlaylistsChanged {
                    notice: None,
                    deleted: None,
                },
                Err(e) => {
                    let _ = tx.send(Internal::PlaylistsChanged {
                        notice: None,
                        deleted: None,
                    });
                    Self::failed(&client, "调整歌单顺序", e)
                }
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn set_playlist_artwork(&mut self, playlist_id: i64, image: Option<Vec<u8>>) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        tokio::spawn(async move {
            let result = match image {
                Some(bytes) => client.upload_playlist_artwork(playlist_id, bytes).await,
                None => client.reset_playlist_artwork(playlist_id).await,
            };
            let msg = match result {
                Ok(_) => Internal::PlaylistsChanged {
                    notice: None,
                    deleted: None,
                },
                Err(e) => Self::failed(&client, "设置歌单封面", e),
            };
            let _ = tx.send(msg);
        });
    }
}

/// Tracks of work / series expansions, each once in first-seen order.
fn unique_tracks(items: Vec<api::WorkTrack>) -> Vec<Track> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .map(|t| t.track)
        .filter(|t| seen.insert(t.id))
        .collect()
}

fn added_notice(r: &api::PlaylistItemResult) -> Internal {
    let notice = if r.added == 0 && r.skipped_duplicate > 0 {
        "歌单里已有这些歌曲".to_owned()
    } else if r.skipped_duplicate > 0 {
        format!(
            "已添加 {} 首到歌单（跳过 {} 首重复）",
            r.added, r.skipped_duplicate
        )
    } else {
        format!("已添加 {} 首到歌单", r.added)
    };
    Internal::PlaylistsChanged {
        notice: Some(notice),
        deleted: None,
    }
}

/// Works linked to an album with each work's detail, albums and tracks
/// (fetched concurrently). Errors leave the inspector without works.
async fn album_works(
    client: &Client,
    album_id: i64,
    tracks: &[Track],
) -> Vec<crate::m032::AlbumWorkEntry> {
    let links = match client.album_works(album_id).await {
        Ok(links) => links,
        Err(e) => {
            tracing::debug!(album_id, error = %e, "album works unavailable");
            return Vec::new();
        }
    };
    let handles: Vec<_> = links
        .iter()
        .map(|link| {
            let client = client.clone();
            let id = link.id;
            tokio::spawn(async move {
                let (detail, albums, tracks) = tokio::join!(
                    client.work(id),
                    client.work_albums(id),
                    client.work_tracks(id)
                );
                (
                    detail.ok(),
                    albums.unwrap_or_default(),
                    tracks.unwrap_or_default(),
                )
            })
        })
        .collect();
    let mut per_work = Vec::with_capacity(handles.len());
    for handle in handles {
        per_work.push(handle.await.unwrap_or_default());
    }
    crate::m032::album_works(album_id, tracks, &links, &per_work)
}
