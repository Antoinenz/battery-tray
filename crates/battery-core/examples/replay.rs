//! Replay a recorded battery trace and measure prediction accuracy.
//!
//! Usage: `replay <trace.csv> [--seed <battery-report.xml>]`
//!
//! Columns: unix_ms,capacity_mwh,rate_mw,voltage_mv,charging,discharging,ac,cpu_pct
//!
//! For every sample the estimator is asked how long capacity will take to move
//! by a fixed step. The trace itself says when that actually happened, so error
//! is measured end to end. A naive `remaining / instantaneous rate` predictor
//! runs alongside as a baseline.

use battery_core::estimator::Estimator;
use battery_core::model::Model;
use battery_core::seed;
use battery_core::types::*;

struct Stats {
    name: &'static str,
    model: Vec<(f64, f64)>,
    naive: Vec<(f64, f64)>,
}

fn pct(v: &mut Vec<f64>, q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * q) as usize).min(v.len() - 1)]
}

fn report(s: &Stats) {
    if s.model.is_empty() {
        println!("\n{}: no resolvable checkpoints", s.name);
        return;
    }
    let mut rel: Vec<f64> = s.model.iter().map(|(p, a)| (p - a).abs() / a).collect();
    let bias = s.model.iter().map(|(p, a)| (p - a) / a).sum::<f64>() / s.model.len() as f64;
    let median = pct(&mut rel, 0.5);
    let p90 = pct(&mut rel, 0.9);
    println!("\n{}  (n={})", s.name, s.model.len());
    println!("  median abs error : {:5.1}%", median * 100.0);
    println!("  p90 abs error    : {:5.1}%", p90 * 100.0);
    println!("  bias (+ = long)  : {:+5.1}%", bias * 100.0);
    if !s.naive.is_empty() {
        let mut n: Vec<f64> = s.naive.iter().map(|(p, a)| (p - a).abs() / a).collect();
        let nm = pct(&mut n, 0.5);
        println!("  naive baseline   : {:5.1}%  (n={})", nm * 100.0, s.naive.len());
        let better = (nm - median) / nm * 100.0;
        println!("  improvement      : {better:+5.1}% vs naive");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: replay <trace.csv> [--seed <report.xml>]");
        std::process::exit(2);
    };
    let seed_path = args.iter().position(|a| a == "--seed").and_then(|i| args.get(i + 1));

    let text = std::fs::read_to_string(path).expect("read trace");
    let mut rows: Vec<Sample> = Vec::new();
    for line in text.lines().skip(1) {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 {
            continue;
        }
        let g = |i: usize| f.get(i).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(0.0);
        let mut state = 0u32;
        if g(4) != 0.0 {
            state |= BATTERY_CHARGING;
        }
        if g(5) != 0.0 {
            state |= BATTERY_DISCHARGING;
        }
        if g(6) != 0.0 {
            state |= BATTERY_POWER_ON_LINE;
        }
        rows.push(Sample {
            t_ms: g(0) as i64,
            capacity_mwh: g(1) as u32,
            full_mwh: 0,
            rate_mw: g(2) as i32,
            voltage_mv: g(3) as u32,
            power_state: state,
            cpu_pct: g(7) as f32,
            display_on: true,
        });
    }
    if rows.len() < 10 {
        eprintln!("not enough samples ({})", rows.len());
        std::process::exit(1);
    }

    let full = rows.iter().map(|r| r.capacity_mwh).max().unwrap_or(0).max(38_820);
    for r in rows.iter_mut() {
        r.full_mwh = full;
    }

    let mut model = Model::default();
    if let Some(sp) = seed_path {
        let xml = std::fs::read_to_string(sp).expect("read report");
        let data = seed::parse_report(&xml);
        println!(
            "seeded from {} charge and {} discharge segments",
            data.charge.len(),
            data.discharge.len()
        );
        seed::apply(&mut model, &data, rows[0].t_ms);
        println!(
            "  learned charge peak: {:.1} W",
            model.curve.peak_mw() / 1000.0
        );
    }

    let n_dis = rows.iter().filter(|r| r.discharging()).count();
    let span_min = (rows.last().unwrap().t_ms - rows[0].t_ms) as f64 / 60_000.0;
    println!(
        "samples {}  span {:.1} min  full {} mWh  ({} discharging, {} charging)",
        rows.len(),
        span_min,
        full,
        n_dis,
        rows.len() - n_dis
    );

    let step = (full as f64 * 0.01).max(200.0);
    let mut est = Estimator::new(model);
    let mut dis = Stats { name: "DISCHARGING (time to lose 1% SoC)", model: vec![], naive: vec![] };
    let mut chg = Stats { name: "CHARGING (time to gain 1% SoC)", model: vec![], naive: vec![] };

    for i in 0..rows.len() {
        let e = est.update(rows[i]);
        let r = rows[i];
        let rising = match e.phase {
            Phase::Charging => true,
            Phase::Discharging => false,
            _ => continue,
        };
        let target = if rising {
            r.capacity_mwh as f64 + step
        } else {
            r.capacity_mwh as f64 - step
        };

        // When did capacity actually reach the target?
        let hit = rows[i + 1..].iter().position(|q| {
            if rising {
                q.capacity_mwh as f64 >= target
            } else {
                (q.capacity_mwh as f64) <= target
            }
        });
        let Some(j) = hit else { continue };
        let actual = (rows[i + 1 + j].t_ms - r.t_ms) as f64 / 1000.0;
        if actual <= 30.0 {
            continue;
        }

        let bucket = if rising { &mut chg } else { &mut dis };
        if let Some(pred) = est.predict_to_capacity(target) {
            if (30.0..14_400.0).contains(&pred) {
                bucket.model.push((pred, actual));
            }
        }
        // Naive: this instant's reported rate, extrapolated.
        if r.rate_known() && r.rate_mw != 0 {
            let naive = step / (r.rate_mw.abs() as f64) * 3600.0;
            bucket.naive.push((naive, actual));
        }
    }

    report(&dis);
    report(&chg);

    println!("\nlearned state after replay:");
    println!(
        "  reported-vs-actual calibration: discharge {:.3} (n={:.0}), charge {:.3} (n={:.0})",
        est.model.calib_discharge.mean,
        est.model.calib_discharge.n,
        est.model.calib_charge.mean,
        est.model.calib_charge.n
    );
    println!("  charge curve maturity: {:.0}%", est.model.curve.maturity() * 100.0);
}
