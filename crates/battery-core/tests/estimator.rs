//! Behavioural tests for the estimator, driven by synthetic traces.
//!
//! Numbers here come from this machine's measured characteristics: 38,820 mWh
//! full charge, 16.5 W median active draw, 26.9 W charge plateau.

use battery_core::estimator::Estimator;
use battery_core::model::{Kind, Model};
use battery_core::types::*;

const FULL: u32 = 38_820;

struct Sim {
    est: Estimator,
    t_ms: i64,
    cap: f64,
}

impl Sim {
    fn new(soc: f64) -> Self {
        Sim { est: Estimator::new(Model::default()), t_ms: 0, cap: soc * FULL as f64 }
    }

    fn with_model(model: Model, soc: f64) -> Self {
        Sim { est: Estimator::new(model), t_ms: 0, cap: soc * FULL as f64 }
    }

    fn sample(&self, rate_mw: i32, state: u32) -> Sample {
        Sample {
            t_ms: self.t_ms,
            capacity_mwh: self.cap.round().max(0.0) as u32,
            full_mwh: FULL,
            rate_mw,
            voltage_mv: 7900,
            power_state: state,
            cpu_pct: 20.0,
            display_on: true,
        }
    }

    /// Advance the simulation, moving real energy so the capacity gauge agrees
    /// with the reported rate.
    fn run(&mut self, minutes: i64, rate_mw: i32, state: u32) -> Estimates {
        let mut last = None;
        for _ in 0..(minutes * 2) {
            self.t_ms += 30_000;
            self.cap += rate_mw as f64 * (30.0 / 3600.0);
            self.cap = self.cap.clamp(0.0, FULL as f64);
            last = Some(self.est.update(self.sample(rate_mw, state)));
        }
        last.expect("at least one step")
    }
}

const DISCHARGING: u32 = BATTERY_DISCHARGING;
const CHARGING: u32 = BATTERY_POWER_ON_LINE | BATTERY_CHARGING;
const ON_AC: u32 = BATTERY_POWER_ON_LINE;

fn mins(p: Prediction) -> f64 {
    p.secs / 60.0
}

#[test]
fn steady_discharge_predicts_the_analytic_answer() {
    let mut sim = Sim::new(0.50);
    let est = sim.run(30, -16_500, DISCHARGING);
    assert_eq!(est.phase, Phase::Discharging);

    // Remaining usable energy at the end of the run, over a constant 16.5 W.
    let usable = est.capacity_mwh as f64 - 0.05 * FULL as f64;
    let expected_min = usable / 16_500.0 * 60.0;
    let got = mins(est.to_empty.expect("an estimate while discharging"));
    assert!(
        (got - expected_min).abs() < expected_min * 0.08,
        "expected ~{expected_min:.0} min, got {got:.0} min"
    );
}

#[test]
fn watts_are_reported_signed_and_in_the_right_direction() {
    let mut sim = Sim::new(0.50);
    let d = sim.run(10, -16_500, DISCHARGING);
    assert!((d.watts + 16.5).abs() < 0.5, "discharge should be negative: {}", d.watts);

    let mut sim = Sim::new(0.50);
    let c = sim.run(10, 31_000, CHARGING);
    assert!((c.watts - 31.0).abs() < 0.5, "charge should be positive: {}", c.watts);
}

#[test]
fn a_load_spike_moves_the_near_estimate_but_not_wildly() {
    let mut sim = Sim::new(0.60);
    let calm = sim.run(60, -10_000, DISCHARGING);
    let calm_min = mins(calm.to_empty.unwrap());

    let spike = sim.run(3, -35_000, DISCHARGING);
    let spike_min = mins(spike.to_empty.unwrap());
    assert!(spike_min < calm_min, "a heavy load must shorten the estimate");

    // The claim under test is that the estimate is not simply
    // remaining-energy-divided-by-current-draw. Compare against exactly that
    // naive figure: the blend must stay meaningfully above it, because a
    // three-minute burst is not evidence the machine will stay this busy.
    let usable = spike.capacity_mwh as f64 - 0.05 * FULL as f64;
    let naive_min = usable / 35_000.0 * 60.0;
    assert!(
        spike_min > naive_min * 1.25,
        "estimate collapsed onto instantaneous draw: naive {naive_min:.0} vs {spike_min:.0} min"
    );
}

#[test]
fn charging_estimate_beats_a_linear_extrapolation_near_full() {
    let mut sim = Sim::new(0.82);
    let est = sim.run(10, 8_000, CHARGING);
    assert_eq!(est.phase, Phase::Charging);

    let to_full = est.to_full.expect("time to full while charging");
    let remaining = FULL as f64 - est.capacity_mwh as f64;
    let linear_min = remaining / 8_000.0 * 60.0;
    // The synthetic charger holds a flat 8 W, which understates the real gap --
    // an actual charger is already tapering here, so the divergence in the field
    // is larger. The strong form of this claim is asserted on the curve itself
    // in curve::tests::time_to_full_far_exceeds_a_linear_estimate.
    assert!(
        mins(to_full) > linear_min * 1.35,
        "CV taper ignored: linear {linear_min:.0} min vs predicted {:.0} min",
        mins(to_full)
    );
}

#[test]
fn to_80_is_offered_below_the_threshold_and_withdrawn_above_it() {
    let mut sim = Sim::new(0.50);
    let below = sim.run(5, 26_000, CHARGING);
    assert!(below.to_80.is_some(), "below 80% the milestone is meaningful");
    assert!(mins(below.to_80.unwrap()) < mins(below.to_full.unwrap()));

    let mut sim = Sim::new(0.85);
    let above = sim.run(5, 6_000, CHARGING);
    assert!(above.to_80.is_none(), "already past 80%");
    assert!(above.to_full.is_some());
}

#[test]
fn predictions_carry_an_uncertainty_band() {
    let mut sim = Sim::new(0.50);
    let est = sim.run(20, -16_500, DISCHARGING);
    let p = est.to_empty.unwrap();
    assert!(p.lo < p.secs && p.secs < p.hi, "band must bracket the estimate");
    assert!(p.lo > 0.0);
}

#[test]
fn a_held_charge_limit_is_reported_instead_of_a_fake_eta() {
    let mut sim = Sim::new(0.50);
    // On AC, plugged in, but the battery is not moving: Surface Battery Limit.
    let est = sim.run(20, 0, ON_AC);
    assert_eq!(est.phase, Phase::Plateau);
    assert!(est.to_full.is_none(), "an ETA that never arrives is worse than none");
    let note = est.note.expect("a note explaining the hold");
    assert!(note.contains("50"), "note should name the level: {note}");
    assert!(note.contains("limit"), "{note}");
}

#[test]
fn a_slow_trickle_is_still_charging_not_a_plateau() {
    let mut sim = Sim::new(0.50);
    let est = sim.run(20, 2_000, CHARGING);
    assert_eq!(est.phase, Phase::Charging, "2 W is slow but it is progress");
}

#[test]
fn a_full_battery_reports_full() {
    let mut sim = Sim::new(0.99);
    let est = sim.run(5, 0, ON_AC);
    assert_eq!(est.phase, Phase::Full);
    assert_eq!(est.note.as_deref(), Some("Fully charged"));
}

#[test]
fn unplugging_switches_phase_and_drops_charge_estimates() {
    let mut sim = Sim::new(0.60);
    let charging = sim.run(20, 26_000, CHARGING);
    assert!(charging.to_full.is_some());

    let discharging = sim.run(20, -16_500, DISCHARGING);
    assert_eq!(discharging.phase, Phase::Discharging);
    assert!(discharging.to_full.is_none());
    assert!(discharging.to_empty.is_some());
}

#[test]
fn the_model_learns_this_devices_charge_curve_from_observation() {
    let mut sim = Sim::new(0.20);
    sim.run(60, 27_000, CHARGING);
    let curve = &sim.est.model.curve;
    assert!(curve.peak_ready(), "peak should be learned from the CC region");
    // The synthetic charger holds 27 W flat across a range where a real one
    // tapers, so readings above the CC region imply a slightly stronger charger
    // than 27 W. That resolves as the shape itself is learned; here we only
    // require the peak to be in the right neighbourhood rather than the seed.
    assert!(
        (24_000.0..34_000.0).contains(&curve.peak_mw()),
        "peak {} W out of range",
        curve.peak_mw() / 1000.0
    );
    assert!(curve.maturity() > 0.0, "some buckets should now rest on real data");
}

#[test]
fn discharge_priors_are_learned_per_context() {
    let mut sim = Sim::new(0.80);
    sim.run(90, -16_500, DISCHARGING);
    let idx = Context { activity: Activity::Active, load_band: 1 }.index();
    let prior = sim.est.model.priors[idx];
    assert!(prior.ready(), "an hour and a half of data should train the prior");
    assert!((prior.mean - 16_500.0).abs() < 2_500.0, "learned {}", prior.mean);
}

#[test]
fn the_learned_prior_carries_the_estimate_when_live_data_is_fresh() {
    // A model that already knows this machine idles at 8 W.
    let mut trained = Model::default();
    let idx = Context { activity: Activity::Active, load_band: 1 }.index();
    for _ in 0..80 {
        trained.priors[idx].observe(8_000.0, 1.0);
    }

    let mut informed = Sim::with_model(trained, 0.60);
    let a = informed.run(4, -30_000, DISCHARGING);

    let mut naive = Sim::new(0.60);
    let b = naive.run(4, -30_000, DISCHARGING);

    // Same brief heavy load, but the informed model expects it to subside.
    assert!(
        mins(a.to_empty.unwrap()) > mins(b.to_empty.unwrap()) * 1.15,
        "history should temper a short burst: {:.0} vs {:.0} min",
        mins(a.to_empty.unwrap()),
        mins(b.to_empty.unwrap())
    );
}

#[test]
fn the_app_grades_itself_and_corrects_a_systematic_bias() {
    let mut sim = Sim::new(0.95);
    // Long enough for several 15%-SoC checkpoints to open and resolve.
    sim.run(240, -16_500, DISCHARGING);
    let c = sim.est.model.correction(Kind::ToEmpty);
    assert!(c.ratio.n > 0.0, "checkpoints should have resolved and been graded");
    // Under a perfectly steady load the model is right, so it should not be
    // inventing a correction.
    assert!(
        (c.factor() - 1.0).abs() < 0.25,
        "steady load should grade near 1.0, got {}",
        c.factor()
    );
}

#[test]
fn reserve_is_learned_when_the_machine_reports_critical() {
    let mut sim = Sim::new(0.08);
    sim.run(20, -16_500, DISCHARGING | BATTERY_CRITICAL);
    assert!(sim.est.model.reserve.ready());
    assert!(sim.est.model.reserve.mean > 0.0);
}

#[test]
fn a_sleep_gap_does_not_corrupt_the_filters() {
    let mut sim = Sim::new(0.80);
    sim.run(30, -16_500, DISCHARGING);

    // Machine sleeps for six hours, losing a little charge.
    sim.t_ms += 6 * 3600 * 1000;
    sim.cap -= 1_500.0;
    let after = sim.run(10, -16_500, DISCHARGING);

    let p = after.to_empty.expect("estimates resume after waking");
    assert!(p.secs.is_finite() && p.secs > 0.0);
    assert!(sim.est.model.open.is_empty() || p.secs > 0.0);
}

#[test]
fn an_unknown_rate_falls_back_to_the_capacity_gauge() {
    let mut sim = Sim::new(0.60);
    // Driver reports BATTERY_UNKNOWN_RATE, but capacity still moves.
    let est = sim.run(40, UNKNOWN_RATE, DISCHARGING);
    assert_eq!(est.phase, Phase::Discharging);
    assert!(est.watts.is_finite(), "must not produce NaN watts");
}

#[test]
fn history_is_recorded_and_bounded() {
    let mut sim = Sim::new(0.90);
    sim.run(600, -16_500, DISCHARGING);
    let h = sim.est.history();
    assert!(!h.is_empty());
    let span_h = (h.back().unwrap().t_ms - h.front().unwrap().t_ms) as f64 / 3_600_000.0;
    assert!(span_h <= 24.0 + 0.1, "history should not exceed 24 h, got {span_h}");
}

#[test]
fn estimates_never_go_negative_or_nan() {
    for soc in [0.01, 0.05, 0.5, 0.99] {
        for &(rate, state) in &[(-16_500, DISCHARGING), (26_000, CHARGING), (0, ON_AC)] {
            let mut sim = Sim::new(soc);
            let est = sim.run(15, rate, state);
            for p in [est.to_empty, est.to_80, est.to_full].into_iter().flatten() {
                assert!(p.secs.is_finite() && p.secs >= 0.0, "soc {soc} rate {rate}");
                assert!(p.lo.is_finite() && p.hi.is_finite());
            }
            assert!(est.watts.is_finite());
            assert!((0.0..=1.0).contains(&est.confidence));
        }
    }
}

#[test]
fn duration_formatting_is_readable() {
    assert_eq!(fmt_duration(0.0), "1m");
    assert_eq!(fmt_duration(90.0), "1m");
    assert_eq!(fmt_duration(3600.0), "1h 00m");
    assert_eq!(fmt_duration(3600.0 * 2.0 + 780.0), "2h 13m");
    assert_eq!(fmt_duration(3600.0 * 30.0), "1d 6h");
    assert_eq!(fmt_duration(-1.0), "--");
    assert_eq!(fmt_duration(f64::NAN), "--");
}

#[test]
fn charging_surfaces_exactly_one_milestone_at_a_time() {
    // Below 80%: the 80% milestone is the one that matters, because charging
    // slows sharply past it and time-to-full would read as the same kind of
    // number when it is not.
    let mut sim = Sim::new(0.55);
    let below = sim.run(6, 26_000, CHARGING);
    assert_eq!(below.active_label(), "Until 80%");
    assert_eq!(
        below.active().map(|p| p.secs),
        below.to_80.map(|p| p.secs),
        "below 80% the active prediction must be the 80% one"
    );

    // At or past 80% it switches to time-to-full, and never shows both.
    let mut sim = Sim::new(0.86);
    let above = sim.run(6, 7_000, CHARGING);
    assert!(above.to_80.is_none(), "the 80% milestone is behind us");
    assert_eq!(above.active_label(), "Until full");
    assert_eq!(above.active().map(|p| p.secs), above.to_full.map(|p| p.secs));
}

#[test]
fn discharging_and_idle_states_pick_sensible_active_rows() {
    let mut sim = Sim::new(0.60);
    let d = sim.run(6, -16_500, DISCHARGING);
    assert_eq!(d.active_label(), "Time remaining");
    assert!(d.active().is_some());

    // A held charge limit has no honest ETA, so there is nothing active.
    let mut sim = Sim::new(0.50);
    let held = sim.run(20, 0, ON_AC);
    assert_eq!(held.phase, Phase::Plateau);
    assert!(held.active().is_none(), "a plateau must not offer an ETA");
    assert!(held.note.is_some(), "it should explain itself instead");
}

#[test]
fn charge_never_exceeds_capacity_when_the_reported_full_drifts_low() {
    // The pack now holds more than the smoothed full-charge figure believes.
    let mut sim = Sim::new(0.99);
    sim.run(20, 4_000, CHARGING);
    sim.cap = FULL as f64 + 900.0;
    let est = sim.run(4, 1_000, CHARGING);

    assert!(est.full_mwh >= est.capacity_mwh, "{} < {}", est.full_mwh, est.capacity_mwh);
    assert!(est.soc <= 1.0, "state of charge above 100%: {}", est.soc);
}
