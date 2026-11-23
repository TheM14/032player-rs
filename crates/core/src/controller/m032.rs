//! 032 pages on the core loop: home, works, series, daily mixes, playback
//! history, and which works / series / mix the current track belongs to.

use std::collections::{HashSet, VecDeque};

use api::{
    ApiError, Feature, HomeAlbumPane, HomeSongRow, HomeWorkCard, Page, PlaybackRecord, Query,
    RotationPage, Track, TrackWork, WorkListQuery,
};

use super::library::load_pages;
use super::{Core, DetailKey, Event, Intent, Internal, Loaded, Target};
use crate::browse::TrackList;
use crate::m032::{
    self, MixRow, RESUME_SHOWN, SpotlightTab, album_pane, mix_item, recent_item, record_item,
    rotation, series_item, song_item, work_card_item, work_item,
};

/// Polls today's mixes this often while they are being generated, at
/// most `MIX_POLLS` times.
const MIX_POLL: std::time::Duration = std::time::Duration::from_secs(5);
const MIX_POLLS: u32 = 24;
/// 最近播放 asks for this many recently played tracks.
const RECENT_FETCH: u32 = 16;
/// Rows of 最近播放 next to 接着听; with daily mixes the left column holds
/// `COLUMN_ROWS` rows in total, 最近播放 at least `MIN_RECENT`.
const RECENT_ROWS: usize = 5;
const COLUMN_ROWS: usize = 7;
const MIN_RECENT: usize = 2;
/// Works / series / history pages.
const SERIES_PAGE: u32 = 200;
/// Remembered source -> tracks expansions (playing badges on old servers).
const MAX_SOURCES: usize = 200;

/// What the home page holds between partial reloads.
#[derive(Default)]
pub(super) struct HomeState {
    works_kind: String,
    works_generation: String,
    works_batch: u32,
    songs_generation: String,
    songs_batch: u32,
    chronicle_years: Vec<api::YearCount>,
    random_album: i64,
    resume: Vec<PlaybackRecord>,
    listening_resume: Option<PlaybackRecord>,
    recent: Vec<Track>,
    mixes: Option<api::DailyMixList>,
    mixes_date: Option<chrono::NaiveDate>,
    mix_polls: u32,
    mix_poll_gen: u64,
    mix_poll_task: Option<tokio::task::AbortHandle>,
}

impl Drop for HomeState {
    fn drop(&mut self) {
        if let Some(task) = self.mix_poll_task.take() {
            task.abort();
        }
    }
}

impl HomeState {
    pub fn fresh_mixes(&self) -> Option<&api::DailyMixList> {
        (self.mixes_date == Some(chrono::Local::now().date_naive()))
            .then_some(self.mixes.as_ref())
            .flatten()
    }

    pub fn cancel_mix_poll(&mut self) {
        self.mix_poll_gen += 1;
        if let Some(task) = self.mix_poll_task.take() {
            task.abort();
        }
    }

    pub fn mix_poll_gen(&self) -> u64 {
        self.mix_poll_gen
    }

    pub fn set_track_favorite(&mut self, id: i64, favorite: bool) {
        for track in &mut self.recent {
            if track.id == id {
                track.is_favorite = favorite;
            }
        }
        for rec in &mut self.resume {
            if rec.track.id == id {
                rec.track.is_favorite = favorite;
            }
        }
        if let Some(rec) = self.listening_resume.as_mut()
            && rec.track.id == id
        {
            rec.track.is_favorite = favorite;
        }
    }
}

/// The open daily mix page.
pub(super) struct OpenMix {
    pub id: i64,
    pub rows: Vec<MixRow>,
}

/// A work or series expanded into tracks this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum SourceKey {
    Work(i64),
    Series(i64),
}

/// Track ids per expanded source, oldest first (`playing-source-map.ts`).
#[derive(Default)]
pub(super) struct SourceTracks(VecDeque<(SourceKey, HashSet<i64>)>);

impl SourceTracks {
    pub fn record(&mut self, key: SourceKey, ids: HashSet<i64>) {
        self.0.retain(|(k, _)| *k != key);
        self.0.push_back((key, ids));
        while self.0.len() > MAX_SOURCES {
            self.0.pop_front();
        }
    }

    fn containing(&self, track_id: i64) -> impl Iterator<Item = SourceKey> + '_ {
        self.0
            .iter()
            .filter(move |(_, ids)| ids.contains(&track_id))
            .map(|(k, _)| *k)
    }
}

/// Where the current track belongs, for playing badges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayingSources {
    pub works: Vec<i64>,
    pub series: Vec<i64>,
    /// The daily mix the current entry was queued from.
    pub mix: Option<i64>,
}

fn not_available(e: &ApiError) -> bool {
    matches!(e.status(), Some(404 | 501))
}

impl Core {
    fn has(&self, feature: Feature) -> bool {
        self.library
            .as_ref()
            .is_some_and(|l| l.capabilities.has(feature))
    }

    // ------------------------------------------------------------ home

    pub(super) fn load_home(&mut self) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let state = self.home.get_or_insert_with(HomeState::default);
        state.mix_polls = 0;
        state.cancel_mix_poll();
        let seq = self.next_seq(Target::Home);
        let home_client = client.clone();
        let home_tx = tx.clone();
        tokio::spawn(async move {
            let msg = match home_client.home().await {
                Ok(home) => Internal::Loaded(Target::Home, seq, Loaded::Home(Box::new(home))),
                Err(e) if not_available(&e) => {
                    Internal::Loaded(Target::Home, seq, Loaded::HomeUnavailable)
                }
                Err(e) => Self::failed(&home_client, "加载主页", e),
            };
            let _ = home_tx.send(msg);
        });
        self.load_recent();
        self.load_mixes();
    }

    fn load_recent(&mut self) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::HomeRecent);
        tokio::spawn(async move {
            let mut q = Query::new();
            q.push("sort", "recentlyPlayed").push("limit", RECENT_FETCH);
            let msg = match client.get::<Page<Track>>("/tracks", &q).await {
                Ok(page) => {
                    Internal::Loaded(Target::HomeRecent, seq, Loaded::HomeRecent(page.items))
                }
                Err(e) => Self::failed(&client, "加载最近播放", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn load_mixes(&mut self) {
        if !self.has(Feature::DailyMixes) {
            return;
        }
        let Some((client, tx)) = self.request() else {
            return;
        };
        if let Some(state) = self.home.as_mut() {
            state.cancel_mix_poll();
        }
        let seq = self.next_seq(Target::DailyMixes);
        tokio::spawn(async move {
            match client.daily_mixes().await {
                Ok(list) => {
                    let _ = tx.send(Internal::Loaded(
                        Target::DailyMixes,
                        seq,
                        Loaded::DailyMixes(list),
                    ));
                }
                Err(e) => {
                    tracing::debug!("daily mixes: {e}");
                    if let Some(msg) = super::unauthorized(&client, &e) {
                        let _ = tx.send(msg);
                    } else {
                        let _ = tx.send(Internal::Loaded(
                            Target::DailyMixes,
                            seq,
                            Loaded::DailyMixesFailed,
                        ));
                    }
                }
            }
        });
    }

    /// The works rotation: another type (`next` false: from batch 0) or the
    /// next batch of the shown one.
    pub(super) fn home_works(&mut self, kind: Option<String>, next: bool) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let Some(state) = self.home.as_ref() else {
            return;
        };
        let kind = kind.unwrap_or_else(|| state.works_kind.clone());
        let batch = if next { state.works_batch + 1 } else { 0 };
        let generation = state.works_generation.clone();
        let seq = self.next_seq(Target::HomeWorks);
        tokio::spawn(async move {
            let msg = match client.home_works(&kind, batch, &generation).await {
                Ok(page) => Internal::Loaded(Target::HomeWorks, seq, Loaded::HomeWorks(page)),
                Err(e) => Self::failed(&client, "加载作品", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn home_songs_next(&mut self) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let Some(state) = self.home.as_ref() else {
            return;
        };
        let batch = state.songs_batch + 1;
        let generation = state.songs_generation.clone();
        let seq = self.next_seq(Target::HomeSongs);
        tokio::spawn(async move {
            let msg = match client.home_songs(batch, &generation).await {
                Ok(page) => Internal::Loaded(Target::HomeSongs, seq, Loaded::HomeSongs(page)),
                Err(e) => Self::failed(&client, "加载主题曲", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn home_chronicle(&mut self, year: i32) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::HomeChronicle);
        tokio::spawn(async move {
            let msg = match client.home_chronicle(year).await {
                Ok(c) => Internal::Loaded(Target::HomeChronicle, seq, Loaded::HomeChronicle(c)),
                Err(e) => Self::failed(&client, "加载编年", e),
            };
            let _ = tx.send(msg);
        });
    }

    /// 换一张: another random album for the spotlight.
    pub(super) fn home_reroll(&mut self) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let exclude = self.home.as_ref().map_or(0, |s| s.random_album);
        let seq = self.next_seq(Target::HomeSpotlight);
        tokio::spawn(async move {
            let msg = match client.home_spotlight_random(exclude).await {
                Ok(p) => Internal::Loaded(
                    Target::HomeSpotlight,
                    seq,
                    Loaded::HomeSpotlight(Box::new(p)),
                ),
                Err(e) => Self::failed(&client, "换一张", e),
            };
            let _ = tx.send(msg);
        });
    }

    fn renewed(&self, renewed: bool) {
        if renewed {
            self.emit(Event::Notice("今日推荐已更新".into()));
        }
    }

    fn works_page(&mut self, page: &RotationPage<HomeWorkCard>) {
        if let Some(state) = self.home.as_mut() {
            state.works_kind = if page.kind.is_empty() {
                "all".into()
            } else {
                page.kind.clone()
            };
            state.works_generation.clone_from(&page.generation);
            state.works_batch = page.batch;
        }
    }

    fn songs_page(&mut self, page: &RotationPage<HomeSongRow>) {
        if let Some(state) = self.home.as_mut() {
            state.songs_generation.clone_from(&page.generation);
            state.songs_batch = page.batch;
        }
        self.set_list(
            TrackList::HomeSongs,
            page.items.iter().map(|r| r.track.clone()).collect(),
        );
    }

    /// 最近播放 as shown: without 接着听's rows, shorter next to daily mixes.
    fn emit_recent(&mut self) {
        let Some(state) = self.home.as_ref() else {
            return;
        };
        let mixes = state.mixes.as_ref().map_or(0, |m| m.items.len());
        let (exclude, count): (Vec<i64>, usize) = if mixes > 0 {
            (
                Vec::new(),
                MIN_RECENT.max(COLUMN_ROWS.saturating_sub(mixes)),
            )
        } else {
            (
                state
                    .resume
                    .iter()
                    .take(RESUME_SHOWN)
                    .map(|r| r.track_id)
                    .collect(),
                RECENT_ROWS,
            )
        };
        let rows = m032::recent_rows(&state.recent, &exclude, count);
        let items = rows.iter().map(recent_item).collect();
        self.set_list(TrackList::HomeRecent, rows);
        self.emit(Event::HomeRecent(items));
    }

    fn emit_mixes(&self) {
        let Some(state) = self.home.as_ref() else {
            return;
        };
        let Some(list) = state.mixes.as_ref() else {
            return;
        };
        self.emit(Event::DailyMixes {
            generating: list.status == "generating" && state.mix_poll_task.is_some(),
            items: list.items.iter().map(mix_item).collect(),
        });
    }

    /// Applies a home result (see [`Core::loaded`]).
    pub(super) fn home_loaded(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::Home(home) => {
                let home = *home;
                let view = m032::home_view(&home);
                let spotlight = home.spotlight.as_ref();
                for (tab, pane) in [
                    (
                        SpotlightTab::Recent,
                        spotlight.and_then(|s| s.recent.as_ref()),
                    ),
                    (
                        SpotlightTab::Listening,
                        spotlight.and_then(|s| s.listening.as_ref()),
                    ),
                    (
                        SpotlightTab::Random,
                        spotlight.and_then(|s| s.random.as_ref()),
                    ),
                ] {
                    self.set_list(
                        TrackList::Spotlight(tab),
                        pane.map(|p| p.tracks.clone()).unwrap_or_default(),
                    );
                }
                self.set_list(
                    TrackList::HomeResume,
                    home.resume
                        .iter()
                        .take(RESUME_SHOWN)
                        .map(|r| r.track.clone())
                        .collect(),
                );
                self.works_page(&home.works);
                self.songs_page(&home.songs);
                if let Some(state) = self.home.as_mut() {
                    state.chronicle_years = home
                        .chronicle
                        .as_ref()
                        .map(|c| c.years.clone())
                        .unwrap_or_default();
                    state.random_album = spotlight
                        .and_then(|s| s.random.as_ref())
                        .map_or(0, |p| p.album.id);
                    state.listening_resume = spotlight
                        .and_then(|s| s.listening.as_ref())
                        .and_then(|p| p.resume.clone());
                    state.resume = home.resume;
                }
                self.emit(Event::Home(Box::new(view)));
                self.emit_recent();
                self.emit_mixes();
            }
            Loaded::HomeUnavailable => self.emit(Event::HomeUnavailable),
            Loaded::HomeRecent(tracks) => {
                if let Some(state) = self.home.as_mut() {
                    state.recent = tracks;
                }
                self.emit_recent();
            }
            Loaded::DailyMixes(list) => {
                let generating = list.status == "generating";
                let list_gen = self.list_generation;
                let tx = self.internal.clone();
                let Some(state) = self.home.as_mut() else {
                    return;
                };
                state.cancel_mix_poll();
                state.mixes = Some(list);
                state.mixes_date = Some(chrono::Local::now().date_naive());
                if generating && state.mix_polls < MIX_POLLS {
                    state.mix_polls += 1;
                    let poll_gen = state.mix_poll_gen;
                    let handle = tokio::spawn(async move {
                        tokio::time::sleep(MIX_POLL).await;
                        let _ = tx.send(Internal::PollMixes { list_gen, poll_gen });
                    });
                    state.mix_poll_task = Some(handle.abort_handle());
                }
                self.emit_mixes();
                self.emit_recent();
            }
            Loaded::DailyMixesFailed => {
                let Some(state) = self.home.as_mut() else {
                    return;
                };
                state.cancel_mix_poll();
                let items = state
                    .mixes
                    .as_ref()
                    .map(|m| m.items.iter().map(mix_item).collect())
                    .unwrap_or_default();
                self.emit(Event::DailyMixes {
                    generating: false,
                    items,
                });
            }
            Loaded::HomeWorks(page) => {
                self.renewed(page.renewed);
                self.works_page(&page);
                self.emit(Event::HomeWorks(rotation(&page, work_card_item)));
            }
            Loaded::HomeSongs(page) => {
                self.renewed(page.renewed);
                self.songs_page(&page);
                self.emit(Event::HomeSongs(rotation(&page, song_item)));
            }
            Loaded::HomeChronicle(result) => {
                let years = self
                    .home
                    .as_ref()
                    .map(|s| s.chronicle_years.clone())
                    .unwrap_or_default();
                self.emit(Event::HomeChronicle(m032::chronicle_view(&years, &result)));
            }
            Loaded::HomeSpotlight(pane) => self.spotlight_random(&pane),
            _ => {}
        }
    }

    fn spotlight_random(&mut self, pane: &HomeAlbumPane) {
        if let Some(state) = self.home.as_mut() {
            state.random_album = pane.album.id;
        }
        self.set_list(
            TrackList::Spotlight(SpotlightTab::Random),
            pane.tracks.clone(),
        );
        self.emit(Event::HomeSpotlightPane(album_pane(
            SpotlightTab::Random,
            pane,
        )));
    }

    /// 从 x 继续 on the 最近在听 spotlight tab.
    pub(super) fn resume_listening(&mut self) {
        let Some(record) = self.home.as_ref().and_then(|s| s.listening_resume.clone()) else {
            return;
        };
        self.begin_play_intent();
        self.fetched(
            vec![record.track],
            Intent::play_at(0).starting_at(record.position_millis),
        );
    }

    /// Plays one row alone; 接着听 rows start at their saved position.
    pub(super) fn play_track(&mut self, list: TrackList, index: usize) {
        let Some(track) = self
            .library
            .as_ref()
            .and_then(|l| l.list(list).get(index).cloned())
        else {
            return;
        };
        let start_ms = match list {
            TrackList::HomeResume => self
                .home
                .as_ref()
                .and_then(|s| s.resume.get(index))
                .map_or(0, |r| r.position_millis),
            _ => 0,
        };
        self.begin_play_intent();
        self.fetched(vec![track], Intent::play_at(0).starting_at(start_ms));
    }

    // ------------------------------------------------------------ lists

    pub(super) fn query_works(&mut self, query: WorkListQuery) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::Works);
        self.emit(Event::WorksReset);
        let mut q = Query::new();
        let search = query.search.trim();
        if !search.is_empty() {
            q.push("q", search);
        }
        if !query.kind.is_empty() {
            q.push("type", &query.kind);
        }
        q.push("sort", query.sort.as_str());
        let handle = tokio::spawn(async move {
            let mut sent_any = false;
            let result = load_pages::<api::Work>(
                &client,
                "/works",
                &q,
                super::PAGE,
                None,
                |items, total| {
                    sent_any = true;
                    let items = items.iter().map(work_item).collect();
                    tx.send(Internal::Loaded(
                        Target::Works,
                        seq,
                        Loaded::WorksPage(items, total),
                    ))
                    .is_ok()
                },
            )
            .await;
            if let Err(e) = result {
                if !sent_any {
                    let _ = tx.send(Internal::Loaded(
                        Target::Works,
                        seq,
                        Loaded::WorksPage(Vec::new(), 0),
                    ));
                }
                let _ = tx.send(Self::failed(&client, "加载作品", e));
            }
        });
        self.track_task(Target::Works, handle.abort_handle());
    }

    pub(super) fn query_series(&mut self, search: String) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::SeriesList);
        self.emit(Event::SeriesReset);
        let mut q = Query::new();
        let search = search.trim();
        if !search.is_empty() {
            q.push("q", search);
        }
        let handle = tokio::spawn(async move {
            let mut sent_any = false;
            let result = load_pages::<api::Series>(
                &client,
                "/series",
                &q,
                SERIES_PAGE,
                None,
                |items, total| {
                    sent_any = true;
                    let items = items.iter().map(series_item).collect();
                    tx.send(Internal::Loaded(
                        Target::SeriesList,
                        seq,
                        Loaded::SeriesPage(items, total),
                    ))
                    .is_ok()
                },
            )
            .await;
            if let Err(e) = result {
                if !sent_any {
                    let _ = tx.send(Internal::Loaded(
                        Target::SeriesList,
                        seq,
                        Loaded::SeriesPage(Vec::new(), 0),
                    ));
                }
                let _ = tx.send(Self::failed(&client, "加载系列", e));
            }
        });
        self.track_task(Target::SeriesList, handle.abort_handle());
    }

    pub(super) fn query_history(&mut self) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::History);
        self.set_list(TrackList::History, Vec::new());
        self.emit(Event::HistoryReset);
        let handle = tokio::spawn(async move {
            let mut sent_any = false;
            let result = load_pages::<PlaybackRecord>(
                &client,
                "/playback/history",
                &Query::new(),
                super::PAGE,
                None,
                |items, total| {
                    sent_any = true;
                    tx.send(Internal::Loaded(
                        Target::History,
                        seq,
                        Loaded::HistoryPage(items, total),
                    ))
                    .is_ok()
                },
            )
            .await;
            if let Err(e) = result {
                if !sent_any {
                    let _ = tx.send(Internal::Loaded(
                        Target::History,
                        seq,
                        Loaded::HistoryPage(Vec::new(), 0),
                    ));
                }
                let _ = tx.send(Self::failed(&client, "加载播放记录", e));
            }
        });
        self.track_task(Target::History, handle.abort_handle());
    }

    pub(super) fn history_page(&mut self, records: Vec<PlaybackRecord>, total: usize) {
        let items = records.iter().map(record_item).collect();
        if let Some(library) = self.library.as_mut() {
            library
                .lists
                .entry(TrackList::History)
                .or_default()
                .extend(records.into_iter().map(|r| r.track));
        }
        self.emit(Event::HistoryAppended { items, total });
    }

    pub(super) fn clear_history(&mut self) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let generation = self.list_generation;
        tokio::spawn(async move {
            let msg = match client.clear_playback_history().await {
                Ok(()) => Internal::HistoryCleared(generation),
                Err(e) => Self::failed(&client, "清空播放历史", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn history_cleared(&mut self) {
        if self.library.is_none() {
            return;
        }
        self.emit(Event::Notice("已清空播放历史".into()));
        self.query_history();
        // 接着听, 最近播放 and the spotlight depend on playback records.
        if self.home.is_some() {
            self.load_home();
        }
    }

    // ------------------------------------------------------------ daily mixes

    pub(super) fn rate_mix(&mut self, id: i64, value: i32, reasons: Vec<String>) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let generation = self.list_generation;
        tokio::spawn(async move {
            let msg = match client.rate_daily_mix(id, value, &reasons).await {
                Ok(_) => Internal::MixChanged {
                    generation,
                    id,
                    notice: None,
                    replaced_by: None,
                },
                Err(e) => mix_failure(generation, &client, &e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn replace_mix(&mut self, id: i64) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let generation = self.list_generation;
        tokio::spawn(async move {
            let msg = match client.regenerate_daily_mix(id).await {
                Ok(next) => Internal::MixChanged {
                    generation,
                    id,
                    notice: Some(format!("已换成「{}」", next.title)),
                    replaced_by: Some(next.id),
                },
                Err(e) => mix_failure(generation, &client, &e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn save_mix(&mut self, id: i64) {
        let Some((client, tx)) = self.request() else {
            return;
        };
        let generation = self.list_generation;
        tokio::spawn(async move {
            let msg = match client.save_daily_mix(id).await {
                Ok(playlist) => {
                    let _ = tx.send(Internal::PlaylistsChanged {
                        generation,
                        notice: None,
                        deleted: None,
                    });
                    Internal::MixChanged {
                        generation,
                        id,
                        notice: Some(format!("已另存为歌单「{}」", playlist.name)),
                        replaced_by: None,
                    }
                }
                Err(e) => mix_failure(generation, &client, &e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn mix_changed(
        &mut self,
        id: i64,
        notice: Option<String>,
        replaced_by: Option<i64>,
    ) {
        if self.library.is_none() {
            return;
        }
        if let Some(notice) = notice {
            self.emit(Event::Notice(notice));
        }
        if let Some(home) = self.home.as_mut() {
            home.mixes = None;
            home.mixes_date = None;
            self.load_mixes();
        }
        match replaced_by {
            Some(to) => self.emit(Event::DailyMixReplaced { from: id, to }),
            None if self.open_detail == Some(DetailKey::DailyMix(id)) => {
                self.open_detail(DetailKey::DailyMix(id));
            }
            None => {}
        }
    }

    /// 不再推荐 / recommend again the row `index` of the open mix.
    pub(super) fn block_recommendation(&mut self, index: usize, blocked: bool) {
        let Some(mix_id) = self.open_mix.as_ref().map(|m| m.id) else {
            return;
        };
        let Some(track_id) = self
            .library
            .as_ref()
            .and_then(|l| l.list(TrackList::Detail).get(index))
            .map(|t| t.id)
        else {
            return;
        };
        let Some((client, tx)) = self.request() else {
            return;
        };
        let generation = self.list_generation;
        tokio::spawn(async move {
            let msg = match client.set_recommendation_block(track_id, blocked).await {
                Ok(()) => Internal::MixBlocked {
                    generation,
                    mix_id,
                    index,
                    blocked,
                },
                Err(e) => Self::failed(&client, "操作", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn mix_blocked(&mut self, mix_id: i64, index: usize, blocked: bool) {
        let Some(mix) = self.open_mix.as_mut().filter(|m| m.id == mix_id) else {
            return;
        };
        if let Some(row) = mix.rows.get_mut(index) {
            row.blocked = blocked;
        }
        self.emit(Event::MixRowBlocked { index, blocked });
    }

    /// Playable rows of the open mix from row `start`, with their playback
    /// contexts when the server takes them (`with_context`).
    pub(super) fn mix_playable(
        &self,
        start: usize,
        with_context: bool,
    ) -> Option<(Vec<Track>, Intent)> {
        let mix = self.open_mix.as_ref()?;
        let tracks = self.library.as_ref()?.list(TrackList::Detail);
        let with_context = with_context && self.has(Feature::PlaybackContext);
        let mut out = Vec::new();
        let mut contexts = Vec::new();
        let mut first = 0;
        for (i, (track, row)) in tracks.iter().zip(&mix.rows).enumerate() {
            if row.missing {
                continue;
            }
            if i < start {
                first = out.len() + 1;
            }
            out.push(track.clone());
            contexts.push(
                (with_context && mix.id > 0)
                    .then(|| session::PlaybackContext::daily_mix(mix.id, row.position)),
            );
        }
        let first = first.min(out.len().saturating_sub(1));
        Some((out, Intent::play_at(first).with_contexts(contexts)))
    }

    // ------------------------------------------------------------ playing badges

    /// Works / series / mix of the current entry; asks the server which
    /// works the track belongs to (`trackWorks`) when not known yet.
    pub(super) fn update_playing_sources(&mut self) {
        let current = self
            .queue
            .current()
            .map(|i| (i.track.id, i.context.as_ref().map(|c| c.id)));
        let Some((track_id, mix)) = current else {
            self.emit_playing_sources(PlayingSources::default());
            return;
        };
        let known = self
            .track_works
            .as_ref()
            .is_some_and(|(id, _)| *id == track_id);
        if !known && self.has(Feature::TrackWorks) {
            self.track_works = Some((track_id, Vec::new()));
            let generation = self.list_generation;
            if let Some((client, tx)) = self.request() {
                tokio::spawn(async move {
                    match client.track_works(track_id).await {
                        Ok(works) => {
                            let _ = tx.send(Internal::TrackWorks {
                                generation,
                                track_id,
                                works,
                            });
                        }
                        Err(e) => tracing::debug!("works of track {track_id}: {e}"),
                    }
                });
            }
        }
        let works: &[TrackWork] = match &self.track_works {
            Some((id, works)) if *id == track_id => works,
            _ => &[],
        };
        let mut sources = PlayingSources {
            works: works.iter().map(|w| w.id).collect(),
            series: works.iter().filter_map(|w| w.series_id).collect(),
            mix,
        };
        for key in self.sources.containing(track_id) {
            match key {
                SourceKey::Work(id) => sources.works.push(id),
                SourceKey::Series(id) => sources.series.push(id),
            }
        }
        sources.works.sort_unstable();
        sources.works.dedup();
        sources.series.sort_unstable();
        sources.series.dedup();
        self.emit_playing_sources(sources);
    }

    fn emit_playing_sources(&mut self, sources: PlayingSources) {
        if self.playing_sources != sources {
            self.playing_sources = sources.clone();
            self.emit(Event::PlayingSources(sources));
        }
    }
}

fn mix_failure(generation: u64, client: &api::Client, e: &ApiError) -> Internal {
    if e.is_unauthorized() {
        return Internal::Unauthorized {
            api_token: client.api_token().to_owned(),
        };
    }
    Internal::Notice {
        generation,
        text: match e.status() {
            Some(429) => "今天换一个的次数已用完".into(),
            Some(409) => e.to_string(),
            _ => format!("操作失败：{e}"),
        },
    }
}
