use serde::{Deserialize, Serialize};

pub const BUCKETS: usize = 20;

/// The CC region, where charge power is at its plateau. Peak power is only
/// sampled here so that a reading taken deep in the CV taper cannot be mistaken
/// for the charger's real capability.
const CC_LO: f64 = 0.15;
const CC_HI: f64 = 0.55;

/// Seeded charge shape, as a fraction of peak power. The body up to ~80% is
/// measured from contiguous charge sessions in 14 days of this machine's own
/// `powercfg` history: a constant-current plateau to ~50%, then an early taper
/// from ~60% (this hardware throttles sooner than the textbook 80%).
///
/// The tail above 80% is a conservative constant-voltage decline rather than a
/// measurement. An earlier version put it far lower, derived from `UsageEntry`
/// averages, but those cover whole AC periods including time already at full,
/// so they understate the true charge rate badly -- the same pollution that
/// makes them unusable for the curve generally. A direct reading on this
/// machine at 91% SoC showed 13.4 W against a 27.4 W peak, i.e. ~0.49, which
/// the old tail underestimated by half.
///
/// This is only a starting point; every bucket is replaced by observation.
const SEED_SHAPE: [f64; BUCKETS] = [
    0.55, 0.72, 0.85, 1.00, 0.97, 1.00, 1.00, 0.99, 0.96, 0.96, //  0-50%
    0.87, 0.84, 0.82, 0.83, 0.79, 0.67, 0.62, 0.54, 0.45, 0.26, // 50-100%
];

/// Fallback peak until this device's charger is observed, in mW.
const SEED_PEAK_MW: f64 = 26_850.0;

/// Never integrate against a rate below this fraction of peak, or time-to-full
/// diverges as the curve approaches zero.
const MIN_RATE_FRAC: f64 = 0.02;

/// Target quantile for the peak estimate, and the step as a fraction of the
/// current estimate. Tracking the 80th percentile rather than the mean matters:
/// charge observations include trickle and near-full periods, and a mean over
/// them badly understates what the charger can actually deliver -- measured at
/// 17 W against a real 27 W charger before this was changed.
const PEAK_QUANTILE: f64 = 0.80;
const PEAK_STEP: f64 = 0.02;

/// Charge power as a function of state of charge, learned per device.
///
/// Stored as a normalised *shape* plus a separately tracked peak. Keeping them
/// apart means plugging into a weaker charger rescales the whole curve instead
/// of corrupting the learned shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeCurve {
    pub shape: [f64; BUCKETS],
    pub counts: [f64; BUCKETS],
    /// Running 80th-percentile estimate of charge power at the plateau, in mW.
    pub peak: f64,
    pub peak_n: f64,
}

impl Default for ChargeCurve {
    fn default() -> Self {
        ChargeCurve {
            shape: SEED_SHAPE,
            counts: [0.0; BUCKETS],
            peak: SEED_PEAK_MW,
            peak_n: 0.0,
        }
    }
}

fn bucket_of(soc: f64) -> usize {
    ((soc.clamp(0.0, 0.9999) * BUCKETS as f64) as usize).min(BUCKETS - 1)
}

impl ChargeCurve {
    pub fn peak_ready(&self) -> bool {
        self.peak_n >= 3.0
    }

    pub fn peak_mw(&self) -> f64 {
        if self.peak_ready() && self.peak > 1000.0 {
            self.peak
        } else {
            SEED_PEAK_MW
        }
    }

    /// Nudge the peak estimate toward the 80th percentile of what it is shown.
    ///
    /// Robbins-Monro quantile tracking: a step up is taken with weight `q` and
    /// a step down with weight `1-q`, so the estimate settles where 80% of
    /// observations fall below. A single outlier moves it under 2%, but a
    /// genuinely weaker charger pulls it down within a session.
    fn observe_peak(&mut self, implied: f64, weight: f64) {
        if !implied.is_finite() || !(1_000.0..150_000.0).contains(&implied) || weight <= 0.0 {
            return;
        }
        if self.peak_n <= 0.0 {
            self.peak = implied;
            self.peak_n = weight;
            return;
        }
        let step = PEAK_STEP * self.peak * weight.min(4.0);
        self.peak += if implied > self.peak {
            step * PEAK_QUANTILE
        } else {
            -step * (1.0 - PEAK_QUANTILE)
        };
        self.peak = self.peak.clamp(1_000.0, 150_000.0);
        self.peak_n = (self.peak_n + weight).min(2_000.0);
    }

    /// Shape at an arbitrary SoC, linearly interpolated between bucket centres.
    pub fn shape_at(&self, soc: f64) -> f64 {
        let x = soc.clamp(0.0, 1.0) * BUCKETS as f64 - 0.5;
        if x <= 0.0 {
            return self.shape[0];
        }
        if x >= (BUCKETS - 1) as f64 {
            return self.shape[BUCKETS - 1];
        }
        let i = x.floor() as usize;
        let f = x - i as f64;
        self.shape[i] * (1.0 - f) + self.shape[i + 1] * f
    }

    /// Expected charge power at a given SoC, in mW.
    pub fn rate_at(&self, soc: f64) -> f64 {
        (self.shape_at(soc) * self.peak_mw()).max(MIN_RATE_FRAC * self.peak_mw())
    }

    /// Fold in an observed charge rate. `weight` scales with how much time the
    /// observation covers, so a 20-minute segment counts for more than a blip.
    pub fn observe(&mut self, soc: f64, rate_mw: f64, weight: f64) {
        if !rate_mw.is_finite() || rate_mw <= 200.0 || weight <= 0.0 {
            return;
        }
        if (CC_LO..CC_HI).contains(&soc) {
            // Inside the plateau the observation *is* the peak.
            self.observe_peak(rate_mw, weight);
        } else if self.shape_at(soc) >= 0.5 {
            // Outside it, infer the peak the observation implies via the shape
            // we currently believe. Without this, a machine that only ever
            // charges between 60% and 85% would never learn its charger at all
            // and would stay pinned to the seed forever. Weighted down, and not
            // attempted deep in the tail where dividing by a small shape value
            // would amplify any error.
            let implied = rate_mw / self.shape_at(soc);
            self.observe_peak(implied, weight * 0.4);
        }
        // Until the charger's peak is known, a normalised shape would be
        // measured against a guess, so only learn the peak first.
        if !self.peak_ready() {
            return;
        }
        let b = bucket_of(soc);
        let norm = (rate_mw / self.peak_mw()).clamp(0.01, 1.6);
        self.counts[b] = (self.counts[b] + weight).min(200.0);
        let alpha = (weight / self.counts[b]).clamp(0.01, 1.0);
        self.shape[b] += alpha * (norm - self.shape[b]);
    }

    /// Seconds to move from `from_mwh` to `to_mwh`, integrating the learned
    /// curve in 0.5%-SoC steps. This is what makes time-to-full correct:
    /// dividing the remaining energy by the present rate ignores the taper
    /// ahead, and underestimates the last stretch by roughly half.
    pub fn time_to(&self, from_mwh: f64, to_mwh: f64, full_mwh: f64) -> f64 {
        if full_mwh <= 0.0 || to_mwh <= from_mwh {
            return 0.0;
        }
        let step = (full_mwh * 0.005).max(1.0);
        let mut e = from_mwh;
        let mut t = 0.0;
        let mut guard = 0;
        while e < to_mwh && guard < 10_000 {
            let de = (to_mwh - e).min(step);
            let soc = (e + de * 0.5) / full_mwh;
            let rate = self.rate_at(soc);
            t += de / rate * 3600.0;
            e += de;
            guard += 1;
        }
        t
    }

    /// Refine the peak from a complete charge session whose SoC span is too
    /// wide to attribute to a single bucket.
    ///
    /// A session's average rate says nothing about shape -- a run from 41% to
    /// 84% averages 20 W, which is simultaneously below the 27 W plateau and
    /// above the tail. What it does constrain is the integral: how long the
    /// whole span took. Comparing that against what the current curve predicts
    /// yields an implied overall level, and leaves the shape alone.
    pub fn calibrate_from_session(
        &mut self,
        from_mwh: f64,
        to_mwh: f64,
        full_mwh: f64,
        secs: f64,
        weight: f64,
    ) {
        if secs <= 0.0 || to_mwh <= from_mwh || full_mwh <= 0.0 {
            return;
        }
        let predicted = self.time_to(from_mwh, to_mwh, full_mwh);
        if predicted <= 0.0 {
            return;
        }
        self.observe_peak(self.peak_mw() * predicted / secs, weight);
    }

    /// How much of the curve rests on real observation rather than the seed.
    pub fn maturity(&self) -> f64 {
        let learned = self.counts.iter().filter(|&&c| c >= 3.0).count();
        learned as f64 / BUCKETS as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: f64 = 38_820.0;

    #[test]
    fn seeded_curve_reproduces_the_measured_taper() {
        let c = ChargeCurve::default();
        // Measured plateau on this hardware: ~26.9 W.
        assert!((c.rate_at(0.30) - 26_850.0).abs() < 500.0, "{}", c.rate_at(0.30));
        // Around 90% a direct reading gave 13.4 W against a 27.4 W peak.
        let near_full = c.rate_at(0.91);
        assert!(
            (10_000.0..16_000.0).contains(&near_full),
            "tail at 91% should match the observed ~13 W, got {near_full}"
        );
        // And it must still collapse by the very top.
        assert!(c.rate_at(0.99) < near_full * 0.75);
    }

    #[test]
    fn charging_slows_monotonically_across_the_taper() {
        let c = ChargeCurve::default();
        let (a, b, d) = (c.rate_at(0.50), c.rate_at(0.75), c.rate_at(0.95));
        assert!(a > b && b > d, "expected monotone taper, got {a} {b} {d}");
    }

    #[test]
    fn time_to_full_far_exceeds_a_linear_estimate() {
        let c = ChargeCurve::default();
        let from = 0.80 * FULL;
        let learned = c.time_to(from, FULL, FULL);
        // The naive estimate: remaining energy at the *current* 80% rate.
        let naive = (FULL - from) / c.rate_at(0.80) * 3600.0;
        assert!(
            learned > naive * 1.4,
            "curve must punish the CV tail: learned {learned:.0}s vs naive {naive:.0}s"
        );
    }

    #[test]
    fn the_cv_tail_is_far_slower_per_unit_of_energy() {
        let c = ChargeCurve::default();
        let first = c.time_to(0.0, 0.80 * FULL, FULL);
        let last = c.time_to(0.80 * FULL, FULL, FULL);
        // Seconds per mWh: the last fifth of the pack should cost several times
        // more time per unit of energy than everything before it.
        let dens_first = first / (0.80 * FULL);
        let dens_last = last / (0.20 * FULL);
        assert!(
            dens_last > dens_first * 1.8,
            "tail density {dens_last:.4} vs body {dens_first:.4} s/mWh"
        );
        // A fifth of the energy still costs a substantial share of the total.
        assert!(last > first * 0.4, "last 20% {last:.0}s vs first 80% {first:.0}s");
    }

    #[test]
    fn time_to_is_zero_when_target_already_reached() {
        let c = ChargeCurve::default();
        assert_eq!(c.time_to(0.9 * FULL, 0.5 * FULL, FULL), 0.0);
        assert_eq!(c.time_to(FULL, FULL, FULL), 0.0);
    }

    #[test]
    fn peak_is_only_learned_inside_the_cc_region() {
        let mut c = ChargeCurve::default();
        for _ in 0..20 {
            // A slow reading deep in the taper must not be taken for the peak.
            c.observe(0.95, 3_000.0, 1.0);
        }
        assert!(!c.peak_ready(), "CV-region samples must not train the peak");

        let mut c2 = ChargeCurve::default();
        for _ in 0..20 {
            c2.observe(0.30, 31_000.0, 1.0);
        }
        assert!(c2.peak_ready());
        assert!((c2.peak_mw() - 31_000.0).abs() < 1_500.0, "{}", c2.peak_mw());
    }

    /// A machine that is only ever topped up between 60% and 85% never visits
    /// the plateau, but must still learn its charger.
    #[test]
    fn peak_is_inferred_from_partial_charges_above_the_cc_region() {
        let mut c = ChargeCurve::default();
        // Seed shape at 0.70 is 0.79, so 22 W here implies a ~27.8 W charger.
        for _ in 0..40 {
            c.observe(0.70, 22_000.0, 1.0);
        }
        assert!(c.peak_ready(), "a top-up charge must still train the peak");
        assert!(
            (c.peak_mw() - 27_800.0).abs() < 3_000.0,
            "implied peak {} W",
            c.peak_mw() / 1000.0
        );
    }

    #[test]
    fn a_stronger_charger_rescales_the_whole_curve() {
        let mut c = ChargeCurve::default();
        let tail_before = c.shape_at(0.95);
        for _ in 0..30 {
            c.observe(0.30, 40_000.0, 1.0);
        }
        // Shape is preserved; absolute rates scale with the new peak.
        assert!((c.shape_at(0.95) - tail_before).abs() < 0.02);
        assert!(c.rate_at(0.95) > 4_500.0, "{}", c.rate_at(0.95));
    }

    #[test]
    fn observation_overrides_the_seed_for_that_bucket() {
        let mut c = ChargeCurve::default();
        for _ in 0..30 {
            c.observe(0.30, 26_850.0, 1.0);
        }
        // This device turns out to taper hard at 65%, unlike the seed.
        for _ in 0..40 {
            c.observe(0.66, 5_000.0, 2.0);
        }
        // The observed bucket itself converges on 5000/26850 = 0.19.
        let b = bucket_of(0.66);
        assert!(c.shape[b] < 0.25, "seed 0.83 should give way to data, got {}", c.shape[b]);
        // Reading between bucket centres still blends in the untrained
        // neighbour, so it moves less far -- but it must move a long way.
        let learned = c.shape_at(0.66);
        assert!(learned < 0.45, "interpolated value should follow the data: {learned}");
        assert!(c.maturity() > 0.0);
    }

    #[test]
    fn garbage_observations_are_ignored() {
        let mut c = ChargeCurve::default();
        let before = c.shape;
        c.observe(0.3, f64::NAN, 1.0);
        c.observe(0.3, -500.0, 1.0);
        c.observe(0.3, 0.0, 1.0);
        c.observe(0.3, 26_000.0, 0.0);
        assert_eq!(before, c.shape);
        assert!(!c.peak_ready());
    }

    #[test]
    fn integration_terminates_even_with_a_collapsed_curve() {
        let mut c = ChargeCurve::default();
        c.shape = [0.0; BUCKETS];
        let t = c.time_to(0.0, FULL, FULL);
        assert!(t.is_finite() && t > 0.0, "must not diverge, got {t}");
    }
}
