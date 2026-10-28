//! Drives the sans-IO playback session on tokio: timers become sleeping
//! tasks, transmits become HTTP posts, and both report back to the core loop
//! through [`ReporterMsg`].

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use api::{Capabilities, Client};
use session::{
    DeliveryId, KeyValueStorage, PlaybackAdapter, Scheduler, SessionOptions, TimerId,
    TransportOutcome,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::{AbortHandle, JoinHandle};

#[derive(Debug)]
pub(crate) enum ReporterMsg {
    Timer(TimerId),
    Outcome(DeliveryId, TransportOutcome),
    /// The server rejected this API token (handled by the core loop).
    Unauthorized(String),
}

struct TokioScheduler {
    tx: UnboundedSender<ReporterMsg>,
    next: TimerId,
    timers: HashMap<TimerId, AbortHandle>,
}

impl Scheduler for TokioScheduler {
    fn set_timeout(&mut self, delay_millis: u64) -> TimerId {
        self.timers.retain(|_, h| !h.is_finished());
        self.next += 1;
        let id = self.next;
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_millis)).await;
            let _ = tx.send(ReporterMsg::Timer(id));
        });
        self.timers.insert(id, handle.abort_handle());
        id
    }

    fn clear_timeout(&mut self, id: TimerId) {
        if let Some(handle) = self.timers.remove(&id) {
            handle.abort();
        }
    }
}

pub(crate) struct Reporter {
    adapter: PlaybackAdapter,
    client: Client,
    tx: UnboundedSender<ReporterMsg>,
    keepalive: Vec<JoinHandle<()>>,
}

/// Storage prefix per server, like `music032_playback_${serverId}`.
fn storage_prefix(client: &Client) -> String {
    let key: String = client
        .base_url()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("music032_playback_{key}")
}

impl Reporter {
    pub(crate) fn new(
        client: Client,
        capabilities: &Capabilities,
        storage: Box<dyn KeyValueStorage>,
        tx: UnboundedSender<ReporterMsg>,
    ) -> Self {
        let scheduler = TokioScheduler {
            tx: tx.clone(),
            next: 0,
            timers: HashMap::new(),
        };
        let mut options = SessionOptions::new(
            Box::new(scheduler),
            Box::new(|| uuid::Uuid::new_v4().to_string()),
        )
        .with_client_kind(capabilities.client_kind());
        options.storage = Some(storage);
        options.storage_prefix = storage_prefix(&client);
        let adapter = PlaybackAdapter::new(
            options,
            Box::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            }),
        );
        Self {
            adapter,
            client,
            tx,
            keepalive: Vec::new(),
        }
    }

    /// Applies one batch of player signals, then sends what the session queued.
    pub(crate) fn act(&mut self, f: impl FnOnce(&mut PlaybackAdapter)) {
        f(&mut self.adapter);
        self.adapter.flush();
        self.drive();
    }

    pub(crate) fn handle(&mut self, msg: ReporterMsg) {
        let session = self.adapter.session_mut();
        match msg {
            ReporterMsg::Timer(id) => session.on_timer(id),
            ReporterMsg::Outcome(id, outcome) => session.handle_outcome(id, outcome),
            ReporterMsg::Unauthorized(_) => return,
        }
        self.drive();
    }

    fn drive(&mut self) {
        while let Some(tx) = self.adapter.session_mut().poll_transmit() {
            let client = self.client.clone();
            let reply = self.tx.clone();
            let task = tokio::spawn(async move {
                let mut rejected = false;
                let outcome = match client.post_playback_event(&tx.body, tx.keepalive).await {
                    Ok((status, json)) => {
                        rejected = status == 401;
                        TransportOutcome::response(status, json)
                    }
                    Err(e) => {
                        tracing::debug!("playback event failed: {e}");
                        TransportOutcome::NetworkError
                    }
                };
                if !tx.keepalive {
                    let _ = reply.send(ReporterMsg::Outcome(tx.id, outcome));
                    if rejected {
                        let token = client.api_token().to_owned();
                        let _ = reply.send(ReporterMsg::Unauthorized(token));
                    }
                }
            });
            if tx.keepalive {
                self.keepalive.push(task);
            }
        }
        while let Some(error) = self.adapter.session_mut().poll_protocol_error() {
            tracing::warn!(?error, "032 playback session stopped reporting");
        }
        self.keepalive.retain(|t| !t.is_finished());
    }

    /// App close: best-effort `client_closed`, waiting at most `timeout`.
    pub(crate) async fn close(&mut self, timeout: Duration) {
        self.adapter.on_close();
        self.drive();
        let pending = std::mem::take(&mut self.keepalive);
        let _ = tokio::time::timeout(timeout, async {
            for task in pending {
                let _ = task.await;
            }
        })
        .await;
    }
}
