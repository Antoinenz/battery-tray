use crate::curve::ChargeCurve;
use crate::filter::Stat;
use crate::types::CONTEXT_COUNT;
use serde::{Deserialize, Serialize};

pub const MODEL_VERSION: u32 = 2;

/// Which prediction a correction applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    ToEmpty = 0,
    To80 = 1,
    ToFull = 2,
}
pub const KIND_COUNT: usize = 3;

/// Rolling record of `actual / predicted` for one prediction kind.
///
/// This is how the app grades itself. Predictions are logged when made and
/// closed when the target is actually reached, so the ratio measures real
/// end-to-end accuracy rather than internal consistency. Its mean becomes a
/// correction factor; its spread becomes the displayed uncertainty band.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Correction {
    pub ratio: Stat,
}

impl Correction {
    /// Multiplier applied to raw predictions. Clamped so a pathological run of
    /// outcomes cannot drive estimates to nonsense.
    pub fn factor(&self) -> f64 {
        if self.ratio.ready() && self.ratio.mean.is_finite() {
            self.ratio.mean.clamp(0.6, 1.6)
        } else {
            1.0
        }
    }
    /// Relative uncertainty, from observed spread. Falls back to a deliberately
    /// wide band before enough outcomes have been seen to know better.
    pub fn rel_err(&self) -> f64 {
        if self.ratio.ready() {
            self.ratio.sd().clamp(0.06, 0.5)
        } else {
            0.30
        }
    }
    pub fn observe(&mut self, actual_s: f64, predicted_s: f64) {
        if predicted_s <= 30.0 || !actual_s.is_finite() || actual_s <= 0.0 {
            return;
        }
        let r = actual_s / predicted_s;
        // Ratios this extreme mean the situation changed (workload shifted,
        // charger swapped), not that the model is wrong by that much.
        if (0.2..5.0).contains(&r) {
            self.ratio.observe(r, 1.0);
        }
    }
}

/// A prediction awaiting its outcome.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct OpenPrediction {
    pub kind: Kind,
    pub made_at_ms: i64,
    pub predicted_s: f64,
    pub target_mwh: f64,
    /// True when the target is reached by capacity rising (charging).
    pub rising: bool,
}

/// What the app has learned so far, in terms a person can read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Training {
    pub seeded: bool,
    /// Fraction of the charge curve resting on observation rather than seed.
    pub curve_maturity: f64,
    pub charger_peak_w: Option<f64>,
    pub contexts_learned: usize,
    pub context_total: usize,
    pub active_draw_w: Option<f64>,
    pub idle_draw_w: Option<f64>,
    pub reserve_wh: Option<f64>,
    /// How many predictions have been graded against their outcome.
    pub graded_predictions: f64,
    /// Typical relative error, once enough outcomes have been seen.
    pub typical_error: Option<f64>,
}

/// Everything the app learns and keeps across restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub version: u32,
    pub curve: ChargeCurve,
    /// Discharge power prior per context bucket, in mW.
    pub priors: Vec<Stat>,
    /// Capacity in mWh at which this machine actually stops. Learned, because
    /// the usable floor is not zero and differs per device.
    pub reserve: Stat,
    /// Smoothed full-charge capacity. The OS figure drifts day to day
    /// (38,323 -> 39,655 mWh observed), so it must not be taken as constant.
    pub full_mwh: Stat,
    /// Reported-rate vs capacity-slope calibration, per phase.
    pub calib_discharge: Stat,
    pub calib_charge: Stat,
    pub corrections: Vec<Correction>,
    pub open: Vec<OpenPrediction>,
    pub seeded: bool,
    pub seeded_at_ms: i64,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            version: MODEL_VERSION,
            curve: ChargeCurve::default(),
            priors: vec![Stat::default(); CONTEXT_COUNT],
            reserve: Stat::default(),
            full_mwh: Stat::default(),
            calib_discharge: Stat::default(),
            calib_charge: Stat::default(),
            corrections: vec![Correction::default(); KIND_COUNT],
            open: Vec::new(),
            seeded: false,
            seeded_at_ms: 0,
        }
    }
}

impl Model {
    /// Repair a model loaded from disk that was written by another version or
    /// hand-edited, so a bad file degrades to defaults instead of panicking.
    pub fn sanitise(&mut self) {
        if self.priors.len() != CONTEXT_COUNT {
            self.priors = vec![Stat::default(); CONTEXT_COUNT];
        }
        if self.corrections.len() != KIND_COUNT {
            self.corrections = vec![Correction::default(); KIND_COUNT];
        }
        for s in self.curve.shape.iter_mut() {
            if !s.is_finite() || *s < 0.0 {
                *s = 0.0;
            }
        }
        self.open.retain(|p| p.predicted_s.is_finite() && p.target_mwh.is_finite());
        self.version = MODEL_VERSION;
    }

    /// Usable energy floor in mWh: below this the machine stops, so it is not
    /// part of the runtime budget.
    pub fn reserve_mwh(&self, full_mwh: f64) -> f64 {
        let default = 0.05 * full_mwh;
        let v = if self.reserve.ready() { self.reserve.mean } else { default };
        v.clamp(0.0, 0.15 * full_mwh)
    }

    /// A readable account of what the app has actually learned, for the
    /// settings window. Everything here is derived, never stored separately,
    /// so it cannot drift from the model it describes.
    pub fn training(&self) -> Training {
        let active_mid = self.priors[crate::types::Context {
            activity: crate::types::Activity::Active,
            load_band: 1,
        }
        .index()];
        let idle_mid = self.priors[crate::types::Context {
            activity: crate::types::Activity::Standby,
            load_band: 1,
        }
        .index()];
        let graded: f64 = self.corrections.iter().map(|c| c.ratio.n).sum();
        Training {
            seeded: self.seeded,
            curve_maturity: self.curve.maturity(),
            charger_peak_w: self.curve.peak_ready().then(|| self.curve.peak_mw() / 1000.0),
            contexts_learned: self.priors.iter().filter(|p| p.ready()).count(),
            context_total: self.priors.len(),
            active_draw_w: active_mid.ready().then(|| active_mid.mean / 1000.0),
            idle_draw_w: idle_mid.ready().then(|| idle_mid.mean / 1000.0),
            reserve_wh: self.reserve.ready().then(|| self.reserve.mean / 1000.0),
            graded_predictions: graded,
            // Only meaningful once some individual prediction kind has enough
            // resolved outcomes; a total spread thinly across three kinds
            // would otherwise report a confident-looking zero.
            typical_error: {
                let ready: Vec<f64> = self
                    .corrections
                    .iter()
                    .filter(|c| c.ratio.ready())
                    .map(|c| c.rel_err())
                    .collect();
                (!ready.is_empty()).then(|| ready.iter().sum::<f64>() / ready.len() as f64)
            },
        }
    }

    pub fn correction(&self, kind: Kind) -> &Correction {
        &self.corrections[kind as usize]
    }
    pub fn correction_mut(&mut self, kind: Kind) -> &mut Correction {
        &mut self.corrections[kind as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_starts_neutral_and_wide() {
        let c = Correction::default();
        assert_eq!(c.factor(), 1.0, "no data means no correction");
        assert!(c.rel_err() > 0.2, "unknown accuracy should read as uncertain");
    }

    #[test]
    fn correction_learns_a_systematic_bias() {
        let mut c = Correction::default();
        // The app has been consistently predicting 20% short.
        for _ in 0..30 {
            c.observe(1200.0, 1000.0);
        }
        assert!((c.factor() - 1.2).abs() < 0.05, "factor {}", c.factor());
        assert!(c.rel_err() < 0.2, "consistent outcomes should tighten the band");
    }

    #[test]
    fn correction_is_clamped_against_runaway() {
        let mut c = Correction::default();
        for _ in 0..50 {
            c.observe(4000.0, 1000.0);
        }
        assert!(c.factor() <= 1.6);
    }

    #[test]
    fn correction_rejects_outliers_and_degenerate_input() {
        let mut c = Correction::default();
        c.observe(100_000.0, 1000.0); // situation changed, not model error
        c.observe(1000.0, 5.0); // prediction too short to grade
        c.observe(f64::NAN, 1000.0);
        assert_eq!(c.ratio.n, 0.0);
    }

    #[test]
    fn reserve_defaults_to_five_percent_then_learns() {
        let mut m = Model::default();
        assert!((m.reserve_mwh(40_000.0) - 2_000.0).abs() < 1.0);
        for _ in 0..10 {
            m.reserve.observe(1_200.0, 1.0);
        }
        assert!((m.reserve_mwh(40_000.0) - 1_200.0).abs() < 50.0);
    }

    #[test]
    fn reserve_is_clamped_to_a_sane_fraction() {
        let mut m = Model::default();
        for _ in 0..10 {
            m.reserve.observe(30_000.0, 1.0); // absurd
        }
        assert!(m.reserve_mwh(40_000.0) <= 6_000.0);
    }

    #[test]
    fn sanitise_repairs_a_corrupt_model() {
        let mut m = Model::default();
        m.priors.clear();
        m.corrections.clear();
        m.curve.shape[3] = f64::NAN;
        m.sanitise();
        assert_eq!(m.priors.len(), CONTEXT_COUNT);
        assert_eq!(m.corrections.len(), KIND_COUNT);
        assert!(m.curve.shape[3].is_finite());
    }


    #[test]
    fn a_fresh_model_reports_that_it_has_learned_nothing() {
        let t = Model::default().training();
        assert!(!t.seeded);
        assert_eq!(t.curve_maturity, 0.0);
        assert_eq!(t.charger_peak_w, None, "the seed peak is not a measurement");
        assert_eq!(t.contexts_learned, 0);
        assert_eq!(t.graded_predictions, 0.0);
        assert_eq!(t.typical_error, None);
    }

    #[test]
    fn training_reflects_what_the_model_actually_holds() {
        let mut m = Model::default();
        m.seeded = true;
        for _ in 0..30 {
            m.curve.observe(0.30, 27_400.0, 2.0);
        }
        let idx = crate::types::Context {
            activity: crate::types::Activity::Active,
            load_band: 1,
        }
        .index();
        for _ in 0..10 {
            m.priors[idx].observe(16_300.0, 1.0);
        }
        for _ in 0..10 {
            m.correction_mut(Kind::ToEmpty).observe(1000.0, 1000.0);
        }

        let t = m.training();
        assert!(t.seeded);
        assert!(t.curve_maturity > 0.0);
        assert!((t.charger_peak_w.unwrap() - 27.4).abs() < 2.0, "{:?}", t.charger_peak_w);
        assert_eq!(t.contexts_learned, 1);
        assert!((t.active_draw_w.unwrap() - 16.3).abs() < 0.5);
        assert!(t.graded_predictions >= 10.0);
        assert!(t.typical_error.is_some());
    }

    #[test]
    fn model_round_trips_through_json() {
        let mut m = Model::default();
        m.curve.observe(0.3, 30_000.0, 5.0);
        m.reserve.observe(1500.0, 1.0);
        let s = serde_json::to_string(&m).unwrap();
        let back: Model = serde_json::from_str(&s).unwrap();
        assert_eq!(back.version, MODEL_VERSION);
        assert!((back.reserve.mean - m.reserve.mean).abs() < 1e-9);
        assert_eq!(back.curve.shape, m.curve.shape);
    }
}
