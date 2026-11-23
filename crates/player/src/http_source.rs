//! Seekable, auto-reconnecting HTTP stream source for `symphonia`.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use symphonia::core::io::MediaSource;
use tokio::sync::watch;

const LOOKBEHIND_BYTES: usize = 2 * 1024 * 1024;
const SMALL_FORWARD_SKIP: u64 = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RETRIES: usize = 4;

/// Race-free cancellation token that wakes in-flight async HTTP operations immediately.
#[derive(Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = watch::channel(false);
        Self { tx: Arc::new(tx) }
    }

    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    pub async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|&c| c).await;
    }
}

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
    cancel: CancelToken,
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
        cancel: CancelToken,
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
        self.cancel.is_cancelled()
    }

    /// Opens an HTTP stream starting at `offset` (or 0 if the server ignores
    /// `Range`) without modifying the logical cursor `self.pos`.
    fn connect_at(&mut self, offset: u64, reset_buf: bool) -> io::Result<u64> {
        if self.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let client = self.client.clone();
        let url = self.url.clone();
        let cancel = self.cancel.clone();
        let resp = self.rt.block_on(async move {
            let mut req = client.get(&url);
            if offset > 0 {
                req = req.header(RANGE, format!("bytes={offset}-"));
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                }
                res = req.send() => {
                    res.map_err(|e| io::Error::other(e.without_url().to_string()))
                }
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let kind = if status.is_client_error() {
                io::ErrorKind::InvalidInput
            } else {
                io::ErrorKind::Other
            };
            return Err(io::Error::new(kind, format!("HTTP {status}")));
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

        let actual_start = if offset > 0 && status == reqwest::StatusCode::OK {
            // Server ignored Range and restarted from 0.
            0
        } else {
            offset
        };
        self.response = Some(resp);
        if reset_buf || actual_start != self.buf_end() {
            self.buf.clear();
            self.buf_start = actual_start;
        }
        self.eof = false;
        Ok(actual_start)
    }

    fn open_at(&mut self, offset: u64) -> io::Result<()> {
        let actual_start = self.connect_at(offset, true)?;
        if actual_start < offset {
            self.discard_until(offset)?;
        }
        Ok(())
    }

    fn discard_until(&mut self, target: u64) -> io::Result<()> {
        while self.buf_end() < target {
            let n = self.fetch_chunk()?;
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

    fn fetch_chunk(&mut self) -> io::Result<usize> {
        let mut attempt = 0;
        let mut last_err: Option<io::Error> = None;
        loop {
            if self.cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            if self.response.is_none() {
                if attempt > 0 {
                    self.backoff(attempt)?;
                }
                let resume_at = self.buf_end();
                match self.connect_at(resume_at, false) {
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::InvalidInput
                        ) =>
                    {
                        return Err(e);
                    }
                    Err(e) => {
                        if !self.accept_ranges || attempt >= MAX_RETRIES {
                            return Err(e);
                        }
                        attempt += 1;
                        last_err = Some(e);
                        continue;
                    }
                }
            }
            let resp = self.response.as_mut().expect("response opened above");
            let cancel = self.cancel.clone();
            let chunk_res = self.rt.block_on(async {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                    }
                    res = resp.chunk() => {
                        res.map_err(|e| io::Error::other(e.without_url().to_string()))
                    }
                }
            });
            match chunk_res {
                Ok(Some(bytes)) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    self.buf.extend(&bytes[..]);
                    self.trim_lookbehind();
                    return Ok(bytes.len());
                }
                Ok(None) => {
                    let end = self.buf_end();
                    if let Some(total) = self.total_len
                        && end < total
                    {
                        self.response = None;
                        if self.accept_ranges && attempt < MAX_RETRIES {
                            attempt += 1;
                            continue;
                        }
                        return Err(last_err.unwrap_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                format!("truncated HTTP stream at {end}/{total} bytes"),
                            )
                        }));
                    }
                    self.eof = true;
                    return Ok(0);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    self.response = None;
                    return Err(e);
                }
                Err(e) => {
                    self.response = None;
                    if !self.accept_ranges || attempt >= MAX_RETRIES {
                        return Err(e);
                    }
                    attempt += 1;
                    last_err = Some(e);
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
        let cancel = self.cancel.clone();
        self.rt.block_on(async move {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                }
                () = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
            }
        })
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
        if let Some(total) = self.total_len
            && self.pos >= total
        {
            return Ok(0);
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

        // Ensure `self.buf` contains at least one byte at `self.pos`.
        while self.buf_end() <= self.pos {
            if self.eof {
                return Ok(0);
            }
            let n = self.fetch_chunk()?;
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
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf = [0u8; 2048];
        let mut req = Vec::new();
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        String::from_utf8_lossy(&req).to_string()
    }

    fn parse_range_start(req: &str) -> Option<u64> {
        req.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            if !k.eq_ignore_ascii_case("range") {
                return None;
            }
            let start = v.trim().strip_prefix("bytes=")?.split('-').next()?;
            start.parse().ok()
        })
    }

    #[test]
    fn cancel_wakes_stalled_open_immediately() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = read_http_request(&mut s);
                // Hold the socket open without replying.
                std::thread::sleep(Duration::from_secs(5));
            }
        });

        let rt = test_runtime();
        let cancel = CancelToken::new();
        let cancel_clone = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            cancel_clone.cancel();
        });

        let started = Instant::now();
        let err = HttpMediaSource::open(
            test_client(),
            format!("http://{addr}/slow.flac"),
            rt.handle().clone(),
            cancel,
        )
        .err()
        .expect("stalled open must fail on cancel");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel took too long: {:?}",
            started.elapsed()
        );
        drop(server);
    }

    #[test]
    fn reconnect_during_discard_until_preserves_cursor() {
        let payload: Vec<u8> = (0u32..4096).map(|i| (i % 251) as u8).collect();
        let data = Arc::new(payload.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_srv = calls.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let Ok((mut s, _)) = listener.accept() else {
                    break;
                };
                let req = read_http_request(&mut s);
                let n = calls_srv.fetch_add(1, Ordering::SeqCst);
                let start = parse_range_start(&req).unwrap_or(0) as usize;
                if n == 0 {
                    // Initial response: send only the first 512 bytes, then drop mid-stream.
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = s.write_all(header.as_bytes());
                    let _ = s.write_all(&data[..512]);
                } else {
                    // Reconnect: serve from requested range to end.
                    let slice = &data[start..];
                    let header = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        start,
                        data.len() - 1,
                        data.len(),
                        slice.len()
                    );
                    let _ = s.write_all(header.as_bytes());
                    let _ = s.write_all(slice);
                }
            }
        });

        let rt = test_runtime();
        let mut src = HttpMediaSource::open(
            test_client(),
            format!("http://{addr}/track.flac"),
            rt.handle().clone(),
            CancelToken::new(),
        )
        .unwrap();
        let mut head = [0u8; 64];
        src.read_exact(&mut head).unwrap();
        assert_eq!(&head[..], &payload[..64]);

        // Small forward skip (< SMALL_FORWARD_SKIP) triggers discard_until, which hits
        // the dropped connection at byte 512 and must reconnect without overwriting self.pos.
        src.seek(SeekFrom::Start(2048)).unwrap();
        let mut tail = [0u8; 128];
        src.read_exact(&mut tail).unwrap();
        assert_eq!(&tail[..], &payload[2048..2176]);
        assert!(calls.load(Ordering::SeqCst) >= 2);
        drop(server);
    }

    #[test]
    fn reconnect_retries_after_transient_connect_error() {
        let payload: Vec<u8> = (0u32..2048).map(|i| ((i * 7) % 253) as u8).collect();
        let data = Arc::new(payload.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_srv = calls.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut s, _)) = listener.accept() else {
                    break;
                };
                let req = read_http_request(&mut s);
                let n = calls_srv.fetch_add(1, Ordering::SeqCst);
                let start = parse_range_start(&req).unwrap_or(0) as usize;
                match n {
                    0 => {
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            data.len()
                        );
                        let _ = s.write_all(header.as_bytes());
                        let _ = s.write_all(&data[..256]);
                    }
                    1 => {
                        // First reconnect attempt fails transiently with 503.
                        let _ = s.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    _ => {
                        let slice = &data[start..];
                        let header = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            start,
                            data.len() - 1,
                            data.len(),
                            slice.len()
                        );
                        let _ = s.write_all(header.as_bytes());
                        let _ = s.write_all(slice);
                    }
                }
            }
        });

        let rt = test_runtime();
        let mut src = HttpMediaSource::open(
            test_client(),
            format!("http://{addr}/retry.flac"),
            rt.handle().clone(),
            CancelToken::new(),
        )
        .unwrap();
        let mut out = Vec::new();
        src.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
        assert!(calls.load(Ordering::SeqCst) >= 3);
        drop(server);
    }

    #[test]
    fn truncated_stream_without_ranges_returns_unexpected_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = read_http_request(&mut s);
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n",
                );
                let _ = s.write_all(&[0xAB; 200]);
            }
        });

        let rt = test_runtime();
        let mut src = HttpMediaSource::open(
            test_client(),
            format!("http://{addr}/trunc.flac"),
            rt.handle().clone(),
            CancelToken::new(),
        )
        .unwrap();
        let mut out = Vec::new();
        let err = src.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(out.len(), 200);
        drop(server);
    }

    #[test]
    fn parses_content_range_header() {
        assert_eq!(parse_content_range_total("bytes 0-99/12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes 0-99/*"), None);
    }
}
