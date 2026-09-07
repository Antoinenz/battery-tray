//! Background cold-start seeding.
//!
//! Shells out to `powercfg /batteryreport /xml` once, on a worker thread, and
//! hands the parsed result back to the UI thread. `powercfg` takes about a
//! second and must never block the message loop.

use battery_core::seed::{self, SeedData};
use std::os::windows::process::CommandExt;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

const WM_APP_SEED: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 3;
/// Keep the console window from flashing on screen.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn spawn(hwnd: HWND, slot: Arc<Mutex<Option<SeedData>>>) {
    let hwnd = hwnd as isize;
    std::thread::spawn(move || {
        let Some(data) = collect() else { return };
        if data.charge.is_empty() && data.discharge.is_empty() {
            return;
        }
        if let Ok(mut g) = slot.lock() {
            *g = Some(data);
        }
        unsafe {
            PostMessageW(hwnd as HWND, WM_APP_SEED, 0, 0);
        }
    });
}

fn collect() -> Option<SeedData> {
    let out = std::env::temp_dir().join("battery-tray-seed.xml");
    let status = std::process::Command::new("powercfg")
        .args([
            "/batteryreport",
            "/xml",
            "/output",
            &out.to_string_lossy(),
            "/duration",
            "14",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let xml = std::fs::read_to_string(&out).ok()?;
    let _ = std::fs::remove_file(&out);
    Some(seed::parse_report(&xml))
}
