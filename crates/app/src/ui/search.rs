//! Unified local-library search UI state and handlers.

use std::rc::Rc;

use client_core::{
    AlbumItem, ArtistItem, ArtworkFit, Command, FavoriteKind, SearchBestMatch, SearchCategory,
    TrackItem, TrackList, TrackSource,
};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, VecModel};

use crate::cards::Card;
use crate::{
    AppWindow, SearchBestMatch as SlintBestMatch, SearchCompactCard, SearchPreviewTrack,
    SearchState,
};

use super::m032::Bind;
use super::{
    Dialog, GRID_SEARCH_ALBUMS, GRID_SEARCH_ARTISTS, GRID_SEARCH_COMPOSERS, Route, Ui, artist_refs,
    cards, int, track_rows, with_ui,
};

const BEST_ART: (f32, f32) = (176.0, 176.0);
const COMPACT_ART: (f32, f32) = (72.0, 72.0);
const TRACK_ART: (f32, f32) = (48.0, 48.0);

#[derive(Default)]
pub(super) struct SearchUiState {
    active_query: String,
    loaded_tabs: [bool; 5],
    best_match: Option<SearchBestMatch>,
}

pub(super) fn install_callbacks(window: &AppWindow) {
    let search = window.global::<SearchState>();
    search.on_search(|text| with_ui(|ui, w| ui.submit_search(w, &text)));
    search.on_select_tab(|tab| with_ui(|ui, w| ui.select_search_tab(w, tab)));
    search.on_open_best(|| with_ui(|ui, w| ui.open_search_best(w)));
    search.on_best_action(|what, arg| {
        with_ui(|ui, w| ui.search_best_action(w, &what, i64::from(arg)));
    });
    search.on_card_action(|kind, what, id, arg| {
        with_ui(|ui, w| {
            ui.search_compact_card_action(w, kind, &what, i64::from(id), i64::from(arg))
        });
    });
    search.on_track_action(|what, index, arg| {
        with_ui(|ui, w| {
            if let Ok(index) = usize::try_from(index) {
                ui.track_action(w, TrackList::SearchPreview, &what, index, i64::from(arg));
            }
        });
    });
}

impl Ui {
    pub(super) fn reset_search(&self, w: &AppWindow, clear_input: bool) {
        self.unbind(Bind::Search);
        *self.search.borrow_mut() = SearchUiState::default();
        self.grids[GRID_SEARCH_ARTISTS].clear();
        self.grids[GRID_SEARCH_ALBUMS].clear();
        self.grids[GRID_SEARCH_COMPOSERS].clear();
        self.search_tracks.set_vec(Vec::new());
        self.search_preview_tracks.set_vec(Vec::new());

        let search = w.global::<SearchState>();
        if clear_input {
            search.set_query_input(SharedString::new());
        }
        search.set_active_query(SharedString::new());
        search.set_tab(0);
        search.set_loading(false);
        search.set_tab_loading(false);
        clear_overview_models(&search);
    }

    pub(super) fn submit_search(&self, w: &AppWindow, text: &str) {
        let query: String = text.trim().chars().take(200).collect();
        if query.is_empty() {
            self.reset_search(w, false);
            return;
        }
        self.unbind(Bind::Search);
        {
            let mut state = self.search.borrow_mut();
            state.active_query = query.clone();
            state.loaded_tabs = [false; 5];
            state.best_match = None;
        }
        self.grids[GRID_SEARCH_ARTISTS].clear();
        self.grids[GRID_SEARCH_ALBUMS].clear();
        self.grids[GRID_SEARCH_COMPOSERS].clear();
        self.search_tracks.set_vec(Vec::new());
        self.search_preview_tracks.set_vec(Vec::new());

        let search = w.global::<SearchState>();
        search.set_active_query(query.as_str().into());
        search.set_tab(0);
        search.set_loading(true);
        search.set_tab_loading(false);
        clear_overview_models(&search);

        self.send(Command::Search(query));
    }

    pub(super) fn select_search_tab(&self, w: &AppWindow, tab: i32) {
        let tab = tab.clamp(0, 4);
        let search = w.global::<SearchState>();
        search.set_tab(tab);
        if tab == 0 {
            search.set_tab_loading(false);
            return;
        }
        let (query, already_loaded) = {
            let state = self.search.borrow();
            (
                state.active_query.clone(),
                state.loaded_tabs[usize::try_from(tab).unwrap_or(0)],
            )
        };
        if query.is_empty() || already_loaded {
            search.set_tab_loading(false);
            return;
        }
        let Some(category) = category_for_tab(tab) else {
            return;
        };
        search.set_tab_loading(true);
        self.send(Command::SearchCategory { query, category });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn search_overview_loaded(
        &self,
        w: &AppWindow,
        query: String,
        best_match: Option<SearchBestMatch>,
        artists: Vec<ArtistItem>,
        artists_total: usize,
        albums: Vec<AlbumItem>,
        albums_total: usize,
        tracks: Vec<TrackItem>,
        tracks_total: usize,
        composers: Vec<ArtistItem>,
        composers_total: usize,
    ) {
        if query != self.search.borrow().active_query {
            return;
        }
        self.unbind(Bind::Search);
        {
            let mut state = self.search.borrow_mut();
            state.best_match = best_match.clone();
            state.loaded_tabs[0] = true;
        }
        self.search_preview_tracks
            .set_vec(track_rows(tracks.clone(), 0));

        let best_favorite = best_match.as_ref().is_some_and(|b| match b.category {
            SearchCategory::Artists => artists.iter().any(|x| x.id == b.id && x.favorite),
            SearchCategory::Albums => albums.iter().any(|x| x.id == b.id && x.favorite),
            SearchCategory::Tracks => tracks.iter().any(|x| x.id == b.id && x.favorite),
            SearchCategory::Composers => composers.iter().any(|x| x.id == b.id && x.favorite),
        });
        let search = w.global::<SearchState>();
        let (best_slint, best_art) = best_match_row(best_match.as_ref(), best_favorite);
        search.set_best_match(best_slint);
        if !best_art.is_empty() {
            let w_weak = self.window.clone();
            self.bind_image(
                w,
                Bind::Search,
                &best_art,
                BEST_ART,
                ArtworkFit::Cover,
                move |image| {
                    if let Some(w) = w_weak.upgrade() {
                        let s = w.global::<SearchState>();
                        let mut bm = s.get_best_match();
                        if bm.has {
                            bm.cover = image.clone();
                            s.set_best_match(bm);
                        }
                    }
                },
            );
        }

        let artist_model = Rc::new(VecModel::<SearchCompactCard>::default());
        let mut artist_rows = Vec::with_capacity(artists.len());
        let mut artist_arts = Vec::with_capacity(artists.len());
        for a in artists {
            artist_arts.push(a.image.clone());
            artist_rows.push(artist_compact_card(&a));
        }
        artist_model.set_vec(artist_rows);
        self.bind_compact_covers(w, &artist_model, &artist_arts);

        let album_model = Rc::new(VecModel::<SearchCompactCard>::default());
        let mut album_rows = Vec::with_capacity(albums.len());
        let mut album_arts = Vec::with_capacity(albums.len());
        for a in albums {
            album_arts.push(a.artwork.clone());
            album_rows.push(album_compact_card(&a));
        }
        album_model.set_vec(album_rows);
        self.bind_compact_covers(w, &album_model, &album_arts);

        let composer_model = Rc::new(VecModel::<SearchCompactCard>::default());
        let mut composer_rows = Vec::with_capacity(composers.len());
        let mut composer_arts = Vec::with_capacity(composers.len());
        for a in composers {
            composer_arts.push(a.image.clone());
            composer_rows.push(composer_compact_card(&a));
        }
        composer_model.set_vec(composer_rows);
        self.bind_compact_covers(w, &composer_model, &composer_arts);

        let track_model = Rc::new(VecModel::<SearchPreviewTrack>::default());
        let mut preview_rows = Vec::with_capacity(tracks.len());
        let mut track_arts = Vec::with_capacity(tracks.len());
        for (i, t) in tracks.into_iter().enumerate() {
            track_arts.push(t.artwork.clone());
            preview_rows.push(preview_track_row(&t, i));
        }
        track_model.set_vec(preview_rows);
        self.bind_track_covers(w, &track_model, &track_arts);

        search.set_artists(ModelRc::from(artist_model));
        search.set_artists_total(count_i32(artists_total));
        search.set_albums(ModelRc::from(album_model));
        search.set_albums_total(count_i32(albums_total));
        search.set_tracks(ModelRc::from(track_model));
        search.set_tracks_total(count_i32(tracks_total));
        search.set_composers(ModelRc::from(composer_model));
        search.set_composers_total(count_i32(composers_total));
        search.set_loading(false);
    }

    pub(super) fn search_category_reset(&self, w: &AppWindow, category: SearchCategory) {
        let tab = tab_for_category(category);
        self.search.borrow_mut().loaded_tabs[tab] = false;
        match category {
            SearchCategory::Artists => self.grids[GRID_SEARCH_ARTISTS].clear(),
            SearchCategory::Albums => self.grids[GRID_SEARCH_ALBUMS].clear(),
            SearchCategory::Tracks => self.search_tracks.set_vec(Vec::new()),
            SearchCategory::Composers => self.grids[GRID_SEARCH_COMPOSERS].clear(),
        }
        let search = w.global::<SearchState>();
        if usize::try_from(search.get_tab()).ok() == Some(tab) {
            search.set_tab_loading(true);
        }
    }

    pub(super) fn search_category_appended(
        &self,
        w: &AppWindow,
        category: SearchCategory,
        artists: Vec<ArtistItem>,
        albums: Vec<AlbumItem>,
        tracks: Vec<TrackItem>,
        total: usize,
    ) {
        let tab = tab_for_category(category);
        self.search.borrow_mut().loaded_tabs[tab] = true;
        let search = w.global::<SearchState>();
        match category {
            SearchCategory::Artists => {
                self.grids[GRID_SEARCH_ARTISTS].append(cards(artists));
                search.set_artists_total(count_i32(total));
            }
            SearchCategory::Albums => {
                self.grids[GRID_SEARCH_ALBUMS].append(cards(albums));
                search.set_albums_total(count_i32(total));
            }
            SearchCategory::Tracks => {
                let offset = self.search_tracks.row_count();
                for row in track_rows(tracks, offset) {
                    self.search_tracks.push(row);
                }
                search.set_tracks_total(count_i32(total));
            }
            SearchCategory::Composers => {
                let items = artists.into_iter().map(composer_card).collect();
                self.grids[GRID_SEARCH_COMPOSERS].append(items);
                search.set_composers_total(count_i32(total));
            }
        }
        if usize::try_from(search.get_tab()).ok() == Some(tab) {
            search.set_tab_loading(false);
        }
    }

    pub(super) fn clear_search_loading_on_notice(&self, w: &AppWindow, text: &str) {
        if text.starts_with("搜索") {
            let search = w.global::<SearchState>();
            search.set_loading(false);
            search.set_tab_loading(false);
        }
    }

    pub(super) fn update_search_favorite(
        &self,
        w: &AppWindow,
        kind: FavoriteKind,
        id: i64,
        favorite: bool,
    ) {
        let search = w.global::<SearchState>();
        let mut best = search.get_best_match();
        let best_matches = best.has
            && i64::from(best.id) == id
            && match kind {
                FavoriteKind::Artist => best.kind == 0 || best.kind == 3,
                FavoriteKind::Album => best.kind == 1,
                FavoriteKind::Track => best.kind == 2,
            };
        if best_matches && best.favorite != favorite {
            best.favorite = favorite;
            search.set_best_match(best);
        }

        match kind {
            FavoriteKind::Artist => {
                set_compact_favorite(&search.get_artists(), id, favorite);
                set_compact_favorite(&search.get_composers(), id, favorite);
            }
            FavoriteKind::Album => {
                set_compact_favorite(&search.get_albums(), id, favorite);
            }
            FavoriteKind::Track => {
                let tracks = search.get_tracks();
                for i in 0..tracks.row_count() {
                    if let Some(mut row) = tracks.row_data(i)
                        && i64::from(row.id) == id
                        && row.favorite != favorite
                    {
                        row.favorite = favorite;
                        tracks.set_row_data(i, row);
                    }
                }
            }
        }
    }

    fn bind_compact_covers(
        &self,
        w: &AppWindow,
        model: &Rc<VecModel<SearchCompactCard>>,
        paths: &[String],
    ) {
        for (i, path) in paths.iter().enumerate() {
            let model = model.clone();
            self.bind_image(
                w,
                Bind::Search,
                path,
                COMPACT_ART,
                ArtworkFit::Cover,
                move |image: &Image| {
                    if let Some(mut row) = model.row_data(i) {
                        row.cover = image.clone();
                        model.set_row_data(i, row);
                    }
                },
            );
        }
    }

    fn bind_track_covers(
        &self,
        w: &AppWindow,
        model: &Rc<VecModel<SearchPreviewTrack>>,
        paths: &[String],
    ) {
        for (i, path) in paths.iter().enumerate() {
            let model = model.clone();
            self.bind_image(
                w,
                Bind::Search,
                path,
                TRACK_ART,
                ArtworkFit::Cover,
                move |image: &Image| {
                    if let Some(mut row) = model.row_data(i) {
                        row.cover = image.clone();
                        model.set_row_data(i, row);
                    }
                },
            );
        }
    }

    fn open_search_best(&self, w: &AppWindow) {
        let Some(best) = self.search.borrow().best_match.clone() else {
            return;
        };
        match best.category {
            SearchCategory::Artists | SearchCategory::Composers => {
                self.navigate(w, Route::Artist(best.id));
            }
            SearchCategory::Albums => self.navigate(w, Route::Album(best.id)),
            SearchCategory::Tracks => {
                let index = self.preview_track_index(best.id).unwrap_or(0);
                self.send(Command::PlayTrack(TrackList::SearchPreview, index));
            }
        }
    }

    fn search_best_action(&self, w: &AppWindow, what: &str, arg: i64) {
        let Some(best) = self.search.borrow().best_match.clone() else {
            return;
        };
        match best.category {
            SearchCategory::Artists | SearchCategory::Composers => match what {
                "open" => self.navigate(w, Route::Artist(best.id)),
                "favorite" => self.send(Command::SetFavorite {
                    kind: FavoriteKind::Artist,
                    id: best.id,
                    favorite: arg != 0,
                }),
                other => self.source_action(other, TrackSource::Artist(best.id), arg),
            },
            SearchCategory::Albums => match what {
                "open" => self.navigate(w, Route::Album(best.id)),
                "favorite" => self.send(Command::SetFavorite {
                    kind: FavoriteKind::Album,
                    id: best.id,
                    favorite: arg != 0,
                }),
                "new-playlist" => {
                    self.open_dialog(
                        w,
                        Dialog::New(Some(TrackSource::Album(best.id))),
                        &best.title,
                        "",
                    );
                }
                other => self.source_action(other, TrackSource::Album(best.id), arg),
            },
            SearchCategory::Tracks => {
                let index = self.preview_track_index(best.id).unwrap_or(0);
                if what == "open" || what == "play" {
                    self.send(Command::PlayTrack(TrackList::SearchPreview, index));
                } else {
                    self.track_action(w, TrackList::SearchPreview, what, index, arg);
                }
            }
        }
    }

    fn search_compact_card_action(&self, w: &AppWindow, kind: i32, what: &str, id: i64, arg: i64) {
        if id <= 0 {
            return;
        }
        let is_album = kind == 1;
        match what {
            "open" => {
                let route = if is_album {
                    Route::Album(id)
                } else {
                    Route::Artist(id)
                };
                self.navigate(w, route);
            }
            "favorite" => self.send(Command::SetFavorite {
                kind: if is_album {
                    FavoriteKind::Album
                } else {
                    FavoriteKind::Artist
                },
                id,
                favorite: arg != 0,
            }),
            "new-playlist" if is_album => {
                let albums = w.global::<SearchState>().get_albums();
                let name = (0..albums.row_count())
                    .filter_map(|i| albums.row_data(i))
                    .find(|c| i64::from(c.id) == id)
                    .map(|c| c.title.to_string())
                    .unwrap_or_default();
                self.open_dialog(w, Dialog::New(Some(TrackSource::Album(id))), &name, "");
            }
            other => {
                let source = if is_album {
                    TrackSource::Album(id)
                } else {
                    TrackSource::Artist(id)
                };
                self.source_action(other, source, arg);
            }
        }
    }

    fn preview_track_index(&self, track_id: i64) -> Option<usize> {
        (0..self.search_preview_tracks.row_count()).find(|&i| {
            self.search_preview_tracks
                .row_data(i)
                .is_some_and(|r| i64::from(r.id) == track_id)
        })
    }
}

fn clear_overview_models(search: &SearchState<'_>) {
    search.set_best_match(SlintBestMatch::default());
    search.set_artists(ModelRc::default());
    search.set_artists_total(0);
    search.set_albums(ModelRc::default());
    search.set_albums_total(0);
    search.set_tracks(ModelRc::default());
    search.set_tracks_total(0);
    search.set_composers(ModelRc::default());
    search.set_composers_total(0);
}

fn category_for_tab(tab: i32) -> Option<SearchCategory> {
    match tab {
        1 => Some(SearchCategory::Artists),
        2 => Some(SearchCategory::Albums),
        3 => Some(SearchCategory::Tracks),
        4 => Some(SearchCategory::Composers),
        _ => None,
    }
}

fn tab_for_category(category: SearchCategory) -> usize {
    match category {
        SearchCategory::Artists => 1,
        SearchCategory::Albums => 2,
        SearchCategory::Tracks => 3,
        SearchCategory::Composers => 4,
    }
}

fn count_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

fn artist_subtitle(a: &ArtistItem) -> String {
    match (a.album_count > 0, a.track_count > 0) {
        (true, true) => format!("{} 张专辑 · {} 首演唱", a.album_count, a.track_count),
        (true, false) => format!("{} 张专辑", a.album_count),
        (false, true) => format!("{} 首演唱", a.track_count),
        (false, false) => String::new(),
    }
}

fn composer_subtitle(a: &ArtistItem) -> String {
    if a.track_count > 0 {
        format!("作曲 {} 首", a.track_count)
    } else {
        artist_subtitle(a)
    }
}

fn album_meta(a: &AlbumItem) -> String {
    match (a.year > 0, a.track_count > 0) {
        (true, true) => format!("{} · {} 首", a.year, a.track_count),
        (true, false) => a.year.to_string(),
        (false, true) => format!("{} 首", a.track_count),
        (false, false) => String::new(),
    }
}

fn best_match_row(best: Option<&SearchBestMatch>, favorite: bool) -> (SlintBestMatch, String) {
    let Some(best) = best else {
        return (SlintBestMatch::default(), String::new());
    };
    let (kind, kind_label) = match best.category {
        SearchCategory::Artists => (0, "歌手"),
        SearchCategory::Albums => (1, "专辑"),
        SearchCategory::Tracks => (2, "歌曲"),
        SearchCategory::Composers => (3, "作曲家"),
    };
    let album_id = if best.category == SearchCategory::Albums {
        best.id
    } else {
        best.album_id
    };
    (
        SlintBestMatch {
            has: true,
            kind,
            kind_label: kind_label.into(),
            id: int(best.id),
            album_id: int(album_id),
            title: best.title.as_str().into(),
            subtitle: best.subtitle.as_str().into(),
            cover: Image::default(),
            favorite,
        },
        best.image.clone(),
    )
}

fn artist_compact_card(a: &ArtistItem) -> SearchCompactCard {
    SearchCompactCard {
        id: int(a.id),
        title: a.name.as_str().into(),
        subtitle: artist_subtitle(a).into(),
        meta: SharedString::new(),
        cover: Image::default(),
        is_avatar: true,
        favorite: a.favorite,
    }
}

fn album_compact_card(a: &AlbumItem) -> SearchCompactCard {
    SearchCompactCard {
        id: int(a.id),
        title: a.title.as_str().into(),
        subtitle: a.artist.as_str().into(),
        meta: album_meta(a).into(),
        cover: Image::default(),
        is_avatar: false,
        favorite: a.favorite,
    }
}

fn composer_compact_card(a: &ArtistItem) -> SearchCompactCard {
    SearchCompactCard {
        id: int(a.id),
        title: a.name.as_str().into(),
        subtitle: composer_subtitle(a).into(),
        meta: SharedString::new(),
        cover: Image::default(),
        is_avatar: true,
        favorite: a.favorite,
    }
}

fn composer_card(a: ArtistItem) -> Card {
    let subtitle = composer_subtitle(&a);
    let mut card = Card::from(a);
    card.subtitle = subtitle;
    card
}

fn preview_track_row(t: &TrackItem, index: usize) -> SearchPreviewTrack {
    SearchPreviewTrack {
        id: int(t.id),
        index: count_i32(index),
        title: t.title.as_str().into(),
        artist: t.artist.as_str().into(),
        album: t.album.as_str().into(),
        album_id: int(t.album_id),
        artist_id: int(t.artist_id),
        artists: artist_refs(&t.artists),
        duration: t.duration.as_str().into(),
        cover: Image::default(),
        favorite: t.favorite,
        disabled: false,
    }
}

fn set_compact_favorite(model: &ModelRc<SearchCompactCard>, id: i64, favorite: bool) {
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i)
            && i64::from(row.id) == id
            && row.favorite != favorite
        {
            row.favorite = favorite;
            model.set_row_data(i, row);
        }
    }
}
