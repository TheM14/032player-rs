//! UPnP SOAP calls: ordered arguments in, the response arguments out.

use std::collections::HashMap;

use crate::Error;

pub(crate) const AV_TRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
pub(crate) const RENDERING_CONTROL: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
pub(crate) const GROUP_RENDERING: &str = "urn:schemas-upnp-org:service:GroupRenderingControl:1";
pub(crate) const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
pub(crate) const ZONE_GROUP_TOPOLOGY: &str = "urn:schemas-upnp-org:service:ZoneGroupTopology:1";

/// Response arguments by element name.
pub(crate) type Values = HashMap<String, String>;

/// XML 1.0 valid character range (Section 2.2): rejects NUL, most C0 controls,
/// and U+FFFE/U+FFFF noncharacters which cause Sonos UPnP XML parsers to reject
/// the entire batch.
fn is_valid_xml_char(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\r'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'..='\u{10FFFF}'
    )
}

/// Escapes text for XML content and attributes, stripping illegal XML 1.0 characters.
pub(crate) fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if is_valid_xml_char(c) => out.push(c),
            _ => {}
        }
    }
    out
}

/// The SOAP envelope for `action`; arguments keep their order (devices
/// are picky about it).
pub(crate) fn envelope(service: &str, action: &str, args: &[(&str, String)]) -> String {
    let mut body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{service}">"#
    );
    for (name, value) in args {
        body.push_str(&format!("<{name}>{}</{name}>", escape(value)));
    }
    body.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
    body
}

/// Text of every direct child of the first element named `wrapper`.
pub(crate) fn response_values(raw: &str, wrapper: &str) -> Option<Values> {
    let doc = roxmltree::Document::parse(raw).ok()?;
    let node = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == wrapper)?;
    Some(
        node.children()
            .filter(roxmltree::Node::is_element)
            .map(|c| {
                let text: String = c
                    .children()
                    .filter_map(|t| t.text())
                    .collect::<Vec<_>>()
                    .concat();
                (c.tag_name().name().to_owned(), text)
            })
            .collect(),
    )
}

#[derive(Clone)]
pub(crate) struct Soap {
    http: reqwest::Client,
}

impl Soap {
    pub(crate) fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub(crate) async fn call(
        &self,
        control_url: &str,
        service: &str,
        action: &str,
        args: &[(&str, String)],
    ) -> Result<Values, Error> {
        let body = envelope(service, action, args);
        let res = self
            .http
            .post(control_url)
            .header("Content-Type", r#"text/xml; charset="utf-8""#)
            .header("SOAPACTION", format!("\"{service}#{action}\""))
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Network(format!("{action}: {e}")))?;
        let status = res.status().as_u16();
        let raw = res
            .text()
            .await
            .map_err(|e| Error::Network(format!("{action}: {e}")))?;
        if status != 200 {
            let fault = response_values(&raw, "UPnPError").unwrap_or_default();
            if let Some(code) = fault
                .get("errorCode")
                .and_then(|c| c.trim().parse::<u32>().ok())
            {
                return Err(Error::Upnp {
                    action: action.to_owned(),
                    code,
                    description: fault.get("errorDescription").cloned().unwrap_or_default(),
                });
            }
            return Err(Error::Http {
                action: action.to_owned(),
                status,
            });
        }
        response_values(&raw, &format!("{action}Response"))
            .ok_or_else(|| Error::Invalid(format!("{action}: no response")))
    }
}

/// `H:MM:SS` for AVTransport `Seek` / `REL_TIME`.
pub(crate) fn format_duration(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// `H+:MM:SS(.fff)` in ms; anything else (`NOT_IMPLEMENTED`, empty, non-finite,
/// negative, or overflowing) is 0.
pub(crate) fn parse_duration(value: &str) -> i64 {
    let parts: Vec<&str> = value.trim().split(':').collect();
    let [h, m, s] = parts.as_slice() else {
        return 0;
    };
    let (Ok(h), Ok(m), Ok(s)) = (h.parse::<i64>(), m.parse::<i64>(), s.parse::<f64>()) else {
        return 0;
    };
    if h < 0 || m < 0 || !s.is_finite() || s < 0.0 || s > 86_400.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let frac = (s * 1000.0).round() as i64;
    let Some(h_ms) = h.checked_mul(3_600_000) else {
        return 0;
    };
    let Some(m_ms) = m.checked_mul(60_000) else {
        return 0;
    };
    h_ms.checked_add(m_ms)
        .and_then(|v| v.checked_add(frac))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_escapes_arguments_in_order() {
        let body = envelope(
            AV_TRANSPORT,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0".into()),
                ("CurrentURI", "http://a/b?x=1&y=<2>".into()),
            ],
        );
        assert!(body.contains(
            "<u:SetAVTransportURI xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID><CurrentURI>http://a/b?x=1&amp;y=&lt;2&gt;</CurrentURI></u:SetAVTransportURI>"
        ));
    }

    #[test]
    fn reads_response_values_and_faults() {
        let raw = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetPositionInfoResponse xmlns:u="urn:x"><Track>3</Track><RelTime>0:01:02</RelTime><TrackMetaData>&lt;DIDL-Lite&gt;&lt;/DIDL-Lite&gt;</TrackMetaData><Empty/></u:GetPositionInfoResponse></s:Body></s:Envelope>"#;
        let v = response_values(raw, "GetPositionInfoResponse").unwrap();
        assert_eq!(v["Track"], "3");
        assert_eq!(v["TrackMetaData"], "<DIDL-Lite></DIDL-Lite>");
        assert_eq!(v["Empty"], "");
        let fault = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>701</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#;
        assert_eq!(
            response_values(fault, "UPnPError").unwrap()["errorCode"],
            "701"
        );
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(3_723_900), "1:02:03");
        assert_eq!(format_duration(-5), "0:00:00");
        assert_eq!(parse_duration("0:03:25"), 205_000);
        assert_eq!(parse_duration("1:00:01.500"), 3_601_500);
        assert_eq!(parse_duration("NOT_IMPLEMENTED"), 0);
        assert_eq!(parse_duration(""), 0);
        assert_eq!(parse_duration("0:00:NaN"), 0);
        assert_eq!(parse_duration("0:00:inf"), 0);
        assert_eq!(parse_duration("-1:00:00"), 0);
        assert_eq!(parse_duration("99999999999999999:00:00"), 0);
    }

    #[test]
    fn escape_strips_illegal_xml_control_chars_and_keeps_valid_whitespace() {
        let dirty = "Song\0\x01\x08\x0B\x0C\x1F\t\n\r & <tag> 曲\u{FFFE}\u{FFFF}";
        assert_eq!(escape(dirty), "Song\t\n\r &amp; &lt;tag&gt; 曲");
    }
}
