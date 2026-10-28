//! UI-thread state: models, callbacks, and applying core events.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use client_core::{
    AudioDevice, Command, Commander, Event, LoginFailure, QueuePosition, Repeat, Rgba, Settings,
    SignOutReason, TrackSource, format,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer,
    VecModel,
};

use crate::albums::{AlbumGridModel, image_key};
use crate::{AppWindow, LyricLine, QueueRow, TrackRow};

struct Ui {
    window: slint::Weak<AppWindow>,
    commander: Commander,
    albums: Rc<AlbumGridModel>,
    tracks: Rc<VecModel<TrackRow>>,
    lyrics: Rc<VecModel<LyricLine>>,
    queue: Rc<VecModel<QueueRow>>,
    now_cover_key: RefCell<Option<String>>,
    notice_timer: Timer,
    /// Engine names behind the device combo box; index 0 (`None`) is the
    /// system default.
    device_names: RefCell<Vec<Option<String>>>,
    selected_device: RefCell<Option<String>>,
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
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
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
pub fn install(window: &AppWindow, commander: Commander, settings: &Settings) {
    #[allow(clippy::cast_possible_truncation)]
    window.set_volume(settings.volume as f32);
    window.set_show_lyrics(settings.show_lyrics);
    window.set_exclusive(settings.exclusive);
    window.set_repeat(repeat_index(settings.repeat));
    window.set_shuffle(settings.shuffle);
    let albums = AlbumGridModel::new(commander.clone());
    let tracks = Rc::new(VecModel::<TrackRow>::default());
    let lyrics = Rc::new(VecModel::<LyricLine>::default());
    let queue = Rc::new(VecModel::<QueueRow>::default());
    window.set_queue(ModelRc::from(queue.clone()));
    window.set_album_rows(ModelRc::from(albums.clone()));
    window.set_tracks(ModelRc::from(tracks.clone()));
    window.set_lyrics(ModelRc::from(lyrics.clone()));

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

    let c = commander.clone();
    window.on_show_lyrics_changed(move |show| c.send(Command::SetShowLyrics(show)));
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

    let grid = albums.clone();
    let weak = window.as_weak();
    window.on_album_layout_changed(move |columns, card_size| {
        let scale = weak.upgrade().map_or(1.0, |w| w.window().scale_factor());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let px = (card_size * scale).ceil().max(0.0) as u32;
        grid.set_layout(usize::try_from(columns).unwrap_or(1), px);
    });
    let c = commander.clone();
    window.on_play_album(move |id| {
        c.send(Command::PlayAlbum {
            album_id: id.into(),
        })
    });
    let c = commander.clone();
    window.on_play_track(move |index| {
        if let Ok(index) = usize::try_from(index) {
            c.send(Command::PlayTracks { index });
        }
    });
    let c = commander.clone();
    window.on_enqueue_track(move |index, next| {
        if let Ok(index) = usize::try_from(index) {
            c.send(Command::Enqueue {
                source: TrackSource::Library(index),
                at: position(next),
            });
        }
    });
    let c = commander.clone();
    window.on_enqueue_album(move |id, next| {
        c.send(Command::Enqueue {
            source: TrackSource::Album(id.into()),
            at: position(next),
        });
    });
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
    let c = commander.clone();
    window.on_volume_changed(move |v| c.send(Command::SetVolume(f64::from(v))));

    let ui = Rc::new(Ui {
        window: window.as_weak(),
        commander,
        albums,
        tracks,
        lyrics,
        queue,
        now_cover_key: RefCell::default(),
        notice_timer: Timer::default(),
        device_names: RefCell::new(vec![None]),
        selected_device: RefCell::new(settings.audio_device.clone()),
    });
    UI.with(|cell| *cell.borrow_mut() = Some(ui));
}

fn position(next: bool) -> QueuePosition {
    if next {
        QueuePosition::Next
    } else {
        QueuePosition::Last
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

fn to_image(rgba: &Rgba) -> Image {
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

/// Applies one core event on the UI thread.
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
            *ui.now_cover_key.borrow_mut() = None;
        }
        Event::LoginFailed(failure) => {
            w.set_login_busy(false);
            w.set_login_error(login_message(&failure).into());
        }
        Event::LoggedIn { server_url } => {
            w.set_login_busy(false);
            w.set_logged_in(true);
            w.set_server_label(server_url.into());
        }
        Event::AlbumsReset => {
            ui.albums.clear();
            w.set_album_loaded(0);
            w.set_album_total(0);
        }
        Event::AlbumsAppended { items, total } => {
            ui.albums.append(items);
            w.set_album_loaded(i32::try_from(ui.albums.len()).unwrap_or(i32::MAX));
            w.set_album_total(i32::try_from(total).unwrap_or(i32::MAX));
        }
        Event::TracksReset => {
            ui.tracks.set_vec(Vec::new());
            w.set_track_total(0);
        }
        Event::TracksAppended { items, total } => {
            let offset = ui.tracks.row_count();
            let rows: Vec<TrackRow> = items
                .into_iter()
                .enumerate()
                .map(|(i, t)| TrackRow {
                    id: int(t.id),
                    index: i32::try_from(offset + i).unwrap_or(i32::MAX),
                    title: t.title.into(),
                    artist: t.artist.into(),
                    album: t.album.into(),
                    duration: t.duration.into(),
                })
                .collect();
            ui.tracks.extend(rows);
            w.set_track_total(i32::try_from(total).unwrap_or(i32::MAX));
        }
        Event::Image { key, image } => {
            let image = to_image(&image);
            ui.albums.set_cover(&key, &image);
            if ui.now_cover_key.borrow().as_deref() == Some(key.as_str()) {
                w.set_np_cover(image);
            }
        }
        Event::NowPlaying(None) => {
            w.set_has_track(false);
            w.set_playing_id(-1);
        }
        Event::NowPlaying(Some(now)) => {
            w.set_has_track(true);
            w.set_playing_id(int(now.track_id));
            w.set_np_title(now.title.into());
            w.set_np_artist(now.artist.into());
            w.set_np_album(now.album.into());
            w.set_np_quality(now.quality.unwrap_or_default().into());
            // Same size as the grid so both share the cached artwork.
            let px = ui.albums.cover_px();
            let key = (!now.artwork.is_empty()).then(|| image_key(&now.artwork, px));
            match &key {
                Some(key) => {
                    w.set_np_cover(ui.albums.cover(&now.artwork).unwrap_or_default());
                    ui.commander.send(Command::LoadImage {
                        key: key.clone(),
                        path: now.artwork,
                        px,
                    });
                }
                None => w.set_np_cover(Image::default()),
            }
            *ui.now_cover_key.borrow_mut() = key;
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
        Event::Paused(paused) => w.set_paused(paused),
        Event::Buffering(buffering) => w.set_buffering(buffering),
        #[allow(clippy::cast_possible_truncation)]
        Event::Volume(volume) => w.set_volume(volume as f32),
        Event::Lyrics {
            lines,
            synced,
            source,
        } => {
            ui.lyrics.set_vec(
                lines
                    .into_iter()
                    .map(|text| LyricLine { text: text.into() })
                    .collect::<Vec<_>>(),
            );
            w.set_lyrics_synced(synced);
            w.set_lyrics_active(-1);
            w.set_lyrics_source(source.unwrap_or_default().into());
        }
        Event::Queue { items, current } => {
            ui.queue.set_vec(
                items
                    .into_iter()
                    .map(|e| QueueRow {
                        uid: i32::try_from(e.uid).unwrap_or(i32::MAX),
                        title: e.title.into(),
                        artist: e.artist.into(),
                        duration: e.duration.into(),
                    })
                    .collect::<Vec<_>>(),
            );
            w.set_queue_current(row_index(current));
        }
        Event::QueuePosition(current) => w.set_queue_current(row_index(current)),
        Event::Repeat(repeat) => w.set_repeat(repeat_index(repeat)),
        Event::Shuffle(shuffle) => w.set_shuffle(shuffle),
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
        Event::Notice(text) => {
            tracing::warn!("{text}");
            w.set_notice(text.into());
            let weak = ui.window.clone();
            ui.notice_timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_secs(5),
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_notice(SharedString::new());
                    }
                },
            );
        }
    });
}
