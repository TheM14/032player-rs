//! UI side of the 032 pages: home, works, series, daily mixes, playback
//! history, lyrics tools, and images bound to rows by key.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use client_core::m032::{self as views, SpotlightTab};
use client_core::{
    ArtworkFit, Command, DetailView, FavoriteKind, LyricRow, LyricsResultItem, LyricsTools,
    PlayingSources, QueuePosition, TrackItem, TrackList, TrackSource,
};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, VecModel};

use super::{Route, Ui, int, track_row};
use crate::cards::{Card, image_key, physical_box};
use crate::{
    AlbumWorkRow, AppWindow, FactRow, Home, HomeRow, HomeStats, Lib032, LyricLine, LyricsResult,
    LyricsSearch, MixInfo, MixTag, NamedItem, PersonRow, SeriesInfo, SeriesPane, WorkInfo,
    WorkUseRow, YearBar,
};

/// Card grids of the 032 pages (see app.slint).
pub(super) const GRID_WORKS: usize = 8;
pub(super) const GRID_SERIES: usize = 9;
pub(super) const GRID_HOME_WORKS: usize = 10;
pub(super) const GRID_HOME_ALBUMS: usize = 11;
pub(super) const GRID_CHRONICLE: usize = 12;
pub(super) const GRID_HOME_PLAYLISTS: usize = 13;
pub(super) const GRID_WORK_ALBUMS: usize = 14;
pub(super) const GRID_MEMBERS: usize = 15;
pub(super) const GRID_SERIES_ALBUMS: usize = 16;

/// Pages of app.slint.
pub(super) const PAGE_HOME: i32 = 20;
pub(super) const PAGE_WORKS: i32 = 21;
pub(super) const PAGE_SERIES: i32 = 22;
pub(super) const PAGE_HISTORY: i32 = 23;

/// Track list ids of app.slint.
pub(super) fn track_list(id: i32) -> TrackList {
    match id {
        1 => TrackList::Favorites,
        2 => TrackList::Detail,
        3 => TrackList::Search,
        4 => TrackList::Spotlight(SpotlightTab::Recent),
        5 => TrackList::Spotlight(SpotlightTab::Listening),
        6 => TrackList::Spotlight(SpotlightTab::Random),
        7 => TrackList::HomeResume,
        8 => TrackList::HomeRecent,
        9 => TrackList::HomeSongs,
        10 => TrackList::History,
        11 => TrackList::Related,
        12 => TrackList::ArtistFavorites,
        13 => TrackList::SearchPreview,
        14 => TrackList::ArtistCredits,
        _ => TrackList::Library,
    }
}

fn spotlight_list(tab: SpotlightTab) -> i32 {
    match tab {
        SpotlightTab::Listening => 5,
        SpotlightTab::Random => 6,
        _ => 4,
    }
}

fn tab_id(tab: SpotlightTab) -> i32 {
    match tab {
        SpotlightTab::Recent => 0,
        SpotlightTab::Listening => 1,
        SpotlightTab::Series => 2,
        SpotlightTab::Random => 3,
    }
}

fn tab_from_id(id: i32) -> SpotlightTab {
    match id {
        1 => SpotlightTab::Listening,
        2 => SpotlightTab::Series,
        3 => SpotlightTab::Random,
        _ => SpotlightTab::Recent,
    }
}

/// The home page is reloaded when shown after this long.
const HOME_STALE: Duration = Duration::from_secs(60);
// Image boxes (logical px); they must match the `.slint` sizes, since
// covers are decoded to exactly the box and drawn 1:1.
/// Home rows (`HomeRowView`, plain).
const ROW_COVER: (f32, f32) = (48.0, 48.0);
/// 接着听 cards (`HomeRowView`, `card`).
const CARD_ROW_COVER: (f32, f32) = (84.0, 84.0);
/// Daily mix rows (`HomeRowView`, `mix`: 64, 52 on narrow windows) and
/// a quarter of their 2x2 mosaic.
const MIX_COVER: (f32, f32) = (64.0, 64.0);
const MIX_TILE: (f32, f32) = (32.0, 32.0);
/// Spotlight series album rows (`PreviewRow`).
const PREVIEW_COVER: (f32, f32) = (44.0, 44.0);
/// Album inspector people (`AlbumInspector`).
const PERSON_AVATAR: (f32, f32) = (36.0, 36.0);
/// Work posters: album inspector and the "◆ OP" popover, header capsules.
const WORK_POSTER: (f32, f32) = (72.0, 108.0);
const WORK_CHIP: (f32, f32) = (20.0, 30.0);
/// Sidebar playlists.
const SIDE_COVER: (f32, f32) = (42.0, 42.0);
/// Work / series page poster (`DetailHero`).
const DETAIL_POSTER: (f32, f32) = (160.0, 240.0);
/// Daily mix page cover: one, or a 2x2 mosaic.
const MIX_PAGE_COVER: (f32, f32) = (180.0, 180.0);
const MIX_PAGE_TILE: (f32, f32) = (90.0, 90.0);
const WORK_TYPES: [&str; 4] = ["all", "anime", "game", "movie"];
const LANGUAGES: [&str; 3] = ["zh-Hans", "zh-Hant", "en"];

/// Groups of image bindings, dropped together when their rows are replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Bind {
    Home,
    /// Spotlight art (re-bound when its size changes).
    Hero,
    Detail,
    /// Queue thumbnails (kept until they arrive; the queue caches them).
    Queue,
    /// Sidebar playlist covers.
    Side,
    /// Unified search overview images (best match, compact cards, preview tracks).
    Search,
}

type Setter = Box<dyn Fn(&Image)>;

#[derive(Default)]
pub(super) struct M032State {
    /// Base URL of the server (for web console pages).
    pub server: String,
    panes: Vec<views::AlbumPane>,
    series_pane: Option<views::SeriesPane>,
    tab: SpotlightTab,
    home_loaded_at: Option<Instant>,
    works_loaded: bool,
    series_loaded: bool,
    pub sources: PlayingSources,
    /// Open work / series / mix page.
    work: Option<views::WorkView>,
    series: Option<views::SeriesView>,
    mix: Option<views::MixView>,
    bindings: HashMap<String, Vec<(Bind, Setter)>>,
    /// Open album: track id -> first linked work (the "◆ OP" marker).
    tieup_works: HashMap<i64, i64>,
}

impl M032State {
    pub fn reset(&mut self) {
        let server = std::mem::take(&mut self.server);
        *self = Self {
            server,
            ..Self::default()
        };
    }
}

fn s(text: &str) -> SharedString {
    SharedString::from(text)
}

fn rc<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn home_row(t: &TrackItem, index: usize) -> HomeRow {
    let subtitle = if t.album.is_empty() {
        t.artist.clone()
    } else if t.artist.is_empty() {
        t.album.clone()
    } else {
        format!("{} · {}", t.artist, t.album)
    };
    HomeRow {
        index: i32::try_from(index).unwrap_or(i32::MAX),
        id: int(t.id),
        title: s(&t.title),
        subtitle: s(&subtitle),
        album_id: int(t.album_id),
        artist_id: int(t.artist_id),
        artists: super::artist_refs(&t.artists),
        favorite: t.favorite,
        duration: s(&t.duration),
        percent: -1,
        ..HomeRow::default()
    }
}

fn work_card(w: &views::WorkItem, home: bool) -> Card {
    let (artist, year) = if home || w.subtitle.is_empty() {
        (w.kind.clone(), String::new())
    } else {
        (w.subtitle.clone(), w.kind.clone())
    };
    let note = if home {
        w.roles.clone()
    } else if w.track_count > 0 {
        format!("{} 首", w.track_count)
    } else {
        String::new()
    };
    Card {
        id: w.id,
        title: w.title.clone(),
        subtitle: artist,
        year: 0,
        artwork: w.artwork.clone(),
        // The home's work card ends with "收录 N 首".
        added: if home && w.track_count > 0 {
            format!("收录 {} 首", w.track_count)
        } else {
            String::new()
        },
        // The home's horizontal card prints "系列：<badge>".
        badge: if home {
            w.series_title.clone()
        } else {
            String::new()
        },
        note: if year.is_empty() {
            note
        } else {
            format!("{year} · {note}")
        },
        ..Card::default()
    }
}

impl Ui {
    // ------------------------------------------------------------ images

    /// Loads `path` for a `(width, height)` logical box using `fit` and
    /// hands it to `set` (once).
    pub(super) fn bind_image(
        &self,
        w: &AppWindow,
        group: Bind,
        path: &str,
        (width, height): (f32, f32),
        fit: ArtworkFit,
        set: impl Fn(&Image) + 'static,
    ) {
        if path.is_empty() {
            return;
        }
        let px = physical_box(width, height, self.scale(w));
        let key = image_key(path, px, fit);
        let first = {
            let mut state = self.m032.borrow_mut();
            let slots = state.bindings.entry(key.clone()).or_default();
            slots.push((group, Box::new(set)));
            slots.len() == 1
        };
        if first {
            self.send(Command::LoadImage {
                key,
                path: path.to_owned(),
                width: px.0,
                height: px.1,
                fit,
            });
        }
    }

    pub(super) fn unbind(&self, group: Bind) {
        let mut state = self.m032.borrow_mut();
        state.bindings.retain(|_, slots| {
            slots.retain(|(g, _)| *g != group);
            !slots.is_empty()
        });
    }

    pub(super) fn image_arrived(&self, key: &str, image: &Image) {
        let slots = self.m032.borrow_mut().bindings.remove(key);
        for (_, set) in slots.into_iter().flatten() {
            set(image);
        }
    }

    /// Covers of home-style rows, set into `model` as they arrive.
    fn bind_row_covers(
        &self,
        w: &AppWindow,
        group: Bind,
        model: &std::rc::Rc<VecModel<HomeRow>>,
        paths: &[String],
        size: (f32, f32),
    ) {
        for (i, path) in paths.iter().enumerate() {
            let model = model.clone();
            self.bind_image(w, group, path, size, ArtworkFit::Contain, move |image| {
                if let Some(mut row) = model.row_data(i) {
                    row.cover = image.clone();
                    model.set_row_data(i, row);
                }
            });
        }
    }

    // ------------------------------------------------------------ routes

    /// A 032 page was shown: load what it needs.
    pub(super) fn page_shown(&self, w: &AppWindow, page: i32) {
        match page {
            PAGE_HOME => {
                let stale = self
                    .m032
                    .borrow()
                    .home_loaded_at
                    .is_none_or(|t| t.elapsed() > HOME_STALE);
                if stale {
                    self.m032.borrow_mut().home_loaded_at = Some(Instant::now());
                    self.send(Command::LoadHome);
                }
            }
            PAGE_WORKS => {
                if !std::mem::replace(&mut self.m032.borrow_mut().works_loaded, true) {
                    self.send(Command::QueryWorks(client_core::WorkListQuery::default()));
                }
            }
            PAGE_SERIES => {
                let supported = w.global::<Lib032>().get_series_supported();
                if supported && !std::mem::replace(&mut self.m032.borrow_mut().series_loaded, true)
                {
                    self.send(Command::QuerySeries(String::new()));
                }
            }
            PAGE_HISTORY => self.send(Command::QueryHistory),
            _ => {}
        }
    }

    /// Opens a page of the 032 web console in the browser.
    pub(super) fn open_web(&self, path: &str) {
        let server = self.m032.borrow().server.clone();
        if !server.is_empty() {
            os::browser::open_url(&format!("{server}{path}"));
        }
    }

    // ------------------------------------------------------------ home

    pub(super) fn select_tab(&self, w: &AppWindow, tab: i32) {
        self.show_tab(w, tab_from_id(tab));
    }

    pub(super) fn home_loaded(&self, w: &AppWindow, view: views::HomeView) {
        let home = w.global::<Home>();
        self.unbind(Bind::Home);
        home.set_loading(false);
        home.set_unavailable(false);
        home.set_empty(view.empty);
        {
            let mut state = self.m032.borrow_mut();
            state.panes = view
                .spotlight
                .as_ref()
                .map(|s| s.albums.clone())
                .unwrap_or_default();
            state.series_pane = view.spotlight.as_ref().and_then(|s| s.series.clone());
        }
        let (labels, ids, default) = view.spotlight.as_ref().map_or_else(
            || (Vec::new(), Vec::new(), SpotlightTab::Recent),
            |spot| {
                (
                    spot.tabs.iter().map(|t| s(t.label())).collect(),
                    spot.tabs.iter().map(|t| tab_id(*t)).collect(),
                    spot.default_tab,
                )
            },
        );
        home.set_tab_labels(rc(labels));
        home.set_tab_ids(rc(ids));
        self.show_tab(w, default);

        let resume: Vec<HomeRow> = view
            .resume
            .iter()
            .enumerate()
            .map(|(i, r)| HomeRow {
                percent: r.percent,
                duration: s(&r.progress),
                note: s(&r.when),
                ..home_row(&r.track, i)
            })
            .collect();
        self.home_resume.set_vec(resume);
        let covers: Vec<String> = view.resume.iter().map(|r| r.artwork.clone()).collect();
        self.bind_row_covers(w, Bind::Home, &self.home_resume, &covers, CARD_ROW_COVER);

        // Electron rows: title, artists, "入库于 …" (no year).
        self.grids[GRID_HOME_ALBUMS].set(
            view.recent_albums
                .iter()
                .cloned()
                .map(|a| {
                    let added = client_core::format::relative_time(&a.added_at);
                    Card {
                        year: 0,
                        added: if added.is_empty() {
                            String::new()
                        } else {
                            format!("入库于 {added}")
                        },
                        ..Card::from(a)
                    }
                })
                .collect(),
        );
        self.home_works(w, view.works);
        self.home_songs(w, view.songs);
        match view.chronicle {
            Some(c) => self.chronicle(w, c),
            None => {
                home.set_years(rc(Vec::new()));
                self.grids[GRID_CHRONICLE].clear();
            }
        }
        self.grids[GRID_HOME_PLAYLISTS].set(view.playlists.into_iter().map(Card::from).collect());
        let host = super::server_host(&self.m032.borrow().server);
        w.set_server_status(
            if view.stats.tracks > 0 {
                format!("{host} · {} 首", super::thousands(view.stats.tracks))
            } else {
                host
            }
            .into(),
        );
        home.set_stats(HomeStats {
            albums: int(view.stats.albums),
            tracks: int(view.stats.tracks),
            works: int(view.stats.works),
            last_scan: s(&view.stats.last_scan),
            pending: int(view.stats.pending_review),
        });
    }

    fn show_tab(&self, w: &AppWindow, tab: SpotlightTab) {
        let home = w.global::<Home>();
        // Spotlight art follows the width (clamp(180px, 17vw, 244px)).
        self.unbind(Bind::Hero);
        let art = home.get_hero_art().max(1.0);
        self.m032.borrow_mut().tab = tab;
        home.set_tab(tab_id(tab));
        if tab == SpotlightTab::Series {
            let Some(pane) = self.m032.borrow().series_pane.clone() else {
                return;
            };
            home.set_series(SeriesPane {
                is_series: pane.is_series,
                series_id: int(pane.series_id),
                first_work_id: int(pane.first_work_id),
                title: s(&pane.title),
                by: s(&pane.by),
                facts: s(&pane.facts.join(" · ")),
                album_count: int(pane.album_count),
            });
            home.set_series_works(rc(pane
                .works
                .iter()
                .map(|(id, title, meta)| NamedItem {
                    id: int(*id),
                    title: s(title),
                    meta: s(meta),
                    ..NamedItem::default()
                })
                .collect()));
            let albums = std::rc::Rc::new(VecModel::from(
                pane.albums
                    .iter()
                    .map(|a| NamedItem {
                        id: int(a.id),
                        title: s(&a.title),
                        meta: s(&a.subtitle),
                        year: s(&a.year),
                        cover: Image::default(),
                    })
                    .collect::<Vec<_>>(),
            ));
            home.set_series_albums(ModelRc::from(albums.clone()));
            for (i, a) in pane.albums.iter().enumerate() {
                let albums = albums.clone();
                self.bind_image(
                    w,
                    Bind::Home,
                    &a.artwork,
                    PREVIEW_COVER,
                    ArtworkFit::Contain,
                    move |image| {
                        if let Some(mut row) = albums.row_data(i) {
                            row.cover = image.clone();
                            albums.set_row_data(i, row);
                        }
                    },
                );
            }
            let fan = std::rc::Rc::new(VecModel::from(vec![Image::default(); pane.fan.len()]));
            home.set_series_fan(ModelRc::from(fan.clone()));
            for (i, path) in pane.fan.iter().enumerate() {
                let fan = fan.clone();
                let side = (art * 0.77).floor();
                self.bind_image(
                    w,
                    Bind::Hero,
                    path,
                    (side, side),
                    ArtworkFit::Cover,
                    move |image| {
                        fan.set_row_data(i, image.clone());
                    },
                );
            }
            return;
        }
        let pane = self
            .m032
            .borrow()
            .panes
            .iter()
            .find(|p| p.tab == tab)
            .cloned();
        let Some(pane) = pane else { return };
        home.set_pane(crate::AlbumPane {
            tab: tab_id(tab),
            list: spotlight_list(tab),
            album_id: int(pane.album.id),
            title: s(&pane.album.title),
            artist: s(&pane.album.artist),
            artist_id: int(pane.album.artist_id),
            artists: super::artist_refs(&pane.album.artists),
            facts: s(&pane.facts.join(" · ")),
            spec: s(&pane.spec),
            note: s(&pane.note),
            resume: s(&pane.resume),
            more: s(&pane.more),
            favorite: pane.album.favorite,
            cover: Image::default(),
            track_count: int(pane.track_count),
        });
        self.pane_tracks.set_vec(
            pane.tracks
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, t)| {
                    let mut row = track_row(t, i, false);
                    if row.number.is_empty() {
                        row.number = (i + 1).to_string().into();
                    }
                    row
                })
                .collect::<Vec<_>>(),
        );
        let weak = self.window.clone();
        let album_id = pane.album.id;
        self.bind_image(
            w,
            Bind::Hero,
            &pane.album.artwork,
            (art, art),
            ArtworkFit::Contain,
            move |image| {
                if let Some(w) = weak.upgrade() {
                    let home = w.global::<Home>();
                    let mut p = home.get_pane();
                    if i64::from(p.album_id) == album_id {
                        p.cover = image.clone();
                        home.set_pane(p);
                    }
                }
            },
        );
    }

    /// A new pane for a tab (换一张).
    pub(super) fn spotlight_pane(&self, w: &AppWindow, pane: views::AlbumPane) {
        let tab = pane.tab;
        {
            let mut state = self.m032.borrow_mut();
            state.panes.retain(|p| p.tab != tab);
            state.panes.push(pane);
        }
        if self.m032.borrow().tab == tab {
            self.show_tab(w, tab);
        }
    }

    pub(super) fn home_works(&self, w: &AppWindow, works: views::Rotation<views::WorkItem>) {
        let home = w.global::<Home>();
        home.set_works_count(int(works.count));
        home.set_works_pages(i32::try_from(works.pages).unwrap_or(0));
        home.set_works_type(
            WORK_TYPES
                .iter()
                .position(|t| *t == works.kind)
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(0),
        );
        let grid = &self.grids[GRID_HOME_WORKS];
        grid.set(works.items.iter().map(|w| work_card(w, true)).collect());
        grid.set_playing(&self.playing_works());
    }

    pub(super) fn home_songs(&self, w: &AppWindow, songs: views::Rotation<views::SongItem>) {
        let home = w.global::<Home>();
        home.set_songs_count(int(songs.count));
        home.set_songs_pages(i32::try_from(songs.pages).unwrap_or(0));
        self.home_songs.set_vec(
            songs
                .items
                .iter()
                .enumerate()
                .map(|(i, song)| HomeRow {
                    subtitle: s(&song.track.artist),
                    tag: s(&song.role),
                    tag_accent: song.primary,
                    link: s(&song.work_title),
                    link_id: int(song.work_id),
                    ..home_row(&song.track, i)
                })
                .collect::<Vec<_>>(),
        );
        let covers: Vec<String> = songs.items.iter().map(|r| r.artwork.clone()).collect();
        self.bind_row_covers(w, Bind::Home, &self.home_songs, &covers, ROW_COVER);
    }

    pub(super) fn home_recent(&self, w: &AppWindow, items: Vec<views::RecordItem>) {
        self.home_recent.set_vec(
            items
                .iter()
                .enumerate()
                .map(|(i, r)| HomeRow {
                    note: s(&r.when),
                    duration: SharedString::new(),
                    ..home_row(&r.track, i)
                })
                .collect::<Vec<_>>(),
        );
        let covers: Vec<String> = items.iter().map(|r| r.artwork.clone()).collect();
        self.bind_row_covers(w, Bind::Home, &self.home_recent, &covers, ROW_COVER);
    }

    pub(super) fn home_mixes(&self, w: &AppWindow, generating: bool, items: &[views::MixItem]) {
        w.global::<Home>().set_mixes_generating(generating);
        self.set_mix_rows(w, Bind::Home, &self.home_mixes, items);
    }

    /// Daily mix rows: a 2x2 mosaic of the first four covers, or the first
    /// cover alone when there are fewer (Electron `DailyMixCovers`).
    fn set_mix_rows(
        &self,
        w: &AppWindow,
        group: Bind,
        model: &std::rc::Rc<VecModel<HomeRow>>,
        items: &[views::MixItem],
    ) {
        let mut rows = mix_rows(items);
        let mut single = Vec::with_capacity(items.len());
        for (row, m) in rows.iter_mut().zip(items) {
            if m.covers.len() >= 4 {
                let tiles = std::rc::Rc::new(VecModel::from(vec![Image::default(); 4]));
                row.covers = ModelRc::from(tiles.clone());
                for (i, path) in m.covers.iter().take(4).enumerate() {
                    let tiles = tiles.clone();
                    self.bind_image(w, group, path, MIX_TILE, ArtworkFit::Cover, move |image| {
                        tiles.set_row_data(i, image.clone());
                    });
                }
                single.push(String::new());
            } else {
                single.push(m.covers.first().cloned().unwrap_or_default());
            }
        }
        model.set_vec(rows);
        self.bind_row_covers(w, group, model, &single, MIX_COVER);
    }

    pub(super) fn chronicle(&self, w: &AppWindow, c: views::ChronicleView) {
        let home = w.global::<Home>();
        home.set_years(rc(c
            .years
            .iter()
            .map(|y| YearBar {
                year: y.year,
                count: int(y.count),
                height: y.height,
            })
            .collect()));
        home.set_year(c.year);
        home.set_year_total(int(c.total));
        // All of the same year: title and artists only (Electron rows).
        self.grids[GRID_CHRONICLE].set(
            c.albums
                .into_iter()
                .map(|a| Card {
                    year: 0,
                    ..Card::from(a)
                })
                .collect(),
        );
    }

    pub(super) fn home_action(&self, w: &AppWindow, action: &str, arg: i64) {
        if action == "pane-artist" {
            self.choose_artist(w, w.global::<Home>().get_pane().artists, arg);
            return;
        }
        let pane = || {
            let state = self.m032.borrow();
            let tab = state.tab;
            state.panes.iter().find(|p| p.tab == tab).cloned()
        };
        let series = || self.m032.borrow().series_pane.clone();
        match action {
            "play-pane" | "append-pane" => {
                if let Some(p) = pane() {
                    let source = TrackSource::Album(p.album.id);
                    self.send(if action == "play-pane" {
                        Command::Play(source)
                    } else {
                        Command::Enqueue {
                            source,
                            at: QueuePosition::Last,
                        }
                    });
                }
            }
            "resume" => self.send(Command::ResumeListening),
            "favorite" => {
                if let Some(p) = pane() {
                    self.send(Command::SetFavorite {
                        kind: FavoriteKind::Album,
                        id: p.album.id,
                        favorite: arg != 0,
                    });
                }
            }
            "reroll" => self.send(Command::HomeReroll),
            "hero-art" => {
                let tab = self.m032.borrow().tab;
                self.show_tab(w, tab);
            }
            "open-album" => {
                if arg > 0 {
                    self.navigate(w, Route::Album(arg));
                }
            }
            "play-series" => {
                if let Some(p) = series() {
                    self.send(Command::Play(if p.is_series {
                        TrackSource::Series(p.series_id)
                    } else {
                        TrackSource::Work(p.first_work_id)
                    }));
                }
            }
            "open-series" => {
                if let Some(p) = series() {
                    if p.is_series {
                        self.open_series(w, p.series_id);
                    } else if p.first_work_id > 0 {
                        self.navigate(w, Route::Work(p.first_work_id));
                    }
                }
            }
            "works-type" => {
                let kind = usize::try_from(arg)
                    .ok()
                    .and_then(|i| WORK_TYPES.get(i))
                    .copied()
                    .unwrap_or("all");
                w.global::<Home>().set_works_type(int(arg));
                self.send(Command::HomeWorks {
                    kind: Some(kind.to_owned()),
                    next: false,
                });
            }
            "works-next" => self.send(Command::HomeWorks {
                kind: None,
                next: true,
            }),
            "songs-next" => self.send(Command::HomeSongsNext),
            "year" => {
                if let Ok(year) = i32::try_from(arg) {
                    w.global::<Home>().set_year(year);
                    self.send(Command::HomeChronicle(year));
                }
            }
            "year-all" => {
                // The albums page filtered to that year.
                let mut q = w.get_q_albums();
                q.year_from = arg.to_string().into();
                q.year_to = arg.to_string().into();
                w.set_q_albums(q.clone());
                self.query(w, 0, q);
                self.navigate(w, Route::Page(super::PAGE_ALBUMS));
            }
            "mix-play" => self.send(Command::Play(TrackSource::DailyMix(arg))),
            "mix-open" => self.navigate(w, Route::DailyMix(arg)),
            _ => {}
        }
    }

    /// Series open in the client, or on the web without the series API.
    pub(super) fn open_series(&self, w: &AppWindow, id: i64) {
        if w.global::<Lib032>().get_series_supported() {
            self.navigate(w, Route::Series(id));
        } else {
            self.open_web(&format!("/admin/series/{id}"));
        }
    }

    // ------------------------------------------------------------ lists

    pub(super) fn works_appended(&self, w: &AppWindow, items: &[views::WorkItem], total: usize) {
        let grid = &self.grids[GRID_WORKS];
        grid.append(items.iter().map(|w| work_card(w, false)).collect());
        grid.set_playing(&self.playing_works());
        let lib = w.global::<Lib032>();
        lib.set_works_total(i32::try_from(total).unwrap_or(i32::MAX));
        lib.set_works_loading(false);
    }

    pub(super) fn series_appended(
        &self,
        w: &AppWindow,
        items: Vec<views::SeriesItem>,
        total: usize,
    ) {
        let grid = &self.grids[GRID_SERIES];
        grid.append(
            items
                .into_iter()
                .map(|s| Card {
                    id: s.id,
                    title: s.title,
                    subtitle: s.meta,
                    artwork: s.poster,
                    badge: if s.renamed {
                        "已改名".into()
                    } else {
                        String::new()
                    },
                    ..Card::default()
                })
                .collect(),
        );
        grid.set_playing(&self.playing_series());
        let lib = w.global::<Lib032>();
        lib.set_series_total(i32::try_from(total).unwrap_or(i32::MAX));
        lib.set_series_loading(false);
    }

    pub(super) fn history_appended(
        &self,
        w: &AppWindow,
        items: Vec<views::RecordItem>,
        total: usize,
    ) {
        let offset = self.played.row_count();
        self.played
            .extend(items.into_iter().enumerate().map(|(i, r)| {
                let mut row = track_row(r.track, offset + i, false);
                row.tag = s(&r.state);
                row.tag_accent = r.state == "播放中";
                row.note = s(&format!(
                    "{} · {} · 播放 {} 次",
                    r.when, r.progress, r.play_count
                ));
                row
            }));
        let lib = w.global::<Lib032>();
        lib.set_history_total(i32::try_from(total).unwrap_or(i32::MAX));
        lib.set_history_loading(false);
    }

    /// Card actions of the 032 grids; `false` when the grid is not one.
    pub(super) fn m032_card_action(
        &self,
        w: &AppWindow,
        grid: usize,
        action: &str,
        id: i64,
        arg: i64,
    ) -> bool {
        let source = match grid {
            GRID_WORKS | GRID_HOME_WORKS | GRID_MEMBERS => TrackSource::Work(id),
            GRID_SERIES => TrackSource::Series(id),
            _ => return false,
        };
        match (action, source) {
            ("open", TrackSource::Work(id)) => self.navigate(w, Route::Work(id)),
            ("open", TrackSource::Series(id)) => self.open_series(w, id),
            ("new-playlist", _) => {
                let name = self.grids[grid]
                    .item(id)
                    .map(|c| c.title)
                    .unwrap_or_default();
                self.open_dialog(w, super::Dialog::New(Some(source)), &name, "");
            }
            (other, _) => self.source_action(other, source, arg),
        }
        true
    }

    // ------------------------------------------------------------ details

    pub(super) fn reset_m032_detail(&self, w: &AppWindow) {
        self.unbind(Bind::Detail);
        self.m032.borrow_mut().tieup_works.clear();
        w.set_album_works(rc(Vec::new()));
        w.set_album_release(rc(Vec::new()));
        w.set_album_people(rc(Vec::new()));
        w.set_album_media(rc(Vec::new()));
        w.set_album_review(SharedString::new());
        let lib = w.global::<Lib032>();
        lib.set_work(WorkInfo::default());
        lib.set_work_poster(Image::default());
        lib.set_series_info(SeriesInfo::default());
        lib.set_series_poster(Image::default());
        lib.set_mix(MixInfo::default());
        lib.set_mix_covers(rc(Vec::new()));
        lib.set_mix_tags(rc(Vec::new()));
        self.mix_others.set_vec(Vec::new());
        for grid in [GRID_WORK_ALBUMS, GRID_MEMBERS, GRID_SERIES_ALBUMS] {
            self.grids[grid].clear();
        }
        let mut state = self.m032.borrow_mut();
        state.work = None;
        state.series = None;
        state.mix = None;
    }

    /// Album inspector: release / media facts, people (with avatars), review.
    pub(super) fn album_inspector(&self, w: &AppWindow, album: &client_core::AlbumView) {
        let facts = |rows: &[(String, String)]| {
            rc(rows
                .iter()
                .map(|(label, value)| FactRow {
                    label: s(label),
                    value: s(value),
                })
                .collect())
        };
        w.set_album_release(facts(&album.release_facts));
        w.set_album_media(facts(&album.media_facts));
        w.set_album_review(s(&album.review));
        let people = std::rc::Rc::new(VecModel::from(
            album
                .people
                .iter()
                .map(|p| PersonRow {
                    id: int(p.id),
                    name: s(&p.name),
                    roles: s(&format!("{} · {} 首", p.roles, p.track_count)),
                    avatar: Image::default(),
                })
                .collect::<Vec<_>>(),
        ));
        w.set_album_people(ModelRc::from(people.clone()));
        for (i, p) in album.people.iter().enumerate() {
            let people = people.clone();
            let path = format!("/api/v1/artists/{}/image", p.id);
            self.bind_image(
                w,
                Bind::Detail,
                &path,
                PERSON_AVATAR,
                ArtworkFit::Cover,
                move |image| {
                    if let Some(mut row) = people.row_data(i) {
                        row.avatar = image.clone();
                        people.set_row_data(i, row);
                    }
                },
            );
        }
    }

    /// Works linked to the open album: header capsules, inspector, and the
    /// "◆ OP" marker of the tracks.
    pub(super) fn album_works_loaded(
        &self,
        w: &AppWindow,
        album_id: i64,
        works: &[views::AlbumWorkEntry],
        tieups: &[(i64, String, usize)],
    ) {
        if self.route.get() != Route::Album(album_id) {
            return;
        }
        let model = std::rc::Rc::new(VecModel::from(
            works
                .iter()
                .map(|wk| AlbumWorkRow {
                    id: int(wk.id),
                    title: s(&wk.title),
                    translated: s(&wk.translated),
                    meta: s(&wk.meta),
                    relation: s(&wk.relation),
                    usage: s(&wk.usage),
                    ost: wk.ost,
                    poster: Image::default(),
                    chip: Image::default(),
                    uses: rc(wk
                        .tracks
                        .iter()
                        .map(|u| WorkUseRow {
                            label: s(&u.label),
                            title: s(&u.title),
                        })
                        .collect()),
                })
                .collect::<Vec<_>>(),
        ));
        w.set_album_works(ModelRc::from(model.clone()));
        for (i, wk) in works.iter().enumerate() {
            let model = model.clone();
            let chips = model.clone();
            self.bind_image(
                w,
                Bind::Detail,
                &wk.poster,
                WORK_POSTER,
                ArtworkFit::Cover,
                move |image| {
                    if let Some(mut row) = model.row_data(i) {
                        row.poster = image.clone();
                        model.set_row_data(i, row);
                    }
                },
            );
            self.bind_image(
                w,
                Bind::Detail,
                &wk.poster,
                WORK_CHIP,
                ArtworkFit::Cover,
                move |image| {
                    if let Some(mut row) = chips.row_data(i) {
                        row.chip = image.clone();
                        chips.set_row_data(i, row);
                    }
                },
            );
            let tracks = self.detail_tracks.clone();
            let work_id = int(wk.id);
            self.bind_image(
                w,
                Bind::Detail,
                &wk.poster,
                WORK_POSTER,
                ArtworkFit::Cover,
                move |image| {
                    for i in 0..tracks.row_count() {
                        let Some(row) = tracks.row_data(i) else {
                            continue;
                        };
                        let Some(works) = row
                            .tieup_works
                            .as_any()
                            .downcast_ref::<VecModel<crate::TieupWork>>()
                        else {
                            continue;
                        };
                        for j in 0..works.row_count() {
                            if let Some(mut item) = works.row_data(j)
                                && item.id == work_id
                            {
                                item.poster = image.clone();
                                works.set_row_data(j, item);
                            }
                        }
                    }
                },
            );
        }
        {
            let mut state = self.m032.borrow_mut();
            state.tieup_works.clear();
            for wk in works {
                for u in &wk.tracks {
                    state.tieup_works.entry(u.track_id).or_insert(wk.id);
                }
            }
        }
        for i in 0..self.detail_tracks.row_count() {
            let Some(mut row) = self.detail_tracks.row_data(i) else {
                continue;
            };
            let id = i64::from(row.id);
            if let Some((_, label, count)) = tieups.iter().find(|(t, ..)| *t == id) {
                row.tieup_tooltip = works
                    .iter()
                    .filter_map(|wk| {
                        let uses: Vec<_> = wk
                            .tracks
                            .iter()
                            .filter(|u| u.track_id == id)
                            .map(|u| u.label.as_str())
                            .collect();
                        if uses.is_empty() {
                            return None;
                        }
                        let translated = if wk.translated.is_empty() {
                            String::new()
                        } else {
                            format!("\n{}", wk.translated)
                        };
                        Some(format!(
                            "{}{}\n{} · {}",
                            wk.title,
                            translated,
                            uses.join(" / "),
                            wk.meta
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
                    .into();
                row.tieup_works = rc(works
                    .iter()
                    .filter_map(|wk| {
                        let uses: Vec<_> = wk
                            .tracks
                            .iter()
                            .filter(|u| u.track_id == id)
                            .map(|u| u.label.as_str())
                            .collect();
                        if uses.is_empty() {
                            return None;
                        }
                        Some(crate::TieupWork {
                            id: int(wk.id),
                            title: s(&wk.title),
                            translated: s(&wk.translated),
                            meta: s(&wk.meta),
                            usage: s(&uses.join(" / ")),
                            poster: works
                                .iter()
                                .position(|w| w.id == wk.id)
                                .and_then(|i| model.row_data(i))
                                .map(|w| w.poster)
                                .unwrap_or_default(),
                        })
                    })
                    .collect());
                row.tieup = s(label);
                row.tieup_count = i32::try_from(*count).unwrap_or(i32::MAX);
                self.detail_tracks.set_row_data(i, row);
            }
        }
    }

    /// Sidebar playlist list (Electron `sidebarPlaylistList`).
    pub(super) fn side_playlists(&self, w: &AppWindow, items: &[client_core::PlaylistItem]) {
        self.unbind(Bind::Side);
        let model = std::rc::Rc::new(VecModel::from(
            items
                .iter()
                .map(|p| NamedItem {
                    id: int(p.id),
                    title: s(&p.name),
                    meta: s(&match &p.duration {
                        Some(d) if p.track_count > 0 => format!("{} 首 · {d}", p.track_count),
                        _ => format!("{} 首", p.track_count),
                    }),
                    ..NamedItem::default()
                })
                .collect::<Vec<_>>(),
        ));
        w.set_side_playlists(ModelRc::from(model.clone()));
        for (i, p) in items.iter().enumerate() {
            let model = model.clone();
            self.bind_image(
                w,
                Bind::Side,
                &p.artwork,
                SIDE_COVER,
                ArtworkFit::Contain,
                move |image| {
                    if let Some(mut row) = model.row_data(i) {
                        row.cover = image.clone();
                        model.set_row_data(i, row);
                    }
                },
            );
        }
    }

    pub(super) fn tieup_work(&self, track_id: i64) -> Option<i64> {
        self.m032.borrow().tieup_works.get(&track_id).copied()
    }

    fn bind_poster(&self, w: &AppWindow, path: &str, set: fn(&Lib032, Image)) {
        let weak = self.window.clone();
        self.bind_image(
            w,
            Bind::Detail,
            path,
            DETAIL_POSTER,
            ArtworkFit::Cover,
            move |image| {
                if let Some(w) = weak.upgrade() {
                    set(&w.global::<Lib032>(), image.clone());
                }
            },
        );
    }

    /// Fills a work / series / mix page; `false` for other views.
    pub(super) fn m032_detail(&self, w: &AppWindow, view: &DetailView, rows: usize) -> bool {
        let lib = w.global::<Lib032>();
        match view {
            DetailView::Work(v) => {
                lib.set_work(WorkInfo {
                    id: int(v.id),
                    title: s(&v.title),
                    translated: s(&v.translated),
                    reading: s(&v.reading),
                    facts: s(&v.facts.join(" · ")),
                    bangumi_url: s(&v.bangumi_url),
                    series_id: int(v.series_id),
                    series_title: s(&v.series_title),
                    series_meta: s(&v.series_meta),
                    album_count: i32::try_from(v.albums.len()).unwrap_or(0),
                    collected: i32::try_from(v.collected).unwrap_or(0),
                });
                self.bind_poster(w, &v.poster, |lib, image| lib.set_work_poster(image));
                self.grids[GRID_WORK_ALBUMS].set(
                    v.albums
                        .iter()
                        .map(|a| Card {
                            id: a.album_id,
                            title: a.title.clone(),
                            subtitle: a.meta.clone(),
                            artwork: a.artwork.clone(),
                            badge: a.relation.clone(),
                            note: a
                                .usage
                                .iter()
                                .map(|(title, role, _)| format!("{role} {title}"))
                                .collect::<Vec<_>>()
                                .join(" · "),
                            ..Card::default()
                        })
                        .collect(),
                );
                self.m032.borrow_mut().work = Some(v.clone());
            }
            DetailView::Series(v) => {
                lib.set_series_info(SeriesInfo {
                    id: int(v.id),
                    title: s(&v.title),
                    renamed: v.renamed,
                    facts: s(&v.facts.join(" · ")),
                    member_count: i32::try_from(v.member_count).unwrap_or(0),
                    album_count: i32::try_from(v.albums.len()).unwrap_or(0),
                    playing: self.m032.borrow().sources.series.contains(&v.id),
                });
                self.bind_poster(w, &v.poster, |lib, image| lib.set_series_poster(image));
                let grouped = v.groups.len() > 1;
                let grid = &self.grids[GRID_MEMBERS];
                grid.set(
                    v.groups
                        .iter()
                        .flat_map(|(label, members)| {
                            members.iter().map(move |m| Card {
                                badge: if grouped {
                                    label.clone()
                                } else {
                                    String::new()
                                },
                                ..work_card(m, false)
                            })
                        })
                        .collect(),
                );
                grid.set_playing(&self.playing_works());
                self.grids[GRID_SERIES_ALBUMS].set(
                    v.albums
                        .iter()
                        .map(|a| Card {
                            id: a.album_id,
                            title: a.title.clone(),
                            subtitle: a.meta.clone(),
                            artwork: a.artwork.clone(),
                            ..Card::default()
                        })
                        .collect(),
                );
                self.m032.borrow_mut().series = Some(v.clone());
            }
            DetailView::DailyMix(v) => {
                lib.set_mix(MixInfo {
                    id: int(v.id),
                    title: s(&v.title),
                    description: s(&v.description),
                    kicker: s(&[v.theme.as_str(), v.context.as_str()]
                        .into_iter()
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ")),
                    subtitle: s(&v.subtitle),
                    rating: v.rating,
                    disliked_note: s(&v.disliked_note),
                    current: v.current,
                    replacements_left: int(v.replacements_left),
                    saved_playlist_id: int(v.saved_playlist_id),
                    track_count: i32::try_from(v.rows.len()).unwrap_or(0),
                });
                lib.set_mix_tags(rc(v
                    .tags
                    .iter()
                    .map(|(label, style)| MixTag {
                        label: s(label),
                        style: *style,
                    })
                    .collect()));
                lib.set_mix_reasons(rc(api_reasons()
                    .iter()
                    .map(|r| v.reasons.iter().any(|x| x == r))
                    .collect()));
                let covers =
                    std::rc::Rc::new(VecModel::from(vec![Image::default(); v.covers.len()]));
                lib.set_mix_covers(ModelRc::from(covers.clone()));
                let (size, fit) = if v.covers.len() >= 4 {
                    (MIX_PAGE_TILE, ArtworkFit::Cover)
                } else {
                    (MIX_PAGE_COVER, ArtworkFit::Contain)
                };
                for (i, path) in v.covers.iter().enumerate() {
                    let covers = covers.clone();
                    self.bind_image(w, Bind::Detail, path, size, fit, move |image| {
                        covers.set_row_data(i, image.clone());
                    });
                }
                self.set_mix_rows(w, Bind::Detail, &self.mix_others, &v.others);
                // Rows: 新发现, missing files, 不再推荐.
                for (i, row) in v.rows.iter().enumerate().take(rows) {
                    if let Some(mut r) = self.detail_tracks.row_data(i) {
                        if row.explore {
                            r.tag = "新发现".into();
                            r.tag_accent = true;
                        }
                        if row.missing {
                            r.tag = "文件缺失".into();
                            r.tag_accent = false;
                        }
                        r.disabled = row.missing;
                        r.blocked = row.blocked;
                        self.detail_tracks.set_row_data(i, r);
                    }
                }
                self.m032.borrow_mut().mix = Some(v.clone());
            }
            _ => return false,
        }
        true
    }

    /// Actions of the work / series / mix pages (`Lib032.detail-action`).
    pub(super) fn lib032_action(&self, w: &AppWindow, action: &str, arg: i64) {
        if action == "clear-history" {
            self.send(Command::ClearHistory);
            return;
        }
        let (source, web) = match self.route.get() {
            Route::Work(id) => (TrackSource::Work(id), format!("/admin/works/{id}")),
            Route::Series(id) => (TrackSource::Series(id), format!("/admin/series/{id}")),
            Route::DailyMix(id) => (
                TrackSource::List(TrackList::Detail),
                format!("/admin/daily-mixes/{id}"),
            ),
            _ => return,
        };
        let mix_id = match self.route.get() {
            Route::DailyMix(id) => Some(id),
            _ => None,
        };
        match action {
            "play" => self.send(Command::Play(source)),
            "shuffle" => self.send(Command::ShufflePlay(source)),
            "last" => self.send(Command::Enqueue {
                // Queued mix songs belong to the queue, not the mix.
                source: mix_id.map_or(source, TrackSource::DailyMix),
                at: QueuePosition::Last,
            }),
            "web" => self.open_web(&web),
            "bangumi" => {
                let url = self
                    .m032
                    .borrow()
                    .work
                    .as_ref()
                    .map(|v| v.bangumi_url.clone())
                    .unwrap_or_default();
                if !url.is_empty() {
                    os::browser::open_url(&url);
                }
            }
            "like" | "undo" | "dislike" => {
                let Some(id) = mix_id else { return };
                let (value, reasons) = match action {
                    "like" => (1, Vec::new()),
                    "undo" => (0, Vec::new()),
                    _ => {
                        let chosen = w.global::<Lib032>().get_mix_reasons();
                        let reasons = api_reasons()
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| chosen.row_data(*i).unwrap_or(false))
                            .map(|(_, r)| (*r).to_owned())
                            .collect();
                        (-1, reasons)
                    }
                };
                self.send(Command::RateDailyMix { id, value, reasons });
            }
            "replace" => {
                if let Some(id) = mix_id {
                    self.send(Command::ReplaceDailyMix(id));
                }
            }
            "save" => {
                if let Some(id) = mix_id {
                    self.send(Command::SaveDailyMix(id));
                }
            }
            "saved" if arg > 0 => self.navigate(w, Route::Playlist(arg)),
            _ => {}
        }
    }

    pub(super) fn mix_row_blocked(&self, index: usize, blocked: bool) {
        if let Some(mut r) = self.detail_tracks.row_data(index) {
            r.blocked = blocked;
            self.detail_tracks.set_row_data(index, r);
        }
    }

    // ------------------------------------------------------------ badges

    fn playing_works(&self) -> HashSet<i64> {
        self.m032.borrow().sources.works.iter().copied().collect()
    }

    fn playing_series(&self) -> HashSet<i64> {
        self.m032.borrow().sources.series.iter().copied().collect()
    }

    pub(super) fn playing_sources(&self, w: &AppWindow, sources: PlayingSources) {
        w.set_playing_mix(sources.mix.map_or(-1, int));
        self.m032.borrow_mut().sources = sources;
        let works = self.playing_works();
        for grid in [GRID_WORKS, GRID_HOME_WORKS, GRID_MEMBERS] {
            self.grids[grid].set_playing(&works);
        }
        let series = self.playing_series();
        self.grids[GRID_SERIES].set_playing(&series);
        let lib = w.global::<Lib032>();
        let mut info = lib.get_series_info();
        let playing = series.contains(&i64::from(info.id));
        if info.playing != playing {
            info.playing = playing;
            lib.set_series_info(info);
        }
    }

    // ------------------------------------------------------------ lyrics

    pub(super) fn lyrics_lines(&self, w: &AppWindow, lines: Vec<LyricRow>) {
        w.set_lyrics_translated_lines(lines.iter().any(|l| !l.translation.is_empty()));
        self.lyrics.set_vec(
            lines
                .into_iter()
                .map(|l| LyricLine {
                    text: l.text.into(),
                    translation: l.translation.into(),
                })
                .collect::<Vec<_>>(),
        );
    }

    pub(super) fn lyrics_tools(&self, w: &AppWindow, tools: &LyricsTools) {
        w.set_lyrics_can_search(tools.can_search);
        w.set_lyrics_can_translate(tools.can_translate);
        w.set_lyrics_translation_shown(tools.translation_shown);
        w.set_lyrics_translating(tools.translating);
        w.set_lyrics_offset(int(tools.offset_ms));
    }

    /// Opens the lyrics search prefilled with the current track.
    pub(super) fn open_lyrics_search(&self, w: &AppWindow) {
        let search = w.global::<LyricsSearch>();
        let (name, artist) = self
            .now
            .borrow()
            .as_ref()
            .map(|n| (n.title.clone(), n.artist.clone()))
            .unwrap_or_default();
        search.set_name(s(&name));
        search.set_artist(s(&artist));
        search.set_selected(-1);
        search.set_preview(rc(Vec::new()));
        self.lyrics_results.set_vec(Vec::new());
        search.set_searching(true);
        self.send(Command::SearchLyrics { name, artist });
    }

    pub(super) fn lyrics_results(&self, w: &AppWindow, results: Vec<LyricsResultItem>) {
        let search = w.global::<LyricsSearch>();
        search.set_searching(false);
        search.set_selected(-1);
        search.set_preview(rc(Vec::new()));
        self.lyrics_results.set_vec(
            results
                .into_iter()
                .map(|r| LyricsResult {
                    title: r.title.into(),
                    artist: r.artist.into(),
                    meta: r.meta.into(),
                    score: r.score.into(),
                })
                .collect::<Vec<_>>(),
        );
    }

    pub(super) fn lyrics_preview(w: &AppWindow, lines: Vec<String>) {
        w.global::<LyricsSearch>()
            .set_preview(rc(lines.into_iter().map(SharedString::from).collect()));
    }

    /// Lyrics settings from the settings page.
    pub(super) fn lyrics_settings(w: &AppWindow) -> client_core::LyricsSettings {
        let language = usize::try_from(w.get_lyrics_language())
            .ok()
            .and_then(|i| LANGUAGES.get(i))
            .copied()
            .unwrap_or("zh-Hans");
        client_core::LyricsSettings {
            translation_language: language.to_owned(),
            translation_mode: if w.get_lyrics_mode() == 1 {
                client_core::TranslationMode::Translated
            } else {
                client_core::TranslationMode::Bilingual
            },
            auto_translate: w.get_lyrics_auto(),
        }
    }

    pub(super) fn show_lyrics_settings(w: &AppWindow, settings: &client_core::LyricsSettings) {
        w.set_lyrics_language(
            LANGUAGES
                .iter()
                .position(|l| *l == settings.translation_language)
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(0),
        );
        w.set_lyrics_mode(i32::from(
            settings.translation_mode == client_core::TranslationMode::Translated,
        ));
        w.set_lyrics_auto(settings.auto_translate);
    }
}

/// Dislike reason codes in the order of the checkboxes.
fn api_reasons() -> [&'static str; 5] {
    ["repetitive", "theme", "tracks", "flow", "mismatch"]
}

/// Works page filters: type index into the combo box (0: every type) and
/// sort index (title, year, updated).
pub(super) fn works_query(search: &str, kind: i32, sort: i32) -> client_core::WorkListQuery {
    client_core::WorkListQuery {
        search: search.trim().to_owned(),
        kind: usize::try_from(kind - 1)
            .ok()
            .and_then(|i| views::WORK_TYPES.get(i))
            .map(|k| (*k).to_owned())
            .unwrap_or_default(),
        sort: match sort {
            1 => client_core::WorkSort::Year,
            2 => client_core::WorkSort::Updated,
            _ => client_core::WorkSort::Title,
        },
    }
}

fn mix_rows(items: &[views::MixItem]) -> Vec<HomeRow> {
    items
        .iter()
        .enumerate()
        .map(|(i, m)| HomeRow {
            index: i32::try_from(i).unwrap_or(i32::MAX),
            id: int(m.id),
            title: s(&m.title),
            subtitle: s(&m.subtitle),
            percent: -1,
            dimmed: m.rating == -1,
            ..HomeRow::default()
        })
        .collect()
}

/// Favorite flag of rows on the home page.
pub(super) fn set_row_favorite(model: &VecModel<HomeRow>, id: i64, favorite: bool) {
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
