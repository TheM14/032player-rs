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

/// Active ASIO stream parameters negotiated with the driver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AsioStreamConfig {
    pub sample_rate: f64,
    pub buffer_size: usize,
    pub sample_type: AsioSampleType,
    pub output_channels: usize,
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
        matches!(
            self,
            Self::Int16Lsb
                | Self::Int24Lsb
                | Self::Int32Lsb
                | Self::Float32Lsb
                | Self::Float64Lsb
                | Self::Int32Lsb16
                | Self::Int32Lsb18
                | Self::Int32Lsb20
                | Self::Int32Lsb24
        )
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
        _ => dst.fill(0),
    }
}

/// FiiO ASIO driver registry key & CLSID on Windows.
pub const FIIO_ASIO_NAME: &str = "FiiO ASIO Driver";
pub const FIIO_ASIO_CLSID: &str = "{6B3BA606-8664-4426-8994-0F1E6FE6199F}";
pub const FIIO_ENDPOINT_MATCH: &str = "FiiO Q series";

/// Enumerates ASIO drivers registered under `HKLM\SOFTWARE\ASIO`.
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

/// Finds the registered FiiO ASIO driver if installed.
#[must_use]
pub fn find_fiio_driver() -> Option<AsioDriverInfo> {
    list_drivers().into_iter().find(|d| {
        d.key.eq_ignore_ascii_case(FIIO_ASIO_NAME)
            || d.name.eq_ignore_ascii_case(FIIO_ASIO_NAME)
            || d.clsid.eq_ignore_ascii_case(FIIO_ASIO_CLSID)
    })
}

/// Returns the GUID (e.g. `{ef05e2a9-d17f-46af-8fed-022ced0cb378}`) of the
/// current Windows default audio render endpoint, if available.
#[must_use]
pub fn default_audio_endpoint_guid() -> Option<String> {
    #[cfg(windows)]
    {
        driver::default_audio_endpoint_guid()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Probes an ASIO driver by opening it briefly and reading its capabilities.
pub fn probe_driver(clsid: &str, sample_rate: f64) -> Result<AsioStreamConfig> {
    #[cfg(windows)]
    {
        driver::probe_driver(clsid, sample_rate)
    }
    #[cfg(not(windows))]
    {
        let _ = (clsid, sample_rate);
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
    /// Opens the ASIO driver identified by `clsid` at `sample_rate` Hz and starts
    /// audio streaming. `render` receives `(context, interleaved_stereo_f32)`.
    pub fn start(clsid: &str, sample_rate: f64, render: RenderCallback) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: driver::AsioOutputHandle::start(clsid, sample_rate, render)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (clsid, sample_rate, render);
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
}
