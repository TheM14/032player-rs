//! Controlling a room's coordinator. Albums and playlists go into the
//! speaker's own queue (`x-rincon-queue`), so it advances by itself.

use std::time::Duration;

use crate::didl::{QueueEntry, parse_didl};
use crate::soap::{
    AV_TRANSPORT, CONTENT_DIRECTORY, GROUP_RENDERING, RENDERING_CONTROL, Soap, Values,
    format_duration, parse_duration,
};
use crate::{Error, Item, Room};

/// `AddMultipleURIsToQueue` takes at most this many entries.
const BATCH: usize = 16;
const BROWSE_PAGE: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportState {
    Playing,
    Paused,
    Stopped,
    /// Loading / buffering after `Play`, a track change or `Seek`.
    Transitioning,
}

/// What the speaker is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: TransportState,
    /// 0-based queue position while the queue is the source.
    pub index: Option<usize>,
    pub track_uri: String,
    pub position_ms: i64,
    pub duration_ms: i64,
    /// The speaker's queue is the transport's source.
    pub queue_active: bool,
    pub queue_length: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMode {
    Normal,
    RepeatAll,
    RepeatOne,
}

impl PlayMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::RepeatAll => "REPEAT_ALL",
            Self::RepeatOne => "REPEAT_ONE",
        }
    }
}

/// The coordinator of one room.
#[derive(Clone)]
pub struct Speaker {
    soap: Soap,
    base_url: String,
    uuid: String,
}

fn transport_state(raw: &str) -> TransportState {
    match raw {
        "PLAYING" => TransportState::Playing,
        "PAUSED_PLAYBACK" | "PAUSED_RECORDING" => TransportState::Paused,
        "TRANSITIONING" => TransportState::Transitioning,
        _ => TransportState::Stopped,
    }
}

fn get(values: &Values, key: &str) -> String {
    values.get(key).cloned().unwrap_or_default()
}

impl Speaker {
    pub fn new(http: reqwest::Client, room: &Room) -> Self {
        Self {
            soap: Soap::new(http),
            base_url: room.base_url.trim_end_matches('/').to_owned(),
            uuid: room.id.clone(),
        }
    }

    async fn av(&self, action: &str, args: &[(&str, String)]) -> Result<Values, Error> {
        let mut all = vec![("InstanceID", "0".to_owned())];
        all.extend(args.iter().cloned());
        self.soap
            .call(
                &format!("{}/MediaRenderer/AVTransport/Control", self.base_url),
                AV_TRANSPORT,
                action,
                &all,
            )
            .await
    }

    fn queue_uri(&self) -> String {
        format!("x-rincon-queue:{}#0", self.uuid)
    }

    /// Sonos answers 701 while still `TRANSITIONING` after `Play` / a new URI.
    async fn retry_transition<F, Fut>(&self, mut action: F) -> Result<(), Error>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<(), Error>>,
    {
        let mut last = Ok(());
        for _ in 0..6 {
            last = action().await;
            match &last {
                Err(e) if e.upnp_code() == Some(701) => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                _ => return last,
            }
        }
        last
    }

    /// Replaces the queue with `items` and selects `start`; `play` starts
    /// it (at `position_ms`).
    pub async fn replace(
        &self,
        items: &[Item],
        start: usize,
        position_ms: i64,
        play: bool,
    ) -> Result<(), Error> {
        self.av("RemoveAllTracksFromQueue", &[]).await?;
        self.append(items).await?;
        if items.is_empty() {
            return Ok(());
        }
        let start = if start < items.len() { start } else { 0 };
        self.use_queue().await?;
        self.retry_transition(|| self.select_track(start)).await?;
        if !play {
            return Ok(());
        }
        self.play().await?;
        if position_ms > 0 {
            self.retry_transition(|| self.seek(position_ms)).await?;
        }
        Ok(())
    }

    /// Appends in batches; firmware that rejects the batch call gets one
    /// `AddURIToQueue` per item for the rest.
    pub async fn append(&self, items: &[Item]) -> Result<(), Error> {
        for (n, chunk) in items.chunks(BATCH).enumerate() {
            let uris: Vec<&str> = chunk.iter().map(|i| i.uri.as_str()).collect();
            let metas: Vec<String> = chunk.iter().map(Item::didl).collect();
            let result = self
                .av(
                    "AddMultipleURIsToQueue",
                    &[
                        ("UpdateID", "0".into()),
                        ("NumberOfURIs", chunk.len().to_string()),
                        ("EnqueuedURIs", uris.join(" ")),
                        ("EnqueuedURIsMetaData", metas.join(" ")),
                        ("ContainerURI", String::new()),
                        ("ContainerMetaData", String::new()),
                        ("DesiredFirstTrackNumberEnqueued", "0".into()),
                        ("EnqueueAsNext", "0".into()),
                    ],
                )
                .await;
            match result {
                Ok(_) => {}
                Err(e) if e.upnp_code().is_some() => {
                    for item in &items[n * BATCH..] {
                        self.add_one(item, 0).await?;
                    }
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Enqueues at the 1-based `position` (0 = the end).
    async fn add_one(&self, item: &Item, position: usize) -> Result<(), Error> {
        self.av(
            "AddURIToQueue",
            &[
                ("EnqueuedURI", item.uri.clone()),
                ("EnqueuedURIMetaData", item.didl()),
                ("DesiredFirstTrackNumberEnqueued", position.to_string()),
                ("EnqueueAsNext", "0".into()),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Inserts before the 0-based `at`; `None` appends.
    pub async fn insert(&self, items: &[Item], at: Option<usize>) -> Result<(), Error> {
        match at {
            None => self.append(items).await,
            Some(at) => {
                for (k, item) in items.iter().enumerate() {
                    self.add_one(item, at + 1 + k).await?;
                }
                Ok(())
            }
        }
    }

    pub async fn remove(&self, index: usize) -> Result<(), Error> {
        self.av(
            "RemoveTrackFromQueue",
            &[
                ("ObjectID", format!("Q:0/{}", index + 1)),
                ("UpdateID", "0".into()),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Moves one entry from `from` to `to` (0-based, final position).
    pub async fn move_item(&self, from: usize, to: usize) -> Result<(), Error> {
        if from == to {
            return Ok(());
        }
        // InsertBefore counts positions before the move.
        let insert_before = if to > from { to + 2 } else { to + 1 };
        self.av(
            "ReorderTracksInQueue",
            &[
                ("StartingIndex", (from + 1).to_string()),
                ("NumberOfTracks", "1".into()),
                ("InsertBefore", insert_before.to_string()),
                ("UpdateID", "0".into()),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Swaps one entry (a live transcode re-opened at an offset).
    pub async fn replace_item(&self, index: usize, item: &Item) -> Result<(), Error> {
        self.remove(index).await?;
        self.add_one(item, index + 1).await
    }

    async fn use_queue(&self) -> Result<(), Error> {
        self.av(
            "SetAVTransportURI",
            &[
                ("CurrentURI", self.queue_uri()),
                ("CurrentURIMetaData", String::new()),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn select_track(&self, index: usize) -> Result<(), Error> {
        self.av(
            "Seek",
            &[
                ("Unit", "TRACK_NR".into()),
                ("Target", (index + 1).to_string()),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Plays queue entry `index` (re-selecting the queue as the source if
    /// the speaker was playing something else), from `position_ms`.
    pub async fn play_index(&self, index: usize, position_ms: i64) -> Result<(), Error> {
        let media = self.av("GetMediaInfo", &[]).await?;
        if !get(&media, "CurrentURI").starts_with("x-rincon-queue:") {
            self.use_queue().await?;
        }
        self.retry_transition(|| self.select_track(index)).await?;
        self.play().await?;
        if position_ms > 0 {
            self.retry_transition(|| self.seek(position_ms)).await?;
        }
        Ok(())
    }

    pub async fn play(&self) -> Result<(), Error> {
        self.av("Play", &[("Speed", "1".into())]).await.map(|_| ())
    }

    pub async fn pause(&self) -> Result<(), Error> {
        self.av("Pause", &[]).await.map(|_| ())
    }

    pub async fn stop(&self) -> Result<(), Error> {
        self.av("Stop", &[]).await.map(|_| ())
    }

    pub async fn seek(&self, position_ms: i64) -> Result<(), Error> {
        self.av(
            "Seek",
            &[
                ("Unit", "REL_TIME".into()),
                ("Target", format_duration(position_ms)),
            ],
        )
        .await
        .map(|_| ())
    }

    pub async fn set_play_mode(&self, mode: PlayMode) -> Result<(), Error> {
        self.av("SetPlayMode", &[("NewPlayMode", mode.as_str().into())])
            .await
            .map(|_| ())
    }

    /// `PlayMode` as Sonos reports it (`NORMAL`, `SHUFFLE`, ...).
    pub async fn play_mode(&self) -> Result<String, Error> {
        Ok(get(
            &self.av("GetTransportSettings", &[]).await?,
            "PlayMode",
        ))
    }

    pub async fn status(&self) -> Result<Status, Error> {
        let transport = self.av("GetTransportInfo", &[]).await?;
        let position = self.av("GetPositionInfo", &[]).await?;
        let media = self.av("GetMediaInfo", &[]).await?;
        let queue_active = get(&media, "CurrentURI").starts_with("x-rincon-queue:");
        let index = get(&position, "Track")
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| queue_active && *n > 0)
            .map(|n| n - 1);
        Ok(Status {
            state: transport_state(get(&transport, "CurrentTransportState").trim()),
            index,
            track_uri: get(&position, "TrackURI"),
            position_ms: parse_duration(&get(&position, "RelTime")),
            duration_ms: parse_duration(&get(&position, "TrackDuration")),
            queue_active,
            queue_length: if queue_active {
                get(&media, "NrTracks").trim().parse().unwrap_or(0)
            } else {
                0
            },
        })
    }

    /// The queue's entries (at most `limit`).
    pub async fn queue(&self, limit: usize) -> Result<Vec<QueueEntry>, Error> {
        let mut out = Vec::new();
        while out.len() < limit {
            let res = self
                .soap
                .call(
                    &format!("{}/MediaServer/ContentDirectory/Control", self.base_url),
                    CONTENT_DIRECTORY,
                    "Browse",
                    &[
                        ("ObjectID", "Q:0".into()),
                        ("BrowseFlag", "BrowseDirectChildren".into()),
                        ("Filter", "dc:title,res".into()),
                        ("StartingIndex", out.len().to_string()),
                        ("RequestedCount", BROWSE_PAGE.to_string()),
                        ("SortCriteria", String::new()),
                    ],
                )
                .await?;
            let page = parse_didl(&get(&res, "Result"));
            let total: usize = get(&res, "TotalMatches").trim().parse().unwrap_or(0);
            let empty = page.is_empty();
            out.extend(page);
            if empty || out.len() >= total {
                break;
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    /// Group volume (0-100), or the coordinator's own.
    pub async fn volume(&self) -> Result<i32, Error> {
        let group = self
            .soap
            .call(
                &format!(
                    "{}/MediaRenderer/GroupRenderingControl/Control",
                    self.base_url
                ),
                GROUP_RENDERING,
                "GetGroupVolume",
                &[("InstanceID", "0".into())],
            )
            .await;
        let values = match group {
            Ok(v) => v,
            Err(_) => {
                self.soap
                    .call(
                        &format!("{}/MediaRenderer/RenderingControl/Control", self.base_url),
                        RENDERING_CONTROL,
                        "GetVolume",
                        &[("InstanceID", "0".into()), ("Channel", "Master".into())],
                    )
                    .await?
            }
        };
        Ok(get(&values, "CurrentVolume").trim().parse().unwrap_or(0))
    }

    pub async fn set_volume(&self, volume: i32) -> Result<(), Error> {
        let v = volume.clamp(0, 100).to_string();
        let group = self
            .soap
            .call(
                &format!(
                    "{}/MediaRenderer/GroupRenderingControl/Control",
                    self.base_url
                ),
                GROUP_RENDERING,
                "SetGroupVolume",
                &[("InstanceID", "0".into()), ("DesiredVolume", v.clone())],
            )
            .await;
        if group.is_ok() {
            return Ok(());
        }
        self.soap
            .call(
                &format!("{}/MediaRenderer/RenderingControl/Control", self.base_url),
                RENDERING_CONTROL,
                "SetVolume",
                &[
                    ("InstanceID", "0".into()),
                    ("Channel", "Master".into()),
                    ("DesiredVolume", v),
                ],
            )
            .await
            .map(|_| ())
    }
}
