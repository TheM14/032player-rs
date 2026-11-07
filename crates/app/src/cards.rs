//! Card grid model (albums, artists, playlists): items chunked into rows of
//! `columns` cards for a virtualized ListView. Covers load lazily:
//! `row_data` is only called for rows the ListView instantiates, and that
//! is where artwork is requested.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use client_core::{AlbumItem, ArtistItem, Command, Commander, PlaylistItem};
use slint::{Image, Model, ModelNotify, ModelRc, ModelTracker, SharedString, VecModel};

use crate::{AlbumCard, AlbumRow};

/// Decoded covers kept in the model; older ones are dropped and reloaded
/// (from the core byte cache or the server) when scrolled back into view.
/// A screen shows at most ~30 cards.
const MAX_COVERS: usize = 96;

/// Physical pixel box an image is decoded for.
pub type ImageBox = (u32, u32);

/// Image request key: display box and artwork path.
pub fn image_key(path: &str, (width, height): ImageBox) -> String {
    format!("{width}x{height}|{path}")
}

/// Physical box of a `width` x `height` logical one at `scale`.
pub fn physical_box(width: f32, height: f32, scale: f32) -> ImageBox {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let px = |v: f32| ((v * scale).round().max(1.0) as u32).min(4096);
    (px(width), px(height))
}

/// Artwork path of an image key.
pub fn key_path(key: &str) -> Option<&str> {
    parse_key(key).map(|(_, path)| path)
}

fn parse_key(key: &str) -> Option<(ImageBox, &str)> {
    let (size, path) = key.split_once('|')?;
    let (width, height) = size.split_once('x')?;
    Some(((width.parse().ok()?, height.parse().ok()?), path))
}

/// What a card shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub id: i64,
    pub title: String,
    pub subtitle: String,
    pub year: i32,
    /// Server-relative artwork / image path.
    pub artwork: String,
    pub artist_id: i64,
    pub favorite: bool,
    pub badge: String,
    /// Third line (track uses on a work page, roles of a work).
    pub note: String,
    /// Muted last line ("入库于 3 天前").
    pub added: String,
    /// The current track belongs to it.
    pub playing: bool,
}

impl From<AlbumItem> for Card {
    fn from(a: AlbumItem) -> Self {
        // The artist page groups releases by relation; elsewhere the
        // badge stays empty.
        let badge = "";
        Self {
            id: a.id,
            title: a.title,
            subtitle: a.artist,
            year: a.year,
            artwork: a.artwork,
            artist_id: a.artist_id,
            favorite: a.favorite,
            badge: badge.into(),
            ..Self::default()
        }
    }
}

impl From<ArtistItem> for Card {
    fn from(a: ArtistItem) -> Self {
        let subtitle = match (a.album_count, a.track_count) {
            (0, 0) => String::new(),
            (0, t) => format!("{t} 首"),
            (n, _) => format!("{n} 张专辑"),
        };
        Self {
            id: a.id,
            title: a.name,
            subtitle,
            artwork: a.image,
            favorite: a.favorite,
            ..Self::default()
        }
    }
}

impl From<PlaylistItem> for Card {
    fn from(p: PlaylistItem) -> Self {
        let subtitle = match p.duration {
            Some(d) if p.track_count > 0 => format!("{} 首 · {d}", p.track_count),
            _ => format!("{} 首", p.track_count),
        };
        Self {
            id: p.id,
            title: p.name,
            subtitle,
            artwork: p.artwork,
            ..Self::default()
        }
    }
}

struct Entry {
    item: Card,
    /// Cover and the box it was decoded for. A cover of an older size
    /// stays on screen until the current size arrives.
    cover: Option<(Image, ImageBox)>,
    requested: Cell<bool>,
}

pub struct CardGridModel {
    entries: RefCell<Vec<Entry>>,
    /// Entry indices by artwork path.
    by_path: RefCell<HashMap<String, Vec<usize>>>,
    columns: Cell<usize>,
    /// Physical box covers are decoded for (cover box x scale factor).
    cover_px: Cell<ImageBox>,
    /// Box waiting for the resize to settle.
    pending_px: Cell<ImageBox>,
    /// Last cover box in logical px (0 before the first layout).
    logical: Cell<(f32, f32)>,
    /// Cards shown at most (the home grids show a number that depends on
    /// the width).
    limit: Cell<usize>,
    resize_timer: slint::Timer,
    /// Entry indices holding a cover, oldest first.
    loaded: RefCell<VecDeque<usize>>,
    notify: ModelNotify,
    commander: Commander,
    /// Ids marked as playing; also applied to cards added later.
    playing: RefCell<std::collections::HashSet<i64>>,
}

impl CardGridModel {
    pub fn new(commander: Commander) -> Rc<Self> {
        Rc::new(Self {
            entries: RefCell::default(),
            by_path: RefCell::default(),
            columns: Cell::new(4),
            cover_px: Cell::new((256, 256)),
            pending_px: Cell::new((0, 0)),
            logical: Cell::new((0.0, 0.0)),
            limit: Cell::new(usize::MAX),
            resize_timer: slint::Timer::default(),
            loaded: RefCell::default(),
            notify: ModelNotify::default(),
            commander,
            playing: RefCell::default(),
        })
    }

    fn rows_for(len: usize, columns: usize) -> usize {
        len.div_ceil(columns.max(1))
    }

    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.by_path.borrow_mut().clear();
        self.loaded.borrow_mut().clear();
        self.notify.reset();
    }

    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    pub fn append(&self, items: Vec<Card>) {
        let columns = self.columns.get();
        let (old_len, new_len) = {
            let mut entries = self.entries.borrow_mut();
            let mut by_path = self.by_path.borrow_mut();
            let old_len = entries.len();
            let playing = self.playing.borrow();
            for mut item in items {
                item.playing = item.playing || playing.contains(&item.id);
                if !item.artwork.is_empty() {
                    by_path
                        .entry(item.artwork.clone())
                        .or_default()
                        .push(entries.len());
                }
                entries.push(Entry {
                    item,
                    cover: None,
                    requested: Cell::new(false),
                });
            }
            (old_len, entries.len())
        };
        if self.limit.get() != usize::MAX {
            self.notify.reset();
            return;
        }
        let old_rows = Self::rows_for(old_len, columns);
        let new_rows = Self::rows_for(new_len, columns);
        if old_len % columns != 0 && old_rows > 0 {
            self.notify.row_changed(old_rows - 1);
        }
        if new_rows > old_rows {
            self.notify.row_added(old_rows, new_rows - old_rows);
        }
    }

    /// New column count and cover box. Covers are decoded at exactly the
    /// displayed size so they are drawn 1:1 (a stretched or shrunk texture
    /// looks soft); a new size applies once the resize has settled for a
    /// moment, and the old covers stay until replaced.
    ///
    /// `logical` is the cover box (width, height) in logical px.
    pub fn set_layout(self: &Rc<Self>, columns: usize, logical: (f32, f32), scale: f32) {
        self.logical.set(logical);
        let columns = columns.max(1);
        let px = physical_box(logical.0.max(16.0), logical.1.max(16.0), scale);
        if columns != self.columns.get() {
            self.columns.set(columns);
            self.notify.reset();
        }
        if px == self.cover_px.get() {
            self.resize_timer.stop();
            return;
        }
        // Nothing shown yet: no need to wait.
        if self.loaded.borrow().is_empty() {
            self.apply_px(px);
            return;
        }
        self.pending_px.set(px);
        let weak = Rc::downgrade(self);
        self.resize_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(200),
            move || {
                if let Some(model) = weak.upgrade() {
                    model.apply_px(model.pending_px.get());
                }
            },
        );
    }

    /// Shows at most `limit` cards (0: all).
    pub fn set_limit(&self, limit: usize) {
        let limit = if limit == 0 { usize::MAX } else { limit };
        if self.limit.replace(limit) != limit {
            self.notify.reset();
        }
    }

    fn shown(&self) -> usize {
        self.entries.borrow().len().min(self.limit.get())
    }

    /// The window moved to a screen with another scale factor.
    pub fn rescale(self: &Rc<Self>, scale: f32) {
        let logical = self.logical.get();
        if logical.0 > 0.0 {
            self.set_layout(self.columns.get(), logical, scale);
        }
    }

    fn apply_px(&self, px: ImageBox) {
        if px.0 == 0 || px == self.cover_px.get() {
            return;
        }
        self.cover_px.set(px);
        for entry in self.entries.borrow().iter() {
            entry.requested.set(false);
        }
        self.notify.reset();
    }

    /// Stores a loaded cover; true when some album uses it. Covers decoded
    /// for an outdated size are ignored.
    pub fn set_cover(&self, key: &str, image: &Image) -> bool {
        let Some((px, path)) = parse_key(key) else {
            return false;
        };
        if px != self.cover_px.get() {
            return false;
        }
        let Some(indices) = self.by_path.borrow().get(path).cloned() else {
            return false;
        };
        let columns = self.columns.get();
        let mut rows = Vec::new();
        {
            let mut entries = self.entries.borrow_mut();
            let mut loaded = self.loaded.borrow_mut();
            for i in indices {
                if let Some(entry) = entries.get_mut(i) {
                    if entry.cover.is_none() {
                        loaded.push_back(i);
                    }
                    entry.cover = Some((image.clone(), px));
                    rows.push(i / columns);
                }
            }
            while loaded.len() > MAX_COVERS {
                let Some(old) = loaded.pop_front() else { break };
                if let Some(entry) = entries.get_mut(old) {
                    entry.cover = None;
                    entry.requested.set(false);
                }
            }
        }
        rows.dedup();
        for row in rows {
            self.notify.row_changed(row);
        }
        true
    }

    /// Replaces every item (small lists that are loaded at once).
    pub fn set(&self, items: Vec<Card>) {
        self.clear();
        self.append(items);
    }

    /// Updates the favorite flag of the card `id`.
    pub fn set_favorite(&self, id: i64, favorite: bool) {
        let columns = self.columns.get();
        let rows: Vec<usize> = {
            let mut entries = self.entries.borrow_mut();
            entries
                .iter_mut()
                .enumerate()
                .filter(|(_, e)| e.item.id == id && e.item.favorite != favorite)
                .map(|(i, e)| {
                    e.item.favorite = favorite;
                    i / columns
                })
                .collect()
        };
        for row in rows {
            self.notify.row_changed(row);
        }
    }

    /// Marks the cards whose id is in `ids` as playing.
    pub fn set_playing(&self, ids: &std::collections::HashSet<i64>) {
        self.playing.borrow_mut().clone_from(ids);
        let columns = self.columns.get();
        let rows: Vec<usize> = {
            let mut entries = self.entries.borrow_mut();
            entries
                .iter_mut()
                .enumerate()
                .filter_map(|(i, e)| {
                    let playing = ids.contains(&e.item.id);
                    (e.item.playing != playing).then(|| {
                        e.item.playing = playing;
                        i / columns
                    })
                })
                .collect()
        };
        let mut rows = rows;
        rows.dedup();
        for row in rows {
            self.notify.row_changed(row);
        }
    }

    pub fn item(&self, id: i64) -> Option<Card> {
        self.entries
            .borrow()
            .iter()
            .find(|e| e.item.id == id)
            .map(|e| e.item.clone())
    }

    pub fn cover(&self, path: &str) -> Option<Image> {
        let by_path = self.by_path.borrow();
        let i = *by_path.get(path)?.first()?;
        self.entries
            .borrow()
            .get(i)?
            .cover
            .as_ref()
            .map(|(image, _)| image.clone())
    }
}

impl Model for CardGridModel {
    type Data = AlbumRow;

    fn row_count(&self) -> usize {
        Self::rows_for(self.shown(), self.columns.get())
    }

    fn row_data(&self, row: usize) -> Option<AlbumRow> {
        let shown = self.shown();
        let entries = self.entries.borrow();
        let columns = self.columns.get();
        let start = row * columns;
        if start >= shown {
            return None;
        }
        let end = (start + columns).min(shown);
        let px = self.cover_px.get();
        let cards: Vec<AlbumCard> = entries[start..end]
            .iter()
            .map(|entry| {
                let path = &entry.item.artwork;
                let current = entry.cover.as_ref().is_some_and(|(_, at)| *at == px);
                if !current && !path.is_empty() && !entry.requested.get() {
                    entry.requested.set(true);
                    self.commander.send(Command::LoadImage {
                        key: image_key(path, px),
                        path: path.clone(),
                        width: px.0,
                        height: px.1,
                    });
                }
                AlbumCard {
                    id: i32::try_from(entry.item.id).unwrap_or(i32::MAX),
                    title: SharedString::from(entry.item.title.as_str()),
                    artist: SharedString::from(entry.item.subtitle.as_str()),
                    year: if entry.item.year > 0 {
                        entry.item.year.to_string().into()
                    } else {
                        SharedString::new()
                    },
                    cover: entry
                        .cover
                        .as_ref()
                        .map(|(image, _)| image.clone())
                        .unwrap_or_default(),
                    artist_id: i32::try_from(entry.item.artist_id).unwrap_or(0),
                    favorite: entry.item.favorite,
                    badge: SharedString::from(entry.item.badge.as_str()),
                    note: SharedString::from(entry.item.note.as_str()),
                    added: SharedString::from(entry.item.added.as_str()),
                    playing: entry.item.playing,
                }
            })
            .collect();
        Some(AlbumRow {
            items: ModelRc::new(VecModel::from(cards)),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
