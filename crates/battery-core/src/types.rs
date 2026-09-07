use serde::{Deserialize, Serialize};

/// Documented `BATTERY_STATUS.PowerState` flags. Real drivers set vendor bits
/// beyond these (this Surface reports 0x62), so always mask before comparing.
pub const BATTERY_POWER_ON_LINE: u32 = 0x0000_0001;
pub const BATTERY_DISCHARGING: u32 = 0x0000_0002;
pub const BATTERY_CHARGING: u32 = 0x0000_0004;
pub const BATTERY_CRITICAL: u32 = 0x0000_0008;
pub const POWER_STATE_MASK: u32 =
    BATTERY_POWER_ON_LINE | BATTERY_DISCHARGING | BATTERY_CHARGING | BATTERY_CRITICAL;

/// `BATTERY_UNKNOWN_RATE`, returned when the driver cannot measure current.
pub const UNKNOWN_RATE: i32 = i32::MIN;
pub const UNKNOWN_CAPACITY: u32 = 0xFFFF_FFFF;

/// One reading from the battery driver plus the system context at that moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub t_ms: i64,
    pub capacity_mwh: u32,
    pub full_mwh: u32,
    /// Signed, as the driver reports it: positive = into the battery.
    pub rate_mw: i32,
    pub voltage_mv: u32,
    pub power_state: u32,
    pub cpu_pct: f32,
    pub display_on: bool,
}

impl Sample {
    pub fn ac_online(&self) -> bool {
        self.power_state & BATTERY_POWER_ON_LINE != 0
    }
    pub fn charging(&self) -> bool {
        self.power_state & BATTERY_CHARGING != 0
    }
    pub fn discharging(&self) -> bool {
        self.power_state & BATTERY_DISCHARGING != 0
    }
    pub fn critical(&self) -> bool {
        self.power_state & BATTERY_CRITICAL != 0
    }
    pub fn rate_known(&self) -> bool {
        self.rate_mw != UNKNOWN_RATE
    }
    pub fn soc(&self) -> f64 {
        if self.full_mwh == 0 {
            return 0.0;
        }
        (self.capacity_mwh as f64 / self.full_mwh as f64).clamp(0.0, 1.0)
    }
}

/// What the battery is doing. `Plateau` means on AC but capacity is pinned --
/// a vendor charge limit (Surface Battery Limit, Windows Smart Charging).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Discharging,
    Charging,
    Full,
    Plateau,
    Unknown,
}

/// Coarse activity class. The battery report shows a 7x power difference
/// between these, making it the highest-value context feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Active,
    Standby,
}

/// Context bucket for the discharge prior: activity x CPU load band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub activity: Activity,
    pub load_band: u8,
}

pub const CONTEXT_COUNT: usize = 6;

impl Context {
    pub fn from_sample(s: &Sample) -> Self {
        let activity = if s.display_on {
            Activity::Active
        } else {
            Activity::Standby
        };
        let load_band = match s.cpu_pct {
            p if p < 15.0 => 0,
            p if p < 50.0 => 1,
            _ => 2,
        };
        Context { activity, load_band }
    }
    pub fn index(&self) -> usize {
        let a = match self.activity {
            Activity::Active => 0,
            Activity::Standby => 1,
        };
        a * 3 + self.load_band as usize
    }
}

/// A time prediction with an uncertainty band, all in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prediction {
    pub secs: f64,
    pub lo: f64,
    pub hi: f64,
}

/// Everything needed to show a charge level that moves smoothly.
///
/// The gauge only reports whole 10 mWh steps -- about 0.026% on a 39 Wh
/// pack -- so a two-decimal readout taken straight from it lurches. These
/// fields let the display dead-reckon between steps from the measured
/// power flow, which is exactly the quantity that says how fast the true
/// value is moving.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SocTrack {
    /// Charge level at the moment the gauge last changed.
    pub base: f64,
    pub as_of_ms: i64,
    /// Signed change in charge level per millisecond.
    pub per_ms: f64,
    /// One gauge step, in charge-level units. Drift is never allowed to
    /// exceed this: past it the reading itself would have moved.
    pub quantum: f64,
}

impl SocTrack {
    pub fn at(&self, now_ms: i64) -> f64 {
        let dt = (now_ms - self.as_of_ms).max(0) as f64;
        let drift = (self.per_ms * dt).clamp(-self.quantum, self.quantum);
        (self.base + drift).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Estimates {
    pub phase: Phase,
    pub soc: f64,
    pub capacity_mwh: u32,
    pub full_mwh: u32,
    /// Signed display watts: positive = charging.
    pub watts: f64,
    pub to_empty: Option<Prediction>,
    pub to_80: Option<Prediction>,
    pub to_full: Option<Prediction>,
    /// Set when an ETA would be misleading, e.g. "Held at 50% - charge limit".
    pub note: Option<String>,
    /// 0..1, from the app's own rolling prediction error.
    pub confidence: f64,
    /// Lets the caller interpolate the charge level between gauge steps.
    pub soc_track: SocTrack,
}

impl Estimates {
    /// The one prediction worth showing right now.
    ///
    /// While charging that is time-to-80% until 80% is reached and time-to-full
    /// afterwards, never both: charging slows sharply past 80%, so the two
    /// numbers describe quite different things and showing them together
    /// invites reading the wrong one.
    pub fn active(&self) -> Option<Prediction> {
        match self.phase {
            Phase::Discharging => self.to_empty,
            Phase::Charging => self.to_80.or(self.to_full),
            _ => None,
        }
    }

    /// Label for [`Estimates::active`].
    pub fn active_label(&self) -> &'static str {
        match self.phase {
            Phase::Discharging => "Time remaining",
            Phase::Charging if self.to_80.is_some() => "Until 80%",
            Phase::Charging => "Until full",
            _ => "Status",
        }
    }
}

pub fn fmt_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--".into();
    }
    let total = secs.round() as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    if h >= 24 {
        let d = h / 24;
        return format!("{}d {}h", d, h % 24);
    }
    if h > 0 {
        format!("{}h {:02}m", h, m)
    } else {
        format!("{}m", m.max(1))
    }
}
