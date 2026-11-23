use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_registry::LOCAL_MACHINE;
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, S_FALSE, S_OK};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSIDFromString, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, EnumThreadWindows, GetDesktopWindow,
    IsWindowVisible, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};
use windows_sys::core::{BOOL, GUID};

use crate::ffi::{
    ASE_NOT_PRESENT, ASE_OK, ASE_SUCCESS, ASIO_FALSE, ASIO_TRUE, AsioBool, AsioBufferInfo,
    AsioCallbacks, AsioChannelInfo, AsioErrorCode, AsioTime, CLSID_MM_DEVICE_ENUMERATOR, IAsio,
    IID_IMM_DEVICE_ENUMERATOR, IMMDevice, IMMDeviceEnumerator, K_ASIO_BUFFER_SIZE_CHANGE,
    K_ASIO_ENGINE_VERSION, K_ASIO_LATENCIES_CHANGED, K_ASIO_RESET_REQUEST, K_ASIO_RESYNC_REQUEST,
    K_ASIO_SELECTOR_SUPPORTED, K_ASIO_SUPPORTS_TIME_INFO,
};
use crate::slot::{CallbackSlot, StreamSlot, StreamSlotGuard};
use crate::{
    AsioCallbackContext, AsioDriverInfo, AsioError, AsioSampleType, AsioStreamConfig,
    RenderCallback, Result, choose_buffer_frames, write_stereo_channel_buffers,
};

static STREAM_SLOT: StreamSlot = StreamSlot::new();
static ACTIVE_CALLBACK: CallbackSlot<CallbackState> = CallbackSlot::new();
static STANDALONE_CPL: Mutex<Option<StandaloneCpl>> = Mutex::new(None);

struct StandaloneCpl {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StandaloneCpl {
    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn close_standalone_control_panel() {
    let old = STANDALONE_CPL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(mut cpl) = old {
        cpl.stop();
    }
}

unsafe extern "system" fn check_visible_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` points to a valid `bool` on the stack of `has_visible_thread_window`.
    unsafe {
        if IsWindowVisible(hwnd) != 0 {
            *(lparam as *mut bool) = true;
            return 0; // stop enumerating
        }
    }
    1
}

fn has_visible_thread_window() -> bool {
    let mut found = false;
    // SAFETY: `GetCurrentThreadId` returns the current thread id and `check_visible_window`
    // only writes through `&raw mut found` while `EnumThreadWindows` is on the stack.
    unsafe {
        EnumThreadWindows(
            GetCurrentThreadId(),
            Some(check_visible_window),
            (&raw mut found) as LPARAM,
        );
    }
    found
}

struct CallbackState {
    iasio: usize, // *mut IAsio as usize (used only for output_ready during callback)
    supports_output_ready: AtomicBool,
    active: AtomicBool,
    reset_requested: AtomicBool,
    rendered_frames: Arc<AtomicU64>,
    frames: usize,
    sample_rate: f64,
    sample_type: AsioSampleType,
    buffers: [[usize; 2]; 2], // [double_buffer_index][0=L, 1=R]
    render: Mutex<RenderInner>,
}

struct RenderInner {
    callback: RenderCallback,
    scratch_lr: Vec<f32>,
}

impl CallbackState {
    fn invoke(&self, double_buffer_index: i32) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        let idx = match double_buffer_index {
            0 => 0,
            1 => 1,
            _ => return,
        };
        self.fill_buffers(idx);
    }

    fn fill_buffers(&self, idx: usize) {
        let Some(bps) = self.sample_type.bytes_per_sample() else {
            return;
        };
        let Some(byte_len) = self.frames.checked_mul(bps) else {
            return;
        };
        let Some(needed) = self.frames.checked_mul(2) else {
            return;
        };
        let ptr_l = self.buffers[idx][0] as *mut u8;
        let ptr_r = self.buffers[idx][1] as *mut u8;
        if ptr_l.is_null()
            || ptr_r.is_null()
            || (ptr_l as usize).abs_diff(ptr_r as usize) < byte_len
        {
            return;
        }

        // Serialize mutable access to the DMA buffers using `render`'s lock so
        // even a buggy driver calling `bufferSwitch` concurrently cannot alias
        // `&mut [u8]` references.
        let mut guard = match self.render.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return,
            Err(TryLockError::Poisoned(_)) => {
                // Previous callback panicked; silence further callbacks safely.
                self.active.store(false, Ordering::Release);
                return;
            }
        };

        // SAFETY: `ptr_l` and `ptr_r` are non-null, non-overlapping for `byte_len`
        // bytes, serialized by `guard`, and guaranteed valid until `ACTIVE_CALLBACK.clear()`
        // completes before `dispose_buffers()`.
        let (left, right) = unsafe {
            (
                std::slice::from_raw_parts_mut(ptr_l, byte_len),
                std::slice::from_raw_parts_mut(ptr_r, byte_len),
            )
        };

        if guard.scratch_lr.len() < needed {
            guard.scratch_lr.resize(needed, 0.0);
        }
        let inner = &mut *guard;
        let ctx = AsioCallbackContext {
            frames: self.frames,
            sample_rate: self.sample_rate,
        };
        (inner.callback)(ctx, &mut inner.scratch_lr[..needed]);
        let _ = write_stereo_channel_buffers(
            self.sample_type,
            &inner.scratch_lr[..needed],
            left,
            right,
            self.frames,
        );
        drop(guard);

        self.rendered_frames
            .fetch_add(self.frames as u64, Ordering::Relaxed);

        if self.supports_output_ready.load(Ordering::Relaxed) {
            let raw = self.iasio as *mut IAsio;
            if !raw.is_null() && self.active.load(Ordering::Acquire) {
                // SAFETY: `raw` remains valid while the caller holds `ACTIVE_CALLBACK`'s read guard.
                let rc = unsafe { ((*(*raw).vtbl).output_ready)(raw) };
                if rc == ASE_NOT_PRESENT {
                    self.supports_output_ready.store(false, Ordering::Relaxed);
                }
            }
        }
    }
}

unsafe extern "C" fn on_buffer_switch(double_buffer_index: i32, _direct_process: AsioBool) {
    let _ = ACTIVE_CALLBACK.with_active(|state| state.invoke(double_buffer_index));
}

unsafe extern "C" fn on_buffer_switch_time_info(
    params: *mut AsioTime,
    double_buffer_index: i32,
    direct_process: AsioBool,
) -> *mut AsioTime {
    let _ = ACTIVE_CALLBACK.with_active(|state| state.invoke(double_buffer_index));
    let _ = direct_process;
    params
}

unsafe extern "C" fn on_sample_rate_did_change(_s_rate: f64) {
    let _ = ACTIVE_CALLBACK.with_active(|state| {
        state.reset_requested.store(true, Ordering::Release);
    });
}

unsafe extern "C" fn on_asio_message(
    selector: i32,
    value: i32,
    _message: *mut c_void,
    _opt: *mut f64,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| match selector {
        K_ASIO_SELECTOR_SUPPORTED => match value {
            K_ASIO_ENGINE_VERSION
            | K_ASIO_RESET_REQUEST
            | K_ASIO_BUFFER_SIZE_CHANGE
            | K_ASIO_RESYNC_REQUEST
            | K_ASIO_LATENCIES_CHANGED
            | K_ASIO_SUPPORTS_TIME_INFO => 1,
            _ => 0,
        },
        K_ASIO_ENGINE_VERSION => 2,
        K_ASIO_RESET_REQUEST | K_ASIO_BUFFER_SIZE_CHANGE => {
            let _ = ACTIVE_CALLBACK.with_active(|state| {
                state.reset_requested.store(true, Ordering::Release);
            });
            1
        }
        K_ASIO_RESYNC_REQUEST | K_ASIO_LATENCIES_CHANGED | K_ASIO_SUPPORTS_TIME_INFO => 1,
        _ => 0,
    }))
    .unwrap_or(0)
}

pub fn list_drivers() -> Vec<AsioDriverInfo> {
    let Ok(asio_key) = LOCAL_MACHINE.open("SOFTWARE\\ASIO") else {
        return Vec::new();
    };
    let Ok(subkeys) = asio_key.keys() else {
        return Vec::new();
    };
    let mut drivers = Vec::new();
    for key_name in subkeys {
        let Ok(sub) = asio_key.open(&key_name) else {
            continue;
        };
        let Ok(clsid) = sub.get_string("CLSID") else {
            continue;
        };
        let name = sub
            .get_string("Description")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| key_name.clone());
        drivers.push(AsioDriverInfo {
            key: key_name,
            name,
            clsid,
        });
    }
    drivers
}

pub fn default_audio_endpoint_guid() -> Option<String> {
    // SAFETY: standard COM initialization and IMMDeviceEnumerator query.
    unsafe {
        let hr = CoInitializeEx(null(), COINIT_MULTITHREADED as u32);
        let initialized = hr == S_OK || hr == S_FALSE;
        let mut enumerator: *mut IMMDeviceEnumerator = null_mut();
        let hr_create = CoCreateInstance(
            &CLSID_MM_DEVICE_ENUMERATOR,
            null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IMM_DEVICE_ENUMERATOR,
            (&raw mut enumerator).cast(),
        );
        if hr_create < 0 || enumerator.is_null() {
            if initialized {
                CoUninitialize();
            }
            return None;
        }
        let mut device: *mut IMMDevice = null_mut();
        // eRender = 0, eMultimedia = 1
        let hr_ep =
            ((*(*enumerator).vtbl).get_default_audio_endpoint)(enumerator, 0, 1, &raw mut device);
        let guid = if hr_ep >= 0 && !device.is_null() {
            let mut id_ptr = null_mut();
            let hr_id = ((*(*device).vtbl).get_id)(device, &raw mut id_ptr);
            let out = if hr_id >= 0 && !id_ptr.is_null() {
                let len = (0..).take_while(|&i| *id_ptr.add(i) != 0).count();
                let wide = std::slice::from_raw_parts(id_ptr, len);
                let full = String::from_utf16_lossy(wide);
                CoTaskMemFree(id_ptr.cast());
                extract_endpoint_guid(&full)
            } else {
                None
            };
            ((*(*device).vtbl).release)(device);
            out
        } else {
            None
        };
        ((*(*enumerator).vtbl).release)(enumerator);
        if initialized {
            CoUninitialize();
        }
        guid
    }
}

fn extract_endpoint_guid(id: &str) -> Option<String> {
    let start = id.rfind('{')?;
    let end = id[start..].find('}')? + start;
    Some(id[start..=end].to_string())
}

fn parse_clsid(clsid: &str) -> Result<GUID> {
    let mut wide: Vec<u16> = clsid.encode_utf16().collect();
    wide.push(0);
    let mut guid = GUID::from_u128(0);
    // SAFETY: `wide` is NUL-terminated UTF-16 and `guid` is valid writable memory.
    let hr = unsafe { CLSIDFromString(wide.as_ptr(), &raw mut guid) };
    if hr < 0 {
        return Err(AsioError::InvalidClsid(clsid.to_owned()));
    }
    Ok(guid)
}

struct ComGuard(bool);

impl ComGuard {
    fn init_sta() -> Result<Self> {
        // SAFETY: initializes COM apartment for the current thread.
        let hr = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        if hr == S_OK || hr == S_FALSE {
            Ok(Self(true))
        } else if hr as u32 == 0x8001_0106 {
            // RPC_E_CHANGED_MODE: COM already initialized on this thread.
            Ok(Self(false))
        } else {
            Err(AsioError::Com(hr as u32, "CoInitializeEx failed".into()))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches a successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Hidden window owned by the STA thread so driver `init()` and `controlPanel()`
/// receive an HWND attached to the pumping message loop.
struct HwndGuard {
    hwnd: HWND,
    owned: bool,
}

impl HwndGuard {
    fn create() -> Self {
        let class_name: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let title: Vec<u16> = "032player-asio\0".encode_utf16().collect();
        // SAFETY: creates a hidden window using the built-in STATIC class.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if !hwnd.is_null() {
            Self { hwnd, owned: true }
        } else {
            // SAFETY: fallback to the desktop window if hidden window creation fails.
            let desktop = unsafe { GetDesktopWindow() };
            Self {
                hwnd: desktop,
                owned: false,
            }
        }
    }

    fn as_ptr(&self) -> *mut c_void {
        self.hwnd
    }
}

impl Drop for HwndGuard {
    fn drop(&mut self) {
        if self.owned && !self.hwnd.is_null() {
            // SAFETY: `hwnd` was created on this thread by `CreateWindowExW`.
            unsafe {
                DestroyWindow(self.hwnd);
            }
        }
    }
}

struct IAsioHandle {
    raw: *mut IAsio,
    _hwnd: HwndGuard,
    _com: ComGuard,
}

impl IAsioHandle {
    fn open(clsid: &str) -> Result<Self> {
        let com = ComGuard::init_sta()?;
        let guid = parse_clsid(clsid)?;
        let mut raw: *mut IAsio = null_mut();
        // SAFETY: `guid` is both CLSID and IID for Windows ASIO COM drivers.
        let hr = unsafe {
            CoCreateInstance(
                &guid,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &guid,
                (&raw mut raw).cast(),
            )
        };
        if hr < 0 || raw.is_null() {
            return Err(AsioError::Com(
                hr as u32,
                format!("CoCreateInstance failed for {clsid} (last={})", unsafe {
                    GetLastError()
                }),
            ));
        }
        let hwnd = HwndGuard::create();
        let handle = Self {
            raw,
            _hwnd: hwnd,
            _com: com,
        };
        // SAFETY: `handle._hwnd` stays alive for the lifetime of the IASIO handle.
        let ok = unsafe { ((*(*handle.raw).vtbl).init)(handle.raw, handle._hwnd.as_ptr()) };
        if ok != ASIO_TRUE {
            let msg = handle.error_message();
            return Err(AsioError::Driver {
                code: ASE_NOT_PRESENT,
                message: if msg.is_empty() {
                    "IASIO::init failed (device disconnected or busy)".into()
                } else {
                    msg
                },
            });
        }
        Ok(handle)
    }

    fn error_message(&self) -> String {
        let mut buf = [0 as c_char; 128];
        // SAFETY: `buf` is 128 bytes as specified by ASIO `getErrorMessage`.
        unsafe { ((*(*self.raw).vtbl).get_error_message)(self.raw, buf.as_mut_ptr()) };
        c_char_buf_to_string(&buf)
    }

    fn check(&self, code: AsioErrorCode, what: &str) -> Result<()> {
        if code == ASE_OK || code == ASE_SUCCESS {
            return Ok(());
        }
        let msg = self.error_message();
        Err(AsioError::Driver {
            code,
            message: if msg.is_empty() {
                what.to_owned()
            } else {
                format!("{what}: {msg}")
            },
        })
    }

    fn get_channels(&self) -> Result<(i32, i32)> {
        let (mut ins, mut outs) = (0, 0);
        // SAFETY: valid pointers for output counts.
        let rc =
            unsafe { ((*(*self.raw).vtbl).get_channels)(self.raw, &raw mut ins, &raw mut outs) };
        self.check(rc, "getChannels")?;
        Ok((ins, outs))
    }

    fn get_buffer_size(&self) -> Result<(i32, i32, i32, i32)> {
        let (mut min, mut max, mut pref, mut gran) = (0, 0, 0, 0);
        // SAFETY: valid pointers for buffer sizes.
        let rc = unsafe {
            ((*(*self.raw).vtbl).get_buffer_size)(
                self.raw,
                &raw mut min,
                &raw mut max,
                &raw mut pref,
                &raw mut gran,
            )
        };
        self.check(rc, "getBufferSize")?;
        Ok((min, max, pref, gran))
    }

    fn can_sample_rate(&self, rate: f64) -> bool {
        // SAFETY: checks hardware sample rate capability.
        unsafe { ((*(*self.raw).vtbl).can_sample_rate)(self.raw, rate) == ASE_OK }
    }

    fn get_sample_rate(&self) -> Result<f64> {
        let mut rate = 0.0;
        // SAFETY: valid f64 pointer.
        let rc = unsafe { ((*(*self.raw).vtbl).get_sample_rate)(self.raw, &raw mut rate) };
        self.check(rc, "getSampleRate")?;
        Ok(rate)
    }

    fn set_sample_rate(&self, rate: f64) -> Result<()> {
        if !self.can_sample_rate(rate) {
            return Err(AsioError::UnsupportedSampleRate(rate));
        }
        // SAFETY: switches driver hardware sample rate.
        let rc = unsafe { ((*(*self.raw).vtbl).set_sample_rate)(self.raw, rate) };
        self.check(rc, "setSampleRate")
    }

    fn get_output_channel_info(&self, channel: i32) -> Result<AsioChannelInfo> {
        let mut info = AsioChannelInfo {
            channel,
            is_input: ASIO_FALSE,
            ..Default::default()
        };
        // SAFETY: `info` is packed(4) matching ASIO ABI.
        let rc = unsafe { ((*(*self.raw).vtbl).get_channel_info)(self.raw, &raw mut info) };
        self.check(rc, "getChannelInfo")?;
        Ok(info)
    }

    fn get_output_latency(&self) -> i32 {
        let (mut ins, mut outs) = (0, 0);
        // SAFETY: valid pointers for latency frames.
        let rc =
            unsafe { ((*(*self.raw).vtbl).get_latencies)(self.raw, &raw mut ins, &raw mut outs) };
        if rc == ASE_OK { outs.max(0) } else { 0 }
    }

    fn create_buffers(
        &self,
        infos: &mut [AsioBufferInfo; 2],
        buffer_size: i32,
        callbacks: &mut AsioCallbacks,
    ) -> Result<()> {
        // SAFETY: `infos` has length 2 matching `num_channels = 2`, `callbacks` lives as long as buffers.
        let rc = unsafe {
            ((*(*self.raw).vtbl).create_buffers)(
                self.raw,
                infos.as_mut_ptr(),
                2,
                buffer_size,
                callbacks,
            )
        };
        self.check(rc, "createBuffers")
    }

    fn start(&self) -> Result<()> {
        // SAFETY: starts ASIO streaming.
        let rc = unsafe { ((*(*self.raw).vtbl).start)(self.raw) };
        self.check(rc, "start")
    }

    fn stop(&self) -> Result<()> {
        // SAFETY: stops ASIO streaming.
        let rc = unsafe { ((*(*self.raw).vtbl).stop)(self.raw) };
        self.check(rc, "stop")
    }

    fn dispose_buffers(&self) -> Result<()> {
        // SAFETY: frees driver buffers.
        let rc = unsafe { ((*(*self.raw).vtbl).dispose_buffers)(self.raw) };
        self.check(rc, "disposeBuffers")
    }

    fn control_panel(&self) -> Result<()> {
        // SAFETY: asks driver to open its configuration window.
        let rc = unsafe { ((*(*self.raw).vtbl).control_panel)(self.raw) };
        self.check(rc, "controlPanel")
    }
}

impl Drop for IAsioHandle {
    fn drop(&mut self) {
        // SAFETY: releases the COM interface.
        unsafe {
            ((*(*self.raw).vtbl).release)(self.raw);
        }
    }
}

fn c_char_buf_to_string(buf: &[c_char]) -> String {
    // SAFETY: `buf` is a fixed-size C char buffer; CStr scans up to NUL or we clamp.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), buf.len()) };
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

pub fn probe_driver(clsid: &str, sample_rate: f64) -> Result<AsioStreamConfig> {
    let clsid = clsid.to_owned();
    thread::Builder::new()
        .name("asio-probe".into())
        .spawn(move || {
            let handle = IAsioHandle::open(&clsid)?;
            let (_, outs) = handle.get_channels()?;
            if outs < 2 {
                return Err(AsioError::NotEnoughChannels(outs));
            }
            if !handle.can_sample_rate(sample_rate) {
                return Err(AsioError::UnsupportedSampleRate(sample_rate));
            }
            let (min_sz, max_sz, pref, gran) = handle.get_buffer_size()?;
            let buffer_frames = choose_buffer_frames(min_sz, max_sz, pref, gran);
            let ch0 = handle.get_output_channel_info(0)?;
            let sample_type = AsioSampleType::from_raw(ch0.sample_type);
            if !sample_type.is_supported() {
                return Err(AsioError::UnsupportedFormat(sample_type));
            }
            Ok(AsioStreamConfig {
                sample_rate,
                buffer_size: usize::try_from(buffer_frames).unwrap_or(512),
                sample_type,
                output_channels: usize::try_from(outs).unwrap_or(2),
            })
        })
        .map_err(|_| AsioError::Stopped)?
        .join()
        .map_err(|_| AsioError::Stopped)?
}

pub fn open_control_panel_standalone(clsid: &str) -> Result<()> {
    close_standalone_control_panel();
    let clsid = clsid.to_owned();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_w = shutdown.clone();
    let (init_tx, init_rx) = mpsc::sync_channel::<Result<()>>(1);
    let thread = thread::Builder::new()
        .name("asio-cpl".into())
        .spawn(move || {
            let handle = match IAsioHandle::open(&clsid) {
                Ok(h) => h,
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                    return;
                }
            };
            let res = handle.control_panel();
            let ok = res.is_ok();
            let _ = init_tx.send(res);
            if !ok {
                return;
            }
            // Keep the COM STA apartment alive while a modeless in-process control panel
            // window stays visible, or for a short grace window for external panel launchers.
            let grace_deadline = Instant::now() + Duration::from_secs(2);
            while !shutdown_w.load(Ordering::Acquire) {
                pump_messages();
                if Instant::now() >= grace_deadline && !has_visible_thread_window() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        })
        .map_err(|_| AsioError::Stopped)?;

    match init_rx.recv_timeout(Duration::from_millis(250)) {
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Ok(Ok(())) | Err(mpsc::RecvTimeoutError::Timeout) => {
            // Either the panel opened asynchronously or `control_panel()` is running
            // modally on the STA thread; keep the host handle tracked until closed.
            *STANDALONE_CPL.lock().unwrap_or_else(|e| e.into_inner()) = Some(StandaloneCpl {
                shutdown,
                thread: Some(thread),
            });
            Ok(())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = thread.join();
            Err(AsioError::Stopped)
        }
    }
}

enum ActorCmd {
    SetSampleRate(f64, mpsc::SyncSender<Result<AsioStreamConfig>>),
    OpenControlPanel(mpsc::SyncSender<Result<()>>),
    Shutdown,
}

pub struct AsioOutputHandle {
    cmd: mpsc::Sender<ActorCmd>,
    config: Arc<Mutex<AsioStreamConfig>>,
    rendered_frames: Arc<AtomicU64>,
    output_latency: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl AsioOutputHandle {
    pub fn start(clsid: &str, sample_rate: f64, render: RenderCallback) -> Result<Self> {
        let clsid = clsid.to_owned();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<SelfInit>>(1);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ActorCmd>();
        let thread = thread::Builder::new()
            .name("asio-com".into())
            .spawn(move || actor_main(&clsid, sample_rate, render, init_tx, cmd_rx))
            .map_err(|_| AsioError::Stopped)?;

        let init = init_rx.recv().map_err(|_| AsioError::Stopped)??;
        Ok(Self {
            cmd: cmd_tx,
            config: init.config,
            rendered_frames: init.rendered_frames,
            output_latency: init.output_latency,
            last_error: init.last_error,
            thread: Some(thread),
        })
    }

    pub fn config(&self) -> AsioStreamConfig {
        *self.config.lock().expect("config lock")
    }

    pub fn set_sample_rate(&self, sample_rate: f64) -> Result<AsioStreamConfig> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.cmd
            .send(ActorCmd::SetSampleRate(sample_rate, tx))
            .map_err(|_| AsioError::Stopped)?;
        rx.recv().map_err(|_| AsioError::Stopped)?
    }

    pub fn rendered_frames(&self) -> u64 {
        self.rendered_frames.load(Ordering::Relaxed)
    }

    pub fn output_latency_frames(&self) -> u64 {
        self.output_latency.load(Ordering::Relaxed)
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.lock().expect("asio error lock").take()
    }

    pub fn open_control_panel(&self) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.cmd
            .send(ActorCmd::OpenControlPanel(tx))
            .map_err(|_| AsioError::Stopped)?;
        // Return promptly when the driver opens a modal dialog on `asio-com`, so callers
        // (decode worker / engine mutex) never stall waiting for the user to close the panel.
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(res) => res,
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(AsioError::Stopped),
        }
    }
}

impl Drop for AsioOutputHandle {
    fn drop(&mut self) {
        let _ = self.cmd.send(ActorCmd::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct SelfInit {
    config: Arc<Mutex<AsioStreamConfig>>,
    rendered_frames: Arc<AtomicU64>,
    output_latency: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
}

struct ActiveStream {
    handle: IAsioHandle,
    callbacks: Box<AsioCallbacks>,
    cb_state: Option<Arc<CallbackState>>,
    config: AsioStreamConfig,
    rendered_frames: Arc<AtomicU64>,
    output_latency: Arc<AtomicU64>,
    // Dropped last (after `ActiveStream::drop` and `IAsioHandle::drop`).
    _slot: StreamSlotGuard,
}

impl ActiveStream {
    fn open(clsid: &str, sample_rate: f64, render: RenderCallback) -> Result<Self> {
        close_standalone_control_panel();
        let slot = STREAM_SLOT.try_acquire()?;
        let handle = IAsioHandle::open(clsid)?;
        let (_, outs) = handle.get_channels()?;
        if outs < 2 {
            return Err(AsioError::NotEnoughChannels(outs));
        }

        let current_rate = handle.get_sample_rate().unwrap_or(0.0);
        if (current_rate - sample_rate).abs() > 0.5 {
            handle.set_sample_rate(sample_rate)?;
        }

        let callbacks = Box::new(AsioCallbacks {
            buffer_switch: Some(on_buffer_switch),
            sample_rate_did_change: Some(on_sample_rate_did_change),
            asio_message: Some(on_asio_message),
            buffer_switch_time_info: Some(on_buffer_switch_time_info),
        });

        let mut stream = Self {
            handle,
            callbacks,
            cb_state: None,
            config: AsioStreamConfig {
                sample_rate,
                buffer_size: 0,
                sample_type: AsioSampleType::Int32Lsb,
                output_channels: usize::try_from(outs).unwrap_or(2),
            },
            rendered_frames: Arc::new(AtomicU64::new(0)),
            output_latency: Arc::new(AtomicU64::new(0)),
            _slot: slot,
        };
        stream.setup_and_start(sample_rate, render)?;
        Ok(stream)
    }

    fn setup_and_start(&mut self, sample_rate: f64, render: RenderCallback) -> Result<()> {
        let (min_sz, max_sz, pref, gran) = self.handle.get_buffer_size()?;
        let buffer_frames = choose_buffer_frames(min_sz, max_sz, pref, gran);
        let frames_usize = usize::try_from(buffer_frames).unwrap_or(512);
        let ch0 = self.handle.get_output_channel_info(0)?;
        let sample_type = AsioSampleType::from_raw(ch0.sample_type);
        let Some(bps) = sample_type
            .is_supported()
            .then(|| sample_type.bytes_per_sample())
            .flatten()
        else {
            return Err(AsioError::UnsupportedFormat(sample_type));
        };
        let byte_len = frames_usize
            .checked_mul(bps)
            .ok_or_else(|| AsioError::Driver {
                code: ASE_NOT_PRESENT,
                message: "ASIO buffer size overflow".into(),
            })?;
        let needed_samples = frames_usize
            .checked_mul(2)
            .ok_or_else(|| AsioError::Driver {
                code: ASE_NOT_PRESENT,
                message: "ASIO frame count overflow".into(),
            })?;

        let mut infos = [
            AsioBufferInfo {
                is_input: ASIO_FALSE,
                channel_num: 0,
                buffers: [null_mut(), null_mut()],
            },
            AsioBufferInfo {
                is_input: ASIO_FALSE,
                channel_num: 1,
                buffers: [null_mut(), null_mut()],
            },
        ];

        self.handle
            .create_buffers(&mut infos, buffer_frames, &mut self.callbacks)?;

        let buffers = [
            [infos[0].buffers[0] as usize, infos[1].buffers[0] as usize],
            [infos[0].buffers[1] as usize, infos[1].buffers[1] as usize],
        ];
        for [ptr_l, ptr_r] in buffers {
            if ptr_l == 0 || ptr_r == 0 || ptr_l.abs_diff(ptr_r) < byte_len {
                let _ = self.handle.dispose_buffers();
                return Err(AsioError::Driver {
                    code: ASE_NOT_PRESENT,
                    message: "driver returned invalid or overlapping ASIO buffer pointers".into(),
                });
            }
        }

        let state = Arc::new(CallbackState {
            iasio: self.handle.raw as usize,
            supports_output_ready: AtomicBool::new(true),
            active: AtomicBool::new(true),
            reset_requested: AtomicBool::new(false),
            rendered_frames: self.rendered_frames.clone(),
            frames: frames_usize,
            sample_rate,
            sample_type,
            buffers,
            render: Mutex::new(RenderInner {
                callback: render,
                scratch_lr: vec![0.0; needed_samples],
            }),
        });

        ACTIVE_CALLBACK.install(Arc::clone(&state));
        self.cb_state = Some(state);

        if let Err(e) = self.handle.start() {
            let _ = self.teardown_buffers();
            return Err(e);
        }

        let latency = u64::try_from(self.handle.get_output_latency()).unwrap_or(0);
        self.output_latency.store(latency, Ordering::Relaxed);
        self.config.sample_rate = sample_rate;
        self.config.buffer_size = frames_usize;
        self.config.sample_type = sample_type;
        Ok(())
    }

    fn teardown_buffers(&mut self) -> Option<RenderCallback> {
        let state = self.cb_state.take()?;
        state.active.store(false, Ordering::Release);
        let _ = self.handle.stop();
        // Detaches the global callback slot and waits for every in-flight
        // callback to return before disposing driver DMA buffers.
        ACTIVE_CALLBACK.clear();
        let _ = self.handle.dispose_buffers();
        let state = Arc::try_unwrap(state)
            .ok()
            .expect("ACTIVE_CALLBACK cleared; ActiveStream holds the sole Arc");
        let inner = state.render.into_inner().unwrap_or_else(|e| e.into_inner());
        Some(inner.callback)
    }

    fn reconfigure(&mut self, sample_rate: f64) -> Result<AsioStreamConfig> {
        if !self.handle.can_sample_rate(sample_rate) {
            return Err(AsioError::UnsupportedSampleRate(sample_rate));
        }
        let render = self.teardown_buffers().ok_or(AsioError::Stopped)?;
        let current = self.handle.get_sample_rate().unwrap_or(0.0);
        if (current - sample_rate).abs() > 0.5
            && let Err(e) = self.handle.set_sample_rate(sample_rate)
        {
            // Restore previous sample rate if possible.
            let _ = self.setup_and_start(self.config.sample_rate, render);
            return Err(e);
        }
        self.setup_and_start(sample_rate, render)?;
        Ok(self.config)
    }
}

impl Drop for ActiveStream {
    fn drop(&mut self) {
        let _ = self.teardown_buffers();
    }
}

fn actor_main(
    clsid: &str,
    sample_rate: f64,
    render: RenderCallback,
    init_tx: mpsc::SyncSender<Result<SelfInit>>,
    cmd_rx: mpsc::Receiver<ActorCmd>,
) {
    let mut stream = match ActiveStream::open(clsid, sample_rate, render) {
        Ok(s) => s,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    let shared_cfg = Arc::new(Mutex::new(stream.config));
    let rendered_frames = stream.rendered_frames.clone();
    let output_latency = stream.output_latency.clone();
    let last_error = Arc::new(Mutex::new(None));
    let _ = init_tx.send(Ok(SelfInit {
        config: shared_cfg.clone(),
        rendered_frames,
        output_latency,
        last_error: last_error.clone(),
    }));

    loop {
        pump_messages();
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                ActorCmd::SetSampleRate(rate, reply) => {
                    let res = stream.reconfigure(rate);
                    if let Ok(cfg) = res {
                        *shared_cfg.lock().expect("config lock") = cfg;
                    }
                    let _ = reply.send(res);
                }
                ActorCmd::OpenControlPanel(reply) => {
                    let _ = reply.send(stream.handle.control_panel());
                }
                ActorCmd::Shutdown => return,
            }
        }

        if let Some(state) = stream.cb_state.as_ref()
            && state.reset_requested.swap(false, Ordering::AcqRel)
        {
            let target_rate = stream.config.sample_rate;
            tracing::info!(sample_rate = target_rate, "ASIO reset requested by driver");
            match stream.reconfigure(target_rate) {
                Ok(cfg) => *shared_cfg.lock().expect("config lock") = cfg,
                Err(e) => {
                    tracing::warn!("ASIO reset reconfigure failed: {e}");
                    *last_error.lock().expect("asio error lock") = Some(e.to_string());
                    return;
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn pump_messages() {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: dispatches pending Windows messages for COM STA apartment on this thread.
    unsafe {
        while PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_guid_from_mmdevice_id() {
        assert_eq!(
            extract_endpoint_guid("{0.0.0.00000000}.{ef05e2a9-d17f-46af-8fed-022ced0cb378}"),
            Some("{ef05e2a9-d17f-46af-8fed-022ced0cb378}".into())
        );
    }

    #[test]
    #[ignore = "requires physical FiiO K9 Pro / FiiO ASIO Driver connected on Windows"]
    fn probe_fiio_asio_hardware() {
        let drivers = list_drivers();
        println!("ASIO drivers: {drivers:?}");
        println!("Default endpoint GUID: {:?}", default_audio_endpoint_guid());
        let fiio = crate::find_fiio_driver().expect("FiiO ASIO Driver not registered");
        let cfg = probe_driver(&fiio.clsid, 44_100.0).expect("probe 44.1kHz");
        println!("44.1kHz config: {cfg:?}");
        let cfg96 = probe_driver(&fiio.clsid, 96_000.0).expect("probe 96kHz");
        println!("96kHz config: {cfg96:?}");
    }

    #[test]
    #[ignore = "requires physical FiiO K9 Pro / FiiO ASIO Driver connected on Windows"]
    fn stream_fiio_asio_silent_smoke_and_rate_switch() {
        let fiio = crate::find_fiio_driver().expect("FiiO ASIO Driver not registered");
        let output =
            crate::AsioOutput::start(&fiio.clsid, 44_100.0, Box::new(|_ctx, buf| buf.fill(0.0)))
                .expect("start 44.1kHz ASIO stream");
        thread::sleep(Duration::from_millis(200));
        let frames_44 = output.rendered_frames();
        assert!(frames_44 > 0, "expected rendered frames at 44.1kHz, got 0");
        let cfg96 = output
            .set_sample_rate(96_000.0)
            .expect("switch ASIO sample rate to 96kHz");
        assert!((cfg96.sample_rate - 96_000.0).abs() < 1.0);
        thread::sleep(Duration::from_millis(200));
        let frames_96 = output.rendered_frames();
        assert!(
            frames_96 > frames_44,
            "expected rendered frames to advance after 96kHz switch: {frames_44} -> {frames_96}"
        );
        drop(output);
    }
}
