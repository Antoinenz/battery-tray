//! Cold-start seeding from Windows' own battery report.
//!
//! `powercfg /batteryreport /xml` exposes ~14 days of state transitions with
//! real mWh capacities, plus contiguous charge/discharge sessions. Mining it on
//! first run means the app is accurate on day one instead of after a week of
//! learning. Parsed with a small scanner rather than an XML crate: the shape is
//! fixed and flat, and this keeps the dependency list at two.

use crate::model::Model;
use crate::types::{Activity, Context};

/// One contiguous charge session, as recorded by Windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChargeSession {
    pub from_mwh: f64,
    pub to_mwh: f64,
    pub full_mwh: f64,
    pub secs: f64,
}

impl ChargeSession {
    pub fn span_soc(&self) -> f64 {
        if self.full_mwh <= 0.0 {
            return 0.0;
        }
        (self.to_mwh - self.from_mwh) / self.full_mwh
    }
    pub fn rate_mw(&self) -> f64 {
        if self.secs <= 0.0 {
            return 0.0;
        }
        (self.to_mwh - self.from_mwh) / (self.secs / 3600.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DischargeObs {
    pub activity: Activity,
    pub rate_mw: f64,
    pub hours: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeedData {
    pub charge: Vec<ChargeSession>,
    pub discharge: Vec<DischargeObs>,
}

/// A charge session covering no more than this much SoC is narrow enough to
/// attribute to a single point on the curve.
const NARROW_SPAN: f64 = 0.10;

/// Segments shorter than this are dominated by timing noise.
const MIN_SEGMENT_S: f64 = 120.0;
/// Plausible whole-system power range, in mW.
const MIN_W: f64 = 200.0;
const MAX_W: f64 = 120_000.0;

/// Find `name="value"`, requiring whitespace before the name so that asking for
/// `ChargeCapacity` cannot match `FullChargeCapacity`.
fn attr<'a>(el: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{}=\"", name);
    let mut from = 0;
    while let Some(p) = el[from..].find(&needle) {
        let at = from + p;
        let preceded_by_space = at > 0
            && el[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        if preceded_by_space {
            let start = at + needle.len();
            let rest = &el[start..];
            return rest.find('"').map(|e| &rest[..e]);
        }
        from = at + needle.len();
    }
    None
}

fn num<T: std::str::FromStr>(el: &str, name: &str) -> Option<T> {
    attr(el, name)?.trim().parse::<T>().ok()
}

/// Yield each `<Name .../>` element as a string slice.
fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{}", name);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = xml[i..].find(&open) {
        let s = i + p;
        // Reject a longer tag that merely starts with the same letters. Real
        // reports put each attribute on its own line, so the delimiter is
        // whitespace of any kind, not necessarily a space.
        let after = xml[s + open.len()..].chars().next();
        if !matches!(after, Some(c) if c.is_whitespace() || c == '/' || c == '>') {
            i = s + open.len();
            continue;
        }
        match xml[s..].find('>') {
            Some(e) => {
                out.push(&xml[s..s + e + 1]);
                i = s + e + 1;
            }
            None => break,
        }
    }
    out
}

/// Days since the Unix epoch (Howard Hinnant's civil-date algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` to Unix seconds.
pub fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let p = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (p(0, 4)?, p(5, 7)?, p(8, 10)?);
    let (h, mi, sec) = (p(11, 13)?, p(14, 16)?, p(17, 19)?);
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

fn plausible(mw: f64) -> bool {
    mw.is_finite() && (MIN_W..MAX_W).contains(&mw)
}

/// Extract usable power observations from a battery report.
pub fn parse_report(xml: &str) -> SeedData {
    let mut out = SeedData::default();

    // UsageEntry: duration in 100 ns ticks, Discharge in mWh (negative = gained).
    for el in elements(xml, "UsageEntry") {
        let (Some(ticks), Some(delta), Some(cap), Some(full)) = (
            num::<i64>(el, "Duration"),
            num::<f64>(el, "Discharge"),
            num::<f64>(el, "ChargeCapacity"),
            num::<f64>(el, "FullChargeCapacity"),
        ) else {
            continue;
        };
        let secs = ticks as f64 / 1e7;
        if secs < MIN_SEGMENT_S || full <= 0.0 {
            continue;
        }
        let hours = secs / 3600.0;
        let mw = delta.abs() / hours;
        if !plausible(mw) {
            continue;
        }
        // Charge segments are deliberately NOT taken from UsageEntry: a
        // segment can span AC time in which the battery was already full or
        // barely charging, so its average rate is not a charge rate. Contiguous
        // Drain sessions below are the clean source for that.
        let _ = cap;
        if delta > 0.0 && attr(el, "Ac") == Some("0") {
            let activity = match attr(el, "EntryType") {
                Some("Active") => Activity::Active,
                _ => Activity::Standby,
            };
            out.discharge.push(DischargeObs { activity, rate_mw: mw, hours });
        }
    }

    // Drain: contiguous sessions, giving better SoC resolution for the curve.
    for el in elements(xml, "Drain") {
        let (Some(t0), Some(t1), Some(c0), Some(c1), Some(full)) = (
            attr(el, "StartTimestamp").and_then(parse_iso8601_utc),
            attr(el, "EndTimestamp").and_then(parse_iso8601_utc),
            num::<f64>(el, "StartChargeCapacity"),
            num::<f64>(el, "EndChargeCapacity"),
            num::<f64>(el, "StartFullChargeCapacity"),
        ) else {
            continue;
        };
        let secs = (t1 - t0) as f64;
        if secs < MIN_SEGMENT_S || full <= 0.0 || c1 <= c0 {
            continue;
        }
        let hours = secs / 3600.0;
        let mw = (c1 - c0) / hours;
        if !plausible(mw) {
            continue;
        }
        out.charge.push(ChargeSession {
            from_mwh: c0,
            to_mwh: c1,
            full_mwh: full,
            secs,
        });
    }

    out
}

/// Fold seed observations into a model.
pub fn apply(model: &mut Model, data: &SeedData, now_ms: i64) {
    // Narrow sessions pin down one bucket of the shape; wide ones only
    // constrain the overall level. Sorting shortest-span first lets the peak
    // settle on clean point observations before the integral checks run.
    let mut sessions: Vec<&ChargeSession> = data.charge.iter().collect();
    sessions.sort_by(|a, b| a.span_soc().partial_cmp(&b.span_soc()).unwrap_or(std::cmp::Ordering::Equal));

    for s in &sessions {
        let w = weight(s.secs / 3600.0);
        if s.span_soc() <= NARROW_SPAN {
            let soc = (s.from_mwh + s.to_mwh) * 0.5 / s.full_mwh;
            model.curve.observe(soc, s.rate_mw(), w);
        } else {
            model.curve.calibrate_from_session(s.from_mwh, s.to_mwh, s.full_mwh, s.secs, w);
        }
    }

    for o in &data.discharge {
        let w = weight(o.hours);
        // The report classifies activity but not CPU load. Seed the mid band at
        // full weight and the outer bands at a fraction, so every bucket starts
        // sensible and real samples quickly pull them apart.
        for band in 0..3u8 {
            let scale = if band == 1 { 1.0 } else { 0.4 };
            let idx = Context { activity: o.activity, load_band: band }.index();
            model.priors[idx].observe(o.rate_mw, w * scale);
        }
    }

    model.seeded = true;
    model.seeded_at_ms = now_ms;
}

fn weight(hours: f64) -> f64 {
    (hours * 4.0).clamp(0.05, 6.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<BatteryReport>
      <UsageEntry Timestamp="2026-08-24T01:25:26Z" Duration="36000000000" Ac="0"
        EntryType="Active" ChargeCapacity="20000" Discharge="16500"
        FullChargeCapacity="38820" />
      <UsageEntry Timestamp="2026-08-24T02:25:26Z" Duration="36000000000" Ac="0"
        EntryType="ConnectedStandby" ChargeCapacity="18000" Discharge="2200"
        FullChargeCapacity="38820" />
      <UsageEntry Timestamp="2026-08-24T03:25:26Z" Duration="36000000000" Ac="1"
        EntryType="Active" ChargeCapacity="12000" Discharge="-26850"
        FullChargeCapacity="38820" />
      <UsageEntry Timestamp="2026-08-24T04:25:26Z" Duration="100" Ac="0"
        EntryType="Active" ChargeCapacity="12000" Discharge="500"
        FullChargeCapacity="38820" />
      <Drain StartTimestamp="2026-08-24T05:00:00Z" EndTimestamp="2026-08-24T06:00:00Z"
        StartChargeCapacity="10000" StartFullChargeCapacity="38820"
        EndChargeCapacity="36850" EndFullChargeCapacity="38820" />
    </BatteryReport>"#;

    #[test]
    fn parses_discharge_segments_by_activity() {
        let d = parse_report(XML);
        // One hour at 16,500 mWh drained == 16.5 W.
        let active = d
            .discharge
            .iter()
            .find(|o| o.activity == Activity::Active)
            .expect("active segment");
        assert!((active.rate_mw - 16_500.0).abs() < 1.0);
        let standby = d
            .discharge
            .iter()
            .find(|o| o.activity == Activity::Standby)
            .expect("standby segment");
        assert!((standby.rate_mw - 2_200.0).abs() < 1.0);
    }

    /// A UsageEntry marked AC can cover time when the battery was already full
    /// or barely charging, so its average is not a charge rate. Only contiguous
    /// Drain sessions may feed the curve.
    #[test]
    fn charge_data_comes_only_from_contiguous_sessions() {
        let d = parse_report(XML);
        assert_eq!(d.charge.len(), 1, "the AC UsageEntry must not become charge data");
        let s = d.charge[0];
        assert_eq!(s.from_mwh, 10_000.0);
        assert_eq!(s.to_mwh, 36_850.0);
        assert!((s.secs - 3600.0).abs() < 1.0);
        assert!((s.rate_mw() - 26_850.0).abs() < 1.0);
        assert!(s.span_soc() > 0.6);
    }

    #[test]
    fn short_segments_are_rejected_as_timing_noise() {
        let d = parse_report(XML);
        assert_eq!(d.discharge.len(), 2, "the 10 us segment must be dropped");
    }

    #[test]
    fn iso8601_parses_against_known_epochs() {
        assert_eq!(parse_iso8601_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_utc("2000-01-01T00:00:00Z"), Some(946_684_800));
        assert_eq!(parse_iso8601_utc("2026-08-24T01:25:26Z"), Some(1_787_534_726));
        assert_eq!(parse_iso8601_utc("garbage"), None);
        assert_eq!(parse_iso8601_utc(""), None);
    }

    #[test]
    fn element_scan_does_not_match_a_longer_tag() {
        let xml = r#"<Drain A="1" /><DrainSummary A="2" />"#;
        assert_eq!(elements(xml, "Drain").len(), 1);
    }

    /// `powercfg` writes one attribute per line. An earlier version required a
    /// space after the tag name and silently parsed nothing from real reports.
    #[test]
    fn parses_real_powercfg_formatting_with_newlines() {
        let xml = "\u{feff}<Batteries>\n    <UsageEntry\n      \
                   Timestamp=\"2026-08-24T01:25:26Z\"\n      Duration=\"36000000000\"\n      \
                   Ac=\"0\"\n      EntryType=\"Active\"\n      ChargeCapacity=\"20000\"\n      \
                   Discharge=\"16500\"\n      FullChargeCapacity=\"38820\"\n      />\n</Batteries>";
        let d = parse_report(xml);
        assert_eq!(d.discharge.len(), 1, "newline-separated attributes must parse");
        assert!((d.discharge[0].rate_mw - 16_500.0).abs() < 1.0);
    }

    #[test]
    fn attribute_lookup_is_not_fooled_by_a_longer_name() {
        let el = r#"<X FullChargeCapacity="37060" ChargeCapacity="1290" />"#;
        assert_eq!(attr(el, "ChargeCapacity"), Some("1290"));
        assert_eq!(attr(el, "FullChargeCapacity"), Some("37060"));
        // A name that appears only as a suffix must not resolve.
        assert_eq!(attr(r#"<X FullChargeCapacity="1" />"#, "ChargeCapacity"), None);
    }

    #[test]
    fn seeding_trains_both_the_curve_and_the_priors() {
        let mut m = Model::default();
        assert!(!m.seeded);
        let d = parse_report(XML);
        apply(&mut m, &d, 1000);
        assert!(m.seeded);
        assert!(m.curve.peak_ready(), "a charge session should train the peak");
        let active = m.priors[Context { activity: Activity::Active, load_band: 1 }.index()];
        assert!(active.ready());
        assert!((active.mean - 16_500.0).abs() < 2_000.0, "{}", active.mean);
        let standby = m.priors[Context { activity: Activity::Standby, load_band: 1 }.index()];
        assert!(standby.mean < active.mean / 3.0, "standby must be far lower");
    }

    /// A wide session says how long the whole span took; it must adjust the
    /// overall level without flattening the taper it spans.
    #[test]
    fn a_wide_session_scales_the_curve_without_reshaping_it() {
        let mut m = Model::default();
        let before: Vec<f64> = (0..20).map(|i| m.curve.shape_at(i as f64 / 20.0)).collect();
        let full = 38_820.0;
        // This span really took twice as long as the seed curve predicts.
        let predicted = m.curve.time_to(0.2 * full, 0.9 * full, full);
        m.curve
            .calibrate_from_session(0.2 * full, 0.9 * full, full, predicted * 2.0, 4.0);
        let after: Vec<f64> = (0..20).map(|i| m.curve.shape_at(i as f64 / 20.0)).collect();
        assert_eq!(before, after, "shape must be untouched by an integral check");
        assert!(
            m.curve.peak_mw() < 26_850.0,
            "a slower-than-predicted session must lower the level, got {}",
            m.curve.peak_mw()
        );
    }

    #[test]
    fn malformed_xml_yields_nothing_rather_than_panicking() {
        assert_eq!(parse_report(""), SeedData::default());
        assert_eq!(parse_report("<UsageEntry"), SeedData::default());
        assert_eq!(parse_report(r#"<UsageEntry Duration="x" />"#), SeedData::default());
        assert_eq!(
            parse_report(r#"<Drain StartChargeCapacity="1" />"#),
            SeedData::default()
        );
    }

    #[test]
    fn implausible_power_is_discarded() {
        let xml = r#"<UsageEntry Duration="36000000000" Ac="0" EntryType="Active"
            ChargeCapacity="20000" Discharge="9000000" FullChargeCapacity="38820" />"#;
        assert!(parse_report(xml).discharge.is_empty(), "9 kW is not a laptop");
    }
}
