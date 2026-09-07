use crate::filter::{Ewma, Slope, Stat};
use crate::model::{Kind, Model, OpenPrediction};
use crate::types::*;
use std::collections::VecDeque;

/// Live draw dominates the first ~10 minutes of the horizon.
const TAU_NOW_S: f64 = 600.0;
/// The last hour's behaviour bridges to the long-run historical prior.
const TAU_RECENT_S: f64 = 3600.0;

const INTEGRATION_STEP_S: f64 = 60.0;
const MAX_HORIZON_S: f64 = 72.0 * 3600.0;
/// Below this the reading is noise, not a meaningful load.
const MIN_POWER_MW: f64 = 300.0;

/// A capacity change smaller than this fraction of full, sustained on AC, is a
/// charge limit holding the battery rather than a charge in progress.
const PLATEAU_BAND: f64 = 0.004;
const PLATEAU_HOLD_S: f64 = 480.0;

/// At or above this SoC on AC, call it full.
const FULL_SOC: f64 = 0.98;

/// Sample spacing beyond which the machine was asleep; filters and pending
/// predictions cannot be carried across the gap.
const GAP_S: f64 = 1800.0;

const HISTORY_INTERVAL_MS: i64 = 30_000;
const HISTORY_SPAN_MS: i64 = 24 * 3600 * 1000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistPoint {
    pub t_ms: i64,
    /// Signed watts: positive = into the battery.
    pub watts: f32,
    pub soc: f32,
}

pub struct Estimator {
    pub model: Model,
    fast: Ewma,
    slow: Ewma,
    cap_slope: Slope,
    session: Stat,
    phase: Phase,
    last: Option<Sample>,
    /// Reference point for detecting a charge-limit plateau.
    plateau_ref: Option<(i64, u32)>,
    history: VecDeque<HistPoint>,
    last_hist_ms: i64,
    dirty: bool,
}

impl Estimator {
    pub fn new(mut model: Model) -> Self {
        model.sanitise();
        Estimator {
            model,
            fast: Ewma::new(30.0),
            slow: Ewma::new(600.0),
            cap_slope: Slope::new(900.0),
            session: Stat::default(),
            phase: Phase::Unknown,
            last: None,
            plateau_ref: None,
            history: VecDeque::new(),
            last_hist_ms: i64::MIN,
            dirty: false,
        }
    }

    pub fn history(&self) -> &VecDeque<HistPoint> {
        &self.history
    }

    pub fn load_history(&mut self, pts: Vec<HistPoint>) {
        self.history = pts.into_iter().collect();
        self.last_hist_ms = self.history.back().map(|p| p.t_ms).unwrap_or(i64::MIN);
    }

    /// True if the learned model changed since last checked, so the caller
    /// knows when a save is worth doing.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Full-charge capacity to measure against.
    ///
    /// Bounded below by the driver's own reported figure as well as the
    /// smoothed one, which lags it. It must NOT be bounded by the charge
    /// currently present: doing that makes `full` follow capacity downwards as
    /// the battery drains, freezing the reading at exactly 100%.
    fn full_mwh(&self, s: &Sample) -> f64 {
        let smoothed = if self.model.full_mwh.ready() && self.model.full_mwh.mean > 1000.0 {
            self.model.full_mwh.mean
        } else {
            0.0
        };
        let reported = if s.full_mwh > 0 && s.full_mwh != UNKNOWN_CAPACITY {
            s.full_mwh as f64
        } else {
            0.0
        };
        smoothed.max(reported).max(1.0)
    }

    fn classify(&mut self, s: &Sample, full: f64) -> Phase {
        if s.discharging() && !s.ac_online() {
            self.plateau_ref = None;
            return Phase::Discharging;
        }
        if !s.ac_online() {
            // Off AC and not flagged discharging: still running off the pack.
            self.plateau_ref = None;
            return Phase::Discharging;
        }
        if s.soc() >= FULL_SOC {
            self.plateau_ref = None;
            return Phase::Full;
        }
        // On AC below full: decide between charging and a held charge limit.
        match self.plateau_ref {
            None => {
                self.plateau_ref = Some((s.t_ms, s.capacity_mwh));
                Phase::Charging
            }
            Some((t0, c0)) => {
                let moved = (s.capacity_mwh as f64 - c0 as f64).abs();
                if moved > PLATEAU_BAND * full {
                    self.plateau_ref = Some((s.t_ms, s.capacity_mwh));
                    Phase::Charging
                } else if (s.t_ms - t0) as f64 / 1000.0 >= PLATEAU_HOLD_S {
                    Phase::Plateau
                } else {
                    Phase::Charging
                }
            }
        }
    }

    fn reset_filters(&mut self) {
        self.fast.reset();
        self.slow.reset();
        self.cap_slope.clear();
        self.session = Stat::default();
    }

    pub fn update(&mut self, s: Sample) -> Estimates {
        let gap_s = self
            .last
            .map(|p| (s.t_ms - p.t_ms) as f64 / 1000.0)
            .unwrap_or(0.0);
        let resumed = gap_s > GAP_S || gap_s < 0.0;
        if resumed {
            self.reset_filters();
            self.plateau_ref = None;
            // Outcomes cannot be attributed across a sleep.
            self.model.open.clear();
        }

        if s.full_mwh > 0 && s.full_mwh != UNKNOWN_CAPACITY {
            self.model.full_mwh.observe(s.full_mwh as f64, 0.5);
        }
        let full = self.full_mwh(&s);
        // A freshly topped-up pack can report marginally more charge than its
        // own stated capacity. Showing that would put the readout above 100%,
        // so the charge is capped for display and prediction alike.
        let capacity = (s.capacity_mwh as f64).min(full);

        let phase = self.classify(&s, full);
        if phase != self.phase {
            self.reset_filters();
            self.drop_incompatible_predictions(phase);
            self.phase = phase;
        }

        // ---- rate tracks -------------------------------------------------
        let reported_mw = if s.rate_known() { s.rate_mw as f64 } else { 0.0 };
        let magnitude = reported_mw.abs();
        if s.rate_known() && magnitude > 0.0 {
            self.fast.update(s.t_ms, magnitude);
            self.slow.update(s.t_ms, magnitude);
            self.session.observe(magnitude, 1.0);
        }
        self.cap_slope.push(s.t_ms, s.capacity_mwh as f64);
        // Capacity slope in mW, signed the same way as the reported rate.
        let slope_mw = self.cap_slope.slope_per_s().map(|v| v * 3600.0);

        self.update_calibration(phase, magnitude, slope_mw);
        let learn_dt = if resumed { 0.0 } else { gap_s };
        self.learn(&s, phase, full, slope_mw, learn_dt);

        // ---- display watts ----------------------------------------------
        let watts = if s.rate_known() && magnitude > 0.0 {
            let m = self.fast.get().unwrap_or(magnitude);
            let sign = if phase == Phase::Discharging { -1.0 } else { 1.0 };
            sign * m / 1000.0
        } else {
            slope_mw.map(|v| v / 1000.0).unwrap_or(0.0)
        };

        self.push_history(&s, watts);

        // ---- predictions --------------------------------------------------
        let mut est = Estimates {
            phase,
            // Derived from the same capacity figure shown beside it, so the
            // percentage and the watt-hours always agree.
            soc: (capacity / full).clamp(0.0, 1.0),
            capacity_mwh: capacity.round() as u32,
            full_mwh: full.round() as u32,
            watts,
            to_empty: None,
            to_80: None,
            to_full: None,
            note: None,
            confidence: 0.0,
        };

        match phase {
            Phase::Discharging => {
                let reserve = self.model.reserve_mwh(full);
                let raw = self.integrate_discharge(capacity, reserve, full);
                est.to_empty = raw.map(|r| self.finish(r, Kind::ToEmpty));
                est.confidence = self.confidence(Kind::ToEmpty);
            }
            Phase::Charging => {
                let cap = capacity;
                let target80 = 0.80 * full;
                if cap < target80 {
                    let raw = self.model.curve.time_to(cap, target80, full);
                    est.to_80 = Some(self.finish(raw, Kind::To80));
                }
                let raw_full = self.model.curve.time_to(cap, full, full);
                est.to_full = Some(self.finish(raw_full, Kind::ToFull));
                est.confidence = self.confidence(Kind::ToFull) * (0.5 + 0.5 * self.model.curve.maturity());
            }
            Phase::Plateau => {
                est.note = Some(format!("Held at {:.0}% - charge limit", s.soc() * 100.0));
                est.confidence = 1.0;
            }
            Phase::Full => {
                est.note = Some("Fully charged".into());
                est.confidence = 1.0;
            }
            Phase::Unknown => {}
        }

        self.score(&s, phase, full);
        self.last = Some(s);
        est
    }

    /// Reconcile the driver's reported rate against the capacity gauge's own
    /// slope. The reported figure is responsive but can be biased; the slope is
    /// slow but truthful. The ratio between them is a per-device constant.
    fn update_calibration(&mut self, phase: Phase, magnitude: f64, slope_mw: Option<f64>) {
        let (Some(slope), Some(slow)) = (slope_mw, self.slow.get()) else {
            return;
        };
        if magnitude < 1000.0 || slow < 1000.0 || slope.abs() < 1000.0 {
            return;
        }
        let ratio = slope.abs() / slow;
        if !(0.5..2.0).contains(&ratio) {
            return;
        }
        match phase {
            Phase::Discharging => self.model.calib_discharge.observe(ratio, 1.0),
            Phase::Charging => self.model.calib_charge.observe(ratio, 1.0),
            _ => {}
        }
        self.dirty = true;
    }

    fn calibration(&self, phase: Phase) -> f64 {
        let st = match phase {
            Phase::Charging => &self.model.calib_charge,
            _ => &self.model.calib_discharge,
        };
        if st.ready() {
            st.mean.clamp(0.8, 1.25)
        } else {
            1.0
        }
    }

    /// Fold this sample into the learned model.
    ///
    /// Observation weight is proportional to the time the sample represents,
    /// not a flat amount per sample, so the model learns at the same rate
    /// whether the driver is waking us every two seconds or every five.
    fn learn(&mut self, s: &Sample, phase: Phase, full: f64, slope_mw: Option<f64>, dt_s: f64) {
        if s.critical() && s.capacity_mwh > 0 {
            self.model.reserve.observe(s.capacity_mwh as f64, 1.0);
            self.dirty = true;
        }
        let w = (dt_s / 60.0).clamp(0.0, 1.0);
        if w <= 0.0 {
            return;
        }
        match phase {
            Phase::Discharging => {
                // Prefer the capacity-derived rate: it is what actually left
                // the pack, free of any reported-rate bias.
                let mw = match slope_mw {
                    Some(v) if v.abs() > MIN_POWER_MW => v.abs(),
                    _ => self.slow.get().unwrap_or(0.0),
                };
                if mw > MIN_POWER_MW {
                    let idx = Context::from_sample(s).index();
                    self.model.priors[idx].observe(mw, w);
                    self.dirty = true;
                }
            }
            Phase::Charging => {
                let mw = match slope_mw {
                    Some(v) if v > MIN_POWER_MW => v,
                    _ => self.slow.get().unwrap_or(0.0),
                };
                if mw > MIN_POWER_MW && full > 0.0 {
                    self.model.curve.observe(s.soc(), mw, w);
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    /// Expected draw in mW at horizon `h` seconds out: a smooth handoff from
    /// what the machine is doing now, through the last hour, to how it normally
    /// behaves in this context.
    fn power_at(&self, h: f64, now: f64, recent: f64, prior: f64) -> f64 {
        let w_now = (-h / TAU_NOW_S).exp();
        let w_recent = (1.0 - w_now) * (-h / TAU_RECENT_S).exp();
        let w_prior = (1.0 - w_now - w_recent).max(0.0);
        (w_now * now + w_recent * recent + w_prior * prior).max(MIN_POWER_MW)
    }

    fn discharge_powers(&self) -> Option<(f64, f64, f64)> {
        let k = self.calibration(Phase::Discharging);
        let now = self.fast.get()? * k;
        // For the medium horizon, prefer the capacity gauge's own slope: it is
        // the energy that actually left the pack. Measured on this machine, the
        // driver's reported rate overstated true flow by ~50% in the minutes
        // after unplugging, so a ratio-based correction converges too slowly to
        // help. The slope needs no correction because it *is* the outcome.
        let slope_mw = self
            .cap_slope
            .slope_per_s()
            .map(|v| (v * 3600.0).abs())
            .filter(|v| *v > MIN_POWER_MW && self.cap_slope.span_s() >= 300.0);
        let recent = slope_mw.unwrap_or_else(|| self.slow.get().unwrap_or(now) * k);
        let ctx_prior = self
            .last
            .map(|s| self.model.priors[Context::from_sample(&s).index()])
            .filter(|p| p.ready())
            .map(|p| p.mean);
        let prior = ctx_prior
            .or_else(|| self.session.ready().then_some(self.session.mean))
            .unwrap_or(recent);
        Some((now, recent, prior))
    }

    /// Seconds for capacity to fall from `from_mwh` to `target_mwh`, integrating
    /// a draw that varies with horizon. A single division cannot express that.
    fn integrate_discharge(&self, from_mwh: f64, target_mwh: f64, _full: f64) -> Option<f64> {
        let (now, recent, prior) = self.discharge_powers()?;
        if from_mwh <= target_mwh {
            return Some(0.0);
        }
        let mut e = from_mwh - target_mwh;
        let mut t = 0.0;
        while e > 0.0 && t < MAX_HORIZON_S {
            let p = self.power_at(t, now, recent, prior);
            e -= p * INTEGRATION_STEP_S / 3600.0;
            t += INTEGRATION_STEP_S;
        }
        Some(t.min(MAX_HORIZON_S))
    }

    /// Predicted seconds for capacity to fall to `target_mwh`, uncorrected.
    ///
    /// Exposed for offline validation against recorded traces: it is the same
    /// integration the displayed estimate uses, against a target whose outcome
    /// is actually observable within a recording.
    pub fn predict_to_capacity(&self, target_mwh: f64) -> Option<f64> {
        let s = self.last?;
        let full = self.full_mwh(&s);
        match self.phase {
            Phase::Discharging => self.integrate_discharge(s.capacity_mwh as f64, target_mwh, full),
            Phase::Charging => Some(self.model.curve.time_to(s.capacity_mwh as f64, target_mwh, full)),
            _ => None,
        }
    }

    /// Apply the learned correction and attach an uncertainty band.
    fn finish(&self, raw_s: f64, kind: Kind) -> Prediction {
        let c = self.model.correction(kind);
        let secs = (raw_s * c.factor()).clamp(0.0, MAX_HORIZON_S);
        let e = c.rel_err();
        Prediction { secs, lo: secs * (1.0 - e), hi: secs * (1.0 + e) }
    }

    fn confidence(&self, kind: Kind) -> f64 {
        (1.0 - self.model.correction(kind).rel_err() / 0.5).clamp(0.0, 1.0)
    }

    fn push_history(&mut self, s: &Sample, watts: f64) {
        if s.t_ms.saturating_sub(self.last_hist_ms) < HISTORY_INTERVAL_MS {
            return;
        }
        self.last_hist_ms = s.t_ms;
        self.history.push_back(HistPoint {
            t_ms: s.t_ms,
            watts: watts as f32,
            soc: s.soc() as f32,
        });
        let cutoff = s.t_ms - HISTORY_SPAN_MS;
        while self.history.front().is_some_and(|p| p.t_ms < cutoff) {
            self.history.pop_front();
        }
    }

    // ---- self-evaluation -------------------------------------------------

    fn drop_incompatible_predictions(&mut self, phase: Phase) {
        self.model.open.retain(|p| match p.kind {
            Kind::ToEmpty => phase == Phase::Discharging,
            Kind::To80 | Kind::ToFull => phase == Phase::Charging,
        });
    }

    /// Close any prediction whose target has been reached, then open a fresh
    /// checkpoint if none is pending.
    ///
    /// Checkpoints are deliberately near-term (a 15% SoC move) rather than the
    /// full milestone: they exercise exactly the same integration, but they
    /// actually resolve many times per session instead of once, and time-to-
    /// empty would otherwise almost never resolve at all -- the machine dies
    /// before the outcome can be recorded.
    fn score(&mut self, s: &Sample, phase: Phase, full: f64) {
        let cap = s.capacity_mwh as f64;
        let mut closed = Vec::new();
        self.model.open.retain(|p| {
            let hit = if p.rising { cap >= p.target_mwh } else { cap <= p.target_mwh };
            if hit {
                closed.push((p.kind, (s.t_ms - p.made_at_ms) as f64 / 1000.0, p.predicted_s));
                false
            } else {
                true
            }
        });
        for (kind, actual, predicted) in closed {
            self.model.correction_mut(kind).observe(actual, predicted);
            self.dirty = true;
        }

        let (kind, target, rising) = match phase {
            Phase::Discharging => {
                let reserve = self.model.reserve_mwh(full);
                let t = (cap - 0.15 * full).max(reserve);
                if t >= cap {
                    return;
                }
                (Kind::ToEmpty, t, false)
            }
            Phase::Charging => {
                let t80 = 0.80 * full;
                if cap < t80 {
                    (Kind::To80, (cap + 0.12 * full).min(t80), true)
                } else {
                    (Kind::ToFull, (cap + 0.06 * full).min(full), true)
                }
            }
            _ => return,
        };
        if target <= cap && rising {
            return;
        }
        if self.model.open.iter().any(|p| p.kind == kind) {
            return;
        }
        let raw = match phase {
            Phase::Discharging => self.integrate_discharge(cap, target, full),
            Phase::Charging => Some(self.model.curve.time_to(cap, target, full)),
            _ => None,
        };
        // Grade the *uncorrected* prediction, so the correction measures the
        // model's own error rather than compounding with itself.
        if let Some(raw) = raw {
            if (120.0..MAX_HORIZON_S).contains(&raw) {
                self.model.open.push(OpenPrediction {
                    kind,
                    made_at_ms: s.t_ms,
                    predicted_s: raw,
                    target_mwh: target,
                    rising,
                });
            }
        }
    }
}
