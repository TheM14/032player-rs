//! UI-thread state: models, callbacks, navigation and applying core events.

mod cast;
mod m032;
mod shortcuts;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use client_core::{
    AlbumFilter, AlbumSort, ArtistFilter, ArtistSort, AudioDevice, Command, Commander, DetailView,
    Event, FavoriteKind, LoginFailure, NowPlaying, PlaylistFilter, PlaylistSort, QueuePosition,
    Repeat, Rgba, RightPanel, ServerFeatures, Settings, SignOutReason, SleepTimer, SortOrder,
    ThemeMode, TrackFilter, TrackItem, TrackList, TrackSort, TrackSource, Transcode, format,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer,
    VecModel,
};

use crate::cards::{Card, CardGridModel, image_key, key_path, physical_box};
use crate::{
    AppWindow, CreditPart, DetailInfo, FactRow, Home, HomeRow, Lib032, ListQuery, LyricLine,
    LyricsResult, LyricsSearch, PlaylistChoice, QueueRow, ReleaseGroupRow, TrackRow,
};
use m032::{
    Bind, GRID_CHRONICLE, GRID_HOME_ALBUMS, GRID_HOME_PLAYLISTS, GRID_HOME_WORKS, GRID_MEMBERS,
    GRID_SERIES, GRID_SERIES_ALBUMS, GRID_WORK_ALBUMS, GRID_WORKS, M032State, PAGE_HOME,
    track_list,
};

/// Card grids, by the `grid` id used in app.slint.
const GRID_ALBUMS: usize = 0;
const GRID_ARTISTS: usize = 1;
const GRID_FAV_ALBUMS: usize = 2;
const GRID_FAV_ARTISTS: usize = 3;
const GRID_SEARCH_ARTISTS: usize = 4;
const GRID_SEARCH_ALBUMS: usize = 5;
const GRID_RELEASES: usize = 6;
const GRID_PLAYLISTS: usize = 7;
/// Release groups of the artist page (专辑, 单曲与 EP, 合辑与现场, 其他发行,
/// 合作发行, 参与作品, 幕后参与), one grid each.
const RELEASE_GRIDS: [usize; 7] = [GRID_RELEASES, 17, 18, 19, 20, 21, 22];
const GRIDS: usize = 23;
/// Grids of album cards: the card of the playing album gets the
/// equalizer badge (Electron `useAlbumPlayState`).
const ALBUM_GRIDS: [usize; 13] = [
    GRID_ALBUMS,
    GRID_FAV_ALBUMS,
    GRID_SEARCH_ALBUMS,
    GRID_RELEASES,
    17,
    18,
    19,
    20,
    21,
    22,
    GRID_HOME_ALBUMS,
    GRID_CHRONICLE,
    GRID_WORK_ALBUMS,
];

/// Pages of app.slint.
const PAGE_ALBUMS: i32 = 0;
const PAGE_TRACKS: i32 = 1;
const PAGE_PLAYLISTS: i32 = 4;
const PAGE_DETAIL: i32 = 10;

/// Queue thumbnails (logical px).
const QUEUE_COVER: f32 = 40.0;
/// Player bar cover (logical px).
const NP_COVER: f32 = 56.0;
/// Playlist artwork upload limit of the server.
const MAX_ARTWORK_BYTES: u64 = 10 * 1024 * 1024;
const MAX_HISTORY: usize = 50;

/// Where the content area is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Page(i32),
    Album(i64),
    Artist(i64),
    Playlist(i64),
    Similar(TrackList, usize),
    Work(i64),
    Series(i64),
    DailyMix(i64),
}

/// What the open playlist dialog does on confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialog {
    New(Option<TrackSource>),
    Edit(i64),
    Delete(i64),
}

/// Sort options of a list: value, label, default descending.
struct Sorts<T> {
    options: Vec<(T, &'static str, bool)>,
}

impl<T: Copy + PartialEq> Sorts<T> {
    fn labels(&self) -> ModelRc<SharedString> {
        ModelRc::new(VecModel::from(
            self.options
                .iter()
                .map(|(_, l, _)| SharedString::from(*l))
                .collect::<Vec<_>>(),
        ))
    }

    fn desc(&self) -> ModelRc<bool> {
        ModelRc::new(VecModel::from(
            self.options.iter().map(|(_, _, d)| *d).collect::<Vec<_>>(),
        ))
    }

    fn get(&self, index: i32) -> Option<T> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.options.get(i))
            .map(|(v, _, _)| *v)
    }

    fn index(&self, value: T) -> i32 {
        self.options
            .iter()
            .position(|(v, _, _)| *v == value)
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or(0)
    }
}

/// Sort menus as in `list-sort-by-dropdown.tsx` (MUSIC032); random and
/// some track sorts need `/browse`.
fn album_sorts(browse: bool) -> Sorts<AlbumSort> {
    let mut options = vec![
        (AlbumSort::Title, "名称", false),
        (AlbumSort::Added, "最近添加", true),
        (AlbumSort::RecentlyPlayed, "最近播放", true),
        (AlbumSort::Date, "发行日期", true),
        (AlbumSort::Year, "发行年份", true),
    ];
    if browse {
        options.push((AlbumSort::Random, "随机", false));
    }
    Sorts { options }
}

fn track_sorts(browse: bool) -> Sorts<TrackSort> {
    let mut options = vec![
        (TrackSort::Album, "专辑", false),
        (TrackSort::Title, "名称", false),
        (TrackSort::Plays, "播放次数", true),
        (TrackSort::RecentlyPlayed, "最近播放", true),
        (TrackSort::Date, "发行日期", true),
        (TrackSort::Year, "发行年份", true),
    ];
    if browse {
        options.push((TrackSort::Added, "最近添加", true));
        options.push((TrackSort::Duration, "时长", false));
        options.push((TrackSort::Random, "随机", false));
    }
    Sorts { options }
}

fn artist_sorts(browse: bool) -> Sorts<ArtistSort> {
    let mut options = vec![
        (ArtistSort::Name, "名称", false),
        (ArtistSort::Albums, "专辑数", true),
        (ArtistSort::Tracks, "曲目数", true),
    ];
    if browse {
        options.push((ArtistSort::Random, "随机", false));
    }
    Sorts { options }
}

fn playlist_sorts() -> Sorts<PlaylistSort> {
    Sorts {
        options: vec![
            (PlaylistSort::Name, "名称", false),
            (PlaylistSort::Duration, "时长", true),
            (PlaylistSort::TrackCount, "曲目数", true),
        ],
    }
}

/// Transcode choices the server offers (index 0: the original file).
fn transcode_options(features: &ServerFeatures) -> Vec<(&'static str, Option<Transcode>)> {
    let mut options = vec![("原始文件（默认，无损）", None)];
    let has = |f: &str| features.transcode.iter().any(|t| t == f);
    if has("flac") {
        options.push(("FLAC（最高 48 kHz）", Some(Transcode::Flac48)));
    }
    if has("mp3") {
        options.push(("MP3 320 kbps", Some(Transcode::Mp3(320))));
        options.push(("MP3 192 kbps", Some(Transcode::Mp3(192))));
        options.push(("MP3 128 kbps", Some(Transcode::Mp3(128))));
    }
    if has("ogg") {
        options.push(("Opus 192 kbps", Some(Transcode::Ogg(192))));
        options.push(("Opus 128 kbps", Some(Transcode::Ogg(128))));
        options.push(("Opus 96 kbps", Some(Transcode::Ogg(96))));
    }
    options
}

fn order(desc: bool) -> SortOrder {
    if desc {
        SortOrder::Desc
    } else {
        SortOrder::Asc
    }
}

fn query(sort: i32, desc: bool) -> ListQuery {
    ListQuery {
        sort,
        desc,
        ..ListQuery::default()
    }
}

/// Year range from the two fields; a reversed range is swapped (the
/// server rejects it).
fn years(q: &ListQuery) -> (Option<i32>, Option<i32>) {
    let parse = |s: &SharedString| s.trim().parse::<i32>().ok().filter(|y| *y > 0);
    match (parse(&q.year_from), parse(&q.year_to)) {
        (Some(a), Some(b)) if a > b => (Some(b), Some(a)),
        range => range,
    }
}

struct Ui {
    window: slint::Weak<AppWindow>,
    commander: Commander,
    grids: Vec<Rc<CardGridModel>>,
    tracks: Rc<VecModel<TrackRow>>,
    fav_tracks: Rc<VecModel<TrackRow>>,
    detail_tracks: Rc<VecModel<TrackRow>>,
    search_tracks: Rc<VecModel<TrackRow>>,
    /// The playback history page.
    played: Rc<VecModel<TrackRow>>,
    /// Preview tracks of the shown spotlight tab.
    pane_tracks: Rc<VecModel<TrackRow>>,
    /// Related songs in the full-screen player (`TrackList::Related`).
    related: Rc<VecModel<TrackRow>>,
    /// "收藏的歌曲" of the open artist page.
    artist_favs: Rc<VecModel<TrackRow>>,
    /// Track the related songs were requested for (0: none).
    related_for: Cell<i64>,
    /// The original cover in the full-screen player.
    full_cover_key: RefCell<Option<String>>,
    home_mixes: Rc<VecModel<HomeRow>>,
    home_recent: Rc<VecModel<HomeRow>>,
    home_resume: Rc<VecModel<HomeRow>>,
    home_songs: Rc<VecModel<HomeRow>>,
    mix_others: Rc<VecModel<HomeRow>>,
    lyrics_results: Rc<VecModel<LyricsResult>>,
    m032: RefCell<M032State>,
    lyrics: Rc<VecModel<LyricLine>>,
    queue: Rc<VecModel<QueueRow>>,
    playlist_choices: Rc<VecModel<PlaylistChoice>>,
    now: RefCell<Option<NowPlaying>>,
    now_cover_key: RefCell<Option<String>>,
    /// The sidebar copy of the current cover.
    now_large_key: RefCell<Option<String>>,
    detail_cover_key: RefCell<Option<String>>,
    detail: RefCell<Option<DetailView>>,
    route: Cell<Route>,
    history: RefCell<Vec<Route>>,
    dialog: Cell<Option<Dialog>>,
    /// Artist filters of the albums / tracks pages (id, name).
    album_artist: RefCell<Option<(i64, String)>>,
    track_artist: RefCell<Option<(i64, String)>>,
    /// Last query sent per target (pages re-announce theirs when shown).
    sent: RefCell<HashMap<i32, (ListQuery, Option<i64>)>>,
    browse: Cell<bool>,
    transcode: Cell<Option<Transcode>>,
    transcode_choices: RefCell<Vec<Option<Transcode>>>,
    notice_timer: Timer,
    /// Polls the scale factor (no change event in Slint) so artwork is
    /// re-decoded when the window moves to a screen with another scale.
    scale_timer: Timer,
    last_scale: Cell<f32>,
    /// Window size seen last while shown and not minimized (saved on quit).
    window_state: Cell<client_core::WindowState>,
    /// The whole queue (play order) and the current position; the queue
    /// model holds the entries matching the queue search.
    queue_all: RefCell<(Vec<client_core::QueueEntry>, Option<usize>)>,
    queue_search: RefCell<String>,
    /// Queue thumbnails by artwork path (40px; queues repeat albums).
    queue_covers: Rc<RefCell<HashMap<String, Image>>>,
    /// Artwork path of each row in the queue model.
    queue_paths: Rc<RefCell<Vec<String>>>,
    /// Release page of a newer version.
    update_url: RefCell<String>,
    /// Engine names behind the device combo box; index 0 (`None`) is the
    /// system default.
    device_names: RefCell<Vec<Option<String>>>,
    selected_device: RefCell<Option<String>>,
    /// Sonos rooms and the output.
    cast: RefCell<cast::CastView>,
}

impl Ui {
    fn device_index(&self) -> i32 {
        let selected = self.selected_device.borrow();
        self.device_names
            .borrow()
            .iter()
            .position(|n| *n == *selected)
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or(0)
    }

    /// Fills the device combo box; a saved device that is not connected
    /// stays listed so the selection is not silently lost.
    fn set_devices(&self, w: &AppWindow, devices: Vec<AudioDevice>) {
        let mut names = vec![None];
        let mut labels: Vec<SharedString> = vec!["系统默认".into()];
        for d in devices.into_iter().filter(|d| d.name != "auto") {
            let label = if d.description.is_empty() {
                d.name.clone()
            } else {
                d.description
            };
            labels.push(label.into());
            names.push(Some(d.name));
        }
        if let Some(selected) = self.selected_device.borrow().clone()
            && !names.contains(&Some(selected.clone()))
        {
            labels.push(format!("{selected}（未连接）").into());
            names.push(Some(selected));
        }
        *self.device_names.borrow_mut() = names;
        w.set_audio_devices(ModelRc::new(VecModel::from(labels)));
        w.set_audio_device_index(self.device_index());
    }

    fn send(&self, command: Command) {
        self.commander.send(command);
    }

    fn track_model(&self, list: TrackList) -> Option<&Rc<VecModel<TrackRow>>> {
        Some(match list {
            TrackList::Library => &self.tracks,
            TrackList::Favorites => &self.fav_tracks,
            TrackList::Detail => &self.detail_tracks,
            TrackList::Search => &self.search_tracks,
            TrackList::Spotlight(_) => &self.pane_tracks,
            TrackList::History => &self.played,
            TrackList::Related => &self.related,
            TrackList::ArtistFavorites => &self.artist_favs,
            TrackList::HomeResume | TrackList::HomeRecent | TrackList::HomeSongs => return None,
        })
    }

    /// Id, title and favorite flag of a row of any track list.
    fn row_basics(&self, list: TrackList, index: usize) -> Option<(i64, String, bool)> {
        if let Some(model) = self.track_model(list) {
            let row = model.row_data(index)?;
            return Some((row.id.into(), row.title.to_string(), row.favorite));
        }
        let model = match list {
            TrackList::HomeResume => &self.home_resume,
            TrackList::HomeRecent => &self.home_recent,
            _ => &self.home_songs,
        };
        let row = model.row_data(index)?;
        Some((row.id.into(), row.title.to_string(), row.favorite))
    }

    fn scale(&self, w: &AppWindow) -> f32 {
        let _ = self;
        w.window().scale_factor()
    }

    // ------------------------------------------------------------ navigation

    fn navigate(&self, w: &AppWindow, route: Route) {
        let current = self.route.get();
        if current != route {
            let mut history = self.history.borrow_mut();
            history.push(current);
            if history.len() > MAX_HISTORY {
                history.remove(0);
            }
        }
        self.show(w, route);
    }

    fn go_back(&self, w: &AppWindow) {
        let previous = self.history.borrow_mut().pop();
        if let Some(route) = previous {
            self.show(w, route);
        }
    }

    fn show(&self, w: &AppWindow, route: Route) {
        // Going to a page (album / artist links, menus) leaves full screen.
        if w.get_full_open() {
            w.set_full_open(false);
            self.full_changed(w);
        }
        self.route.set(route);
        w.set_can_go_back(!self.history.borrow().is_empty());
        let (kind, id) = match route {
            Route::Page(page) => {
                w.set_page(page);
                self.page_shown(w, page);
                return;
            }
            Route::Album(id) => (0, id),
            Route::Artist(id) => (1, id),
            Route::Playlist(id) => (2, id),
            Route::Similar(..) => (3, 0),
            Route::Work(id) => (4, id),
            Route::Series(id) => (5, id),
            Route::DailyMix(id) => (6, id),
        };
        // Empty page of the right kind until the detail arrives.
        *self.detail.borrow_mut() = None;
        *self.detail_cover_key.borrow_mut() = None;
        w.set_detail(DetailInfo {
            kind,
            id: int(id),
            ..DetailInfo::default()
        });
        w.set_detail_cover(Image::default());
        w.set_detail_loading(true);
        self.detail_tracks.set_vec(Vec::new());
        self.artist_favs.set_vec(Vec::new());
        for grid in RELEASE_GRIDS {
            self.grids[grid].clear();
        }
        w.set_artist_groups(ModelRc::default());
        w.set_artist_about(SharedString::new());
        w.set_artist_facts(ModelRc::default());
        w.set_artist_tab(0);
        self.reset_m032_detail(w);
        w.set_page(PAGE_DETAIL);
        self.send(match route {
            Route::Album(id) => Command::OpenAlbum(id),
            Route::Artist(id) => Command::OpenArtist(id),
            Route::Playlist(id) => Command::OpenPlaylist(id),
            Route::Similar(list, index) => Command::OpenSimilar(list, index),
            Route::Work(id) => Command::OpenWork(id),
            Route::Series(id) => Command::OpenSeries(id),
            Route::DailyMix(id) => Command::OpenDailyMix(id),
            Route::Page(_) => return,
        });
    }

    // ------------------------------------------------------------ queries

    /// Sends a list query unless it is the one already shown.
    fn query(&self, w: &AppWindow, target: i32, q: ListQuery) {
        let artist = match target {
            0 => self.album_artist.borrow().as_ref().map(|a| a.0),
            1 => self.track_artist.borrow().as_ref().map(|a| a.0),
            _ => None,
        };
        let key = (q.clone(), artist);
        if self.sent.borrow().get(&target) == Some(&key) {
            return;
        }
        {
            let mut sent = self.sent.borrow_mut();
            sent.insert(target, key);
            // Album artists (2) and all artists (7) share one result list.
            match target {
                2 => sent.remove(&7),
                7 => sent.remove(&2),
                _ => None,
            };
        }
        let browse = self.browse.get();
        let (year_from, year_to) = years(&q);
        let search = q.search.trim().to_owned();
        match target {
            0 | 4 => {
                let filter = AlbumFilter {
                    sort: album_sorts(browse).get(q.sort).unwrap_or(AlbumSort::Added),
                    order: order(q.desc),
                    year_from,
                    year_to,
                    artist_id: artist,
                    search,
                };
                if target == 4 {
                    w.set_fav_loading(true);
                }
                self.send(Command::QueryAlbums {
                    favorites: target == 4,
                    filter,
                });
            }
            1 | 5 => {
                let filter = TrackFilter {
                    sort: track_sorts(browse).get(q.sort).unwrap_or(TrackSort::Title),
                    order: order(q.desc),
                    year_from,
                    year_to,
                    artist_id: artist,
                    search,
                };
                if target == 5 {
                    w.set_fav_loading(true);
                }
                self.send(Command::QueryTracks {
                    favorites: target == 5,
                    filter,
                });
            }
            2 | 6 | 7 => {
                let filter = ArtistFilter {
                    sort: artist_sorts(browse).get(q.sort).unwrap_or(ArtistSort::Name),
                    order: order(q.desc),
                    search,
                    all_roles: target == 7,
                };
                if target == 6 {
                    w.set_fav_loading(true);
                }
                self.send(Command::QueryArtists {
                    favorites: target == 6,
                    filter,
                });
            }
            3 => self.send(Command::QueryPlaylists(PlaylistFilter {
                sort: playlist_sorts().get(q.sort).unwrap_or_default(),
                order: order(q.desc),
                search,
            })),
            _ => {}
        }
    }

    /// Shows the albums (`albums`) or tracks page filtered by an artist.
    fn filter_by_artist(&self, w: &AppWindow, albums: bool, artist: Option<(i64, String)>) {
        let label: SharedString = artist
            .as_ref()
            .map(|(_, name)| format!("艺人：{name}").into())
            .unwrap_or_default();
        if albums {
            *self.album_artist.borrow_mut() = artist;
            w.set_album_filter(label);
            self.query(w, 0, w.get_q_albums());
            if self.route.get() != Route::Page(PAGE_ALBUMS) {
                self.navigate(w, Route::Page(PAGE_ALBUMS));
            }
        } else {
            *self.track_artist.borrow_mut() = artist;
            w.set_track_filter(label);
            self.query(w, 1, w.get_q_tracks());
            if self.route.get() != Route::Page(PAGE_TRACKS) {
                self.navigate(w, Route::Page(PAGE_TRACKS));
            }
        }
    }

    // ------------------------------------------------------------ actions

    fn open_dialog(&self, w: &AppWindow, dialog: Dialog, name: &str, description: &str) {
        self.dialog.set(Some(dialog));
        w.set_dialog_name(name.into());
        w.set_dialog_description(description.into());
        w.set_dialog(match dialog {
            Dialog::New(_) => 1,
            Dialog::Edit(_) => 2,
            Dialog::Delete(_) => 3,
        });
    }

    fn card_action(&self, w: &AppWindow, grid: usize, action: &str, id: i64, arg: i64) {
        if self.m032_card_action(w, grid, action, id, arg) {
            return;
        }
        let card = self.grids.get(grid).and_then(|g| g.item(id));
        let (source, kind) = match grid {
            GRID_ARTISTS | GRID_FAV_ARTISTS | GRID_SEARCH_ARTISTS => {
                (TrackSource::Artist(id), Some(FavoriteKind::Artist))
            }
            GRID_PLAYLISTS | GRID_HOME_PLAYLISTS => (TrackSource::Playlist(id), None),
            _ => (TrackSource::Album(id), Some(FavoriteKind::Album)),
        };
        match action {
            "open" => self.navigate(
                w,
                match source {
                    TrackSource::Artist(id) => Route::Artist(id),
                    TrackSource::Playlist(id) => Route::Playlist(id),
                    _ => Route::Album(id),
                },
            ),
            "favorite" => {
                if let Some(kind) = kind {
                    self.send(Command::SetFavorite {
                        kind,
                        id,
                        favorite: arg != 0,
                    });
                }
            }
            "artist" => self.navigate(w, Route::Artist(arg)),
            "new-playlist" => {
                let name = card.map(|c| c.title).unwrap_or_default();
                self.open_dialog(w, Dialog::New(Some(source)), &name, "");
            }
            "delete" => {
                let name = card.map(|c| c.title).unwrap_or_default();
                w.set_dialog_message(format!("删除歌单「{name}」？此操作不能撤销。").into());
                self.open_dialog(w, Dialog::Delete(id), "", "");
            }
            other => self.source_action(other, source, arg),
        }
    }

    /// Play / shuffle / queue / add-to-playlist actions on a source.
    fn source_action(&self, action: &str, source: TrackSource, arg: i64) {
        match action {
            "play" => self.send(Command::Play(source)),
            "shuffle" => self.send(Command::ShufflePlay(source)),
            "next" => self.send(Command::Enqueue {
                source,
                at: QueuePosition::Next,
            }),
            "last" => self.send(Command::Enqueue {
                source,
                at: QueuePosition::Last,
            }),
            "add" => self.send(Command::AddToPlaylist {
                playlist_id: arg,
                source,
            }),
            _ => {}
        }
    }

    fn open_playlist_id(&self) -> Option<i64> {
        match self.route.get() {
            Route::Playlist(id) => Some(id),
            _ => None,
        }
    }

    fn track_action(&self, w: &AppWindow, list: TrackList, action: &str, index: usize, arg: i64) {
        let row = self.row_basics(list, index);
        match action {
            // Rows of the home page and the history play alone.
            "play"
                if matches!(
                    list,
                    TrackList::HomeResume
                        | TrackList::HomeRecent
                        | TrackList::HomeSongs
                        | TrackList::History
                ) =>
            {
                self.send(Command::PlayTrack(list, index));
            }
            "block" => self.send(Command::BlockRecommendation {
                index,
                blocked: arg != 0,
            }),
            "favorite" => {
                if let Some((id, _, _)) = row {
                    self.send(Command::SetFavorite {
                        kind: FavoriteKind::Track,
                        id,
                        favorite: arg != 0,
                    });
                }
            }
            "similar" => self.navigate(w, Route::Similar(list, index)),
            "info" => self.send(Command::TrackInfo(list, index)),
            "tieup" => {
                if let Some((id, _, _)) = row
                    && let Some(work) = self.tieup_work(id)
                {
                    self.navigate(w, Route::Work(work));
                }
            }
            "work" => self.navigate(w, Route::Work(arg)),
            "album" => self.navigate(w, Route::Album(arg)),
            "artist" => self.navigate(w, Route::Artist(arg)),
            "new-playlist" => {
                let name = row.map(|r| r.1).unwrap_or_default();
                let source = TrackSource::Track(list, index);
                self.open_dialog(w, Dialog::New(Some(source)), &name, "");
            }
            "remove" => {
                if let Some(playlist_id) = self.open_playlist_id() {
                    self.send(Command::RemoveFromPlaylist { playlist_id, index });
                }
            }
            "move" => {
                let Some(playlist_id) = self.open_playlist_id() else {
                    return;
                };
                let Ok(to) = usize::try_from(arg) else { return };
                // Show the new order now; the reload confirms it.
                let model = &self.detail_tracks;
                if index < model.row_count() && to < model.row_count() {
                    let row = model.remove(index);
                    model.insert(to, row);
                    for i in index.min(to)..=index.max(to) {
                        if let Some(mut r) = model.row_data(i) {
                            r.index = i32::try_from(i).unwrap_or(i32::MAX);
                            model.set_row_data(i, r);
                        }
                    }
                }
                self.send(Command::MovePlaylistItem {
                    playlist_id,
                    from: index,
                    to,
                });
            }
            other => self.source_action(other, TrackSource::Track(list, index), arg),
        }
    }

    fn detail_action(&self, w: &AppWindow, action: &str, arg: i64) {
        let Some(view) = self.detail.borrow().clone() else {
            return;
        };
        let (source, add_source) = match &view {
            DetailView::Album(a) => (
                TrackSource::List(TrackList::Detail),
                TrackSource::Album(a.id),
            ),
            DetailView::Artist(a) => (TrackSource::Artist(a.id), TrackSource::Artist(a.id)),
            DetailView::Playlist(_) | DetailView::Similar { .. } => (
                TrackSource::List(TrackList::Detail),
                TrackSource::List(TrackList::Detail),
            ),
            DetailView::Work(_) | DetailView::Series(_) | DetailView::DailyMix(_) => return,
        };
        match (action, &view) {
            ("favorite", DetailView::Album(a)) => self.send(Command::SetFavorite {
                kind: FavoriteKind::Album,
                id: a.id,
                favorite: arg != 0,
            }),
            ("favorite", DetailView::Artist(a)) => self.send(Command::SetFavorite {
                kind: FavoriteKind::Artist,
                id: a.id,
                favorite: arg != 0,
            }),
            ("artist", _) => self.navigate(w, Route::Artist(arg)),
            ("work", _) => self.navigate(w, Route::Work(arg)),
            ("add", _) => self.source_action("add", add_source, arg),
            ("new-playlist", view) => {
                let name = match view {
                    DetailView::Album(a) => a.title.clone(),
                    DetailView::Artist(a) => a.name.clone(),
                    DetailView::Playlist(p) => p.name.clone(),
                    DetailView::Similar { title, .. } => format!("与「{title}」相似"),
                    _ => String::new(),
                };
                self.open_dialog(w, Dialog::New(Some(add_source)), &name, "");
            }
            ("edit", DetailView::Playlist(p)) => {
                self.open_dialog(w, Dialog::Edit(p.id), &p.name, &p.description);
            }
            ("delete", DetailView::Playlist(p)) => {
                w.set_dialog_message(format!("删除歌单「{}」？此操作不能撤销。", p.name).into());
                self.open_dialog(w, Dialog::Delete(p.id), "", "");
            }
            ("artwork", DetailView::Playlist(p)) => self.pick_artwork(p.id),
            ("artwork-reset", DetailView::Playlist(p)) => {
                self.send(Command::SetPlaylistArtwork {
                    playlist_id: p.id,
                    image: None,
                });
            }
            // 播放收藏: the artist's "收藏的歌曲".
            ("play-favorites", DetailView::Artist(_)) => {
                self.send(Command::Play(TrackSource::List(TrackList::ArtistFavorites)));
            }
            // Group buttons of the artist page (Electron `AlbumSection`).
            ("group-play" | "group-next" | "group-last", DetailView::Artist(a)) => {
                let group = usize::try_from(arg).unwrap_or(0);
                let source = TrackSource::ArtistGroup(a.id, group);
                self.source_action(action.trim_start_matches("group-"), source, 0);
            }
            (other, _) => self.source_action(other, source, arg),
        }
    }

    /// Asks for an image file and uploads it as the playlist artwork.
    fn pick_artwork(&self, playlist_id: i64) {
        let Some(path) = os::dialog::pick_image() else {
            return;
        };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_ARTWORK_BYTES {
            self.notice("图片太大（最大 10 MB）".into());
            return;
        }
        match std::fs::read(&path) {
            Ok(bytes) => self.send(Command::SetPlaylistArtwork {
                playlist_id,
                image: Some(bytes),
            }),
            Err(e) => self.notice(format!("读取图片失败：{e}")),
        }
    }

    fn dialog_confirm(&self, name: &str, description: &str) {
        match self.dialog.take() {
            Some(Dialog::New(tracks)) => self.send(Command::CreatePlaylist {
                name: name.to_owned(),
                description: description.to_owned(),
                tracks,
            }),
            Some(Dialog::Edit(id)) => self.send(Command::UpdatePlaylist {
                id,
                name: name.to_owned(),
                description: description.to_owned(),
            }),
            Some(Dialog::Delete(id)) => self.send(Command::DeletePlaylist(id)),
            None => {}
        }
    }

    fn notice(&self, text: String) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        tracing::warn!("{text}");
        w.set_notice(text.into());
        let weak = self.window.clone();
        self.notice_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(5),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.set_notice(SharedString::new());
                }
            },
        );
    }

    // ------------------------------------------------------------ events

    fn logged_in(&self, w: &AppWindow, features: &ServerFeatures) {
        self.m032.borrow_mut().reset();
        let home = w.global::<Home>();
        home.set_loading(true);
        home.set_unavailable(false);
        w.set_has_home(features.home);
        w.set_lyrics_translation_available(features.lyrics_translation);
        let lib = w.global::<Lib032>();
        lib.set_series_supported(features.series);
        for model in [&self.played, &self.pane_tracks] {
            model.set_vec(Vec::new());
        }
        let browse = features.browse;
        self.browse.set(browse);
        w.set_browse(browse);
        w.set_playlist_artwork(features.playlist_artwork);
        let albums = album_sorts(browse);
        let tracks = track_sorts(browse);
        let artists = artist_sorts(browse);
        let playlists = playlist_sorts();
        w.set_album_sorts(albums.labels());
        w.set_album_sort_desc(albums.desc());
        w.set_track_sorts(tracks.labels());
        w.set_track_sort_desc(tracks.desc());
        w.set_artist_sorts(artists.labels());
        w.set_artist_sort_desc(artists.desc());
        w.set_playlist_sorts(playlists.labels());
        w.set_playlist_sort_desc(playlists.desc());

        // The core loads albums and tracks with these at sign-in.
        let initial = TrackFilter::initial(browse);
        let q_albums = query(albums.index(AlbumSort::Added), true);
        let q_tracks = query(tracks.index(initial.sort), initial.order == SortOrder::Desc);
        w.set_q_albums(q_albums.clone());
        w.set_q_tracks(q_tracks.clone());
        w.set_q_fav_albums(q_albums.clone());
        w.set_q_fav_tracks(query(tracks.index(TrackSort::Title), false));
        w.set_q_artists(query(artists.index(ArtistSort::Name), false));
        w.set_q_all_artists(query(artists.index(ArtistSort::Name), false));
        w.set_q_fav_artists(query(artists.index(ArtistSort::Name), false));
        w.set_q_playlists(query(playlists.index(PlaylistSort::Name), false));
        *self.album_artist.borrow_mut() = None;
        *self.track_artist.borrow_mut() = None;
        w.set_album_filter(SharedString::new());
        w.set_track_filter(SharedString::new());
        {
            let mut sent = self.sent.borrow_mut();
            sent.clear();
            sent.insert(0, (q_albums, None));
            sent.insert(1, (q_tracks, None));
        }

        let options = transcode_options(features);
        w.set_transcode_options(ModelRc::new(VecModel::from(
            options
                .iter()
                .map(|(label, _)| SharedString::from(*label))
                .collect::<Vec<_>>(),
        )));
        let choices: Vec<Option<Transcode>> = options.into_iter().map(|(_, t)| t).collect();
        let current = self.transcode.get();
        w.set_transcode_index(
            choices
                .iter()
                .position(|t| *t == current)
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(0),
        );
        *self.transcode_choices.borrow_mut() = choices;

        self.history.borrow_mut().clear();
        self.show(
            w,
            Route::Page(if features.home {
                PAGE_HOME
            } else {
                PAGE_ALBUMS
            }),
        );
        w.set_search_text(SharedString::new());
    }

    fn detail_loaded(&self, w: &AppWindow, view: DetailView, tracks: Vec<TrackItem>) {
        // Only the page on screen.
        let matches = match (&view, self.route.get()) {
            (DetailView::Album(a), Route::Album(id)) => a.id == id,
            (DetailView::Artist(a), Route::Artist(id)) => a.id == id,
            (DetailView::Playlist(p), Route::Playlist(id)) => p.id == id,
            (DetailView::Similar { .. }, Route::Similar(..)) => true,
            (DetailView::Work(v), Route::Work(id)) => v.id == id,
            (DetailView::Series(v), Route::Series(id)) => v.id == id,
            (DetailView::DailyMix(v), Route::DailyMix(id)) => v.id == id,
            _ => false,
        };
        if !matches {
            return;
        }
        let rows = tracks.len();
        let multi_disc = matches!(&view, DetailView::Album(a) if a.multi_disc);
        self.detail_tracks.set_vec(
            tracks
                .into_iter()
                .enumerate()
                .map(|(i, t)| track_row(t, i, multi_disc))
                .collect::<Vec<_>>(),
        );
        if self.m032_detail(w, &view, rows) {
            let (kind, id, title) = match &view {
                DetailView::Work(v) => (4, v.id, v.title.clone()),
                DetailView::Series(v) => (5, v.id, v.title.clone()),
                DetailView::DailyMix(v) => (6, v.id, v.title.clone()),
                _ => (0, 0, String::new()),
            };
            w.set_detail(DetailInfo {
                kind,
                id: int(id),
                title: title.into(),
                ..DetailInfo::default()
            });
            w.set_detail_loading(false);
            *self.detail.borrow_mut() = Some(view);
            return;
        }
        let (info, artwork) = match &view {
            DetailView::Album(a) => {
                let mut meta = Vec::new();
                if a.year > 0 {
                    meta.push(a.year.to_string());
                }
                meta.push(format!("{} 首", a.track_count));
                meta.push(a.duration.clone());
                if !a.genres.is_empty() {
                    meta.push(a.genres.clone());
                }
                if !a.label.is_empty() {
                    meta.push(a.label.clone());
                }
                self.album_inspector(w, a);
                (
                    DetailInfo {
                        kicker: a.kind.as_str().into(),
                        kind: 0,
                        id: int(a.id),
                        title: a.title.as_str().into(),
                        subtitle: a.artist.as_str().into(),
                        subtitle_id: int(a.artist_id),
                        meta: meta.join(" · ").into(),
                        quality: a.quality.clone().unwrap_or_default().into(),
                        favorite: a.favorite,
                        ..DetailInfo::default()
                    },
                    a.artwork.clone(),
                )
            }
            DetailView::Artist(a) => {
                let groups: Vec<ReleaseGroupRow> = client_core::release_groups(&a.releases)
                    .into_iter()
                    .zip(RELEASE_GRIDS)
                    .map(|(group, grid)| {
                        let count = group.items.len();
                        self.grids[grid].set(group.items.into_iter().map(Card::from).collect());
                        ReleaseGroupRow {
                            title: group.title.into(),
                            count: i32::try_from(count).unwrap_or(i32::MAX),
                            grid: i32::try_from(grid).unwrap_or(0),
                            rows: ModelRc::from(self.grids[grid].clone()),
                        }
                    })
                    .collect();
                w.set_artist_groups(ModelRc::new(VecModel::from(groups)));
                let mut about = a.biography.trim().to_owned();
                if !about.is_empty() && !a.biography_source.is_empty() {
                    about.push_str(&format!("\n\n来源：{}", a.biography_source));
                }
                w.set_artist_about(about.into());
                w.set_artist_facts(ModelRc::new(VecModel::from(artist_fact_rows(a))));
                let mut meta = Vec::new();
                if !a.artist_type.is_empty() {
                    meta.push(a.artist_type.clone());
                }
                if !a.country.is_empty() {
                    meta.push(a.country.clone());
                }
                if a.album_count > 0 {
                    meta.push(format!("{} 张专辑", a.album_count));
                }
                if a.track_count > 0 {
                    meta.push(format!("{} 首", a.track_count));
                }
                (
                    DetailInfo {
                        kind: 1,
                        id: int(a.id),
                        title: a.name.as_str().into(),
                        subtitle: artist_alias(a).into(),
                        meta: meta.join(" · ").into(),
                        favorite: a.favorite,
                        ..DetailInfo::default()
                    },
                    a.image.clone(),
                )
            }
            DetailView::Playlist(p) => {
                let mut meta = vec![format!("{} 首", p.track_count)];
                if let Some(d) = &p.duration {
                    meta.push(d.clone());
                }
                (
                    DetailInfo {
                        kind: 2,
                        id: int(p.id),
                        title: p.name.as_str().into(),
                        meta: meta.join(" · ").into(),
                        description: p.description.as_str().into(),
                        custom_artwork: p.has_custom_artwork,
                        ..DetailInfo::default()
                    },
                    p.artwork.clone(),
                )
            }
            DetailView::Similar { title, .. } => (
                DetailInfo {
                    kind: 3,
                    title: format!("与「{title}」相似").into(),
                    meta: format!("{} 首 · 按元数据相似度", self.detail_tracks.row_count()).into(),
                    ..DetailInfo::default()
                },
                String::new(),
            ),
            DetailView::Work(_) | DetailView::Series(_) | DetailView::DailyMix(_) => return,
        };
        w.set_detail(info);
        w.set_detail_loading(false);
        self.load_detail_cover(w, &artwork);
        *self.detail.borrow_mut() = Some(view);
    }

    /// Fills the queue panel: entries matching the search (title or
    /// artist, case-insensitive), the current one marked.
    fn show_queue(&self, w: &AppWindow) {
        let search = self.queue_search.borrow().to_lowercase();
        let all = self.queue_all.borrow();
        let (items, current) = &*all;
        let current_uid = current.and_then(|i| items.get(i)).map(|e| e.uid);
        let mut rows = Vec::new();
        let mut paths = Vec::new();
        let mut missing = Vec::new();
        let mut current_row = None;
        let covers = self.queue_covers.borrow();
        for (n, e) in items.iter().enumerate() {
            if !search.is_empty()
                && !e.title.to_lowercase().contains(&search)
                && !e.artist.to_lowercase().contains(&search)
            {
                continue;
            }
            if Some(e.uid) == current_uid {
                current_row = Some(rows.len());
            }
            let cover = covers.get(&e.artwork).cloned();
            if cover.is_none() && !e.artwork.is_empty() && !missing.contains(&e.artwork) {
                missing.push(e.artwork.clone());
            }
            rows.push(QueueRow {
                uid: i32::try_from(e.uid).unwrap_or(i32::MAX),
                number: int(i64::try_from(n + 1).unwrap_or(i64::MAX)),
                title: e.title.as_str().into(),
                artist: e.artist.as_str().into(),
                duration: e.duration.as_str().into(),
                cover: cover.unwrap_or_default(),
            });
            paths.push(e.artwork.clone());
        }
        drop(covers);
        self.queue.set_vec(rows);
        *self.queue_paths.borrow_mut() = paths;
        // A cover fills every row of its album, now and on later updates.
        for path in missing {
            let queue = self.queue.clone();
            let covers = self.queue_covers.clone();
            let paths = self.queue_paths.clone();
            self.bind_image(
                w,
                m032::Bind::Queue,
                &path.clone(),
                (QUEUE_COVER, QUEUE_COVER),
                move |image| {
                    covers.borrow_mut().insert(path.clone(), image.clone());
                    for (i, p) in paths.borrow().iter().enumerate() {
                        if *p == path
                            && let Some(mut row) = queue.row_data(i)
                        {
                            row.cover = image.clone();
                            queue.set_row_data(i, row);
                        }
                    }
                },
            );
        }
        w.set_queue_total(i32::try_from(items.len()).unwrap_or(i32::MAX));
        w.set_queue_current(row_index(current_row));
    }

    /// Remembers the window size and position; a maximized window keeps
    /// the ones it had before maximizing.
    fn record_window(&self, w: &AppWindow) {
        let win = w.window();
        if !win.is_visible() || win.is_minimized() {
            return;
        }
        let mut state = self.window_state.get();
        state.maximized = win.is_maximized() || win.is_fullscreen();
        if !state.maximized {
            let size = win.size().to_logical(win.scale_factor());
            if size.width > 0.0 && size.height > 0.0 {
                state.width = size.width;
                state.height = size.height;
            }
            let pos = win.position();
            (state.x, state.y) = (Some(pos.x), Some(pos.y));
        }
        self.window_state.set(state);
    }

    fn check_scale(&self, w: &AppWindow) {
        let scale = self.scale(w);
        if (scale - self.last_scale.replace(scale)).abs() < f32::EPSILON {
            return;
        }
        tracing::info!(scale, "scale factor changed, reloading artwork");
        for grid in &self.grids {
            grid.rescale(scale);
        }
        self.queue_covers.borrow_mut().clear();
        self.show_queue(w);
        self.reload_detail_cover(w);
        let artwork = self.now.borrow().as_ref().map(|n| n.artwork.clone());
        if let Some(artwork) = artwork.filter(|a| !a.is_empty()) {
            let px = physical_box(NP_COVER, NP_COVER, scale);
            let key = image_key(&artwork, px);
            self.send(Command::LoadImage {
                key: key.clone(),
                path: artwork,
                width: px.0,
                height: px.1,
            });
            *self.now_cover_key.borrow_mut() = Some(key);
            *self.now_large_key.borrow_mut() = None;
            self.load_now_large(
                w,
                &self
                    .now
                    .borrow()
                    .as_ref()
                    .map(|n| n.artwork.clone())
                    .unwrap_or_default(),
            );
        }
    }

    fn mark_playing_album(&self, album_id: i64) {
        let ids: std::collections::HashSet<i64> =
            (album_id > 0).then_some(album_id).into_iter().collect();
        for grid in ALBUM_GRIDS {
            self.grids[grid].set_playing(&ids);
        }
        // Series pages list albums too.
        self.grids[GRID_SERIES_ALBUMS].set_playing(&ids);
    }

    /// The current cover at the size of the right panel's cover.
    fn load_now_large(&self, w: &AppWindow, artwork: &str) {
        let side = w.get_np_art_side();
        let px = physical_box(side, side, self.scale(w));
        let key = (!artwork.is_empty()).then(|| image_key(artwork, px));
        if *self.now_large_key.borrow() == key {
            return;
        }
        match &key {
            Some(key) => self.send(Command::LoadImage {
                key: key.clone(),
                path: artwork.to_owned(),
                width: px.0,
                height: px.1,
            }),
            None => w.set_np_cover_large(Image::default()),
        }
        *self.now_large_key.borrow_mut() = key;
    }

    /// Full-screen player opened, closed, resized, switched tabs or the
    /// track changed: the original cover at the shown size and, on the
    /// related tab, the current track's similar songs.
    fn full_changed(&self, w: &AppWindow) {
        if !w.get_full_open() {
            // Free the large image; it is reloaded (byte cache) on reopen.
            *self.full_cover_key.borrow_mut() = None;
            w.set_full_cover(Image::default());
            return;
        }
        let (artwork, track_id) = self
            .now
            .borrow()
            .as_ref()
            .map(|n| (n.artwork.clone(), n.track_id))
            .unwrap_or_default();
        let side = w.get_full_cover_side();
        let px = physical_box(side, side, self.scale(w));
        let key = (!artwork.is_empty() && w.get_full_cover_side() > 0.0)
            .then(|| format!("full|{}", image_key(&artwork, px)));
        if *self.full_cover_key.borrow() != key {
            match &key {
                Some(key) => {
                    let same_art = self
                        .full_cover_key
                        .borrow()
                        .as_deref()
                        .is_some_and(|old| old.ends_with(&format!("|{artwork}")));
                    if !same_art {
                        // Until it arrives: the player bar copy.
                        w.set_full_cover(w.get_np_cover());
                    }
                    self.send(Command::LoadOriginalImage {
                        key: key.clone(),
                        path: artwork.clone(),
                        width: px.0,
                        height: px.1,
                    });
                }
                None => w.set_full_cover(Image::default()),
            }
            *self.full_cover_key.borrow_mut() = key;
        }
        if w.get_full_tab() == 1 && track_id > 0 && self.related_for.get() != track_id {
            self.related_for.set(track_id);
            self.related.set_vec(Vec::new());
            w.set_full_related_loading(true);
            self.send(Command::LoadRelated(track_id));
        }
    }

    /// The detail header cover, decoded at the header's cover size
    /// (`detail-art-side`, which follows the window width tier).
    fn load_detail_cover(&self, w: &AppWindow, artwork: &str) {
        let side = w.get_detail_art_side();
        let px = physical_box(side, side, self.scale(w));
        let key = (!artwork.is_empty()).then(|| image_key(artwork, px));
        let old = self.detail_cover_key.replace(key.clone());
        if old == key {
            return;
        }
        // Another size of the same artwork stays until the new one arrives.
        if old.as_deref().and_then(key_path) != Some(artwork) {
            w.set_detail_cover(Image::default());
        }
        if let Some(key) = key {
            self.send(Command::LoadImage {
                key,
                path: artwork.to_owned(),
                width: px.0,
                height: px.1,
            });
        }
    }

    /// Reloads the open detail cover for a new header size or scale.
    fn reload_detail_cover(&self, w: &AppWindow) {
        let path = self
            .detail_cover_key
            .borrow()
            .as_deref()
            .and_then(key_path)
            .map(str::to_owned);
        if let Some(path) = path {
            self.load_detail_cover(w, &path);
        }
    }

    fn favorite_changed(&self, w: &AppWindow, kind: FavoriteKind, id: i64, favorite: bool) {
        match kind {
            FavoriteKind::Track => {
                for model in [&self.home_resume, &self.home_recent, &self.home_songs] {
                    m032::set_row_favorite(model, id, favorite);
                }
                for model in [
                    &self.tracks,
                    &self.fav_tracks,
                    &self.detail_tracks,
                    &self.search_tracks,
                    &self.played,
                    &self.pane_tracks,
                    &self.related,
                    &self.artist_favs,
                ] {
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
                let mut now = self.now.borrow_mut();
                let current = now.as_ref().is_some_and(|n| n.track_id == id)
                    || i64::from(w.get_playing_id()) == id;
                if current {
                    if let Some(now) = now.as_mut() {
                        now.favorite = favorite;
                    }
                    w.set_np_favorite(favorite);
                }
                tracing::debug!(id, favorite, current, "track favorite changed");
            }
            FavoriteKind::Album | FavoriteKind::Artist => {
                for grid in &self.grids {
                    grid.set_favorite(id, favorite);
                }
                if kind == FavoriteKind::Album {
                    let home = w.global::<Home>();
                    let mut pane = home.get_pane();
                    if i64::from(pane.album_id) == id {
                        pane.favorite = favorite;
                        home.set_pane(pane);
                    }
                }
                let mut info = w.get_detail();
                let kind_matches = (kind == FavoriteKind::Album && info.kind == 0)
                    || (kind == FavoriteKind::Artist && info.kind == 1);
                if kind_matches && i64::from(info.id) == id {
                    info.favorite = favorite;
                    w.set_detail(info);
                    if let Some(view) = self.detail.borrow_mut().as_mut() {
                        match view {
                            DetailView::Album(a) => a.favorite = favorite,
                            DetailView::Artist(a) => a.favorite = favorite,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// Window size to save on quit (`None` before the UI is installed).
pub fn window_state() -> Option<client_core::WindowState> {
    let mut state = None;
    with_ui(|ui, w| {
        ui.record_window(w);
        state = Some(ui.window_state.get());
    });
    state
}

fn with_ui(f: impl FnOnce(&Ui, &AppWindow)) {
    let ui = UI.with(|cell| cell.borrow().clone());
    if let Some(ui) = ui
        && let Some(window) = ui.window.upgrade()
    {
        f(&ui, &window);
    }
}

/// Wires callbacks and models; `settings` are the saved ones.
#[allow(clippy::too_many_lines)]
pub fn install(window: &AppWindow, commander: Commander, settings: &Settings) {
    shortcuts::install(window, settings);
    cast::install(window, &commander, settings);
    #[allow(clippy::cast_possible_truncation)]
    window.set_volume(settings.volume as f32);
    let panel = settings.right_panel;
    window.set_panel_open(panel.open);
    window.set_panel_queue_tab(panel.queue_tab);
    window.set_panel_queue(panel.queue);
    window.set_panel_lyrics(panel.lyrics);
    window.set_panel_lyrics_first(panel.lyrics_first);
    window.set_panel_split(panel.split);
    window.set_panel_width(panel.width);
    window.set_sidebar_width(settings.sidebar_width);
    window.set_app_version(env!("CARGO_PKG_VERSION").into());
    window.on_open_logs(|| {
        let dir = crate::log_dir();
        os::browser::open_url(&dir.to_string_lossy());
    });
    window.set_exclusive(settings.exclusive);
    window.set_close_to_tray(settings.close_to_tray);
    window.set_fade_on_pause(settings.fade_on_pause);
    window.set_theme_mode(match settings.theme {
        ThemeMode::System => 0,
        ThemeMode::Dark => 1,
        ThemeMode::Light => 2,
    });
    window.set_repeat(repeat_index(settings.repeat));
    window.set_shuffle(settings.shuffle);
    let grids: Vec<Rc<CardGridModel>> = (0..GRIDS)
        .map(|_| CardGridModel::new(commander.clone()))
        .collect();
    // The artist page's release groups share one Flickable (no ListView).
    for grid in RELEASE_GRIDS {
        grids[grid].set_lazy(true);
    }
    let new_tracks = || Rc::new(VecModel::<TrackRow>::default());
    let ui = Rc::new(Ui {
        window: window.as_weak(),
        commander: commander.clone(),
        grids,
        tracks: new_tracks(),
        fav_tracks: new_tracks(),
        detail_tracks: new_tracks(),
        search_tracks: new_tracks(),
        played: new_tracks(),
        pane_tracks: new_tracks(),
        related: new_tracks(),
        artist_favs: new_tracks(),
        related_for: Cell::new(0),
        full_cover_key: RefCell::default(),
        home_mixes: Rc::new(VecModel::default()),
        home_recent: Rc::new(VecModel::default()),
        home_resume: Rc::new(VecModel::default()),
        home_songs: Rc::new(VecModel::default()),
        mix_others: Rc::new(VecModel::default()),
        lyrics_results: Rc::new(VecModel::default()),
        m032: RefCell::default(),
        lyrics: Rc::new(VecModel::default()),
        queue: Rc::new(VecModel::default()),
        playlist_choices: Rc::new(VecModel::default()),
        now: RefCell::default(),
        now_cover_key: RefCell::default(),
        now_large_key: RefCell::default(),
        detail_cover_key: RefCell::default(),
        detail: RefCell::default(),
        route: Cell::new(Route::Page(PAGE_ALBUMS)),
        history: RefCell::default(),
        dialog: Cell::new(None),
        album_artist: RefCell::default(),
        track_artist: RefCell::default(),
        sent: RefCell::default(),
        browse: Cell::new(true),
        transcode: Cell::new(settings.transcode),
        transcode_choices: RefCell::new(vec![None]),
        notice_timer: Timer::default(),
        scale_timer: Timer::default(),
        queue_all: RefCell::default(),
        queue_covers: Rc::default(),
        queue_paths: Rc::default(),
        queue_search: RefCell::default(),
        last_scale: Cell::new(window.window().scale_factor()),
        window_state: Cell::new(settings.window),
        update_url: RefCell::default(),
        device_names: RefCell::new(vec![None]),
        selected_device: RefCell::new(settings.audio_device.clone()),
        cast: RefCell::default(),
    });
    window.set_album_rows(ModelRc::from(ui.grids[GRID_ALBUMS].clone()));
    window.set_artist_rows(ModelRc::from(ui.grids[GRID_ARTISTS].clone()));
    window.set_fav_album_rows(ModelRc::from(ui.grids[GRID_FAV_ALBUMS].clone()));
    window.set_fav_artist_rows(ModelRc::from(ui.grids[GRID_FAV_ARTISTS].clone()));
    window.set_search_artist_rows(ModelRc::from(ui.grids[GRID_SEARCH_ARTISTS].clone()));
    window.set_search_album_rows(ModelRc::from(ui.grids[GRID_SEARCH_ALBUMS].clone()));
    window.set_playlist_rows(ModelRc::from(ui.grids[GRID_PLAYLISTS].clone()));
    window.set_tracks(ModelRc::from(ui.tracks.clone()));
    window.set_fav_tracks(ModelRc::from(ui.fav_tracks.clone()));
    window.set_detail_tracks(ModelRc::from(ui.detail_tracks.clone()));
    window.set_search_tracks(ModelRc::from(ui.search_tracks.clone()));
    window.set_queue(ModelRc::from(ui.queue.clone()));
    window.set_full_related(ModelRc::from(ui.related.clone()));
    window.set_artist_favs(ModelRc::from(ui.artist_favs.clone()));
    window.on_full_changed(|| with_ui(|ui, w| ui.full_changed(w)));
    window.on_open_update(|| {
        with_ui(|ui, _| {
            let url = ui.update_url.borrow().clone();
            if !url.is_empty() {
                os::browser::open_url(&url);
            }
        });
    });
    let c = commander.clone();
    window.on_now_info(move || c.send(Command::NowTrackInfo));
    {
        let home = window.global::<Home>();
        home.set_pane_tracks(ModelRc::from(ui.pane_tracks.clone()));
        home.set_mixes(ModelRc::from(ui.home_mixes.clone()));
        home.set_recent(ModelRc::from(ui.home_recent.clone()));
        home.set_resume(ModelRc::from(ui.home_resume.clone()));
        home.set_songs(ModelRc::from(ui.home_songs.clone()));
        home.set_works(ModelRc::from(ui.grids[GRID_HOME_WORKS].clone()));
        home.set_recent_albums(ModelRc::from(ui.grids[GRID_HOME_ALBUMS].clone()));
        home.set_chronicle(ModelRc::from(ui.grids[GRID_CHRONICLE].clone()));
        home.set_playlists(ModelRc::from(ui.grids[GRID_HOME_PLAYLISTS].clone()));
        let lib = window.global::<Lib032>();
        lib.set_works(ModelRc::from(ui.grids[GRID_WORKS].clone()));
        lib.set_series(ModelRc::from(ui.grids[GRID_SERIES].clone()));
        lib.set_history(ModelRc::from(ui.played.clone()));
        lib.set_work_albums(ModelRc::from(ui.grids[GRID_WORK_ALBUMS].clone()));
        lib.set_series_members(ModelRc::from(ui.grids[GRID_MEMBERS].clone()));
        lib.set_series_albums(ModelRc::from(ui.grids[GRID_SERIES_ALBUMS].clone()));
        lib.set_mix_others(ModelRc::from(ui.mix_others.clone()));
        window
            .global::<LyricsSearch>()
            .set_results(ModelRc::from(ui.lyrics_results.clone()));
    }
    Ui::show_lyrics_settings(window, &settings.lyrics);
    window.set_lyrics(ModelRc::from(ui.lyrics.clone()));
    window.set_playlist_choices(ModelRc::from(ui.playlist_choices.clone()));
    ui.scale_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(1),
        || {
            with_ui(|ui, w| {
                ui.check_scale(w);
                ui.record_window(w);
            });
        },
    );
    UI.with(|cell| *cell.borrow_mut() = Some(ui));

    let c = commander.clone();
    let weak = window.as_weak();
    window.on_login(move |server, api_token, media_token| {
        if let Some(w) = weak.upgrade() {
            w.set_login_busy(true);
            w.set_login_error(SharedString::new());
        }
        c.send(Command::Login {
            server_url: server.trim().to_owned(),
            api_token: api_token.trim().to_owned(),
            media_token: media_token.trim().to_owned(),
        });
    });
    let c = commander.clone();
    window.on_logout(move || c.send(Command::Logout));

    // Navigation and library.
    window.on_navigate(|page, id| {
        with_ui(|ui, w| {
            let id = i64::from(id);
            let route = match page {
                11 => Route::Artist(id),
                12 => Route::Playlist(id),
                10 => Route::Album(id),
                13 => Route::Work(id),
                14 => {
                    ui.open_series(w, id);
                    return;
                }
                15 => Route::DailyMix(id),
                22 if !w.global::<Lib032>().get_series_supported() => {
                    // Without the series API the list lives on the web.
                    ui.open_web("/admin/series");
                    return;
                }
                page => Route::Page(page),
            };
            ui.navigate(w, route);
        });
    });
    window.on_go_back(|| with_ui(|ui, w| ui.go_back(w)));
    window.on_query_changed(|target, q| with_ui(|ui, w| ui.query(w, target, q)));
    window.on_clear_filter(|target| with_ui(|ui, w| ui.filter_by_artist(w, target == 0, None)));
    window.on_card_layout_changed(|grid, columns, card_size| {
        with_ui(|ui, w| {
            // Posters are 2:3.
            let poster = matches!(
                usize::try_from(grid).unwrap_or(0),
                GRID_WORKS | GRID_SERIES | GRID_HOME_WORKS | GRID_MEMBERS
            );
            let logical = (card_size, card_size * if poster { 1.5 } else { 1.0 });
            if let Some(model) = usize::try_from(grid).ok().and_then(|g| ui.grids.get(g)) {
                model.set_layout(usize::try_from(columns).unwrap_or(1), logical, ui.scale(w));
            }
        });
    });
    window.on_card_row_shown(|grid, row| {
        with_ui(|ui, _| {
            let model = usize::try_from(grid).ok().and_then(|g| ui.grids.get(g));
            if let (Some(model), Ok(row)) = (model, usize::try_from(row)) {
                model.show_row(row);
            }
        });
    });
    window.on_card_limit_changed(|grid, limit| {
        with_ui(|ui, _| {
            if let Some(model) = usize::try_from(grid).ok().and_then(|g| ui.grids.get(g)) {
                model.set_limit(usize::try_from(limit).unwrap_or(0));
            }
        });
    });
    window.on_card_action(|grid, action, id, arg| {
        with_ui(|ui, w| {
            if let Ok(grid) = usize::try_from(grid) {
                ui.card_action(w, grid, &action, id.into(), arg.into());
            }
        });
    });
    window.on_track_action(|list, action, index, arg| {
        with_ui(|ui, w| {
            if let Ok(index) = usize::try_from(index) {
                ui.track_action(w, track_list(list), &action, index, arg.into());
            }
        });
    });
    window
        .on_detail_action(|action, arg| with_ui(|ui, w| ui.detail_action(w, &action, arg.into())));
    window.on_dialog_confirm(|name, description| {
        with_ui(|ui, _| ui.dialog_confirm(name.trim(), description.trim()));
    });
    window.on_new_playlist(|| {
        with_ui(|ui, w| ui.open_dialog(w, Dialog::New(None), "", ""));
    });
    let c = commander.clone();
    window.on_shuffle_all(move || c.send(Command::ShuffleAll));
    let c = commander.clone();
    window.on_play_list(move |list, shuffle| {
        let source = TrackSource::List(track_list(list));
        c.send(if shuffle {
            Command::ShufflePlay(source)
        } else {
            Command::Play(source)
        });
    });
    let c = commander.clone();
    let weak = window.as_weak();
    window.on_search(move |text| {
        if let Some(w) = weak.upgrade() {
            w.set_search_loading(!text.trim().is_empty());
        }
        c.send(Command::Search(text.to_string()));
    });
    window.on_toggle_now_favorite(|| {
        with_ui(|ui, _| {
            if let Some(now) = ui.now.borrow().as_ref() {
                ui.send(Command::SetFavorite {
                    kind: FavoriteKind::Track,
                    id: now.track_id,
                    favorite: !now.favorite,
                });
            }
        });
    });
    window.on_open_now_album(|| {
        with_ui(|ui, w| {
            let album = ui.now.borrow().as_ref().map(|n| n.album_id);
            if let Some(id) = album.filter(|id| *id > 0) {
                ui.navigate(w, Route::Album(id));
            }
        });
    });
    window.on_open_now_artist(|| {
        with_ui(|ui, w| {
            let artist = ui.now.borrow().as_ref().map(|n| n.artist_id);
            if let Some(id) = artist.filter(|id| *id > 0) {
                ui.navigate(w, Route::Artist(id));
            }
        });
    });

    // 032 pages.
    {
        let home = window.global::<Home>();
        home.on_tab_selected(|tab| with_ui(|ui, w| ui.select_tab(w, tab)));
        home.on_action(|action, arg| with_ui(|ui, w| ui.home_action(w, &action, arg.into())));
        let lib = window.global::<Lib032>();
        lib.on_query_works(|search, kind, sort| {
            with_ui(|ui, w| {
                w.global::<Lib032>().set_works_loading(true);
                ui.send(Command::QueryWorks(m032::works_query(&search, kind, sort)));
            });
        });
        lib.on_query_series(|search| {
            with_ui(|ui, w| {
                w.global::<Lib032>().set_series_loading(true);
                ui.send(Command::QuerySeries(search.to_string()));
            });
        });
        lib.on_detail_action(|action, arg| {
            with_ui(|ui, w| ui.lib032_action(w, &action, arg.into()));
        });
        lib.on_open_web(|path| with_ui(|ui, _| ui.open_web(&path)));
        let search = window.global::<LyricsSearch>();
        search.on_search(|name, artist| {
            with_ui(|ui, w| {
                w.global::<LyricsSearch>().set_searching(true);
                ui.send(Command::SearchLyrics {
                    name: name.to_string(),
                    artist: artist.to_string(),
                });
            });
        });
        let c = commander.clone();
        search.on_select(move |i| {
            if let Ok(i) = usize::try_from(i) {
                c.send(Command::PreviewLyrics(i));
            }
        });
        let c = commander.clone();
        search.on_apply(move |i| {
            if let Ok(i) = usize::try_from(i) {
                c.send(Command::ApplyLyrics(i));
            }
        });
    }
    window.on_lyrics_tool(|tool| {
        with_ui(|ui, w| match tool.as_str() {
            "search" => ui.open_lyrics_search(w),
            "clear" => ui.send(Command::ClearLyrics),
            "translate" => ui.send(Command::ToggleTranslation),
            _ => {}
        });
    });
    let c = commander.clone();
    window.on_lyrics_offset_changed(move |ms| c.send(Command::SetLyricsOffset(ms.into())));
    let c = commander.clone();
    window.on_lyrics_seek(move |i| {
        if let Ok(i) = usize::try_from(i) {
            c.send(Command::SeekToLyric(i));
        }
    });
    let c = commander.clone();
    let weak = window.as_weak();
    window.on_lyrics_settings_changed(move || {
        if let Some(w) = weak.upgrade() {
            c.send(Command::SetLyricsSettings(Ui::lyrics_settings(&w)));
        }
    });

    // Settings.
    let c = commander.clone();
    let weak = window.as_weak();
    window.on_panel_changed(move || {
        if let Some(w) = weak.upgrade() {
            c.send(Command::SetRightPanel(RightPanel {
                open: w.get_panel_open(),
                queue_tab: w.get_panel_queue_tab(),
                queue: w.get_panel_queue(),
                lyrics: w.get_panel_lyrics(),
                lyrics_first: w.get_panel_lyrics_first(),
                split: w.get_panel_split(),
                width: w.get_panel_width(),
            }));
        }
    });
    let c = commander.clone();
    let weak = window.as_weak();
    window.on_sidebar_resized(move || {
        if let Some(w) = weak.upgrade() {
            c.send(Command::SetSidebarWidth(w.get_sidebar_width()));
        }
    });
    // The detail header cover follows the header's cover size.
    window.on_detail_art_changed(|| with_ui(|ui, w| ui.reload_detail_cover(w)));
    // The right panel cover follows the panel width.
    window.on_np_art_changed(|| {
        with_ui(|ui, w| {
            let artwork = ui.now.borrow().as_ref().map(|n| n.artwork.clone());
            ui.load_now_large(w, &artwork.unwrap_or_default());
        });
    });
    window.on_queue_search_changed(|text| {
        with_ui(|ui, w| {
            *ui.queue_search.borrow_mut() = text.trim().to_owned();
            ui.show_queue(w);
        });
    });
    window.on_save_queue(|| {
        with_ui(|ui, w| ui.open_dialog(w, Dialog::New(Some(TrackSource::Queue)), "播放队列", ""));
    });
    let c = commander.clone();
    window.on_close_to_tray_toggled(move |on| c.send(Command::SetCloseToTray(on)));
    let c = commander.clone();
    window.on_fade_on_pause_toggled(move |on| c.send(Command::SetFadeOnPause(on)));
    let c = commander.clone();
    window.on_theme_selected(move |i| {
        c.send(Command::SetTheme(match i {
            0 => ThemeMode::System,
            2 => ThemeMode::Light,
            _ => ThemeMode::Dark,
        }));
    });
    let c = commander.clone();
    window.on_settings_opened(move || c.send(Command::RefreshAudioDevices));
    let c = commander.clone();
    let weak = window.as_weak();
    window.on_audio_device_selected(move |index| {
        let name = UI.with(|cell| {
            let ui = cell.borrow();
            let names = ui.as_ref()?.device_names.borrow();
            usize::try_from(index)
                .ok()
                .and_then(|i| names.get(i).cloned())
        });
        match name {
            Some(name) => c.send(Command::SetAudioDevice(name)),
            None => {
                if let Some(w) = weak.upgrade() {
                    w.set_output_busy(false);
                }
            }
        }
    });
    let c = commander.clone();
    window.on_exclusive_toggled(move |on| c.send(Command::SetExclusive(on)));
    window.on_transcode_selected(|index| {
        with_ui(|ui, _| {
            let choice = usize::try_from(index)
                .ok()
                .and_then(|i| ui.transcode_choices.borrow().get(i).copied());
            if let Some(transcode) = choice {
                ui.transcode.set(transcode);
                ui.send(Command::SetTranscode(transcode));
            }
        });
    });

    // Queue and transport.
    let c = commander.clone();
    window.on_play_queue_item(move |uid| {
        if let Ok(uid) = u64::try_from(uid) {
            c.send(Command::PlayQueueItem(uid));
        }
    });
    let c = commander.clone();
    window.on_remove_queue_item(move |uid| {
        if let Ok(uid) = u64::try_from(uid) {
            c.send(Command::RemoveFromQueue(uid));
        }
    });
    let c = commander.clone();
    window.on_move_queue_item(move |uid, to| {
        if let (Ok(uid), Ok(to)) = (u64::try_from(uid), usize::try_from(to)) {
            c.send(Command::MoveQueueItem { uid, to });
        }
    });
    let c = commander.clone();
    window.on_set_sleep(move |mode, minutes| {
        let timer = match mode {
            1 => Some(SleepTimer::EndOfTrack),
            2 => Some(SleepTimer::EndOfAlbum),
            3 => u32::try_from(minutes).ok().map(SleepTimer::Minutes),
            _ => None,
        };
        c.send(Command::SetSleepTimer(timer));
    });
    let c = commander.clone();
    window.on_clear_queue(move || c.send(Command::ClearQueue));
    let c = commander.clone();
    let weak = window.as_weak();
    window.on_toggle_shuffle(move || {
        if let Some(w) = weak.upgrade() {
            c.send(Command::SetShuffle(!w.get_shuffle()));
        }
    });
    let c = commander.clone();
    let weak = window.as_weak();
    window.on_cycle_repeat(move || {
        if let Some(w) = weak.upgrade() {
            c.send(Command::SetRepeat(
                repeat_from_index(w.get_repeat()).cycle(),
            ));
        }
    });
    let c = commander.clone();
    window.on_toggle_pause(move || c.send(Command::TogglePause));
    let c = commander.clone();
    window.on_next(move || c.send(Command::Next));
    let c = commander.clone();
    window.on_previous(move || c.send(Command::Previous));
    let c = commander.clone();
    window.on_seek(move |ms| {
        c.send(Command::Seek {
            position_ms: ms.into(),
        })
    });
    let c = commander;
    window.on_volume_changed(move |v| c.send(Command::SetVolume(f64::from(v))));
}

fn repeat_index(repeat: Repeat) -> i32 {
    match repeat {
        Repeat::Off => 0,
        Repeat::All => 1,
        Repeat::One => 2,
    }
}

fn repeat_from_index(index: i32) -> Repeat {
    match index {
        1 => Repeat::All,
        2 => Repeat::One,
        _ => Repeat::Off,
    }
}

fn row_index(index: Option<usize>) -> i32 {
    index.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1)
}

pub(super) fn to_image(rgba: &Rgba) -> Image {
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba.pixels, rgba.width, rgba.height);
    Image::from_rgba8(buffer)
}

fn login_message(failure: &LoginFailure) -> String {
    match failure {
        LoginFailure::CredentialsRequired => "API Token 和媒体凭据都必须填写".into(),
        LoginFailure::TokensMustDiffer => "API Token 和媒体凭据不能相同".into(),
        LoginFailure::ApiTokenInvalid => "API Token 无效".into(),
        LoginFailure::UnsupportedRevision(r) => {
            format!("服务器 API 版本（revision {r}）与客户端不兼容，需要 revision 3")
        }
        LoginFailure::MediaTokenInvalid => "媒体凭据无效".into(),
        LoginFailure::InvalidUrl => "服务器地址无效".into(),
        LoginFailure::Other(e) => format!("连接失败：{e}"),
    }
}

fn int(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// Alias line under the artist name. Skips blanks and a repeat of the name.
fn artist_alias(artist: &client_core::ArtistView) -> String {
    let name = artist.name.trim();
    artist
        .aliases
        .iter()
        .map(|alias| alias.trim())
        .filter(|alias| !alias.is_empty() && *alias != name)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Overview column. Only fields the artist API actually returns; there is
/// no birth date, so that row is never invented.
fn artist_fact_rows(artist: &client_core::ArtistView) -> Vec<FactRow> {
    let mut rows = Vec::new();
    for (label, value) in [
        ("类型", artist.artist_type.trim()),
        ("国家/地区", artist.country.trim()),
    ] {
        if !value.is_empty() {
            rows.push(FactRow {
                label: label.into(),
                value: value.into(),
            });
        }
    }
    if artist.album_count > 0 {
        rows.push(FactRow {
            label: "专辑".into(),
            value: format!("{} 张", artist.album_count).into(),
        });
    }
    if artist.track_count > 0 {
        rows.push(FactRow {
            label: "曲目".into(),
            value: format!("{} 首", artist.track_count).into(),
        });
    }
    rows
}

/// A list row; `multi_disc` numbers album tracks "disc-track".
fn track_row(t: TrackItem, index: usize, multi_disc: bool) -> TrackRow {
    let number = match (t.track_number, multi_disc) {
        (n, _) if n <= 0 => String::new(),
        (n, true) => format!("{}-{n:02}", t.disc_number.max(1)),
        (n, false) => n.to_string(),
    };
    TrackRow {
        id: int(t.id),
        index: i32::try_from(index).unwrap_or(i32::MAX),
        title: t.title.into(),
        artist: t.artist.into(),
        album: t.album.into(),
        duration: t.duration.into(),
        credits: ModelRc::new(VecModel::from(
            t.credits
                .into_iter()
                .map(|p| CreditPart {
                    text: p.text.into(),
                    id: int(p.artist_id),
                })
                .collect::<Vec<_>>(),
        )),
        album_id: int(t.album_id),
        artist_id: int(t.artist_id),
        favorite: t.favorite,
        number: number.into(),
        tag: t.tag.into(),
        ..TrackRow::default()
    }
}

fn track_rows(items: Vec<TrackItem>, offset: usize) -> Vec<TrackRow> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, t)| track_row(t, offset + i, false))
        .collect()
}

fn cards<T: Into<Card>>(items: Vec<T>) -> Vec<Card> {
    items.into_iter().map(Into::into).collect()
}

/// Applies one core event on the UI thread.
#[allow(clippy::too_many_lines)]
pub fn apply(event: Event) {
    with_ui(|ui, w| match event {
        Event::SavedLogin(saved) => {
            if !saved.server_url.is_empty() {
                w.set_login_server(saved.server_url.into());
            }
            w.set_login_api_token_saved(saved.has_api_token);
            w.set_login_media_token_saved(saved.has_media_token);
            if saved.signing_in {
                w.set_login_busy(true);
                w.set_login_error(SharedString::new());
            }
        }
        Event::SignedOut(reason) => {
            w.set_logged_in(false);
            w.set_login_busy(false);
            w.set_login_api_token(SharedString::new());
            w.set_login_media_token(SharedString::new());
            w.set_login_error(match reason {
                SignOutReason::User => SharedString::new(),
                SignOutReason::Expired => "API Token 已失效，请重新输入".into(),
            });
            w.set_dialog(0);
            w.set_full_open(false);
            ui.full_changed(w);
            ui.related_for.set(0);
            ui.related.set_vec(Vec::new());
            *ui.now_cover_key.borrow_mut() = None;
            *ui.now_large_key.borrow_mut() = None;
            *ui.now.borrow_mut() = None;
            for grid in &ui.grids {
                grid.clear();
            }
            ui.detail_tracks.set_vec(Vec::new());
            ui.search_tracks.set_vec(Vec::new());
            ui.unbind(Bind::Home);
            ui.unbind(Bind::Detail);
            for model in [
                &ui.home_mixes,
                &ui.home_recent,
                &ui.home_resume,
                &ui.home_songs,
            ] {
                model.set_vec(Vec::new());
            }
        }
        Event::LoginFailed(failure) => {
            w.set_login_busy(false);
            w.set_login_error(login_message(&failure).into());
        }
        Event::LoggedIn {
            server_url,
            features,
        } => {
            w.set_login_busy(false);
            w.set_logged_in(true);
            w.set_server_label(server_url.as_str().into());
            w.set_server_status(server_host(server_url.as_str()).into());
            ui.logged_in(w, &features);
            ui.m032.borrow_mut().server = server_url;
        }
        Event::AlbumsReset => {
            ui.grids[GRID_ALBUMS].clear();
            w.set_album_loaded(0);
            w.set_album_total(0);
            w.set_albums_loading(true);
        }
        Event::AlbumsAppended { items, total } => {
            ui.grids[GRID_ALBUMS].append(cards(items));
            w.set_album_loaded(i32::try_from(ui.grids[GRID_ALBUMS].len()).unwrap_or(i32::MAX));
            w.set_album_total(i32::try_from(total).unwrap_or(i32::MAX));
            w.set_albums_loading(false);
        }
        Event::TracksReset => {
            ui.tracks.set_vec(Vec::new());
            w.set_track_total(0);
            w.set_tracks_loading(true);
        }
        Event::TracksAppended { items, total } => {
            let offset = ui.tracks.row_count();
            ui.tracks.extend(track_rows(items, offset));
            w.set_track_total(i32::try_from(total).unwrap_or(i32::MAX));
            w.set_tracks_loading(false);
        }
        Event::ArtistsReset => {
            ui.grids[GRID_ARTISTS].clear();
            w.set_artist_total(0);
            w.set_artists_loading(true);
        }
        Event::ArtistsAppended { items, total } => {
            ui.grids[GRID_ARTISTS].append(cards(items));
            w.set_artist_total(i32::try_from(total).unwrap_or(i32::MAX));
            w.set_artists_loading(false);
        }
        Event::FavoriteAlbums(items) => {
            w.set_fav_album_count(i32::try_from(items.len()).unwrap_or(i32::MAX));
            ui.grids[GRID_FAV_ALBUMS].set(cards(items));
            w.set_fav_loading(false);
        }
        Event::FavoriteTracks(items) => {
            ui.fav_tracks.set_vec(track_rows(items, 0));
            w.set_fav_loading(false);
        }
        Event::FavoriteArtists(items) => {
            w.set_fav_artist_count(i32::try_from(items.len()).unwrap_or(i32::MAX));
            ui.grids[GRID_FAV_ARTISTS].set(cards(items));
            w.set_fav_loading(false);
        }
        Event::Playlists(items) => {
            w.set_playlist_count(i32::try_from(items.len()).unwrap_or(i32::MAX));
            ui.grids[GRID_PLAYLISTS].set(cards(items));
        }
        Event::PlaylistChoices(items) => {
            ui.side_playlists(w, &items);
            ui.playlist_choices.set_vec(
                items
                    .into_iter()
                    .map(|p| PlaylistChoice {
                        id: int(p.id),
                        name: p.name.into(),
                    })
                    .collect::<Vec<_>>(),
            );
        }
        Event::PlaylistDeleted(id) => {
            if ui.route.get() == Route::Playlist(id) {
                ui.navigate(w, Route::Page(PAGE_PLAYLISTS));
            }
        }
        Event::Detail { view, tracks } => ui.detail_loaded(w, view, tracks),
        Event::AlbumWorks {
            album_id,
            works,
            tieups,
        } => ui.album_works_loaded(w, album_id, &works, &tieups),
        Event::SearchResults {
            query: _,
            artists,
            albums,
            tracks,
        } => {
            w.set_search_artist_count(i32::try_from(artists.len()).unwrap_or(i32::MAX));
            w.set_search_album_count(i32::try_from(albums.len()).unwrap_or(i32::MAX));
            ui.grids[GRID_SEARCH_ARTISTS].set(cards(artists));
            ui.grids[GRID_SEARCH_ALBUMS].set(cards(albums));
            ui.search_tracks.set_vec(track_rows(tracks, 0));
            w.set_search_loading(false);
        }
        Event::Favorite { kind, id, favorite } => ui.favorite_changed(w, kind, id, favorite),
        Event::Image { key, image } => {
            let image = to_image(&image);
            ui.image_arrived(&key, &image);
            for grid in &ui.grids {
                grid.set_cover(&key, &image);
            }
            if ui.now_cover_key.borrow().as_deref() == Some(key.as_str()) {
                w.set_np_cover(image.clone());
            }
            if ui.now_large_key.borrow().as_deref() == Some(key.as_str()) {
                w.set_np_cover_large(image.clone());
            }
            if ui.full_cover_key.borrow().as_deref() == Some(key.as_str()) {
                w.set_full_cover(image.clone());
            }
            if ui.detail_cover_key.borrow().as_deref() == Some(key.as_str()) {
                w.set_detail_cover(image);
            }
        }
        Event::NowPlaying(None) => {
            ui.mark_playing_album(0);
            w.set_has_track(false);
            w.set_playing_id(-1);
            w.set_np_favorite(false);
            *ui.now.borrow_mut() = None;
            ui.full_changed(w);
        }
        Event::NowPlaying(Some(now)) => {
            ui.mark_playing_album(now.album_id);
            w.set_has_track(true);
            w.set_playing_id(int(now.track_id));
            w.set_np_title(now.title.as_str().into());
            w.set_np_artist(now.artist.as_str().into());
            w.set_np_album(now.album.as_str().into());
            w.set_np_quality(now.quality.clone().unwrap_or_default().into());
            w.set_np_favorite(now.favorite);
            // Decoded at the player bar size: a large grid cover shrunk
            // by the renderer (no mipmaps) looks jagged.
            let px = physical_box(NP_COVER, NP_COVER, ui.scale(w));
            let key = (!now.artwork.is_empty()).then(|| image_key(&now.artwork, px));
            match &key {
                Some(key) => {
                    let placeholder = ui.grids.iter().find_map(|g| g.cover(&now.artwork));
                    w.set_np_cover(placeholder.unwrap_or_default());
                    ui.send(Command::LoadImage {
                        key: key.clone(),
                        path: now.artwork.clone(),
                        width: px.0,
                        height: px.1,
                    });
                }
                None => w.set_np_cover(Image::default()),
            }
            *ui.now_cover_key.borrow_mut() = key;
            ui.load_now_large(w, &now.artwork);
            *ui.now.borrow_mut() = Some(now);
            ui.full_changed(w);
        }
        Event::UpdateAvailable { version, url } => {
            ui.notice(format!("发现新版本 {version}，可在设置页下载"));
            w.set_update_version(version.into());
            *ui.update_url.borrow_mut() = url;
        }
        Event::TrackInfo { title: _, facts } => {
            w.set_info_rows(ModelRc::new(VecModel::from(
                facts
                    .into_iter()
                    .map(|(label, value)| crate::FactRow {
                        label: label.into(),
                        value: value.into(),
                    })
                    .collect::<Vec<_>>(),
            )));
            w.set_dialog(6);
        }
        Event::ArtistFavorites { artist_id, tracks } => {
            if ui.route.get() == Route::Artist(artist_id) {
                ui.artist_favs.set_vec(track_rows(tracks, 0));
            }
        }
        Event::Related { track_id, tracks } => {
            if ui.related_for.get() == track_id {
                ui.related.set_vec(track_rows(tracks, 0));
                w.set_full_related_loading(false);
            }
        }
        Event::Position {
            position_ms,
            duration_ms,
        } => {
            w.set_position_ms(int(position_ms));
            w.set_duration_ms(int(duration_ms));
            w.set_position_text(format::duration(position_ms).into());
            w.set_duration_text(format::duration(duration_ms).into());
        }
        Event::Paused(paused) => {
            w.set_paused(paused);
            w.global::<crate::Playback>().set_paused(paused);
        }
        Event::Buffering(buffering) => w.set_buffering(buffering),
        #[allow(clippy::cast_possible_truncation)]
        Event::Volume(volume) => w.set_volume(volume as f32),
        Event::Lyrics {
            lines,
            synced,
            source,
        } => {
            ui.lyrics_lines(w, lines);
            w.set_lyrics_synced(synced);
            w.set_lyrics_active(-1);
            w.set_lyrics_source(source.unwrap_or_default().into());
        }
        Event::Queue { items, current } => {
            *ui.queue_all.borrow_mut() = (items, current);
            ui.show_queue(w);
        }
        Event::QueuePosition(current) => {
            ui.queue_all.borrow_mut().1 = current;
            ui.show_queue(w);
        }
        Event::Repeat(repeat) => w.set_repeat(repeat_index(repeat)),
        Event::Shuffle(shuffle) => w.set_shuffle(shuffle),
        Event::SleepTimer(status) => {
            w.set_sleep_active(status.is_some());
            let label = match status {
                None => String::new(),
                Some(s) => match (s.timer, s.remaining_ms) {
                    (SleepTimer::EndOfTrack, _) => "本曲".into(),
                    (SleepTimer::EndOfAlbum, _) => "本专辑".into(),
                    (SleepTimer::Minutes(_), remaining) => {
                        format::duration(remaining.unwrap_or(0).max(0))
                    }
                },
            };
            w.set_sleep_label(label.into());
        }
        Event::AudioDevices(devices) => ui.set_devices(w, devices),
        Event::AudioDevice(device) => {
            *ui.selected_device.borrow_mut() = device;
            w.set_audio_device_index(ui.device_index());
            w.set_output_busy(false);
        }
        Event::Exclusive(enabled) => {
            w.set_exclusive(enabled);
            w.set_output_busy(false);
        }
        Event::LyricsActive(active) => {
            w.set_lyrics_active(active.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1));
        }
        Event::Notice(text) => ui.notice(text),
        Event::LyricsTools(tools) => ui.lyrics_tools(w, &tools),
        Event::LyricsSearchResults(results) => ui.lyrics_results(w, results),
        Event::LyricsPreview(lines) => Ui::lyrics_preview(w, lines),
        Event::Home(view) => ui.home_loaded(w, *view),
        Event::HomeUnavailable => {
            let home = w.global::<Home>();
            home.set_loading(false);
            home.set_unavailable(true);
        }
        Event::HomeWorks(works) => ui.home_works(w, works),
        Event::HomeSongs(songs) => ui.home_songs(w, songs),
        Event::HomeChronicle(c) => ui.chronicle(w, c),
        Event::HomeSpotlightPane(pane) => ui.spotlight_pane(w, pane),
        Event::HomeRecent(items) => ui.home_recent(w, items),
        Event::DailyMixes { generating, items } => ui.home_mixes(w, generating, &items),
        Event::DailyMixReplaced { from, to } => {
            if ui.route.get() == Route::DailyMix(from) {
                // Replaces the page instead of stacking the old mix.
                ui.show(w, Route::DailyMix(to));
            }
        }
        Event::MixRowBlocked { index, blocked } => ui.mix_row_blocked(index, blocked),
        Event::WorksReset => {
            ui.grids[GRID_WORKS].clear();
            let lib = w.global::<Lib032>();
            lib.set_works_total(0);
            lib.set_works_loading(true);
        }
        Event::WorksAppended { items, total } => ui.works_appended(w, &items, total),
        Event::SeriesReset => {
            ui.grids[GRID_SERIES].clear();
            let lib = w.global::<Lib032>();
            lib.set_series_total(0);
            lib.set_series_loading(true);
        }
        Event::SeriesAppended { items, total } => ui.series_appended(w, items, total),
        Event::HistoryReset => {
            ui.played.set_vec(Vec::new());
            let lib = w.global::<Lib032>();
            lib.set_history_total(0);
            lib.set_history_loading(true);
        }
        Event::HistoryAppended { items, total } => ui.history_appended(w, items, total),
        Event::PlayingSources(sources) => ui.playing_sources(w, sources),
        Event::CastRooms {
            searching,
            rooms,
            error,
        } => {
            let mut cast = ui.cast.borrow_mut();
            cast.rooms(searching, rooms, error);
            cast.show(w);
        }
        Event::CastOutput(output) => {
            let mut cast = ui.cast.borrow_mut();
            cast.output(output);
            cast.show(w);
        }
    });
}

/// Host of the server URL for the sidebar status ("music.032").
pub(crate) fn server_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // Drop user info.
    host.rsplit_once('@').map_or(host, |(_, h)| h).to_owned()
}

/// "18930" -> "18,930".
pub(crate) fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[cfg(test)]
mod shell_tests {
    use super::{server_host, thousands};

    #[test]
    fn server_status_parts() {
        assert_eq!(
            server_host("https://music.032.example/"),
            "music.032.example"
        );
        assert_eq!(server_host("http://u:p@10.0.0.2:8080/x"), "10.0.0.2:8080");
        assert_eq!(server_host("music.local"), "music.local");
        assert_eq!(thousands(18930), "18,930");
        assert_eq!(thousands(930), "930");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
