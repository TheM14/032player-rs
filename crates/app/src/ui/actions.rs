//! Navigation, scroll restoration, list queries, dialogs, and user actions.

use std::time::Duration;

use client_core::{
    AlbumFilter, AlbumSort, ArtistFilter, ArtistSort, Command, DetailView, FavoriteKind,
    PlaylistFilter, QueuePosition, TrackFilter, TrackList, TrackSort, TrackSource,
};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString};

use super::m032::GRID_HOME_PLAYLISTS;
use super::{
    Dialog, GRID_ARTISTS, GRID_FAV_ARTISTS, GRID_PLAYLISTS, GRID_SEARCH_ARTISTS,
    GRID_SEARCH_COMPOSERS, HistoryEntry, MAX_ARTWORK_BYTES, MAX_HISTORY, PAGE_ALBUMS, PAGE_DETAIL,
    PAGE_TRACKS, RELEASE_GRIDS, Route, Ui, album_sorts, artist_refs, artist_sorts, int, moved_away,
    order, playlist_sorts, track_sorts, with_ui, years,
};
use crate::{AppWindow, CreditPart, DetailInfo, DetailKind, DialogKind, ListQuery};

impl Ui {
    // ------------------------------------------------------------ navigation

    pub(super) fn navigate(&self, w: &AppWindow, route: Route) {
        let current = self.route.get();
        if current != route {
            let mut history = self.history.borrow_mut();
            history.push(HistoryEntry {
                route: current,
                scroll_y: w.get_content_scroll_y(),
                inspector_scroll_y: w.get_inspector_scroll_y(),
            });
            if history.len() > MAX_HISTORY {
                history.remove(0);
            }
        }
        self.show(w, route);
    }

    pub(super) fn go_back(&self, w: &AppWindow) {
        let previous = self.history.borrow_mut().pop();
        if let Some(entry) = previous {
            self.show_with_scroll(w, entry.route, entry.scroll_y, entry.inspector_scroll_y);
        }
    }

    pub(super) fn show(&self, w: &AppWindow, route: Route) {
        self.show_with_scroll(w, route, 0.0, 0.0);
    }

    pub(super) fn cancel_scroll_restore(&self) {
        self.scroll_timer.stop();
        self.scroll_restore.set((0.0, 0.0));
        self.scroll_restore_ticks.set(0);
        self.scroll_seen.set((None, None));
    }

    /// Applies a back-navigation offset for about two seconds while asynchronous
    /// detail content grows. A short-lived page cannot clamp the saved offset to
    /// zero before its tracks or cards arrive.
    pub(super) fn restore_scroll(&self, w: &AppWindow, scroll_y: f32, inspector_scroll_y: f32) {
        self.cancel_scroll_restore();
        w.set_content_scroll_y(scroll_y);
        w.set_inspector_scroll_y(inspector_scroll_y);
        if scroll_y.abs() < 1.0 && inspector_scroll_y.abs() < 1.0 {
            return;
        }
        self.scroll_restore.set((scroll_y, inspector_scroll_y));
        self.scroll_restore_ticks.set(125);
        self.scroll_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            || {
                with_ui(|ui, w| {
                    let (scroll_y, inspector_scroll_y) = ui.scroll_restore.get();
                    let scroll_needed = scroll_y.abs() >= 1.0;
                    let inspector_needed = inspector_scroll_y.abs() >= 1.0;
                    let cur_scroll = w.get_content_scroll_y();
                    let cur_inspector = w.get_inspector_scroll_y();
                    let scroll_done = !scroll_needed || (cur_scroll - scroll_y).abs() < 1.0;
                    let inspector_done =
                        !inspector_needed || (cur_inspector - inspector_scroll_y).abs() < 1.0;
                    let (mut seen_scroll, mut seen_inspector) = ui.scroll_seen.get();
                    let user_scrolled = (scroll_needed
                        && !scroll_done
                        && seen_scroll.is_some_and(|prev| moved_away(cur_scroll, prev, scroll_y)))
                        || (inspector_needed
                            && !inspector_done
                            && seen_inspector.is_some_and(|prev| {
                                moved_away(cur_inspector, prev, inspector_scroll_y)
                            }));
                    let ticks = ui.scroll_restore_ticks.get().saturating_sub(1);
                    ui.scroll_restore_ticks.set(ticks);
                    if user_scrolled || (scroll_done && inspector_done) || ticks == 0 {
                        ui.cancel_scroll_restore();
                        return;
                    }
                    if scroll_needed && !scroll_done {
                        seen_scroll = Some(cur_scroll);
                        w.set_content_scroll_y(scroll_y);
                    }
                    if inspector_needed && !inspector_done {
                        seen_inspector = Some(cur_inspector);
                        w.set_inspector_scroll_y(inspector_scroll_y);
                    }
                    ui.scroll_seen.set((seen_scroll, seen_inspector));
                });
            },
        );
    }

    fn show_with_scroll(
        &self,
        w: &AppWindow,
        route: Route,
        scroll_y: f32,
        inspector_scroll_y: f32,
    ) {
        // Going to a page (album / artist links, menus) leaves full screen.
        if w.get_full_open() {
            w.set_full_open(false);
            self.full_changed(w);
        }
        self.restore_scroll(w, scroll_y, inspector_scroll_y);
        self.route.set(route);
        w.set_can_go_back(!self.history.borrow().is_empty());
        let (kind, id) = match route {
            Route::Page(page) => {
                w.set_page(page);
                self.page_shown(w, page);
                return;
            }
            Route::Album(id) => (DetailKind::Album, id),
            Route::Artist(id) => (DetailKind::Artist, id),
            Route::Playlist(id) => (DetailKind::Playlist, id),
            Route::Similar(id) => (DetailKind::Similar, id),
            Route::Versions(id) => (DetailKind::Versions, id),
            Route::Work(id) => (DetailKind::Work, id),
            Route::Series(id) => (DetailKind::Series, id),
            Route::DailyMix(id) => (DetailKind::DailyMix, id),
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
        w.set_album_multi_disc(false);
        self.detail_tracks.set_vec(Vec::new());
        self.artist_favs.set_vec(Vec::new());
        self.artist_credits.set_vec(Vec::new());
        w.set_artist_credit_roles(ModelRc::default());
        w.set_artist_credit_role(SharedString::new());
        w.set_artist_pure_credit(false);
        w.set_artist_credit_summary(SharedString::new());
        for grid in RELEASE_GRIDS {
            self.grids[grid].clear();
        }
        w.set_artist_groups(ModelRc::default());
        w.set_artist_about(SharedString::new());
        w.set_artist_facts(ModelRc::default());
        self.reset_m032_detail(w);
        w.set_page(PAGE_DETAIL);
        self.send(match route {
            Route::Album(id) => Command::OpenAlbum(id),
            Route::Artist(id) => Command::OpenArtist(id),
            Route::Playlist(id) => Command::OpenPlaylist(id),
            Route::Similar(id) => Command::OpenSimilarById(id),
            Route::Versions(id) => Command::OpenVersionsById(id),
            Route::Work(id) => Command::OpenWork(id),
            Route::Series(id) => Command::OpenSeries(id),
            Route::DailyMix(id) => Command::OpenDailyMix(id),
            Route::Page(_) => return,
        });
    }

    // ------------------------------------------------------------ queries

    /// Sends a list query unless it is the one already shown.
    pub(super) fn query(&self, w: &AppWindow, target: i32, q: ListQuery) {
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
                let role = if target == 7 && w.get_artist_credit_roles_supported() {
                    *self.artist_dir_role.borrow()
                } else {
                    None
                };
                let filter = ArtistFilter {
                    sort: artist_sorts(browse).get(q.sort).unwrap_or(ArtistSort::Name),
                    order: order(q.desc),
                    search,
                    all_roles: target == 6 || target == 7,
                    role,
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
    pub(super) fn filter_by_artist(
        &self,
        w: &AppWindow,
        albums: bool,
        artist: Option<(i64, String)>,
    ) {
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

    pub(super) fn choose_artist(&self, w: &AppWindow, refs: ModelRc<CreditPart>, fallback: i64) {
        match refs.row_count() {
            0 if fallback > 0 => self.navigate(w, Route::Artist(fallback)),
            1 => {
                if let Some(a) = refs.row_data(0) {
                    self.navigate(w, Route::Artist(a.id.into()));
                }
            }
            n if n > 1 => {
                w.set_artist_choices(refs);
                w.set_dialog(DialogKind::PickArtist);
            }
            _ => {}
        }
    }

    pub(super) fn open_dialog(&self, w: &AppWindow, dialog: Dialog, name: &str, description: &str) {
        self.dialog.set(Some(dialog));
        w.set_dialog_name(name.into());
        w.set_dialog_description(description.into());
        w.set_dialog(match dialog {
            Dialog::New(_) => DialogKind::NewPlaylist,
            Dialog::Edit(_) => DialogKind::EditPlaylist,
            Dialog::Delete(_) => DialogKind::DeletePlaylist,
        });
    }

    pub(super) fn card_action(&self, w: &AppWindow, grid: usize, action: &str, id: i64, arg: i64) {
        if self.m032_card_action(w, grid, action, id, arg) {
            return;
        }
        let card = self.grids.get(grid).and_then(|g| g.item(id));
        let (source, kind) = match grid {
            GRID_ARTISTS | GRID_FAV_ARTISTS | GRID_SEARCH_ARTISTS | GRID_SEARCH_COMPOSERS => {
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
            "artist" if arg > 0 => self.navigate(w, Route::Artist(arg)),
            "artists" => {
                let refs = card
                    .as_ref()
                    .map(|c| artist_refs(&c.artists))
                    .unwrap_or_default();
                self.choose_artist(w, refs, arg);
            }
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
    pub(super) fn source_action(&self, action: &str, source: TrackSource, arg: i64) {
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

    pub(super) fn track_action(
        &self,
        w: &AppWindow,
        list: TrackList,
        action: &str,
        index: usize,
        arg: i64,
    ) {
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
            "similar" => {
                if let Some((id, _, _)) = row {
                    self.navigate(w, Route::Similar(id));
                }
            }
            "versions" => {
                if let Some((id, _, _)) = row {
                    self.navigate(w, Route::Versions(id));
                }
            }
            "play-disc" => self.send(Command::PlayAlbumDisc(i32::try_from(arg).unwrap_or(1))),
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
            "artists" => {
                let refs = self
                    .track_model(list)
                    .and_then(|m| m.row_data(index))
                    .map(|r| r.artists)
                    .or_else(|| {
                        match list {
                            TrackList::HomeResume => self.home_resume.row_data(index),
                            TrackList::HomeRecent => self.home_recent.row_data(index),
                            TrackList::HomeSongs => self.home_songs.row_data(index),
                            _ => None,
                        }
                        .map(|r| r.artists)
                    })
                    .unwrap_or_default();
                self.choose_artist(w, refs, arg);
            }
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

    pub(super) fn detail_action(&self, w: &AppWindow, action: &str, arg: i64) {
        let Some(view) = self.detail.borrow().clone() else {
            return;
        };
        let (source, add_source) = match &view {
            DetailView::Album(a) => (
                TrackSource::List(TrackList::Detail),
                TrackSource::Album(a.id),
            ),
            DetailView::Artist(a) => (TrackSource::Artist(a.id), TrackSource::Artist(a.id)),
            DetailView::Playlist(_) | DetailView::Similar { .. } | DetailView::Versions { .. } => (
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
            ("artists", _) => {
                let refs = match &view {
                    DetailView::Album(a) => artist_refs(&a.artists),
                    _ => ModelRc::default(),
                };
                self.choose_artist(w, refs, arg);
            }
            ("work", _) => self.navigate(w, Route::Work(arg)),
            ("add", _) => self.source_action("add", add_source, arg),
            ("new-playlist", view) => {
                let name = match view {
                    DetailView::Album(a) => a.title.clone(),
                    DetailView::Artist(a) => a.name.clone(),
                    DetailView::Playlist(p) => p.name.clone(),
                    DetailView::Similar { title, .. } => format!("与「{title}」相似"),
                    DetailView::Versions { title, .. } => title.clone(),
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
            ("artwork", DetailView::Playlist(p)) => self.pick_artwork(w, p.id),
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

    /// Asks for an image file asynchronously and reads it off the UI thread
    /// before uploading it as the playlist artwork.
    fn pick_artwork(&self, w: &AppWindow, playlist_id: i64) {
        let handle = w.window().window_handle();
        let fut = os::dialog::pick_image_async(Some(&handle));
        let commander = self.commander.clone();
        let _ = slint::spawn_local(async move {
            let Some(path) = fut.await else {
                return;
            };
            std::thread::spawn(move || {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if size > MAX_ARTWORK_BYTES {
                    let _ = slint::invoke_from_event_loop(|| {
                        with_ui(|ui, _| ui.notice("图片太大（最大 10 MB）".into()));
                    });
                    return;
                }
                match std::fs::read(&path) {
                    Ok(bytes) => commander.send(Command::SetPlaylistArtwork {
                        playlist_id,
                        image: Some(bytes),
                    }),
                    Err(e) => {
                        let msg = format!("读取图片失败：{e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            with_ui(|ui, _| ui.notice(msg));
                        });
                    }
                }
            });
        });
    }

    pub(super) fn dialog_confirm(&self, name: &str, description: &str) {
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
}
