//! Clean-room Windows COM ASIO audio output host.
//!
//! Enumerates ASIO drivers from the Windows registry (`HKLM\SOFTWARE\ASIO`)
//! and drives `IASIO` via its COM vtable without depending on the proprietary
//! Steinberg ASIO SDK.

#[cfg(windows)]
mod driver;
#[cfg(windows)]
mod ffi;
mod ring;
mod slot;

pub use ring::{SpscConsumer, SpscProducer, spsc_ring};

use std::fmt;

/// Standard PCM sample rates probed when inspecting an ASIO driver.
pub const STANDARD_SAMPLE_RATES: &[u32] = &[
    22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

#[derive(Debug, thiserror::Error)]
pub enum AsioError {
    #[error("ASIO is only supported on Windows")]
    Unsupported,
    #[error("an ASIO output stream is already active")]
    AlreadyActive,
    #[error("ASIO driver not found: {0}")]
    DriverNotFound(String),
    #[error("invalid ASIO CLSID: {0}")]
    InvalidClsid(String),
    #[error("COM error (0x{0:08x}): {1}")]
    Com(u32, String),
    #[error("ASIO error ({code}): {message}")]
    Driver { code: i32, message: String },
    #[error("unsupported ASIO sample format: {0}")]
    UnsupportedFormat(AsioSampleType),
    #[error("ASIO driver has fewer than 2 output channels ({0})")]
    NotEnoughChannels(i32),
    #[error(
        "invalid ASIO channel pair ({left}, {right}) for device with {available} output channels"
    )]
    InvalidChannelPair {
        left: usize,
        right: usize,
        available: usize,
    },
    #[error("ASIO output channels have mismatched sample formats ({left} vs {right})")]
    ChannelFormatMismatch {
        left: AsioSampleType,
        right: AsioSampleType,
    },
    #[error("ASIO sample rate {0} Hz is not supported by the device")]
    UnsupportedSampleRate(f64),
    #[error("ASIO host thread stopped")]
    Stopped,
}

pub type Result<T> = std::result::Result<T, AsioError>;

/// Registered ASIO driver entry from `HKLM\SOFTWARE\ASIO`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioDriverInfo {
    pub key: String,
    pub name: String,
    pub clsid: String,
}

/// A stereo pair of output channels exposed by an ASIO driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioChannelPair {
    pub left: usize,
    pub right: usize,
    pub name: String,
}

/// Target ASIO driver and stereo output channel pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioTarget {
    pub clsid: String,
    pub left_channel: usize,
    pub right_channel: usize,
}

impl AsioTarget {
    #[must_use]
    pub fn new(clsid: impl Into<String>, left_channel: usize, right_channel: usize) -> Self {
        Self {
            clsid: clsid.into(),
            left_channel,
            right_channel,
        }
    }

    #[must_use]
    pub fn stereo_default(clsid: impl Into<String>) -> Self {
        Self::new(clsid, 0, 1)
    }
}

/// Hardware capabilities reported when probing an ASIO driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioCapabilities {
    pub output_channels: usize,
    pub channel_pairs: Vec<AsioChannelPair>,
    pub supported_sample_rates: Vec<u32>,
    pub sample_type: AsioSampleType,
    pub buffer_size: usize,
}

/// Active ASIO stream parameters negotiated with the driver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AsioStreamConfig {
    pub sample_rate: f64,
    pub buffer_size: usize,
    pub sample_type: AsioSampleType,
    pub output_channels: usize,
    pub left_channel: usize,
    pub right_channel: usize,
}

/// Callback invocation passed to the render closure on each ASIO `bufferSwitch`.
pub struct AsioCallbackContext {
    pub frames: usize,
    pub sample_rate: f64,
}

/// Clean-room representation of ASIO sample formats (`ASIOSampleType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsioSampleType {
    Int16Msb,
    Int24Msb,
    Int32Msb,
    Float32Msb,
    Float64Msb,
    Int32Msb16,
    Int32Msb18,
    Int32Msb20,
    Int32Msb24,
    Int16Lsb,
    Int24Lsb,
    Int32Lsb,
    Float32Lsb,
    Float64Lsb,
    Int32Lsb16,
    Int32Lsb18,
    Int32Lsb20,
    Int32Lsb24,
    Unknown(i32),
}

impl AsioSampleType {
    #[must_use]
    pub const fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Int16Msb,
            1 => Self::Int24Msb,
            2 => Self::Int32Msb,
            3 => Self::Float32Msb,
            4 => Self::Float64Msb,
            8 => Self::Int32Msb16,
            9 => Self::Int32Msb18,
            10 => Self::Int32Msb20,
            11 => Self::Int32Msb24,
            16 => Self::Int16Lsb,
            17 => Self::Int24Lsb,
            18 => Self::Int32Lsb,
            19 => Self::Float32Lsb,
            20 => Self::Float64Lsb,
            24 => Self::Int32Lsb16,
            25 => Self::Int32Lsb18,
            26 => Self::Int32Lsb20,
            27 => Self::Int32Lsb24,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn bytes_per_sample(self) -> Option<usize> {
        match self {
            Self::Int16Msb | Self::Int16Lsb => Some(2),
            Self::Int24Msb | Self::Int24Lsb => Some(3),
            Self::Int32Msb
            | Self::Float32Msb
            | Self::Int32Msb16
            | Self::Int32Msb18
            | Self::Int32Msb20
            | Self::Int32Msb24
            | Self::Int32Lsb
            | Self::Float32Lsb
            | Self::Int32Lsb16
            | Self::Int32Lsb18
            | Self::Int32Lsb20
            | Self::Int32Lsb24 => Some(4),
            Self::Float64Msb | Self::Float64Lsb => Some(8),
            Self::Unknown(_) => None,
        }
    }

    #[must_use]
    pub const fn is_supported(self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

impl fmt::Display for AsioSampleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Converts interleaved stereo `f32` samples (`[L0, R0, L1, R1, ...]`) into
/// separate driver channel buffers (`left`, `right`) of length `frames * bytes_per_sample`.
pub fn write_stereo_channel_buffers(
    sample_type: AsioSampleType,
    interleaved_lr: &[f32],
    left: &mut [u8],
    right: &mut [u8],
    frames: usize,
) -> Result<()> {
    let bps = sample_type
        .bytes_per_sample()
        .ok_or(AsioError::UnsupportedFormat(sample_type))?;
    if !sample_type.is_supported() {
        return Err(AsioError::UnsupportedFormat(sample_type));
    }
    debug_assert!(left.len() >= frames * bps);
    debug_assert!(right.len() >= frames * bps);

    for i in 0..frames {
        let l = interleaved_lr.get(i * 2).copied().unwrap_or(0.0);
        let r = interleaved_lr.get(i * 2 + 1).copied().unwrap_or(0.0);
        write_sample(sample_type, l, &mut left[i * bps..(i + 1) * bps]);
        write_sample(sample_type, r, &mut right[i * bps..(i + 1) * bps]);
    }
    Ok(())
}

#[inline]
fn write_sample(sample_type: AsioSampleType, sample: f32, dst: &mut [u8]) {
    let clamped = f64::from(sample).clamp(-1.0, 1.0);
    match sample_type {
        AsioSampleType::Int16Msb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 32_767.0).round() as i16;
            dst.copy_from_slice(&v.to_be_bytes());
        }
        AsioSampleType::Int24Msb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 8_388_607.0).round() as i32;
            let b = v.to_be_bytes();
            dst[0] = b[1];
            dst[1] = b[2];
            dst[2] = b[3];
        }
        AsioSampleType::Int32Msb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 2_147_483_647.0).round() as i32;
            dst.copy_from_slice(&v.to_be_bytes());
        }
        AsioSampleType::Float32Msb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = clamped as f32;
            dst.copy_from_slice(&v.to_be_bytes());
        }
        AsioSampleType::Float64Msb => {
            dst.copy_from_slice(&clamped.to_be_bytes());
        }
        AsioSampleType::Int32Msb16 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 32_767.0).round() as i32;
            dst.copy_from_slice(&v.to_be_bytes());
        }
        AsioSampleType::Int32Msb18 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 131_071.0).round() as i32;
            dst.copy_from_slice(&v.to_be_bytes());
        }
        AsioSampleType::Int32Msb20 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 524_287.0).round() as i32;
            dst.copy_from_slice(&v.to_be_bytes());
        }
        AsioSampleType::Int32Msb24 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 8_388_607.0).round() as i32;
            dst.copy_from_slice(&v.to_be_bytes());
        }
        AsioSampleType::Int16Lsb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 32_767.0).round() as i16;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        AsioSampleType::Int24Lsb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 8_388_607.0).round() as i32;
            let b = v.to_le_bytes();
            dst[0] = b[0];
            dst[1] = b[1];
            dst[2] = b[2];
        }
        AsioSampleType::Int32Lsb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 2_147_483_647.0).round() as i32;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        AsioSampleType::Float32Lsb => {
            #[allow(clippy::cast_possible_truncation)]
            let v = clamped as f32;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        AsioSampleType::Float64Lsb => {
            dst.copy_from_slice(&clamped.to_le_bytes());
        }
        AsioSampleType::Int32Lsb16 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 32_767.0).round() as i32;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        AsioSampleType::Int32Lsb18 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 131_071.0).round() as i32;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        AsioSampleType::Int32Lsb20 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 524_287.0).round() as i32;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        AsioSampleType::Int32Lsb24 => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (clamped * 8_388_607.0).round() as i32;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        AsioSampleType::Unknown(_) => dst.fill(0),
    }
}

/// Groups ordered output channels `(channel_index, channel_name)` into stereo
/// output pairs `(2k, 2k + 1)`.
#[must_use]
pub fn build_channel_pairs(channels: &[(usize, String)]) -> Vec<AsioChannelPair> {
    let mut pairs = Vec::with_capacity(channels.len() / 2);
    for [(left, left_name), (right, right_name)] in channels.as_chunks::<2>().0 {
        let name = format_channel_pair_name(*left, left_name, *right, right_name);
        pairs.push(AsioChannelPair {
            left: *left,
            right: *right,
            name,
        });
    }
    pairs
}

fn format_channel_pair_name(
    left: usize,
    left_name: &str,
    right: usize,
    right_name: &str,
) -> String {
    let l = left_name.trim();
    let r = right_name.trim();
    let idx = format!("{}-{}", left + 1, right + 1);
    match (l.is_empty(), r.is_empty()) {
        (true, true) => format!("输出 {idx}"),
        (false, true) => format!("{idx} · {l}"),
        (true, false) => format!("{idx} · {r}"),
        (false, false) if l == r => format!("{idx} · {l}"),
        (false, false) => format!("{idx} · {l} / {r}"),
    }
}

/// Selects a valid ASIO buffer size in frames honoring the driver's `[min, max]`
/// bounds and granularity.
#[must_use]
pub fn choose_buffer_frames(min: i32, max: i32, preferred: i32, granularity: i32) -> i32 {
    if max <= 0 {
        return if preferred > 0 {
            preferred.max(64)
        } else {
            512
        };
    }
    let lo = min.max(1).min(max);
    let mut size = if preferred > 0 { preferred } else { 512 }.clamp(lo, max);
    if granularity > 0 {
        let offset = size - lo;
        let rem = offset % granularity;
        if rem != 0 {
            size = (size - rem).clamp(lo, max);
        }
    }
    size
}

/// Enumerates ASIO drivers registered under `HKLM\SOFTWARE\ASIO` whose COM
/// server DLL exists on disk.
#[must_use]
pub fn list_drivers() -> Vec<AsioDriverInfo> {
    #[cfg(windows)]
    {
        driver::list_drivers()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Probes an ASIO driver by opening it briefly and reading its output channel
/// pairs, sample format, buffer size, and supported sample rates.
pub fn probe_driver(clsid: &str) -> Result<AsioCapabilities> {
    #[cfg(windows)]
    {
        driver::probe_driver(clsid)
    }
    #[cfg(not(windows))]
    {
        let _ = clsid;
        Err(AsioError::Unsupported)
    }
}

/// Opens the ASIO driver's native control panel dialog on a dedicated STA thread.
pub fn open_control_panel_standalone(clsid: &str) -> Result<()> {
    #[cfg(windows)]
    {
        driver::open_control_panel_standalone(clsid)
    }
    #[cfg(not(windows))]
    {
        let _ = clsid;
        Err(AsioError::Unsupported)
    }
}

pub type RenderCallback = Box<dyn FnMut(AsioCallbackContext, &mut [f32]) + Send + 'static>;

/// Safe, `Send + Sync` handle to an active ASIO output stream hosted on a
/// dedicated Windows COM STA thread.
pub struct AsioOutput {
    #[cfg(windows)]
    inner: driver::AsioOutputHandle,
}

impl AsioOutput {
    /// Opens the ASIO driver and output channel pair identified by `target` at
    /// `sample_rate` Hz and starts audio streaming. `render` receives
    /// `(context, interleaved_stereo_f32)`.
    pub fn start(target: &AsioTarget, sample_rate: f64, render: RenderCallback) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: driver::AsioOutputHandle::start(target, sample_rate, render)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (target, sample_rate, render);
            Err(AsioError::Unsupported)
        }
    }

    #[must_use]
    pub fn config(&self) -> AsioStreamConfig {
        #[cfg(windows)]
        {
            self.inner.config()
        }
        #[cfg(not(windows))]
        {
            unreachable!()
        }
    }

    /// Reconfigures the active ASIO stream to a new sample rate (stops,
    /// disposes buffers, sets sample rate, recreates buffers, restarts).
    pub fn set_sample_rate(&self, sample_rate: f64) -> Result<AsioStreamConfig> {
        #[cfg(windows)]
        {
            self.inner.set_sample_rate(sample_rate)
        }
        #[cfg(not(windows))]
        {
            let _ = sample_rate;
            Err(AsioError::Unsupported)
        }
    }

    /// Total frames rendered by hardware callbacks since start/reset.
    #[must_use]
    pub fn rendered_frames(&self) -> u64 {
        #[cfg(windows)]
        {
            self.inner.rendered_frames()
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// Driver-reported output latency in frames (when known).
    #[must_use]
    pub fn output_latency_frames(&self) -> u64 {
        #[cfg(windows)]
        {
            self.inner.output_latency_frames()
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// Returns and clears any asynchronous fatal error reported by the ASIO
    /// host thread (such as a failed hardware reset after disconnect).
    #[must_use]
    pub fn take_error(&self) -> Option<String> {
        #[cfg(windows)]
        {
            self.inner.take_error()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Opens the driver's native control panel on the active STA instance.
    pub fn open_control_panel(&self) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.open_control_panel()
        }
        #[cfg(not(windows))]
        {
            Err(AsioError::Unsupported)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_packing_int16_and_int32_and_int24() {
        let input = [1.0_f32, -1.0, 0.0, 0.5];
        let mut l16 = [0u8; 4];
        let mut r16 = [0u8; 4];
        write_stereo_channel_buffers(AsioSampleType::Int16Lsb, &input, &mut l16, &mut r16, 2)
            .unwrap();
        assert_eq!(i16::from_le_bytes([l16[0], l16[1]]), 32_767);
        assert_eq!(i16::from_le_bytes([r16[0], r16[1]]), -32_767);
        assert_eq!(i16::from_le_bytes([l16[2], l16[3]]), 0);

        let mut l24 = [0u8; 6];
        let mut r24 = [0u8; 6];
        write_stereo_channel_buffers(AsioSampleType::Int24Lsb, &input, &mut l24, &mut r24, 2)
            .unwrap();
        assert_eq!(&l24[0..3], &[0xff, 0xff, 0x7f]);
        assert_eq!(&r24[0..3], &[0x01, 0x00, 0x80]);

        let mut l32 = [0u8; 8];
        let mut r32 = [0u8; 8];
        write_stereo_channel_buffers(AsioSampleType::Int32Lsb, &input, &mut l32, &mut r32, 2)
            .unwrap();
        assert_eq!(
            i32::from_le_bytes(l32[0..4].try_into().unwrap()),
            2_147_483_647
        );
        assert_eq!(
            i32::from_le_bytes(r32[0..4].try_into().unwrap()),
            -2_147_483_647
        );
    }

    #[test]
    fn buffer_size_respects_driver_bounds_and_granularity() {
        assert_eq!(choose_buffer_frames(64, 2048, 512, 64), 512);
        // Fixed small buffer drivers (e.g. min == max == 32) must not be forced up to 64.
        assert_eq!(choose_buffer_frames(32, 32, 32, 0), 32);
        // Out-of-range preferred values are clamped into [min, max].
        assert_eq!(choose_buffer_frames(128, 1024, 4096, 64), 1024);
        assert_eq!(choose_buffer_frames(128, 1024, 0, 64), 512);
        // Granularity alignment stays within [min, max].
        assert_eq!(choose_buffer_frames(64, 1024, 200, 64), 192);
        // Bogus zero bounds fall back safely.
        assert_eq!(choose_buffer_frames(0, 0, 0, 0), 512);
        assert_eq!(choose_buffer_frames(0, 0, 256, 0), 256);
    }

    #[test]
    fn sample_packing_msb() {
        let input = [1.0_f32, -1.0];
        let mut l16 = [0u8; 2];
        let mut r16 = [0u8; 2];
        write_stereo_channel_buffers(AsioSampleType::Int16Msb, &input, &mut l16, &mut r16, 1)
            .unwrap();
        assert_eq!(i16::from_be_bytes(l16), 32_767);
        assert_eq!(i16::from_be_bytes(r16), -32_767);

        let mut l24 = [0u8; 3];
        let mut r24 = [0u8; 3];
        write_stereo_channel_buffers(AsioSampleType::Int24Msb, &input, &mut l24, &mut r24, 1)
            .unwrap();
        assert_eq!(&l24, &[0x7f, 0xff, 0xff]);
        assert_eq!(&r24, &[0x80, 0x00, 0x01]);
    }

    #[test]
    fn builds_stereo_channel_pairs() {
        let channels = vec![
            (0, "MAIN Left".into()),
            (1, "MAIN Right".into()),
            (2, "LOOPBACK Left".into()),
            (3, "LOOPBACK Right".into()),
            (4, "".into()),
            (5, "".into()),
        ];
        let pairs = build_channel_pairs(&channels);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].name, "1-2 · MAIN Left / MAIN Right");
        assert_eq!(pairs[1].name, "3-4 · LOOPBACK Left / LOOPBACK Right");
        assert_eq!(pairs[2].name, "输出 5-6");
    }
}
