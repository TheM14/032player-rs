//! A simulated Sonos coordinator on wiremock, for tests (feature `fake`).
//! It keeps a queue, the selected track, the transport state and a log of
//! the actions it received.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::Room;
use crate::didl::parse_didl;
use crate::soap::{escape, format_duration};

pub const UUID: &str = "RINCON_FAKE01400";

#[derive(Clone, Debug, Default)]
pub struct State {
    /// `(uri, title)` per queue entry.
    pub queue: Vec<(String, String)>,
    /// The queue is the transport's source.
    pub queue_source: bool,
    /// 1-based selected queue entry (0: none).
    pub track: usize,
    /// `PLAYING`, `PAUSED_PLAYBACK`, `STOPPED`, `TRANSITIONING`.
    pub transport: String,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub volume: i32,
    pub mode: String,
    /// Received actions with their main argument (`Seek TRACK_NR 2`).
    pub log: Vec<String>,
}

pub struct FakeSpeaker {
    pub server: MockServer,
    state: Arc<Mutex<State>>,
}

struct Soap(Arc<Mutex<State>>, String);

fn args(body: &str) -> (String, HashMap<String, String>) {
    let Ok(doc) = roxmltree::Document::parse(body) else {
        return (String::new(), HashMap::new());
    };
    let Some(body) = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Body")
    else {
        return (String::new(), HashMap::new());
    };
    let Some(action) = body.children().find(roxmltree::Node::is_element) else {
        return (String::new(), HashMap::new());
    };
    let values = action
        .children()
        .filter(roxmltree::Node::is_element)
        .map(|c| {
            (
                c.tag_name().name().to_owned(),
                c.text().unwrap_or("").to_owned(),
            )
        })
        .collect();
    (action.tag_name().name().to_owned(), values)
}

fn reply(action: &str, values: &[(&str, String)]) -> ResponseTemplate {
    let mut inner = String::new();
    for (k, v) in values {
        inner.push_str(&format!("<{k}>{}</{k}>", escape(v)));
    }
    ResponseTemplate::new(200).set_body_string(format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:{action}Response xmlns:u="urn:x">{inner}</u:{action}Response></s:Body></s:Envelope>"#
    ))
}

fn fault(code: u32) -> ResponseTemplate {
    ResponseTemplate::new(500).set_body_string(format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>{code}</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#
    ))
}

impl Respond for Soap {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&request.body).to_string();
        let (action, a) = args(&body);
        let arg = |k: &str| a.get(k).cloned().unwrap_or_default();
        let mut s = self.0.lock().unwrap();
        match action.as_str() {
            "RemoveAllTracksFromQueue" => {
                s.log.push(action.clone());
                s.queue.clear();
                s.track = 0;
                reply(&action, &[])
            }
            "AddMultipleURIsToQueue" => {
                let uris: Vec<String> = arg("EnqueuedURIs")
                    .split(' ')
                    .filter(|u| !u.is_empty())
                    .map(str::to_owned)
                    .collect();
                s.log.push(format!("{action} {}", uris.len()));
                let metas = arg("EnqueuedURIsMetaData");
                let titles: Vec<String> = parse_didl(&format!("<x>{metas}</x>"))
                    .into_iter()
                    .map(|e| e.title)
                    .collect();
                for (i, uri) in uris.into_iter().enumerate() {
                    let title = titles.get(i).cloned().unwrap_or_default();
                    s.queue.push((uri, title));
                }
                reply(&action, &[])
            }
            "AddURIToQueue" => {
                let at: usize = arg("DesiredFirstTrackNumberEnqueued").parse().unwrap_or(0);
                s.log.push(format!("{action} {at}"));
                let title = parse_didl(&arg("EnqueuedURIMetaData"))
                    .first()
                    .map(|e| e.title.clone())
                    .unwrap_or_default();
                let entry = (arg("EnqueuedURI"), title);
                if at == 0 || at > s.queue.len() {
                    s.queue.push(entry);
                } else {
                    s.queue.insert(at - 1, entry);
                    if s.track >= at {
                        s.track += 1;
                    }
                }
                reply(&action, &[])
            }
            "RemoveTrackFromQueue" => {
                let n: usize = arg("ObjectID")
                    .trim_start_matches("Q:0/")
                    .parse()
                    .unwrap_or(0);
                s.log.push(format!("{action} {n}"));
                if n >= 1 && n <= s.queue.len() {
                    s.queue.remove(n - 1);
                    if s.track > n {
                        s.track -= 1;
                    }
                }
                reply(&action, &[])
            }
            "ReorderTracksInQueue" => {
                let from: usize = arg("StartingIndex").parse().unwrap_or(1);
                let before: usize = arg("InsertBefore").parse().unwrap_or(1);
                s.log.push(format!("{action} {from} {before}"));
                if from >= 1 && from <= s.queue.len() {
                    let entry = s.queue.remove(from - 1);
                    let to = if before > from {
                        before - 2
                    } else {
                        before - 1
                    };
                    let to = to.min(s.queue.len());
                    s.queue.insert(to, entry);
                }
                reply(&action, &[])
            }
            "SetAVTransportURI" => {
                let uri = arg("CurrentURI");
                s.log.push(format!("{action} {uri}"));
                s.queue_source = uri.starts_with("x-rincon-queue:");
                s.transport = "STOPPED".into();
                reply(&action, &[])
            }
            "Seek" => {
                let unit = arg("Unit");
                let target = arg("Target");
                s.log.push(format!("{action} {unit} {target}"));
                if unit == "TRACK_NR" {
                    s.track = target.parse().unwrap_or(1);
                    s.position_ms = 0;
                } else {
                    s.position_ms = crate::soap::parse_duration(&target);
                }
                reply(&action, &[])
            }
            "Play" => {
                s.log.push(action.clone());
                if s.track == 0 && !s.queue.is_empty() {
                    s.track = 1;
                }
                s.transport = "PLAYING".into();
                reply(&action, &[])
            }
            "Pause" => {
                s.log.push(action.clone());
                if s.transport != "PLAYING" {
                    return fault(701);
                }
                s.transport = "PAUSED_PLAYBACK".into();
                reply(&action, &[])
            }
            "Stop" => {
                s.log.push(action.clone());
                s.transport = "STOPPED".into();
                reply(&action, &[])
            }
            "SetPlayMode" => {
                let mode = arg("NewPlayMode");
                s.log.push(format!("{action} {mode}"));
                s.mode = mode;
                reply(&action, &[])
            }
            "GetTransportSettings" => reply(&action, &[("PlayMode", s.mode.clone())]),
            "GetTransportInfo" => reply(
                &action,
                &[
                    ("CurrentTransportState", s.transport.clone()),
                    ("CurrentTransportStatus", "OK".into()),
                ],
            ),
            "GetPositionInfo" => {
                let uri = s
                    .track
                    .checked_sub(1)
                    .and_then(|i| s.queue.get(i))
                    .map(|e| e.0.clone())
                    .unwrap_or_default();
                reply(
                    &action,
                    &[
                        ("Track", s.track.to_string()),
                        ("TrackDuration", format_duration(s.duration_ms)),
                        ("TrackMetaData", String::new()),
                        ("TrackURI", uri),
                        ("RelTime", format_duration(s.position_ms)),
                    ],
                )
            }
            "GetMediaInfo" => reply(
                &action,
                &[
                    ("NrTracks", s.queue.len().to_string()),
                    (
                        "CurrentURI",
                        if s.queue_source {
                            format!("x-rincon-queue:{UUID}#0")
                        } else {
                            String::new()
                        },
                    ),
                ],
            ),
            "GetGroupVolume" => reply(&action, &[("CurrentVolume", s.volume.to_string())]),
            "SetGroupVolume" => {
                let v = arg("DesiredVolume");
                s.log.push(format!("{action} {v}"));
                s.volume = v.parse().unwrap_or(0);
                reply(&action, &[])
            }
            "Browse" => {
                let start: usize = arg("StartingIndex").parse().unwrap_or(0);
                let count: usize = arg("RequestedCount").parse().unwrap_or(100);
                let page: Vec<&(String, String)> = s.queue.iter().skip(start).take(count).collect();
                let mut didl = String::from(
                    r#"<DIDL-Lite xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/">"#,
                );
                for (i, (uri, title)) in page.iter().enumerate() {
                    didl.push_str(&format!(
                        r#"<item id="Q:0/{}"><dc:title>{}</dc:title><res protocolInfo="http-get:*:audio/flac:*">{}</res></item>"#,
                        start + i + 1,
                        escape(title),
                        escape(uri)
                    ));
                }
                didl.push_str("</DIDL-Lite>");
                reply(
                    &action,
                    &[
                        ("Result", didl),
                        ("NumberReturned", page.len().to_string()),
                        ("TotalMatches", s.queue.len().to_string()),
                    ],
                )
            }
            "GetZoneGroupState" => {
                let location = format!("{}/xml/device_description.xml", self.1);
                reply(
                    &action,
                    &[(
                        "ZoneGroupState",
                        format!(
                            r#"<ZoneGroupState><ZoneGroups><ZoneGroup Coordinator="{UUID}" ID="g"><ZoneGroupMember UUID="{UUID}" Location="{location}" ZoneName="测试房间"/></ZoneGroup></ZoneGroups></ZoneGroupState>"#
                        ),
                    )],
                )
            }
            _ => fault(401),
        }
    }
}

impl FakeSpeaker {
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let state = Arc::new(Mutex::new(State {
            transport: "STOPPED".into(),
            mode: "NORMAL".into(),
            volume: 30,
            duration_ms: 180_000,
            ..State::default()
        }));
        Mock::given(method("GET"))
            .and(path("/xml/device_description.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType><friendlyName>Fake</friendlyName><manufacturer>Sonos, Inc.</manufacturer><modelName>Sonos One</modelName><UDN>uuid:{UUID}</UDN><roomName>测试房间</roomName></device></root>"#
            )))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(Soap(state.clone(), server.uri()))
            .mount(&server)
            .await;
        Self { server, state }
    }

    /// `host:port` to configure as a speaker address.
    pub fn host(&self) -> String {
        self.server.address().to_string()
    }

    pub fn room(&self) -> Room {
        Room {
            id: UUID.into(),
            name: "测试房间".into(),
            base_url: self.server.uri(),
            host: self.server.address().ip().to_string(),
            member_count: 1,
            model: "Sonos One".into(),
        }
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
    }

    /// The current track ended: on to the next (or stopped at the end).
    pub fn finish_track(&self) {
        let mut s = self.state.lock().unwrap();
        s.position_ms = 0;
        if s.track < s.queue.len() {
            s.track += 1;
        } else {
            s.track = 1;
            s.transport = "STOPPED".into();
        }
    }

    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn clear_log(&self) {
        self.state.lock().unwrap().log.clear();
    }
}
