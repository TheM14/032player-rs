//! Applying core events and updating UI models, covers, queue, and favorites.

use std::collections::HashSet;

use client_core::{
    AlbumSort, ArtistSort, ArtworkFit, Command, DetailView, Event, ExclusiveMode, FavoriteKind,
    PlaylistSort, ServerFeatures, SignOutReason, SleepTimer, SortOrder, TrackFilter, TrackItem,
    TrackSort, Transcode, format,
};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, VecModel};

use super::m032::{self, Bind, GRID_SERIES, GRID_SERIES_ALBUMS, GRID_WORKS, PAGE_HOME};
use super::{
    ALBUM_GRIDS, ARTIST_GRIDS, GRID_ALBUMS, GRID_ARTISTS, GRID_FAV_ALBUMS, GRID_FAV_ARTISTS,
    GRID_PLAYLISTS, NP_COVER, PAGE_ALBUMS, PAGE_PLAYLISTS, QUEUE_COVER, RELEASE_GRID_IDS,
    RELEASE_GRIDS, Route, Ui, album_sorts, artist_alias, artist_fact_rows, artist_refs,
    artist_sorts, cards, exclusive_mode_index, int, login_message, playlist_sorts, query,
    repeat_index, row_index, server_host, to_image, track_row, track_rows, track_sorts,
    transcode_options, with_ui,
};
use crate::cards::{Card, image_key, key_path, physical_box};
use crate::{
    AppWindow, DetailInfo, DetailKind, DialogKind, Home, Lib032, PlaylistChoice, QueueRow,
    ReleaseGroupRow, TrackRow,
};

impl Ui {
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
        w.set_artist_credit_roles_supported(features.artist_credit_roles);
        *self.artist_dir_role.borrow_mut() = None;
        w.set_artist_dir_role(SharedString::new());
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
        self.reset_search(w, true);
    }

    fn detail_loaded(&self, w: &AppWindow, view: DetailView, tracks: Vec<TrackItem>) {
        // Only the page on screen.
        let matches = match (&view, self.route.get()) {
            (DetailView::Album(a), Route::Album(id)) => a.id == id,
            (DetailView::Artist(a), Route::Artist(id)) => a.requested_id == id,
            (DetailView::Playlist(p), Route::Playlist(id)) => p.id == id,
            (DetailView::Similar { track_id, .. }, Route::Similar(id)) => *track_id == id,
            (DetailView::Versions { track_id, .. }, Route::Versions(id)) => *track_id == id,
            (DetailView::Work(v), Route::Work(id)) => v.id == id,
            (DetailView::Series(v), Route::Series(id)) => v.id == id,
            (DetailView::DailyMix(v), Route::DailyMix(id)) => v.id == id,
            _ => false,
        };
        if !matches {
            return;
        }
        self.unbind(Bind::Detail);
        let rows = tracks.len();
        let multi_disc = matches!(&view, DetailView::Album(a) if a.multi_disc);
        w.set_album_multi_disc(multi_disc);
        let artworks: Vec<(usize, String)> = if matches!(&view, DetailView::Versions { .. }) {
            tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.artwork.is_empty())
                .map(|(i, t)| (i, t.artwork.clone()))
                .collect()
        } else {
            Vec::new()
        };
        self.detail_tracks.set_vec(
            tracks
                .into_iter()
                .enumerate()
                .map(|(i, t)| track_row(t, i, multi_disc))
                .collect::<Vec<_>>(),
        );
        for (i, path) in artworks {
            let tracks_model = self.detail_tracks.clone();
            self.bind_image(
                w,
                Bind::Detail,
                &path,
                (54.0, 54.0),
                ArtworkFit::Contain,
                move |image| {
                    if let Some(mut row) = tracks_model.row_data(i) {
                        row.cover = image.clone();
                        tracks_model.set_row_data(i, row);
                    }
                },
            );
        }
        if self.m032_detail(w, &view, rows) {
            let (kind, id, title) = match &view {
                DetailView::Work(v) => (DetailKind::Work, v.id, v.title.clone()),
                DetailView::Series(v) => (DetailKind::Series, v.id, v.title.clone()),
                DetailView::DailyMix(v) => (DetailKind::DailyMix, v.id, v.title.clone()),
                _ => (DetailKind::Album, 0, String::new()),
            };
            w.set_detail(DetailInfo {
                kind,
                id: int(id),
                title: title.into(),
                ..DetailInfo::default()
            });
            w.set_detail_loading(false);
            *self.detail.borrow_mut() = Some(view);
            let (scroll_y, inspector_scroll_y) = self.scroll_restore.get();
            if scroll_y.abs() >= 1.0 || inspector_scroll_y.abs() >= 1.0 {
                self.restore_scroll(w, scroll_y, inspector_scroll_y);
            }
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
                        kind: DetailKind::Album,
                        id: int(a.id),
                        title: a.title.as_str().into(),
                        subtitle: a.artist.as_str().into(),
                        subtitle_id: int(a.artist_id),
                        artists: artist_refs(&a.artists),
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
                    .zip(RELEASE_GRIDS.into_iter().zip(RELEASE_GRID_IDS))
                    .map(|(group, (grid, grid_id))| {
                        let count = group.items.len();
                        self.grids[grid].set(group.items.into_iter().map(Card::from).collect());
                        ReleaseGroupRow {
                            title: group.title.into(),
                            count: i32::try_from(count).unwrap_or(i32::MAX),
                            grid: grid_id,
                            rows: ModelRc::from(self.grids[grid].clone()),
                        }
                    })
                    .collect();
                w.set_artist_groups(ModelRc::new(VecModel::from(groups)));
                w.set_artist_pure_credit(a.is_pure_credit);
                w.set_artist_credit_roles(ModelRc::new(VecModel::from(
                    a.credit_roles
                        .iter()
                        .map(|r| crate::CreditRoleTab {
                            key: r.key.as_str().into(),
                            label: r.label.as_str().into(),
                            count: int(r.count),
                        })
                        .collect::<Vec<_>>(),
                )));
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
                w.set_artist_credit_summary(
                    a.credit_roles
                        .iter()
                        .filter(|r| !r.key.is_empty())
                        .map(|r| format!("{} {} 首", r.label, r.count))
                        .collect::<Vec<_>>()
                        .join(" · ")
                        .into(),
                );
                (
                    DetailInfo {
                        kind: DetailKind::Artist,
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
                        kind: DetailKind::Playlist,
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
                    kind: DetailKind::Similar,
                    title: format!("与「{title}」相似").into(),
                    meta: format!("{} 首 · 按元数据相似度", self.detail_tracks.row_count()).into(),
                    ..DetailInfo::default()
                },
                String::new(),
            ),
            DetailView::Versions {
                title,
                composer,
                duration,
                ..
            } => (
                DetailInfo {
                    kind: DetailKind::Versions,
                    title: title.as_str().into(),
                    subtitle: if composer.is_empty() {
                        SharedString::new()
                    } else {
                        format!("作曲 {composer}").into()
                    },
                    meta: if duration.is_empty() {
                        format!("{} 条记录", self.detail_tracks.row_count()).into()
                    } else {
                        format!("{} 条记录 · {duration}", self.detail_tracks.row_count()).into()
                    },
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
        let (scroll_y, inspector_scroll_y) = self.scroll_restore.get();
        if scroll_y.abs() >= 1.0 || inspector_scroll_y.abs() >= 1.0 {
            self.restore_scroll(w, scroll_y, inspector_scroll_y);
        }
    }

    /// Fills the queue panel: entries matching the search (title or
    /// artist, case-insensitive), the current one marked.
    pub(super) fn show_queue(&self, w: &AppWindow) {
        self.unbind(Bind::Queue);
        let search = self.queue_search.borrow().to_lowercase();
        let (rows, paths, missing, total, current_row, current_pos) = {
            let all = self.queue_all.borrow();
            let (items, current) = &*all;
            let current_uid = current.and_then(|i| items.get(i)).map(|e| e.uid);
            let mut rows = Vec::new();
            let mut paths = Vec::new();
            let mut missing = Vec::new();
            let mut missing_seen = HashSet::new();
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
                if cover.is_none()
                    && !e.artwork.is_empty()
                    && missing_seen.insert(e.artwork.as_str())
                {
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
            (rows, paths, missing, items.len(), current_row, *current)
        };
        self.queue.set_vec(rows);
        *self.queue_paths.borrow_mut() = paths;
        // A cover fills every row of its album, now and on later updates.
        for path in missing {
            let queue = self.queue.clone();
            let covers = self.queue_covers.clone();
            let paths = self.queue_paths.clone();
            self.bind_image(
                w,
                Bind::Queue,
                &path.clone(),
                (QUEUE_COVER, QUEUE_COVER),
                ArtworkFit::Contain,
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
        w.set_queue_total(i32::try_from(total).unwrap_or(i32::MAX));
        w.set_queue_current(row_index(current_row));
        w.set_queue_position(row_index(current_pos));
    }

    /// Updates only the active row indicator when the current track changes,
    /// avoiding a full rebuild of the queue model.
    fn update_queue_position(&self, w: &AppWindow) {
        let all = self.queue_all.borrow();
        let (items, current) = &*all;
        let current_uid = current.and_then(|i| items.get(i)).map(|e| e.uid);
        let current_row = current_uid.and_then(|uid| {
            let uid_i32 = i32::try_from(uid).unwrap_or(i32::MAX);
            if self.queue_search.borrow().is_empty()
                && let Some(i) = *current
                && self.queue.row_data(i).is_some_and(|row| row.uid == uid_i32)
            {
                return Some(i);
            }
            (0..self.queue.row_count())
                .find(|&i| self.queue.row_data(i).is_some_and(|row| row.uid == uid_i32))
        });
        w.set_queue_current(row_index(current_row));
        w.set_queue_position(row_index(*current));
    }

    pub(super) fn check_scale(&self, w: &AppWindow) {
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
        self.rescale_m032_images(w);
        let artwork = self.now.borrow().as_ref().map(|n| n.artwork.clone());
        if let Some(artwork) = artwork.filter(|a| !a.is_empty()) {
            let px = physical_box(NP_COVER, NP_COVER, scale);
            let key = image_key(&artwork, px, ArtworkFit::Contain);
            self.send(Command::LoadImage {
                key: key.clone(),
                path: artwork,
                width: px.0,
                height: px.1,
                fit: ArtworkFit::Contain,
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
        *self.full_cover_key.borrow_mut() = None;
        self.full_changed(w);
    }

    fn mark_playing_album(&self, album_id: i64) {
        let ids: HashSet<i64> = (album_id > 0).then_some(album_id).into_iter().collect();
        for grid in ALBUM_GRIDS {
            self.grids[grid].set_playing(&ids);
        }
        // Series pages list albums too.
        self.grids[GRID_SERIES_ALBUMS].set_playing(&ids);
    }

    /// The current cover at the size of the right panel's cover.
    pub(super) fn load_now_large(&self, w: &AppWindow, artwork: &str) {
        let side = w.get_np_art_side();
        let px = physical_box(side, side, self.scale(w));
        let key = (!artwork.is_empty()).then(|| image_key(artwork, px, ArtworkFit::Contain));
        if *self.now_large_key.borrow() == key {
            return;
        }
        match &key {
            Some(key) => self.send(Command::LoadImage {
                key: key.clone(),
                path: artwork.to_owned(),
                width: px.0,
                height: px.1,
                fit: ArtworkFit::Contain,
            }),
            None => w.set_np_cover_large(Image::default()),
        }
        *self.now_large_key.borrow_mut() = key;
    }

    /// Full-screen player opened, closed, resized, switched tabs or the
    /// track changed: the original cover at the shown size.
    pub(super) fn full_changed(&self, w: &AppWindow) {
        if !w.get_full_open() {
            // Free the large image; it is reloaded (byte cache) on reopen.
            *self.full_cover_key.borrow_mut() = None;
            w.set_full_cover(Image::default());
            return;
        }
        let artwork = self
            .now
            .borrow()
            .as_ref()
            .map(|n| n.artwork.clone())
            .unwrap_or_default();
        let side = w.get_full_cover_side();
        let px = physical_box(side, side, self.scale(w));
        let key = (!artwork.is_empty() && w.get_full_cover_side() > 0.0)
            .then(|| format!("full|{}", image_key(&artwork, px, ArtworkFit::Contain)));
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
                        fit: ArtworkFit::Contain,
                    });
                }
                None => w.set_full_cover(Image::default()),
            }
            *self.full_cover_key.borrow_mut() = key;
        }
    }

    /// The detail header cover, decoded at the header's cover size
    /// (`detail-art-side`, which follows the window width tier).
    fn load_detail_cover(&self, w: &AppWindow, artwork: &str) {
        let side = w.get_detail_art_side();
        let px = physical_box(side, side, self.scale(w));
        let fit = if w.get_detail().kind == DetailKind::Artist {
            ArtworkFit::Cover
        } else {
            ArtworkFit::Contain
        };
        let key = (!artwork.is_empty()).then(|| image_key(artwork, px, fit));
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
                fit,
            });
        }
    }

    /// Reloads the open detail cover for a new header size or scale.
    pub(super) fn reload_detail_cover(&self, w: &AppWindow) {
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
                    &self.search_preview_tracks,
                    &self.played,
                    &self.pane_tracks,
                    &self.artist_favs,
                    &self.artist_credits,
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
                self.update_search_favorite(w, kind, id, favorite);
            }
            FavoriteKind::Album | FavoriteKind::Artist => {
                self.update_search_favorite(w, kind, id, favorite);
                if kind == FavoriteKind::Album {
                    for grid in ALBUM_GRIDS {
                        self.grids[grid].set_favorite(id, favorite);
                    }
                    self.grids[GRID_SERIES_ALBUMS].set_favorite(id, favorite);
                    self.set_spotlight_album_favorite(w, id, favorite);
                } else {
                    for grid in ARTIST_GRIDS {
                        self.grids[grid].set_favorite(id, favorite);
                    }
                }
                let mut info = w.get_detail();
                let kind_matches = (kind == FavoriteKind::Album && info.kind == DetailKind::Album)
                    || (kind == FavoriteKind::Artist && info.kind == DetailKind::Artist);
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
            } else {
                ui.finish_restore(w);
            }
        }
        Event::SignedOut(reason) => {
            ui.finish_restore(w);
            ui.cancel_scroll_restore();
            w.set_content_scroll_y(0.0);
            w.set_inspector_scroll_y(0.0);
            w.set_logged_in(false);
            w.set_login_busy(false);
            w.set_login_api_token(SharedString::new());
            w.set_login_media_token(SharedString::new());
            w.set_login_error(match reason {
                SignOutReason::User => SharedString::new(),
                SignOutReason::Expired => "API Token 已失效，请重新输入".into(),
            });
            w.set_server_label(SharedString::new());
            w.set_server_status(SharedString::new());
            ui.dialog.set(None);
            w.set_dialog(DialogKind::None);
            w.set_full_open(false);
            ui.full_changed(w);
            *ui.now_cover_key.borrow_mut() = None;
            *ui.now_large_key.borrow_mut() = None;
            *ui.full_cover_key.borrow_mut() = None;
            *ui.detail_cover_key.borrow_mut() = None;
            *ui.now.borrow_mut() = None;
            *ui.detail.borrow_mut() = None;
            w.set_has_track(false);
            w.set_playing_id(-1);
            w.set_np_favorite(false);
            w.set_np_cover(Image::default());
            w.set_np_cover_large(Image::default());
            w.set_detail_cover(Image::default());
            w.set_full_cover(Image::default());
            for grid in &ui.grids {
                grid.clear();
            }
            for model in [
                &ui.tracks,
                &ui.fav_tracks,
                &ui.detail_tracks,
                &ui.played,
                &ui.pane_tracks,
                &ui.artist_favs,
                &ui.artist_credits,
            ] {
                model.set_vec(Vec::new());
            }
            for model in [
                &ui.home_mixes,
                &ui.home_recent,
                &ui.home_resume,
                &ui.home_songs,
                &ui.mix_others,
            ] {
                model.set_vec(Vec::new());
            }
            ui.lyrics.set_vec(Vec::new());
            ui.lyrics_results.set_vec(Vec::new());
            ui.queue.set_vec(Vec::new());
            ui.playlist_choices.set_vec(Vec::new());
            w.set_side_playlists(ModelRc::default());
            w.set_lyrics_active(-1);
            w.set_lyrics_synced(false);
            w.set_lyrics_source(SharedString::new());
            w.set_lyrics_translated_lines(false);
            *ui.queue_all.borrow_mut() = (Vec::new(), None);
            ui.queue_search.borrow_mut().clear();
            w.set_queue_search(SharedString::new());
            ui.queue_covers.borrow_mut().clear();
            ui.queue_paths.borrow_mut().clear();
            w.set_queue_total(0);
            w.set_queue_current(-1);
            w.set_queue_position(-1);
            w.set_playing_mix(-1);
            ui.history.borrow_mut().clear();
            ui.route.set(Route::Page(PAGE_ALBUMS));
            w.set_can_go_back(false);
            ui.sent.borrow_mut().clear();
            *ui.album_artist.borrow_mut() = None;
            *ui.track_artist.borrow_mut() = None;
            w.set_album_filter(SharedString::new());
            w.set_track_filter(SharedString::new());
            *ui.artist_dir_role.borrow_mut() = None;
            w.set_artist_dir_role(SharedString::new());
            w.set_artist_credit_roles_supported(false);
            ui.reset_m032_detail(w);
            ui.reset_search(w, true);
            {
                let mut m = ui.m032.borrow_mut();
                m.reset();
                m.server.clear();
            }
        }
        Event::LoginFailed(failure) => {
            ui.finish_restore(w);
            w.set_login_busy(false);
            w.set_login_error(login_message(&failure).into());
        }
        Event::LoggedIn {
            server_url,
            features,
        } => {
            ui.finish_restore(w);
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
            {
                let mut history = ui.history.borrow_mut();
                history.retain(|entry| entry.route != Route::Playlist(id));
                history.dedup_by(|a, b| a.route == b.route);
                w.set_can_go_back(!history.is_empty());
            }
            if ui.route.get() == Route::Playlist(id) {
                ui.show(w, Route::Page(PAGE_PLAYLISTS));
            }
        }
        Event::Detail { view, tracks } => ui.detail_loaded(w, view, tracks),
        Event::AlbumWorks {
            album_id,
            works,
            tieups,
        } => ui.album_works_loaded(w, album_id, &works, &tieups),
        Event::SearchResults {
            query,
            best_match,
            artists,
            artists_total,
            albums,
            albums_total,
            tracks,
            tracks_total,
            composers,
            composers_total,
        } => ui.search_overview_loaded(
            w,
            query,
            best_match,
            artists,
            artists_total,
            albums,
            albums_total,
            tracks,
            tracks_total,
            composers,
            composers_total,
        ),
        Event::SearchCategoryReset { category } => ui.search_category_reset(w, category),
        Event::SearchCategoryAppended {
            category,
            artists,
            albums,
            tracks,
            total,
        } => ui.search_category_appended(w, category, artists, albums, tracks, total),
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
        Event::ImageFailed { key } => {
            ui.image_failed(&key);
            for grid in &ui.grids {
                grid.image_failed(&key);
            }
            if ui.now_cover_key.borrow().as_deref() == Some(key.as_str()) {
                *ui.now_cover_key.borrow_mut() = None;
            }
            if ui.now_large_key.borrow().as_deref() == Some(key.as_str()) {
                *ui.now_large_key.borrow_mut() = None;
            }
            if ui.full_cover_key.borrow().as_deref() == Some(key.as_str()) {
                *ui.full_cover_key.borrow_mut() = None;
            }
            if ui.detail_cover_key.borrow().as_deref() == Some(key.as_str()) {
                *ui.detail_cover_key.borrow_mut() = None;
            }
        }
        Event::MediaCover(_) => {}
        Event::NowPlaying(None) => {
            ui.mark_playing_album(0);
            w.set_has_track(false);
            w.set_playing_id(-1);
            w.set_np_year("".into());
            w.set_np_favorite(false);
            *ui.now_cover_key.borrow_mut() = None;
            *ui.now_large_key.borrow_mut() = None;
            w.set_np_cover(Image::default());
            w.set_np_cover_large(Image::default());
            *ui.now.borrow_mut() = None;
            ui.full_changed(w);
        }
        Event::NowPlaying(Some(now)) => {
            ui.mark_playing_album(now.album_id);
            w.set_has_track(true);
            w.set_playing_id(int(now.track_id));
            w.set_np_title(now.title.as_str().into());
            w.set_np_artist(now.artist.as_str().into());
            w.set_np_artists(artist_refs(&now.artists));
            w.set_np_album(now.album.as_str().into());
            w.set_np_year(if now.year > 0 {
                now.year.to_string().into()
            } else {
                "".into()
            });
            w.set_np_quality(now.quality.clone().unwrap_or_default().into());
            w.set_np_favorite(now.favorite);
            // Decoded at the player bar size: a large grid cover shrunk
            // by the renderer (no mipmaps) looks jagged.
            let px = physical_box(NP_COVER, NP_COVER, ui.scale(w));
            let key =
                (!now.artwork.is_empty()).then(|| image_key(&now.artwork, px, ArtworkFit::Contain));
            if *ui.now_cover_key.borrow() != key {
                match &key {
                    Some(key) => {
                        let placeholder = ui.grids.iter().find_map(|g| g.cover(&now.artwork));
                        w.set_np_cover(placeholder.unwrap_or_default());
                        ui.send(Command::LoadImage {
                            key: key.clone(),
                            path: now.artwork.clone(),
                            width: px.0,
                            height: px.1,
                            fit: ArtworkFit::Contain,
                        });
                    }
                    None => w.set_np_cover(Image::default()),
                }
                *ui.now_cover_key.borrow_mut() = key;
            }
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
            w.set_dialog(DialogKind::TrackInfo);
        }
        Event::ArtistFavorites { artist_id, tracks } => {
            if matches!(ui.detail.borrow().as_ref(), Some(DetailView::Artist(a))
                if a.id == artist_id && ui.route.get() == Route::Artist(a.requested_id))
            {
                ui.artist_favs.set_vec(track_rows(tracks, 0));
            }
        }
        Event::ArtistCredits {
            artist_id,
            active_role,
            tracks,
        } => {
            if matches!(ui.detail.borrow().as_ref(), Some(DetailView::Artist(a))
                if a.id == artist_id && ui.route.get() == Route::Artist(a.requested_id))
            {
                let rows: Vec<TrackRow> = track_rows(tracks, 0)
                    .into_iter()
                    .map(|mut row| {
                        row.tag_accent = !row.tag.is_empty();
                        row
                    })
                    .collect();
                ui.artist_credits.set_vec(rows);
                w.set_artist_credit_role(active_role.into());
            }
        }
        Event::Related { .. } => {}
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
            ui.update_queue_position(w);
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
        Event::OutputMode {
            mode,
            asio_available,
            asio_fallback,
        } => {
            w.set_exclusive(mode != ExclusiveMode::Off);
            w.set_exclusive_mode(exclusive_mode_index(mode));
            w.set_asio_available(asio_available);
            w.set_asio_fallback(asio_fallback);
            w.set_output_busy(false);
        }
        Event::AsioState(state) => ui.set_asio_state(w, state),
        Event::LyricsActive(active) => {
            w.set_lyrics_active(active.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1));
        }
        Event::LyricsReset => {
            w.set_lyrics_reset_seq(w.get_lyrics_reset_seq().wrapping_add(1));
        }
        Event::Notice(text) => {
            ui.clear_search_loading_on_notice(w, &text);
            ui.notice(text);
        }
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
