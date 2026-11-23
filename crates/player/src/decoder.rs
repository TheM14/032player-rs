//! `symphonia` PCM decoder for FLAC / WAV / ALAC / MP3 / AAC / Vorbis.
//! Opus streams are explicitly rejected so the hybrid engine can fall back
//! to WASAPI (libmpv).

use std::io;

use symphonia::core::audio::{AudioBufferRef, SampleBuffer, Signal};
use symphonia::core::codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

const MAX_CONSECUTIVE_FRAME_ERRORS: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("Opus is not supported in ASIO mode")]
    UnsupportedOpus,
    #[error("no playable audio track found")]
    NoTrack,
    #[error("missing sample rate in audio stream")]
    MissingSampleRate,
    #[error("cancelled")]
    Cancelled,
    #[error("decode: {0}")]
    Other(String),
}

pub struct TrackDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sample_rate: u32,
    duration_secs: Option<f64>,
    time_base: Option<TimeBase>,
    scratch: Vec<f32>,
}

impl TrackDecoder {
    pub fn open(
        source: Box<dyn MediaSource>,
        url_hint: Option<&str>,
        mime_hint: Option<&str>,
    ) -> Result<Self, DecodeError> {
        let mut hint = Hint::new();
        if let Some(url) = url_hint
            && let Ok(parsed) = url::Url::parse(url)
            && let Some(ext) = parsed
                .path()
                .rsplit('/')
                .next()
                .and_then(|seg| seg.rsplit_once('.'))
                .map(|(_, ext)| ext)
        {
            hint.with_extension(ext);
        }
        if let Some(mime) = mime_hint {
            hint.mime_type(mime);
        }

        let mss = MediaSourceStream::new(source, Default::default());
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| match e {
                SymphoniaError::IoError(io_e) if io_e.kind() == io::ErrorKind::Interrupted => {
                    DecodeError::Cancelled
                }
                other => DecodeError::Other(other.to_string()),
            })?;

        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(DecodeError::NoTrack)?;

        if track.codec_params.codec == CODEC_TYPE_OPUS {
            return Err(DecodeError::UnsupportedOpus);
        }

        let track_id = track.id;
        let sample_rate = track
            .codec_params
            .sample_rate
            .ok_or(DecodeError::MissingSampleRate)?;
        let time_base = track.codec_params.time_base;
        let duration_secs = match (track.codec_params.n_frames, time_base) {
            (Some(frames), Some(tb)) => {
                let t = tb.calc_time(frames);
                Some(t.seconds as f64 + t.frac)
            }
            (Some(frames), None) => Some(frames as f64 / f64::from(sample_rate)),
            _ => None,
        };

        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| {
                let msg = e.to_string();
                if msg.to_ascii_lowercase().contains("opus") {
                    DecodeError::UnsupportedOpus
                } else {
                    DecodeError::Other(msg)
                }
            })?;

        Ok(Self {
            format,
            decoder,
            track_id,
            sample_rate,
            duration_secs,
            time_base,
            scratch: Vec::new(),
        })
    }

    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[must_use]
    pub const fn duration_secs(&self) -> Option<f64> {
        self.duration_secs
    }

    /// Decodes the next packet into `out` as interleaved stereo `f32` samples.
    /// Returns `Ok(false)` on clean EOF.
    pub fn next_packet_stereo(&mut self, out: &mut Vec<f32>) -> Result<bool, DecodeError> {
        out.clear();
        let mut errors = 0;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::Interrupted => {
                    return Err(DecodeError::Cancelled);
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(DecodeError::Other(e.to_string())),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    to_interleaved_stereo(&decoded, &mut self.scratch, out);
                    if out.is_empty() {
                        continue;
                    }
                    return Ok(true);
                }
                Err(SymphoniaError::DecodeError(_)) if errors < MAX_CONSECUTIVE_FRAME_ERRORS => {
                    errors += 1;
                    continue;
                }
                Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::Interrupted => {
                    return Err(DecodeError::Cancelled);
                }
                Err(e) => return Err(DecodeError::Other(e.to_string())),
            }
        }
    }

    /// Seeks to `seconds` and resets decoder state, returning the landed time.
    pub fn seek(&mut self, seconds: f64) -> Result<f64, DecodeError> {
        let clamped = seconds.max(0.0);
        let target = Time::from(clamped);
        let seeked = match self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: target,
                track_id: Some(self.track_id),
            },
        ) {
            Ok(s) => s,
            Err(SymphoniaError::IoError(io_e)) if io_e.kind() == io::ErrorKind::Interrupted => {
                return Err(DecodeError::Cancelled);
            }
            Err(_) => self
                .format
                .seek(
                    SeekMode::Coarse,
                    SeekTo::Time {
                        time: target,
                        track_id: Some(self.track_id),
                    },
                )
                .map_err(|e| match e {
                    SymphoniaError::IoError(io_e) if io_e.kind() == io::ErrorKind::Interrupted => {
                        DecodeError::Cancelled
                    }
                    other => DecodeError::Other(other.to_string()),
                })?,
        };
        self.decoder.reset();
        let actual = if let Some(tb) = self.time_base {
            let t = tb.calc_time(seeked.actual_ts);
            t.seconds as f64 + t.frac
        } else {
            seeked.actual_ts as f64 / f64::from(self.sample_rate)
        };
        Ok(actual)
    }
}

fn to_interleaved_stereo(decoded: &AudioBufferRef<'_>, scratch: &mut Vec<f32>, out: &mut Vec<f32>) {
    let frames = decoded.frames();
    if frames == 0 {
        return;
    }
    let spec = *decoded.spec();
    let channels = spec.channels.count().max(1);

    if channels == 2
        && let AudioBufferRef::F32(buf) = decoded
    {
        let l = buf.chan(0);
        let r = buf.chan(1);
        out.reserve(frames * 2);
        for i in 0..frames {
            out.push(l[i]);
            out.push(r[i]);
        }
        return;
    }

    let mut sample_buf = SampleBuffer::<f32>::new(frames as u64, spec);
    sample_buf.copy_interleaved_ref(decoded.clone());
    let samples = sample_buf.samples();
    scratch.clear();
    scratch.extend_from_slice(samples);

    out.reserve(frames * 2);
    match channels {
        1 => {
            for &s in scratch.iter() {
                out.push(s);
                out.push(s);
            }
        }
        2 => {
            out.extend_from_slice(scratch);
        }
        _ => {
            for frame in scratch.chunks_exact(channels) {
                out.push(frame[0]);
                out.push(frame[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn make_wav(sample_rate: u32, frames: usize) -> Vec<u8> {
        let data_bytes = u32::try_from(frames * 4).unwrap();
        let riff_len = 36 + data_bytes;
        let mut out = Vec::with_capacity(44 + data_bytes as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&riff_len.to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // stereo
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for i in 0..frames {
            let v = ((i % 100) as i16) * 100;
            out.extend_from_slice(&v.to_le_bytes());
            out.extend_from_slice(&(-v).to_le_bytes());
        }
        out
    }

    #[test]
    fn decodes_and_seeks_wav_to_stereo_f32() {
        let wav = make_wav(48_000, 48_000);
        let mut dec = TrackDecoder::open(
            Box::new(Cursor::new(wav)),
            Some("http://example/test.wav"),
            Some("audio/wav"),
        )
        .unwrap();
        assert_eq!(dec.sample_rate(), 48_000);
        assert!((dec.duration_secs().unwrap() - 1.0).abs() < 0.01);

        let mut pcm = Vec::new();
        let mut total_frames = 0;
        while dec.next_packet_stereo(&mut pcm).unwrap() {
            total_frames += pcm.len() / 2;
        }
        assert_eq!(total_frames, 48_000);

        let landed = dec.seek(0.5).unwrap();
        assert!((landed - 0.5).abs() < 0.05);
    }
}
