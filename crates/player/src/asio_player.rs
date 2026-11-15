//! Pure-Rust ASIO playback engine (`reqwest` + `symphonia` + `asio`).
//!
//! Matches `Player` (`libmpv`) semantics (`PlayerEvent`, gapless `append`,
//! software volume + fade gain, HTTP proxy, reconnect, seek, buffering).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use asio::SpscRing;

use crate::decoder::{DecodeError, TrackDecoder};
use crate::http_source::{HttpMediaSource, build_http_client};
use crate::{EndReason, PlayerError, PlayerEvent, Result};

/// Ring capacity in stereo `f32` samples (524,288 frames ≈ 11.8 s @ 44.1 kHz).
const RING_SAMPLES: usize = 1_048_576;
const TIME_POS_STEP_MS: u128 = 80;

pub type RenderFn = Box<dyn FnMut(usize, &mut [f32]) + Send + 'static>;

/// Abstraction over the audio hardware sink so `AsioPlayer` can be unit-tested
/// without a physical ASIO driver.
pub trait AudioSink: Send + 'static {
    fn start(&mut self, sample_rate: f64, render: RenderFn) -> Result<()>;
    fn set_sample_rate(&mut self, sample_rate: f64) -> Result<()>;
    fn stop(&mut self);
    fn rendered_frames(&self) -> u64;
    fn output_latency_frames(&self) -> u64;
    fn open_control_panel(&self) -> Result<()>;
}

#[cfg(windows)]
pub struct WindowsAsioSink {
    clsid: String,
    active: Option<asio::AsioOutput>,
}

#[cfg(windows)]
impl WindowsAsioSink {
    #[must_use]
    pub fn new(clsid: impl Into<String>) -> Self {
        Self {
            clsid: clsid.into(),
            active: None,
        }
    }
}

#[cfg(windows)]
impl AudioSink for WindowsAsioSink {
    fn start(&mut self, sample_rate: f64, mut render: RenderFn) -> Result<()> {
        self.active = None;
        let out = asio::AsioOutput::start(
            &self.clsid,
            sample_rate,
            Box::new(move |ctx, buf| render(ctx.frames, buf)),
        )
        .map_err(|e| PlayerError::Mpv(e.to_string()))?;
        self.active = Some(out);
        Ok(())
    }

    fn set_sample_rate(&mut self, sample_rate: f64) -> Result<()> {
        let Some(active) = self.active.as_ref() else {
            return Err(PlayerError::Mpv("ASIO output is not active".into()));
        };
        active
            .set_sample_rate(sample_rate)
            .map(|_| ())
            .map_err(|e| PlayerError::Mpv(e.to_string()))
    }

    fn stop(&mut self) {
        self.active = None;
    }

    fn rendered_frames(&self) -> u64 {
        self.active
            .as_ref()
            .map_or(0, asio::AsioOutput::rendered_frames)
    }

    fn output_latency_frames(&self) -> u64 {
        self.active
            .as_ref()
            .map_or(0, asio::AsioOutput::output_latency_frames)
    }

    fn open_control_panel(&self) -> Result<()> {
        if let Some(active) = self.active.as_ref() {
            active
                .open_control_panel()
                .map_err(|e| PlayerError::Mpv(e.to_string()))
        } else {
            asio::open_control_panel_standalone(&self.clsid)
                .map_err(|e| PlayerError::Mpv(e.to_string()))
        }
    }
}

struct SharedState {
    ring: SpscRing,
    paused: AtomicBool,
    muted: AtomicBool,
    /// When true, callback outputs silence without touching `ring`.
    seeking: AtomicBool,
    /// Producer finished decoding and no same-rate gapless track follows.
    drain_eof: AtomicBool,
    /// Consumer drained `ring` completely after `drain_eof`.
    drained_eof_reached: AtomicBool,
    /// Set by consumer when `ring` underruns while `drain_eof == false`.
    underrun: AtomicBool,
    volume_bits: AtomicU64,
    fade_bits: AtomicU64,
    /// Total stereo frames popped from `ring` and handed to hardware.
    consumed_frames: AtomicU64,
    /// Cumulative consumed frame index at which the current track started.
    track_start_consumed: AtomicU64,
    /// Cumulative pushed frame index where the appended same-rate track begins (`u64::MAX` = none).
    next_boundary_frame: AtomicU64,
    boundary_crossed: AtomicBool,
}

impl SharedState {
    fn new(volume: f64) -> Self {
        Self {
            ring: SpscRing::new(RING_SAMPLES),
            paused: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            seeking: AtomicBool::new(false),
            drain_eof: AtomicBool::new(false),
            drained_eof_reached: AtomicBool::new(false),
            underrun: AtomicBool::new(false),
            volume_bits: AtomicU64::new(volume.clamp(0.0, 100.0).to_bits()),
            fade_bits: AtomicU64::new(1.0_f64.to_bits()),
            consumed_frames: AtomicU64::new(0),
            track_start_consumed: AtomicU64::new(0),
            next_boundary_frame: AtomicU64::new(u64::MAX),
            boundary_crossed: AtomicBool::new(false),
        }
    }

    fn target_amp(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            return 0.0;
        }
        let vol = (f64::from_bits(self.volume_bits.load(Ordering::Relaxed)) / 100.0)
            .clamp(0.0, 1.0)
            .powi(3);
        let fade = f64::from_bits(self.fade_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let amp = (vol * fade) as f32;
        amp
    }
}

enum Cmd {
    Load(String),
    Append(String),
    ClearUpcoming,
    Stop,
    SetPaused(bool),
    Seek(f64),
    OpenControlPanel(mpsc::SyncSender<Result<()>>),
    Shutdown,
}

pub struct AsioPlayer {
    cmd_tx: mpsc::Sender<Cmd>,
    shared: Arc<SharedState>,
    proxy: Arc<Mutex<Option<String>>>,
    cancel_load: Arc<Mutex<Arc<AtomicBool>>>,
    thread: Option<JoinHandle<()>>,
}

impl AsioPlayer {
    pub fn new<S: AudioSink>(
        sink: S,
        volume: f64,
        on_event: impl FnMut(PlayerEvent) + Send + 'static,
    ) -> Result<Self> {
        let shared = Arc::new(SharedState::new(volume));
        let proxy = Arc::new(Mutex::new(None));
        let cancel_load = Arc::new(Mutex::new(Arc::new(AtomicBool::new(false))));
        let (cmd_tx, cmd_rx) = mpsc::channel();

        let shared_w = shared.clone();
        let proxy_w = proxy.clone();
        let cancel_w = cancel_load.clone();
        let thread = thread::Builder::new()
            .name("asio-player".into())
            .spawn(move || {
                worker_loop(
                    sink,
                    shared_w,
                    proxy_w,
                    cancel_w,
                    cmd_rx,
                    Box::new(on_event),
                );
            })
            .map_err(|e| PlayerError::Mpv(e.to_string()))?;

        Ok(Self {
            cmd_tx,
            shared,
            proxy,
            cancel_load,
            thread: Some(thread),
        })
    }

    pub fn load(&self, url: &str) -> Result<()> {
        self.abort_active_io();
        self.cmd_tx
            .send(Cmd::Load(url.to_owned()))
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))
    }

    pub fn append(&self, url: &str) -> Result<()> {
        self.cmd_tx
            .send(Cmd::Append(url.to_owned()))
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))
    }

    pub fn clear_upcoming(&self) -> Result<()> {
        self.cmd_tx
            .send(Cmd::ClearUpcoming)
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))
    }

    pub fn stop(&self) -> Result<()> {
        self.abort_active_io();
        self.cmd_tx
            .send(Cmd::Stop)
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.cmd_tx
            .send(Cmd::SetPaused(paused))
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))
    }

    pub fn seek(&self, seconds: f64) -> Result<()> {
        self.cmd_tx
            .send(Cmd::Seek(seconds))
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))
    }

    pub fn set_volume(&self, volume: f64) {
        self.shared
            .volume_bits
            .store(volume.clamp(0.0, 100.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_fade_gain(&self, gain: f64) {
        self.shared
            .fade_bits
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_http_proxy(&self, proxy: Option<&str>) {
        *self.proxy.lock().expect("proxy lock") = proxy.map(ToOwned::to_owned);
    }

    pub fn open_control_panel(&self) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.cmd_tx
            .send(Cmd::OpenControlPanel(tx))
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))?;
        rx.recv()
            .map_err(|_| PlayerError::Mpv("ASIO worker stopped".into()))?
    }

    fn abort_active_io(&self) {
        self.cancel_load
            .lock()
            .expect("cancel lock")
            .store(true, Ordering::Release);
    }
}

impl Drop for AsioPlayer {
    fn drop(&mut self) {
        self.abort_active_io();
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct PrefetchedTrack {
    url: String,
    decoder: TrackDecoder,
    predecoded: Vec<f32>,
    eof_in_predecode: bool,
    sample_rate: u32,
    duration_secs: Option<f64>,
}

struct PrefetchHandle {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<std::result::Result<PrefetchedTrack, DecodeError>>,
}

struct ActiveTrack {
    url: String,
    decoder: TrackDecoder,
    sample_rate: u32,
    duration_secs: Option<f64>,
    seek_offset_secs: f64,
    pushed_frames: u64,
    eof_decoded: bool,
    pending_pcm: Vec<f32>,
    pending_offset: usize,
}

fn spawn_prefetch(
    url: String,
    proxy: Option<String>,
    rt: tokio::runtime::Handle,
) -> PrefetchHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_w = cancel.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("asio-prefetch".into())
        .spawn(move || {
            let res = open_and_predecode(&url, proxy.as_deref(), rt, cancel_w);
            let _ = tx.send(res);
        })
        .ok();
    PrefetchHandle { cancel, rx }
}

fn open_and_predecode(
    url: &str,
    proxy: Option<&str>,
    rt: tokio::runtime::Handle,
    cancel: Arc<AtomicBool>,
) -> std::result::Result<PrefetchedTrack, DecodeError> {
    let client = build_http_client(proxy).map_err(|e| DecodeError::Other(e.to_string()))?;
    let source = HttpMediaSource::open(client, url, rt, cancel)
        .map_err(|e| DecodeError::Other(e.to_string()))?;
    let mime = source.content_type().map(ToOwned::to_owned);
    let mut decoder = TrackDecoder::open(Box::new(source), Some(url), mime.as_deref())?;
    let sample_rate = decoder.sample_rate();
    let duration_secs = decoder.duration_secs();

    let target_samples = usize::try_from(sample_rate).unwrap_or(44_100) * 2; // 1 second
    let mut predecoded = Vec::with_capacity(target_samples);
    let mut pkt = Vec::new();
    let mut eof_in_predecode = false;
    while predecoded.len() < target_samples {
        match decoder.next_packet_stereo(&mut pkt) {
            Ok(true) => predecoded.extend_from_slice(&pkt),
            Ok(false) => {
                eof_in_predecode = true;
                break;
            }
            Err(e) => return Err(e),
        }
    }

    Ok(PrefetchedTrack {
        url: url.to_owned(),
        decoder,
        predecoded,
        eof_in_predecode,
        sample_rate,
        duration_secs,
    })
}

fn make_render_fn(shared: Arc<SharedState>) -> RenderFn {
    let mut current_gain: Option<f32> = None;
    Box::new(move |frames: usize, out_lr: &mut [f32]| {
        let needed = frames * 2;
        if shared.seeking.load(Ordering::Acquire) || shared.paused.load(Ordering::Acquire) {
            out_lr[..needed].fill(0.0);
            return;
        }

        let got = shared.ring.pop(&mut out_lr[..needed]);
        let got_frames = got / 2;
        let target_amp = shared.target_amp();
        let prev = current_gain.unwrap_or(target_amp);
        let ramp_samples = got.min(128);
        if (prev - target_amp).abs() > 1e-6 && ramp_samples > 0 {
            let denom = ramp_samples as f32;
            for (i, sample) in out_lr[..got].iter_mut().enumerate() {
                let gain = if i < ramp_samples {
                    prev + (target_amp - prev) * ((i + 1) as f32 / denom)
                } else {
                    target_amp
                };
                *sample *= gain;
            }
        } else if (target_amp - 1.0).abs() > 1e-6 {
            for sample in &mut out_lr[..got] {
                *sample *= target_amp;
            }
        }
        current_gain = Some(target_amp);

        if got < needed {
            out_lr[got..needed].fill(0.0);
            if got == 0 && shared.drain_eof.load(Ordering::Acquire) {
                shared.drained_eof_reached.store(true, Ordering::Release);
            } else if !shared.drain_eof.load(Ordering::Acquire) {
                shared.underrun.store(true, Ordering::Release);
            }
        }

        if got_frames > 0 {
            let new_consumed = shared
                .consumed_frames
                .fetch_add(got_frames as u64, Ordering::AcqRel)
                + got_frames as u64;
            let boundary = shared.next_boundary_frame.load(Ordering::Acquire);
            if new_consumed >= boundary {
                shared.boundary_crossed.store(true, Ordering::Release);
            }
        }
    })
}

fn worker_loop<S: AudioSink>(
    mut sink: S,
    shared: Arc<SharedState>,
    proxy: Arc<Mutex<Option<String>>>,
    cancel_load: Arc<Mutex<Arc<AtomicBool>>>,
    cmd_rx: mpsc::Receiver<Cmd>,
    mut emit: Box<dyn FnMut(PlayerEvent) + Send + 'static>,
) {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            emit(PlayerEvent::EndFile {
                reason: EndReason::Error,
                error: Some(format!("Tokio init failed: {e}")),
            });
            return;
        }
    };

    let mut sink_rate: Option<u32> = None;
    let mut active: Option<ActiveTrack> = None;
    let mut upcoming_url: Option<String> = None;
    let mut prefetch: Option<PrefetchHandle> = None;
    let mut prefetched_ready: Option<PrefetchedTrack> = None;
    let mut prefetch_error: Option<String> = None;
    let mut buffering_reported = false;
    let mut last_pos_emit = Instant::now();
    let mut packet_buf = Vec::new();

    let clear_upcoming = |upcoming_url: &mut Option<String>,
                          prefetch: &mut Option<PrefetchHandle>,
                          prefetched_ready: &mut Option<PrefetchedTrack>,
                          prefetch_error: &mut Option<String>| {
        *upcoming_url = None;
        if let Some(p) = prefetch.take() {
            p.cancel.store(true, Ordering::Release);
        }
        *prefetched_ready = None;
        *prefetch_error = None;
    };

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Shutdown => {
                    clear_upcoming(
                        &mut upcoming_url,
                        &mut prefetch,
                        &mut prefetched_ready,
                        &mut prefetch_error,
                    );
                    sink.stop();
                    return;
                }
                Cmd::Stop => {
                    clear_upcoming(
                        &mut upcoming_url,
                        &mut prefetch,
                        &mut prefetched_ready,
                        &mut prefetch_error,
                    );
                    let was_active = active.take().is_some();
                    shared.seeking.store(true, Ordering::Release);
                    shared.ring.clear();
                    shared.drain_eof.store(false, Ordering::Release);
                    shared.drained_eof_reached.store(false, Ordering::Release);
                    shared
                        .next_boundary_frame
                        .store(u64::MAX, Ordering::Release);
                    shared.boundary_crossed.store(false, Ordering::Release);
                    shared.seeking.store(false, Ordering::Release);
                    sink.stop();
                    sink_rate = None;
                    if buffering_reported {
                        buffering_reported = false;
                        emit(PlayerEvent::Buffering(false));
                    }
                    if was_active {
                        emit(PlayerEvent::EndFile {
                            reason: EndReason::Stop,
                            error: None,
                        });
                    }
                }
                Cmd::ClearUpcoming => {
                    clear_upcoming(
                        &mut upcoming_url,
                        &mut prefetch,
                        &mut prefetched_ready,
                        &mut prefetch_error,
                    );
                }
                Cmd::Append(url) => {
                    clear_upcoming(
                        &mut upcoming_url,
                        &mut prefetch,
                        &mut prefetched_ready,
                        &mut prefetch_error,
                    );
                    upcoming_url = Some(url.clone());
                    let p = proxy.lock().expect("proxy lock").clone();
                    prefetch = Some(spawn_prefetch(url, p, rt.handle().clone()));
                }
                Cmd::SetPaused(paused) => {
                    shared.paused.store(paused, Ordering::Release);
                    emit(PlayerEvent::Paused(paused));
                }
                Cmd::OpenControlPanel(reply) => {
                    let _ = reply.send(sink.open_control_panel());
                }
                Cmd::Seek(seconds) => {
                    if let Some(track) = active.as_mut() {
                        shared.seeking.store(true, Ordering::Release);
                        thread::sleep(Duration::from_millis(15));
                        shared.ring.clear();
                        shared.drain_eof.store(false, Ordering::Release);
                        shared.drained_eof_reached.store(false, Ordering::Release);
                        shared
                            .next_boundary_frame
                            .store(u64::MAX, Ordering::Release);
                        shared.boundary_crossed.store(false, Ordering::Release);
                        shared.underrun.store(false, Ordering::Release);
                        track.pending_pcm.clear();
                        track.pending_offset = 0;
                        track.eof_decoded = false;
                        match track.decoder.seek(seconds) {
                            Ok(landed) => {
                                track.seek_offset_secs = landed;
                                track.pushed_frames = 0;
                                let consumed = shared.consumed_frames.load(Ordering::Acquire);
                                shared
                                    .track_start_consumed
                                    .store(consumed, Ordering::Release);
                                shared.seeking.store(false, Ordering::Release);
                                emit(PlayerEvent::PlaybackRestart);
                                emit(PlayerEvent::TimePos(landed));
                            }
                            Err(DecodeError::Cancelled) => {
                                shared.seeking.store(false, Ordering::Release);
                            }
                            Err(e) => {
                                shared.seeking.store(false, Ordering::Release);
                                tracing::warn!("ASIO seek failed: {e}");
                            }
                        }
                    }
                }
                Cmd::Load(url) => {
                    clear_upcoming(
                        &mut upcoming_url,
                        &mut prefetch,
                        &mut prefetched_ready,
                        &mut prefetch_error,
                    );
                    let was_active = active.take().is_some();
                    shared.seeking.store(true, Ordering::Release);
                    shared.ring.clear();
                    shared.drain_eof.store(false, Ordering::Release);
                    shared.drained_eof_reached.store(false, Ordering::Release);
                    shared
                        .next_boundary_frame
                        .store(u64::MAX, Ordering::Release);
                    shared.boundary_crossed.store(false, Ordering::Release);
                    shared.underrun.store(false, Ordering::Release);
                    if was_active {
                        emit(PlayerEvent::EndFile {
                            reason: EndReason::Stop,
                            error: None,
                        });
                    }

                    let cancel = Arc::new(AtomicBool::new(false));
                    *cancel_load.lock().expect("cancel lock") = cancel.clone();
                    let p = proxy.lock().expect("proxy lock").clone();
                    emit(PlayerEvent::Buffering(true));
                    buffering_reported = true;

                    match open_and_predecode(&url, p.as_deref(), rt.handle().clone(), cancel) {
                        Ok(opened) => {
                            let target_rate = opened.sample_rate;
                            let sink_res = match sink_rate {
                                None => sink
                                    .start(f64::from(target_rate), make_render_fn(shared.clone())),
                                Some(r) if r != target_rate => {
                                    sink.set_sample_rate(f64::from(target_rate))
                                }
                                Some(_) => Ok(()),
                            };
                            if let Err(e) = sink_res {
                                sink.stop();
                                sink_rate = None;
                                shared.seeking.store(false, Ordering::Release);
                                buffering_reported = false;
                                emit(PlayerEvent::Buffering(false));
                                emit(PlayerEvent::EndFile {
                                    reason: EndReason::Error,
                                    error: Some(e.to_string()),
                                });
                                continue;
                            }
                            sink_rate = Some(target_rate);

                            let mut track = ActiveTrack {
                                url: opened.url,
                                decoder: opened.decoder,
                                sample_rate: target_rate,
                                duration_secs: opened.duration_secs,
                                seek_offset_secs: 0.0,
                                pushed_frames: 0,
                                eof_decoded: opened.eof_in_predecode,
                                pending_pcm: opened.predecoded,
                                pending_offset: 0,
                            };
                            push_pending(&mut track, &shared.ring);
                            let consumed = shared.consumed_frames.load(Ordering::Acquire);
                            shared
                                .track_start_consumed
                                .store(consumed, Ordering::Release);
                            shared.paused.store(false, Ordering::Release);
                            shared.seeking.store(false, Ordering::Release);
                            if track.eof_decoded && track.pending_offset >= track.pending_pcm.len()
                            {
                                shared.drain_eof.store(true, Ordering::Release);
                            }

                            emit(PlayerEvent::FileLoaded {
                                path: Some(track.url.clone()),
                            });
                            if let Some(dur) = track.duration_secs {
                                emit(PlayerEvent::Duration(dur));
                            }
                            emit(PlayerEvent::Paused(false));
                            buffering_reported = false;
                            emit(PlayerEvent::Buffering(false));
                            last_pos_emit = Instant::now();
                            active = Some(track);
                        }
                        Err(DecodeError::Cancelled) => {
                            shared.seeking.store(false, Ordering::Release);
                            if buffering_reported {
                                buffering_reported = false;
                                emit(PlayerEvent::Buffering(false));
                            }
                        }
                        Err(e) => {
                            shared.seeking.store(false, Ordering::Release);
                            if buffering_reported {
                                buffering_reported = false;
                                emit(PlayerEvent::Buffering(false));
                            }
                            emit(PlayerEvent::EndFile {
                                reason: EndReason::Error,
                                error: Some(e.to_string()),
                            });
                        }
                    }
                }
            }
        }

        // Poll completed prefetch if any.
        if prefetched_ready.is_none()
            && let Some(handle) = prefetch.as_ref()
            && let Ok(res) = handle.rx.try_recv()
        {
            prefetch = None;
            match res {
                Ok(ready) => prefetched_ready = Some(ready),
                Err(e) => {
                    tracing::warn!("ASIO prefetch failed: {e}");
                    prefetch_error = Some(e.to_string());
                }
            }
        }

        // Check gapless boundary transition.
        if shared.boundary_crossed.swap(false, Ordering::AcqRel)
            && let Some(next_track) = prefetched_ready.take()
            && let Some(cur) = active.as_mut()
        {
            upcoming_url = None;
            let boundary = shared.next_boundary_frame.swap(u64::MAX, Ordering::AcqRel);
            shared
                .track_start_consumed
                .store(boundary, Ordering::Release);
            cur.url = next_track.url;
            cur.decoder = next_track.decoder;
            cur.sample_rate = next_track.sample_rate;
            cur.duration_secs = next_track.duration_secs;
            cur.seek_offset_secs = 0.0;
            cur.pushed_frames = next_track.predecoded.len() as u64 / 2;
            cur.eof_decoded = next_track.eof_in_predecode;
            cur.pending_pcm.clear();
            cur.pending_offset = 0;

            emit(PlayerEvent::EndFile {
                reason: EndReason::Eof,
                error: None,
            });
            emit(PlayerEvent::FileLoaded {
                path: Some(cur.url.clone()),
            });
            if let Some(dur) = cur.duration_secs {
                emit(PlayerEvent::Duration(dur));
            }
        }

        // Decode & feed ring buffer for active track.
        if let Some(track) = active.as_mut() {
            push_pending(track, &shared.ring);

            while !track.eof_decoded
                && track.pending_offset >= track.pending_pcm.len()
                && shared.ring.available_write() >= 16_384
            {
                match track.decoder.next_packet_stereo(&mut packet_buf) {
                    Ok(true) => {
                        track.pending_pcm.clear();
                        track.pending_pcm.extend_from_slice(&packet_buf);
                        track.pending_offset = 0;
                        push_pending(track, &shared.ring);
                    }
                    Ok(false) => {
                        track.eof_decoded = true;
                        break;
                    }
                    Err(DecodeError::Cancelled) => {
                        track.eof_decoded = true;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("ASIO decode error: {e}");
                        track.eof_decoded = true;
                        break;
                    }
                }
            }

            // When current track is completely decoded & pushed, handle upcoming track.
            if track.eof_decoded
                && track.pending_offset >= track.pending_pcm.len()
                && shared.next_boundary_frame.load(Ordering::Acquire) == u64::MAX
                && !shared.drain_eof.load(Ordering::Acquire)
            {
                // If prefetch is still running, let it finish before deciding gapless vs drain.
                if prefetched_ready.is_none()
                    && prefetch_error.is_none()
                    && let Some(handle) = prefetch.take()
                {
                    match handle.rx.recv() {
                        Ok(Ok(ready)) => prefetched_ready = Some(ready),
                        Ok(Err(e)) => prefetch_error = Some(e.to_string()),
                        Err(_) => {}
                    }
                }

                if let Some(next) = prefetched_ready.as_ref()
                    && next.sample_rate == track.sample_rate
                {
                    if shared.ring.len() <= 16_384
                        && shared.ring.available_write() >= next.predecoded.len()
                    {
                        let boundary = shared.track_start_consumed.load(Ordering::Acquire)
                            + track.pushed_frames;
                        shared
                            .next_boundary_frame
                            .store(boundary, Ordering::Release);
                        let _ = shared.ring.push(&next.predecoded);
                    }
                } else {
                    shared.drain_eof.store(true, Ordering::Release);
                }
            }

            // Handle clean drain EOF (no same-rate gapless track).
            if shared.drained_eof_reached.swap(false, Ordering::AcqRel) {
                active = None;
                shared.drain_eof.store(false, Ordering::Release);
                emit(PlayerEvent::EndFile {
                    reason: EndReason::Eof,
                    error: None,
                });

                // If `core` had preloaded a different-sample-rate track via `append`,
                // `core` expects `FileLoaded` to follow `EndFile(Eof)`.
                if let Some(next) = prefetched_ready.take() {
                    upcoming_url = None;
                    let target_rate = next.sample_rate;
                    let sink_res = match sink_rate {
                        None => sink.start(f64::from(target_rate), make_render_fn(shared.clone())),
                        Some(r) if r != target_rate => sink.set_sample_rate(f64::from(target_rate)),
                        Some(_) => Ok(()),
                    };
                    if let Err(e) = sink_res {
                        sink.stop();
                        sink_rate = None;
                        emit(PlayerEvent::EndFile {
                            reason: EndReason::Error,
                            error: Some(e.to_string()),
                        });
                    } else {
                        sink_rate = Some(target_rate);
                        let mut next_active = ActiveTrack {
                            url: next.url,
                            decoder: next.decoder,
                            sample_rate: target_rate,
                            duration_secs: next.duration_secs,
                            seek_offset_secs: 0.0,
                            pushed_frames: 0,
                            eof_decoded: next.eof_in_predecode,
                            pending_pcm: next.predecoded,
                            pending_offset: 0,
                        };
                        push_pending(&mut next_active, &shared.ring);
                        let consumed = shared.consumed_frames.load(Ordering::Acquire);
                        shared
                            .track_start_consumed
                            .store(consumed, Ordering::Release);
                        if next_active.eof_decoded
                            && next_active.pending_offset >= next_active.pending_pcm.len()
                        {
                            shared.drain_eof.store(true, Ordering::Release);
                        }
                        emit(PlayerEvent::FileLoaded {
                            path: Some(next_active.url.clone()),
                        });
                        if let Some(dur) = next_active.duration_secs {
                            emit(PlayerEvent::Duration(dur));
                        }
                        last_pos_emit = Instant::now();
                        active = Some(next_active);
                    }
                } else if let Some(err) = prefetch_error.take()
                    && upcoming_url.take().is_some()
                {
                    emit(PlayerEvent::EndFile {
                        reason: EndReason::Error,
                        error: Some(err),
                    });
                }
            }

            // Buffering state & TimePos reporting.
            if let Some(track) = active.as_ref() {
                if shared.underrun.swap(false, Ordering::AcqRel) && !buffering_reported {
                    buffering_reported = true;
                    emit(PlayerEvent::Buffering(true));
                } else if buffering_reported {
                    let min_resume = usize::try_from(track.sample_rate).unwrap_or(44_100) / 2; // 250ms stereo samples
                    if shared.ring.len() >= min_resume || track.eof_decoded {
                        buffering_reported = false;
                        emit(PlayerEvent::Buffering(false));
                    }
                }

                if !shared.paused.load(Ordering::Acquire)
                    && last_pos_emit.elapsed().as_millis() >= TIME_POS_STEP_MS
                {
                    let consumed = shared.consumed_frames.load(Ordering::Acquire);
                    let start = shared.track_start_consumed.load(Ordering::Acquire);
                    let latency = sink.output_latency_frames();
                    let played_frames = consumed.saturating_sub(start).saturating_sub(latency);
                    let pos = track.seek_offset_secs
                        + (played_frames as f64 / f64::from(track.sample_rate));
                    emit(PlayerEvent::TimePos(pos));
                    last_pos_emit = Instant::now();
                }
            }
        }

        // Wait briefly if ring is well-fed or idle.
        let busy_decoding = active
            .as_ref()
            .is_some_and(|t| !t.eof_decoded && shared.ring.available_write() >= 16_384);
        if !busy_decoding {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn push_pending(track: &mut ActiveTrack, ring: &SpscRing) {
    if track.pending_offset >= track.pending_pcm.len() {
        return;
    }
    let slice = &track.pending_pcm[track.pending_offset..];
    let pushed = ring.push(slice);
    track.pending_offset += pushed;
    track.pushed_frames += (pushed / 2) as u64;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{AsioPlayer, AudioSink, RenderFn};
    use crate::{EndReason, PlayerEvent, Result};

    struct FakeSink {
        rates: Arc<Mutex<Vec<f64>>>,
        rendered_samples: Arc<Mutex<Vec<f32>>>,
        frames: Arc<AtomicU64>,
        running: Arc<AtomicBool>,
        pump: Option<thread::JoinHandle<()>>,
    }

    struct FakeSinkLog {
        rates: Arc<Mutex<Vec<f64>>>,
        rendered_samples: Arc<Mutex<Vec<f32>>>,
    }

    impl FakeSink {
        fn new() -> (Self, FakeSinkLog) {
            let rates = Arc::new(Mutex::new(Vec::new()));
            let rendered_samples = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    rates: rates.clone(),
                    rendered_samples: rendered_samples.clone(),
                    frames: Arc::new(AtomicU64::new(0)),
                    running: Arc::new(AtomicBool::new(false)),
                    pump: None,
                },
                FakeSinkLog {
                    rates,
                    rendered_samples,
                },
            )
        }
    }

    impl AudioSink for FakeSink {
        fn start(&mut self, sample_rate: f64, mut render: RenderFn) -> Result<()> {
            self.stop();
            self.rates.lock().unwrap().push(sample_rate);
            let running = Arc::new(AtomicBool::new(true));
            self.running = running.clone();
            let captured = self.rendered_samples.clone();
            let frames = self.frames.clone();
            let handle = thread::spawn(move || {
                let mut buf = vec![0.0_f32; 512];
                while running.load(Ordering::Acquire) {
                    render(256, &mut buf);
                    frames.fetch_add(256, Ordering::Release);
                    if buf.iter().any(|s| *s != 0.0) {
                        captured.lock().unwrap().extend_from_slice(&buf);
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            });
            self.pump = Some(handle);
            Ok(())
        }

        fn set_sample_rate(&mut self, sample_rate: f64) -> Result<()> {
            self.rates.lock().unwrap().push(sample_rate);
            Ok(())
        }

        fn stop(&mut self) {
            self.running.store(false, Ordering::Release);
            if let Some(h) = self.pump.take() {
                let _ = h.join();
            }
        }

        fn rendered_frames(&self) -> u64 {
            self.frames.load(Ordering::Acquire)
        }

        fn open_control_panel(&self) -> Result<()> {
            Ok(())
        }

        fn output_latency_frames(&self) -> u64 {
            0
        }
    }

    fn make_wav(sample_rate: u32, frames: usize, value: i16) -> Vec<u8> {
        let data_bytes = u32::try_from(frames * 4).unwrap();
        let mut out = Vec::with_capacity(44 + data_bytes as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for _ in 0..(frames * 2) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }

    fn spawn_http_files(files: HashMap<&'static str, Vec<u8>>) -> (String, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let mut req = Vec::new();
                let mut chunk = [0u8; 512];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            req.extend_from_slice(&chunk[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") || req.len() >= 8192 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let text = String::from_utf8_lossy(&req);
                let path = text
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                if let Some(body) = files.get(path) {
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                } else {
                    let _ =
                        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    let _ = stream.flush();
                }
            }
        });
        (format!("http://{addr}"), shutdown)
    }

    #[test]
    fn plays_gapless_and_switches_sample_rate() {
        let wav_44a = make_wav(44_100, 16_384, 16_000);
        let wav_44b = make_wav(44_100, 16_384, 8_000);
        let wav_48 = make_wav(48_000, 8_192, 12_000);
        let files = HashMap::from([("/a.wav", wav_44a), ("/b.wav", wav_44b), ("/c.wav", wav_48)]);
        let (base, shutdown) = spawn_http_files(files);

        let (sink, log) = FakeSink::new();
        let (tx, rx) = mpsc::channel();
        let player = AsioPlayer::new(sink, 100.0, move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();
        player.set_fade_gain(0.5);

        let url_a = format!("{base}/a.wav");
        let url_b = format!("{base}/b.wav");
        let url_c = format!("{base}/c.wav");

        player.load(&url_a).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut loaded_count = 0;
        let mut appended_b = false;
        let mut appended_c = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(PlayerEvent::FileLoaded { path }) => {
                    loaded_count += 1;
                    if path.as_deref() == Some(url_a.as_str()) && !appended_b {
                        player.append(&url_b).unwrap();
                        appended_b = true;
                    } else if path.as_deref() == Some(url_b.as_str()) && !appended_c {
                        player.append(&url_c).unwrap();
                        appended_c = true;
                    } else if path.as_deref() == Some(url_c.as_str()) {
                        break;
                    }
                }
                Ok(PlayerEvent::EndFile {
                    reason: EndReason::Error,
                    error,
                }) => panic!("unexpected player error: {error:?}"),
                Ok(_) | Err(_) => {}
            }
        }

        // Wait briefly for final track EOF drain.
        let mut got_final_eof = false;
        let eof_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < eof_deadline {
            if let Ok(PlayerEvent::EndFile {
                reason: EndReason::Eof,
                ..
            }) = rx.recv_timeout(Duration::from_millis(50))
            {
                got_final_eof = true;
                break;
            }
        }

        player.stop().unwrap();
        shutdown.store(true, Ordering::Release);

        assert_eq!(loaded_count, 3);
        assert!(got_final_eof);
        assert_eq!(*log.rates.lock().unwrap(), vec![44_100.0, 48_000.0]);
        assert!(!log.rendered_samples.lock().unwrap().is_empty());
    }
}
