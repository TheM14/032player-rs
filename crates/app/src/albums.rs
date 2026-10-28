//! Album grid model: albums chunked into rows of `columns` cards for a
//! virtualized ListView. Covers load lazily: `row_data` is only called for
//! rows the ListView instantiates, and that is where artwork is requested.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use client_core::{AlbumItem, Command, Commander};
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

struct Entry {
    item: AlbumItem,
    cover: Option<Image>,
    requested: Cell<bool>,
}

pub struct AlbumGridModel {
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

impl AlbumGridModel {
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

    pub fn append(&self, items: Vec<AlbumItem>) {
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

    pub fn cover(&self, path: &str) -> Option<Image> {
        let by_path = self.by_path.borrow();
        let i = *by_path.get(path)?.first()?;
        self.entries.borrow().get(i)?.cover.clone()
    }
}

impl Model for AlbumGridModel {
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
                    artist: SharedString::from(entry.item.artist.as_str()),
                    year: if entry.item.year > 0 {
                        entry.item.year.to_string().into()
                    } else {
                        SharedString::new()
                    },
                    cover: entry.cover.clone().unwrap_or_default(),
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
