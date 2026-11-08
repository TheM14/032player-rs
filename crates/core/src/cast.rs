//! Sonos output (MusicBridge `SonosPlan.kt` / `SonosQueue.kt`): what the
//! speaker gets for a track, which part of the play order sits in its
//! queue, and how queue edits map onto the speaker queue. Pure functions;
//! the core loop drives them (`controller/cast.rs`).

use std::ops::Range;

use api::{Capabilities, Track, Url};

use crate::queue::Repeat;
use crate::settings::{CastHiRes, CastOpus, CastSettings};

/// Sonos plays at most 24-bit / 48 kHz.
pub const MAX_SAMPLE_RATE: i32 = 48_000;
/// Most entries put into the speaker queue (MusicBridge
/// `SONOS_QUEUE_MAX_TRACKS`); longer queues go in windows.
pub const MAX_QUEUE: usize = 2_000;

/// How the speaker gets a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `/stream`, the original file.
    Original,
    /// `/transcode.mp3?bitrate=320`, generated live: seeking re-opens it
    /// with `offsetMs`.
    LiveMp3,
    /// `/transcode.flac?maxSampleRate=48000`, served from the server cache
    /// (built on the first request).
    StaticFlac,
}

fn codec(track: &Track) -> String {
    track
        .codec
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Lossless codecs (MusicBridge `LOSSLESS_AUDIO_CODECS`, ffprobe names).
pub fn is_lossless(codec: &str) -> bool {
    matches!(
        codec,
        "flac" | "alac" | "wav" | "wave" | "aiff" | "ape" | "wavpack" | "tta" | "pcm"
    ) || codec.starts_with("pcm_")
        || codec.starts_with("dsd")
}

/// Codecs Sonos decodes itself (032 server `sonosCodecs`).
pub fn sonos_decodes(codec: &str) -> bool {
    matches!(
        codec,
        "mp3" | "aac" | "flac" | "alac" | "vorbis" | "wav" | "wave" | "aiff"
    ) || codec.starts_with("pcm_")
}

/// The plan for one track. `Err` when the speaker cannot play it at all
/// (Opus or another foreign codec and no transcoder).
pub fn plan(track: &Track, caps: &Capabilities, settings: &CastSettings) -> Result<Kind, String> {
    let codec = codec(track);
    let opus = codec == "opus";
    let hires = is_lossless(&codec)
        && track.sample_rate.unwrap_or(0) > MAX_SAMPLE_RATE
        && settings.hires != CastHiRes::Original;
    // Known but not playable by Sonos (APE, WavPack, DSD, WMA ...).
    let foreign = !codec.is_empty() && !opus && !sonos_decodes(&codec);
    if !opus && !hires && !foreign {
        return Ok(Kind::Original);
    }
    let flac_wanted =
        (opus && settings.opus == CastOpus::Flac) || (hires && settings.hires == CastHiRes::Flac48);
    if flac_wanted && caps.can_transcode("flac") {
        return Ok(Kind::StaticFlac);
    }
    if caps.can_transcode("mp3") {
        return Ok(Kind::LiveMp3);
    }
    if opus || foreign {
        return Err(format!(
            "服务器未启用转码，无法在 Sonos 播放 {}",
            if opus { "Opus".to_owned() } else { codec }
        ));
    }
    // Hi-res without a transcoder: try the original.
    Ok(Kind::Original)
}

/// MIME type the speaker is told.
pub fn mime(track: &Track, kind: Kind) -> String {
    match kind {
        Kind::LiveMp3 => "audio/mpeg".into(),
        Kind::StaticFlac => "audio/flac".into(),
        Kind::Original => {
            if !track.mime_type.trim().is_empty() {
                return track.mime_type.trim().to_owned();
            }
            match codec(track).as_str() {
                "flac" => "audio/flac",
                "aac" | "alac" => "audio/mp4",
                "vorbis" | "opus" => "audio/ogg",
                "wav" | "wave" => "audio/wav",
                _ => "audio/mpeg",
            }
            .into()
        }
    }
}

/// The part of the play order (`len` entries, `current` playing) that goes
/// into the speaker queue: everything when it fits, else a window from
/// the current entry. `keep` is the start of the window already on the
/// speaker, kept while it still holds the current entry and the next.
pub fn window(len: usize, current: usize, keep: Option<usize>) -> Range<usize> {
    if len <= MAX_QUEUE {
        return 0..len;
    }
    if let Some(start) = keep
        && start <= current
        && current + 1 < start + MAX_QUEUE
        && start + MAX_QUEUE <= len
    {
        return start..start + MAX_QUEUE;
    }
    let start = current.min(len - MAX_QUEUE);
    start..start + MAX_QUEUE
}

/// The speaker's play mode. Shuffle is the core's own play order, so the
/// speaker never shuffles; it only wraps when it holds the whole queue.
pub fn play_mode(repeat: Repeat, whole_queue: bool) -> sonos::PlayMode {
    match repeat {
        Repeat::One => sonos::PlayMode::RepeatOne,
        Repeat::All if whole_queue => sonos::PlayMode::RepeatAll,
        _ => sonos::PlayMode::Normal,
    }
}

/// One edit turning the speaker queue `old` into `new` (032 web player
/// `diffQueue`), or `Replace`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diff {
    Equal,
    /// `new[at..at + count]` goes before old index `at`.
    Insert {
        at: usize,
        count: usize,
    },
    Remove(usize),
    Move {
        from: usize,
        to: usize,
    },
    Replace,
}

pub fn diff<T: PartialEq>(old: &[T], new: &[T]) -> Diff {
    if old == new {
        return Diff::Equal;
    }
    let mut i = 0;
    while i < old.len() && i < new.len() && old[i] == new[i] {
        i += 1;
    }
    if new.len() > old.len() {
        let grow = new.len() - old.len();
        if new[i + grow..] == old[i..] {
            return Diff::Insert { at: i, count: grow };
        }
    } else if new.len() + 1 == old.len() && new[i..] == old[i + 1..] {
        return Diff::Remove(i);
    } else if new.len() == old.len() {
        let mut j = old.len() - 1;
        while j > i && old[j] == new[j] {
            j -= 1;
        }
        // One entry moved down from i to j, or up from j to i.
        if new[i..j] == old[i + 1..=j] && new[j] == old[i] {
            return Diff::Move { from: i, to: j };
        }
        if new[i + 1..=j] == old[i..j] && new[i] == old[j] {
            return Diff::Move { from: j, to: i };
        }
    }
    Diff::Replace
}

/// Identity of a media URI for matching what the speaker reports: the
/// path, without the query (a live entry re-opened at an offset keeps it)
/// and host (the speaker may get a LAN address for a loopback server).
pub fn uri_identity(uri: &str) -> String {
    let uri = uri.trim();
    let uri = uri
        .strip_prefix("x-rincon-mp3radio://")
        .map(|rest| format!("http://{rest}"))
        .unwrap_or_else(|| uri.to_owned());
    match Url::parse(&uri) {
        Ok(url) => url.path().to_owned(),
        Err(_) => uri.split(['?', '#']).next().unwrap_or("").to_owned(),
    }
}

/// The queue entry the speaker plays: its track number when the entry
/// there has the reported URI (or none is reported), else the first entry
/// with that URI.
pub fn resolve_index<S: AsRef<str>>(
    identities: &[S],
    index: Option<usize>,
    track_uri: &str,
) -> Option<usize> {
    let reported = if track_uri.trim().is_empty() {
        String::new()
    } else {
        uri_identity(track_uri)
    };
    if let Some(i) = index.filter(|i| *i < identities.len())
        && (reported.is_empty() || identities[i].as_ref() == reported)
    {
        return Some(i);
    }
    if reported.is_empty() {
        return None;
    }
    identities.iter().position(|id| id.as_ref() == reported)
}

/// A speaker queue entry recognised as a track of this server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueRef {
    pub track_id: i64,
    pub kind: Kind,
    /// `offsetMs` of a live entry.
    pub offset_ms: i64,
}

/// Maps a queued URI to a track of the server at `base_url` with media
/// token `media_token`. The token is matched first (the web panel may push
/// LAN addresses while this app uses a domain), then the origin.
pub fn parse_queue_uri(uri: &str, base_url: &str, media_token: &str) -> Option<QueueRef> {
    let uri = uri.trim();
    let uri = uri
        .strip_prefix("x-rincon-mp3radio://")
        .map(|rest| format!("http://{rest}"))
        .unwrap_or_else(|| uri.to_owned());
    let url = Url::parse(&uri).ok()?;
    let mut segments: Vec<&str> = url.path_segments()?.collect();
    let file = segments.pop()?;
    let id = segments.pop()?;
    if segments.len() < 3 || segments[segments.len() - 3..] != ["api", "v1", "tracks"] {
        return None;
    }
    let track_id: i64 = id.parse().ok()?;
    let kind = match file {
        "stream" => Kind::Original,
        "transcode.flac" => Kind::StaticFlac,
        "transcode.mp3" | "transcode.ogg" => Kind::LiveMp3,
        _ => return None,
    };
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    let token_matches = param("mediaToken").is_some_and(|t| !t.is_empty() && t == media_token);
    let origin_matches = Url::parse(base_url)
        .is_ok_and(|b| b.origin().ascii_serialization() == url.origin().ascii_serialization());
    if !token_matches && !origin_matches {
        return None;
    }
    Some(QueueRef {
        track_id,
        kind,
        offset_ms: param("offsetMs")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .max(0),
    })
}

/// The speaker cannot reach a server on this machine's loopback address:
/// such URLs get this machine's LAN address `lan` instead.
pub fn speaker_url(url: Url, lan: Option<std::net::IpAddr>) -> String {
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match (loopback, lan) {
        (true, Some(ip)) => {
            let mut url = url;
            if url.set_ip_host(ip).is_ok() {
                return url.to_string();
            }
            url.to_string()
        }
        _ => url.to_string(),
    }
}

/// The base URL's host is a loopback address.
pub fn is_loopback_server(base_url: &str) -> bool {
    Url::parse(base_url).is_ok_and(|u| match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    })
}

/// Configured speaker addresses (comma, semicolon or whitespace separated).
pub fn parse_hosts(hosts: &str) -> Vec<String> {
    hosts
        .split([',', ';', ' ', '\n', '\t', '，', '；'])
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(formats: &[&str]) -> Capabilities {
        let formats: serde_json::Map<String, serde_json::Value> = formats
            .iter()
            .map(|f| ((*f).to_owned(), serde_json::json!({"available": true})))
            .collect();
        serde_json::from_value(serde_json::json!({
            "apiRevision": 3,
            "transcode": {"available": true, "formats": formats},
        }))
        .unwrap()
    }

    fn track(codec: &str, rate: i32) -> Track {
        Track {
            id: 7,
            codec: Some(codec.into()),
            sample_rate: Some(rate),
            ..Track::default()
        }
    }

    #[test]
    fn plans_like_musicbridge() {
        let all = caps(&["mp3", "flac", "ogg"]);
        let none = caps(&[]);
        let d = CastSettings::default();
        let flac = CastSettings {
            hires: CastHiRes::Flac48,
            opus: CastOpus::Flac,
            ..CastSettings::default()
        };
        let original = CastSettings {
            hires: CastHiRes::Original,
            ..CastSettings::default()
        };
        assert_eq!(plan(&track("flac", 44_100), &all, &d), Ok(Kind::Original));
        assert_eq!(plan(&track("mp3", 44_100), &all, &d), Ok(Kind::Original));
        assert_eq!(plan(&track("", 0), &all, &d), Ok(Kind::Original));
        assert_eq!(plan(&track("flac", 96_000), &all, &d), Ok(Kind::LiveMp3));
        assert_eq!(
            plan(&track("flac", 96_000), &all, &flac),
            Ok(Kind::StaticFlac)
        );
        assert_eq!(
            plan(&track("flac", 96_000), &all, &original),
            Ok(Kind::Original)
        );
        assert_eq!(plan(&track("flac", 96_000), &none, &d), Ok(Kind::Original));
        assert_eq!(plan(&track("opus", 48_000), &all, &d), Ok(Kind::LiveMp3));
        assert_eq!(
            plan(&track("opus", 48_000), &all, &flac),
            Ok(Kind::StaticFlac)
        );
        // No FLAC on the server: MP3 instead.
        assert_eq!(
            plan(&track("opus", 48_000), &caps(&["mp3"]), &flac),
            Ok(Kind::LiveMp3)
        );
        assert!(plan(&track("opus", 48_000), &none, &d).is_err());
        assert_eq!(plan(&track("ape", 44_100), &all, &d), Ok(Kind::LiveMp3));
        assert_eq!(
            plan(&track("dsd_lsbf", 2_822_400), &all, &flac),
            Ok(Kind::StaticFlac)
        );
        assert!(plan(&track("wmav2", 44_100), &none, &d).is_err());
    }

    #[test]
    fn windows_long_queues() {
        assert_eq!(window(10, 3, None), 0..10);
        assert_eq!(window(5000, 100, None), 100..2100);
        assert_eq!(window(5000, 4500, None), 3000..5000);
        // Kept while it holds the current entry and the next.
        assert_eq!(window(5000, 1500, Some(100)), 100..2100);
        assert_eq!(window(5000, 2099, Some(100)), 2099..4099);
        assert_eq!(window(5000, 50, Some(100)), 50..2050);
    }

    #[test]
    fn diffs_queue_edits() {
        let old = [1, 2, 3, 4, 5];
        assert_eq!(diff(&old, &old), Diff::Equal);
        assert_eq!(
            diff(&old, &[1, 2, 9, 8, 3, 4, 5]),
            Diff::Insert { at: 2, count: 2 }
        );
        assert_eq!(
            diff(&old, &[1, 2, 3, 4, 5, 6]),
            Diff::Insert { at: 5, count: 1 }
        );
        assert_eq!(diff(&old, &[1, 3, 4, 5]), Diff::Remove(1));
        assert_eq!(diff(&old, &[1, 2, 3, 4]), Diff::Remove(4));
        assert_eq!(diff(&old, &[1, 3, 4, 2, 5]), Diff::Move { from: 1, to: 3 });
        assert_eq!(diff(&old, &[1, 4, 2, 3, 5]), Diff::Move { from: 3, to: 1 });
        assert_eq!(diff(&old, &[5, 4, 3, 2, 1]), Diff::Replace);
        assert_eq!(diff(&old, &[]), Diff::Replace);
        assert_eq!(diff(&[], &[1, 2]), Diff::Insert { at: 0, count: 2 });
    }

    #[test]
    fn resolves_the_playing_entry() {
        let ids: Vec<String> = [
            "http://h/api/v1/tracks/1/stream?mediaToken=m",
            "http://h/api/v1/tracks/2/transcode.mp3?bitrate=320&mediaToken=m",
            "http://h/api/v1/tracks/1/stream?mediaToken=m",
        ]
        .iter()
        .map(|u| uri_identity(u))
        .collect();
        // The track number wins when it matches (the same song twice).
        assert_eq!(
            resolve_index(
                &ids,
                Some(2),
                "http://h/api/v1/tracks/1/stream?mediaToken=m"
            ),
            Some(2)
        );
        // A live entry re-opened at an offset keeps its identity.
        assert_eq!(
            resolve_index(
                &ids,
                Some(1),
                "http://10.0.0.2/api/v1/tracks/2/transcode.mp3?offsetMs=5000"
            ),
            Some(1)
        );
        assert_eq!(
            resolve_index(&ids, Some(0), "http://h/api/v1/tracks/2/transcode.mp3"),
            Some(1)
        );
        assert_eq!(resolve_index(&ids, Some(1), ""), Some(1));
        assert_eq!(resolve_index(&ids, None, "x-sonos-http:radio"), None);
        assert_eq!(resolve_index(&ids, Some(9), ""), None);
    }

    #[test]
    fn recognises_queued_tracks() {
        let base = "https://music.example.com";
        assert_eq!(
            parse_queue_uri(
                "http://192.168.1.5:4533/api/v1/tracks/42/transcode.mp3?mediaToken=m&bitrate=320&offsetMs=61000",
                base,
                "m"
            ),
            Some(QueueRef {
                track_id: 42,
                kind: Kind::LiveMp3,
                offset_ms: 61_000
            })
        );
        assert_eq!(
            parse_queue_uri(
                "https://music.example.com/api/v1/tracks/3/stream?mediaToken=old",
                base,
                "m"
            ),
            Some(QueueRef {
                track_id: 3,
                kind: Kind::Original,
                offset_ms: 0
            })
        );
        assert_eq!(
            parse_queue_uri(
                "http://10.0.0.1/api/v1/tracks/3/transcode.flac?mediaToken=x",
                base,
                "m"
            ),
            None
        );
        assert_eq!(
            parse_queue_uri("x-sonos-spotify:spotify%3atrack", base, "m"),
            None
        );
        assert_eq!(
            parse_queue_uri(
                "https://music.example.com/music/api/v1/tracks/5/stream",
                "https://music.example.com/music",
                ""
            ),
            Some(QueueRef {
                track_id: 5,
                kind: Kind::Original,
                offset_ms: 0
            })
        );
    }

    #[test]
    fn rewrites_loopback_servers() {
        let lan = Some("192.168.1.9".parse().unwrap());
        let url = Url::parse("http://127.0.0.1:4533/api/v1/tracks/1/stream?mediaToken=m").unwrap();
        assert_eq!(
            speaker_url(url.clone(), lan),
            "http://192.168.1.9:4533/api/v1/tracks/1/stream?mediaToken=m"
        );
        assert_eq!(
            speaker_url(url, None),
            "http://127.0.0.1:4533/api/v1/tracks/1/stream?mediaToken=m"
        );
        let url = Url::parse("http://localhost:4533/x").unwrap();
        assert_eq!(speaker_url(url, lan), "http://192.168.1.9:4533/x");
        let url = Url::parse("https://music.example.com/x").unwrap();
        assert_eq!(speaker_url(url, lan), "https://music.example.com/x");
        assert!(is_loopback_server("http://localhost:4533"));
        assert!(!is_loopback_server("http://192.168.1.2:4533"));
        assert_eq!(
            parse_hosts(" 10.0.0.2, 10.0.0.3；x\n"),
            ["10.0.0.2", "10.0.0.3", "x"]
        );
    }
}
