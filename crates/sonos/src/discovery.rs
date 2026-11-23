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
                http.get(&location).send().await?.text().await
            })
            .await
            .ok()?
            .ok()?;
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
    let mut rooms = Vec::new();
    for player in players.values() {
        let url = format!("{}/ZoneGroupTopology/Control", player.base_url);
        let call = soap.call(&url, ZONE_GROUP_TOPOLOGY, "GetZoneGroupState", &[]);
        if let Ok(Ok(values)) = tokio::time::timeout(DESCRIPTION_TIMEOUT, call).await
            && let Some(state) = values.get("ZoneGroupState")
        {
            rooms = parse_zone_groups(state, &players);
            if !rooms.is_empty() {
                break;
            }
        }
    }
    if rooms.is_empty() {
        rooms = players
            .into_values()
            .map(|p| Room {
                host: host_of(&p.base_url),
                id: p.uuid,
                name: p.name,
                base_url: p.base_url,
                member_count: 1,
                model: p.model,
            })
            .collect();
    }
    rooms.sort_by_key(|r| r.name.to_lowercase());
    Ok(rooms)
}

/// Description URL of a configured speaker address.
fn static_location(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    if host.contains("://") {
        return Some(host.to_owned());
    }
    let with_port = if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:1400")
    };
    Some(format!("http://{with_port}/xml/device_description.xml"))
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
                    let payload = String::from_utf8_lossy(&buf[..n]);
                    if let Some(location) = header_value(&payload, "LOCATION") {
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
            locations.extend(found);
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
pub(crate) fn parse_zone_groups(state: &str, known: &HashMap<String, Player>) -> Vec<Room> {
    let Ok(doc) = roxmltree::Document::parse(state) else {
        return Vec::new();
    };
    let mut rooms = Vec::new();
    for group in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "ZoneGroup")
    {
        let coordinator = group.attribute("Coordinator").unwrap_or("").to_owned();
        if coordinator.is_empty() {
            continue;
        }
        let mut names: Vec<String> = Vec::new();
        let mut base_url = String::new();
        let mut visible = 0;
        for member in group
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "ZoneGroupMember")
        {
            let invisible = member.attribute("Invisible") == Some("1");
            let name = member.attribute("ZoneName").unwrap_or("").trim().to_owned();
            if member.attribute("UUID") == Some(coordinator.as_str()) {
                base_url = origin(member.attribute("Location").unwrap_or(""));
                names.insert(0, name);
            } else if !invisible {
                names.push(name);
            }
            if !invisible {
                visible += 1;
            }
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
            member_count: visible.max(1),
        });
    }
    rooms
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
}
