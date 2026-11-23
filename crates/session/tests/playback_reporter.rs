//! Port of `032musicplayer/scripts/music032/playback-reporter.test.mjs`.
//! Drives the adapter + session with a fake transport, clock and timers.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use session::{
    AdapterStatus, AdapterTrack, EndReason, EventType, MemoryStorage, PlaybackAdapter,
    PlaybackContext, PlaybackEventBody, PlaybackState, Scheduler, SessionOptions, TimerId,
    TransportOutcome,
};

fn track_a() -> AdapterTrack {
    AdapterTrack {
        context: None,
        duration_millis: 240_000,
        track_id: 1,
        unique_id: "qa".into(),
    }
}

fn track_b() -> AdapterTrack {
    AdapterTrack {
        context: None,
        duration_millis: 180_000,
        track_id: 2,
        unique_id: "qb".into(),
    }
}

type Timers = Arc<Mutex<BTreeMap<TimerId, u64>>>;

struct FakeScheduler {
    timers: Timers,
    next: TimerId,
}

impl Scheduler for FakeScheduler {
    fn set_timeout(&mut self, delay_millis: u64) -> TimerId {
        self.next += 1;
        self.timers.lock().unwrap().insert(self.next, delay_millis);
        self.next
    }

    fn clear_timeout(&mut self, id: TimerId) {
        self.timers.lock().unwrap().remove(&id);
    }
}

type Respond = Box<dyn FnMut(&PlaybackEventBody, &[PlaybackEventBody]) -> Option<TransportOutcome>>;

#[derive(Default)]
struct Opts {
    client_kind: Option<Box<dyn Fn() -> String + Send>>,
    respond: Option<Respond>,
    storage: Option<MemoryStorage>,
}

struct Harness {
    adapter: PlaybackAdapter,
    clock: Arc<AtomicI64>,
    keepalive: Vec<PlaybackEventBody>,
    respond: Option<Respond>,
    sent: Vec<PlaybackEventBody>,
    timers: Timers,
}

impl Harness {
    fn new() -> Self {
        Self::with(Opts::default())
    }

    fn with(opts: Opts) -> Self {
        let timers: Timers = Arc::default();
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let mut uuid_n = 0u64;
        let mut options = SessionOptions::new(
            Box::new(FakeScheduler {
                timers: timers.clone(),
                next: 0,
            }),
            Box::new(move || {
                uuid_n += 1;
                format!("00000000-0000-4000-8000-{uuid_n:012}")
            }),
        );
        options.client_kind = opts.client_kind;
        options.retry_delays_millis = vec![];
        options.storage = Some(Box::new(opts.storage.unwrap_or_default()));
        let now_clock = clock.clone();
        let adapter =
            PlaybackAdapter::new(options, Box::new(move || now_clock.load(Ordering::Relaxed)));
        Self {
            adapter,
            clock,
            keepalive: vec![],
            respond: opts.respond,
            sent: vec![],
            timers,
        }
    }

    /// Delivers every queued event, like `await session.idle()`.
    fn idle(&mut self) {
        while let Some(tx) = self.adapter.session_mut().poll_transmit() {
            if tx.keepalive {
                self.keepalive.push(tx.body);
                continue;
            }
            self.sent.push(tx.body.clone());
            let custom = self
                .respond
                .as_mut()
                .and_then(|respond| respond(&tx.body, &self.sent));
            let outcome = custom.unwrap_or_else(|| {
                let state = tx.body.state.unwrap_or(PlaybackState::Playing);
                TransportOutcome::response(200, json!({ "applied": true, "state": state }))
            });
            self.adapter.session_mut().handle_outcome(tx.id, outcome);
        }
    }

    /// Applies signals as one player action, then flushes.
    fn act(&mut self, f: impl FnOnce(&mut PlaybackAdapter)) {
        f(&mut self.adapter);
        self.adapter.flush();
        self.idle();
    }

    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::Relaxed);
    }

    fn run_timers(&mut self) {
        let due: Vec<TimerId> = std::mem::take(&mut *self.timers.lock().unwrap())
            .into_keys()
            .collect();
        for id in due {
            self.adapter.session_mut().on_timer(id);
        }
        self.idle();
    }

    fn timer_delays(&self) -> Vec<u64> {
        self.timers.lock().unwrap().values().copied().collect()
    }

    fn last(&self) -> &PlaybackEventBody {
        self.sent.last().expect("an event was sent")
    }

    /// Plays `track` from the start and moves the playhead to `position_sec`.
    fn play_to(&mut self, track: AdapterTrack, position_sec: u32) {
        self.act(|a| {
            a.on_track_change(Some(track));
            a.on_status(AdapterStatus::Playing);
        });
        for s in 1..=position_sec {
            self.advance(1000);
            self.act(|a| a.on_progress(f64::from(s)));
        }
    }
}

fn kind_name(kind: EventType) -> &'static str {
    match kind {
        EventType::Buffering => "buffering",
        EventType::End => "end",
        EventType::Heartbeat => "heartbeat",
        EventType::Pause => "pause",
        EventType::Resume => "resume",
        EventType::Seek => "seek",
        EventType::Start => "start",
    }
}

fn types(events: &[PlaybackEventBody]) -> Vec<String> {
    events
        .iter()
        .map(|e| format!("{}:{}", kind_name(e.kind), e.track_id))
        .collect()
}

fn ends(events: &[PlaybackEventBody]) -> Vec<&PlaybackEventBody> {
    events.iter().filter(|e| e.kind == EventType::End).collect()
}

fn error_response(status: u16, code: &str) -> TransportOutcome {
    TransportOutcome::response(status, json!({ "error": { "code": code } }))
}

#[test]
fn client_kind_defaults_to_web_and_is_fixed_per_session_when_it_is_a_function() {
    let mut plain = Harness::new();
    plain.play_to(track_a(), 1);
    assert_eq!(plain.sent[0].client_kind, "web");

    let kind = Arc::new(Mutex::new("web".to_owned()));
    let read_kind = kind.clone();
    let mut h = Harness::with(Opts {
        client_kind: Some(Box::new(move || read_kind.lock().unwrap().clone())),
        ..Opts::default()
    });
    h.play_to(track_a(), 2);
    // Capabilities arrive mid-session: the running session keeps its kind.
    *kind.lock().unwrap() = "desktop".into();
    h.act(|a| a.on_status(AdapterStatus::Paused));
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_b()));
        a.on_user_navigate();
        a.on_status(AdapterStatus::Playing);
    });
    let kinds: Vec<String> = h
        .sent
        .iter()
        .map(|e| format!("{}:{}:{}", kind_name(e.kind), e.track_id, e.client_kind))
        .collect();
    assert_eq!(
        kinds,
        ["start:1:web", "pause:1:web", "end:1:web", "start:2:desktop"]
    );
}

#[test]
fn normal_completion_auto_advance_ends_with_completed_at_the_real_final_position() {
    let mut h = Harness::new();
    h.play_to(track_a(), 239);
    // Auto next: position resets to 0 and the track changes in one action.
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_b()));
    });

    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:2"]);
    let end = ends(&h.sent)[0];
    assert_eq!(end.end_reason, Some(EndReason::Completed));
    assert_eq!(end.position_millis, 239_000);
    assert_eq!(end.duration_millis, track_a().duration_millis);
    let start_b = &h.sent[2];
    assert_eq!(start_b.position_millis, 0);
    assert_eq!(start_b.duration_millis, track_b().duration_millis);
    assert_ne!(start_b.session_id, h.sent[0].session_id);
    assert_eq!(start_b.resumed_from_session_id, None);
}

#[test]
fn track_switch_user_skip_ends_the_old_session_with_its_own_data() {
    let mut h = Harness::new();
    h.play_to(track_a(), 42);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_b()));
        a.on_user_navigate();
    });

    let end = ends(&h.sent)[0];
    assert_eq!(end.track_id, 1);
    assert_eq!(end.end_reason, Some(EndReason::Skipped));
    assert_eq!(end.position_millis, 42_000);
    assert_eq!(end.duration_millis, track_a().duration_millis);
    assert_eq!(end.session_id, h.sent[0].session_id);
    // The old end is delivered before the new start.
    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:2"]);
}

#[test]
fn next_pressed_close_to_the_end_is_still_a_skip_not_a_loop_restart() {
    let mut h = Harness::new();
    h.play_to(track_a(), 238);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_b()));
        a.on_user_navigate();
    });
    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:2"]);
    assert_eq!(ends(&h.sent)[0].end_reason, Some(EndReason::Skipped));
    assert_eq!(ends(&h.sent)[0].position_millis, 238_000);
}

#[test]
fn seek_stays_in_the_same_session_previous_restart_is_a_seek() {
    let mut h = Harness::new();
    h.play_to(track_a(), 30);
    h.act(|a| {
        a.on_progress(120.0);
        a.on_seek(120.0);
    });
    h.advance(5000);
    // "Previous" after >10s only seeks to 0.
    h.act(|a| a.on_seek(0.0));

    assert_eq!(types(&h.sent), ["start:1", "seek:1", "seek:1"]);
    assert_eq!(h.sent[1].position_millis, 120_000);
    assert_eq!(h.sent[2].position_millis, 0);
    assert!(h.sent.iter().all(|e| e.session_id == h.sent[0].session_id));
    let seqs: Vec<u64> = h.sent.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [1, 2, 3]);
}

#[test]
fn pause_then_resume_reports_both_transitions_at_the_right_positions() {
    let mut h = Harness::new();
    h.play_to(track_a(), 10);
    h.act(|a| a.on_status(AdapterStatus::Paused));
    h.act(|a| a.on_status(AdapterStatus::Playing));

    assert_eq!(types(&h.sent), ["start:1", "pause:1", "resume:1"]);
    assert_eq!(h.sent[1].position_millis, 10_000);
    assert_eq!(h.sent[2].position_millis, 10_000);
}

#[test]
fn heartbeat_cadence_15s_playing_60s_paused_with_the_live_position() {
    let mut h = Harness::new();
    h.play_to(track_a(), 20);
    assert_eq!(h.timer_delays(), [15_000]);
    h.run_timers();
    let hb = h.last();
    assert_eq!(hb.kind, EventType::Heartbeat);
    assert_eq!(hb.state, Some(PlaybackState::Playing));
    assert_eq!(hb.position_millis, 20_000);

    h.act(|a| a.on_status(AdapterStatus::Paused));
    assert_eq!(h.timer_delays(), [60_000]);
}

#[test]
fn repeat_one_via_mpv_counts_as_a_new_play() {
    let mut h = Harness::new();
    h.play_to(track_a(), 239);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_repeated();
    });

    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:1"]);
    assert_eq!(ends(&h.sent)[0].end_reason, Some(EndReason::Completed));
    assert_eq!(ends(&h.sent)[0].position_millis, 239_000);
    let restart = &h.sent[2];
    assert_ne!(restart.session_id, h.sent[0].session_id);
    assert_eq!(restart.resumed_from_session_id, None);
    assert_eq!(restart.position_millis, 0);
}

#[test]
fn repeat_one_via_looping_engine_is_detected_from_the_playhead_wrapping() {
    let mut h = Harness::new();
    h.play_to(track_a(), 239);
    h.advance(1000);
    h.act(|a| a.on_progress(0.4));
    h.advance(1000);
    h.act(|a| a.on_progress(1.4));

    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:1"]);
    assert_eq!(ends(&h.sent)[0].end_reason, Some(EndReason::Completed));
    // A later tick does not restart again.
    h.advance(1000);
    h.act(|a| a.on_progress(2.4));
    assert_eq!(h.sent.len(), 3);
}

#[test]
fn a_backward_seek_near_the_end_is_not_mistaken_for_a_loop() {
    let mut h = Harness::new();
    h.play_to(track_a(), 235);
    h.act(|a| {
        a.on_progress(1.0);
        a.on_seek(1.0);
    });
    h.advance(1000);
    h.act(|a| a.on_progress(2.0));
    assert_eq!(types(&h.sent), ["start:1", "seek:1"]);
}

#[test]
fn buffering_reports_buffering_and_returns_to_playing() {
    let mut h = Harness::new();
    h.play_to(track_a(), 5);
    h.adapter.on_buffering(true);
    h.idle();
    h.adapter.on_buffering(false);
    h.idle();
    assert_eq!(types(&h.sent), ["start:1", "buffering:1", "resume:1"]);
    assert_eq!(h.sent[1].position_millis, 5_000);
}

#[test]
fn a_track_that_starts_while_buffering_starts_in_buffering_state() {
    let mut h = Harness::new();
    h.act(|a| a.on_track_change(Some(track_a())));
    h.adapter.on_buffering(true);
    h.act(|a| a.on_status(AdapterStatus::Playing));
    assert_eq!(h.sent[0].kind, EventType::Start);
    assert_eq!(h.sent[0].state, Some(PlaybackState::Buffering));
}

#[test]
fn playback_failure_ends_with_error_retrying_the_same_track_resumes_the_chain() {
    let mut h = Harness::new();
    h.play_to(track_a(), 50);
    h.adapter.on_error();
    h.idle();
    h.act(|a| a.on_status(AdapterStatus::Paused));
    h.act(|a| a.on_status(AdapterStatus::Playing));

    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:1"]);
    assert_eq!(ends(&h.sent)[0].end_reason, Some(EndReason::Error));
    assert_eq!(ends(&h.sent)[0].position_millis, 50_000);
    assert_eq!(
        h.sent[2].resumed_from_session_id.as_deref(),
        Some(h.sent[0].session_id.as_str())
    );
    assert_eq!(h.sent[2].position_millis, 50_000);
}

#[test]
fn decode_failure_that_auto_skips_error_end_then_a_fresh_session_for_the_next_track() {
    let mut h = Harness::new();
    h.play_to(track_a(), 3);
    h.adapter.on_error();
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_b()));
    });
    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:2"]);
    assert_eq!(ends(&h.sent).len(), 1);
    assert_eq!(ends(&h.sent)[0].end_reason, Some(EndReason::Error));
    assert_eq!(h.sent[2].resumed_from_session_id, None);
}

#[test]
fn app_close_sends_client_closed_via_keepalive_next_launch_resumes_without_double_counting() {
    let storage = MemoryStorage::new();
    let mut h = Harness::with(Opts {
        storage: Some(storage.clone()),
        ..Opts::default()
    });
    h.play_to(track_a(), 150);
    h.adapter.on_close();
    h.idle();

    assert_eq!(h.keepalive.len(), 1);
    let closed = &h.keepalive[0];
    assert_eq!(closed.kind, EventType::End);
    assert_eq!(closed.end_reason, Some(EndReason::ClientClosed));
    assert_eq!(closed.position_millis, 150_000);
    assert_eq!(closed.session_id, h.sent[0].session_id);
    assert_eq!(closed.seq, h.last().seq + 1);

    // Restart: same persistent storage, restored queue.
    let mut h2 = Harness::with(Opts {
        storage: Some(storage),
        ..Opts::default()
    });
    // The restored track is applied first, then the restored position.
    h2.act(|a| {
        a.on_queue_restored();
        a.on_track_change(Some(track_a()));
    });
    h2.act(|a| a.on_progress(150.0));
    h2.act(|a| a.on_status(AdapterStatus::Playing));
    assert_eq!(h2.sent[0].kind, EventType::Start);
    assert_eq!(
        h2.sent[0].resumed_from_session_id.as_deref(),
        Some(h.sent[0].session_id.as_str())
    );
    assert_eq!(h2.sent[0].position_millis, 150_000);
    assert_eq!(h2.sent[0].client_id, h.sent[0].client_id);
}

#[test]
fn end_of_queue_single_track_stop_and_reset_still_ends_completed_at_the_real_position() {
    let mut h = Harness::new();
    h.play_to(track_a(), 239);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_status(AdapterStatus::Stopped);
        a.on_seek(0.0);
    });
    assert_eq!(types(&h.sent), ["start:1", "end:1"]);
    assert_eq!(ends(&h.sent)[0].end_reason, Some(EndReason::Completed));
    assert_eq!(ends(&h.sent)[0].position_millis, 239_000);
    // Play again from the start: a new play at position 0.
    h.act(|a| a.on_status(AdapterStatus::Playing));
    assert_eq!(h.sent[2].kind, EventType::Start);
    assert_eq!(h.sent[2].position_millis, 0);
    assert_eq!(h.sent[2].resumed_from_session_id, None);
}

#[test]
fn explicit_stop_mid_track_ends_with_stopped() {
    let mut h = Harness::new();
    h.play_to(track_a(), 60);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_status(AdapterStatus::Stopped);
    });
    assert_eq!(ends(&h.sent)[0].end_reason, Some(EndReason::Stopped));
    assert_eq!(ends(&h.sent)[0].position_millis, 60_000);
}

#[test]
fn browsing_tracks_while_paused_opens_no_sessions() {
    let mut h = Harness::new();
    h.act(|a| a.on_track_change(Some(track_a())));
    h.act(|a| a.on_track_change(Some(track_b())));
    h.act(|a| a.on_track_change(Some(track_a())));
    assert!(h.sent.is_empty());
}

#[test]
fn switching_to_a_non_032_song_ends_the_session_and_starts_nothing() {
    let mut h = Harness::new();
    h.play_to(track_a(), 20);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(None);
        a.on_user_navigate();
    });
    assert_eq!(types(&h.sent), ["start:1", "end:1"]);
    assert_eq!(ends(&h.sent)[0].position_millis, 20_000);
}

#[test]
fn session_expired_409_one_resume_with_resumed_from_then_the_event_is_resent() {
    let mut expired = false;
    let mut h = Harness::with(Opts {
        respond: Some(Box::new(move |body, _| {
            if body.kind == EventType::Heartbeat && !expired {
                expired = true;
                return Some(error_response(409, "session_expired"));
            }
            None
        })),
        ..Opts::default()
    });
    h.play_to(track_a(), 20);
    h.run_timers();

    let [first, heartbeat, recovery_start, retried] = &h.sent[..] else {
        panic!("expected 4 events, got {:?}", types(&h.sent));
    };
    assert_eq!(heartbeat.kind, EventType::Heartbeat);
    assert_eq!(recovery_start.kind, EventType::Start);
    assert_eq!(
        recovery_start.resumed_from_session_id.as_deref(),
        Some(first.session_id.as_str())
    );
    assert_eq!(recovery_start.state, Some(PlaybackState::Playing));
    assert_eq!(retried.kind, EventType::Heartbeat);
    assert_eq!(retried.session_id, recovery_start.session_id);
}

#[test]
fn resume_invalid_409_blocks_reporting_until_an_explicit_new_play() {
    let mut h = Harness::with(Opts {
        respond: Some(Box::new(|body, _| {
            body.resumed_from_session_id
                .is_some()
                .then(|| error_response(409, "resume_invalid"))
        })),
        ..Opts::default()
    });
    h.play_to(track_a(), 50);
    h.adapter.on_error();
    h.idle();
    h.act(|a| a.on_status(AdapterStatus::Paused));
    h.act(|a| a.on_status(AdapterStatus::Playing));
    assert!(h.adapter.session().is_blocked());
    let count = h.sent.len();
    h.act(|a| a.on_seek(10.0));
    assert_eq!(h.sent.len(), count, "blocked: no events");

    // An explicit new play lifts the block.
    h.act(|a| {
        a.on_track_change(Some(track_b()));
        a.on_user_navigate();
    });
    assert!(!h.adapter.session().is_blocked());
    assert_eq!(h.last().kind, EventType::Start);
    assert_eq!(h.last().track_id, 2);
}

#[test]
fn session_not_found_404_on_end_past_the_threshold_restores_the_play_with_start_and_end() {
    let mut h = Harness::with(Opts {
        respond: Some(Box::new(|body, sent| {
            let end_count = sent.iter().filter(|e| e.kind == EventType::End).count();
            (body.kind == EventType::End && end_count == 1)
                .then(|| error_response(404, "session_not_found"))
        })),
        ..Opts::default()
    });
    h.play_to(track_a(), 200);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_b()));
    });
    let recovered: Vec<PlaybackEventBody> =
        h.sent.iter().filter(|e| e.track_id == 1).cloned().collect();
    assert_eq!(types(&recovered), ["start:1", "end:1", "start:1", "end:1"]);
    assert_eq!(recovered[2].state, Some(PlaybackState::Playing));
    assert_eq!(recovered[2].position_millis, 200_000);
    // The recovery replays the original reason (200s of 240s is not near the end).
    assert_eq!(recovered[3].end_reason, Some(EndReason::Stopped));
}

#[test]
fn network_failure_on_end_gives_up_after_bounded_retries_without_blocking_the_next_track() {
    let mut h = Harness::with(Opts {
        respond: Some(Box::new(|body, _| {
            (body.kind == EventType::End).then(|| TransportOutcome::response(503, json!(null)))
        })),
        ..Opts::default()
    });
    h.play_to(track_a(), 30);
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_b()));
        a.on_user_navigate();
    });
    assert_eq!(types(&h.sent), ["start:1", "end:1", "start:2"]);
}

#[test]
fn daily_mix_context_rides_on_fresh_starts_only() {
    let mix_a = AdapterTrack {
        context: Some(PlaybackContext::daily_mix(7, 0)),
        ..track_a()
    };
    let mix_b = AdapterTrack {
        context: Some(PlaybackContext::daily_mix(7, 1)),
        ..track_b()
    };
    let mut h = Harness::new();
    h.play_to(mix_a, 50);
    // Playback failure, then a retry of the same track resumes the chain.
    h.adapter.on_error();
    h.idle();
    h.act(|a| a.on_status(AdapterStatus::Paused));
    h.act(|a| a.on_status(AdapterStatus::Playing));
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(mix_b));
        a.on_user_navigate();
    });
    h.act(|a| {
        a.on_progress(0.0);
        a.on_track_change(Some(track_a()));
        a.on_user_navigate();
    });

    let starts: Vec<&PlaybackEventBody> = h
        .sent
        .iter()
        .filter(|e| e.kind == EventType::Start)
        .collect();
    assert_eq!(starts[0].context, Some(PlaybackContext::daily_mix(7, 0)));
    assert_eq!(
        starts[1].resumed_from_session_id.as_deref(),
        Some(starts[0].session_id.as_str())
    );
    assert_eq!(starts[1].context, None);
    assert_eq!(starts[2].context, Some(PlaybackContext::daily_mix(7, 1)));
    assert_eq!(starts[3].context, None);
    assert!(
        h.sent
            .iter()
            .filter(|e| e.kind != EventType::Start)
            .all(|e| e.context.is_none())
    );
}

#[test]
fn an_invalid_daily_mix_context_is_dropped_instead_of_failing_the_start() {
    let mut h = Harness::new();
    h.play_to(
        AdapterTrack {
            context: Some(PlaybackContext::daily_mix(0, 3)),
            ..track_a()
        },
        1,
    );
    assert_eq!(h.sent[0].kind, EventType::Start);
    assert_eq!(h.sent[0].context, None);
}

// ---- Rust-only checks (not in the JS suite) --------------------------------

#[test]
fn event_body_serializes_like_the_reference() {
    let mut h = Harness::new();
    h.play_to(
        AdapterTrack {
            context: Some(PlaybackContext::daily_mix(7, 2)),
            ..track_a()
        },
        1,
    );
    let value = serde_json::to_value(&h.sent[0]).unwrap();
    assert_eq!(
        value,
        json!({
            "clientId": "00000000-0000-4000-8000-000000000002",
            "clientKind": "web",
            "context": { "id": 7, "position": 2, "type": "dailyMix" },
            "durationMillis": 240000,
            "positionMillis": 0,
            "seq": 1,
            "sessionId": "00000000-0000-4000-8000-000000000001",
            "state": "playing",
            "trackId": 1,
            "type": "start",
        })
    );
}

#[test]
fn network_errors_retry_with_the_configured_delays_and_the_same_body() {
    use session::{PlayOptions, PlaybackSession};

    let timers: Timers = Arc::default();
    let mut options = SessionOptions::new(
        Box::new(FakeScheduler {
            timers: timers.clone(),
            next: 0,
        }),
        Box::new(|| "id-1".to_owned()),
    )
    .with_client_kind("desktop");
    options.retry_delays_millis = vec![1000, 3000];
    let mut s = PlaybackSession::new(options);
    s.play(
        5,
        PlayOptions {
            initial_state: Some(PlaybackState::Playing),
            ..PlayOptions::default()
        },
    );
    let heartbeat_timer = *timers.lock().unwrap().keys().next().unwrap();
    let first = s.poll_transmit().unwrap();
    assert_eq!(first.body.client_kind, "desktop");
    s.handle_outcome(first.id, TransportOutcome::NetworkError);
    assert!(s.poll_transmit().is_none(), "waits for the retry timer");
    let retry_timer = *timers
        .lock()
        .unwrap()
        .iter()
        .find(|(id, ms)| **id != heartbeat_timer && **ms == 1000)
        .unwrap()
        .0;
    s.on_timer(retry_timer);
    let second = s.poll_transmit().unwrap();
    assert_eq!(second.body, first.body);
    s.handle_outcome(second.id, TransportOutcome::response(502, json!(null)));
    assert!(timers.lock().unwrap().values().any(|ms| *ms == 3000));
}

#[test]
fn late_resume_invalid_for_old_track_does_not_block_new_track() {
    use session::{PlayOptions, PlaybackSession};

    let timers: Timers = Arc::default();
    let mut seq = 0u64;
    let mut options = SessionOptions::new(
        Box::new(FakeScheduler {
            timers: timers.clone(),
            next: 0,
        }),
        Box::new(move || {
            seq += 1;
            format!("00000000-0000-4000-8000-{seq:012}")
        }),
    );
    options.retry_delays_millis = vec![];
    options.storage = Some(Box::new(MemoryStorage::new()));
    let mut s = PlaybackSession::new(options);

    // Start track 1 and confirm it.
    s.play(
        1,
        PlayOptions {
            initial_state: Some(PlaybackState::Playing),
            duration_millis: Some(200_000),
            ..PlayOptions::default()
        },
    );
    let tx_start_1 = s.poll_transmit().unwrap();
    s.handle_outcome(
        tx_start_1.id,
        TransportOutcome::response(200, json!({ "applied": true, "state": "playing" })),
    );

    // Switch to track 2; End(1) is now inflight while Start(2) is queued behind it.
    s.play(
        2,
        PlayOptions {
            initial_state: Some(PlaybackState::Playing),
            duration_millis: Some(180_000),
            ..PlayOptions::default()
        },
    );
    let tx_end_1 = s.poll_transmit().unwrap();
    assert_eq!(tx_end_1.body.kind, EventType::End);
    assert_eq!(tx_end_1.body.track_id, 1);

    // Server says track 1 expired, triggering recovery Start(resumeFrom=1) + End(1).
    s.handle_outcome(tx_end_1.id, error_response(409, "session_expired"));
    let tx_rec_start_1 = s.poll_transmit().unwrap();
    assert_eq!(tx_rec_start_1.body.kind, EventType::Start);
    assert_eq!(tx_rec_start_1.body.track_id, 1);
    assert!(tx_rec_start_1.body.resumed_from_session_id.is_some());

    // Server rejects old track 1's recovery with 409 resume_invalid.
    s.handle_outcome(tx_rec_start_1.id, error_response(409, "resume_invalid"));

    // Track 2 must NOT be blocked; its Start and subsequent Pause must still transmit.
    assert!(!s.is_blocked());
    let tx_start_2 = s.poll_transmit().unwrap();
    assert_eq!(tx_start_2.body.kind, EventType::Start);
    assert_eq!(tx_start_2.body.track_id, 2);
    s.handle_outcome(
        tx_start_2.id,
        TransportOutcome::response(200, json!({ "applied": true, "state": "playing" })),
    );
    s.notify_paused(Some(10_000));
    let tx_pause_2 = s.poll_transmit().unwrap();
    assert_eq!(tx_pause_2.body.kind, EventType::Pause);
    assert_eq!(tx_pause_2.body.track_id, 2);
}

#[test]
fn idempotent_end_response_preserves_error_snapshot_and_does_not_block() {
    let mut h = Harness::with(Opts {
        respond: Some(Box::new(|body, _| {
            (body.kind == EventType::End).then(|| {
                TransportOutcome::response(200, json!({ "applied": false, "state": "ended" }))
            })
        })),
        ..Opts::default()
    });
    h.play_to(track_a(), 50);
    let original_id = h.sent[0].session_id.clone();
    h.adapter.on_error();
    h.idle();
    assert!(!h.adapter.session().is_blocked());
    assert!(h.adapter.session_mut().poll_protocol_error().is_none());

    // Retrying the same track still resumes the persisted chain.
    h.act(|a| a.on_status(AdapterStatus::Paused));
    h.act(|a| a.on_status(AdapterStatus::Playing));
    assert_eq!(h.last().kind, EventType::Start);
    assert_eq!(
        h.last().resumed_from_session_id.as_deref(),
        Some(original_id.as_str())
    );
}

#[test]
fn expired_recovery_does_not_persist_unconfirmed_session_and_missing_recovery_stops_heartbeat() {
    use session::{KeyValueStorage, PlayOptions, PlaybackSession};

    let storage = MemoryStorage::new();
    let timers: Timers = Arc::default();
    let mut seq = 0u64;
    let mut options = SessionOptions::new(
        Box::new(FakeScheduler {
            timers: timers.clone(),
            next: 0,
        }),
        Box::new(move || {
            seq += 1;
            format!("00000000-0000-4000-8000-{seq:012}")
        }),
    );
    options.retry_delays_millis = vec![1000];
    options.storage = Some(Box::new(storage.clone()));
    let mut s = PlaybackSession::new(options);

    s.play(
        1,
        PlayOptions {
            initial_state: Some(PlaybackState::Playing),
            ..PlayOptions::default()
        },
    );
    let tx_start = s.poll_transmit().unwrap();
    let initial_session_id = tx_start.body.session_id.clone();
    s.handle_outcome(
        tx_start.id,
        TransportOutcome::response(200, json!({ "applied": true, "state": "playing" })),
    );
    assert!(
        storage
            .get("032_playback_session_v1")
            .unwrap()
            .contains(&initial_session_id)
    );

    // Pause triggers 409 session_expired: snapshot must stay on initial_session_id until
    // the recovery Start is actually confirmed by the server.
    s.notify_paused(Some(10_000));
    let tx_pause = s.poll_transmit().unwrap();
    s.handle_outcome(tx_pause.id, error_response(409, "session_expired"));
    assert!(
        storage
            .get("032_playback_session_v1")
            .unwrap()
            .contains(&initial_session_id),
        "unconfirmed recovery session must not overwrite snapshot"
    );

    let tx_rec_start = s.poll_transmit().unwrap();
    assert_eq!(tx_rec_start.body.kind, EventType::Start);
    assert_ne!(tx_rec_start.body.session_id, initial_session_id);
    s.handle_outcome(
        tx_rec_start.id,
        TransportOutcome::response(200, json!({ "applied": true, "state": "paused" })),
    );
    assert!(
        storage
            .get("032_playback_session_v1")
            .unwrap()
            .contains(&tx_rec_start.body.session_id),
        "confirmed recovery session must now be persisted"
    );

    // Replayed pause gets 404 session_not_found -> pushes Start(depth+1); if that also
    // fails with 404 session_not_found, session finalizes locally and cancels heartbeat.
    let tx_pause_retry = s.poll_transmit().unwrap();
    s.handle_outcome(tx_pause_retry.id, error_response(404, "session_not_found"));
    let tx_missing_start = s.poll_transmit().unwrap();
    assert_eq!(tx_missing_start.body.kind, EventType::Start);
    s.handle_outcome(
        tx_missing_start.id,
        error_response(404, "session_not_found"),
    );
    assert!(s.is_blocked());
    assert!(timers.lock().unwrap().is_empty(), "heartbeat must stop");
    assert_eq!(
        s.poll_protocol_error().unwrap().reason,
        "missing_recovery_failed"
    );

    // dispose() cancels pending WaitingRetry timers and clears inflight + keepalive.
    s.play(
        2,
        PlayOptions {
            initial_state: Some(PlaybackState::Playing),
            ..PlayOptions::default()
        },
    );
    let tx2 = s.poll_transmit().unwrap();
    s.handle_outcome(tx2.id, TransportOutcome::NetworkError);
    assert!(
        timers.lock().unwrap().values().any(|&ms| ms == 1000),
        "retry timer scheduled"
    );
    s.suspend(session::EventPosition::default());
    s.dispose();
    assert!(timers.lock().unwrap().is_empty(), "all timers cancelled");
    assert!(s.is_idle());
    assert!(s.poll_transmit().is_none());
}

#[test]
fn adapter_resets_buffering_on_error_stopped_and_repeat_one() {
    let mut h = Harness::new();
    h.play_to(track_a(), 40);

    // 1. Error while buffering must not leave the retry starting in Buffering state.
    h.adapter.on_buffering(true);
    h.idle();
    h.adapter.on_error();
    h.idle();
    h.act(|a| a.on_status(AdapterStatus::Paused));
    h.act(|a| a.on_status(AdapterStatus::Playing));
    assert_eq!(h.last().kind, EventType::Start);
    assert_eq!(h.last().state, Some(PlaybackState::Playing));

    // 2. Stopped while buffering must not leave the next start in Buffering state.
    h.adapter.on_buffering(true);
    h.idle();
    h.act(|a| a.on_status(AdapterStatus::Stopped));
    h.act(|a| a.on_status(AdapterStatus::Playing));
    assert_eq!(h.last().kind, EventType::Start);
    assert_eq!(h.last().state, Some(PlaybackState::Playing));

    // 3. Repeat-one while buffering must start the next pass in Playing state.
    h.adapter.on_buffering(true);
    h.idle();
    h.act(|a| a.on_repeated());
    assert_eq!(h.last().kind, EventType::Start);
    assert_eq!(h.last().state, Some(PlaybackState::Playing));
}
