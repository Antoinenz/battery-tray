use std::collections::VecDeque;

/// Time-aware exponential moving average. Uses real elapsed time rather than a
/// fixed per-sample alpha, so irregular sampling (an event-driven wait that
/// returns early) does not distort the time constant.
#[derive(Clone, Debug)]
pub struct Ewma {
    tau_s: f64,
    value: Option<f64>,
    last_t: i64,
}

impl Ewma {
    pub fn new(tau_s: f64) -> Self {
        Ewma { tau_s, value: None, last_t: 0 }
    }
    pub fn update(&mut self, t_ms: i64, x: f64) -> f64 {
        match self.value {
            None => {
                self.value = Some(x);
                self.last_t = t_ms;
                x
            }
            Some(prev) => {
                let dt = ((t_ms - self.last_t) as f64 / 1000.0).max(0.0);
                self.last_t = t_ms;
                let alpha = 1.0 - (-dt / self.tau_s).exp();
                let v = prev + alpha * (x - prev);
                self.value = Some(v);
                v
            }
        }
    }
    pub fn get(&self) -> Option<f64> {
        self.value
    }
    pub fn reset(&mut self) {
        self.value = None;
    }
}

/// Least-squares slope over a sliding time window. Used on the capacity gauge:
/// its 10 mWh quantisation averages out over a long window, giving a truer
/// energy flow than the driver's instantaneous rate.
#[derive(Clone, Debug)]
pub struct Slope {
    window_s: f64,
    pts: VecDeque<(i64, f64)>,
}

impl Slope {
    pub fn new(window_s: f64) -> Self {
        Slope { window_s, pts: VecDeque::new() }
    }
    pub fn push(&mut self, t_ms: i64, y: f64) {
        self.pts.push_back((t_ms, y));
        let cutoff = t_ms - (self.window_s * 1000.0) as i64;
        while let Some(&(t, _)) = self.pts.front() {
            if t < cutoff {
                self.pts.pop_front();
            } else {
                break;
            }
        }
    }
    pub fn clear(&mut self) {
        self.pts.clear();
    }
    pub fn span_s(&self) -> f64 {
        match (self.pts.front(), self.pts.back()) {
            (Some(a), Some(b)) => (b.0 - a.0) as f64 / 1000.0,
            _ => 0.0,
        }
    }
    /// Slope in y-units per second, or None if the window is too short or
    /// degenerate to be meaningful.
    pub fn slope_per_s(&self) -> Option<f64> {
        if self.pts.len() < 4 || self.span_s() < 60.0 {
            return None;
        }
        let n = self.pts.len() as f64;
        let t0 = self.pts.front().unwrap().0;
        let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
        for &(t, y) in &self.pts {
            let x = (t - t0) as f64 / 1000.0;
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let den = n * sxx - sx * sx;
        if den.abs() < 1e-9 {
            return None;
        }
        Some((n * sxy - sx * sy) / den)
    }
}

pub fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Running mean/variance with exponential forgetting, so the app tracks a
/// changing machine instead of averaging over its whole lifetime.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct Stat {
    pub mean: f64,
    pub var: f64,
    pub n: f64,
}

impl Stat {
    pub fn observe(&mut self, x: f64, weight: f64) {
        if !x.is_finite() || weight <= 0.0 {
            return;
        }
        // Seed from the first observation. Folding it into a mean of zero would
        // inject a variance the size of the value itself, and every uncertainty
        // band derived from this stat would start far too wide.
        if self.n <= 0.0 {
            self.n = weight.min(500.0);
            self.mean = x;
            self.var = 0.0;
            return;
        }
        self.n = (self.n + weight).min(500.0);
        let alpha = (weight / self.n).clamp(0.002, 1.0);
        let d = x - self.mean;
        self.mean += alpha * d;
        self.var += alpha * (d * d - self.var);
    }
    pub fn sd(&self) -> f64 {
        self.var.max(0.0).sqrt()
    }
    pub fn ready(&self) -> bool {
        self.n >= 3.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_converges_and_respects_elapsed_time() {
        let mut e = Ewma::new(30.0);
        assert_eq!(e.update(0, 10.0), 10.0, "first sample seeds the filter");
        // One time constant of a step from 10 -> 20 covers ~63% of the gap.
        let v = e.update(30_000, 20.0);
        assert!((v - 16.32).abs() < 0.1, "got {v}");
        // A long gap should nearly complete the transition.
        let v = e.update(30_000 + 300_000, 20.0);
        assert!(v > 19.9, "got {v}");
    }

    #[test]
    fn ewma_ignores_time_going_backwards() {
        let mut e = Ewma::new(30.0);
        e.update(10_000, 10.0);
        let v = e.update(0, 50.0);
        assert_eq!(v, 10.0, "negative dt must not move the average");
    }

    #[test]
    fn slope_recovers_a_known_rate() {
        // 100 mWh lost per minute == -1.6667 mWh/s.
        let mut s = Slope::new(900.0);
        for i in 0..20 {
            s.push(i * 60_000, 20_000.0 - (i as f64) * 100.0);
        }
        let sl = s.slope_per_s().expect("enough points");
        assert!((sl - (-100.0 / 60.0)).abs() < 1e-6, "got {sl}");
    }

    #[test]
    fn slope_needs_a_real_window() {
        let mut s = Slope::new(900.0);
        s.push(0, 1.0);
        s.push(1000, 2.0);
        assert!(s.slope_per_s().is_none(), "two points over 1s is not a trend");
    }

    #[test]
    fn slope_evicts_points_outside_the_window() {
        let mut s = Slope::new(300.0);
        for i in 0..100 {
            s.push(i * 10_000, i as f64);
        }
        assert!(s.span_s() <= 300.0, "window not trimmed: {}", s.span_s());
    }

    #[test]
    fn stat_tracks_mean_and_spread() {
        let mut st = Stat::default();
        for _ in 0..50 {
            st.observe(16.0, 1.0);
            st.observe(20.0, 1.0);
        }
        assert!((st.mean - 18.0).abs() < 0.5, "mean {}", st.mean);
        assert!(st.sd() > 1.0, "sd should reflect the spread, got {}", st.sd());
        assert!(st.ready());
    }

    #[test]
    fn stat_rejects_non_finite() {
        let mut st = Stat::default();
        st.observe(f64::NAN, 1.0);
        st.observe(f64::INFINITY, 1.0);
        assert_eq!(st.n, 0.0);
    }

    #[test]
    fn median_handles_both_parities() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&mut []), 0.0);
    }
}
