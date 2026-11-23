//! Finding Sonos rooms: SSDP M-SEARCH from every IPv4 interface plus any
//! configured hosts, device descriptions, then the zone group topology so
//! a room is always addressed through its group coordinator.

use std::collections::{BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::task::JoinSet;

use crate::soap::{Soap, ZONE_GROUP_TOPOLOGY};
use crate::{Error, Room};

const SSDP_ADDR: (Ipv4Addr, u16) = (Ipv4Addr::new(239, 255, 255, 250), 1900);
const ST_ZONE_PLAYER: &str = "urn:schemas-upnp-org:device:ZonePlayer:1";
const DESCRIPTION_TIMEOUT: Duration = Duration::from_secs(4);

/// A player from its device description.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Player {
    pub uuid: String,
    pub name: String,
    pub model: String,
    pub base_url: String,
}

/// Rooms on the network, sorted by name. `hosts` are extra speaker
/// addresses (`192.168.1.30`, `host:port` or a description URL) probed
/// besides SSDP, for networks where multicast does not get through.
pub async fn discover(
    http: &reqwest::Client,
    timeout: Duration,
    hosts: &[String],
) -> Result<Vec<Room>, Error> {
    let mut locations: BTreeSet<String> = ssdp_search(timeout).await;
    locations.extend(hosts.iter().filter_map(|h| static_location(h)));
    tracing::debug!(count = locations.len(), "sonos locations");

    let mut tasks = JoinSet::new();
    for location in locations {
        let http = http.clone();
        tasks.spawn(async move {
            let text = tokio::time::timeout(DESCRIPTION_TIMEOUT, async {
                let res = http.get(&location).send().await.ok()?;
                read_text_limited(res, MAX_DESCRIPTION_BYTES).await
            })
            .await
            .ok()??;
            parse_description(&location, &text)
        });
    }
    let mut players: HashMap<String, Player> = HashMap::new();
    while let Some(result) = tasks.join_next().await {
        if let Ok(Some(player)) = result {
            players.insert(player.uuid.clone(), player);
        }
    }
    if players.is_empty() {
        return Ok(Vec::new());
    }

    let soap = Soap::new(http.clone());
    let mut sorted_players: Vec<&Player> = players.values().collect();
    sorted_players.sort_by(|a, b| a.uuid.cmp(&b.uuid));

    let mut covered = BTreeSet::new();
    let mut rooms_by_id: HashMap<String, Room> = HashMap::new();
    for player in sorted_players {
        if covered.contains(&player.uuid) {
            continue;
        }
        let url = format!("{}/ZoneGroupTopology/Control", player.base_url);
        let call = soap.call(&url, ZONE_GROUP_TOPOLOGY, "GetZoneGroupState", &[]);
        if let Ok(Ok(values)) = tokio::time::timeout(DESCRIPTION_TIMEOUT, call).await
            && let Some(state) = values.get("ZoneGroupState")
            && let Some((parsed_rooms, members)) = parse_zone_groups_with_members(state, &players)
            && !members.is_empty()
        {
            covered.extend(members);
            covered.insert(player.uuid.clone());
            for room in parsed_rooms {
                rooms_by_id.entry(room.id.clone()).or_insert(room);
            }
        }
    }
    for player in players.into_values() {
        if !covered.contains(&player.uuid) {
            rooms_by_id.entry(player.uuid.clone()).or_insert(Room {
                host: host_of(&player.base_url),
                id: player.uuid,
                name: player.name,
                base_url: player.base_url,
                member_count: 1,
                model: player.model,
            });
        }
    }
    let mut rooms: Vec<Room> = rooms_by_id.into_values().collect();
    rooms.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rooms)
}

/// Description URL of a configured speaker address.
fn static_location(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let has_scheme = host.contains("://");
    let raw = if has_scheme {
        host.to_owned()
    } else {
        format!("http://{host}")
    };
    let mut url = url::Url::parse(&raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    if !has_scheme && url.port().is_none() {
        let _ = url.set_port(Some(1400));
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/xml/device_description.xml");
    }
    Some(url.into())
}

/// IPv4 addresses to search from: every non-loopback, non-link-local
/// interface (a multi-homed PC would otherwise only search the default
/// route); the unspecified address when there are none.
fn search_addresses() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(ip) if !ip.is_link_local() && !ip.is_loopback() => Some(ip),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    if out.is_empty() {
        out.push(Ipv4Addr::UNSPECIFIED);
    }
    out
}

const MAX_DESCRIPTION_BYTES: usize = 512 * 1024;
const MAX_SSDP_LOCATIONS: usize = 128;

async fn read_text_limited(mut res: reqwest::Response, max_bytes: usize) -> Option<String> {
    if res
        .content_length()
        .is_some_and(|len| len > max_bytes as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.ok()? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).ok()
}

fn is_valid_ssdp_location(raw: &str) -> bool {
    url::Url::parse(raw)
        .ok()
        .is_some_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

async fn ssdp_search(timeout: Duration) -> BTreeSet<String> {
    let request = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {ST_ZONE_PLAYER}\r\n\r\n"
    );
    let mut tasks = JoinSet::new();
    for local in search_addresses() {
        let request = request.clone();
        tasks.spawn(async move {
            let mut found = BTreeSet::new();
            let socket = match UdpSocket::bind(SocketAddr::from((local, 0))).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(%local, "ssdp bind: {e}");
                    return found;
                }
            };
            if !local.is_unspecified()
                && let Err(e) = socket2::SockRef::from(&socket).set_multicast_if_v4(&local)
            {
                tracing::debug!(%local, "ssdp multicast_if: {e}");
            }
            let target = SocketAddr::from(SSDP_ADDR);
            for _ in 0..3 {
                if let Err(e) = socket.send_to(request.as_bytes(), target).await {
                    tracing::debug!(%local, "ssdp send: {e}");
                    return found;
                }
            }
            let mut buf = vec![0u8; 8192];
            let _ = tokio::time::timeout(timeout, async {
                loop {
                    let Ok((n, _)) = socket.recv_from(&mut buf).await else {
                        break;
                    };
                    if found.len() >= MAX_SSDP_LOCATIONS {
                        break;
                    }
                    let payload = String::from_utf8_lossy(&buf[..n]);
                    if let Some(location) = header_value(&payload, "LOCATION")
                        && is_valid_ssdp_location(&location)
                    {
                        found.insert(location);
                    }
                }
            })
            .await;
            found
        });
    }
    let mut locations = BTreeSet::new();
    while let Some(result) = tasks.join_next().await {
        if let Ok(found) = result {
            for loc in found {
                if locations.len() >= MAX_SSDP_LOCATIONS {
                    break;
                }
                locations.insert(loc);
            }
        }
    }
    locations
}

fn header_value(payload: &str, name: &str) -> Option<String> {
    payload.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim().eq_ignore_ascii_case(name) && !value.trim().is_empty())
            .then(|| value.trim().to_owned())
    })
}

/// `uuid:RINCON_xxx::urn:...` -> `RINCON_xxx`.
pub(crate) fn normalize_uuid(udn: &str) -> String {
    let value = udn.trim();
    let value = value.strip_prefix("uuid:").unwrap_or(value);
    value.split("::").next().unwrap_or("").trim().to_owned()
}

/// `scheme://host:port` of a URL; empty when it has none.
pub(crate) fn origin(raw: &str) -> String {
    url::Url::parse(raw)
        .ok()
        .filter(|u| u.host_str().is_some())
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default()
}

pub(crate) fn host_of(raw: &str) -> String {
    url::Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A Sonos player from its device description; `None` for other devices.
pub(crate) fn parse_description(location: &str, xml: &str) -> Option<Player> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let device = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "device")?;
    let field = |name: &str| {
        device
            .children()
            .find(|c| c.is_element() && c.tag_name().name() == name)
            .and_then(|c| c.text())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let sonos = field("manufacturer").to_lowercase().contains("sonos")
        || field("deviceType").starts_with("urn:schemas-upnp-org:device:ZonePlayer");
    let uuid = normalize_uuid(&field("UDN"));
    let base_url = origin(location);
    if !sonos || uuid.is_empty() || base_url.is_empty() {
        return None;
    }
    let name = [field("roomName"), field("friendlyName")]
        .into_iter()
        .find(|n| !n.is_empty())
        .unwrap_or_default();
    Some(Player {
        uuid,
        name,
        model: field("modelName"),
        base_url,
    })
}

/// Rooms of a `GetZoneGroupState` document (newer firmware wraps the
/// groups in `<ZoneGroupState>`, older sends `<ZoneGroups>`).
#[cfg(test)]
pub(crate) fn parse_zone_groups(state: &str, known: &HashMap<String, Player>) -> Vec<Room> {
    parse_zone_groups_with_members(state, known)
        .map(|(rooms, _)| rooms)
        .unwrap_or_default()
}

/// Parses visible rooms and every member/satellite UUID referenced by a
/// `GetZoneGroupState` document. Purely invisible groups (e.g. Sonos Bridge,
/// Boost, or bonded satellites) are recorded as covered members but omitted
/// from the returned room list.
fn parse_zone_groups_with_members(
    state: &str,
    known: &HashMap<String, Player>,
) -> Option<(Vec<Room>, BTreeSet<String>)> {
    let doc = roxmltree::Document::parse(state).ok()?;
    let mut rooms = Vec::new();
    let mut members = BTreeSet::new();
    for group in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "ZoneGroup")
    {
        let coordinator = group.attribute("Coordinator").unwrap_or("").to_owned();
        let mut names: Vec<String> = Vec::new();
        let mut base_url = String::new();
        let mut visible = 0;
        for member in group
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "ZoneGroupMember")
        {
            if let Some(uuid) = member.attribute("UUID").filter(|u| !u.is_empty()) {
                members.insert(uuid.to_owned());
            }
            for sat in member
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "Satellite")
            {
                if let Some(uuid) = sat.attribute("UUID").filter(|u| !u.is_empty()) {
                    members.insert(uuid.to_owned());
                }
            }
            let invisible = member.attribute("Invisible") == Some("1");
            let name = member.attribute("ZoneName").unwrap_or("").trim().to_owned();
            if member.attribute("UUID") == Some(coordinator.as_str()) {
                base_url = origin(member.attribute("Location").unwrap_or(""));
                if !invisible {
                    names.insert(0, name);
                }
            } else if !invisible {
                names.push(name);
            }
            if !invisible {
                visible += 1;
            }
        }
        if coordinator.is_empty() || visible == 0 {
            continue;
        }
        if base_url.is_empty() {
            if let Some(p) = known.get(&coordinator) {
                base_url.clone_from(&p.base_url);
            } else {
                continue;
            }
        }
        let mut seen = BTreeSet::new();
        let name = names
            .into_iter()
            .filter(|n| !n.is_empty() && seen.insert(n.clone()))
            .collect::<Vec<_>>()
            .join(" + ");
        if name.is_empty() {
            continue;
        }
        rooms.push(Room {
            host: host_of(&base_url),
            model: known
                .get(&coordinator)
                .map(|p| p.model.clone())
                .unwrap_or_default(),
            id: coordinator,
            name,
            base_url,
            member_count: visible,
        });
    }
    Some((rooms, members))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_device_descriptions() {
        let xml = r#"<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType><friendlyName>192.168.1.30 - Sonos One</friendlyName><manufacturer>Sonos, Inc.</manufacturer><modelName>Sonos One</modelName><UDN>uuid:RINCON_000E58A0B1C201400</UDN><roomName>客厅</roomName><deviceList><device><friendlyName>Media Renderer</friendlyName></device></deviceList></device></root>"#;
        let p =
            parse_description("http://192.168.1.30:1400/xml/device_description.xml", xml).unwrap();
        assert_eq!(p.uuid, "RINCON_000E58A0B1C201400");
        assert_eq!(p.name, "客厅");
        assert_eq!(p.base_url, "http://192.168.1.30:1400");
        let other = xml
            .replace("Sonos, Inc.", "Acme")
            .replace("ZonePlayer", "MediaRenderer");
        assert!(parse_description("http://x:1/d.xml", &other).is_none());
    }

    #[test]
    fn groups_rooms_by_coordinator() {
        let state = r#"<ZoneGroupState><ZoneGroups>
            <ZoneGroup Coordinator="RINCON_A" ID="g1">
              <ZoneGroupMember UUID="RINCON_B" Location="http://10.0.0.3:1400/xml/device_description.xml" ZoneName="卧室"/>
              <ZoneGroupMember UUID="RINCON_A" Location="http://10.0.0.2:1400/xml/device_description.xml" ZoneName="客厅"><Satellite UUID="RINCON_S" ZoneName="客厅" Invisible="1"/></ZoneGroupMember>
              <ZoneGroupMember UUID="RINCON_SUB" Location="http://10.0.0.4:1400/x.xml" ZoneName="客厅" Invisible="1"/>
            </ZoneGroup>
            <ZoneGroup Coordinator="RINCON_C" ID="g2">
              <ZoneGroupMember UUID="RINCON_C" Location="http://10.0.0.5:1400/xml/device_description.xml" ZoneName="书房"/>
            </ZoneGroup>
        </ZoneGroups></ZoneGroupState>"#;
        let rooms = parse_zone_groups(state, &HashMap::new());
        assert_eq!(rooms.len(), 2);
        assert_eq!(rooms[0].id, "RINCON_A");
        assert_eq!(rooms[0].name, "客厅 + 卧室");
        assert_eq!(rooms[0].base_url, "http://10.0.0.2:1400");
        assert_eq!(rooms[0].member_count, 2);
        assert_eq!(rooms[1].name, "书房");
        // Older firmware: <ZoneGroups> is the root.
        let old = state
            .replace("<ZoneGroupState>", "")
            .replace("</ZoneGroupState>", "");
        assert_eq!(parse_zone_groups(&old, &HashMap::new()).len(), 2);
    }

    #[test]
    fn helpers() {
        assert_eq!(normalize_uuid("uuid:RINCON_X::urn:schemas"), "RINCON_X");
        assert_eq!(
            static_location("192.168.1.30").unwrap(),
            "http://192.168.1.30:1400/xml/device_description.xml"
        );
        assert_eq!(
            static_location(" 10.0.0.2:1401 ").unwrap(),
            "http://10.0.0.2:1401/xml/device_description.xml"
        );
        assert_eq!(
            static_location("192.168.1.30/").unwrap(),
            "http://192.168.1.30:1400/xml/device_description.xml"
        );
        assert_eq!(
            static_location("http://192.168.1.30:1400").unwrap(),
            "http://192.168.1.30:1400/xml/device_description.xml"
        );
        assert_eq!(
            static_location("http://192.168.1.30:1400/").unwrap(),
            "http://192.168.1.30:1400/xml/device_description.xml"
        );
        assert_eq!(
            static_location("http://192.168.1.30:1400/custom.xml").unwrap(),
            "http://192.168.1.30:1400/custom.xml"
        );
        assert_eq!(static_location("ftp://192.168.1.30/x"), None);
        assert_eq!(static_location("  "), None);
        assert_eq!(
            header_value(
                "HTTP/1.1 200 OK\r\nlocation: http://a:1400/x.xml\r\n",
                "LOCATION"
            )
            .unwrap(),
            "http://a:1400/x.xml"
        );
    }

    #[test]
    fn skips_invisible_only_zone_groups() {
        let state = r#"<ZoneGroupState><ZoneGroups>
            <ZoneGroup Coordinator="RINCON_BRIDGE" ID="g0">
              <ZoneGroupMember UUID="RINCON_BRIDGE" Location="http://10.0.0.10:1400/xml/device_description.xml" ZoneName="BRIDGE" Invisible="1"/>
            </ZoneGroup>
            <ZoneGroup Coordinator="RINCON_LIVE" ID="g1">
              <ZoneGroupMember UUID="RINCON_LIVE" Location="http://10.0.0.11:1400/xml/device_description.xml" ZoneName="客厅"/>
            </ZoneGroup>
        </ZoneGroups></ZoneGroupState>"#;
        let (rooms, members) = parse_zone_groups_with_members(state, &HashMap::new()).unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].id, "RINCON_LIVE");
        assert!(members.contains("RINCON_BRIDGE"));
        assert!(members.contains("RINCON_LIVE"));
    }

    #[tokio::test]
    async fn discover_merges_multiple_households_and_excludes_invisible_bridge() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        async fn spawn_sonos_node(
            uuid: &'static str,
            room: &'static str,
            state_xml: String,
        ) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let base = format!("http://{addr}");
            let desc = format!(
                r#"<?xml version="1.0"?><root><device><deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType><manufacturer>Sonos, Inc.</manufacturer><modelName>Sonos One</modelName><UDN>uuid:{uuid}</UDN><roomName>{room}</roomName></device></root>"#
            );
            let soap = format!(
                r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetZoneGroupStateResponse xmlns:u="{ZONE_GROUP_TOPOLOGY}"><ZoneGroupState>{}</ZoneGroupState></u:GetZoneGroupStateResponse></s:Body></s:Envelope>"#,
                crate::soap::escape(&state_xml.replace("{BASE}", &base))
            );
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else {
                        break;
                    };
                    let desc = desc.clone();
                    let soap = soap.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let body = if req.starts_with("GET /xml/device_description.xml") {
                            desc
                        } else {
                            soap
                        };
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                    });
                }
            });
            base
        }

        let s1_state = r#"<ZoneGroupState><ZoneGroups>
            <ZoneGroup Coordinator="RINCON_BRIDGE" ID="b0">
              <ZoneGroupMember UUID="RINCON_BRIDGE" Location="{BASE}/xml/device_description.xml" ZoneName="BRIDGE" Invisible="1"/>
            </ZoneGroup>
            <ZoneGroup Coordinator="RINCON_S1" ID="g1">
              <ZoneGroupMember UUID="RINCON_S1" Location="{BASE}/xml/device_description.xml" ZoneName="S1 客厅"/>
            </ZoneGroup>
        </ZoneGroups></ZoneGroupState>"#
            .to_owned();
        let s2_state = r#"<ZoneGroupState><ZoneGroups>
            <ZoneGroup Coordinator="RINCON_S2" ID="g2">
              <ZoneGroupMember UUID="RINCON_S2" Location="{BASE}/xml/device_description.xml" ZoneName="S2 书房"/>
            </ZoneGroup>
        </ZoneGroups></ZoneGroupState>"#
            .to_owned();

        let h1 = spawn_sonos_node("RINCON_S1", "S1 客厅", s1_state).await;
        let h2 = spawn_sonos_node("RINCON_S2", "S2 书房", s2_state).await;

        let http = crate::http_client();
        let rooms = discover(&http, Duration::from_millis(50), &[h1, h2])
            .await
            .unwrap();
        let ids: Vec<&str> = rooms.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["RINCON_S1", "RINCON_S2"]);
    }

    #[tokio::test]
    async fn ssdp_location_validation_and_description_size_limit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        assert!(is_valid_ssdp_location(
            "http://192.168.1.10:1400/xml/device_description.xml"
        ));
        assert!(is_valid_ssdp_location("https://sonos.local/desc.xml"));
        assert!(!is_valid_ssdp_location("file:///etc/passwd"));
        assert!(!is_valid_ssdp_location("ftp://192.168.1.10/desc.xml"));
        assert!(!is_valid_ssdp_location("http://"));
        assert!(!is_valid_ssdp_location("/relative/desc.xml"));
        assert!(!is_valid_ssdp_location(""));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in [vec![b'a'; 128], vec![b'b'; 16]] {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut req = [0u8; 1024];
                let _ = sock.read(&mut req).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
            }
        });

        let http = crate::http_client();
        let url = format!("http://{addr}/desc.xml");
        let over = http.get(&url).send().await.unwrap();
        assert_eq!(read_text_limited(over, 64).await, None);
        let ok = http.get(&url).send().await.unwrap();
        assert_eq!(read_text_limited(ok, 64).await, Some("b".repeat(16)));
    }

    #[test]
    fn topology_handles_legacy_wrapper_satellites_and_missing_known_player() {
        // Legacy firmware sends `<ZoneGroups>` without `<ZoneGroupState>`.
        // Bonded home-theater `<Satellite>` members must be marked covered and
        // never create standalone rooms; coordinator absent from `known` still
        // resolves from its `<ZoneGroupMember Location=...>` attribute.
        let legacy_xml = r#"<ZoneGroups>
            <ZoneGroup Coordinator="RINCON_ARC" ID="g1">
                <ZoneGroupMember UUID="RINCON_ARC" Location="http://192.168.1.50:1400/xml/device_description.xml" ZoneName="Living Room">
                    <Satellite UUID="RINCON_SUB" Location="http://192.168.1.51:1400/xml/device_description.xml" ZoneName="Living Room"/>
                    <Satellite UUID="RINCON_SURR_L" Location="http://192.168.1.52:1400/xml/device_description.xml" ZoneName="Living Room"/>
                </ZoneGroupMember>
                <ZoneGroupMember UUID="RINCON_ONE" Location="http://192.168.1.53:1400/xml/device_description.xml" ZoneName="Kitchen"/>
                <ZoneGroupMember UUID="RINCON_HIDDEN" Location="http://192.168.1.54:1400/xml/device_description.xml" ZoneName="Hidden" Invisible="1"/>
            </ZoneGroup>
            <ZoneGroup Coordinator="RINCON_MISSING_LOC" ID="g2">
                <ZoneGroupMember UUID="RINCON_MISSING_LOC" Location="" ZoneName="Fallback Room"/>
            </ZoneGroup>
            <ZoneGroup Coordinator="" ID="g3">
                <ZoneGroupMember UUID="RINCON_NO_COORD" Location="http://192.168.1.55:1400/xml/device_description.xml" ZoneName="Orphan"/>
            </ZoneGroup>
        </ZoneGroups>"#;

        let mut known = HashMap::new();
        known.insert(
            "RINCON_MISSING_LOC".to_owned(),
            Player {
                uuid: "RINCON_MISSING_LOC".into(),
                name: "Fallback Room".into(),
                model: "Sonos Era 100".into(),
                base_url: "http://192.168.1.60:1400".into(),
            },
        );
        let (rooms, covered) = parse_zone_groups_with_members(legacy_xml, &known).unwrap();
        assert!(covered.contains("RINCON_SUB"));
        assert!(covered.contains("RINCON_SURR_L"));
        assert!(covered.contains("RINCON_HIDDEN"));
        assert_eq!(rooms.len(), 2);
        assert_eq!(rooms[0].id, "RINCON_ARC");
        assert_eq!(rooms[0].name, "Living Room + Kitchen");
        assert_eq!(rooms[0].base_url, "http://192.168.1.50:1400");
        assert_eq!(rooms[0].member_count, 2);
        assert_eq!(rooms[1].id, "RINCON_MISSING_LOC");
        assert_eq!(rooms[1].base_url, "http://192.168.1.60:1400");
        assert_eq!(rooms[1].model, "Sonos Era 100");
    }
}
