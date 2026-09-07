//! Windows platform layer: battery IOCTLs and cheap system context.

pub mod battery;
pub mod sysinfo;

pub use battery::{Battery, BatteryInfo, RawStatus};
pub use sysinfo::{system_uses_light_theme, CpuMeter};

use battery_core::types::{Sample, POWER_STATE_MASK, UNKNOWN_RATE};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A wait that keeps returning this fast has been ignored by the driver.
const SPIN_THRESHOLD_MS: u128 = 150;
const SPIN_STRIKES: u32 = 6;
/// How often to re-read full-charge capacity, which drifts day to day.
const INFO_REFRESH_S: u64 = 300;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Produces [`Sample`]s, blocking in the kernel between them.
pub struct Sampler {
    battery: Battery,
    cpu: CpuMeter,
    info: BatteryInfo,
    last_state: u32,
    last_cap: u32,
    last_info_refresh: Instant,
    spin_strikes: u32,
    /// Set when the driver ignores the blocking wait; falls back to sleeping.
    polling: bool,
}

impl Sampler {
    pub fn new() -> Option<Sampler> {
        let mut battery = Battery::open_first()?;
        let info = battery.info().or_else(|| {
            battery.refresh_tag();
            battery.info()
        })?;
        let st = battery.status().unwrap_or_default();
        Some(Sampler {
            battery,
            cpu: CpuMeter::new(),
            info,
            last_state: st.power_state,
            last_cap: st.capacity_mwh,
            last_info_refresh: Instant::now(),
            spin_strikes: 0,
            polling: false,
        })
    }

    pub fn info(&self) -> &BatteryInfo {
        &self.info
    }

    pub fn is_polling(&self) -> bool {
        self.polling
    }

    fn band(&self) -> u32 {
        // Wake on roughly a tenth of a percent of capacity: responsive while
        // charging fast, without waking pointlessly on gauge quantisation.
        (self.info.full_mwh / 1000).clamp(20, 200)
    }

    /// Block until the battery changes or `timeout_ms` elapses, then produce a
    /// sample. Returns `None` only if the battery went away.
    pub fn next_sample(&mut self, timeout_ms: u32, display_on: bool) -> Option<Sample> {
        let started = Instant::now();
        let status = if self.polling {
            std::thread::sleep(std::time::Duration::from_millis(timeout_ms as u64));
            self.battery.status()
        } else {
            let band = self.band();
            let r = self
                .battery
                .wait(timeout_ms, self.last_state, self.last_cap, band);
            // A driver that ignores the wait would spin this thread at 100%.
            if started.elapsed().as_millis() < SPIN_THRESHOLD_MS {
                self.spin_strikes += 1;
                if self.spin_strikes >= SPIN_STRIKES {
                    self.polling = true;
                }
            } else {
                self.spin_strikes = 0;
            }
            r.or_else(|| self.battery.status())
        };

        let status = match status {
            Some(s) => s,
            None => {
                // Most often a pack swap invalidating the tag.
                if !self.battery.refresh_tag() {
                    return None;
                }
                self.battery.status()?
            }
        };

        if self.last_info_refresh.elapsed().as_secs() >= INFO_REFRESH_S {
            if let Some(i) = self.battery.info() {
                self.info = i;
            }
            self.last_info_refresh = Instant::now();
        }

        self.last_state = status.power_state;
        self.last_cap = status.capacity_mwh;

        let rate = if self.info.relative || status.rate_mw == UNKNOWN_RATE {
            UNKNOWN_RATE
        } else {
            status.rate_mw
        };

        Some(Sample {
            t_ms: now_ms(),
            capacity_mwh: status.capacity_mwh,
            full_mwh: self.info.full_mwh,
            rate_mw: rate,
            voltage_mv: status.voltage_mv,
            // Vendor bits beyond the documented four appear in the wild (this
            // Surface reports 0x62); keep only what is defined.
            power_state: status.power_state & POWER_STATE_MASK,
            cpu_pct: self.cpu.sample(),
            display_on,
        })
    }
}
