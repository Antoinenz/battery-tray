//! Deciding when to interrupt the user.
//!
//! Kept here, away from the platform layer, because the hard part is not
//! showing a notification but not showing it twice. Each alert latches when it
//! fires and only rearms once the battery has clearly moved away from the
//! condition, so a level hovering on a threshold cannot produce a stream of
//! them.

use crate::settings::Settings;
use crate::types::{Estimates, Phase};

/// How far the charge must recover past a threshold before that alert can fire
/// again. Without it, a battery resting at exactly the warning level would
/// alert on every sample that crossed back and forth.
const REARM_MARGIN: f64 = 0.03;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    Low(u8),
    Critical(u8),
    Reached80,
    Full,
}

impl Alert {
    pub fn title(&self) -> String {
        match self {
            Alert::Low(_) => "Battery low".into(),
            Alert::Critical(_) => "Battery critically low".into(),
            Alert::Reached80 => "Charged to 80%".into(),
            Alert::Full => "Battery full".into(),
        }
    }

    /// The body text. Time remaining is filled in by the caller where known,
    /// since that is the part people actually act on.
    pub fn body(&self, remaining: Option<&str>) -> String {
        match self {
            Alert::Low(pct) | Alert::Critical(pct) => match remaining {
                Some(t) => format!("{pct}% left, about {t} remaining."),
                None => format!("{pct}% left."),
            },
            Alert::Reached80 => {
                "Unplugging here is easier on the battery; the last 20% charges slowly.".into()
            }
            Alert::Full => "Fully charged.".into(),
        }
    }
}

/// Latches for each alert, so none of them repeats while the condition holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Alerts {
    low_fired: bool,
    critical_fired: bool,
    eighty_fired: bool,
    full_fired: bool,
}

impl Alerts {
    /// Consider the latest reading, returning an alert if one is due.
    ///
    /// At most one is returned per call, most urgent first: a battery that
    /// falls straight past both thresholds should say "critically low", not
    /// both messages at once.
    pub fn evaluate(&mut self, est: &Estimates, s: &Settings) -> Option<Alert> {
        let pct = est.soc * 100.0;

        match est.phase {
            Phase::Discharging => {
                // Charge milestones rearm as soon as the battery is in use.
                self.eighty_fired = false;
                self.full_fired = false;

                let crit = s.alert_critical_pct as f64;
                let low = s.alert_low_pct as f64;
                if pct > crit + REARM_MARGIN * 100.0 {
                    self.critical_fired = false;
                }
                if pct > low + REARM_MARGIN * 100.0 {
                    self.low_fired = false;
                }

                if s.alert_critical && pct <= crit && !self.critical_fired {
                    self.critical_fired = true;
                    // A battery below the critical mark is also below the low
                    // mark; latch both so the milder one cannot follow.
                    self.low_fired = true;
                    return Some(Alert::Critical(s.alert_critical_pct));
                }
                if s.alert_low && pct <= low && !self.low_fired {
                    self.low_fired = true;
                    return Some(Alert::Low(s.alert_low_pct));
                }
                None
            }
            Phase::Charging | Phase::Plateau | Phase::Full => {
                // Drain warnings rearm once the battery is being replenished.
                self.low_fired = false;
                self.critical_fired = false;

                if pct < 75.0 {
                    self.eighty_fired = false;
                }
                if est.phase != Phase::Full {
                    self.full_fired = false;
                }

                if s.alert_full && est.phase == Phase::Full && !self.full_fired {
                    self.full_fired = true;
                    self.eighty_fired = true;
                    return Some(Alert::Full);
                }
                if s.alert_80 && pct >= 80.0 && !self.eighty_fired {
                    self.eighty_fired = true;
                    return Some(Alert::Reached80);
                }
                None
            }
            Phase::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Prediction;

    fn est(phase: Phase, soc: f64) -> Estimates {
        Estimates {
            phase,
            soc,
            capacity_mwh: (soc * 38_820.0) as u32,
            full_mwh: 38_820,
            watts: if phase == Phase::Discharging { -16.5 } else { 20.0 },
            to_empty: (phase == Phase::Discharging)
                .then_some(Prediction { secs: 3600.0, lo: 3000.0, hi: 4200.0 }),
            to_80: None,
            to_full: None,
            note: None,
            confidence: 0.5,
        }
    }

    #[test]
    fn a_low_battery_warns_once() {
        let s = Settings::default();
        let mut a = Alerts::default();
        assert_eq!(a.evaluate(&est(Phase::Discharging, 0.30), &s), None);
        assert_eq!(
            a.evaluate(&est(Phase::Discharging, 0.20), &s),
            Some(Alert::Low(20))
        );
        // Still low, but already said so.
        for soc in [0.19, 0.18, 0.17, 0.15] {
            assert_eq!(a.evaluate(&est(Phase::Discharging, soc), &s), None);
        }
    }

    #[test]
    fn critical_supersedes_low_rather_than_doubling_up() {
        let s = Settings::default();
        let mut a = Alerts::default();
        // Straight from healthy to critical in one step.
        assert_eq!(
            a.evaluate(&est(Phase::Discharging, 0.08), &s),
            Some(Alert::Critical(10))
        );
        assert_eq!(a.evaluate(&est(Phase::Discharging, 0.07), &s), None);
    }

    #[test]
    fn hovering_on_the_threshold_does_not_repeat() {
        let s = Settings::default();
        let mut a = Alerts::default();
        assert!(a.evaluate(&est(Phase::Discharging, 0.20), &s).is_some());
        // Drifting either side of 20% must stay quiet until it recovers well past.
        for soc in [0.201, 0.199, 0.202, 0.198, 0.21, 0.22] {
            assert_eq!(a.evaluate(&est(Phase::Discharging, soc), &s), None, "at {soc}");
        }
        assert_eq!(a.evaluate(&est(Phase::Discharging, 0.24), &s), None);
        // Genuinely recovered, then drained again: worth saying once more.
        assert_eq!(a.evaluate(&est(Phase::Charging, 0.50), &s), None);
        assert_eq!(
            a.evaluate(&est(Phase::Discharging, 0.20), &s),
            Some(Alert::Low(20))
        );
    }

    #[test]
    fn charging_rearms_the_drain_warnings() {
        let s = Settings::default();
        let mut a = Alerts::default();
        assert!(a.evaluate(&est(Phase::Discharging, 0.09), &s).is_some());
        assert_eq!(a.evaluate(&est(Phase::Charging, 0.09), &s), None);
        assert_eq!(
            a.evaluate(&est(Phase::Discharging, 0.09), &s),
            Some(Alert::Critical(10)),
            "after a charge it is worth warning again"
        );
    }

    #[test]
    fn charge_milestones_are_off_unless_asked_for() {
        let s = Settings::default();
        let mut a = Alerts::default();
        assert_eq!(a.evaluate(&est(Phase::Charging, 0.85), &s), None);
        assert_eq!(a.evaluate(&est(Phase::Full, 1.0), &s), None);
    }

    #[test]
    fn the_eighty_percent_milestone_fires_once_per_charge() {
        let s = Settings { alert_80: true, ..Settings::default() };
        let mut a = Alerts::default();
        assert_eq!(a.evaluate(&est(Phase::Charging, 0.70), &s), None);
        assert_eq!(
            a.evaluate(&est(Phase::Charging, 0.81), &s),
            Some(Alert::Reached80)
        );
        assert_eq!(a.evaluate(&est(Phase::Charging, 0.90), &s), None);
        // Down for a while, then charged again.
        assert_eq!(a.evaluate(&est(Phase::Discharging, 0.60), &s), None);
        assert_eq!(
            a.evaluate(&est(Phase::Charging, 0.82), &s),
            Some(Alert::Reached80)
        );
    }

    #[test]
    fn a_full_battery_does_not_also_announce_eighty() {
        let s = Settings { alert_80: true, alert_full: true, ..Settings::default() };
        let mut a = Alerts::default();
        assert_eq!(a.evaluate(&est(Phase::Full, 1.0), &s), Some(Alert::Full));
        assert_eq!(a.evaluate(&est(Phase::Full, 1.0), &s), None);
        assert_eq!(a.evaluate(&est(Phase::Plateau, 0.99), &s), None);
    }

    #[test]
    fn disabled_alerts_stay_silent() {
        let s = Settings {
            alert_low: false,
            alert_critical: false,
            ..Settings::default()
        };
        let mut a = Alerts::default();
        for soc in [0.20, 0.10, 0.03] {
            assert_eq!(a.evaluate(&est(Phase::Discharging, soc), &s), None);
        }
    }

    #[test]
    fn messages_name_the_level_and_the_time_left() {
        let a = Alert::Low(20);
        assert_eq!(a.title(), "Battery low");
        assert!(a.body(Some("1h 12m")).contains("20%"));
        assert!(a.body(Some("1h 12m")).contains("1h 12m"));
        assert!(!a.body(None).contains("about"));
        assert!(Alert::Reached80.body(None).contains("20%"));
    }
}
