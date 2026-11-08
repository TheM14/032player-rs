//! DIDL-Lite: the metadata Sonos shows for a queue entry, and the entries
//! read back from the queue.

use crate::soap::{escape, format_duration};

/// One queue entry: the URI the speaker fetches and what it displays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub uri: String,
    /// `audio/flac`, ...; empty: `audio/mpeg`.
    pub mime: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Cover URL the speaker can fetch.
    pub art_uri: String,
    pub duration_ms: i64,
    /// Opaque stable item id (`032-track-{id}`).
    pub id: String,
}

impl Item {
    /// The item as a single-item DIDL-Lite document.
    pub fn didl(&self) -> String {
        let mime = if self.mime.is_empty() {
            "audio/mpeg"
        } else {
            &self.mime
        };
        let id = if self.id.is_empty() {
            "032-item"
        } else {
            &self.id
        };
        let mut b = String::from(
            r#"<DIDL-Lite xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:r="urn:schemas-rinconnetworks-com:metadata-1-0/" xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/">"#,
        );
        b.push_str(&format!(
            r#"<item id="{}" parentID="032" restricted="true">"#,
            escape(id)
        ));
        let title = if self.title.trim().is_empty() {
            "Unknown"
        } else {
            &self.title
        };
        b.push_str(&format!("<dc:title>{}</dc:title>", escape(title)));
        if !self.artist.is_empty() {
            b.push_str(&format!(
                "<dc:creator>{0}</dc:creator><upnp:artist>{0}</upnp:artist>",
                escape(&self.artist)
            ));
        }
        if !self.album.is_empty() {
            b.push_str(&format!("<upnp:album>{}</upnp:album>", escape(&self.album)));
        }
        if !self.art_uri.is_empty() {
            b.push_str(&format!(
                "<upnp:albumArtURI>{}</upnp:albumArtURI>",
                escape(&self.art_uri)
            ));
        }
        b.push_str("<upnp:class>object.item.audioItem.musicTrack</upnp:class>");
        b.push_str(&format!(
            r#"<res protocolInfo="http-get:*:{}:*""#,
            escape(mime)
        ));
        if self.duration_ms > 0 {
            b.push_str(&format!(
                r#" duration="{}.000""#,
                format_duration(self.duration_ms)
            ));
        }
        b.push_str(&format!(">{}</res></item></DIDL-Lite>", escape(&self.uri)));
        b
    }
}

/// What a queue Browse result tells about an entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueueEntry {
    pub title: String,
    pub uri: String,
}

pub(crate) fn parse_didl(doc: &str) -> Vec<QueueEntry> {
    if doc.trim().is_empty() {
        return Vec::new();
    }
    let Ok(doc) = roxmltree::Document::parse(doc) else {
        return Vec::new();
    };
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "item")
        .map(|item| {
            let child_text = |name: &str| {
                item.children()
                    .find(|c| c.is_element() && c.tag_name().name() == name)
                    .and_then(|c| c.text())
                    .unwrap_or("")
                    .trim()
                    .to_owned()
            };
            QueueEntry {
                title: child_text("title"),
                uri: child_text("res"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn didl_round_trip() {
        let item = Item {
            uri: "http://h/api/v1/tracks/7/stream?mediaToken=a&b=1".into(),
            mime: "audio/flac".into(),
            title: "歌 & <曲>".into(),
            artist: "A".into(),
            album: "B".into(),
            art_uri: "http://h/art?x=1&size=512".into(),
            duration_ms: 205_400,
            id: "032-track-7".into(),
        };
        let didl = item.didl();
        assert!(didl.contains(r#"<res protocolInfo="http-get:*:audio/flac:*" duration="0:03:25.000">http://h/api/v1/tracks/7/stream?mediaToken=a&amp;b=1</res>"#));
        assert!(didl.contains("<dc:title>歌 &amp; &lt;曲&gt;</dc:title>"));
        let parsed = parse_didl(&didl);
        assert_eq!(
            parsed,
            vec![QueueEntry {
                title: "歌 & <曲>".into(),
                uri: item.uri.clone(),
            }]
        );
        assert!(parse_didl("NOT_IMPLEMENTED").is_empty());
    }
}
