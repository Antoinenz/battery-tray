use crate::estimator::HistPoint;
use crate::model::Model;
use crate::settings::Settings;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const HISTORY_MAGIC: u32 = 0x3148_5442; // "BTH1"
const HISTORY_RECORD: usize = 16;
const HISTORY_MAX: usize = 4096;

pub fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("BatteryTray")
}

pub fn ensure_dir() -> std::io::Result<PathBuf> {
    let d = data_dir();
    fs::create_dir_all(&d)?;
    Ok(d)
}

/// Write via a temporary file and rename, so a crash mid-write cannot leave a
/// truncated model behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // Windows rename fails if the destination exists.
    let _ = fs::remove_file(path);
    fs::rename(&tmp, path)
}

pub fn model_path() -> PathBuf {
    data_dir().join("model.json")
}
pub fn history_path() -> PathBuf {
    data_dir().join("history.bin")
}

/// Load the learned model, falling back to defaults for anything unreadable.
/// A corrupt file must never stop the app from starting.
/// Parse a stored model, falling back to defaults for anything unusable.
///
/// A model written by a different version may encode assumptions that no longer
/// hold -- a revised seed curve, a changed statistic -- and those are invisible
/// once stored as plain numbers. Start over rather than silently carrying stale
/// beliefs forward; re-seeding is cheap.
pub fn parse_model(json: &str) -> Model {
    let mut m = serde_json::from_str::<Model>(strip_bom(json))
        .ok()
        .filter(|m: &Model| m.version == crate::model::MODEL_VERSION)
        .unwrap_or_default();
    m.sanitise();
    m
}

/// Strip a UTF-8 byte-order mark. Windows editors -- and PowerShell's
/// `Set-Content -Encoding utf8` -- prepend one, and `serde_json` rejects it,
/// which would silently discard a hand-edited config.
fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// Parse stored settings. Unlike the learned model these are user intent, so a
/// file from another version is repaired field-by-field rather than discarded.
pub fn parse_settings(json: &str) -> Settings {
    serde_json::from_str::<Settings>(strip_bom(json)).unwrap_or_default()
}

pub fn load_settings() -> Settings {
    parse_settings(&fs::read_to_string(settings_path()).unwrap_or_default())
}

pub fn save_settings(s: &Settings) -> std::io::Result<()> {
    ensure_dir()?;
    let bytes = serde_json::to_vec_pretty(s).map_err(std::io::Error::other)?;
    write_atomic(&settings_path(), &bytes)
}

pub fn load_model() -> Model {
    parse_model(&fs::read_to_string(model_path()).unwrap_or_default())
}

pub fn save_model(m: &Model) -> std::io::Result<()> {
    ensure_dir()?;
    let bytes = serde_json::to_vec(m).map_err(std::io::Error::other)?;
    write_atomic(&model_path(), &bytes)
}

pub fn encode_history(pts: &[HistPoint]) -> Vec<u8> {
    let take = pts.len().min(HISTORY_MAX);
    let pts = &pts[pts.len() - take..];
    let mut out = Vec::with_capacity(8 + take * HISTORY_RECORD);
    out.extend_from_slice(&HISTORY_MAGIC.to_le_bytes());
    out.extend_from_slice(&(take as u32).to_le_bytes());
    for p in pts {
        out.extend_from_slice(&p.t_ms.to_le_bytes());
        out.extend_from_slice(&p.watts.to_le_bytes());
        out.extend_from_slice(&p.soc.to_le_bytes());
    }
    out
}

pub fn decode_history(bytes: &[u8]) -> Vec<HistPoint> {
    if bytes.len() < 8 || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != HISTORY_MAGIC {
        return Vec::new();
    }
    let n = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let avail = (bytes.len() - 8) / HISTORY_RECORD;
    let n = n.min(avail).min(HISTORY_MAX);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let o = 8 + i * HISTORY_RECORD;
        out.push(HistPoint {
            t_ms: i64::from_le_bytes(bytes[o..o + 8].try_into().unwrap()),
            watts: f32::from_le_bytes(bytes[o + 8..o + 12].try_into().unwrap()),
            soc: f32::from_le_bytes(bytes[o + 12..o + 16].try_into().unwrap()),
        });
    }
    out
}

pub fn load_history() -> Vec<HistPoint> {
    fs::read(history_path()).map(|b| decode_history(&b)).unwrap_or_default()
}

pub fn save_history(pts: &[HistPoint]) -> std::io::Result<()> {
    ensure_dir()?;
    write_atomic(&history_path(), &encode_history(pts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: i64, w: f32, s: f32) -> HistPoint {
        HistPoint { t_ms: t, watts: w, soc: s }
    }

    #[test]
    fn history_round_trips() {
        let pts = vec![pt(1000, -14.5, 0.62), pt(31_000, 31.3, 0.63)];
        let back = decode_history(&encode_history(&pts));
        assert_eq!(back, pts);
    }

    #[test]
    fn history_is_capped_keeping_the_newest() {
        let pts: Vec<_> = (0..6000).map(|i| pt(i as i64 * 1000, i as f32, 0.5)).collect();
        let back = decode_history(&encode_history(&pts));
        assert_eq!(back.len(), HISTORY_MAX);
        assert_eq!(back.last().unwrap().t_ms, 5999_000, "must keep the most recent");
    }

    #[test]
    fn garbage_history_decodes_to_nothing_rather_than_panicking() {
        assert!(decode_history(&[]).is_empty());
        assert!(decode_history(&[1, 2, 3]).is_empty());
        assert!(decode_history(b"NOPEnope....").is_empty());
        // Valid header claiming more records than the file holds.
        let mut b = HISTORY_MAGIC.to_le_bytes().to_vec();
        b.extend_from_slice(&9999u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 16]);
        assert_eq!(decode_history(&b).len(), 1);
    }

    #[test]
    fn empty_history_round_trips() {
        assert!(decode_history(&encode_history(&[])).is_empty());
    }

    #[test]
    fn a_current_model_round_trips_through_storage() {
        let mut m = Model::default();
        m.curve.observe(0.3, 31_000.0, 5.0);
        m.seeded = true;
        let back = parse_model(&serde_json::to_string(&m).unwrap());
        assert!(back.seeded);
        assert!((back.curve.peak_mw() - m.curve.peak_mw()).abs() < 1.0);
    }

    #[test]
    fn a_model_file_with_a_byte_order_mark_still_loads() {
        let mut m = Model::default();
        m.seeded = true;
        let json = format!("\u{feff}{}", serde_json::to_string(&m).unwrap());
        assert!(parse_model(&json).seeded);
    }

    #[test]
    fn a_model_from_another_version_is_discarded_not_migrated() {
        let mut m = Model::default();
        m.seeded = true;
        m.curve.shape[10] = 0.01; // a belief from an older seed
        let mut v: serde_json::Value = serde_json::to_value(&m).unwrap();
        v["version"] = serde_json::json!(crate::model::MODEL_VERSION + 1);
        let back = parse_model(&v.to_string());
        assert!(!back.seeded, "a foreign version must re-seed from scratch");
        assert_ne!(back.curve.shape[10], 0.01, "stale shape must not survive");
    }

    #[test]
    fn settings_survive_a_round_trip_and_tolerate_junk() {
        use crate::settings::{GraphKind, TrayMode};
        let s = Settings { tray_mode: TrayMode::Watts, graph: GraphKind::Level, decimals: true };
        assert_eq!(parse_settings(&serde_json::to_string(&s).unwrap()), s);
        // Anything unreadable falls back rather than losing the app.
        for junk in ["", "{", "[]", "null", "not json"] {
            assert_eq!(parse_settings(junk), Settings::default());
        }
        // A byte-order mark must not silently discard the user's choices.
        let with_bom = format!("\u{feff}{}", serde_json::to_string(&s).unwrap());
        assert_eq!(parse_settings(&with_bom), s, "BOM should be tolerated");
    }

    #[test]
    fn unreadable_or_missing_model_falls_back_to_defaults() {
        for s in ["", "{", "null", "{\"version\":1}", "[]"] {
            let m = parse_model(s);
            assert_eq!(m.version, crate::model::MODEL_VERSION);
            assert!(!m.seeded);
        }
    }
}
