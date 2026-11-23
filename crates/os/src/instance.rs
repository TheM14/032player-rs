//! Single instance: the first process holds a lock file and listens on a
//! loopback port; a second start asks it to show its window and exits.
//! Standard library only (`File::try_lock`, TCP on 127.0.0.1) plus a
//! per-instance random nonce stored in a private port file.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const ACTIVATE: &str = "032player activate";
const LOCK_FILE: &str = "instance.lock";
const PORT_FILE: &str = "instance.port";
const MAX_REQUEST_BYTES: u64 = 128;
const MAX_REPLY_BYTES: u64 = 32;

pub enum Instance {
    /// This is the only instance; keep the guard alive until exit.
    Primary(PrimaryGuard),
    /// Another instance runs; `notified` tells whether it was asked to
    /// show its window.
    Secondary { notified: bool },
}

/// Holds the listener shutdown handle, port file path and process lock
/// (`None` when locking is unavailable).
pub struct PrimaryGuard {
    _listener: Option<ListenerHandle>,
    // Dropped last so the lock stays held until the listener stops and
    // removes `instance.port`.
    _lock: Option<File>,
}

struct ListenerHandle {
    port: u16,
    port_file: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake `listener.accept()` immediately.
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.port_file);
    }
}

/// Becomes the primary instance in `dir`, or notifies the running one.
/// `on_activate` runs (on a background thread) when a later start asks
/// the primary to show itself.
pub fn acquire(dir: &Path, on_activate: impl Fn() + Send + 'static) -> Instance {
    let _ = fs::create_dir_all(dir);
    let lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
    {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!("single instance lock unavailable: {e}");
            return Instance::Primary(PrimaryGuard {
                _listener: None,
                _lock: None,
            });
        }
    };
    match lock.try_lock() {
        Ok(()) => {
            let listener = listen(dir, on_activate);
            Instance::Primary(PrimaryGuard {
                _listener: listener,
                _lock: Some(lock),
            })
        }
        Err(fs::TryLockError::WouldBlock) => Instance::Secondary {
            notified: notify(dir),
        },
        Err(fs::TryLockError::Error(e)) => {
            tracing::warn!("single instance lock failed: {e}");
            Instance::Primary(PrimaryGuard {
                _listener: None,
                _lock: None,
            })
        }
    }
}

fn write_port_file(dir: &Path, port: u16, nonce: &str) -> std::io::Result<PathBuf> {
    let final_path = dir.join(PORT_FILE);
    let tmp_path = dir.join(format!("{PORT_FILE}.{nonce}.tmp"));
    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = opts.open(&tmp_path)?;
        writeln!(file, "{port} {nonce}")?;
        file.flush()?;
        drop(file);
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result.map(|()| final_path)
}

fn listen(dir: &Path, on_activate: impl Fn() + Send + 'static) -> Option<ListenerHandle> {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("single instance listener: {e}");
            return None;
        }
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            tracing::warn!("single instance local_addr: {e}");
            return None;
        }
    };
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let port_file = match write_port_file(dir, port, &nonce) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("single instance port file: {e}");
            return None;
        }
    };
    let expected = format!("{ACTIVATE} {nonce}");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if stop_flag.load(Ordering::Acquire) {
                    break;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
                if let Some(line) = read_bounded_line(&stream, MAX_REQUEST_BYTES)
                    && ct_eq(line.trim().as_bytes(), expected.as_bytes())
                {
                    on_activate();
                    let _ = (&stream).write_all(b"ok\n");
                }
            }
        });
    match spawned {
        Ok(thread) => Some(ListenerHandle {
            port,
            port_file,
            stop,
            thread: Some(thread),
        }),
        Err(e) => {
            let _ = fs::remove_file(&port_file);
            tracing::warn!("single instance thread: {e}");
            None
        }
    }
}

fn read_bounded_line(stream: &TcpStream, max_bytes: u64) -> Option<String> {
    let cap = usize::try_from(max_bytes).unwrap_or(128);
    let mut buf = Vec::with_capacity(cap);
    let mut limited = BufReader::new(stream).take(max_bytes);
    match limited.read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            if !buf.ends_with(b"\n") {
                return None;
            }
            String::from_utf8(buf).ok()
        }
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn parse_port_file(content: &str) -> Option<(u16, String)> {
    let mut parts = content.split_whitespace();
    let port = parts.next()?.parse::<u16>().ok()?;
    let nonce = parts.next()?;
    if nonce.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((port, nonce.to_owned()))
}

/// Asks the primary to show itself. The primary may still be starting
/// (port file not written yet), so this retries for a moment.
fn notify(dir: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some((port, nonce)) = fs::read_to_string(dir.join(PORT_FILE))
            .ok()
            .and_then(|s| parse_port_file(&s))
            && send_activate(port, &nonce)
        {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn send_activate(port: u16, nonce: &str) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    if stream
        .write_all(format!("{ACTIVATE} {nonce}\n").as_bytes())
        .is_err()
    {
        return false;
    }
    read_bounded_line(&stream, MAX_REPLY_BYTES).is_some_and(|reply| reply.trim() == "ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "032player-instance-{}-{}-{}",
            name,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn second_start_activates_the_first() {
        let dir = test_dir("activate");
        let activations = Arc::new(AtomicUsize::new(0));

        let counter = activations.clone();
        let first = acquire(&dir, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(matches!(first, Instance::Primary(_)));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join(PORT_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        // File locks are per open file description, so a second open in
        // the same process behaves like another process.
        let second = acquire(&dir, || {});
        assert!(matches!(second, Instance::Secondary { notified: true }));
        assert_eq!(activations.load(Ordering::SeqCst), 1);

        drop(first);
        assert!(!dir.join(PORT_FILE).exists());
        let third = acquire(&dir, || {});
        assert!(matches!(third, Instance::Primary(_)));
        drop(third);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_wrong_nonce_and_oversized_payloads() {
        let dir = test_dir("security");
        let activations = Arc::new(AtomicUsize::new(0));

        let counter = activations.clone();
        let first = acquire(&dir, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(matches!(first, Instance::Primary(_)));

        let content = fs::read_to_string(dir.join(PORT_FILE)).unwrap();
        let (port, nonce) = parse_port_file(&content).unwrap();

        // Wrong nonce is rejected.
        assert!(!send_activate(port, "00000000000000000000000000000000"));
        assert_eq!(activations.load(Ordering::SeqCst), 0);

        // Legacy unauthenticated request is rejected.
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(format!("{ACTIVATE}\n").as_bytes()).unwrap();
            let _ = s.shutdown(std::net::Shutdown::Write);
            let mut reply = Vec::new();
            let _ = s.read_to_end(&mut reply);
            assert!(reply.is_empty());
        }
        assert_eq!(activations.load(Ordering::SeqCst), 0);

        // Oversized payload without newline is bounded and rejected without stalling.
        {
            let mut s = TcpStream::connect(addr).unwrap();
            let junk = vec![b'A'; 16 * 1024];
            let _ = s.write_all(&junk);
            let _ = s.shutdown(std::net::Shutdown::Write);
            let mut reply = Vec::new();
            let _ = s.read_to_end(&mut reply);
            assert!(reply.is_empty());
        }
        assert_eq!(activations.load(Ordering::SeqCst), 0);

        // Legitimate activation with the real nonce still works afterwards.
        assert!(send_activate(port, &nonce));
        assert_eq!(activations.load(Ordering::SeqCst), 1);

        drop(first);
        let _ = fs::remove_dir_all(&dir);
    }
}
