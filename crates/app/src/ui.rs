//! UI-thread state: models, shared types, helpers, and submodule exports.

mod actions;
mod cast;
mod events;
mod m032;
mod search;
mod shortcuts;
mod wire;

pub use events::apply;
pub use wire::install;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use client_core::m032::SpotlightTab;
use client_core::{
    AlbumSort, ArtistSort, AudioDevice, Command, Commander, DetailView, LoginFailure, NowPlaying,
    PlaylistSort, Repeat, Rgba, ServerFeatures, SortOrder, TrackItem, TrackList, TrackSort,
    TrackSource, Transcode,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer,
    VecModel,
};

use crate::cards::Card;
use crate::cards::CardGridModel;
use crate::{
    AppWindow, CreditPart, FactRow, GridId, HomeRow, ListQuery, LyricLine, LyricsResult,
    PlaylistChoice, QueueRow, TrackListId, TrackRow,
};
use m032::{
    GRID_CHRONICLE, GRID_HOME_ALBUMS, GRID_HOME_PLAYLISTS, GRID_HOME_WORKS, GRID_MEMBERS,
    GRID_SERIES, GRID_SERIES_ALBUMS, GRID_WORK_ALBUMS, GRID_WORKS, M032State,
};
use search::SearchUiState;

/// Card grids, by index into `Ui::grids`.
const GRID_ALBUMS: usize = 0;
const GRID_ARTISTS: usize = 1;
const GRID_FAV_ALBUMS: usize = 2;
const GRID_FAV_ARTISTS: usize = 3;
const GRID_SEARCH_ARTISTS: usize = 4;
const GRID_SEARCH_ALBUMS: usize = 5;
const GRID_SEARCH_COMPOSERS: usize = 23;
const GRID_RELEASES: usize = 6;
const GRID_PLAYLISTS: usize = 7;
/// Release groups of the artist page (专辑, 单曲与 EP, 合辑与现场, 其他发行,
/// 合作发行, 参与作品, 幕后参与), one grid each.
const RELEASE_GRIDS: [usize; 7] = [GRID_RELEASES, 17, 18, 19, 20, 21, 22];
const RELEASE_GRID_IDS: [GridId; 7] = [
    GridId::Releases0,
    GridId::Releases1,
    GridId::Releases2,
    GridId::Releases3,
    GridId::Releases4,
    GridId::Releases5,
    GridId::Releases6,
];
const GRIDS: usize = 24;
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
/// Grids of artist cards (for scoped favorite updates without cross-entity ID collision).
const ARTIST_GRIDS: [usize; 4] = [
    GRID_ARTISTS,
    GRID_FAV_ARTISTS,
    GRID_SEARCH_ARTISTS,
    GRID_SEARCH_COMPOSERS,
];

/// Pages of app.slint.
const PAGE_ALBUMS: i32 = 0;
const PAGE_TRACKS: i32 = 1;
const PAGE_PLAYLISTS: i32 = 4;
const PAGE_DETAIL: i32 = 10;

/// Queue thumbnails (logical px; matches the 48px full-screen stage cover).
const QUEUE_COVER: f32 = 48.0;
/// Decoded queue thumbnails kept in memory.
const MAX_QUEUE_COVERS: usize = 128;
/// Player bar cover (logical px).
const NP_COVER: f32 = 56.0;
/// Playlist artwork upload limit of the server.
const MAX_ARTWORK_BYTES: u64 = 10 * 1024 * 1024;
const MAX_HISTORY: usize = 50;

/// Maps a Slint `GridId` enum to its slot index in `Ui::grids`.
const fn grid_index(id: GridId) -> usize {
    match id {
        GridId::Albums => GRID_ALBUMS,
        GridId::Artists => GRID_ARTISTS,
        GridId::FavAlbums => GRID_FAV_ALBUMS,
        GridId::FavArtists => GRID_FAV_ARTISTS,
        GridId::SearchArtists => GRID_SEARCH_ARTISTS,
        GridId::SearchAlbums => GRID_SEARCH_ALBUMS,
        GridId::Releases0 => GRID_RELEASES,
        GridId::Playlists => GRID_PLAYLISTS,
        GridId::Works => GRID_WORKS,
        GridId::Series => GRID_SERIES,
        GridId::HomeWorks => GRID_HOME_WORKS,
        GridId::HomeAlbums => GRID_HOME_ALBUMS,
        GridId::Chronicle => GRID_CHRONICLE,
        GridId::HomePlaylists => GRID_HOME_PLAYLISTS,
        GridId::WorkAlbums => GRID_WORK_ALBUMS,
        GridId::Members => GRID_MEMBERS,
        GridId::SeriesAlbums => GRID_SERIES_ALBUMS,
        GridId::Releases1 => 17,
        GridId::Releases2 => 18,
        GridId::Releases3 => 19,
        GridId::Releases4 => 20,
        GridId::Releases5 => 21,
        GridId::Releases6 => 22,
        GridId::SearchComposers => GRID_SEARCH_COMPOSERS,
    }
}

/// Maps a Slint `TrackListId` enum to the core `TrackList`.
const fn track_list(id: TrackListId) -> TrackList {
    match id {
        TrackListId::Library => TrackList::Library,
        TrackListId::Favorites => TrackList::Favorites,
        TrackListId::Detail => TrackList::Detail,
        TrackListId::Search => TrackList::Search,
        TrackListId::SpotlightRecent => TrackList::Spotlight(SpotlightTab::Recent),
        TrackListId::SpotlightListening => TrackList::Spotlight(SpotlightTab::Listening),
        TrackListId::SpotlightRandom => TrackList::Spotlight(SpotlightTab::Random),
        TrackListId::HomeResume => TrackList::HomeResume,
        TrackListId::HomeRecent => TrackList::HomeRecent,
        TrackListId::HomeSongs => TrackList::HomeSongs,
        TrackListId::History => TrackList::History,
        TrackListId::Related => TrackList::Related,
        TrackListId::ArtistFavorites => TrackList::ArtistFavorites,
        TrackListId::SearchPreview => TrackList::SearchPreview,
        TrackListId::ArtistCredits => TrackList::ArtistCredits,
    }
}

/// Bounded LRU-like cache for queue cover thumbnails.
struct BoundedCache<V> {
    map: HashMap<String, V>,
    order: VecDeque<String>,
    cap: usize,
}

impl<V> Default for BoundedCache<V> {
    fn default() -> Self {
        Self::new(MAX_QUEUE_COVERS)
    }
}

impl<V> BoundedCache<V> {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    fn insert(&mut self, key: String, value: V) {
        if let Some(pos) = self.order.iter().position(|k| k == &key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
        while self.map.len() > self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

type QueueCoverCache = BoundedCache<Image>;

/// Where the content area is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Page(i32),
    Album(i64),
    Artist(i64),
    Playlist(i64),
    Similar(i64),
    Versions(i64),
    Work(i64),
    Series(i64),
    DailyMix(i64),
}

/// A back-navigation destination and its independent scrolling areas.
#[derive(Clone, Copy, Debug)]
struct HistoryEntry {
    route: Route,
    scroll_y: f32,
    inspector_scroll_y: f32,
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
    search_preview_tracks: Rc<VecModel<TrackRow>>,
    search: RefCell<SearchUiState>,
    /// The playback history page.
    played: Rc<VecModel<TrackRow>>,
    /// Preview tracks of the shown spotlight tab.
    pane_tracks: Rc<VecModel<TrackRow>>,
    /// "收藏的歌曲" of the open artist page.
    artist_favs: Rc<VecModel<TrackRow>>,
    artist_credits: Rc<VecModel<TrackRow>>,
    artist_dir_role: RefCell<Option<client_core::ArtistRole>>,
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
    history: RefCell<Vec<HistoryEntry>>,
    /// Restores asynchronously populated pages after their content grows.
    scroll_timer: Timer,
    scroll_restore: Cell<(f32, f32)>,
    scroll_restore_ticks: Cell<u8>,
    scroll_seen: Cell<(Option<f32>, Option<f32>)>,
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
    restore_hint_timer: Timer,
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
    queue_covers: Rc<RefCell<QueueCoverCache>>,
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
            TrackList::SearchPreview => &self.search_preview_tracks,
            TrackList::Spotlight(_) => &self.pane_tracks,
            TrackList::History => &self.played,
            TrackList::ArtistFavorites => &self.artist_favs,
            TrackList::ArtistCredits => &self.artist_credits,
            TrackList::HomeResume
            | TrackList::HomeRecent
            | TrackList::HomeSongs
            | TrackList::Related => return None,
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

    fn begin_restore(&self, w: &AppWindow) {
        w.set_login_restoring(true);
        w.set_login_restoring_hint(false);
        let weak = self.window.clone();
        self.restore_hint_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(400),
            move || {
                if let Some(w) = weak.upgrade()
                    && w.get_login_restoring()
                    && !w.get_logged_in()
                {
                    w.set_login_restoring_hint(true);
                }
            },
        );
    }

    fn finish_restore(&self, w: &AppWindow) {
        self.restore_hint_timer.stop();
        w.set_login_restoring(false);
        w.set_login_restoring_hint(false);
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
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// Records the current window geometry while it is still visible.
pub fn record_window_now() {
    with_ui(|ui, w| ui.record_window(w));
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

/// Hides `LoginView` while startup restore / auto-login checks saved credentials.
pub fn begin_restore() {
    with_ui(|ui, w| ui.begin_restore(w));
}

fn with_ui(f: impl FnOnce(&Ui, &AppWindow)) {
    let ui = UI.with(|cell| cell.borrow().clone());
    if let Some(ui) = ui
        && let Some(window) = ui.window.upgrade()
    {
        f(&ui, &window);
    }
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

pub(crate) fn artist_refs(artists: &[client_core::ArtistRef]) -> ModelRc<CreditPart> {
    ModelRc::new(VecModel::from(
        artists
            .iter()
            .filter(|a| a.id > 0)
            .map(|a| CreditPart {
                id: int(a.id),
                text: a.name.as_str().into(),
                join_phrase: a.separator().into(),
            })
            .collect::<Vec<_>>(),
    ))
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
                    join_phrase: Default::default(),
                    id: int(p.artist_id),
                })
                .collect::<Vec<_>>(),
        )),
        album_id: int(t.album_id),
        artist_id: int(t.artist_id),
        artists: artist_refs(&t.artists),
        favorite: t.favorite,
        version_count: t.version_count,
        number: number.into(),
        tag: t.tag.into(),
        disc: t.disc_number.max(1),
        year: if t.year > 0 {
            t.year.to_string().into()
        } else {
            SharedString::new()
        },
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

const fn exclusive_mode_index(mode: client_core::ExclusiveMode) -> i32 {
    match mode {
        client_core::ExclusiveMode::Off => 0,
        client_core::ExclusiveMode::Wasapi => 1,
        client_core::ExclusiveMode::Asio => 2,
    }
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

/// True when the viewport moved away from `target` (or past it) compared to
/// the last observed clamped position, meaning the user scrolled manually.
fn moved_away(cur: f32, prev: f32, target: f32) -> bool {
    if target < 0.0 {
        cur > prev + 1.0 || cur < target - 1.0
    } else if target > 0.0 {
        cur < prev - 1.0 || cur > target + 1.0
    } else {
        false
    }
}

#[cfg(test)]
mod shell_tests {
    use super::{
        BoundedCache, GRIDS, RELEASE_GRID_IDS, RELEASE_GRIDS, grid_index, moved_away, server_host,
        thousands, track_list,
    };
    use crate::{GridId, TrackListId};
    use client_core::TrackList;
    use client_core::m032::SpotlightTab;

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

    #[test]
    fn bounded_cache_evicts_oldest_and_promotes_updates() {
        let mut cache = BoundedCache::new(2);
        cache.insert("a".into(), 1);
        cache.insert("b".into(), 2);
        // Re-inserting "a" promotes it so "b" is evicted next.
        cache.insert("a".into(), 10);
        cache.insert("c".into(), 3);
        assert_eq!(cache.get("a"), Some(&10));
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("c"), Some(&3));
        cache.clear();
        assert_eq!(cache.get("a"), None);
    }

    #[test]
    fn moved_away_detects_manual_scroll_against_restore() {
        // Content growing moves clamped offset from 0 toward -1200 (not user scroll).
        assert!(!moved_away(-300.0, 0.0, -1200.0));
        assert!(!moved_away(-300.0, -300.0, -1200.0));
        // User scrolls back up from -300 to -100 while restoring toward -1200.
        assert!(moved_away(-100.0, -300.0, -1200.0));
        // User scrolls past target.
        assert!(moved_away(-1400.0, -1200.0, -1200.0));
    }

    #[test]
    fn grid_and_track_list_ids_map_cleanly() {
        for (id, slot) in RELEASE_GRID_IDS.into_iter().zip(RELEASE_GRIDS) {
            assert_eq!(grid_index(id), slot);
            assert!(slot < GRIDS);
        }
        assert_eq!(grid_index(GridId::Albums), 0);
        assert_eq!(grid_index(GridId::SearchComposers), GRIDS - 1);
        assert_eq!(
            track_list(TrackListId::SpotlightListening),
            TrackList::Spotlight(SpotlightTab::Listening)
        );
        assert_eq!(
            track_list(TrackListId::ArtistCredits),
            TrackList::ArtistCredits
        );
    }
}
