//! Cheap system context. One syscall each, no performance counters, no WMI.

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::GetSystemTimes;

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// System-wide CPU utilisation, measured as a delta between calls.
///
/// Used only as a *context* feature for bucketing the discharge prior --
/// never as a power estimate. The battery's own rate is the ground truth for
/// power; CPU load merely predicts where it is heading.
pub struct CpuMeter {
    prev_idle: u64,
    prev_total: u64,
}

impl Default for CpuMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuMeter {
    pub fn new() -> Self {
        let mut m = CpuMeter { prev_idle: 0, prev_total: 0 };
        m.sample();
        m
    }

    /// Percentage busy since the previous call, 0..100.
    pub fn sample(&mut self) -> f32 {
        unsafe {
            let (mut idle, mut kernel, mut user) = (
                FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
                FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
                FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            );
            if GetSystemTimes(&mut idle, &mut kernel, &mut user) == 0 {
                return 0.0;
            }
            // Kernel time already includes idle time.
            let idle = ft(idle);
            let total = ft(kernel) + ft(user);
            let d_idle = idle.saturating_sub(self.prev_idle);
            let d_total = total.saturating_sub(self.prev_total);
            self.prev_idle = idle;
            self.prev_total = total;
            if d_total == 0 {
                return 0.0;
            }
            let busy = d_total.saturating_sub(d_idle) as f64 / d_total as f64;
            (busy * 100.0).clamp(0.0, 100.0) as f32
        }
    }
}

/// True when Windows is set to the light theme, so the tray icon can be drawn
/// in a colour that is actually visible against the taskbar.
pub fn system_uses_light_theme() -> bool {
    let subkey: Vec<u16> =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
            .encode_utf16()
            .collect();
    let value: Vec<u16> = "SystemUsesLightTheme\0".encode_utf16().collect();
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    unsafe {
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut std::ffi::c_void,
            &mut size,
        );
        // Default to dark: the common Windows 11 taskbar.
        if r == 0 {
            data != 0
        } else {
            false
        }
    }
}
