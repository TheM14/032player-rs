//! Seekable, auto-reconnecting HTTP stream source for `symphonia`.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use symphonia::core::io::MediaSource;

const LOOKBEHIND_BYTES: usize = 2 * 1024 * 1024;
const SMALL_FORWARD_SKIP: u64 = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RETRIES: usize = 4;

pub fn build_http_client(proxy: Option<&str>) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .no_proxy();
    if let Some(proxy_url) = proxy.filter(|s| !s.trim().is_empty()) {
        builder = builder.proxy(reqwest::Proxy::all(proxy_url)?);
    }
    builder.build()
}

/// Synchronous `MediaSource` backed by an async `reqwest` stream driven on a
/// dedicated Tokio runtime handle.
pub struct HttpMediaSource {
    client: reqwest::Client,
    url: String,
    rt: tokio::runtime::Handle,
    cancel: Arc<AtomicBool>,
    response: Option<reqwest::Response>,
    content_type: Option<String>,
    total_len: Option<u64>,
    accept_ranges: bool,
    /// Absolute stream offset of `buf[0]`.
    buf_start: u64,
    buf: VecDeque<u8>,
    /// Current logical read position.
    pos: u64,
    eof: bool,
}

impl HttpMediaSource {
    pub fn open(
        client: reqwest::Client,
        url: impl Into<String>,
        rt: tokio::runtime::Handle,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let mut src = Self {
            client,
            url: url.into(),
            rt,
            cancel,
            response: None,
            content_type: None,
            total_len: None,
            accept_ranges: false,
            buf_start: 0,
            buf: VecDeque::with_capacity(64 * 1024),
            pos: 0,
            eof: false,
        };
        src.open_at(0)?;
        Ok(src)
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    fn open_at(&mut self, offset: u64) -> io::Result<()> {
        if self.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let client = self.client.clone();
        let url = self.url.clone();
        let resp = self
            .rt
            .block_on(async move {
                let mut req = client.get(&url);
                if offset > 0 {
                    req = req.header(RANGE, format!("bytes={offset}-"));
                }
                req.send().await
            })
            .map_err(|e| io::Error::other(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(io::Error::other(format!("HTTP {status}")));
        }

        let headers = resp.headers();
        if self.content_type.is_none()
            && let Some(ct) = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok())
        {
            let mime = ct.split(';').next().unwrap_or(ct).trim();
            if !mime.is_empty() {
                self.content_type = Some(mime.to_owned());
            }
        }

        if headers
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("bytes"))
            || status == reqwest::StatusCode::PARTIAL_CONTENT
        {
            self.accept_ranges = true;
        }

        if self.total_len.is_none() {
            if let Some(cr) = headers.get(CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
                self.total_len = parse_content_range_total(cr);
            } else if offset == 0
                && let Some(cl) = headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok())
            {
                self.total_len = cl.parse::<u64>().ok();
            }
        }

        self.response = Some(resp);
        self.buf.clear();
        self.buf_start = if offset > 0 && status == reqwest::StatusCode::OK {
            // Server ignored Range and restarted from 0.
            0
        } else {
            offset
        };
        self.pos = offset;
        self.eof = false;
        if offset > 0 && self.buf_start == 0 {
            self.discard_until(offset)?;
        }
        Ok(())
    }

    fn discard_until(&mut self, target: u64) -> io::Result<()> {
        let mut scratch = [0u8; 16 * 1024];
        while self.buf_end() < target {
            let need = usize::try_from((target - self.buf_end()).min(scratch.len() as u64))
                .unwrap_or(scratch.len());
            let n = self.fetch_chunk(&mut scratch[..need])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF before seek target",
                ));
            }
        }
        Ok(())
    }

    fn buf_end(&self) -> u64 {
        self.buf_start + self.buf.len() as u64
    }

    fn trim_lookbehind(&mut self) {
        let keep_from = self.pos.saturating_sub(LOOKBEHIND_BYTES as u64);
        if keep_from > self.buf_start {
            let drop = usize::try_from(keep_from - self.buf_start)
                .unwrap_or(self.buf.len())
                .min(self.buf.len());
            self.buf.drain(..drop);
            self.buf_start += drop as u64;
        }
    }

    fn fetch_chunk(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut attempt = 0;
        loop {
            if self.cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let Some(resp) = self.response.as_mut() else {
                self.open_at(self.buf_end())?;
                continue;
            };
            match self.rt.block_on(resp.chunk()) {
                Ok(Some(bytes)) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    let copy_n = bytes.len().min(out.len());
                    out[..copy_n].copy_from_slice(&bytes[..copy_n]);
                    self.buf.extend(&bytes[..]);
                    self.trim_lookbehind();
                    return Ok(copy_n);
                }
                Ok(None) => {
                    let end = self.buf_end();
                    if let Some(total) = self.total_len
                        && end < total
                        && self.accept_ranges
                        && attempt < MAX_RETRIES
                    {
                        attempt += 1;
                        self.response = None;
                        self.backoff(attempt)?;
                        self.open_at(end)?;
                        continue;
                    }
                    self.eof = true;
                    return Ok(0);
                }
                Err(e) => {
                    self.response = None;
                    if !self.accept_ranges || attempt >= MAX_RETRIES {
                        return Err(io::Error::other(e.to_string()));
                    }
                    attempt += 1;
                    let resume_at = self.buf_end();
                    self.backoff(attempt)?;
                    self.open_at(resume_at)?;
                }
            }
        }
    }

    fn backoff(&self, attempt: usize) -> io::Result<()> {
        let ms = match attempt {
            1 => 200,
            2 => 500,
            3 => 1_000,
            _ => 2_000,
        };
        let steps = ms / 50;
        for _ in 0..steps {
            if self.cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

impl Read for HttpMediaSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }

        // If cursor is outside the current rolling window, reposition first.
        let end = self.buf_end();
        if self.pos < self.buf_start {
            if !self.accept_ranges {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "server does not support Range requests",
                ));
            }
            self.open_at(self.pos)?;
        } else if self.pos > end {
            let gap = self.pos - end;
            if gap > SMALL_FORWARD_SKIP && self.accept_ranges {
                self.open_at(self.pos)?;
            } else {
                self.discard_until(self.pos)?;
            }
        }

        // Now `self.pos >= self.buf_start` and `self.pos <= self.buf_end()`.
        if self.pos == self.buf_end() {
            if self.eof {
                return Ok(0);
            }
            let mut tmp = [0u8; 64 * 1024];
            let n = self.fetch_chunk(&mut tmp)?;
            if n == 0 {
                return Ok(0);
            }
        }

        let offset = usize::try_from(self.pos - self.buf_start).unwrap_or(0);
        let avail = self.buf.len().saturating_sub(offset);
        let copy_n = avail.min(out.len());
        let (s1, s2) = self.buf.as_slices();
        if offset < s1.len() {
            let n1 = copy_n.min(s1.len() - offset);
            out[..n1].copy_from_slice(&s1[offset..offset + n1]);
            if n1 < copy_n {
                out[n1..copy_n].copy_from_slice(&s2[..copy_n - n1]);
            }
        } else {
            let o2 = offset - s1.len();
            out[..copy_n].copy_from_slice(&s2[o2..o2 + copy_n]);
        }
        self.pos += copy_n as u64;
        self.trim_lookbehind();
        Ok(copy_n)
    }
}

impl Seek for HttpMediaSource {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let target = match style {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(delta) => self
                .pos
                .checked_add_signed(delta)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before 0"))?,
            SeekFrom::End(delta) => {
                let len = self.total_len.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "unknown stream length")
                })?;
                len.checked_add_signed(delta)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before 0"))?
            }
        };
        if !self.accept_ranges
            && (target < self.buf_start || target > self.buf_end() + SMALL_FORWARD_SKIP)
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stream is not seekable",
            ));
        }
        self.pos = target;
        self.eof = false;
        Ok(self.pos)
    }
}

impl MediaSource for HttpMediaSource {
    fn is_seekable(&self) -> bool {
        self.accept_ranges || self.total_len.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.total_len
    }
}

fn parse_content_range_total(header: &str) -> Option<u64> {
    let (_, total) = header.split_once('/')?;
    if total == "*" {
        return None;
    }
    total.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range_header() {
        assert_eq!(parse_content_range_total("bytes 0-99/12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes 0-99/*"), None);
    }
}
