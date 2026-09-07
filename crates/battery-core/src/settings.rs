//! User-facing preferences.
//!
//! Every field carries `#[serde(default)]` so a settings file written by an
//! older build keeps working when new options are added -- unlike the learned
//! model, stale settings are harmless and should not be discarded.

use serde::{Deserialize, Serialize};

/// What the tray icon depicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TrayMode {
    /// A battery glyph whose fill tracks the charge level.
    #[default]
    Battery,
    /// The charge level as a number.
    Percent,
    /// Watts currently flowing.
    Watts,
    /// Time to empty, or to the active charging milestone.
    Time,
    /// The app mark, unchanging.
    Logo,
}

impl TrayMode {
    pub const ALL: [TrayMode; 5] = [
        TrayMode::Battery,
        TrayMode::Percent,
        TrayMode::Watts,
        TrayMode::Time,
        TrayMode::Logo,
    ];
    pub fn label(self) -> &'static str {
        match self {
            TrayMode::Battery => "Battery preview",
            TrayMode::Percent => "Percentage",
            TrayMode::Watts => "Wattage",
            TrayMode::Time => "Time remaining",
            TrayMode::Logo => "Logo",
        }
    }
    pub fn is_text(self) -> bool {
        matches!(self, TrayMode::Percent | TrayMode::Watts | TrayMode::Time)
    }
}

/// What the panel graph plots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GraphKind {
    /// Charge level over 24 hours, resting on the bottom of the plot.
    Level,
    /// Power in and out over the last hour, centred on zero.
    #[default]
    Throughput,
}

impl GraphKind {
    pub const ALL: [GraphKind; 2] = [GraphKind::Level, GraphKind::Throughput];
    pub fn label(self) -> &'static str {
        match self {
            GraphKind::Level => "Battery level",
            GraphKind::Throughput => "Throughput",
        }
    }
    pub fn span_ms(self) -> i64 {
        match self {
            GraphKind::Level => 24 * 60 * 60 * 1000,
            GraphKind::Throughput => 60 * 60 * 1000,
        }
    }
    pub fn toggled(self) -> Self {
        match self {
            GraphKind::Level => GraphKind::Throughput,
            GraphKind::Throughput => GraphKind::Level,
        }
    }
}

/// Which palette the popup panel uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PanelTheme {
    #[default]
    System,
    Dark,
    Light,
}

impl PanelTheme {
    pub const ALL: [PanelTheme; 3] = [PanelTheme::System, PanelTheme::Dark, PanelTheme::Light];
    pub fn label(self) -> &'static str {
        match self {
            PanelTheme::System => "Follow system",
            PanelTheme::Dark => "Always dark",
            PanelTheme::Light => "Always light",
        }
    }
    /// Resolve to a concrete choice given what the system is currently set to.
    pub fn is_light(self, system_is_light: bool) -> bool {
        match self {
            PanelTheme::System => system_is_light,
            PanelTheme::Dark => false,
            PanelTheme::Light => true,
        }
    }
}

/// Levels offered for the low-battery alerts.
pub const ALERT_LEVELS: [u8; 7] = [5, 10, 15, 20, 25, 30, 40];

fn default_low() -> u8 {
    20
}
fn default_critical() -> u8 {
    10
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub tray_mode: TrayMode,
    #[serde(default)]
    pub graph: GraphKind,
    /// Show decimal places on the charge level, so slow movement is visible.
    /// Defaults on: the gauge moves in ~0.03% steps, which is invisible when
    /// rounded to whole percent.
    #[serde(default = "yes")]
    pub decimals: bool,
    #[serde(default)]
    pub theme: PanelTheme,
    /// Draw the graph at all.
    #[serde(default = "yes")]
    pub show_graph: bool,
    /// Draw the zero line across a throughput graph.
    #[serde(default = "yes")]
    pub graph_zero_line: bool,
    /// When every reading in the window flows the same way, give the
    /// whole plot to that direction instead of holding half of it empty
    /// for a sign that is not present.
    #[serde(default = "yes")]
    pub graph_autofit: bool,
    /// Keep the panel on screen until dismissed, instead of closing it as soon
    /// as it loses focus.
    #[serde(default)]
    pub pin_panel: bool,
    /// Where the user dragged the pinned panel to, if they have.
    #[serde(default)]
    pub panel_pos: Option<(i32, i32)>,

    #[serde(default = "yes")]
    pub alert_low: bool,
    #[serde(default = "default_low")]
    pub alert_low_pct: u8,
    #[serde(default = "yes")]
    pub alert_critical: bool,
    #[serde(default = "default_critical")]
    pub alert_critical_pct: u8,
    /// Off by default: a notification on every charge is easy to resent.
    #[serde(default)]
    pub alert_80: bool,
    #[serde(default)]
    pub alert_full: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tray_mode: TrayMode::default(),
            graph: GraphKind::default(),
            decimals: true,
            theme: PanelTheme::default(),
            show_graph: true,
            graph_zero_line: true,
            graph_autofit: true,
            pin_panel: false,
            panel_pos: None,
            alert_low: true,
            alert_low_pct: default_low(),
            alert_critical: true,
            alert_critical_pct: default_critical(),
            alert_80: false,
            alert_full: false,
        }
    }
}

impl Settings {
    /// Format a charge level for display, honouring the decimals preference.
    ///
    /// The endpoints are always shown whole. At a full or empty pack the
    /// decimals cannot move -- the reading is pinned at the limit -- so
    /// "100.00%" only implies a precision that is not there.
    pub fn format_soc(&self, soc: f64) -> String {
        let pct = (soc * 100.0).clamp(0.0, 100.0);
        if !self.decimals || pct >= 99.995 || pct <= 0.005 {
            format!("{pct:.0}%")
        } else {
            // The capacity gauge quantises to ~0.03% on this class of battery,
            // so two places resolve real movement rather than inventing it.
            format!("{pct:.2}%")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_familiar_ones() {
        let s = Settings::default();
        assert_eq!(s.tray_mode, TrayMode::Battery);
        assert_eq!(s.graph, GraphKind::Throughput);
        assert!(s.decimals, "decimals are on by default");
    }

    #[test]
    fn decimals_control_the_charge_readout() {
        let mut s = Settings::default();
        assert_eq!(s.format_soc(0.912345), "91.23%");
        s.decimals = false;
        assert_eq!(s.format_soc(0.912345), "91%");
        s.decimals = true;
        // The endpoints are pinned, so decimals there would be noise.
        assert_eq!(s.format_soc(1.0), "100%");
        assert_eq!(s.format_soc(0.0), "0%");
        assert_eq!(s.format_soc(0.999_96), "100%", "within rounding of full reads as full");
        assert_eq!(s.format_soc(0.9987), "99.87%", "just below stays precise");
        assert_eq!(s.format_soc(0.00002), "0%");
        assert_eq!(s.format_soc(0.0021), "0.21%");
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            tray_mode: TrayMode::Time,
            graph: GraphKind::Level,
            decimals: true,
            theme: PanelTheme::Light,
            pin_panel: true,
            panel_pos: Some((100, 200)),
            alert_80: true,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    /// Adding an option must not invalidate a file written before it existed.
    #[test]
    fn a_partial_file_fills_in_defaults() {
        let s: Settings = serde_json::from_str(r#"{"decimals":false}"#).unwrap();
        assert!(!s.decimals, "an explicit choice must survive a version change");
        assert_eq!(s.tray_mode, TrayMode::Battery);
        assert_eq!(s.graph, GraphKind::Throughput);
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Settings::default());
    }

    #[test]
    fn graph_options_start_on() {
        let s = Settings::default();
        assert!(s.show_graph && s.graph_zero_line && s.graph_autofit);
    }

    #[test]
    fn theme_resolves_against_the_system_setting() {
        assert!(PanelTheme::System.is_light(true));
        assert!(!PanelTheme::System.is_light(false));
        assert!(PanelTheme::Light.is_light(false), "an explicit choice wins");
        assert!(!PanelTheme::Dark.is_light(true));
    }

    #[test]
    fn alert_defaults_warn_but_do_not_nag() {
        let s = Settings::default();
        assert!(s.alert_low && s.alert_critical, "running out matters");
        assert_eq!((s.alert_low_pct, s.alert_critical_pct), (20, 10));
        assert!(!s.alert_80 && !s.alert_full, "charge milestones are opt-in");
    }

    #[test]
    fn graph_kinds_carry_their_own_span() {
        assert!(GraphKind::Level.span_ms() > GraphKind::Throughput.span_ms());
        assert_eq!(GraphKind::Level.toggled(), GraphKind::Throughput);
        assert_eq!(GraphKind::Throughput.toggled(), GraphKind::Level);
    }

    #[test]
    fn only_the_numeric_modes_are_text() {
        assert!(TrayMode::Percent.is_text());
        assert!(TrayMode::Watts.is_text());
        assert!(TrayMode::Time.is_text());
        assert!(!TrayMode::Battery.is_text());
        assert!(!TrayMode::Logo.is_text());
    }
}
