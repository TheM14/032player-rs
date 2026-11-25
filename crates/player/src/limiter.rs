//! Zero-allocation stereo-linked 5 ms lookahead peak limiter for the ASIO
//! audio render callback.
//!
//! Architecture:
//! 1. Fixed 5 ms stereo delay line (`delay_lr`), pre-allocated per sample rate.
//! 2. Monotonic sliding-window peak detector (`max_deque`) covering the full
//!    lookahead window `[t - L ..= t]` in O(1) amortized time.
//! 3. Exponential 50 ms release envelope followed by a length-`L` moving
//!    average smoother so gain ramps down cleanly across the entire 5 ms
//!    lookahead before a transient exits the delay line.
//!
//! Audio whose peaks stay at or below `limit` passes through the delay line
//! with an exact `1.0` multiplier (zero coloration).

const LOOKAHEAD_SECS: f64 = 0.005;
const RELEASE_SECS: f64 = 0.050;

/// Stereo-linked lookahead peak limiter with pre-allocated state.
pub struct SoftPeakLimiter {
    sample_rate: u32,
    lookahead_frames: usize,
    max_lookahead_frames: usize,
    /// Interleaved stereo delay buffer (`max_lookahead_frames * 2`).
    delay_lr: Vec<f32>,
    delay_pos: usize,
    /// Fixed-capacity ring deque of `(frame_idx, peak)` in strictly decreasing
    /// peak order for O(1) sliding-window maximum (`max_lookahead_frames + 2`).
    deque: Vec<(u64, f32)>,
    dq_head: usize,
    dq_tail: usize,
    dq_len: usize,
    frame_idx: u64,
    /// Gain envelope after instant attack / 50 ms exponential release.
    held_gain: f64,
    release_coeff: f64,
    /// Length-`lookahead_frames` moving average of `held_gain` providing a
    /// smooth attack ramp that reaches `<= target` right as the transient exits.
    avg_buf: Vec<f64>,
    avg_pos: usize,
    avg_sum: f64,
}

fn lookahead_for_rate(sample_rate: u32) -> usize {
    let rate = sample_rate.max(8_000);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = ((f64::from(rate) * LOOKAHEAD_SECS).round() as usize).max(1);
    frames
}

impl SoftPeakLimiter {
    /// Creates a limiter pre-allocated for `sample_rate` Hz.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        Self::with_max_rate(sample_rate, sample_rate)
    }

    /// Creates a limiter pre-allocated for sample rates up to `max_sample_rate` Hz
    /// so later `reconfigure` calls inside a real-time audio callback perform
    /// zero heap allocations.
    #[must_use]
    pub fn with_max_rate(sample_rate: u32, max_sample_rate: u32) -> Self {
        let rate = sample_rate.max(8_000);
        let max_rate = max_sample_rate.max(rate);
        let max_lookahead_frames = lookahead_for_rate(max_rate);
        let lookahead_frames = lookahead_for_rate(rate).min(max_lookahead_frames);
        let release_coeff = 1.0 - (-1.0 / (RELEASE_SECS * f64::from(rate))).exp();
        Self {
            sample_rate: rate,
            lookahead_frames,
            max_lookahead_frames,
            delay_lr: vec![0.0; max_lookahead_frames * 2],
            delay_pos: 0,
            // Capacity `max_lookahead_frames + 2` holds the `lookahead_frames + 1`
            // window entries without head/tail collision.
            deque: vec![(0, 0.0); max_lookahead_frames + 2],
            dq_head: 0,
            dq_tail: 0,
            dq_len: 0,
            frame_idx: 0,
            held_gain: 1.0,
            release_coeff,
            avg_buf: vec![1.0; max_lookahead_frames],
            avg_pos: 0,
            avg_sum: lookahead_frames as f64,
        }
    }

    /// Reconfigures lookahead and release constants for `sample_rate` Hz without
    /// heap allocation (clamped to the pre-allocated `max_sample_rate` capacity)
    /// and clears internal state.
    pub fn reconfigure(&mut self, sample_rate: u32) {
        let rate = sample_rate.max(8_000);
        self.sample_rate = rate;
        self.lookahead_frames = lookahead_for_rate(rate).min(self.max_lookahead_frames);
        self.release_coeff = 1.0 - (-1.0 / (RELEASE_SECS * f64::from(rate))).exp();
        self.reset();
    }

    /// Current configured sample rate in Hz.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Delay introduced by the lookahead line in stereo frames.
    #[must_use]
    pub const fn latency_frames(&self) -> usize {
        self.lookahead_frames
    }

    /// Clears delay audio and envelope state without heap allocation.
    pub fn reset(&mut self) {
        self.delay_lr[..(self.lookahead_frames * 2)].fill(0.0);
        self.delay_pos = 0;
        self.dq_head = 0;
        self.dq_tail = 0;
        self.dq_len = 0;
        self.frame_idx = 0;
        self.held_gain = 1.0;
        self.avg_buf[..self.lookahead_frames].fill(1.0);
        self.avg_pos = 0;
        self.avg_sum = self.lookahead_frames as f64;
    }

    /// Processes `frames` interleaved stereo samples in-place.
    ///
    /// When `limit_active` is `false`, samples still traverse the fixed 5 ms
    /// delay line at unity gain so switching limiting on or off across track
    /// boundaries never causes a discontinuity or time jump.
    pub fn process_in_place(
        &mut self,
        frames: usize,
        buf_lr: &mut [f32],
        ceiling_linear: f32,
        limit_active: bool,
    ) {
        let limit = if limit_active && ceiling_linear.is_finite() && ceiling_linear > 1e-4 {
            f64::from(ceiling_linear)
        } else {
            f64::INFINITY
        };
        let dq_cap = self.deque.len();
        let lookahead = self.lookahead_frames;
        let lookahead_u64 = lookahead as u64;
        let inv_lookahead = 1.0 / lookahead as f64;

        for f in 0..frames {
            let idx = f * 2;
            let in_l = buf_lr[idx];
            let in_r = buf_lr[idx + 1];

            // Exchange incoming frame with the frame delayed by `lookahead` frames.
            let d_idx = self.delay_pos * 2;
            let del_l = self.delay_lr[d_idx];
            let del_r = self.delay_lr[d_idx + 1];
            self.delay_lr[d_idx] = in_l;
            self.delay_lr[d_idx + 1] = in_r;
            self.delay_pos += 1;
            if self.delay_pos == lookahead {
                self.delay_pos = 0;
            }

            // Update sliding-window peak over `[t - lookahead ..= t]`.
            let peak = in_l.abs().max(in_r.abs());
            let t = self.frame_idx;
            self.frame_idx = t.wrapping_add(1);

            while self.dq_len > 0 {
                let back = if self.dq_tail == 0 {
                    dq_cap - 1
                } else {
                    self.dq_tail - 1
                };
                if self.deque[back].1 <= peak {
                    self.dq_tail = back;
                    self.dq_len -= 1;
                } else {
                    break;
                }
            }
            self.deque[self.dq_tail] = (t, peak);
            self.dq_tail += 1;
            if self.dq_tail == dq_cap {
                self.dq_tail = 0;
            }
            self.dq_len += 1;

            while self.dq_len > 0 {
                let front_idx = self.deque[self.dq_head].0;
                if t.wrapping_sub(front_idx) > lookahead_u64 {
                    self.dq_head += 1;
                    if self.dq_head == dq_cap {
                        self.dq_head = 0;
                    }
                    self.dq_len -= 1;
                } else {
                    break;
                }
            }

            let win_peak = f64::from(self.deque[self.dq_head].1);
            let raw_gain = if win_peak > limit {
                (limit / win_peak).min(1.0)
            } else {
                1.0
            };

            if raw_gain < self.held_gain {
                self.held_gain = raw_gain;
            } else {
                self.held_gain += self.release_coeff * (raw_gain - self.held_gain);
                if self.held_gain > 1.0 - 1e-7 && raw_gain >= 1.0 {
                    self.held_gain = 1.0;
                }
            }

            // Moving average over `lookahead` frames creates a smooth attack ramp.
            let old = self.avg_buf[self.avg_pos];
            self.avg_buf[self.avg_pos] = self.held_gain;
            self.avg_sum += self.held_gain - old;
            self.avg_pos += 1;
            if self.avg_pos == lookahead {
                self.avg_pos = 0;
                // Recompute sum once per window to eliminate long-run f64 drift.
                self.avg_sum = self.avg_buf[..lookahead].iter().sum();
            }

            let smooth_gain = (self.avg_sum * inv_lookahead).clamp(0.0, 1.0);
            // Final safety guard in case ceiling was lowered mid-stream.
            let del_peak = f64::from(del_l.abs().max(del_r.abs()));
            let final_gain = if del_peak * smooth_gain > limit && del_peak > 0.0 {
                (limit / del_peak).min(smooth_gain)
            } else {
                smooth_gain
            };

            if final_gain >= 1.0 - 1e-7 {
                buf_lr[idx] = del_l;
                buf_lr[idx + 1] = del_r;
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let g = final_gain as f32;
                buf_lr[idx] = del_l * g;
                buf_lr[idx + 1] = del_r * g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SoftPeakLimiter;

    fn db_to_lin(db: f64) -> f32 {
        #[allow(clippy::cast_possible_truncation)]
        let v = 10.0_f64.powf(db / 20.0) as f32;
        v
    }

    #[test]
    fn below_ceiling_sine_passes_through_bit_identical() {
        let sr = 48_000;
        let mut lim = SoftPeakLimiter::new(sr);
        let delay = lim.latency_frames();
        let limit = db_to_lin(-1.0); // ~0.89125
        let amp = db_to_lin(-1.5); // strictly below ceiling

        let total = delay + 1024;
        let mut input = vec![0.0f32; total * 2];
        for f in 0..total {
            let s = amp * (2.0 * std::f32::consts::PI * 1000.0 * f as f32 / sr as f32).sin();
            input[f * 2] = s;
            input[f * 2 + 1] = -s * 0.5;
        }
        let mut out = input.clone();
        lim.process_in_place(total, &mut out, limit, true);

        for f in 0..1024 {
            let out_idx = (f + delay) * 2;
            let in_idx = f * 2;
            assert_eq!(out[out_idx], input[in_idx], "L frame {f} altered");
            assert_eq!(out[out_idx + 1], input[in_idx + 1], "R frame {f} altered");
        }
    }

    #[test]
    fn transient_spike_is_clamped_to_ceiling_with_stereo_link() {
        let sr = 48_000;
        let mut lim = SoftPeakLimiter::new(sr);
        let delay = lim.latency_frames();
        let limit = db_to_lin(-1.0);

        // 100 ms buffer with a +4 dB transient in L and half-level in R.
        let total = 4_800;
        let mut buf = vec![0.0f32; total * 2];
        for f in 500..700 {
            buf[f * 2] = 1.6;
            buf[f * 2 + 1] = 0.8;
        }
        lim.process_in_place(total, &mut buf, limit, true);

        let mut max_out = 0.0f32;
        for f in 0..total {
            let l = buf[f * 2];
            let r = buf[f * 2 + 1];
            max_out = max_out.max(l.abs()).max(r.abs());
            assert!(
                l.abs() <= limit + 1e-5 && r.abs() <= limit + 1e-5,
                "frame {f} exceeded ceiling: L={l}, R={r}, limit={limit}"
            );
            // Stereo L/R ratio of 2:1 must remain preserved on every active frame.
            if f >= 500 + delay && f < 700 + delay {
                assert!(
                    (l - 2.0 * r).abs() < 1e-5,
                    "stereo imbalance at frame {f}: L={l}, R={r}"
                );
            }
        }
        assert!(max_out > limit * 0.9, "limiter over-attenuated: {max_out}");
    }

    #[test]
    fn release_recovers_to_unity_after_transient_and_reset_clears_state() {
        let sr = 44_100;
        let mut lim = SoftPeakLimiter::new(sr);
        let limit = db_to_lin(-1.0);

        // Drive a loud burst then 400 ms of quiet steady audio.
        let total = 20_000;
        let mut buf = vec![0.0f32; total * 2];
        for f in 0..400 {
            buf[f * 2] = 2.0;
            buf[f * 2 + 1] = 2.0;
        }
        for f in 400..total {
            buf[f * 2] = 0.25;
            buf[f * 2 + 1] = 0.25;
        }
        lim.process_in_place(total, &mut buf, limit, true);

        let tail = buf[(total - 1) * 2];
        assert!(
            (tail - 0.25).abs() < 1e-3,
            "expected gain to recover near unity, got {tail}"
        );

        // Reset clears delayed tail completely.
        lim.reset();
        let mut zeros = vec![0.0f32; lim.latency_frames() * 2];
        lim.process_in_place(lim.latency_frames(), &mut zeros, limit, true);
        assert!(zeros.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn inactive_limiter_keeps_fixed_delay_without_attenuation() {
        let sr = 48_000;
        let mut lim = SoftPeakLimiter::new(sr);
        let delay = lim.latency_frames();
        let limit = db_to_lin(-1.0);

        let total = delay + 64;
        let mut buf = vec![1.5f32; total * 2];
        lim.process_in_place(total, &mut buf, limit, false);

        // First `delay` frames are the zero-initialized delay line; thereafter exact 1.5.
        assert_eq!(buf[0], 0.0);
        assert_eq!(buf[delay * 2], 1.5);
        assert_eq!(buf[(delay + 10) * 2 + 1], 1.5);
    }
}
