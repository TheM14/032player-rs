//! Sonos output in the core loop (MusicBridge queue mode,
//! `AppStatePlaybackCoordinator` / `SonosDiscovery.kt`).
//!
//! The play order (or a window of it, see [`plan::window`]) goes into the
//! speaker's own queue, so the speaker advances by itself and keeps
//! playing when this app quits. A per-room actor task runs the speaker
//! commands in order and polls its state every second; the core follows
//! the speaker's position and feeds the same reporter as local playback,
//! so switching outputs keeps the listen in one session.
//!
//! `mirror` is what the speaker queue holds once every sent command has
//! run; statuses polled before the newest command finished are dropped.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::ops::Range;
use std::time::{Duration, Instant};

use api::{Client, Track, Transcode, Url};
use session::{AdapterStatus, AdapterTrack};
use sonos::{Item, PlayMode, Room, Speaker, Status, TransportState};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::{Core, Event, Internal, Library};
use crate::cast::{self as plan, Diff, Kind, QueueRef};
use crate::queue::Repeat;
use crate::settings::CastRoomSetting;

/// Speaker state poll.
const POLL: Duration = Duration::from_secs(1);
/// Between polls the position moves on by the clock.
const TICK: Duration = Duration::from_millis(250);
/// Every this many polls the volume is read too.
const VOLUME_EVERY: u32 = 10;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
/// After a push the speaker briefly still reports the old track / STOPPED.
const SETTLE: Duration = Duration::from_secs(4);
/// Polls stopped without ever playing: the speaker cannot play the URL.
const NEVER_STARTED_POLLS: u32 = 10;
/// STOPPED with the last position this close to the end: the track ended.
const END_SLACK_MS: i64 = 4_000;
const STOPPED_END_POLLS: u32 = 2;
/// Failed polls before the speaker counts as unreachable.
const LOST_POLLS: u32 = 5;
/// Polls the speaker plays something not in our queue before we let go.
const FOREIGN_POLLS: u32 = 3;
/// Waiting for the server to build the first track's FLAC.
const FLAC_BUDGET: Duration = Duration::from_secs(30);
/// Background FLAC builds of the later tracks, each.
const WARM_BUDGET: Duration = Duration::from_secs(300);
const VOLUME_DEBOUNCE: Duration = Duration::from_millis(150);
/// A polled volume does not override the user's change for this long.
const VOLUME_HOLD: Duration = Duration::from_secs(3);
const ADOPT_FETCHES: usize = 8;

/// A Sonos room for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CastRoom {
    pub id: String,
    pub name: String,
    /// `Sonos One` / `Sonos · 2 个房间`.
    pub detail: String,
}

fn room_view(room: &Room) -> CastRoom {
    let detail = if room.member_count > 1 {
        format!("Sonos · {} 个房间", room.member_count)
    } else if room.model.is_empty() {
        "Sonos".to_owned()
    } else if room.model.to_lowercase().starts_with("sonos") {
        room.model.clone()
    } else {
        format!("Sonos {}", room.model)
    };
    CastRoom {
        id: room.id.clone(),
        name: room.name.clone(),
        detail,
    }
}

/// One speaker queue entry.
#[derive(Clone, Debug)]
pub(super) struct Entry {
    uid: u64,
    kind: Kind,
    /// `offsetMs` the live entry's URL starts at.
    offset_ms: i64,
    identity: String,
}

impl AsRef<str> for Entry {
    fn as_ref(&self) -> &str {
        &self.identity
    }
}

/// The attached room.
pub(super) struct Cast {
    room: Room,
    generation: u64,
    ops: UnboundedSender<(u64, Op)>,
    /// Sequence number of the last command sent.
    sent: u64,
    mirror: Vec<Entry>,
    /// Our queue is on the speaker (`mirror` is valid).
    loaded: bool,
    /// The play mode last sent.
    mode: Option<PlayMode>,
    /// This machine's LAN address towards the speaker (loopback servers).
    lan: Option<IpAddr>,
    settle_until: Option<Instant>,
    state: Option<TransportState>,
    stopped_polls: u32,
    failed_polls: u32,
    foreign_polls: u32,
    buffering: bool,
    /// Position at a moment while playing (between-poll clock).
    anchor: Option<(Instant, i64)>,
    volume: Option<f64>,
    volume_set_at: Option<Instant>,
    volume_revision: u64,
    pending_volume: Option<f64>,
    /// The `Probe` whose answer may be adopted.
    probe_seq: Option<u64>,
    /// Sequence number of the latest `Op::Replace`.
    replace_seq: u64,
    /// Sequence number of an in-flight automatic `cast_resync()` to prevent loops.
    resync_seq: Option<u64>,
    /// A `Replace` whose first track may fall back to live MP3: (seq, uid, offset).
    flac_start: Option<(u64, u64, i64)>,
    /// The transcode plan changed: the next play builds a new speaker queue.
    replan: bool,
    /// Track duration the speaker reported while playing.
    speaker_duration_ms: i64,
}

impl Cast {
    pub(super) fn invalidate(&mut self) {
        self.replan = true;
    }
}

/// A new speaker queue.
pub(super) struct Push {
    items: Vec<Item>,
    /// Entry to select.
    start: usize,
    position_ms: i64,
    play: bool,
    flac: Option<FlacStart>,
    /// FLAC cache entries to build afterwards.
    warm: Vec<Url>,
    mode: PlayMode,
}

/// Speaker entries for part of the play order.
struct Built {
    entries: Vec<Entry>,
    items: Vec<Item>,
    warm: Vec<Url>,
}

pub(super) enum Op {
    Replace(Box<Push>),
    Insert {
        items: Vec<Item>,
        at: Option<usize>,
        warm: Vec<Url>,
    },
    Remove(usize),
    Move {
        from: usize,
        to: usize,
    },
    ReplaceItem {
        index: usize,
        item: Item,
    },
    PlayIndex {
        index: usize,
        position_ms: i64,
        /// Paused again once it plays (a live seek while paused).
        pause: bool,
    },
    Play,
    Pause,
    /// Pause sent on detach: cancels any in-progress FLAC warm-up and retries UPnP 701 while transitioning.
    DetachPause,
    Stop,
    Seek(i64),
    Mode(PlayMode),
    Volume(i32),
    /// Is the speaker playing a queue we could adopt?
    Probe,
    /// The room moved (new address).
    Room(Room),
}

/// The first track of a push is a FLAC cache entry: make sure the server
/// has built it, else play `fallback` (live MP3).
pub(super) struct FlacStart {
    warm: Url,
    fallback: Option<Item>,
    title: String,
}

/// What a failed command means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    /// Transport only (pause, seek, volume ...): the next poll shows the truth.
    Soft,
    /// Incremental speaker queue edit failed; one full resync may recover.
    Queue,
    /// Full queue replacement while paused failed; do not recurse into resync.
    Resync,
    /// Playback did not start.
    Start,
}

impl Op {
    fn failure(&self) -> Failure {
        match self {
            Self::Replace(push) if push.play => Failure::Start,
            Self::PlayIndex { .. } => Failure::Start,
            Self::Replace(_) => Failure::Resync,
            Self::Insert { .. }
            | Self::Remove(_)
            | Self::Move { .. }
            | Self::ReplaceItem { .. } => Failure::Queue,
            _ => Failure::Soft,
        }
    }

    fn cancels_replace_warmup(&self) -> bool {
        matches!(
            self,
            Self::Replace(_)
                | Self::Stop
                | Self::Pause
                | Self::DetachPause
                | Self::PlayIndex { .. }
        )
    }
}

pub(super) enum Outcome {
    Done,
    /// `Op::Replace` was aborted during FLAC warm-up before touching the speaker.
    Aborted,
    /// The first track's FLAC was not ready: it plays as live MP3 with this URI identity.
    FellBack(String),
    Probed(Option<Box<Probed>>),
}

/// What a playing speaker holds.
pub(super) struct Probed {
    uris: Vec<String>,
    status: Status,
    mode: String,
}

pub(super) enum CastMsg {
    Done {
        seq: u64,
        failure: Failure,
        result: Result<Outcome, String>,
    },
    Status {
        /// The last command run before this poll.
        done: u64,
        status: Result<Status, String>,
        volume: Option<i32>,
    },
    Tick,
    Notice(String),
    AdoptTracks {
        seq: u64,
        tracks: Result<Vec<Track>, String>,
        probed: Box<Probed>,
        refs: Vec<QueueRef>,
    },
    VolumeFlush(u64),
}

// ------------------------------------------------------------ the actor

/// Builds the FLAC cache entry of `url` within `budget`, retrying while
/// the server's encoders are busy (MusicBridge `Music032Warmup`).
async fn warm_flac(client: &Client, url: &Url, budget: Duration) -> bool {
    let work = async {
        if client.media_cached(url.clone()).await.unwrap_or(false) {
            return true;
        }
        loop {
            match client.warm_media(url.clone()).await {
                Ok((200 | 206, _)) => return true,
                Ok((503, retry)) => {
                    let secs = retry.unwrap_or(2).clamp(1, 30);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
                Ok((status, _)) => {
                    tracing::debug!(%url, status, "flac warm-up refused");
                    return false;
                }
                Err(e) => {
                    tracing::debug!(%url, "flac warm-up: {e}");
                    return false;
                }
            }
        }
    };
    tokio::time::timeout(budget, work).await.unwrap_or(false)
}

/// Builds the later queued FLAC entries one after another.
fn spawn_warm(client: Client, urls: Vec<Url>) -> Option<JoinHandle<()>> {
    if urls.is_empty() {
        return None;
    }
    Some(tokio::spawn(async move {
        for url in urls {
            warm_flac(&client, &url, WARM_BUDGET).await;
        }
    }))
}

struct Actor {
    speaker: Speaker,
    client: Client,
    tx: UnboundedSender<Internal>,
    generation: u64,
    warming: Vec<JoinHandle<()>>,
}

impl Actor {
    fn send(&self, msg: CastMsg) -> bool {
        self.tx.send(Internal::Cast(self.generation, msg)).is_ok()
    }

    async fn replace(
        &mut self,
        push: Push,
        rx: &mut UnboundedReceiver<(u64, Op)>,
        pending: &mut VecDeque<(u64, Op)>,
    ) -> Result<Outcome, String> {
        let Push {
            mut items,
            start,
            mut position_ms,
            play,
            flac,
            warm,
            mode,
        } = push;
        for task in self.warming.drain(..) {
            task.abort();
        }
        // Check if a superseding/cancel op is already queued before starting work.
        while let Ok(next) = rx.try_recv() {
            pending.push_back(next);
        }
        if pending.iter().any(|(_, op)| op.cancels_replace_warmup()) {
            return Ok(Outcome::Aborted);
        }
        let mut outcome = Outcome::Done;
        if let Some(flac) = flac {
            let client = self.client.clone();
            let warm_url = flac.warm.clone();
            let title = flac.title.clone();
            let tx = self.tx.clone();
            let generation = self.generation;
            let warm_fut = async move {
                if client.media_cached(warm_url.clone()).await.unwrap_or(false) {
                    return true;
                }
                let _ = tx.send(Internal::Cast(
                    generation,
                    CastMsg::Notice(format!("服务器正在准备 FLAC：{title}")),
                ));
                warm_flac(&client, &warm_url, FLAC_BUDGET).await
            };
            tokio::pin!(warm_fut);
            let flac_ready = loop {
                tokio::select! {
                    biased;
                    msg = rx.recv() => {
                        match msg {
                            None => return Ok(Outcome::Aborted),
                            Some(next) => {
                                let cancel = next.1.cancels_replace_warmup();
                                pending.push_back(next);
                                if cancel {
                                    return Ok(Outcome::Aborted);
                                }
                            }
                        }
                    }
                    ready = &mut warm_fut => break ready,
                }
            };
            if !flac_ready {
                let Some(fallback) = flac.fallback else {
                    return Err("FLAC 准备失败，服务器也未启用 MP3 转码，无法在 Sonos 播放".into());
                };
                let identity = plan::uri_identity(&fallback.uri);
                if let Some(item) = items.get_mut(start) {
                    *item = fallback;
                }
                // The live stream starts at the offset itself.
                position_ms = 0;
                outcome = Outcome::FellBack(identity);
            }
        }
        self.speaker
            .replace(&items, start, position_ms, play)
            .await
            .map_err(|e| e.to_string())?;
        if !items.is_empty()
            && let Err(e) = self.speaker.set_play_mode(mode).await
        {
            tracing::debug!("sonos play mode: {e}");
        }
        if let Some(task) = spawn_warm(self.client.clone(), warm) {
            self.warming.push(task);
        }
        Ok(outcome)
    }

    async fn probe(&self) -> Result<Outcome, String> {
        let s = &self.speaker;
        let status = s.status().await.map_err(|e| e.to_string())?;
        let active = matches!(
            status.state,
            TransportState::Playing | TransportState::Paused | TransportState::Transitioning
        );
        if !active || !status.queue_active {
            return Ok(Outcome::Probed(None));
        }
        let uris = s
            .queue(plan::MAX_QUEUE + 1)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|e| e.uri)
            .collect();
        let mode = s.play_mode().await.unwrap_or_default();
        Ok(Outcome::Probed(Some(Box::new(Probed {
            uris,
            status,
            mode,
        }))))
    }

    async fn run(
        &mut self,
        op: Op,
        rx: &mut UnboundedReceiver<(u64, Op)>,
        pending: &mut VecDeque<(u64, Op)>,
    ) -> Result<Outcome, String> {
        let s = &self.speaker;
        let r = match op {
            Op::Replace(push) => return self.replace(*push, rx, pending).await,
            Op::Probe => return self.probe().await,
            Op::Insert { items, at, warm } => {
                let r = s.insert(&items, at).await;
                if let Some(task) = spawn_warm(self.client.clone(), warm) {
                    self.warming.push(task);
                }
                r
            }
            Op::Remove(index) => s.remove(index).await,
            Op::Move { from, to } => s.move_item(from, to).await,
            Op::ReplaceItem { index, item } => s.replace_item(index, &item).await,
            Op::PlayIndex {
                index,
                position_ms,
                pause,
            } => {
                let r = s.play_index(index, position_ms).await;
                if r.is_ok() && pause {
                    // 701 until the new stream plays.
                    for _ in 0..10 {
                        if s.pause().await.is_ok() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                }
                r
            }
            Op::Play => s.play().await,
            Op::Pause => s.pause().await,
            Op::DetachPause => {
                let mut res = s.pause().await;
                if let Err(ref e) = res
                    && e.to_string().contains(" 701")
                {
                    for _ in 0..8 {
                        let transitioning = matches!(
                            s.status().await.map(|st| st.state),
                            Ok(TransportState::Transitioning | TransportState::Playing)
                        );
                        if !transitioning {
                            res = Ok(());
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        res = s.pause().await;
                        if res.is_ok() {
                            break;
                        }
                    }
                }
                res
            }
            Op::Stop => s.stop().await,
            Op::Seek(ms) => s.seek(ms).await,
            Op::Mode(mode) => s.set_play_mode(mode).await,
            Op::Volume(v) => s.set_volume(v).await,
            Op::Room(room) => {
                self.speaker = Speaker::new(sonos::http_client(), &room);
                Ok(())
            }
        };
        r.map(|()| Outcome::Done).map_err(|e| e.to_string())
    }

    async fn poll(&self, polls: u32, done: u64) -> bool {
        let volume = if polls % VOLUME_EVERY == 1 {
            self.speaker.volume().await.ok()
        } else {
            None
        };
        let status = self.speaker.status().await.map_err(|e| e.to_string());
        self.send(CastMsg::Status {
            done,
            status,
            volume,
        })
    }
}

/// Runs the room's commands in order and polls between them. Ends when
/// the core drops the room (after the commands still queued).
async fn actor(mut actor: Actor, mut rx: UnboundedReceiver<(u64, Op)>) {
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut pending: VecDeque<(u64, Op)> = VecDeque::new();
    let mut done = 0;
    let mut polls = 0u32;
    loop {
        let next = if let Some(item) = pending.pop_front() {
            Some(item)
        } else {
            tokio::select! {
                biased;
                msg = rx.recv() => msg,
                _ = poll.tick() => {
                    polls += 1;
                    if !actor.poll(polls, done).await {
                        break;
                    }
                    continue;
                }
                _ = tick.tick() => {
                    if !actor.send(CastMsg::Tick) {
                        break;
                    }
                    continue;
                }
            }
        };
        let Some((seq, op)) = next else { break };
        let failure = op.failure();
        let result = actor.run(op, &mut rx, &mut pending).await;
        done = seq;
        if !actor.send(CastMsg::Done {
            seq,
            failure,
            result,
        }) {
            break;
        }
        if pending.is_empty() && rx.is_empty() {
            polls += 1;
            if !actor.poll(polls, done).await {
                break;
            }
            poll.reset();
        }
    }
    for task in actor.warming {
        task.abort();
    }
}

// ------------------------------------------------------------ the core side

fn playback_status(paused: bool) -> AdapterStatus {
    if paused {
        AdapterStatus::Paused
    } else {
        AdapterStatus::Playing
    }
}

fn repeat_from_sonos(mode: &str) -> Option<Repeat> {
    match mode.to_ascii_uppercase().as_str() {
        "NORMAL" | "SHUFFLE_NOREPEAT" => Some(Repeat::Off),
        "REPEAT_ALL" | "SHUFFLE" => Some(Repeat::All),
        "REPEAT_ONE" | "SHUFFLE_REPEAT_ONE" => Some(Repeat::One),
        _ => None,
    }
}

/// Non-shuffle play mode reported by Sonos; any hardware `SHUFFLE*` mode
/// returns `None` so `cast_play_mode` explicitly clears shuffle after adopt.
fn play_mode_from_sonos(mode: &str) -> Option<PlayMode> {
    match mode.to_ascii_uppercase().as_str() {
        "NORMAL" => Some(PlayMode::Normal),
        "REPEAT_ALL" => Some(PlayMode::RepeatAll),
        "REPEAT_ONE" => Some(PlayMode::RepeatOne),
        _ => None,
    }
}

/// This machine's address towards the room, when the server is on loopback
/// (the speaker must fetch from a LAN address).
fn lan_towards(room: &Room, library: Option<&Library>) -> Option<IpAddr> {
    let base = library?.client.base_url();
    if !plan::is_loopback_server(base) {
        return None;
    }
    sonos::local_address_towards(&room.host)
}

impl Core {
    pub(super) fn casting(&self) -> bool {
        self.cast.is_some()
    }

    fn emit_cast_rooms(&self, error: Option<String>) {
        self.emit(Event::CastRooms {
            searching: self.cast_discovering,
            rooms: self.cast_rooms.iter().map(room_view).collect(),
            error,
        });
    }

    /// Looks for rooms ([`Event::CastRooms`]).
    pub(super) fn discover_cast(&mut self) {
        if self.cast_discovering {
            return;
        }
        self.cast_discovering = true;
        self.emit_cast_rooms(None);
        let hosts = plan::parse_hosts(&self.settings.cast.hosts);
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let result = sonos::discover(&sonos::http_client(), DISCOVERY_TIMEOUT, &hosts)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(Internal::CastRooms(result));
        });
    }

    pub(super) fn cast_rooms_found(&mut self, result: Result<Vec<Room>, String>) {
        self.cast_discovering = false;
        let error = match result {
            Ok(rooms) => {
                self.cast_rooms = rooms;
                None
            }
            Err(e) => Some(e),
        };
        // The attached room may have a new address.
        let moved = self.cast.as_ref().and_then(|c| {
            self.cast_rooms
                .iter()
                .find(|r| {
                    r.id == c.room.id && (r.base_url != c.room.base_url || r.name != c.room.name)
                })
                .cloned()
        });
        if let Some(room) = moved {
            self.cast_send(Op::Room(room.clone()));
            let lan = lan_towards(&room, self.library.as_ref());
            if let Some(cast) = self.cast.as_mut() {
                cast.lan = lan;
                cast.room = room.clone();
            }
            self.save_cast_room(Some(&room));
            self.emit(Event::CastOutput(Some(room_view(&room))));
        }
        self.emit_cast_rooms(error);
    }

    /// `Some(room id)`: play on that Sonos room; `None`: this computer.
    pub(super) fn set_cast_output(&mut self, id: Option<String>) {
        match id {
            None => self.detach_cast(true),
            Some(id) => {
                let room = self.cast_rooms.iter().find(|r| r.id == id).cloned();
                match room {
                    Some(room) => self.attach_cast(room, true),
                    None => self.emit(Event::Notice("找不到这个 Sonos 房间，请重新搜索".into())),
                }
            }
        }
    }

    /// After sign-in: back to the room chosen last time (adopting what it
    /// plays when that is our queue), and refresh the room list.
    pub(super) fn restore_cast(&mut self) {
        let Some(saved) = self.settings.cast.room.clone() else {
            self.emit(Event::CastOutput(None));
            return;
        };
        let room = Room {
            host: Url::parse(&saved.base_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default(),
            id: saved.id,
            name: saved.name,
            base_url: saved.base_url,
            member_count: 1,
            model: String::new(),
        };
        self.attach_cast(room, false);
        self.discover_cast();
    }

    fn save_cast_room(&mut self, room: Option<&Room>) {
        let room = room.map(|r| CastRoomSetting {
            id: r.id.clone(),
            name: r.name.clone(),
            base_url: r.base_url.clone(),
        });
        self.update_settings(|s| s.cast.room = room);
    }

    /// Moves playback (or just the output, when stopped) to `room`. The
    /// current listen continues there, at the same position.
    fn attach_cast(&mut self, room: Room, announce: bool) {
        let Some(client) = self.library.as_ref().map(|l| l.client.clone()) else {
            return;
        };
        if self.cast.as_ref().is_some_and(|c| c.room.id == room.id) {
            return;
        }
        let was_idle = self.idle;
        let playing = !self.idle && !self.effective_paused();
        let position = self.position_ms;
        if self.cast.is_some() {
            self.drop_cast(true);
        } else if !self.idle {
            self.restore_gain();
            let _ = self.engine.stop();
        }
        self.hand_over(was_idle, position);

        self.cast_generation += 1;
        let generation = self.cast_generation;
        let (ops, rx) = mpsc::unbounded_channel();
        let worker = Actor {
            speaker: Speaker::new(sonos::http_client(), &room),
            client,
            tx: self.internal.clone(),
            generation,
            warming: Vec::new(),
        };
        tokio::spawn(actor(worker, rx));
        self.cast = Some(Cast {
            lan: lan_towards(&room, self.library.as_ref()),
            room: room.clone(),
            generation,
            ops,
            sent: 0,
            mirror: Vec::new(),
            loaded: false,
            mode: None,
            settle_until: None,
            state: None,
            stopped_polls: 0,
            failed_polls: 0,
            foreign_polls: 0,
            buffering: false,
            anchor: None,
            volume: None,
            volume_set_at: None,
            volume_revision: 0,
            pending_volume: None,
            probe_seq: None,
            replace_seq: 0,
            resync_seq: None,
            flac_start: None,
            replan: false,
            speaker_duration_ms: 0,
        });
        self.save_cast_room(Some(&room));
        self.emit(Event::CastOutput(Some(room_view(&room))));
        if playing {
            self.play_current(false);
        } else if was_idle {
            // The speaker may already play a queue of ours (pushed before
            // this app was restarted, or by the web player).
            let seq = self.cast_send(Op::Probe);
            if let Some(cast) = self.cast.as_mut() {
                cast.probe_seq = Some(seq);
            }
        }
        if announce {
            self.emit(Event::Notice(format!("已切换到 Sonos「{}」", room.name)));
        }
    }

    /// Back to this computer; a playing listen continues here.
    pub(super) fn detach_cast(&mut self, announce: bool) {
        if self.cast.is_none() {
            return;
        }
        let was_idle = self.idle;
        let playing = !self.idle && !self.paused;
        let position = self.position_ms;
        self.drop_cast(true);
        self.hand_over(was_idle, position);
        self.save_cast_room(None);
        self.emit(Event::CastOutput(None));
        self.emit(Event::Volume(self.settings.volume));
        if playing {
            self.play_current(false);
        }
        if announce {
            self.emit(Event::Notice("已切换回本机播放".into()));
        }
    }

    /// Stops talking to the room (pausing it when it plays our queue).
    pub(super) fn drop_cast(&mut self, pause: bool) {
        let Some(cast) = self.cast.take() else { return };
        if pause && cast.loaded && !self.idle && !self.paused {
            let _ = cast.ops.send((cast.sent + 1, Op::DetachPause));
        }
    }

    /// The output changed under the current entry: nothing renders it now,
    /// and the next play resumes it at `position` without ending the
    /// reported listen (no new play is counted).
    pub(super) fn hand_over(&mut self, was_idle: bool, position: i64) {
        self.restore_gain();
        self.clear_buffering();
        self.idle = true;
        self.loading = false;
        self.paused = true;
        self.preloaded = None;
        self.current_transcode = None;
        self.preloaded_transcode = None;
        self.expected_path = None;
        self.pending_seek_ms = None;
        self.report_seek_ms = None;
        self.stream_offset_ms = 0;
        self.started_uid = None;
        if !was_idle && let Some(item) = self.queue.current() {
            self.resume_at = Some((item.uid, position));
        }
        self.emit(Event::Paused(true));
    }

    fn cast_send(&mut self, op: Op) -> u64 {
        let Some(cast) = self.cast.as_mut() else {
            return 0;
        };
        if !matches!(op, Op::Probe | Op::Room(_) | Op::Volume(_)) {
            cast.probe_seq = None;
        }
        cast.sent += 1;
        let _ = cast.ops.send((cast.sent, op));
        cast.sent
    }

    /// The play order and the part of it for the speaker queue.
    fn cast_window(&self) -> (Vec<u64>, Range<usize>) {
        let order: Vec<u64> = self.queue.in_play_order().map(|i| i.uid).collect();
        let current = self.queue.position().unwrap_or(0);
        let keep = self
            .cast
            .as_ref()
            .filter(|c| c.loaded)
            .and_then(|c| c.mirror.first())
            .and_then(|first| order.iter().position(|u| *u == first.uid));
        let range = plan::window(order.len(), current, keep);
        (order, range)
    }

    /// What the speaker gets for `track`.
    fn cast_item(&self, track: &Track, kind: Kind, offset_ms: i64) -> Option<Item> {
        let client = &self.library.as_ref()?.client;
        let lan = self.cast.as_ref().and_then(|c| c.lan);
        let url = match kind {
            Kind::Original => client.stream_url(track.id),
            Kind::LiveMp3 => client.transcode_url(track.id, Transcode::Mp3(320), offset_ms),
            Kind::StaticFlac => client.transcode_url(track.id, Transcode::Flac48, 0),
        };
        Some(Item {
            uri: plan::speaker_url(url, lan),
            mime: plan::mime(track, kind),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            art_uri: client
                .image_url(&track.artwork_url, Some(512))
                .map(|u| plan::speaker_url(u, lan))
                .unwrap_or_default(),
            duration_ms: track.duration_millis,
            id: format!("032-track-{}", track.id),
        })
    }

    /// Plans and builds the speaker entries of play-order positions `range`.
    /// `start` (an index into `range`) is an error when it cannot play;
    /// other entries fall back to the original file. Also returns the FLAC
    /// cache entries to build, in play order after `start`.
    fn cast_entries(
        &self,
        range: Range<usize>,
        start: Option<usize>,
        offset_ms: i64,
    ) -> Result<Built, String> {
        let library = self.library.as_ref().ok_or("未登录")?;
        let mut entries = Vec::with_capacity(range.len());
        let mut items = Vec::with_capacity(range.len());
        let mut before = Vec::new();
        let mut after = Vec::new();
        for (k, pos) in range.enumerate() {
            let item = self.queue.at(pos).ok_or("队列已变化")?;
            let kind = match plan::plan(&item.track, &library.capabilities, &self.settings.cast) {
                Ok(kind) => kind,
                Err(e) if Some(k) == start => return Err(e),
                Err(_) => Kind::Original,
            };
            let offset = if Some(k) == start && kind == Kind::LiveMp3 {
                offset_ms
            } else {
                0
            };
            let speaker_item = self.cast_item(&item.track, kind, offset).ok_or("未登录")?;
            if kind == Kind::StaticFlac && Some(k) != start {
                let url = library
                    .client
                    .transcode_url(item.track.id, Transcode::Flac48, 0);
                if start.is_some_and(|s| k < s) {
                    before.push(url);
                } else {
                    after.push(url);
                }
            }
            entries.push(Entry {
                uid: item.uid,
                kind,
                offset_ms: offset,
                identity: plan::uri_identity(&speaker_item.uri),
            });
            items.push(speaker_item);
        }
        after.extend(before);
        Ok(Built {
            entries,
            items,
            warm: after,
        })
    }

    /// Replaces the speaker queue with the window `range` of the play order
    /// and selects `start` (index into `range`) at `position_ms`. Returns the
    /// live stream offset of the started entry.
    fn cast_push(
        &mut self,
        range: Range<usize>,
        start: usize,
        position_ms: i64,
        play: bool,
    ) -> Result<i64, String> {
        let whole = range.len() == self.queue.len();
        let first_pos = range.start + start;
        let Built {
            entries,
            items,
            warm,
        } = self.cast_entries(range, Some(start), position_ms)?;
        let first = entries.get(start).cloned().ok_or("队列为空")?;
        let library = self.library.as_ref().ok_or("未登录")?;
        let track = self
            .queue
            .at(first_pos)
            .map(|i| i.track.clone())
            .ok_or("队列已变化")?;
        let flac = (first.kind == Kind::StaticFlac).then(|| FlacStart {
            warm: library.client.transcode_url(track.id, Transcode::Flac48, 0),
            fallback: if library.capabilities.can_transcode("mp3") {
                self.cast_item(&track, Kind::LiveMp3, position_ms)
            } else {
                None
            },
            title: track.title.clone(),
        });
        let live = first.kind == Kind::LiveMp3;
        let mode = self.cast_desired_mode(whole);
        let seq = self.cast_send(Op::Replace(Box::new(Push {
            items,
            start,
            position_ms: if live { 0 } else { position_ms },
            play,
            flac,
            warm,
            mode,
        })));
        let Some(cast) = self.cast.as_mut() else {
            return Err("未连接 Sonos".into());
        };
        cast.mirror = entries;
        cast.loaded = true;
        cast.replace_seq = seq;
        cast.resync_seq = None;
        cast.replan = false;
        cast.buffering = true;
        cast.mode = Some(mode);
        cast.flac_start = (first.kind == Kind::StaticFlac).then_some((seq, first.uid, position_ms));
        cast.settle_until = Some(Instant::now() + SETTLE);
        cast.stopped_polls = 0;
        cast.anchor = None;
        cast.speaker_duration_ms = 0;
        Ok(if live { position_ms } else { 0 })
    }

    /// Plays the current entry on the speaker from `resume_ms`: a jump when
    /// the speaker holds the play order, else a new queue. Returns the live
    /// stream offset.
    pub(super) fn cast_start(&mut self, resume_ms: i64) -> Result<i64, String> {
        let (order, range) = self.cast_window();
        let current = self.queue.position().ok_or("队列为空")?;
        let start = current.checked_sub(range.start).ok_or("队列已变化")?;
        let cast = self.cast.as_ref().ok_or("未连接 Sonos")?;
        let mirrored = cast.loaded
            && !cast.replan
            && cast.mirror.len() == range.len()
            && cast
                .mirror
                .iter()
                .zip(&order[range.clone()])
                .all(|(e, uid)| e.uid == *uid);
        if !mirrored {
            return self.cast_push(range, start, resume_ms, true);
        }
        let entry = cast.mirror[start].clone();
        self.cast_jump(start, &entry, resume_ms, false)
    }

    /// Plays mirror entry `index` (the current entry) from `position_ms`;
    /// a live entry is re-opened there. `pause` pauses it again once
    /// playing. Returns the live stream offset.
    fn cast_jump(
        &mut self,
        index: usize,
        entry: &Entry,
        position_ms: i64,
        pause: bool,
    ) -> Result<i64, String> {
        let mut offset = 0;
        if entry.kind == Kind::LiveMp3 {
            if position_ms > 0 || entry.offset_ms != 0 {
                let track = self
                    .queue
                    .current()
                    .map(|i| i.track.clone())
                    .ok_or("队列为空")?;
                let item = self
                    .cast_item(&track, Kind::LiveMp3, position_ms)
                    .ok_or("未登录")?;
                self.cast_send(Op::ReplaceItem { index, item });
                if let Some(e) = self.cast.as_mut().and_then(|c| c.mirror.get_mut(index)) {
                    e.offset_ms = position_ms;
                }
            }
            offset = position_ms;
            self.cast_send(Op::PlayIndex {
                index,
                position_ms: 0,
                pause,
            });
        } else {
            self.cast_send(Op::PlayIndex {
                index,
                position_ms,
                pause,
            });
        }
        if let Some(cast) = self.cast.as_mut() {
            cast.settle_until = Some(Instant::now() + SETTLE);
            cast.stopped_polls = 0;
            cast.anchor = None;
            cast.buffering = true;
            cast.speaker_duration_ms = 0;
        }
        Ok(offset)
    }

    /// Live stream offset of the current entry on the speaker.
    pub(super) fn cast_offset(&self) -> i64 {
        self.cast_current().map_or(0, |(_, e)| e.offset_ms)
    }

    /// Mirror index of the current entry.
    fn cast_current(&self) -> Option<(usize, Entry)> {
        let uid = self.queue.current()?.uid;
        let cast = self.cast.as_ref().filter(|c| c.loaded)?;
        let index = cast.mirror.iter().position(|e| e.uid == uid)?;
        Some((index, cast.mirror[index].clone()))
    }

    pub(super) fn cast_seek(&mut self, position_ms: i64) -> Result<(), String> {
        let (index, entry) = self.cast_current().ok_or("Sonos 队列已变化")?;
        if entry.kind == Kind::LiveMp3 {
            // A live MP3 cannot seek: re-open it at the position.
            let paused = self.paused;
            self.stream_offset_ms = self.cast_jump(index, &entry, position_ms, paused)?;
        } else {
            self.cast_send(Op::Seek(position_ms));
        }
        if let Some(cast) = self.cast.as_mut() {
            cast.anchor = (!self.paused).then(|| (Instant::now(), position_ms));
        }
        Ok(())
    }

    /// Pause / resume on the speaker. Resuming a live MP3 (its HTTP stream
    /// may be gone) or a stopped speaker re-opens the entry at the position.
    pub(super) fn cast_pause(&mut self, paused: bool) -> Result<(), String> {
        if paused {
            self.cast_send(Op::Pause);
            if let Some(cast) = self.cast.as_mut() {
                cast.anchor = None;
            }
        } else {
            let Some((index, entry)) = self.cast_current() else {
                let offset = self.cast_start(self.position_ms)?;
                self.stream_offset_ms = offset;
                self.loading = true;
                self.apply_paused(false);
                return Ok(());
            };
            let stopped = self
                .cast
                .as_ref()
                .is_some_and(|c| c.state == Some(TransportState::Stopped));
            if entry.kind == Kind::LiveMp3 || stopped {
                let position = self.position_ms;
                let offset = self.cast_jump(index, &entry, position, false)?;
                if entry.kind == Kind::LiveMp3 {
                    self.stream_offset_ms = offset;
                }
            } else {
                self.cast_send(Op::Play);
                let position = self.position_ms;
                if let Some(cast) = self.cast.as_mut() {
                    cast.anchor = Some((Instant::now(), position));
                }
            }
        }
        self.apply_paused(paused);
        Ok(())
    }

    pub(super) fn cast_stop(&mut self) {
        if self.cast.as_ref().is_some_and(|c| c.loaded) {
            self.cast_send(Op::Stop);
        }
        if let Some(cast) = self.cast.as_mut() {
            cast.anchor = None;
            cast.buffering = false;
        }
    }

    fn cast_desired_mode(&self, whole: bool) -> PlayMode {
        if self.pause_at_end() {
            PlayMode::Normal
        } else {
            plan::play_mode(self.settings.repeat, whole)
        }
    }

    /// Keeps the speaker's play mode in line with repeat (and the window).
    pub(super) fn cast_play_mode(&mut self) {
        let Some(cast) = self.cast.as_ref().filter(|c| c.loaded) else {
            return;
        };
        let whole = cast.mirror.len() == self.queue.len();
        let mode = self.cast_desired_mode(whole);
        if cast.mode != Some(mode) {
            self.cast_send(Op::Mode(mode));
            if let Some(cast) = self.cast.as_mut() {
                cast.mode = Some(mode);
            }
        }
    }

    /// The queue changed: apply the same edit to the speaker queue.
    pub(super) fn cast_queue_changed(&mut self) {
        if !self.cast.as_ref().is_some_and(|c| c.loaded) {
            return;
        }
        let (order, range) = self.cast_window();
        let desired = &order[range.clone()];
        let old: Vec<u64> = self
            .cast
            .as_ref()
            .map(|c| c.mirror.iter().map(|e| e.uid).collect())
            .unwrap_or_default();
        match plan::diff(&old, desired) {
            Diff::Equal => {}
            Diff::Insert { at, count } => {
                let positions = range.start + at..range.start + at + count;
                match self.cast_entries(positions, None, 0) {
                    Ok(Built {
                        entries,
                        items,
                        warm,
                    }) => {
                        let at_index = (at < old.len()).then_some(at);
                        self.cast_send(Op::Insert {
                            items,
                            at: at_index,
                            warm,
                        });
                        if let Some(cast) = self.cast.as_mut() {
                            cast.mirror.splice(at..at, entries);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("sonos insert: {e}");
                        if let Some(cast) = self.cast.as_mut() {
                            cast.loaded = false;
                        }
                    }
                }
            }
            Diff::Remove(index) => {
                self.cast_send(Op::Remove(index));
                if let Some(cast) = self.cast.as_mut() {
                    cast.mirror.remove(index);
                }
            }
            Diff::Move { from, to } => {
                self.cast_send(Op::Move { from, to });
                if let Some(cast) = self.cast.as_mut() {
                    let entry = cast.mirror.remove(from);
                    cast.mirror.insert(to, entry);
                }
            }
            Diff::Replace => {
                if desired.is_empty() {
                    self.cast_send(Op::Replace(Box::new(Push {
                        items: Vec::new(),
                        start: 0,
                        position_ms: 0,
                        play: false,
                        flac: None,
                        warm: Vec::new(),
                        mode: PlayMode::Normal,
                    })));
                    if let Some(cast) = self.cast.as_mut() {
                        cast.mirror.clear();
                    }
                } else if self.idle {
                    // Pushed again by the next play.
                    if let Some(cast) = self.cast.as_mut() {
                        cast.loaded = false;
                    }
                } else {
                    // Shuffle toggled, a new window...: keep playing.
                    self.cast_resync();
                }
            }
        }
        self.cast_play_mode();
    }

    /// Replaces the speaker queue with ours at the current position.
    fn cast_resync(&mut self) {
        let (_, range) = self.cast_window();
        let Some(current) = self.queue.position() else {
            return;
        };
        let start = current.saturating_sub(range.start);
        let play = !self.paused;
        match self.cast_push(range, start, self.position_ms, play) {
            Ok(offset) => {
                self.stream_offset_ms = offset;
                if let Some(cast) = self.cast.as_mut() {
                    cast.resync_seq = Some(cast.sent);
                }
            }
            Err(e) => self.emit(Event::Notice(format!("同步 Sonos 队列失败：{e}"))),
        }
    }

    pub(super) fn cast_volume(&mut self, volume: f64) {
        let volume = volume.clamp(0.0, 100.0);
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        cast.volume = Some(volume);
        cast.pending_volume = Some(volume);
        cast.volume_set_at = Some(Instant::now());
        cast.volume_revision += 1;
        let (generation, revision) = (cast.generation, cast.volume_revision);
        let tx = self.internal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(VOLUME_DEBOUNCE).await;
            let _ = tx.send(Internal::Cast(generation, CastMsg::VolumeFlush(revision)));
        });
        self.emit(Event::Volume(volume));
    }

    // ------------------------------------------------------------ messages

    pub(super) fn cast_msg(&mut self, generation: u64, msg: CastMsg) {
        if self
            .cast
            .as_ref()
            .is_none_or(|c| c.generation != generation)
        {
            return;
        }
        match msg {
            CastMsg::Done {
                seq,
                failure,
                result,
            } => self.cast_done(seq, failure, result),
            CastMsg::Status {
                done,
                status,
                volume,
            } => self.cast_status(done, status, volume),
            CastMsg::Tick => self.cast_tick(),
            CastMsg::Notice(text) => self.emit(Event::Notice(text)),
            CastMsg::AdoptTracks {
                seq,
                tracks,
                probed,
                refs,
            } => match tracks {
                Ok(tracks) => {
                    if self.adoptable(seq) {
                        self.cast_adopt(&probed, &refs, Some(tracks), 0);
                    }
                }
                Err(e) => tracing::warn!("sonos queue tracks: {e}"),
            },
            CastMsg::VolumeFlush(revision) => {
                let Some(cast) = self.cast.as_mut() else {
                    return;
                };
                if cast.volume_revision != revision {
                    return;
                }
                if let Some(volume) = cast.pending_volume.take() {
                    #[allow(clippy::cast_possible_truncation)]
                    self.cast_send(Op::Volume(volume.round() as i32));
                }
            }
        }
    }

    fn adoptable(&self, seq: u64) -> bool {
        self.idle && self.cast.as_ref().is_some_and(|c| c.probe_seq == Some(seq))
    }

    fn cast_done(&mut self, seq: u64, failure: Failure, result: Result<Outcome, String>) {
        match result {
            Ok(Outcome::Done) => {
                if let Some(cast) = self.cast.as_mut() {
                    if cast.resync_seq.is_some_and(|r| seq >= r) {
                        cast.resync_seq = None;
                    }
                    if matches!(failure, Failure::Start | Failure::Resync) {
                        cast.settle_until = Some(Instant::now() + SETTLE);
                    }
                }
            }
            Ok(Outcome::Aborted) => {
                if let Some(cast) = self.cast.as_mut() {
                    if cast.replace_seq == seq {
                        cast.loaded = false;
                    }
                    if cast.flac_start.is_some_and(|(s, _, _)| s == seq) {
                        cast.flac_start = None;
                    }
                    if cast.resync_seq == Some(seq) {
                        cast.resync_seq = None;
                    }
                }
            }
            Ok(Outcome::FellBack(identity)) => {
                let Some(cast) = self.cast.as_mut() else {
                    return;
                };
                if cast.resync_seq.is_some_and(|r| seq >= r) {
                    cast.resync_seq = None;
                }
                if matches!(failure, Failure::Start | Failure::Resync) {
                    cast.settle_until = Some(Instant::now() + SETTLE);
                }
                let Some((s, uid, offset)) = cast.flac_start.take() else {
                    return;
                };
                if s != seq {
                    return;
                }
                if let Some(e) = cast.mirror.iter_mut().find(|e| e.uid == uid) {
                    e.kind = Kind::LiveMp3;
                    e.offset_ms = offset;
                    e.identity = identity;
                }
                if self.queue.current().is_some_and(|i| i.uid == uid) {
                    self.stream_offset_ms = offset;
                }
                self.emit(Event::Notice("FLAC 准备失败，已改用 MP3".into()));
            }
            Ok(Outcome::Probed(probed)) => {
                let Some(probed) = probed else { return };
                if self.adoptable(seq) {
                    self.probed(seq, probed);
                }
            }
            Err(e) => {
                tracing::warn!("sonos command failed: {e}");
                let Some(cast) = self.cast.as_mut() else {
                    return;
                };
                if cast.probe_seq == Some(seq) {
                    cast.probe_seq = None;
                    return;
                }
                let name = cast.room.name.clone();
                match failure {
                    // 701: not possible in the speaker's current state (pause
                    // while loading ...); the next poll corrects the state.
                    Failure::Soft if e.contains(" 701") => {}
                    Failure::Soft => self.emit(Event::Notice(format!("Sonos「{name}」：{e}"))),
                    Failure::Start => {
                        // The speaker queue is unknown now: the next play pushes again.
                        cast.loaded = false;
                        cast.resync_seq = None;
                        if self.idle {
                            self.emit(Event::Notice(format!("Sonos「{name}」：{e}")));
                        } else {
                            self.playback_failed(format!("Sonos「{name}」：{e}"));
                        }
                    }
                    Failure::Resync => {
                        // Full paused queue replacement failed: stay unloaded until next play.
                        cast.loaded = false;
                        if cast.resync_seq == Some(seq) {
                            cast.resync_seq = None;
                        }
                        self.emit(Event::Notice(format!("同步 Sonos 队列失败：{e}")));
                    }
                    Failure::Queue => {
                        cast.loaded = false;
                        let should_resync = !self.idle && cast.resync_seq.is_none_or(|r| seq > r);
                        self.emit(Event::Notice(format!("同步 Sonos 队列失败：{e}")));
                        if should_resync {
                            // Push the whole queue again once, where playback is.
                            self.cast_resync();
                        }
                    }
                }
            }
        }
    }

    /// The speaker plays a queue: adopt it when every entry is ours.
    fn probed(&mut self, seq: u64, probed: Box<Probed>) {
        let Some(library) = &self.library else { return };
        let base = library.client.base_url().to_owned();
        let token = library.client.media_token().to_owned();
        let refs: Option<Vec<QueueRef>> = probed
            .uris
            .iter()
            .map(|u| plan::parse_queue_uri(u, &base, &token))
            .collect();
        let Some(refs) = refs.filter(|r| !r.is_empty() && r.len() <= plan::MAX_QUEUE) else {
            return;
        };
        let known: Vec<i64> = self.queue.in_play_order().map(|i| i.track.id).collect();
        let ids: Vec<i64> = refs.iter().map(|r| r.track_id).collect();
        // Match either the full local queue or a contiguous window of a >2000-track queue
        // so restart adoption never truncates the local queue or refetches known tracks.
        if let Some(window_start) = known.windows(ids.len()).position(|w| w == ids.as_slice()) {
            self.cast_adopt(&probed, &refs, None, window_start);
            return;
        }
        let mut local_tracks = std::collections::HashMap::new();
        for item in self.queue.in_play_order() {
            local_tracks
                .entry(item.track.id)
                .or_insert_with(|| item.track.clone());
        }
        for track in library.lists.values().flatten() {
            local_tracks
                .entry(track.id)
                .or_insert_with(|| track.clone());
        }
        if ids.iter().all(|id| local_tracks.contains_key(id)) {
            let tracks = ids
                .iter()
                .map(|id| local_tracks.get(id).expect("checked above").clone())
                .collect();
            self.cast_adopt(&probed, &refs, Some(tracks), 0);
            return;
        }
        let client = library.client.clone();
        let tx = self.internal.clone();
        let generation = self.cast_generation;
        tokio::spawn(async move {
            let tracks = fetch_tracks(&client, &ids, local_tracks).await;
            let _ = tx.send(Internal::Cast(
                generation,
                CastMsg::AdoptTracks {
                    seq,
                    tracks,
                    probed,
                    refs,
                },
            ));
        });
    }

    /// Takes over the speaker's queue: our queue becomes it (`tracks`), or
    /// already contains it starting at `window_start` (`None`), at the speaker's
    /// track, position and state.
    fn cast_adopt(
        &mut self,
        probed: &Probed,
        refs: &[QueueRef],
        tracks: Option<Vec<Track>>,
        window_start: usize,
    ) {
        let identities: Vec<String> = probed.uris.iter().map(|u| plan::uri_identity(u)).collect();
        let Some(index) =
            plan::resolve_index(&identities, probed.status.index, &probed.status.track_uri)
        else {
            return;
        };
        if let Some(tracks) = tracks {
            if self.queue.shuffle() {
                // The speaker's order is the play order now.
                self.queue.set_shuffle(false);
                self.update_settings(|s| s.shuffle = false);
                self.emit(Event::Shuffle(false));
            }
            self.queue.replace(tracks, index);
        } else {
            self.queue.go(window_start + index);
        }
        if let Some(repeat) = repeat_from_sonos(&probed.mode)
            && repeat != self.settings.repeat
        {
            self.update_settings(|s| s.repeat = repeat);
            self.emit(Event::Repeat(repeat));
        }
        let entries: Vec<Entry> = self
            .queue
            .in_play_order()
            .skip(window_start)
            .zip(refs)
            .zip(identities)
            .map(|((item, r), identity)| Entry {
                uid: item.uid,
                kind: r.kind,
                offset_ms: r.offset_ms,
                identity,
            })
            .collect();
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        let offset = refs[index].offset_ms;
        let position = probed.status.position_ms + offset;
        let paused = probed.status.state == TransportState::Paused;
        let speaker_mode = play_mode_from_sonos(&probed.mode);
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        cast.mirror = entries;
        cast.loaded = true;
        cast.probe_seq = None;
        // Reflect the speaker's actual mode (`None` for any `SHUFFLE*` mode) so
        // `cast_play_mode` clears hardware shuffle when needed.
        cast.mode = speaker_mode;
        cast.state = Some(probed.status.state);
        cast.anchor = (!paused).then(|| (Instant::now(), position));
        let name = cast.room.name.clone();

        self.resume_at = None;
        self.idle = false;
        self.loading = false;
        self.paused = paused;
        self.position_ms = position;
        self.stream_offset_ms = offset;
        self.started_uid = Some(item.uid);
        self.reported_step = position / super::POSITION_STEP_MS;
        self.engine_duration_ms = 0;
        let track = AdapterTrack {
            context: item.context.clone(),
            duration_millis: u64::try_from(item.track.duration_millis).unwrap_or(0),
            track_id: item.track.id,
            unique_id: item.uid.to_string(),
        };
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| a.on_track_change(Some(track)));
            r.act(|a| {
                // Continues the persisted listen of this track, if any.
                a.on_queue_restored();
                #[allow(clippy::cast_precision_loss)]
                a.on_progress(position as f64 / 1000.0);
                a.on_status(playback_status(paused));
            });
        }
        let track = item.track.clone();
        self.emit_now_playing(Some(&track));
        self.emit(Event::Paused(paused));
        self.emit_position();
        self.queue_changed();
        self.cast_play_mode();
        self.save_position();
        self.load_lyrics(item.track.id);
        self.update_playing_sources();
        self.emit(Event::Notice(format!(
            "已同步 Sonos「{name}」正在播放的队列"
        )));
    }

    /// Between polls the position runs on by the clock (smooth progress
    /// and lyrics; Sonos reports whole seconds).
    fn cast_tick(&mut self) {
        if self.idle || self.loading || self.paused {
            return;
        }
        let Some((at, ms)) = self.cast.as_ref().and_then(|c| c.anchor) else {
            return;
        };
        // Never run far ahead of the last poll (a stalled speaker).
        let elapsed = i64::try_from(at.elapsed().as_millis())
            .unwrap_or(0)
            .min(2_500);
        let mut position = ms + elapsed;
        let duration = self.duration_ms();
        if duration > 0 {
            position = position.min(duration);
        }
        if position > self.position_ms {
            self.apply_position(position);
        }
    }

    fn cast_status(&mut self, done: u64, status: Result<Status, String>, volume: Option<i32>) {
        let duration = self.duration_ms();
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        if done != cast.sent {
            // A newer command is still pending; this poll may predate it.
            return;
        }
        if let Some(v) = volume {
            let v = f64::from(v);
            let held = cast.pending_volume.is_some()
                || cast
                    .volume_set_at
                    .is_some_and(|t| t.elapsed() < VOLUME_HOLD);
            if !held && cast.volume != Some(v) {
                cast.volume = Some(v);
                self.emit(Event::Volume(v));
            }
        }
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        let status = match status {
            Ok(status) => {
                cast.failed_polls = 0;
                status
            }
            Err(e) => {
                cast.failed_polls += 1;
                if cast.failed_polls == LOST_POLLS {
                    let name = cast.room.name.clone();
                    tracing::warn!("sonos unreachable: {e}");
                    self.emit(Event::Notice(format!("无法连接 Sonos「{name}」：{e}")));
                    if !self.idle && !self.paused {
                        // Not playing as far as we know: stop claiming it.
                        self.apply_paused(true);
                    }
                }
                return;
            }
        };
        cast.state = Some(status.state);
        if !cast.loaded || self.idle {
            return;
        }
        // The URI identifies the entry; a speaker that reports it in another
        // form is still followed by its track number.
        let index =
            plan::resolve_index(&cast.mirror, status.index, &status.track_uri).or_else(|| {
                status
                    .index
                    .filter(|i| *i < cast.mirror.len() && status.queue_length == cast.mirror.len())
            });
        let uid = index.map(|i| cast.mirror[i].uid);
        let offset = index.map_or(0, |i| cast.mirror[i].offset_ms);
        let current = self.queue.current().map(|i| (i.uid, i.track.title.clone()));
        let current_uid = current.as_ref().map(|c| c.0);
        let now = Instant::now();
        if let Some(until) = cast.settle_until {
            let arrived = uid == current_uid
                && matches!(
                    status.state,
                    TransportState::Playing | TransportState::Paused
                );
            if arrived || now >= until {
                cast.settle_until = None;
            }
            if !arrived && now < until {
                return;
            }
        }
        let (Some(index), true) = (index, status.queue_active) else {
            // Another app plays something else on the speaker.
            cast.foreign_polls += 1;
            if cast.foreign_polls >= FOREIGN_POLLS {
                cast.loaded = false;
                cast.foreign_polls = 0;
                let name = cast.room.name.clone();
                self.emit(Event::Notice(format!(
                    "Sonos「{name}」正在播放其他内容，已停止同步"
                )));
                self.stop_playback();
            }
            return;
        };
        cast.foreign_polls = 0;

        if status.state == TransportState::Stopped {
            cast.stopped_polls += 1;
            cast.anchor = None;
            let polls = cast.stopped_polls;
            if self.loading {
                if polls >= NEVER_STARTED_POLLS {
                    let title = current.map(|c| c.1).unwrap_or_default();
                    self.playback_failed(format!("Sonos 未能开始播放：{title}"));
                }
                return;
            }
            let near_end = [duration, cast.speaker_duration_ms]
                .iter()
                .any(|d| *d > 0 && self.position_ms + END_SLACK_MS >= *d);
            if near_end {
                if polls >= STOPPED_END_POLLS {
                    cast.stopped_polls = 0;
                    // The last entry (of the queue or the window) ended.
                    self.end_of_file();
                }
                return;
            }
            if !self.paused {
                // Stopped from the Sonos app; resuming re-opens the entry.
                self.apply_paused(true);
            }
            return;
        }
        cast.stopped_polls = 0;

        if uid != current_uid
            && let Some(uid) = uid
            && !self.cast_follow(index, uid)
        {
            return;
        }
        let was_loading = self.loading;
        self.loading = false;
        let mut position = status.position_ms + offset;
        let duration = self.duration_ms();
        if duration > 0 {
            position = position.min(duration);
        }
        // Fallback guard for Sonos repeat-one loop wrap while an EndOfTrack sleep
        // timer is armed (in case the speaker wrapped before Mode(Normal) took effect).
        if !was_loading
            && self.pause_at_end()
            && self.settings.repeat == Repeat::One
            && duration > 0
            && self.position_ms + 20_000 >= duration
            && position < 3_000
            && self.position_ms.saturating_sub(position) > 10_000
        {
            self.stop_playback();
            self.show_current_stopped();
            self.set_sleep_timer(None);
            return;
        }
        // A live entry's stream starts at its offset.
        let speaker_duration = if status.duration_ms > 0 {
            status.duration_ms + offset
        } else {
            0
        };
        if self.engine_duration_ms == 0 && speaker_duration > 0 {
            self.engine_duration_ms = speaker_duration;
        }
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        let playing = status.state == TransportState::Playing;
        if speaker_duration > 0 {
            cast.speaker_duration_ms = speaker_duration;
        }
        // Sonos reports whole seconds: keep the clock's position when it is
        // within the second after the reported one.
        let predicted = cast
            .anchor
            .map(|(at, ms)| ms + i64::try_from(at.elapsed().as_millis()).unwrap_or(0));
        let position = match predicted {
            Some(p) if playing && p >= position && p < position + 1_000 => p,
            _ => position,
        };
        cast.anchor = playing.then_some((now, position));
        let buffering = status.state == TransportState::Transitioning;
        let buffering_changed = buffering != cast.buffering;
        cast.buffering = buffering;
        self.apply_position(position);
        if buffering_changed {
            self.apply_buffering(buffering);
        }
        match status.state {
            TransportState::Playing if self.paused => self.apply_paused(false),
            TransportState::Paused if !self.paused => self.apply_paused(true),
            _ => {}
        }
    }

    /// The speaker moved to mirror entry `index` by itself (or another
    /// controller skipped): follow it. Returns whether playback goes on.
    fn cast_follow(&mut self, index: usize, uid: u64) -> bool {
        // A live entry left at an offset starts at 0 on its next pass.
        if let Some((old_index, old)) = self.cast_current()
            && old.kind == Kind::LiveMp3
            && old.offset_ms != 0
            && old_index != index
            && let Some(track) = self.queue.current().map(|i| i.track.clone())
            && let Some(item) = self.cast_item(&track, Kind::LiveMp3, 0)
        {
            self.cast_send(Op::ReplaceItem {
                index: old_index,
                item,
            });
            if let Some(e) = self.cast.as_mut().and_then(|c| c.mirror.get_mut(old_index)) {
                e.offset_ms = 0;
            }
        }
        if self.pause_at_end() {
            // Sleep timer: stop at this change of track.
            self.stop_playback();
            self.queue.go_to_uid(uid);
            self.show_current_stopped();
            self.emit_queue_position();
            self.set_sleep_timer(None);
            return false;
        }
        if self.queue.go_to_uid(uid).is_none() {
            return false;
        }
        if let Some(cast) = self.cast.as_mut() {
            cast.speaker_duration_ms = 0;
        }
        self.start_current(false, false);
        self.emit_queue_position();
        true
    }
}

/// The tracks `ids` (in that order) for adopting a speaker queue, reusing any
/// tracks already known locally so large queues avoid redundant HTTP calls.
async fn fetch_tracks(
    client: &Client,
    ids: &[i64],
    mut found: std::collections::HashMap<i64, Track>,
) -> Result<Vec<Track>, String> {
    let mut missing: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| !found.contains_key(id))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(ADOPT_FETCHES));
    let mut tasks = tokio::task::JoinSet::new();
    for id in missing {
        let client = client.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let _permit = semaphore.acquire().await;
            (id, client.track(id).await)
        });
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((id, Ok(track))) => {
                found.insert(id, track);
            }
            Ok((_, Err(e))) => return Err(e.to_string()),
            Err(e) => return Err(e.to_string()),
        }
    }
    ids.iter()
        .map(|id| {
            found
                .get(id)
                .cloned()
                .ok_or_else(|| format!("曲目 {id} 不存在"))
        })
        .collect()
}
