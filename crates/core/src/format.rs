//! Display formatting shared by every view.

use api::Track;

/// "FLAC / 24bit / 48kHz" for lossless, "MP3 / 320kbps / 44.1kHz" for lossy.
/// Missing parts are skipped (`renderer/utils/audio-quality.ts`).
pub fn audio_quality(track: &Track) -> Option<String> {
    let codec = track
        .codec
        .as_deref()
        .filter(|c| !c.is_empty())
        .or(Some(track.container.as_str()).filter(|c| !c.is_empty()))
        .map(str::to_uppercase);
    let depth = match (track.bit_depth, track.bitrate_kbps) {
        (Some(d), _) if d > 0 => Some(format!("{d}bit")),
        (_, Some(b)) if b > 0 => Some(format!("{b}kbps")),
        _ => None,
    };
    let rate = track
        .sample_rate
        .filter(|r| *r > 0)
        .map(|hz| format!("{}kHz", trim_decimal(f64::from(hz) / 1000.0)));
    let parts: Vec<String> = [codec, depth, rate].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" / "))
}

/// Album quality (`getAlbumAudioQuality`): the shared label when every
/// track matches, otherwise "最高 " + the best track's label (bit depth,
/// then sample rate, then bitrate).
pub fn album_quality(tracks: &[Track]) -> Option<String> {
    let rank = |t: &Track| {
        (
            t.bit_depth.unwrap_or(0),
            t.sample_rate.unwrap_or(0),
            t.bitrate_kbps.unwrap_or(0),
        )
    };
    let mut labels = std::collections::HashSet::new();
    let mut best: Option<&Track> = None;
    for track in tracks {
        let Some(label) = audio_quality(track) else {
            continue;
        };
        labels.insert(label);
        if best.is_none_or(|b| rank(track) > rank(b)) {
            best = Some(track);
        }
    }
    let quality = audio_quality(best?)?;
    Some(if labels.len() > 1 {
        format!("最高 {quality}")
    } else {
        quality
    })
}

/// "1 小时 5 分钟" / "42 分钟" for totals (album, playlist).
pub fn total_duration(millis: i64) -> String {
    let minutes = (millis.max(0) + 30_000) / 60_000;
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, m) => format!("{m} 分钟"),
        (h, 0) => format!("{h} 小时"),
        (h, m) => format!("{h} 小时 {m} 分钟"),
    }
}

/// `Number(x.toFixed(1))`: one decimal, trailing ".0" dropped.
fn trim_decimal(value: f64) -> String {
    let s = format!("{value:.1}");
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

/// `m:ss`, or `h:mm:ss` from one hour.
pub fn duration(millis: i64) -> String {
    let total = millis.max(0) / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Audio spec of a set of tracks from the first one that has a codec
/// (`audioSpecLabel` on the home page): "FLAC / 24bit / 96kHz".
pub fn spec_label(tracks: &[Track]) -> Option<String> {
    let track = tracks
        .iter()
        .find(|t| t.codec.as_deref().is_some_and(|c| !c.is_empty()) || !t.container.is_empty())?;
    let codec = track
        .codec
        .as_deref()
        .filter(|c| !c.is_empty())
        .unwrap_or(&track.container)
        .trim()
        .to_uppercase();
    let mut parts = vec![codec];
    if let Some(d) = track.bit_depth.filter(|d| *d > 0) {
        parts.push(format!("{d}bit"));
    }
    if let Some(r) = track.sample_rate.filter(|r| *r > 0) {
        parts.push(format!("{}kHz", trim_decimal(f64::from(r) / 1000.0)));
    }
    parts.retain(|p| !p.is_empty());
    (!parts.is_empty()).then(|| parts.join(" / "))
}

fn parse_time(iso: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(iso.trim())
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// "3 天前" (dayjs `fromNow`, zh-cn); empty for a missing or bad time.
pub fn relative_time(iso: &str) -> String {
    relative_time_at(iso, chrono::Utc::now())
}

fn relative_time_at(iso: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Some(time) = parse_time(iso) else {
        return String::new();
    };
    let seconds = (now - time).num_seconds();
    let future = seconds < 0;
    #[allow(clippy::cast_precision_loss)]
    let s = seconds.unsigned_abs() as f64;
    let (m, h, d) = (s / 60.0, s / 3600.0, s / 86400.0);
    // dayjs relativeTime thresholds.
    #[allow(clippy::cast_possible_truncation)]
    let round = |x: f64| x.round() as i64;
    let text = if s < 45.0 {
        "几秒".to_owned()
    } else if s < 90.0 {
        "1 分钟".to_owned()
    } else if m < 45.0 {
        format!("{} 分钟", round(m))
    } else if m < 90.0 {
        "1 小时".to_owned()
    } else if h < 22.0 {
        format!("{} 小时", round(h))
    } else if h < 36.0 {
        "1 天".to_owned()
    } else if d < 26.0 {
        format!("{} 天", round(d))
    } else if d < 46.0 {
        "1 个月".to_owned()
    } else if d < 320.0 {
        format!("{} 个月", round(d / 30.4).max(2))
    } else if d < 548.0 {
        "1 年".to_owned()
    } else {
        format!("{} 年", round(d / 365.0).max(2))
    };
    if future {
        format!("{text}后")
    } else {
        format!("{text}前")
    }
}

/// Local date and time, "2026年10月12日 09:15" (dayjs `lll`, zh-cn).
pub fn date_time(iso: &str) -> String {
    parse_time(iso).map_or_else(String::new, |t| {
        t.with_timezone(&chrono::Local)
            .format("%Y年%-m月%-d日 %H:%M")
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times() {
        let now = parse_time("2026-10-28T12:00:00Z").unwrap();
        let r = |iso| relative_time_at(iso, now);
        assert_eq!(r("2026-10-28T11:59:30.000Z"), "几秒前");
        assert_eq!(r("2026-10-28T11:30:00Z"), "30 分钟前");
        assert_eq!(r("2026-10-28T02:00:00Z"), "10 小时前");
        assert_eq!(r("2026-10-25T12:00:00Z"), "3 天前");
        assert_eq!(r("2026-07-28T12:00:00Z"), "3 个月前");
        assert_eq!(r("2023-10-28T12:00:00Z"), "3 年前");
        assert_eq!(r(""), "");
        assert_eq!(r("garbage"), "");
        assert!(date_time("2026-10-12T03:15:00.123Z").starts_with("2026年10月12日"));
    }

    #[test]
    fn spec_labels() {
        let t = Track {
            codec: Some("flac".into()),
            bit_depth: Some(24),
            sample_rate: Some(96_000),
            ..Track::default()
        };
        assert_eq!(
            spec_label(&[Track::default(), t]).as_deref(),
            Some("FLAC / 24bit / 96kHz")
        );
        assert_eq!(spec_label(&[Track::default()]), None);
    }

    #[test]
    fn quality_labels() {
        let flac = Track {
            codec: Some("flac".into()),
            bit_depth: Some(24),
            sample_rate: Some(48_000),
            ..Track::default()
        };
        assert_eq!(
            audio_quality(&flac).as_deref(),
            Some("FLAC / 24bit / 48kHz")
        );
        let mp3 = Track {
            container: "mp3".into(),
            bitrate_kbps: Some(320),
            sample_rate: Some(44_100),
            ..Track::default()
        };
        assert_eq!(
            audio_quality(&mp3).as_deref(),
            Some("MP3 / 320kbps / 44.1kHz")
        );
        assert_eq!(audio_quality(&Track::default()), None);
    }

    #[test]
    fn album_quality_is_shared_or_up_to_the_best() {
        let t = |depth, rate| Track {
            codec: Some("flac".into()),
            bit_depth: Some(depth),
            sample_rate: Some(rate),
            ..Track::default()
        };
        assert_eq!(
            album_quality(&[t(16, 44_100), t(16, 44_100)]).as_deref(),
            Some("FLAC / 16bit / 44.1kHz")
        );
        assert_eq!(
            album_quality(&[t(16, 96_000), t(24, 48_000), Track::default()]).as_deref(),
            Some("最高 FLAC / 24bit / 48kHz")
        );
        assert_eq!(album_quality(&[Track::default()]), None);
        assert_eq!(total_duration(42 * 60_000), "42 分钟");
        assert_eq!(total_duration(65 * 60_000 + 10_000), "1 小时 5 分钟");
        assert_eq!(total_duration(120 * 60_000), "2 小时");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(61_999), "1:01");
        assert_eq!(duration(3_600_000 + 5_000), "1:00:05");
        assert_eq!(duration(-5), "0:00");
    }
}
