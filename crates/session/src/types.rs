use serde::{Deserialize, Serialize};

/// Why a playback session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    ClientClosed,
    Completed,
    Error,
    Replaced,
    Skipped,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Buffering,
    End,
    Heartbeat,
    Pause,
    Resume,
    Seek,
    Start,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Buffering,
    Paused,
    Playing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextKind {
    #[serde(rename = "dailyMix")]
    DailyMix,
}

/// Where a play came from (capability `features.playbackContext`). Sent on a
/// fresh start only; a resumed chain inherits it server-side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackContext {
    pub id: i64,
    pub position: i64,
    #[serde(rename = "type")]
    pub kind: ContextKind,
}

impl PlaybackContext {
    pub fn daily_mix(id: i64, position: i64) -> Self {
        Self {
            id,
            position,
            kind: ContextKind::DailyMix,
        }
    }
}

/// Drops contexts the server would reject (`id > 0`, `0 <= position <= 999`).
pub fn normalize_playback_context(context: Option<&PlaybackContext>) -> Option<PlaybackContext> {
    let c = context?;
    if c.id <= 0 || !(0..=999).contains(&c.position) {
        return None;
    }
    Some(c.clone())
}

/// Optional explicit position / duration of an event. `None` falls back to the probe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventPosition {
    pub duration_millis: Option<u64>,
    pub position_millis: Option<u64>,
}

/// Body of `POST /api/v1/playback/events`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackEventBody {
    pub client_id: String,
    pub client_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<PlaybackContext>,
    pub duration_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<EndReason>,
    pub position_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from_session_id: Option<String>,
    pub seq: u64,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PlaybackState>,
    pub track_id: i64,
    #[serde(rename = "type")]
    pub kind: EventType,
}

#[derive(Clone, Debug, Default)]
pub struct PlayOptions {
    pub context: Option<PlaybackContext>,
    pub duration_millis: Option<u64>,
    pub initial_state: Option<PlaybackState>,
    pub position_millis: Option<u64>,
    pub reason: Option<EndReason>,
}

/// Identifies one delivery handed out by [`crate::PlaybackSession::poll_transmit`].
pub type DeliveryId = u64;

/// One event the driver must send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transmit {
    pub id: DeliveryId,
    pub body: PlaybackEventBody,
    /// Best effort send that bypasses the queue (app close). Its outcome is
    /// never reported back.
    pub keepalive: bool,
}

/// Result of sending a [`Transmit`].
#[derive(Clone, Debug, PartialEq)]
pub enum TransportOutcome {
    Response {
        status: u16,
        json: serde_json::Value,
    },
    /// The transport decided not to send (e.g. not signed in).
    NoResponse,
    /// Network failure: retried with bounded backoff.
    NetworkError,
}

impl TransportOutcome {
    pub fn response(status: u16, json: serde_json::Value) -> Self {
        Self::Response { status, json }
    }
}

/// Reported when the server rejects a session in a way the client cannot recover from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub reason: String,
    pub session_id: String,
    pub track_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDebug {
    pub blocked: bool,
    pub client_id: Option<String>,
    pub ended: Option<bool>,
    pub has_played: Option<bool>,
    pub queue_length: usize,
    pub seq: u64,
    pub session_id: Option<String>,
    pub state: Option<PlaybackState>,
}
