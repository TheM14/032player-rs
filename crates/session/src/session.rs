//! 032 Music Server playback session protocol (apiRevision 3).
//!
//! Port of `032musicplayer/src/renderer/api/music032/music032-playback-session.ts`,
//! itself a port of the server's `internal/http/assets/playback-reporter.js`
//! (the reference implementation); keep the semantics in sync with it.
//!
//! Sans-IO: the session never touches the network or the clock. The driver
//! pulls events with [`PlaybackSession::poll_transmit`], sends them and feeds
//! the result back with [`PlaybackSession::handle_outcome`]; timers go
//! through the injected [`Scheduler`] and come back via
//! [`PlaybackSession::on_timer`]. Events are delivered strictly one at a
//! time, in order, like the serial queue of the reference.
//!
//! Desktop differences from the web reference:
//!  - the resume snapshot and the resume_invalid block live in persistent
//!    storage, so an app restart resumes the chain the same way a browser
//!    refresh does;
//!  - every event takes an explicit position/duration from the caller; the
//!    probe is only used for heartbeats of the live session.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::env::{KeyValueStorage, Probe, Scheduler, TimerId};
use crate::types::{
    DeliveryId, EndReason, EventPosition, EventType, PlayOptions, PlaybackContext,
    PlaybackEventBody, PlaybackState, ProtocolError, SessionDebug, Transmit, TransportOutcome,
    normalize_playback_context,
};

/// clientKind of new sessions, read once per session: a session keeps the
/// kind it started with (the server rejects a change).
pub type ClientKindFn = Box<dyn Fn() -> String + Send>;
pub type UuidFn = Box<dyn FnMut() -> String + Send>;

pub struct SessionOptions {
    pub client_kind: Option<ClientKindFn>,
    pub probe: Option<Arc<dyn Probe>>,
    pub heartbeat_active_millis: u64,
    pub heartbeat_paused_millis: u64,
    pub retry_delays_millis: Vec<u64>,
    pub scheduler: Box<dyn Scheduler>,
    pub storage: Option<Box<dyn KeyValueStorage>>,
    pub storage_prefix: String,
    pub uuid: UuidFn,
}

impl SessionOptions {
    pub fn new(scheduler: Box<dyn Scheduler>, uuid: UuidFn) -> Self {
        Self {
            client_kind: None,
            probe: None,
            heartbeat_active_millis: 15_000,
            heartbeat_paused_millis: 60_000,
            retry_delays_millis: vec![1_000, 3_000],
            scheduler,
            storage: None,
            storage_prefix: "032_playback".to_owned(),
            uuid,
        }
    }

    /// Fixed clientKind for every session.
    pub fn with_client_kind(mut self, kind: impl Into<String>) -> Self {
        let kind = kind.into();
        self.client_kind = Some(Box::new(move || kind.clone()));
        self
    }
}

type SessKey = u64;

#[derive(Debug)]
struct Sess {
    /// Fixed for the life of the session: the server treats a change as a conflict.
    client_kind: String,
    context: Option<PlaybackContext>,
    end_delivered: bool,
    ended: bool,
    end_given_up: bool,
    has_played: bool,
    id: String,
    missing_recovery_done: bool,
    seq: u64,
    started: bool,
    state: PlaybackState,
    superseded: bool,
    track_id: i64,
}

#[derive(Clone, Debug)]
struct Job {
    duration_millis: u64,
    end_reason: Option<EndReason>,
    position_millis: u64,
    /// Frozen at enqueue: the state at the moment an end happened.
    prior_state: Option<PlaybackState>,
    recovery_depth: u32,
    refresh_position: bool,
    resume_from: Option<String>,
    session: SessKey,
    state: Option<PlaybackState>,
    kind: EventType,
}

#[derive(Default)]
struct JobExtra {
    duration_millis: Option<u64>,
    end_reason: Option<EndReason>,
    position_millis: Option<u64>,
    recovery_depth: u32,
    refresh_position: bool,
    resume_from: Option<String>,
    state: Option<PlaybackState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Body built, waiting to be handed to the driver.
    Ready,
    /// Handed out, waiting for the outcome.
    Sent,
    /// Failed, waiting for the retry timer.
    WaitingRetry(TimerId),
}

struct Inflight {
    id: DeliveryId,
    job: Job,
    body: PlaybackEventBody,
    attempt: usize,
    phase: Phase,
}

pub struct PlaybackSession {
    client_kind: Option<ClientKindFn>,
    probe: Option<Arc<dyn Probe>>,
    heartbeat_active_millis: u64,
    heartbeat_paused_millis: u64,
    retry_delays_millis: Vec<u64>,
    scheduler: Box<dyn Scheduler>,
    storage: Option<Box<dyn KeyValueStorage>>,
    uuid: UuidFn,
    client_id_key: String,
    snapshot_key: String,
    block_key: String,

    client_id: Option<String>,
    sessions: HashMap<SessKey, Sess>,
    next_sess_key: SessKey,
    session: Option<SessKey>,
    blocked: bool,
    queue: VecDeque<Job>,
    inflight: Option<Inflight>,
    next_delivery: DeliveryId,
    keepalive_out: VecDeque<Transmit>,
    heartbeat_timer: Option<TimerId>,
    protocol_errors: VecDeque<ProtocolError>,
}

fn is_valid_client_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `Number.isInteger` for JSON values.
fn json_integer(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    let f = value.as_f64()?;
    #[allow(clippy::cast_possible_truncation)]
    (f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15).then_some(f as i64)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn past_count_threshold(position_millis: u64, duration_millis: u64) -> bool {
    duration_millis > 0 && position_millis.saturating_mul(2) >= duration_millis
}

struct Snapshot {
    has_played: bool,
    session_id: String,
    track_id: i64,
}

impl PlaybackSession {
    pub fn new(options: SessionOptions) -> Self {
        let prefix = if options.storage_prefix.is_empty() {
            "032_playback".to_owned()
        } else {
            options.storage_prefix
        };
        let or = |v: u64, d: u64| if v == 0 { d } else { v };
        Self {
            client_kind: options.client_kind,
            probe: options.probe,
            heartbeat_active_millis: or(options.heartbeat_active_millis, 15_000),
            heartbeat_paused_millis: or(options.heartbeat_paused_millis, 60_000),
            retry_delays_millis: options.retry_delays_millis,
            scheduler: options.scheduler,
            storage: options.storage,
            uuid: options.uuid,
            client_id_key: format!("{prefix}_client_id"),
            snapshot_key: format!("{prefix}_session_v1"),
            block_key: format!("{prefix}_block_v1"),
            client_id: None,
            sessions: HashMap::new(),
            next_sess_key: 0,
            session: None,
            blocked: false,
            queue: VecDeque::new(),
            inflight: None,
            next_delivery: 0,
            keepalive_out: VecDeque::new(),
            heartbeat_timer: None,
            protocol_errors: VecDeque::new(),
        }
    }

    // ------------------------------------------------------------ storage

    fn read(&self, key: &str) -> Option<String> {
        self.storage.as_ref().and_then(|s| s.get(key))
    }

    fn write(&mut self, key: &str, value: &str) {
        if let Some(s) = self.storage.as_mut() {
            s.set(key, value);
        }
    }

    fn remove(&mut self, key: &str) {
        if let Some(s) = self.storage.as_mut() {
            s.remove(key);
        }
    }

    fn load_client_id(&mut self) -> String {
        if let Some(id) = &self.client_id {
            return id.clone();
        }
        let id = match self.read(&self.client_id_key) {
            Some(id) if is_valid_client_id(&id) => id,
            _ => {
                let id = (self.uuid)();
                let key = self.client_id_key.clone();
                self.write(&key, &id);
                id
            }
        };
        self.client_id = Some(id.clone());
        id
    }

    fn persist_snapshot(&mut self, key: SessKey) {
        let s = &self.sessions[&key];
        let value = json!({
            "hasPlayed": s.has_played,
            "sessionId": s.id,
            "trackId": s.track_id,
        })
        .to_string();
        let k = self.snapshot_key.clone();
        self.write(&k, &value);
    }

    fn read_snapshot(&self) -> Option<Snapshot> {
        let raw = self.read(&self.snapshot_key)?;
        let parsed: Value = serde_json::from_str(&raw).ok()?;
        let session_id = parsed.get("sessionId")?.as_str()?.to_owned();
        let track_id = json_integer(parsed.get("trackId")?)?;
        // Never invent play evidence for legacy snapshots.
        let has_played = parsed.get("hasPlayed") == Some(&Value::Bool(true));
        Some(Snapshot {
            has_played,
            session_id,
            track_id,
        })
    }

    fn clear_snapshot(&mut self) {
        let k = self.snapshot_key.clone();
        self.remove(&k);
    }

    fn write_block_marker(&mut self, track_id: i64) {
        let k = self.block_key.clone();
        let v = json!({ "blocked": true, "trackId": track_id }).to_string();
        self.write(&k, &v);
    }

    fn read_block_marker(&self) -> Option<i64> {
        let raw = self.read(&self.block_key)?;
        let parsed: Value = serde_json::from_str(&raw).ok()?;
        if parsed.get("blocked") != Some(&Value::Bool(true)) {
            return None;
        }
        json_integer(parsed.get("trackId")?)
    }

    fn clear_block_marker(&mut self) {
        let k = self.block_key.clone();
        self.remove(&k);
    }

    // ------------------------------------------------------------ helpers

    fn probe_position(&self) -> u64 {
        self.probe.as_ref().map_or(0, |p| p.position_millis())
    }

    fn probe_duration(&self) -> u64 {
        self.probe.as_ref().map_or(0, |p| p.duration_millis())
    }

    fn resolve_client_kind(&self) -> String {
        let kind = self.client_kind.as_ref().map(|f| f()).unwrap_or_default();
        if kind.is_empty() {
            "web".to_owned()
        } else {
            kind
        }
    }

    fn new_session(&mut self, track_id: i64, state: Option<PlaybackState>) -> SessKey {
        let initial_state = state.unwrap_or(PlaybackState::Buffering);
        let sess = Sess {
            client_kind: self.resolve_client_kind(),
            context: None,
            end_delivered: false,
            ended: false,
            end_given_up: false,
            has_played: initial_state == PlaybackState::Playing,
            id: (self.uuid)(),
            missing_recovery_done: false,
            seq: 0,
            started: false,
            state: initial_state,
            superseded: false,
            track_id,
        };
        let key = self.next_sess_key;
        self.next_sess_key += 1;
        self.sessions.insert(key, sess);
        key
    }

    fn sess(&self, key: SessKey) -> &Sess {
        &self.sessions[&key]
    }

    fn sess_mut(&mut self, key: SessKey) -> &mut Sess {
        self.sessions.get_mut(&key).expect("session referenced")
    }

    fn is_current(&self, key: SessKey) -> bool {
        self.session == Some(key)
    }

    /// Recovery-start state follows the server's priorState rule: hasPlayed
    /// history alone never justifies labelling a recovery start 'playing'.
    fn recovery_start_state(&self, key: SessKey, job: &Job) -> PlaybackState {
        if job.kind == EventType::End {
            return job.prior_state.unwrap_or(PlaybackState::Paused);
        }
        self.sess(key).state
    }

    // ------------------------------------------------------------ queueing

    fn make_job(&self, key: SessKey, kind: EventType, extra: JobExtra) -> Job {
        Job {
            duration_millis: extra
                .duration_millis
                .unwrap_or_else(|| self.probe_duration()),
            end_reason: extra.end_reason,
            position_millis: extra
                .position_millis
                .unwrap_or_else(|| self.probe_position()),
            prior_state: (kind == EventType::End).then(|| self.sess(key).state),
            recovery_depth: extra.recovery_depth,
            refresh_position: extra.refresh_position,
            resume_from: extra.resume_from,
            session: key,
            state: extra.state,
            kind,
        }
    }

    fn enqueue(&mut self, key: SessKey, kind: EventType, extra: JobExtra) {
        let job = self.make_job(key, kind, extra);
        // Only the latest pending seek of a session matters.
        if kind == EventType::Seek {
            self.queue
                .retain(|other| !(other.session == key && other.kind == EventType::Seek));
        }
        self.queue.push_back(job);
        self.pump();
    }

    fn is_stale(&self, job: &Job) -> bool {
        let sess = self.sess(job.session);
        if job.kind == EventType::End {
            return sess.end_delivered || sess.end_given_up;
        }
        if sess.ended || sess.superseded {
            return true;
        }
        if job.kind == EventType::Heartbeat {
            return self.queue.iter().any(|other| other.session == job.session);
        }
        false
    }

    /// Takes the next non-stale job when nothing is in flight.
    fn pump(&mut self) {
        if self.inflight.is_none() {
            while let Some(job) = self.queue.pop_front() {
                if self.is_stale(&job) {
                    continue;
                }
                let body = self.build_body(&job);
                self.next_delivery += 1;
                self.inflight = Some(Inflight {
                    id: self.next_delivery,
                    job,
                    body,
                    attempt: 0,
                    phase: Phase::Ready,
                });
                break;
            }
        }
        self.gc();
    }

    /// Drops sessions nothing refers to any more.
    fn gc(&mut self) {
        let current = self.session;
        let inflight = self.inflight.as_ref().map(|i| i.job.session);
        let queue = &self.queue;
        self.sessions.retain(|key, _| {
            Some(*key) == current
                || Some(*key) == inflight
                || queue.iter().any(|j| j.session == *key)
        });
    }

    fn build_body(&mut self, job: &Job) -> PlaybackEventBody {
        let client_id = self.load_client_id();
        let position_millis = if job.refresh_position {
            self.probe_position()
        } else {
            job.position_millis
        };
        let sess = self.sess_mut(job.session);
        sess.seq += 1;
        let mut body = PlaybackEventBody {
            client_id,
            client_kind: sess.client_kind.clone(),
            context: None,
            duration_millis: job.duration_millis,
            end_reason: None,
            position_millis,
            resumed_from_session_id: None,
            seq: sess.seq,
            session_id: sess.id.clone(),
            state: None,
            track_id: sess.track_id,
            kind: job.kind,
        };
        match job.kind {
            EventType::Start => {
                body.state = job.state;
                if job.resume_from.is_some() {
                    body.resumed_from_session_id.clone_from(&job.resume_from);
                } else {
                    body.context.clone_from(&sess.context);
                }
            }
            EventType::Heartbeat => body.state = Some(sess.state),
            EventType::End => body.end_reason = job.end_reason,
            _ => {}
        }
        body
    }

    // ------------------------------------------------------------ driver API

    /// Next event to send, if any. Keepalive events (app close) come first
    /// and need no outcome; every other event must be answered with
    /// [`Self::handle_outcome`] before the next one is handed out.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        if let Some(t) = self.keepalive_out.pop_front() {
            return Some(t);
        }
        self.pump();
        let inflight = self.inflight.as_mut()?;
        if inflight.phase != Phase::Ready {
            return None;
        }
        inflight.phase = Phase::Sent;
        Some(Transmit {
            id: inflight.id,
            body: inflight.body.clone(),
            keepalive: false,
        })
    }

    /// Feeds back the result of the delivery `id`.
    pub fn handle_outcome(&mut self, id: DeliveryId, outcome: TransportOutcome) {
        let Some(inflight) = self.inflight.as_mut() else {
            return;
        };
        if inflight.id != id || inflight.phase != Phase::Sent {
            return;
        }
        let retryable = match &outcome {
            TransportOutcome::Response { status, .. } => *status >= 500,
            TransportOutcome::NoResponse => false,
            TransportOutcome::NetworkError => true,
        };
        if retryable {
            if let Some(&delay) = self.retry_delays_millis.get(inflight.attempt) {
                inflight.attempt += 1;
                let timer = self.scheduler.set_timeout(delay);
                if let Some(inflight) = self.inflight.as_mut() {
                    inflight.phase = Phase::WaitingRetry(timer);
                }
                return;
            }
            // Bounded retry exhausted: the server-side lease expires the session.
            let inflight = self.inflight.take().expect("inflight");
            if inflight.job.kind == EventType::End {
                self.sess_mut(inflight.job.session).end_given_up = true;
            }
            self.pump();
            return;
        }
        let inflight = self.inflight.take().expect("inflight");
        let (status, json) = match outcome {
            TransportOutcome::Response { status, json } => (status, json),
            _ => (0, Value::Null),
        };
        self.handle_response(inflight.job, status, &json);
        self.pump();
    }

    /// A timer set through the [`Scheduler`] fired.
    pub fn on_timer(&mut self, id: TimerId) {
        if let Some(inflight) = self.inflight.as_ref()
            && inflight.phase == Phase::WaitingRetry(id)
        {
            if self.is_stale(&inflight.job) {
                self.inflight = None;
                self.pump();
            } else if let Some(inflight) = self.inflight.as_mut() {
                inflight.phase = Phase::Ready;
            }
            return;
        }
        if self.heartbeat_timer == Some(id) {
            self.heartbeat_timer = None;
            let Some(cur) = self.session else { return };
            if self.sess(cur).ended || self.blocked {
                return;
            }
            let queued = self
                .queue
                .iter()
                .any(|j| j.session == cur && j.kind == EventType::Heartbeat);
            if !queued {
                self.enqueue(
                    cur,
                    EventType::Heartbeat,
                    JobExtra {
                        refresh_position: true,
                        ..JobExtra::default()
                    },
                );
            }
            self.schedule_heartbeat();
        }
    }

    /// Protocol failures the client cannot recover from, oldest first.
    pub fn poll_protocol_error(&mut self) -> Option<ProtocolError> {
        self.protocol_errors.pop_front()
    }

    /// No queued or in-flight events (keepalive events excluded).
    pub fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.inflight.is_none()
    }

    // ------------------------------------------------------------ responses

    fn handle_response(&mut self, job: Job, status: u16, json: &Value) {
        let key = job.session;
        if (200..300).contains(&status) {
            if job.kind == EventType::Start {
                let s = self.sess_mut(key);
                s.started = true;
                s.missing_recovery_done = false;
            }
            if job.kind == EventType::End {
                let s = self.sess_mut(key);
                s.end_delivered = true;
                s.ended = true;
                if self.is_current(key) {
                    self.cancel_heartbeat();
                }
                return;
            }
            if json.get("applied").is_some_and(truthy) {
                if self.is_current(key) && !self.sess(key).ended {
                    self.persist_snapshot(key);
                }
                return;
            }
            // applied:false with a terminal server state: stop faking life.
            if json.get("state").and_then(Value::as_str) == Some("ended") {
                self.finalize_locally(key, "server_ended");
            }
            return;
        }

        let code = json
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (status, code.as_deref()) {
            (404, Some("session_not_found")) => self.recover_missing(job),
            (409, Some("session_expired")) => self.recover_expired(job),
            (409, Some("resume_invalid")) => self.block_resume_invalid(key),
            (404, Some("not_found")) => self.finalize_locally(key, "track_not_found"),
            (400 | 409, code) => self.finalize_locally(key, code.unwrap_or("invalid")),
            _ => {
                if job.kind == EventType::End {
                    self.sess_mut(key).end_given_up = true;
                }
            }
        }
    }

    fn finalize_locally(&mut self, key: SessKey, reason: &str) {
        let s = self.sess_mut(key);
        s.ended = true;
        s.end_delivered = true;
        let error = ProtocolError {
            reason: reason.to_owned(),
            session_id: s.id.clone(),
            track_id: s.track_id,
        };
        if self.is_current(key) {
            self.session = None;
            self.cancel_heartbeat();
            self.clear_snapshot();
            self.blocked = true;
        }
        if !reason.is_empty() {
            self.protocol_errors.push_back(error);
        }
    }

    /// resume_invalid must never degrade into a silent high-position new start.
    fn block_resume_invalid(&mut self, key: SessKey) {
        let track_id = self.sess(key).track_id;
        let affects_current = self.is_current(key)
            || self.session.is_some_and(|cur| {
                let s = self.sess(cur);
                s.superseded && s.ended && s.track_id == track_id
            });
        if affects_current {
            self.blocked = true;
            self.write_block_marker(track_id);
            if !self.is_current(key) {
                self.session = None;
                self.cancel_heartbeat();
                self.clear_snapshot();
            }
        }
        self.finalize_locally(key, "resume_invalid");
    }

    fn recover_missing(&mut self, job: Job) {
        let key = job.session;
        if job.kind == EventType::End {
            self.sess_mut(key).end_delivered = true;
            if job.recovery_depth < 1
                && job.prior_state == Some(PlaybackState::Playing)
                && past_count_threshold(job.position_millis, job.duration_millis)
            {
                let track_id = self.sess(key).track_id;
                let context = self.sess(key).context.clone();
                let recovery = self.new_session(track_id, Some(PlaybackState::Playing));
                self.sess_mut(recovery).context = context;
                let start = self.make_job(
                    recovery,
                    EventType::Start,
                    JobExtra {
                        duration_millis: Some(job.duration_millis),
                        position_millis: Some(job.position_millis),
                        recovery_depth: job.recovery_depth + 1,
                        state: Some(PlaybackState::Playing),
                        ..JobExtra::default()
                    },
                );
                let end = self.make_job(
                    recovery,
                    EventType::End,
                    JobExtra {
                        duration_millis: Some(job.duration_millis),
                        end_reason: job.end_reason,
                        position_millis: Some(job.position_millis),
                        recovery_depth: job.recovery_depth + 1,
                        ..JobExtra::default()
                    },
                );
                self.queue.push_front(end);
                self.queue.push_front(start);
            }
            return;
        }
        if self.sess(key).superseded || self.sess(key).ended {
            return;
        }
        if self.sess(key).missing_recovery_done || job.recovery_depth >= 2 {
            return self.finalize_locally(key, "missing_recovery_failed");
        }
        self.sess_mut(key).missing_recovery_done = true;
        let state = self.sess(key).state;
        let start = self.make_job(
            key,
            EventType::Start,
            JobExtra {
                recovery_depth: job.recovery_depth + 1,
                state: Some(state),
                ..JobExtra::default()
            },
        );
        self.queue.push_front(start);
    }

    fn recover_expired(&mut self, mut job: Job) {
        let key = job.session;
        if job.recovery_depth >= 1 {
            return self.finalize_locally(key, "expired_recovery_failed");
        }
        let is_end = job.kind == EventType::End;
        // A late 409 for an already replaced/ended session never revives it.
        if !is_end && (self.sess(key).superseded || self.sess(key).ended) {
            return;
        }
        if is_end {
            self.sess_mut(key).end_delivered = true;
        }
        let (track_id, state, has_played, context, old_id) = {
            let s = self.sess(key);
            (
                s.track_id,
                s.state,
                s.has_played,
                s.context.clone(),
                s.id.clone(),
            )
        };
        let recovery = self.new_session(track_id, Some(state));
        {
            let r = self.sess_mut(recovery);
            r.has_played = has_played;
            r.context = context;
        }
        let depth = job.recovery_depth + 1;
        let start_state = self.recovery_start_state(key, &job);
        let start = self.make_job(
            recovery,
            EventType::Start,
            JobExtra {
                duration_millis: (job.duration_millis > 0).then_some(job.duration_millis),
                position_millis: is_end.then_some(job.position_millis),
                recovery_depth: depth,
                resume_from: Some(old_id),
                state: Some(start_state),
                ..JobExtra::default()
            },
        );
        job.session = recovery;
        job.recovery_depth = depth;
        self.sess_mut(key).superseded = true;
        for pending in &mut self.queue {
            if pending.session == key {
                pending.session = recovery;
                pending.recovery_depth = depth;
            }
        }
        self.queue.push_front(job);
        self.queue.push_front(start);
        if !is_end && self.is_current(key) {
            self.session = Some(recovery);
            self.schedule_heartbeat();
        }
    }

    // ----------------------------------------------------------- heartbeat

    fn schedule_heartbeat(&mut self) {
        self.cancel_heartbeat();
        let Some(key) = self.session else { return };
        let sess = self.sess(key);
        if sess.ended || self.blocked {
            return;
        }
        let interval = if sess.state == PlaybackState::Paused {
            self.heartbeat_paused_millis
        } else {
            self.heartbeat_active_millis
        };
        self.heartbeat_timer = Some(self.scheduler.set_timeout(interval));
    }

    fn cancel_heartbeat(&mut self) {
        if let Some(id) = self.heartbeat_timer.take() {
            self.scheduler.clear_timeout(id);
        }
    }

    // ------------------------------------------------------ session cycles

    fn end_session(&mut self, key: Option<SessKey>, reason: EndReason, at: EventPosition) {
        let Some(key) = key else { return };
        if self.sess(key).ended {
            return;
        }
        self.sess_mut(key).ended = true;
        if self.is_current(key) {
            self.cancel_heartbeat();
        }
        self.enqueue(
            key,
            EventType::End,
            JobExtra {
                duration_millis: at.duration_millis,
                end_reason: Some(reason),
                position_millis: at.position_millis,
                ..JobExtra::default()
            },
        );
        // completed/skipped/replaced are finished business: never resume them.
        if self.is_current(key)
            && matches!(
                reason,
                EndReason::Completed | EndReason::Skipped | EndReason::Replaced
            )
        {
            self.clear_snapshot();
        }
    }

    fn live_session(&self) -> Option<SessKey> {
        self.session
            .filter(|key| !self.sess(*key).ended && !self.blocked)
    }

    fn transition(&mut self, kind: EventType, new_state: PlaybackState, position: Option<u64>) {
        let Some(key) = self.live_session() else {
            return;
        };
        let sess = self.sess_mut(key);
        if sess.state == new_state {
            return;
        }
        sess.state = new_state;
        if new_state == PlaybackState::Playing {
            sess.has_played = true;
        }
        self.enqueue(
            key,
            kind,
            JobExtra {
                position_millis: position,
                ..JobExtra::default()
            },
        );
        self.schedule_heartbeat();
    }

    // ------------------------------------------------------------ public API

    pub fn current_track_id(&self) -> Option<i64> {
        self.session
            .map(|k| self.sess(k))
            .filter(|s| !s.ended)
            .map(|s| s.track_id)
    }

    pub fn dispose(&mut self) {
        self.cancel_heartbeat();
        if let Some(inflight) = self.inflight.take()
            && let Phase::WaitingRetry(timer) = inflight.phase
        {
            self.scheduler.clear_timeout(timer);
        }
        self.queue.clear();
        self.keepalive_out.clear();
        self.session = None;
        self.gc();
    }

    pub fn end(&mut self, reason: EndReason, at: EventPosition) {
        self.end_session(self.session, reason, at);
    }

    pub fn debug(&self) -> SessionDebug {
        let s = self.session.map(|k| self.sess(k));
        SessionDebug {
            blocked: self.blocked,
            client_id: self.client_id.clone(),
            ended: s.map(|s| s.ended),
            has_played: s.map(|s| s.has_played),
            queue_length: self.queue.len(),
            seq: s.map_or(0, |s| s.seq),
            session_id: s.map(|s| s.id.clone()),
            state: s.map(|s| s.state),
        }
    }

    pub fn has_active_session(&self) -> bool {
        self.current_track_id().is_some()
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked
    }

    pub fn notify_buffering(&mut self, position_millis: Option<u64>) {
        let Some(key) = self.live_session() else {
            return;
        };
        if self.sess(key).state != PlaybackState::Playing {
            return;
        }
        self.transition(
            EventType::Buffering,
            PlaybackState::Buffering,
            position_millis,
        );
    }

    pub fn notify_paused(&mut self, position_millis: Option<u64>) {
        self.transition(EventType::Pause, PlaybackState::Paused, position_millis);
    }

    pub fn notify_playing(&mut self, position_millis: Option<u64>) {
        self.transition(EventType::Resume, PlaybackState::Playing, position_millis);
    }

    pub fn notify_seek(&mut self, position_millis: Option<u64>) {
        let Some(key) = self.live_session() else {
            return;
        };
        self.enqueue(
            key,
            EventType::Seek,
            JobExtra {
                position_millis,
                ..JobExtra::default()
            },
        );
    }

    /// Explicit new play: ends the previous session (given reason, its own
    /// position) before starting a fresh session for `track_id`.
    pub fn play(&mut self, track_id: i64, opts: PlayOptions) {
        self.blocked = false;
        self.clear_block_marker();
        if let Some(previous) = self.session
            && !self.sess(previous).ended
        {
            self.sess_mut(previous).superseded = true;
            self.end_session(
                Some(previous),
                opts.reason.unwrap_or(EndReason::Replaced),
                EventPosition::default(),
            );
        }
        let key = self.new_session(
            track_id,
            Some(opts.initial_state.unwrap_or(PlaybackState::Buffering)),
        );
        self.sess_mut(key).context = normalize_playback_context(opts.context.as_ref());
        self.session = Some(key);
        let state = self.sess(key).state;
        self.enqueue(
            key,
            EventType::Start,
            JobExtra {
                duration_millis: opts.duration_millis,
                position_millis: opts.position_millis,
                state: Some(state),
                ..JobExtra::default()
            },
        );
        self.schedule_heartbeat();
    }

    /// Same-track recovery (app restart, retry after an error): resumes the
    /// persisted chain when the snapshot names this track, else a new play.
    pub fn restore(&mut self, track_id: i64, opts: PlayOptions) {
        if self.has_active_session() {
            return;
        }
        if let Some(marker) = self.read_block_marker() {
            if marker == track_id {
                self.blocked = true;
                return;
            }
            self.clear_block_marker();
            self.blocked = false;
        }
        if self.blocked {
            return;
        }
        let snapshot = self.read_snapshot().filter(|s| s.track_id == track_id);
        let key = self.new_session(
            track_id,
            Some(opts.initial_state.unwrap_or(PlaybackState::Paused)),
        );
        self.session = Some(key);
        if snapshot.as_ref().is_some_and(|s| s.has_played) {
            self.sess_mut(key).has_played = true;
        }
        // A resumed chain keeps its context server-side; a fresh chain carries the queue's.
        if snapshot.is_none() {
            self.sess_mut(key).context = normalize_playback_context(opts.context.as_ref());
        }
        let state = self.sess(key).state;
        self.enqueue(
            key,
            EventType::Start,
            JobExtra {
                duration_millis: opts.duration_millis,
                position_millis: opts.position_millis,
                resume_from: snapshot.map(|s| s.session_id),
                state: Some(state),
                ..JobExtra::default()
            },
        );
        self.schedule_heartbeat();
    }

    /// App/window close: best-effort end(client_closed) as a keepalive event
    /// that bypasses the serial queue. The snapshot is kept so the next
    /// launch resumes the chain without counting the play twice.
    pub fn suspend(&mut self, at: EventPosition) {
        let Some(key) = self.session else { return };
        if self.sess(key).ended {
            return;
        }
        self.sess_mut(key).ended = true;
        self.cancel_heartbeat();
        let client_id = self.load_client_id();
        let duration_millis = at.duration_millis.unwrap_or_else(|| self.probe_duration());
        let position_millis = at.position_millis.unwrap_or_else(|| self.probe_position());
        let s = self.sess(key);
        let body = PlaybackEventBody {
            client_id,
            client_kind: s.client_kind.clone(),
            context: None,
            duration_millis,
            end_reason: Some(EndReason::ClientClosed),
            position_millis,
            resumed_from_session_id: None,
            seq: s.seq + 1,
            session_id: s.id.clone(),
            state: None,
            track_id: s.track_id,
            kind: EventType::End,
        };
        if s.started {
            self.persist_snapshot(key);
        }
        self.next_delivery += 1;
        self.keepalive_out.push_back(Transmit {
            id: self.next_delivery,
            body,
            keepalive: true,
        });
    }
}
