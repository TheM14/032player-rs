//! Loudness normalization planner backed by server EBU R128 physical metadata.
//!
//! Uses a high default reference (-11 LUFS) with headroom-aware gain merging
//! and bounded transient soft-knee peak limiting (up to 3.5 dB GR at -1.0 dBTP)
//! so 100%-software-volume + external-DAC setups avoid the "suddenly quieter"
//! feel without crushing wide-dynamic-range recordings.

use api::Track;

use crate::engine::EngineLoudness;
use crate::queue::Queue;
use crate::settings::{LoudnessMode, LoudnessSettings, PeakProtection};

/// Output ceiling protecting DAC oversampling filters from inter-sample overs.
pub const CEILING_DB: f64 = -1.0;
/// Maximum transient gain reduction allowed in `SoftLimiter` mode.
pub const SOFT_LIMITER_MAX_GR_DB: f64 = 3.5;
/// Maximum transient gain reduction allowed in `StrongLimiter` mode.
pub const STRONG_LIMITER_MAX_GR_DB: f64 = 6.0;
/// Hard safety bounds on applied per-track gain.
pub const MIN_GAIN_DB: f64 = -24.0;
pub const MAX_GAIN_DB: f64 = 18.0;
/// Conservative floor for unmeasured-track fallback attenuation.
pub const FALLBACK_MIN_DB: f64 = -6.0;
/// Hysteresis margin above `CEILING_DB` before engaging the limiter.
const LIMITER_ENGAGE_MARGIN_DB: f64 = 0.05;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoudnessSource {
    #[default]
    Bypass,
    Track,
    Album,
    Fallback,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessPlan {
    pub active: bool,
    pub gain_db: f64,
    pub ceiling_db: f64,
    pub use_limiter: bool,
    pub source: LoudnessSource,
    /// Effective target reference used to compute `gain_db` (LUFS).
    pub target_lufs: f64,
    /// Estimated peak transient reduction when `use_limiter` is true (dB >= 0).
    pub peak_reduction_db: f64,
}

impl Default for LoudnessPlan {
    fn default() -> Self {
        Self::bypass()
    }
}

impl LoudnessPlan {
    #[must_use]
    pub const fn bypass() -> Self {
        Self {
            active: false,
            gain_db: 0.0,
            ceiling_db: CEILING_DB,
            use_limiter: false,
            source: LoudnessSource::Bypass,
            target_lufs: -11.0,
            peak_reduction_db: 0.0,
        }
    }

    #[must_use]
    pub const fn to_engine(self) -> EngineLoudness {
        EngineLoudness {
            enabled: self.active,
            gain_db: self.gain_db,
            ceiling_db: self.ceiling_db,
            use_limiter: self.use_limiter,
        }
    }
}

/// Converts the player's cubic software volume (`0..=100`) to dB attenuation (`<= 0`).
#[must_use]
pub fn user_volume_db(volume: f64) -> f64 {
    let v = volume.clamp(0.0, 100.0) / 100.0;
    if v <= 1e-4 {
        -96.0
    } else {
        (20.0 * v.powi(3).log10()).min(0.0)
    }
}

/// Stateless Smart-mode rule: use album loudness only during sequential manual
/// playback where an adjacent manual entry belongs to the same album.
#[must_use]
pub fn smart_use_album(queue: &Queue, pos: usize, manual_queue_count: usize) -> bool {
    if queue.shuffle() {
        return false;
    }
    let manual = manual_queue_count.min(queue.len());
    if pos >= manual {
        return false;
    }
    let Some(cur) = queue.at(pos) else {
        return false;
    };
    let album_id = cur.track.album_id;
    if album_id <= 0 {
        return false;
    }
    let prev_same = pos
        .checked_sub(1)
        .and_then(|p| queue.at(p))
        .is_some_and(|i| i.track.album_id == album_id);
    let next_same = (pos + 1 < manual)
        .then(|| queue.at(pos + 1))
        .flatten()
        .is_some_and(|i| i.track.album_id == album_id);
    prev_same || next_same
}

fn finite(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite())
}

/// Computes the normalization plan for `track`.
#[must_use]
pub fn compute_loudness_plan(
    track: &Track,
    settings: &LoudnessSettings,
    use_album_in_smart: bool,
    volume: f64,
    library_median_lufs: Option<f64>,
    server_supported: bool,
) -> LoudnessPlan {
    if !settings.enabled || !server_supported {
        return LoudnessPlan::bypass();
    }

    let target = settings.target.effective_lufs(library_median_lufs);
    let track_pair = match (finite(track.loudness_lufs), finite(track.true_peak_db)) {
        (Some(lufs), Some(peak)) => Some((lufs, peak)),
        _ => None,
    };
    let album_pair = match (
        finite(track.album_loudness_lufs),
        finite(track.album_true_peak_db),
    ) {
        (Some(lufs), Some(peak)) => Some((lufs, peak)),
        _ => None,
    };

    let want_album = match settings.mode {
        LoudnessMode::Smart => use_album_in_smart,
        LoudnessMode::Album => true,
        LoudnessMode::Track => false,
    };

    let chosen = if want_album {
        album_pair
            .map(|(l, p)| (l, p, LoudnessSource::Album))
            .or_else(|| track_pair.map(|(l, p)| (l, p, LoudnessSource::Track)))
    } else {
        track_pair.map(|(l, p)| (l, p, LoudnessSource::Track))
    };

    let Some((measured_lufs, true_peak_db, source)) = chosen else {
        let fallback = finite(library_median_lufs)
            .map_or(0.0, |median| (target - median).clamp(FALLBACK_MIN_DB, 0.0));
        return LoudnessPlan {
            active: true,
            gain_db: fallback,
            ceiling_db: CEILING_DB,
            use_limiter: false,
            source: LoudnessSource::Fallback,
            target_lufs: target,
            peak_reduction_db: 0.0,
        };
    };

    let vol_db = user_volume_db(volume);
    let raw_gain = target - measured_lufs;
    // Headroom-aware merging: when software volume < 100%, vol_db is negative,
    // expanding linear headroom without triggering the limiter.
    let linear_headroom = (CEILING_DB - true_peak_db) - vol_db;
    let max_gr = match settings.peak_mode {
        PeakProtection::SoftLimiter => SOFT_LIMITER_MAX_GR_DB,
        PeakProtection::StrongLimiter => STRONG_LIMITER_MAX_GR_DB,
        PeakProtection::StrictLinear => 0.0,
    };
    let applied_gain = raw_gain
        .min(linear_headroom + max_gr)
        .clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    let peak_after_all = true_peak_db + applied_gain + vol_db;
    let overshoot = (peak_after_all - CEILING_DB).max(0.0);
    // Only engage the limiter when actually boosting the track; unboosted
    // (0 dB or attenuated) tracks stay pure linear / transparent bypass.
    let use_limiter = settings.peak_mode.uses_limiter()
        && applied_gain > LIMITER_ENGAGE_MARGIN_DB
        && overshoot > LIMITER_ENGAGE_MARGIN_DB;

    LoudnessPlan {
        active: true,
        gain_db: applied_gain,
        ceiling_db: CEILING_DB,
        use_limiter,
        source,
        target_lufs: target,
        peak_reduction_db: if use_limiter { overshoot } else { 0.0 },
    }
}

/// Formats loudness physical metrics and optional active compensation rows
/// for the track "查看信息" dialog.
#[must_use]
pub fn track_loudness_facts(track: &Track, plan: Option<&LoudnessPlan>) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if let Some(lufs) = finite(track.loudness_lufs) {
        let mut line = format!("{lufs:.1} LUFS");
        if let Some(peak) = finite(track.true_peak_db) {
            line.push_str(&format!(" · 真峰值 {peak:+.1} dBTP"));
        }
        if let Some(lra) = finite(track.loudness_range) {
            line.push_str(&format!(" · LRA {lra:.1} LU"));
        }
        rows.push(("集成响度".to_owned(), line));
    }
    if let Some(album_lufs) = finite(track.album_loudness_lufs) {
        let mut line = format!("{album_lufs:.1} LUFS");
        if let Some(album_peak) = finite(track.album_true_peak_db) {
            line.push_str(&format!(" · 真峰值 {album_peak:+.1} dBTP"));
        }
        rows.push(("专辑响度".to_owned(), line));
    }
    if let Some(plan) = plan
        && plan.active
    {
        let src = match plan.source {
            LoudnessSource::Album => "按专辑",
            LoudnessSource::Track => "按单曲",
            LoudnessSource::Fallback => "保守回退",
            LoudnessSource::Bypass => "直通",
        };
        let protection = if plan.use_limiter {
            let kind = if plan.peak_reduction_db > SOFT_LIMITER_MAX_GR_DB + 0.05 {
                "强力限幅"
            } else {
                "柔和限幅"
            };
            format!("{kind}，瞬态上限 -{:.1} dB", plan.peak_reduction_db)
        } else {
            "纯线性".to_owned()
        };
        rows.push((
            "响度补偿".to_owned(),
            format!(
                "{:+.1} dB（{src} · 目标 {:.1} LUFS · {protection}）",
                plan.gain_db, plan.target_lufs
            ),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LoudnessTarget;

    fn sample_track(id: i64, album_id: i64, lufs: Option<f64>, peak: Option<f64>) -> Track {
        Track {
            id,
            album_id,
            loudness_lufs: lufs,
            true_peak_db: peak,
            ..Track::default()
        }
    }

    fn enabled_settings() -> LoudnessSettings {
        LoudnessSettings {
            enabled: true,
            target: LoudnessTarget::Loud,
            mode: LoudnessMode::Smart,
            peak_mode: PeakProtection::SoftLimiter,
        }
    }

    #[test]
    fn modern_loud_pop_attenuates_linearly_without_limiter() {
        let tr = sample_track(1, 10, Some(-8.0), Some(0.5));
        let plan = compute_loudness_plan(&tr, &enabled_settings(), false, 100.0, Some(-10.0), true);
        assert!(plan.active);
        assert_eq!(plan.source, LoudnessSource::Track);
        assert!((plan.gain_db - (-3.0)).abs() < 1e-6);
        assert!(!plan.use_limiter);
        assert_eq!(plan.peak_reduction_db, 0.0);
    }

    #[test]
    fn older_track_uses_soft_limiter_and_classical_is_bounded() {
        // -14 LUFS / -1.0 dBTP -> +3.0 dB boost, peak hits +2.0 dBTP -> 3.0 dB transient GR (within 3.5 dB cap).
        let older = sample_track(2, 10, Some(-14.0), Some(-1.0));
        let p_old =
            compute_loudness_plan(&older, &enabled_settings(), false, 100.0, Some(-10.0), true);
        assert!((p_old.gain_db - 3.0).abs() < 1e-6);
        assert!(p_old.use_limiter);
        assert!((p_old.peak_reduction_db - 3.0).abs() < 1e-6);

        // -22 LUFS / -1.0 dBTP -> raw +11 dB exceeds 0 + 3.5 dB -> capped at +3.5 dB to preserve dynamics.
        let classical = sample_track(3, 10, Some(-22.0), Some(-1.0));
        let p_cls = compute_loudness_plan(
            &classical,
            &enabled_settings(),
            false,
            100.0,
            Some(-10.0),
            true,
        );
        assert!((p_cls.gain_db - 3.5).abs() < 1e-6);
        assert!(p_cls.use_limiter);
        assert!((p_cls.peak_reduction_db - 3.5).abs() < 1e-6);
    }

    #[test]
    fn quiet_low_peak_ost_boosts_linearly_without_limiter() {
        // -16 LUFS / -6.0 dBTP -> +5.0 dB boost -> peak -1.0 dBTP -> no limiter needed.
        let ost = sample_track(4, 10, Some(-16.0), Some(-6.0));
        let plan = compute_loudness_plan(&ost, &enabled_settings(), false, 100.0, None, true);
        assert!((plan.gain_db - 5.0).abs() < 1e-6);
        assert!(!plan.use_limiter);
    }

    #[test]
    fn strict_linear_never_engages_limiter() {
        let mut s = enabled_settings();
        s.peak_mode = PeakProtection::StrictLinear;
        let older = sample_track(5, 10, Some(-14.0), Some(-1.0));
        let plan = compute_loudness_plan(&older, &s, false, 100.0, None, true);
        assert!((plan.gain_db - 0.0).abs() < 1e-6);
        assert!(!plan.use_limiter);
    }

    #[test]
    fn headroom_aware_low_volume_avoids_limiter() {
        // 70% cubic volume is ~ -9.29 dB. -18 LUFS / -1.0 dBTP needs +7 dB; combined peak = -3.29 dBTP < -1 dBTP!
        let quiet = sample_track(6, 10, Some(-18.0), Some(-1.0));
        let plan = compute_loudness_plan(&quiet, &enabled_settings(), false, 70.0, None, true);
        assert!((plan.gain_db - 7.0).abs() < 1e-6);
        assert!(!plan.use_limiter);
    }

    #[test]
    fn missing_loudness_uses_conservative_non_positive_fallback() {
        let unmeasured = sample_track(7, 10, None, None);
        // Target -11, median -9 -> -2.0 dB conservative cut.
        let plan = compute_loudness_plan(
            &unmeasured,
            &enabled_settings(),
            false,
            100.0,
            Some(-9.0),
            true,
        );
        assert_eq!(plan.source, LoudnessSource::Fallback);
        assert!((plan.gain_db - (-2.0)).abs() < 1e-6);
        assert!(!plan.use_limiter);

        // Target -11, median -15 -> never blindly boost -> 0.0 dB.
        let plan_no_boost = compute_loudness_plan(
            &unmeasured,
            &enabled_settings(),
            false,
            100.0,
            Some(-15.0),
            true,
        );
        assert_eq!(plan_no_boost.gain_db, 0.0);

        // Target -18, median -8 -> clamped at -6.0 dB.
        let mut dyn_s = enabled_settings();
        dyn_s.target = LoudnessTarget::Dynamic;
        let plan_clamped =
            compute_loudness_plan(&unmeasured, &dyn_s, false, 100.0, Some(-8.0), true);
        assert_eq!(plan_clamped.gain_db, -6.0);
    }

    #[test]
    fn smart_album_detection_follows_queue_context() {
        let mut q = Queue::with_seed(1);
        q.replace(
            vec![
                sample_track(1, 100, Some(-9.0), Some(-0.5)),
                sample_track(2, 100, Some(-14.0), Some(-1.0)),
                sample_track(3, 200, Some(-10.0), Some(-0.5)),
                sample_track(4, 200, Some(-11.0), Some(-0.5)),
            ],
            0,
        );
        // Manual queue has 3 entries ([0, 1, 2]); entry 3 is in auto-radio zone.
        assert!(smart_use_album(&q, 0, 3));
        assert!(smart_use_album(&q, 1, 3));
        // Entry 2 only borders entry 3 which is in auto-radio zone -> track mode.
        assert!(!smart_use_album(&q, 2, 3));
        assert!(!smart_use_album(&q, 3, 3));

        // Enabling shuffle forces track mode everywhere.
        q.set_shuffle(true);
        assert!(!smart_use_album(&q, 0, 3));
    }

    #[test]
    fn album_mode_falls_back_to_track_when_album_fields_missing() {
        let mut s = enabled_settings();
        s.mode = LoudnessMode::Album;
        let tr = sample_track(8, 10, Some(-9.0), Some(-0.5));
        let plan = compute_loudness_plan(&tr, &s, true, 100.0, None, true);
        assert_eq!(plan.source, LoudnessSource::Track);
        assert!((plan.gain_db - (-2.0)).abs() < 1e-6);

        let mut with_album = tr;
        with_album.album_loudness_lufs = Some(-10.0);
        with_album.album_true_peak_db = Some(-0.2);
        let plan_alb = compute_loudness_plan(&with_album, &s, true, 100.0, None, true);
        assert_eq!(plan_alb.source, LoudnessSource::Album);
        assert!((plan_alb.gain_db - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn formats_track_loudness_facts() {
        let mut tr = sample_track(9, 10, Some(-8.4), Some(0.2));
        tr.loudness_range = Some(6.8);
        tr.album_loudness_lufs = Some(-9.1);
        tr.album_true_peak_db = Some(0.5);
        let plan = compute_loudness_plan(&tr, &enabled_settings(), false, 100.0, None, true);
        let facts = track_loudness_facts(&tr, Some(&plan));
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].0, "集成响度");
        assert!(facts[0].1.contains("-8.4 LUFS"));
        assert!(facts[0].1.contains("+0.2 dBTP"));
        assert!(facts[0].1.contains("LRA 6.8 LU"));
        assert_eq!(facts[1].0, "专辑响度");
        assert_eq!(facts[2].0, "响度补偿");
        assert!(facts[2].1.contains("-2.6 dB"));
        assert!(facts[2].1.contains("-11.0 LUFS"));
        assert!(facts[2].1.contains("纯线性"));
    }

    #[test]
    fn library_median_target_and_strong_limiter_track_hot_library() {
        let median = Some(-9.0);
        let soft = LoudnessSettings {
            enabled: true,
            target: LoudnessTarget::LibraryMedian,
            mode: LoudnessMode::Track,
            peak_mode: PeakProtection::SoftLimiter,
        };
        let strong = LoudnessSettings {
            peak_mode: PeakProtection::StrongLimiter,
            ..soft
        };

        // Median track (-9.0 LUFS): 0 dB gain, untouched!
        let mid = sample_track(10, 1, Some(-9.0), Some(-0.2));
        let p_mid = compute_loudness_plan(&mid, &soft, false, 100.0, median, true);
        assert!((p_mid.target_lufs - (-9.0)).abs() < 1e-6);
        assert!((p_mid.gain_db - 0.0).abs() < 1e-6);
        assert!(!p_mid.use_limiter);

        // Hot J-Pop track (-7.5 LUFS): gentle -1.5 dB linear trim.
        let hot = sample_track(11, 1, Some(-7.5), Some(0.0));
        let p_hot = compute_loudness_plan(&hot, &soft, false, 100.0, median, true);
        assert!((p_hot.gain_db - (-1.5)).abs() < 1e-6);
        assert!(!p_hot.use_limiter);

        // Quiet OST (-15.0 LUFS / -0.5 dBTP):
        // SoftLimiter caps at (-1.0 - -0.5) + 3.5 = +3.0 dB (-12.0 LUFS).
        // StrongLimiter caps at (-1.0 - -0.5) + 6.0 = +5.5 dB (-9.5 LUFS).
        let ost = sample_track(12, 1, Some(-15.0), Some(-0.5));
        let p_soft = compute_loudness_plan(&ost, &soft, false, 100.0, median, true);
        assert!((p_soft.gain_db - 3.0).abs() < 1e-6);
        assert!(p_soft.use_limiter);
        assert!((p_soft.peak_reduction_db - 3.5).abs() < 1e-6);

        let p_strong = compute_loudness_plan(&ost, &strong, false, 100.0, median, true);
        assert!((p_strong.gain_db - 5.5).abs() < 1e-6);
        assert!(p_strong.use_limiter);
        assert!((p_strong.peak_reduction_db - 6.0).abs() < 1e-6);
        let strong_facts = track_loudness_facts(&ost, Some(&p_strong));
        assert!(strong_facts.iter().any(|(_, v)| v.contains("强力限幅")));

        // Extreme medians clamp cleanly to [-16.0, -8.0], and missing median falls back to -11.0.
        assert_eq!(
            LoudnessTarget::LibraryMedian.effective_lufs(Some(-6.0)),
            -8.0
        );
        assert_eq!(
            LoudnessTarget::LibraryMedian.effective_lufs(Some(-20.0)),
            -16.0
        );
        assert_eq!(LoudnessTarget::LibraryMedian.effective_lufs(None), -11.0);

        // Unmeasured fallback under LibraryMedian (-9.0 -> -9.0) yields 0.0 dB (no drop).
        let unmeasured = sample_track(13, 1, None, None);
        let p_fb = compute_loudness_plan(&unmeasured, &soft, false, 100.0, median, true);
        assert_eq!(p_fb.source, LoudnessSource::Fallback);
        assert_eq!(p_fb.gain_db, 0.0);
    }
}
