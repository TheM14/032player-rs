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

/// Image request key: display size and artwork path.
pub fn image_key(path: &str, px: u32) -> String {
    format!("{px}|{path}")
}

fn parse_key(key: &str) -> Option<(u32, &str)> {
    let (px, path) = key.split_once('|')?;
    Some((px.parse().ok()?, path))
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
}

impl From<AlbumItem> for Card {
    fn from(a: AlbumItem) -> Self {
        let badge = match a.relation.as_str() {
            "collaboration" => "合作",
            "appearance" => "参与",
            "credit" => "制作",
            _ => "",
        };
        Self {
            id: a.id,
            title: a.title,
            subtitle: a.artist,
            year: a.year,
            artwork: a.artwork,
            artist_id: a.artist_id,
            favorite: a.favorite,
            badge: badge.into(),
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
    cover: Option<Image>,
    requested: Cell<bool>,
}

pub struct CardGridModel {
    entries: RefCell<Vec<Entry>>,
    /// Entry indices by artwork path.
    by_path: RefCell<HashMap<String, Vec<usize>>>,
    columns: Cell<usize>,
    /// Physical size covers are decoded at (card size x scale factor).
    cover_px: Cell<u32>,
    /// Entry indices holding a cover, oldest first.
    loaded: RefCell<VecDeque<usize>>,
    notify: ModelNotify,
    commander: Commander,
}

impl CardGridModel {
    pub fn new(commander: Commander) -> Rc<Self> {
        Rc::new(Self {
            entries: RefCell::default(),
            by_path: RefCell::default(),
            columns: Cell::new(4),
            cover_px: Cell::new(256),
            loaded: RefCell::default(),
            notify: ModelNotify::default(),
            commander,
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
            for item in items {
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
        let old_rows = Self::rows_for(old_len, columns);
        let new_rows = Self::rows_for(new_len, columns);
        if old_len % columns != 0 && old_rows > 0 {
            self.notify.row_changed(old_rows - 1);
        }
        if new_rows > old_rows {
            self.notify.row_added(old_rows, new_rows - old_rows);
        }
    }

    pub fn cover_px(&self) -> u32 {
        self.cover_px.get()
    }

    /// New column count and cover size (physical px). Covers reload only
    /// when the size grows past 125% (blurry) or drops under 60% (wasteful)
    /// of the decoded size, not on every resize step.
    pub fn set_layout(&self, columns: usize, px: u32) {
        let columns = columns.max(1);
        let px = px.clamp(64, 1536);
        let current = self.cover_px.get();
        let reload = px * 4 > current * 5 || px * 5 < current * 3;
        if reload {
            self.cover_px.set(px);
            let mut entries = self.entries.borrow_mut();
            for entry in entries.iter_mut() {
                entry.cover = None;
                entry.requested.set(false);
            }
            self.loaded.borrow_mut().clear();
        }
        if reload || columns != self.columns.get() {
            self.columns.set(columns);
            self.notify.reset();
        }
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
                    entry.cover = Some(image.clone());
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
        self.entries.borrow().get(i)?.cover.clone()
    }
}

impl Model for CardGridModel {
    type Data = AlbumRow;

    fn row_count(&self) -> usize {
        Self::rows_for(self.entries.borrow().len(), self.columns.get())
    }

    fn row_data(&self, row: usize) -> Option<AlbumRow> {
        let entries = self.entries.borrow();
        let columns = self.columns.get();
        let start = row * columns;
        if start >= entries.len() {
            return None;
        }
        let end = (start + columns).min(entries.len());
        let px = self.cover_px.get();
        let cards: Vec<AlbumCard> = entries[start..end]
            .iter()
            .map(|entry| {
                let path = &entry.item.artwork;
                if entry.cover.is_none() && !path.is_empty() && !entry.requested.get() {
                    entry.requested.set(true);
                    self.commander.send(Command::LoadImage {
                        key: image_key(path, px),
                        path: path.clone(),
                        px,
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
                    cover: entry.cover.clone().unwrap_or_default(),
                    artist_id: i32::try_from(entry.item.artist_id).unwrap_or(0),
                    favorite: entry.item.favorite,
                    badge: SharedString::from(entry.item.badge.as_str()),
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
