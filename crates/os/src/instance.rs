//! Single instance: the first process holds a lock file and listens on a
//! loopback port; a second start asks it to show its window and exits.
//! Standard library only (`File::try_lock`, TCP on 127.0.0.1).

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

const ACTIVATE: &str = "032player activate";
const LOCK_FILE: &str = "instance.lock";
const PORT_FILE: &str = "instance.port";

pub enum Instance {
    /// This is the only instance; keep the guard alive until exit.
    Primary(PrimaryGuard),
    /// Another instance runs; `notified` tells whether it was asked to
    /// show its window.
    Secondary { notified: bool },
}

/// Holds the lock (`None` when locking is unavailable).
pub struct PrimaryGuard {
    _lock: Option<File>,
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
            return Instance::Primary(PrimaryGuard { _lock: None });
        }
    };
    match lock.try_lock() {
        Ok(()) => {
            listen(dir, on_activate);
            Instance::Primary(PrimaryGuard { _lock: Some(lock) })
        }
        Err(fs::TryLockError::WouldBlock) => Instance::Secondary {
            notified: notify(dir),
        },
        Err(fs::TryLockError::Error(e)) => {
            tracing::warn!("single instance lock failed: {e}");
            Instance::Primary(PrimaryGuard { _lock: None })
        }
    }
}

fn listen(dir: &Path, on_activate: impl Fn() + Send + 'static) {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("single instance listener: {e}");
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    if let Err(e) = fs::write(dir.join(PORT_FILE), port.to_string()) {
        tracing::warn!("single instance port file: {e}");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_ok() && line.trim() == ACTIVATE {
                    on_activate();
                    let _ = (&stream).write_all(b"ok\n");
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("single instance thread: {e}");
    }
}

/// Asks the primary to show itself. The primary may still be starting
/// (port file not written yet), so this retries for a moment.
fn notify(dir: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(port) = fs::read_to_string(dir.join(PORT_FILE))
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
            && send_activate(port)
        {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn send_activate(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(format!("{ACTIVATE}\n").as_bytes())
        .is_err()
    {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn second_start_activates_the_first() {
        let dir = std::env::temp_dir().join(format!("032player-instance-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let activations = Arc::new(AtomicUsize::new(0));

        let counter = activations.clone();
        let first = acquire(&dir, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(matches!(first, Instance::Primary(_)));

        // File locks are per open file description, so a second open in
        // the same process behaves like another process.
        let second = acquire(&dir, || {});
        assert!(matches!(second, Instance::Secondary { notified: true }));
        assert_eq!(activations.load(Ordering::SeqCst), 1);

        drop(first);
        let third = acquire(&dir, || {});
        assert!(matches!(third, Instance::Primary(_)));
        drop(third);
        let _ = fs::remove_dir_all(&dir);
    }
}
