//! Sonos control over UPnP on the local network: SSDP discovery, zone
//! group topology and the speaker-side play queue (AVTransport). No 032
//! knowledge; `core` decides what goes into the queue.

mod didl;
mod discovery;
#[cfg(feature = "fake")]
pub mod fake;
mod soap;
mod speaker;

use std::net::{IpAddr, UdpSocket};
use std::time::Duration;

pub use didl::{Item, QueueEntry};
pub use discovery::discover;
pub use speaker::{PlayMode, Speaker, Status, TransportState};

/// A room (zone group), addressed through its coordinator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Room {
    /// Coordinator UUID (`RINCON_...`).
    pub id: String,
    /// `客厅` or `客厅 + 卧室` for a group.
    pub name: String,
    /// `http://192.168.1.30:1400`.
    pub base_url: String,
    pub host: String,
    /// Visible players in the group.
    pub member_count: usize,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{action}: UPnP 错误 {code} {description}")]
    Upnp {
        action: String,
        code: u32,
        description: String,
    },
    #[error("{action}: HTTP {status}")]
    Http { action: String, status: u16 },
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    pub fn upnp_code(&self) -> Option<u32> {
        match self {
            Self::Upnp { code, .. } => Some(*code),
            _ => None,
        }
    }
}

/// HTTP client for speakers: LAN only, so never through the system proxy.
pub fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

/// This machine's address on the route to `host` (no packet is sent).
pub fn local_address_towards(host: &str) -> Option<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect((host, 1400)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}
