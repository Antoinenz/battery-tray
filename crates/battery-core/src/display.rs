//! Pacing the charge readout.
//!
//! The gauge reports whole steps, so a two-decimal figure taken straight from
//! it lurches. [`SocTrack`] says where the reading was and how fast charge is
//! flowing; this turns that into a number that advances evenly.
//!
//! Dead reckoning on its own is not enough. The rate it would integrate comes
//! from a filter that steps whenever a new sample lands, so the counter would
//! visibly change pace, and it would stall whenever drift hit the gauge-step
//! bound. This carries its own value and moves it at a velocity that itself
//! eases toward the measured one, so the readout accelerates and decelerates
//! with load instead of switching between speeds.

use crate::types::SocTrack;

/// How quickly the shown speed catches up with the measured one. Long enough
/// that a passing load spike does not visibly change the counter's pace.
const VELOCITY_TAU_MS: f64 = 6_000.0;
/// How quickly the shown value is drawn back toward the reading.
const CORRECTION_TAU_MS: f64 = 9_000.0;
/// The furthest the shown value may sit from the reading, in gauge steps.
const MAX_LEAD_STEPS: f64 = 1.5;
/// Gaps longer than this mean the machine was away; integrating across one
/// would fling the counter.
const MAX_STEP_MS: i64 = 5_000;

#[derive(Debug, Default, Clone, Copy)]
pub struct SocDisplay {
    value: Option<f64>,
    /// Charge level per millisecond, eased rather than taken raw.
    velocity: f64,
    last_ms: i64,
}

impl SocDisplay {
    /// Forget the current pacing, for when the battery changes direction.
    pub fn reset(&mut self) {
        self.value = None;
        self.velocity = 0.0;
    }

    pub fn velocity(&self) -> f64 {
        self.velocity
    }

    pub fn update(&mut self, track: &SocTrack, now_ms: i64) -> f64 {
        let truth = track.base;
        let Some(prev) = self.value else {
            self.value = Some(truth);
            self.velocity = track.per_ms;
            self.last_ms = now_ms;
            return truth;
        };

        let dt = (now_ms - self.last_ms).clamp(0, MAX_STEP_MS) as f64;
        self.last_ms = now_ms;
        if dt <= 0.0 {
            return prev;
        }

        // Ease the speed toward what the battery is actually doing.
        let blend = 1.0 - (-dt / VELOCITY_TAU_MS).exp();
        self.velocity += (track.per_ms - self.velocity) * blend;

        // Advance, then lean back toward the reading. The correction keeps the
        // figure honest without ever being visible as a jump.
        let pull = 1.0 - (-dt / CORRECTION_TAU_MS).exp();
        let mut next = prev + self.velocity * dt + (truth - prev) * pull;

        // Never against the direction of travel: a counter that ticks backwards
        // while the battery drains reads as a fault.
        if self.velocity < 0.0 {
            next = next.min(prev);
        } else if self.velocity > 0.0 {
            next = next.max(prev);
        }

        // And never far from the reading, however wrong the velocity estimate.
        let lead = MAX_LEAD_STEPS * track.quantum;
        next = next.clamp(truth - lead, truth + lead).clamp(0.0, 1.0);

        self.value = Some(next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUANTUM: f64 = 10.0 / 39_000.0;

    /// A battery draining at a steady rate, its gauge stepping as it goes.
    fn drain(soc: f64, per_ms: f64, as_of_ms: i64) -> SocTrack {
        SocTrack { base: soc, as_of_ms, per_ms, quantum: QUANTUM }
    }

    /// Watts drawn from a 39 Wh pack, as a charge fraction per millisecond.
    /// Negative means draining.
    fn rate_w(watts: f64) -> f64 {
        -watts / 39.0 / 3_600_000.0
    }

    #[test]
    fn a_steady_load_produces_evenly_spaced_steps() {
        let mut d = SocDisplay::default();
        let per_ms = rate_w(16.5);
        let mut soc = 0.80;
        let mut t = 0i64;
        let mut shown = Vec::new();

        // Ten minutes at 250 ms, with the gauge quantising as a real one does.
        for i in 0..2400 {
            soc += per_ms * 250.0;
            t += 250;
            let quantised = (soc / QUANTUM).floor() * QUANTUM;
            let v = d.update(&drain(quantised, per_ms, t), t);
            if i > 400 {
                shown.push(v);
            }
        }

        let deltas: Vec<f64> = shown.windows(2).map(|w| w[0] - w[1]).collect();
        let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
        assert!(mean > 0.0, "should be draining");
        // Every step within a few percent of the average: no stalls, no lurches.
        for (i, dlt) in deltas.iter().enumerate() {
            assert!(
                (dlt - mean).abs() < mean * 0.15,
                "step {i} was {dlt:.3e}, mean {mean:.3e}"
            );
        }
    }

    #[test]
    fn the_counter_never_ticks_backwards_while_draining() {
        let mut d = SocDisplay::default();
        let per_ms = rate_w(16.5);
        let mut t = 0i64;
        let mut last = 1.0;
        // The gauge holds still for long stretches, then drops a whole step.
        for i in 0..600 {
            t += 250;
            let quantised = 0.80 - (i / 40) as f64 * QUANTUM;
            let v = d.update(&drain(quantised, per_ms, t), t);
            assert!(v <= last + 1e-12, "went up at {i}: {last} -> {v}");
            last = v;
        }
    }

    #[test]
    fn speed_changes_are_eased_rather_than_switched() {
        let mut d = SocDisplay::default();
        let calm = rate_w(8.0);
        let mut t = 0i64;
        for _ in 0..200 {
            t += 250;
            d.update(&drain(0.60, calm, t), t);
        }
        let before = d.velocity();

        // A sudden heavy load. The pace must move toward it, not snap to it.
        let heavy = rate_w(40.0);
        t += 250;
        d.update(&drain(0.60, heavy, t), t);
        let after = d.velocity();

        assert!(after < before, "should be speeding up");
        assert!(
            after > heavy * 0.25,
            "eased in over time, not switched: {after:.3e} vs target {heavy:.3e}"
        );
    }

    #[test]
    fn the_shown_value_stays_close_to_the_reading() {
        let mut d = SocDisplay::default();
        // A velocity wildly larger than reality: the bound must hold it in.
        let lying = rate_w(200.0);
        let mut t = 0i64;
        for _ in 0..400 {
            t += 250;
            let v = d.update(&drain(0.50, lying, t), t);
            assert!(
                (v - 0.50).abs() <= MAX_LEAD_STEPS * QUANTUM + 1e-12,
                "ran away to {v}"
            );
        }
    }

    #[test]
    fn charging_runs_the_other_way_and_stays_in_range() {
        let mut d = SocDisplay::default();
        let per_ms = -rate_w(30.0);
        let mut t = 0i64;
        let mut last = 0.0;
        for _ in 0..300 {
            t += 250;
            let v = d.update(&drain(0.995, per_ms, t), t);
            assert!(v >= last - 1e-12, "went down while charging");
            assert!((0.0..=1.0).contains(&v));
            last = v;
        }
    }

    #[test]
    fn a_long_gap_does_not_fling_the_counter() {
        let mut d = SocDisplay::default();
        let per_ms = rate_w(16.5);
        let mut t = 0i64;
        for _ in 0..100 {
            t += 250;
            d.update(&drain(0.70, per_ms, t), t);
        }
        // Machine asleep for six hours.
        t += 6 * 3600 * 1000;
        let v = d.update(&drain(0.55, per_ms, t), t);
        assert!(
            (v - 0.55).abs() <= MAX_LEAD_STEPS * QUANTUM + 1e-12,
            "should land near the new reading, got {v}"
        );
    }
}
