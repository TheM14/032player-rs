//! Pure-Rust ASIO playback engine (`reqwest` + `symphonia` + `asio`).
//!
//! Matches `Player` (`libmpv`) semantics (`PlayerEvent`, gapless `append`,
//! software volume + fade gain, HTTP proxy, reconnect, seek, buffering).

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use asio::{SpscConsumer, SpscProducer, spsc_ring};

use crate::decoder::{DecodeError, TrackDecoder};
use crate::http_source::{CancelToken, HttpMediaSource, build_http_client};
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
    fn is_control_panel_open(&self) -> bool {
        false
    }
    fn take_error(&self) -> Option<String> {
        None
    }
}

#[cfg(windows)]
fn format_asio_error(err: &asio::AsioError) -> String {
    match err {
        asio::AsioError::UnsupportedSampleRate(rate) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let hz = rate.round().max(0.0) as u32;
            format!("当前 ASIO 设备不支持 {hz} Hz 采样率")
        }
        other => other.to_string(),
    }
}

#[cfg(windows)]
pub struct WindowsAsioSink {
    target: asio::AsioTarget,
    active: Option<asio::AsioOutput>,
}

#[cfg(windows)]
impl WindowsAsioSink {
    #[must_use]
    pub fn new(target: asio::AsioTarget) -> Self {
        Self {
            target,
            active: None,
        }
    }
}

#[cfg(windows)]
impl AudioSink for WindowsAsioSink {
    fn start(&mut self, sample_rate: f64, render: RenderFn) -> Result<()> {
        self.active = None;
        let shared_render = Arc::new(Mutex::new(render));
        // Retry briefly on transient device-busy errors during WASAPI -> ASIO handover.
        let delays_ms = [0_u64, 80, 160];
        let mut last_err = None;
        for delay in delays_ms {
            if delay > 0 {
                thread::sleep(Duration::from_millis(delay));
            }
            let cb = shared_render.clone();
            match asio::AsioOutput::start(
                &self.target,
                sample_rate,
                Box::new(move |ctx, buf| {
                    if let Ok(mut f) = cb.try_lock() {
                        f(ctx.frames, buf);
                    } else {
                        buf.fill(0.0);
                    }
                }),
            ) {
                Ok(out) => {
                    self.active = Some(out);
                    return Ok(());
                }
                Err(e @ asio::AsioError::Driver { .. }) => {
                    last_err = Some(e);
                }
                Err(e) => {
                    return Err(PlayerError::Mpv(format_asio_error(&e)));
                }
            }
        }
        Err(PlayerError::Mpv(format_asio_error(
            last_err.as_ref().expect("retry recorded an error"),
        )))
    }

    fn set_sample_rate(&mut self, sample_rate: f64) -> Result<()> {
        let Some(active) = self.active.as_ref() else {
            return Err(PlayerError::Mpv("ASIO output is not active".into()));
        };
        active
            .set_sample_rate(sample_rate)
            .map(|_| ())
            .map_err(|e| PlayerError::Mpv(format_asio_error(&e)))
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
                .map_err(|e| PlayerError::Mpv(format_asio_error(&e)))
        } else {
            asio::open_control_panel_standalone(&self.target.clsid)
                .map_err(|e| PlayerError::Mpv(format_asio_error(&e)))
        }
    }

    fn is_control_panel_open(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(asio::AsioOutput::is_control_panel_open)
    }

    fn take_error(&self) -> Option<String> {
        self.active
            .as_ref()
            .and_then(asio::AsioOutput::take_error)
            .map(|e| format_asio_error(&e))
    }
}

struct SharedState {
    paused: AtomicBool,
    muted: AtomicBool,
    /// When true, callback outputs silence without touching the ring.
    seeking: AtomicBool,
    /// Monotonic reset epoch requested by the producer (`worker_loop`).
    /// Only the audio callback (`SpscConsumer`) advances `head` to clear the ring.
    reset_requested: AtomicU64,
    /// Latest reset epoch acknowledged by the audio callback after `consumer.clear()`.
    reset_acked: AtomicU64,
    /// Producer finished decoding and no same-rate gapless track follows.
    drain_eof: AtomicBool,
    /// Consumed frame index when the sink observed an empty ring at EOF.
    /// `u64::MAX` means no drain has been observed.
    drained_eof_frame: AtomicU64,
    /// Set by consumer when the ring underruns while `drain_eof == false`.
    underrun: AtomicBool,
    volume_bits: AtomicU64,
    fade_bits: AtomicU64,
    /// Total stereo frames popped from the ring and handed to hardware.
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
            paused: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            seeking: AtomicBool::new(false),
            reset_requested: AtomicU64::new(0),
            reset_acked: AtomicU64::new(0),
            drain_eof: AtomicBool::new(false),
            drained_eof_frame: AtomicU64::new(u64::MAX),
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
    Load(String, u64),
    Append(String),
    ClearUpcoming,
    Stop,
    SetPaused(bool),
    Seek(f64),
    OpenControlPanel(mpsc::SyncSender<Result<()>>),
    Shutdown,
}

fn coalesce_commands(cmds: Vec<Cmd>) -> Vec<Cmd> {
    if cmds.iter().any(|c| matches!(c, Cmd::Shutdown)) {
        return vec![Cmd::Shutdown];
    }
    let mut out = Vec::with_capacity(cmds.len());
    for cmd in cmds {
        match cmd {
            Cmd::Load(_, _) | Cmd::Stop => {
                out.retain(|prev| matches!(prev, Cmd::OpenControlPanel(_)));
                out.push(cmd);
            }
            Cmd::Append(_) | Cmd::ClearUpcoming => {
                out.retain(|prev| !matches!(prev, Cmd::Append(_) | Cmd::ClearUpcoming));
                out.push(cmd);
            }
            Cmd::Seek(_) => {
                out.retain(|prev| !matches!(prev, Cmd::Seek(_)));
                out.push(cmd);
            }
            Cmd::SetPaused(_) => {
                out.retain(|prev| !matches!(prev, Cmd::SetPaused(_)));
                out.push(cmd);
            }
            Cmd::OpenControlPanel(_) | Cmd::Shutdown => out.push(cmd),
        }
    }
    out
}

pub struct AsioPlayer {
    cmd_tx: mpsc::Sender<Cmd>,
    shared: Arc<SharedState>,
    proxy: Arc<Mutex<Option<String>>>,
    cancel_load: Arc<Mutex<CancelToken>>,
    load_gen: Arc<AtomicU64>,
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
        let cancel_load = Arc::new(Mutex::new(CancelToken::new()));
        let load_gen = Arc::new(AtomicU64::new(0));
        let (cmd_tx, cmd_rx) = mpsc::channel();

        let shared_w = shared.clone();
        let proxy_w = proxy.clone();
        let cancel_w = cancel_load.clone();
        let gen_w = load_gen.clone();
        let thread = thread::Builder::new()
            .name("asio-player".into())
            .spawn(move || {
                worker_loop(
                    sink,
                    shared_w,
                    proxy_w,
                    cancel_w,
                    gen_w,
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
            load_gen,
            thread: Some(thread),
        })
    }

    pub fn load(&self, url: &str) -> Result<()> {
        let generation = self.load_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.abort_active_io();
        self.cmd_tx
            .send(Cmd::Load(url.to_owned(), generation))
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
        self.load_gen.fetch_add(1, Ordering::AcqRel);
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
        self.cancel_load.lock().expect("cancel lock").cancel();
    }
}

impl Drop for AsioPlayer {
    fn drop(&mut self) {
        self.load_gen.fetch_add(1, Ordering::AcqRel);
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
    pushed_at_boundary: usize,
    eof_in_predecode: bool,
    sample_rate: u32,
    duration_secs: Option<f64>,
}

struct PrefetchHandle {
    cancel: CancelToken,
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
    client: reqwest::Client,
    rt: tokio::runtime::Handle,
) -> PrefetchHandle {
    let cancel = CancelToken::new();
    let cancel_w = cancel.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("asio-prefetch".into())
        .spawn(move || {
            let res = open_and_predecode(&url, client, rt, cancel_w);
            let _ = tx.send(res);
        })
        .ok();
    PrefetchHandle { cancel, rx }
}

fn open_and_predecode(
    url: &str,
    client: reqwest::Client,
    rt: tokio::runtime::Handle,
    cancel: CancelToken,
) -> std::result::Result<PrefetchedTrack, DecodeError> {
    let source = HttpMediaSource::open(client, url, rt, cancel).map_err(|e| {
        if e.kind() == io::ErrorKind::Interrupted {
            DecodeError::Cancelled
        } else {
            DecodeError::Other(e.to_string())
        }
    })?;
    let mime = source.content_type().map(ToOwned::to_owned);
    let mut decoder = TrackDecoder::open(Box::new(source), Some(url), mime.as_deref())?;
    let sample_rate = decoder.sample_rate();
    let duration_secs = decoder.duration_secs();

    // Target up to 1 second of stereo PCM, capped to half the ring buffer so even
    // ultra-high sample rates (e.g. 768 kHz) fit cleanly for gapless handoff.
    let target_samples = (usize::try_from(sample_rate).unwrap_or(44_100) * 2).min(RING_SAMPLES / 2);
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
        pushed_at_boundary: 0,
        eof_in_predecode,
        sample_rate,
        duration_secs,
    })
}

const RAMP_FRAMES: usize = 64;

fn make_render_fn(shared: Arc<SharedState>, mut consumer: SpscConsumer) -> RenderFn {
    let mut current_gain: Option<f32> = None;
    let mut was_silenced = false;
    Box::new(move |frames: usize, out_lr: &mut [f32]| {
        let needed = frames * 2;
        let req = shared.reset_requested.load(Ordering::Acquire);
        if req != shared.reset_acked.load(Ordering::Relaxed) {
            consumer.clear();
            shared.reset_acked.store(req, Ordering::Release);
            was_silenced = true;
            current_gain = Some(0.0);
        }
        let seeking = shared.seeking.load(Ordering::Acquire);
        let paused = shared.paused.load(Ordering::Acquire);
        if seeking || paused {
            if paused
                && !seeking
                && !was_silenced
                && let Some(prev) = current_gain.filter(|g| *g > 1e-6)
            {
                // Micro fade-out before entering silence to avoid pause clicks.
                let fade_frames = frames.min(RAMP_FRAMES);
                let got = consumer.pop(&mut out_lr[..(fade_frames * 2)]);
                let got_frames = got / 2;
                if got_frames > 0 {
                    let denom = got_frames as f32;
                    for f in 0..got_frames {
                        let g = prev * (1.0 - (f + 1) as f32 / denom);
                        out_lr[f * 2] *= g;
                        out_lr[f * 2 + 1] *= g;
                    }
                    let new_consumed = shared
                        .consumed_frames
                        .fetch_add(got_frames as u64, Ordering::AcqRel)
                        + got_frames as u64;
                    let boundary = shared.next_boundary_frame.load(Ordering::Acquire);
                    if new_consumed >= boundary {
                        shared.boundary_crossed.store(true, Ordering::Release);
                    }
                }
                out_lr[got..needed].fill(0.0);
            } else {
                out_lr[..needed].fill(0.0);
            }
            current_gain = Some(0.0);
            was_silenced = true;
            return;
        }

        let got = consumer.pop(&mut out_lr[..needed]);
        let got_frames = got / 2;
        let target_amp = shared.target_amp();
        let prev = if was_silenced {
            0.0
        } else {
            current_gain.unwrap_or(target_amp)
        };
        let ramp_frames = got_frames.min(RAMP_FRAMES);
        if (prev - target_amp).abs() > 1e-6 && ramp_frames > 0 {
            // Step per stereo frame so L and R receive identical gain.
            let denom = ramp_frames as f32;
            for f in 0..got_frames {
                let gain = if f < ramp_frames {
                    prev + (target_amp - prev) * ((f + 1) as f32 / denom)
                } else {
                    target_amp
                };
                out_lr[f * 2] *= gain;
                out_lr[f * 2 + 1] *= gain;
            }
        } else if (target_amp - 1.0).abs() > 1e-6 {
            for sample in &mut out_lr[..got] {
                *sample *= target_amp;
            }
        }
        if got_frames > 0 {
            was_silenced = false;
            current_gain = Some(target_amp);
        }

        if got < needed {
            out_lr[got..needed].fill(0.0);
            if got == 0 && shared.drain_eof.load(Ordering::Acquire) {
                shared.drained_eof_frame.store(
                    shared.consumed_frames.load(Ordering::Relaxed),
                    Ordering::Release,
                );
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
    cancel_load: Arc<Mutex<CancelToken>>,
    load_gen: Arc<AtomicU64>,
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
    let mut producer: Option<SpscProducer> = None;
    let mut active: Option<ActiveTrack> = None;
    let mut upcoming_url: Option<String> = None;
    let mut prefetch: Option<PrefetchHandle> = None;
    let mut prefetched_ready: Option<PrefetchedTrack> = None;
    let mut prefetch_error: Option<String> = None;
    let mut buffering_reported = false;
    let mut last_pos_emit = Instant::now();
    let mut last_rendered_frames = 0_u64;
    let mut last_rendered_at = Instant::now();
    let mut packet_buf = Vec::new();
    // Reused across tracks and prefetches; rebuilt only when the proxy changes.
    let mut cached_http: Option<(Option<String>, reqwest::Client)> = None;
    let mut get_http_client =
        |proxy_cfg: &Option<String>| -> std::result::Result<reqwest::Client, DecodeError> {
            if let Some((cached_proxy, client)) = &cached_http
                && cached_proxy == proxy_cfg
            {
                return Ok(client.clone());
            }
            let client = build_http_client(proxy_cfg.as_deref())
                .map_err(|e| DecodeError::Other(e.without_url().to_string()))?;
            cached_http = Some((proxy_cfg.clone(), client.clone()));
            Ok(client)
        };

    let clear_upcoming = |upcoming_url: &mut Option<String>,
                          prefetch: &mut Option<PrefetchHandle>,
                          prefetched_ready: &mut Option<PrefetchedTrack>,
                          prefetch_error: &mut Option<String>| {
        *upcoming_url = None;
        if let Some(p) = prefetch.take() {
            p.cancel.cancel();
        }
        *prefetched_ready = None;
        *prefetch_error = None;
    };

    'worker: loop {
        let mut raw_cmds = Vec::new();
        while let Ok(cmd) = cmd_rx.try_recv() {
            raw_cmds.push(cmd);
        }
        for cmd in coalesce_commands(raw_cmds) {
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
                    // Stop the sink first so its `SpscConsumer` is dropped before
                    // discarding the producer and resetting synchronization state.
                    sink.stop();
                    producer = None;
                    sink_rate = None;
                    shared.reset_requested.store(0, Ordering::Release);
                    shared.reset_acked.store(0, Ordering::Release);
                    shared.drain_eof.store(false, Ordering::Release);
                    shared.drained_eof_frame.store(u64::MAX, Ordering::Release);
                    shared
                        .next_boundary_frame
                        .store(u64::MAX, Ordering::Release);
                    shared.boundary_crossed.store(false, Ordering::Release);
                    shared.underrun.store(false, Ordering::Release);
                    shared.seeking.store(false, Ordering::Release);
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
                    match get_http_client(&p) {
                        Ok(client) => {
                            prefetch = Some(spawn_prefetch(url, client, rt.handle().clone()));
                        }
                        Err(e) => {
                            tracing::warn!("ASIO prefetch client error: {e}");
                            prefetch_error = Some(e.to_string());
                        }
                    }
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
                        match track.decoder.seek(seconds) {
                            Ok(landed) => {
                                let reset_ok = if sink_rate.is_some() && producer.is_some() {
                                    let epoch = request_ring_reset(&shared);
                                    wait_ring_reset(&shared, epoch, Duration::from_millis(500))
                                } else {
                                    true
                                };
                                if !reset_ok {
                                    let rate = sink_rate.unwrap_or(track.sample_rate);
                                    if let Err(e) =
                                        start_fresh_sink(&mut sink, &shared, &mut producer, rate)
                                    {
                                        sink_rate = None;
                                        active = None;
                                        shared.seeking.store(false, Ordering::Release);
                                        emit(PlayerEvent::EndFile {
                                            reason: EndReason::Error,
                                            error: Some(e.to_string()),
                                        });
                                        continue;
                                    }
                                    sink_rate = Some(rate);
                                }
                                shared.drain_eof.store(false, Ordering::Release);
                                shared.drained_eof_frame.store(u64::MAX, Ordering::Release);
                                shared
                                    .next_boundary_frame
                                    .store(u64::MAX, Ordering::Release);
                                shared.boundary_crossed.store(false, Ordering::Release);
                                shared.underrun.store(false, Ordering::Release);
                                track.pending_pcm.clear();
                                track.pending_offset = 0;
                                track.eof_decoded = false;
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
                                continue 'worker;
                            }
                            Err(e) => {
                                shared.seeking.store(false, Ordering::Release);
                                tracing::warn!("ASIO seek failed: {e}");
                            }
                        }
                    }
                }
                Cmd::Load(url, generation) => {
                    if load_gen.load(Ordering::Acquire) != generation {
                        continue 'worker;
                    }
                    clear_upcoming(
                        &mut upcoming_url,
                        &mut prefetch,
                        &mut prefetched_ready,
                        &mut prefetch_error,
                    );
                    let was_active = active.take().is_some();
                    shared.seeking.store(true, Ordering::Release);
                    let reset_epoch = (sink_rate.is_some() && producer.is_some())
                        .then(|| request_ring_reset(&shared));
                    shared.drain_eof.store(false, Ordering::Release);
                    shared.drained_eof_frame.store(u64::MAX, Ordering::Release);
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

                    let cancel = CancelToken::new();
                    *cancel_load.lock().expect("cancel lock") = cancel.clone();
                    if load_gen.load(Ordering::Acquire) != generation {
                        cancel.cancel();
                        shared.seeking.store(false, Ordering::Release);
                        continue 'worker;
                    }
                    let p = proxy.lock().expect("proxy lock").clone();
                    emit(PlayerEvent::Buffering(true));
                    buffering_reported = true;

                    let open_res = match get_http_client(&p) {
                        Ok(client) => open_and_predecode(&url, client, rt.handle().clone(), cancel),
                        Err(e) => Err(e),
                    };
                    match open_res {
                        Ok(opened) => {
                            if load_gen.load(Ordering::Acquire) != generation {
                                shared.seeking.store(false, Ordering::Release);
                                if buffering_reported {
                                    buffering_reported = false;
                                    emit(PlayerEvent::Buffering(false));
                                }
                                continue 'worker;
                            }
                            let target_rate = opened.sample_rate;
                            let sink_res = match sink_rate {
                                None => {
                                    start_fresh_sink(&mut sink, &shared, &mut producer, target_rate)
                                }
                                Some(_) if producer.is_none() => {
                                    start_fresh_sink(&mut sink, &shared, &mut producer, target_rate)
                                }
                                Some(r) => {
                                    let rate_res = if r != target_rate {
                                        sink.set_sample_rate(f64::from(target_rate))
                                    } else {
                                        Ok(())
                                    };
                                    match rate_res {
                                        Ok(()) => {
                                            if let Some(epoch) = reset_epoch
                                                && !wait_ring_reset(
                                                    &shared,
                                                    epoch,
                                                    Duration::from_millis(500),
                                                )
                                            {
                                                start_fresh_sink(
                                                    &mut sink,
                                                    &shared,
                                                    &mut producer,
                                                    target_rate,
                                                )
                                            } else {
                                                Ok(())
                                            }
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                            };
                            if let Err(e) = sink_res {
                                sink.stop();
                                producer = None;
                                sink_rate = None;
                                shared.reset_requested.store(0, Ordering::Release);
                                shared.reset_acked.store(0, Ordering::Release);
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
                            if let Some(prod) = producer.as_mut() {
                                push_pending(&mut track, prod);
                            }
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
                            last_rendered_frames = sink.rendered_frames();
                            last_rendered_at = Instant::now();
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
                            // Release exclusive ASIO ownership when idle after load failure.
                            stop_sink_idle(&mut sink, &shared, &mut producer, &mut sink_rate);
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
        if let Some(handle) = prefetch.as_ref() {
            let result = match handle.rx.try_recv() {
                Ok(result) => Some(result.map_err(|e| e.to_string())),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("ASIO prefetch worker stopped".into()))
                }
            };
            if let Some(result) = result {
                prefetch = None;
                match result {
                    Ok(ready) => prefetched_ready = Some(ready),
                    Err(e) => {
                        tracing::warn!("ASIO prefetch failed: {e}");
                        prefetch_error = Some(e);
                    }
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
            cur.pushed_frames = (next_track.pushed_at_boundary / 2) as u64;
            cur.eof_decoded = next_track.eof_in_predecode;
            if next_track.pushed_at_boundary < next_track.predecoded.len() {
                cur.pending_pcm = next_track.predecoded[next_track.pushed_at_boundary..].to_vec();
            } else {
                cur.pending_pcm.clear();
            }
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
            if let Some(prod) = producer.as_mut() {
                push_pending(track, prod);

                while !track.eof_decoded
                    && track.pending_offset >= track.pending_pcm.len()
                    && prod.available_write() >= 16_384
                {
                    match track.decoder.next_packet_stereo(&mut packet_buf) {
                        Ok(true) => {
                            track.pending_pcm.clear();
                            track.pending_pcm.extend_from_slice(&packet_buf);
                            track.pending_offset = 0;
                            push_pending(track, prod);
                        }
                        Ok(false) => {
                            track.eof_decoded = true;
                            break;
                        }
                        Err(DecodeError::Cancelled) => {
                            continue 'worker;
                        }
                        Err(e) => {
                            tracing::warn!("ASIO decode error: {e}");
                            clear_upcoming(
                                &mut upcoming_url,
                                &mut prefetch,
                                &mut prefetched_ready,
                                &mut prefetch_error,
                            );
                            active = None;
                            stop_sink_idle(&mut sink, &shared, &mut producer, &mut sink_rate);
                            if buffering_reported {
                                buffering_reported = false;
                                emit(PlayerEvent::Buffering(false));
                            }
                            emit(PlayerEvent::EndFile {
                                reason: EndReason::Error,
                                error: Some(e.to_string()),
                            });
                            continue 'worker;
                        }
                    }
                }

                // When current track is completely decoded & pushed, handle upcoming track.
                if track.eof_decoded
                    && track.pending_offset >= track.pending_pcm.len()
                    && shared.next_boundary_frame.load(Ordering::Acquire) == u64::MAX
                    && !shared.drain_eof.load(Ordering::Acquire)
                    && prefetch.is_none()
                {
                    if let Some(next) = prefetched_ready.as_mut()
                        && next.sample_rate == track.sample_rate
                    {
                        if prod.len() <= 16_384 {
                            let to_push = next.predecoded.len().min(prod.available_write()) & !1;
                            if to_push > 0 || next.predecoded.is_empty() {
                                let boundary = shared.track_start_consumed.load(Ordering::Acquire)
                                    + track.pushed_frames;
                                shared
                                    .next_boundary_frame
                                    .store(boundary, Ordering::Release);
                                next.pushed_at_boundary = prod.push(&next.predecoded[..to_push]);
                            }
                        }
                    } else {
                        shared.drain_eof.store(true, Ordering::Release);
                    }
                }
            }

            // A late append can still be loading after the sink drains. Keep the
            // current track until that prefetch resolves, and keep handling commands.
            let end_frame =
                shared.track_start_consumed.load(Ordering::Acquire) + track.pushed_frames;
            let drained = shared.drain_eof.load(Ordering::Acquire)
                && shared.drained_eof_frame.load(Ordering::Acquire) == end_frame;
            let awaiting_prefetch = drained && prefetch.is_some();
            if drained && prefetch.is_none() {
                active = None;
                shared.drain_eof.store(false, Ordering::Release);
                shared.drained_eof_frame.store(u64::MAX, Ordering::Release);
                if buffering_reported {
                    buffering_reported = false;
                    emit(PlayerEvent::Buffering(false));
                }
                emit(PlayerEvent::EndFile {
                    reason: EndReason::Eof,
                    error: None,
                });

                // If `core` had preloaded a different-sample-rate track via `append`,
                // `core` expects `FileLoaded` to follow `EndFile(Eof)`.
                if let Some(next) = prefetched_ready.take() {
                    upcoming_url = None;
                    let target_rate = next.sample_rate;
                    shared.seeking.store(true, Ordering::Release);
                    let sink_res = match sink_rate {
                        None => start_fresh_sink(&mut sink, &shared, &mut producer, target_rate),
                        Some(_) if producer.is_none() => {
                            start_fresh_sink(&mut sink, &shared, &mut producer, target_rate)
                        }
                        Some(r) if r != target_rate => sink.set_sample_rate(f64::from(target_rate)),
                        Some(_) => Ok(()),
                    };
                    if let Err(e) = sink_res {
                        sink.stop();
                        producer = None;
                        sink_rate = None;
                        shared.reset_requested.store(0, Ordering::Release);
                        shared.reset_acked.store(0, Ordering::Release);
                        shared.seeking.store(false, Ordering::Release);
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
                        let consumed = shared.consumed_frames.load(Ordering::Acquire);
                        shared
                            .track_start_consumed
                            .store(consumed, Ordering::Release);
                        if let Some(prod) = producer.as_mut() {
                            push_pending(&mut next_active, prod);
                        }
                        shared.seeking.store(false, Ordering::Release);
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
                        last_rendered_frames = sink.rendered_frames();
                        last_rendered_at = Instant::now();
                        active = Some(next_active);
                    }
                } else if let Some(err) = prefetch_error.take()
                    && upcoming_url.take().is_some()
                {
                    stop_sink_idle(&mut sink, &shared, &mut producer, &mut sink_rate);
                    emit(PlayerEvent::EndFile {
                        reason: EndReason::Error,
                        error: Some(err),
                    });
                } else {
                    // Queue finished with no next track: release exclusive ASIO ownership.
                    stop_sink_idle(&mut sink, &shared, &mut producer, &mut sink_rate);
                }
            }

            // Check async driver error or stalled hardware callbacks (e.g. device unplugged).
            if active.is_some() {
                let mut fatal_err = sink.take_error();
                if fatal_err.is_none() {
                    let rendered = sink.rendered_frames();
                    let paused = shared.paused.load(Ordering::Acquire);
                    let seeking = shared.seeking.load(Ordering::Acquire);
                    if paused
                        || seeking
                        || sink.is_control_panel_open()
                        || rendered != last_rendered_frames
                    {
                        last_rendered_frames = rendered;
                        last_rendered_at = Instant::now();
                    } else if last_rendered_at.elapsed() >= Duration::from_secs(3) {
                        fatal_err = Some("ASIO 硬件输出中断（设备可能已断开）".into());
                    }
                }
                if let Some(err) = fatal_err {
                    clear_upcoming(
                        &mut upcoming_url,
                        &mut prefetch,
                        &mut prefetched_ready,
                        &mut prefetch_error,
                    );
                    active = None;
                    stop_sink_idle(&mut sink, &shared, &mut producer, &mut sink_rate);
                    if buffering_reported {
                        buffering_reported = false;
                        emit(PlayerEvent::Buffering(false));
                    }
                    emit(PlayerEvent::EndFile {
                        reason: EndReason::Error,
                        error: Some(err),
                    });
                    continue 'worker;
                }
            }

            // Buffering state & TimePos reporting.
            if let Some(track) = active.as_ref() {
                let underrun = shared.underrun.swap(false, Ordering::AcqRel);
                if (underrun || awaiting_prefetch) && !buffering_reported {
                    buffering_reported = true;
                    emit(PlayerEvent::Buffering(true));
                } else if buffering_reported && !awaiting_prefetch {
                    let min_resume = (usize::try_from(track.sample_rate).unwrap_or(44_100) / 2)
                        .min(RING_SAMPLES / 2); // 250ms stereo samples, capped to ring
                    let buffered = producer.as_ref().map_or(0, SpscProducer::len);
                    if buffered >= min_resume || track.eof_decoded {
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
        let busy_decoding = active.as_ref().is_some_and(|t| {
            !t.eof_decoded
                && producer
                    .as_ref()
                    .is_some_and(|p| p.available_write() >= 16_384)
        });
        if !busy_decoding {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn request_ring_reset(shared: &SharedState) -> u64 {
    shared.reset_requested.fetch_add(1, Ordering::AcqRel) + 1
}

fn wait_ring_reset(shared: &SharedState, epoch: u64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if shared.reset_acked.load(Ordering::Acquire) == epoch {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn stop_sink_idle<S: AudioSink>(
    sink: &mut S,
    shared: &Arc<SharedState>,
    producer: &mut Option<SpscProducer>,
    sink_rate: &mut Option<u32>,
) {
    shared.seeking.store(true, Ordering::Release);
    sink.stop();
    *producer = None;
    *sink_rate = None;
    shared.reset_requested.store(0, Ordering::Release);
    shared.reset_acked.store(0, Ordering::Release);
    shared.drain_eof.store(false, Ordering::Release);
    shared.drained_eof_frame.store(u64::MAX, Ordering::Release);
    shared
        .next_boundary_frame
        .store(u64::MAX, Ordering::Release);
    shared.boundary_crossed.store(false, Ordering::Release);
    shared.underrun.store(false, Ordering::Release);
    shared.seeking.store(false, Ordering::Release);
}

fn start_fresh_sink<S: AudioSink>(
    sink: &mut S,
    shared: &Arc<SharedState>,
    producer: &mut Option<SpscProducer>,
    target_rate: u32,
) -> Result<()> {
    sink.stop();
    *producer = None;
    shared.reset_requested.store(0, Ordering::Release);
    shared.reset_acked.store(0, Ordering::Release);
    let (new_prod, new_cons) = spsc_ring(RING_SAMPLES);
    sink.start(
        f64::from(target_rate),
        make_render_fn(shared.clone(), new_cons),
    )?;
    *producer = Some(new_prod);
    Ok(())
}

fn push_pending(track: &mut ActiveTrack, ring: &mut SpscProducer) {
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

    use super::{AsioPlayer, AudioSink, Cmd, RenderFn, coalesce_commands};
    use crate::{EndReason, PlayerEvent, Result};

    struct FakeSink(Arc<FakeSinkLog>);

    #[derive(Default)]
    struct FakeSinkLog {
        render: Mutex<Option<RenderFn>>,
        rates: Mutex<Vec<f64>>,
        rendered_samples: Mutex<Vec<f32>>,
        frames: AtomicU64,
    }

    impl FakeSinkLog {
        fn render_buffer(&self) {
            let mut render = self.render.lock().unwrap();
            if let Some(render) = render.as_mut() {
                let mut buf = [0.0; 512];
                render(256, &mut buf);
                self.frames.fetch_add(256, Ordering::Relaxed);
                if buf.iter().any(|s| *s != 0.0) {
                    self.rendered_samples
                        .lock()
                        .unwrap()
                        .extend_from_slice(&buf);
                }
            }
        }
    }

    impl AudioSink for FakeSink {
        fn start(&mut self, sample_rate: f64, render: RenderFn) -> Result<()> {
            self.0.rates.lock().unwrap().push(sample_rate);
            *self.0.render.lock().unwrap() = Some(render);
            Ok(())
        }

        fn set_sample_rate(&mut self, sample_rate: f64) -> Result<()> {
            self.0.rates.lock().unwrap().push(sample_rate);
            Ok(())
        }

        fn stop(&mut self) {
            *self.0.render.lock().unwrap() = None;
        }

        fn rendered_frames(&self) -> u64 {
            self.0.frames.load(Ordering::Relaxed)
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

    fn spawn_http_files(
        files: HashMap<&'static str, Vec<u8>>,
        release_c: mpsc::Receiver<()>,
    ) -> (String, Arc<AtomicBool>) {
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
                if path == "/c.wav" && release_c.recv().is_err() {
                    return;
                }
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
        let (release_c, wait_for_release) = mpsc::channel();
        let (base, shutdown) = spawn_http_files(files, wait_for_release);

        let log = Arc::new(FakeSinkLog::default());
        let sink = FakeSink(log.clone());
        let (tx, rx) = mpsc::channel();
        let player = AsioPlayer::new(sink, 100.0, move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();
        player.set_fade_gain(0.5);

        let url_a = format!("{base}/a.wav");
        let url_b = format!("{base}/b.wav");
        let url_c = format!("{base}/c.wav");

        let expect_event = |expected: PlayerEvent| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out waiting for {expected:?}");
                let event = match rx.recv_timeout(remaining.min(Duration::from_millis(5))) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        log.render_buffer();
                        continue;
                    }
                    Err(e) => panic!("waiting for {expected:?}: {e}"),
                };
                if event == expected {
                    break;
                }
                assert!(
                    !matches!(
                        event,
                        PlayerEvent::EndFile { .. } | PlayerEvent::FileLoaded { .. }
                    ),
                    "expected {expected:?}, got {event:?}"
                );
            }
        };
        let loaded = |url: &str| PlayerEvent::FileLoaded {
            path: Some(url.to_owned()),
        };
        let eof = PlayerEvent::EndFile {
            reason: EndReason::Eof,
            error: None,
        };

        player.load(&url_a).unwrap();
        expect_event(loaded(&url_a));
        player.append(&url_b).unwrap();
        expect_event(eof.clone());
        expect_event(loaded(&url_b));

        // Do not respond to C until B has drained and is waiting for its preload.
        player.append(&url_c).unwrap();
        expect_event(PlayerEvent::Buffering(true));
        let previous_end_frame = player.shared.drained_eof_frame.load(Ordering::Acquire);
        assert_eq!(previous_end_frame, 16_384 + 16_384);
        player.set_paused(true).unwrap();
        expect_event(PlayerEvent::Paused(true));
        player.set_paused(false).unwrap();
        expect_event(PlayerEvent::Paused(false));
        release_c.send(()).unwrap();

        expect_event(eof.clone());
        expect_event(loaded(&url_c));
        // Simulate a last callback from B publishing its drain after C starts.
        player
            .shared
            .drained_eof_frame
            .store(previous_end_frame, Ordering::Release);
        expect_event(eof);
        player.stop().unwrap();
        shutdown.store(true, Ordering::Release);

        assert_eq!(*log.rates.lock().unwrap(), vec![44_100.0, 48_000.0]);
        assert_eq!(
            log.rendered_samples.lock().unwrap().len(),
            2 * (16_384 + 16_384 + 8_192),
            "All three tracks must be rendered"
        );
    }

    #[test]
    fn seek_and_load_replace_reset_ring_via_consumer_without_race() {
        let wav_a = make_wav(44_100, 44_100, 12_000);
        let wav_b = make_wav(44_100, 8_192, 6_000);
        let files = HashMap::from([("/a.wav", wav_a), ("/b.wav", wav_b)]);
        let (_release_c, wait_for_release) = mpsc::channel();
        let (base, shutdown) = spawn_http_files(files, wait_for_release);

        let log = Arc::new(FakeSinkLog::default());
        let sink = FakeSink(log.clone());
        let (tx, rx) = mpsc::channel();
        let player = AsioPlayer::new(sink, 100.0, move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();

        let url_a = format!("{base}/a.wav");
        let url_b = format!("{base}/b.wav");

        let wait_for = |pred: &dyn Fn(&PlayerEvent) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out waiting for event");
                match rx.recv_timeout(remaining.min(Duration::from_millis(5))) {
                    Ok(ev) if pred(&ev) => return ev,
                    Ok(_) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => log.render_buffer(),
                    Err(e) => panic!("channel closed: {e}"),
                }
            }
        };

        player.load(&url_a).unwrap();
        wait_for(&|e| matches!(e, PlayerEvent::FileLoaded { path: Some(p) } if p == &url_a));
        // Render a few buffers then seek mid-track while consumer is active.
        for _ in 0..8 {
            log.render_buffer();
        }
        player.seek(0.5).unwrap();
        wait_for(&|e| matches!(e, PlayerEvent::PlaybackRestart));

        // Replace track while actively playing without stopping first.
        for _ in 0..4 {
            log.render_buffer();
        }
        player.load(&url_b).unwrap();
        wait_for(&|e| matches!(e, PlayerEvent::FileLoaded { path: Some(p) } if p == &url_b));
        wait_for(&|e| {
            matches!(
                e,
                PlayerEvent::EndFile {
                    reason: EndReason::Eof,
                    ..
                }
            )
        });

        player.stop().unwrap();
        shutdown.store(true, Ordering::Release);
    }

    #[test]
    fn coalesce_commands_drops_superseded_loads_and_preserves_control_panel() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let cmds = vec![
            Cmd::Load("http://example/1.flac".into(), 1),
            Cmd::Seek(10.0),
            Cmd::Append("http://example/next.flac".into()),
            Cmd::SetPaused(true),
            Cmd::OpenControlPanel(tx),
            Cmd::Load("http://example/2.flac".into(), 2),
            Cmd::Load("http://example/3.flac".into(), 3),
            Cmd::Seek(20.0),
            Cmd::Seek(30.0),
            Cmd::SetPaused(false),
        ];
        let out = coalesce_commands(cmds);
        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], Cmd::OpenControlPanel(_)));
        assert!(matches!(&out[1], Cmd::Load(u, 3) if u == "http://example/3.flac"));
        assert!(matches!(out[2], Cmd::Seek(s) if (s - 30.0).abs() < f64::EPSILON));
        assert!(matches!(out[3], Cmd::SetPaused(false)));

        let (tx2, _rx2) = mpsc::sync_channel(1);
        let stopped = coalesce_commands(vec![
            Cmd::Load("http://example/1.flac".into(), 1),
            Cmd::OpenControlPanel(tx2),
            Cmd::Stop,
        ]);
        assert_eq!(stopped.len(), 2);
        assert!(matches!(stopped[0], Cmd::OpenControlPanel(_)));
        assert!(matches!(stopped[1], Cmd::Stop));
    }

    #[test]
    fn queue_end_releases_sink_and_high_rate_gapless_does_not_deadlock() {
        // Use 768 kHz WAV tracks (1 second = 1,536,000 stereo samples > RING_SAMPLES)
        // to prove gapless handoff works without deadlocking and releases the sink on EOF.
        let wav_a = make_wav(768_000, 16_384, 10_000);
        let wav_b = make_wav(768_000, 16_384, 5_000);
        let files = HashMap::from([("/a.wav", wav_a), ("/b.wav", wav_b)]);
        let (_release, wait) = mpsc::channel();
        let (base, shutdown) = spawn_http_files(files, wait);

        let log = Arc::new(FakeSinkLog::default());
        let sink = FakeSink(log.clone());
        let (tx, rx) = mpsc::channel();
        let player = AsioPlayer::new(sink, 100.0, move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();

        let url_a = format!("{base}/a.wav");
        let url_b = format!("{base}/b.wav");
        let wait_for = |pred: &dyn Fn(&PlayerEvent) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out waiting for event");
                match rx.recv_timeout(remaining.min(Duration::from_millis(5))) {
                    Ok(ev) if pred(&ev) => return ev,
                    Ok(_) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => log.render_buffer(),
                    Err(e) => panic!("channel closed: {e}"),
                }
            }
        };

        player.load(&url_a).unwrap();
        wait_for(&|e| matches!(e, PlayerEvent::FileLoaded { path: Some(p) } if p == &url_a));
        player.append(&url_b).unwrap();
        wait_for(&|e| {
            matches!(
                e,
                PlayerEvent::EndFile {
                    reason: EndReason::Eof,
                    ..
                }
            )
        });
        wait_for(&|e| matches!(e, PlayerEvent::FileLoaded { path: Some(p) } if p == &url_b));
        wait_for(&|e| {
            matches!(
                e,
                PlayerEvent::EndFile {
                    reason: EndReason::Eof,
                    ..
                }
            )
        });
        // Wait briefly for worker loop to stop sink after queue end.
        let deadline = Instant::now() + Duration::from_secs(2);
        while log.render.lock().unwrap().is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            log.render.lock().unwrap().is_none(),
            "sink must be stopped when queue finishes idle"
        );
        shutdown.store(true, Ordering::Release);
    }

    #[test]
    fn mid_track_stream_truncation_emits_error_and_stops_sink() {
        // Advertise a huge WAV file (200k frames) without Accept-Ranges and close after
        // ~1.5s of audio so open_and_predecode succeeds (1s) and mid-playback decode fails.
        let full = make_wav(44_100, 200_000, 12_000);
        let truncated_len = 44 + 60_000 * 4;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    full.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&full[..truncated_len]);
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });

        let log = Arc::new(FakeSinkLog::default());
        let sink = FakeSink(log.clone());
        let (tx, rx) = mpsc::channel();
        let player = AsioPlayer::new(sink, 100.0, move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();

        let url = format!("http://{addr}/trunc.wav");
        player.load(&url).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_error = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(PlayerEvent::EndFile { reason, .. }) => {
                    assert_eq!(reason, EndReason::Error);
                    saw_error = true;
                    break;
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => log.render_buffer(),
                Err(e) => panic!("channel closed: {e}"),
            }
        }
        assert!(
            saw_error,
            "expected EndReason::Error on mid-track truncation"
        );
        assert!(
            log.render.lock().unwrap().is_none(),
            "sink must be stopped after decode error"
        );
        let _ = srv.join();
    }

    #[test]
    fn volume_ramp_steps_per_stereo_frame_and_fades_pause_resume() {
        let shared = Arc::new(super::SharedState::new(100.0));
        let (mut prod, cons) = asio::spsc_ring(4096);
        let input = vec![1.0f32; 2048];
        assert_eq!(prod.push(&input), 2048);

        let mut render = super::make_render_fn(shared.clone(), cons);
        let mut out = vec![0.0f32; 256];

        // Start steady at 1.0.
        render(128, &mut out);
        assert!((out[0] - 1.0).abs() < 1e-6 && (out[1] - 1.0).abs() < 1e-6);

        // Drop fade gain to 0.5: every stereo frame during ramp must have equal L and R.
        shared.fade_bits.store(0.5_f64.to_bits(), Ordering::Relaxed);
        render(128, &mut out);
        for f in 0..super::RAMP_FRAMES {
            assert!(
                (out[f * 2] - out[f * 2 + 1]).abs() < 1e-6,
                "frame {f} L/R mismatch: {} vs {}",
                out[f * 2],
                out[f * 2 + 1]
            );
        }
        assert!(out[0] < 1.0 && out[0] > 0.5);
        assert!((out[super::RAMP_FRAMES * 2] - 0.5).abs() < 1e-6);

        // Pausing performs a micro fade-out to silence rather than a hard step.
        shared.paused.store(true, Ordering::Release);
        render(128, &mut out);
        assert!(out[0] > 0.0 && out[0] < 0.5);
        assert!((out[(super::RAMP_FRAMES - 1) * 2]).abs() < 1e-6);
        assert!((out[super::RAMP_FRAMES * 2]).abs() < 1e-6);

        // Resuming fades back up from 0.0 per stereo frame.
        shared.paused.store(false, Ordering::Release);
        render(128, &mut out);
        assert!(out[0] > 0.0 && out[0] < 0.1);
        assert!((out[0] - out[1]).abs() < 1e-6);
        assert!((out[super::RAMP_FRAMES * 2] - 0.5).abs() < 1e-6);
    }
}
