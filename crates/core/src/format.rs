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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn durations() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(61_999), "1:01");
        assert_eq!(duration(3_600_000 + 5_000), "1:00:05");
        assert_eq!(duration(-5), "0:00");
    }
}
