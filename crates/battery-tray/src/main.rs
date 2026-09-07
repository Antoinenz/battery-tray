#![windows_subsystem = "windows"]

//! Tray application shell: one hidden message window, a custom-drawn popup
//! panel, a standard-controls settings window, and one sampler thread blocked
//! in the kernel. No async runtime.

mod icon;
mod panel;
mod seedjob;
mod settings_ui;

use battery_core::estimator::{Estimator, HistPoint};
use battery_core::settings::Settings;
use battery_core::types::{fmt_duration, Estimates, Phase, Sample};
use battery_core::{seed, store};
use battery_win::{system_uses_light_theme, BatteryInfo, Sampler};
use icon::{phase_to_glyph, IconCache, IconSpec};
use settings_ui::{Action, Health, SettingsWindow};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::{NMHDR, TCN_SELCHANGE, WM_MOUSELEAVE};
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_SAMPLE: u32 = WM_APP + 2;
const WM_APP_SEED: u32 = WM_APP + 3;

const ID_MENU_SETTINGS: usize = 1;
const ID_MENU_QUIT: usize = 2;

const TIMER_HOUSEKEEPING: usize = 1;
const TIMER_PANEL: usize = 2;
const TIMER_TOOLTIP: usize = 3;

const SAVE_INTERVAL_S: u64 = 300;

/// The graph advances about a pixel every ten seconds, so this is ample to make
/// its motion look continuous while costing almost nothing.
const PANEL_REFRESH_MS: u32 = 500;
/// Hover must be deliberate before a tooltip appears.
const TOOLTIP_DELAY_MS: u32 = 900;

/// Sampling cadence. Faster while the panel is on screen, because that is the
/// only time a fresher reading is worth anything.
const SAMPLE_FOREGROUND_MS: u32 = 1000;
const SAMPLE_BACKGROUND_MS: u32 = 5000;

/// Clicking the tray icon while the panel is open first deactivates the panel,
/// which closes it. Without this grace period the click would immediately
/// reopen it and the icon would never toggle.
const REOPEN_GRACE: Duration = Duration::from_millis(400);

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWCP_ROUND: u32 = 2;

const GUID_CONSOLE_DISPLAY_STATE: GUID = GUID {
    data1: 0x6fe69556,
    data2: 0x704a,
    data3: 0x47a0,
    data4: [0x8f, 0x24, 0xc2, 0x8d, 0x93, 0x6f, 0xda, 0x47],
};

/// Append a line to a trace file when `BATTERY_TRAY_LOG` is set. Tray flyout
/// dismissal depends on the interleaving of activation and tray callback
/// messages, which is far easier to observe than to reason about.
fn trace(msg: &str) {
    use std::io::Write;
    if std::env::var_os("BATTERY_TRAY_LOG").is_none() {
        return;
    }
    let path = store::data_dir().join("trace.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {}", battery_win::now_ms() % 1_000_000, msg);
    }
}

/// Record panics to a file.
///
/// A windowed process has no console, so without this a panic inside a window
/// procedure kills the app with nothing to show for it -- Windows reports only
/// a fault inside whichever system DLL invoked the callback.
fn install_panic_log() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let path = store::data_dir().join("panic.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let where_ = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "unknown".into());
            let _ = writeln!(f, "[{}] {} at {}", battery_win::now_ms(), info, where_);
        }
    }));
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

enum Msg {
    Sample(Sample),
    Info(BatteryInfo),
}

#[derive(Clone, Copy)]
struct SendHwnd(isize);
unsafe impl Send for SendHwnd {}

struct App {
    est: Estimator,
    settings: Settings,
    icons: IconCache,
    fonts: panel::Fonts,
    last: Option<Estimates>,
    health: Health,

    hwnd: HWND,
    panel: HWND,
    settings_hwnd: HWND,
    settings_ui: Option<SettingsWindow>,

    panel_visible: bool,
    panel_hidden_at: Option<Instant>,
    /// Whether the panel window is currently sized to include the graph.
    panel_has_graph: bool,
    /// Pointer position over the time row, awaiting the hover delay.
    hover_pos: Option<(i32, i32)>,
    /// Where the tooltip is currently drawn, if it is showing.
    tooltip_at: Option<(i32, i32)>,

    display_on: Arc<AtomicBool>,
    sample_interval: Arc<AtomicU32>,
    rx: Receiver<Msg>,
    scale: f64,
    last_icon: Option<String>,
    last_save: Instant,
    light_theme: bool,
    seeded_result: Arc<std::sync::Mutex<Option<seed::SeedData>>>,
}

fn app_from(hwnd: HWND) -> Option<&'static mut App> {
    unsafe {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        p.as_mut()
    }
}

fn dpi_scale(hwnd: HWND) -> f64 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi > 0 {
            dpi as f64 / 96.0
        } else {
            1.0
        }
    }
}

// ---------------------------------------------------------------- autostart

fn autostart_key() -> (Vec<u16>, Vec<u16>) {
    (
        wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
        wide("BatteryTray"),
    )
}

fn autostart_enabled() -> bool {
    let (sub, name) = autostart_key();
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) == 0
    }
}

fn set_autostart(on: bool) {
    let (sub, name) = autostart_key();
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_SET_VALUE, &mut key) != 0 {
            return;
        }
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v = wide(&format!("\"{}\"", exe.display()));
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                v.as_ptr() as *const u8,
                (v.len() * 2) as u32,
            );
        } else {
            RegDeleteValueW(key, name.as_ptr());
        }
        RegCloseKey(key);
    }
}

// ---------------------------------------------------------------- tray icon

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid
    }
}

fn tooltip_text(app: &App, est: &Estimates) -> String {
    let pct = app.settings.format_soc(est.soc);
    let head = match est.phase {
        Phase::Charging => format!("{pct}  -  {:.1} W in", est.watts.abs()),
        Phase::Discharging => format!("{pct}  -  {:.1} W out", est.watts.abs()),
        _ => pct,
    };
    let detail = match est.active() {
        Some(p) => format!("{}: {}", est.active_label(), fmt_duration(p.secs)),
        None => est.note.clone().unwrap_or_else(|| "measuring...".into()),
    };
    format!("{head}\r\n{detail}").chars().take(120).collect()
}

fn update_tray(app: &mut App, est: &Estimates) {
    unsafe {
        let spec = IconSpec {
            mode: app.settings.tray_mode,
            size: icon::tray_icon_size(),
            soc: est.soc,
            state: phase_to_glyph(est.phase),
            watts: est.watts,
            secs: est.active().map(|p| p.secs),
            light_taskbar: app.light_theme,
        };
        let hicon = app.icons.get(&spec);
        let key = format!("{hicon:?}");
        let icon_changed = app.last_icon.as_deref() != Some(key.as_str());
        app.last_icon = Some(key);

        let mut nid = notify_data(app.hwnd);
        nid.uFlags = NIF_TIP | NIF_MESSAGE;
        nid.uCallbackMessage = WM_APP_TRAY;
        if icon_changed && !hicon.is_null() {
            nid.uFlags |= NIF_ICON;
            nid.hIcon = hicon;
        }
        let tip = wide(&tooltip_text(app, est));
        let n = tip.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn add_tray(app: &mut App, hwnd: HWND) {
    unsafe {
        let spec = IconSpec {
            mode: app.settings.tray_mode,
            size: icon::tray_icon_size(),
            soc: 1.0,
            state: battery_core::glyph::GlyphState::Unknown,
            watts: 0.0,
            secs: None,
            light_taskbar: app.light_theme,
        };
        let hicon = app.icons.get(&spec);
        let mut nid = notify_data(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = hicon;
        let tip = wide("Battery - starting up");
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(NIM_ADD, &nid);
        app.last_icon = None;
    }
}

// ---------------------------------------------------------------- panel

fn position_panel(app: &App) {
    unsafe {
        let w = (panel::PANEL_W as f64 * app.scale).round() as i32;
        let h = (panel::panel_height(app.panel_has_graph) as f64 * app.scale).round() as i32;

        let mut anchor = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let id = NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: app.hwnd,
            uID: 1,
            guidItem: std::mem::zeroed(),
        };
        let have_anchor = Shell_NotifyIconGetRect(&id, &mut anchor) == 0;

        let mut work = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut RECT as *mut std::ffi::c_void,
            0,
        );

        let margin = (10.0 * app.scale) as i32;
        let (mut x, mut y) = if have_anchor {
            (anchor.left + (anchor.right - anchor.left) / 2 - w / 2, anchor.top - h - margin)
        } else {
            (work.right - w - margin, work.bottom - h - margin)
        };
        x = x.clamp(work.left + margin, (work.right - w - margin).max(work.left + margin));
        y = y.clamp(work.top + margin, (work.bottom - h - margin).max(work.top + margin));

        SetWindowPos(app.panel, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    }
}

fn clear_tooltip(app: &mut App) {
    unsafe {
        KillTimer(app.panel, TIMER_TOOLTIP);
    }
    app.hover_pos = None;
    if app.tooltip_at.take().is_some() {
        unsafe {
            InvalidateRect(app.panel, std::ptr::null(), 0);
        }
    }
}

fn show_panel(app: &mut App) {
    trace("show_panel");
    app.scale = dpi_scale(app.panel);
    if let Some(est) = &app.last {
        let hist: Vec<HistPoint> = app.est.history().iter().copied().collect();
        let _ = est;
        app.panel_has_graph =
            panel::has_graph(&hist, app.settings.graph, battery_win::now_ms());
    }
    position_panel(app);
    unsafe {
        ShowWindow(app.panel, SW_SHOW);
        // Activating the panel is what makes clicking any other window dismiss
        // it, via WM_ACTIVATE below.
        SetForegroundWindow(app.panel);
        SetTimer(app.panel, TIMER_PANEL, PANEL_REFRESH_MS, None);
        InvalidateRect(app.panel, std::ptr::null(), 0);
    }
    app.sample_interval.store(SAMPLE_FOREGROUND_MS, Ordering::Relaxed);
    app.panel_visible = true;
}

fn hide_panel(app: &mut App) {
    trace("hide_panel");
    clear_tooltip(app);
    unsafe {
        KillTimer(app.panel, TIMER_PANEL);
        ShowWindow(app.panel, SW_HIDE);
    }
    app.sample_interval.store(SAMPLE_BACKGROUND_MS, Ordering::Relaxed);
    app.panel_visible = false;
    app.panel_hidden_at = Some(Instant::now());
}

// ---------------------------------------------------------------- settings

fn refresh_settings_readouts(app: &mut App) {
    let training = app.est.model.training();
    let mut health = app.health.clone();
    if let Some(est) = &app.last {
        if est.full_mwh > 0 {
            health.full_wh = est.full_mwh as f64 / 1000.0;
        }
    }
    if let Some(ui) = &app.settings_ui {
        ui.update_readouts(&health, &training);
    }
}

fn show_settings(app: &mut App) {
    unsafe {
        let scale = dpi_scale(app.settings_hwnd);
        let w = (settings_ui::WIN_W as f64 * scale).round() as i32;
        let h = (settings_ui::WIN_H as f64 * scale).round() as i32;
        let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
        AdjustWindowRectEx(&mut r, WS_CAPTION | WS_SYSMENU, 0, 0);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);

        let mut work = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut RECT as *mut std::ffi::c_void,
            0,
        );
        let x = work.left + (work.right - work.left - ww) / 2;
        let y = work.top + (work.bottom - work.top - wh) / 2;
        SetWindowPos(app.settings_hwnd, std::ptr::null_mut(), x, y, ww, wh, SWP_NOZORDER);
    }
    let startup = autostart_enabled();
    if let Some(ui) = &app.settings_ui {
        ui.sync(&app.settings, startup);
    }
    refresh_settings_readouts(app);
    unsafe {
        ShowWindow(app.settings_hwnd, SW_SHOW);
        SetForegroundWindow(app.settings_hwnd);
    }
}

/// Push a settings change through everything that depends on it.
fn settings_changed(app: &mut App) {
    let _ = store::save_settings(&app.settings);
    app.icons.clear();
    app.last_icon = None;
    if let Some(est) = app.last.clone() {
        update_tray(app, &est);
    }
    if let Some(ui) = &app.settings_ui {
        ui.sync(&app.settings, autostart_enabled());
    }
    // The graph kind changes how much history counts as enough to draw.
    let hist: Vec<HistPoint> = app.est.history().iter().copied().collect();
    let has = panel::has_graph(&hist, app.settings.graph, battery_win::now_ms());
    if has != app.panel_has_graph {
        app.panel_has_graph = has;
        if app.panel_visible {
            position_panel(app);
        }
    }
    unsafe {
        InvalidateRect(app.panel, std::ptr::null(), 0);
    }
}

fn apply_action(app: &mut App, action: Action) {
    match action {
        Action::SetTray(m) => {
            if app.settings.tray_mode == m {
                return;
            }
            app.settings.tray_mode = m;
        }
        Action::SetGraph(g) => {
            if app.settings.graph == g {
                return;
            }
            app.settings.graph = g;
        }
        Action::ToggleDecimals => app.settings.decimals = !app.settings.decimals,
        Action::ToggleStartup => {
            set_autostart(!autostart_enabled());
            if let Some(ui) = &app.settings_ui {
                ui.sync(&app.settings, autostart_enabled());
            }
            return;
        }
        Action::ResetLearned => {
            // Learned data only: preferences are not a belief about the
            // battery and should survive.
            app.est.model = battery_core::model::Model::default();
            let _ = store::save_model(&app.est.model);
            seedjob::spawn(app.hwnd, app.seeded_result.clone());
            refresh_settings_readouts(app);
            return;
        }
    }
    settings_changed(app);
}

// ---------------------------------------------------------------- menu

fn show_menu(app: &mut App) {
    unsafe {
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING, ID_MENU_SETTINGS, wide("Settings...").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_MENU_QUIT, wide("Quit").as_ptr());

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(app.hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        match cmd as usize {
            ID_MENU_SETTINGS => show_settings(app),
            ID_MENU_QUIT => {
                PostMessageW(app.hwnd, WM_CLOSE, 0, 0);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- wndprocs

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == taskbar_created_message() {
        if let Some(app) = app_from(hwnd) {
            app.last_icon = None;
            add_tray(app, hwnd);
            if let Some(est) = app.last.clone() {
                update_tray(app, &est);
            }
        }
        return 0;
    }

    match msg {
        WM_APP_TRAY => {
            let Some(app) = app_from(hwnd) else { return 0 };
            match lp as u32 {
                // A second click soon after the first arrives as a double-click
                // rather than another button-up; without this the icon would
                // ignore it entirely.
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    let just_closed = app
                        .panel_hidden_at
                        .is_some_and(|t| t.elapsed() < REOPEN_GRACE);
                    if app.panel_visible {
                        hide_panel(app);
                    } else if !just_closed {
                        show_panel(app);
                    }
                }
                WM_RBUTTONUP => show_menu(app),
                _ => {}
            }
            0
        }
        WM_APP_SAMPLE => {
            if let Some(app) = app_from(hwnd) {
                drain_messages(app);
            }
            0
        }
        WM_APP_SEED => {
            let Some(app) = app_from(hwnd) else { return 0 };
            if let Some(data) = app.seeded_result.lock().ok().and_then(|mut g| g.take()) {
                seed::apply(&mut app.est.model, &data, battery_win::now_ms());
                let _ = store::save_model(&app.est.model);
                refresh_settings_readouts(app);
            }
            0
        }
        WM_TIMER => {
            let Some(app) = app_from(hwnd) else { return 0 };
            if app.last_save.elapsed().as_secs() >= SAVE_INTERVAL_S {
                app.last_save = Instant::now();
                if app.est.take_dirty() {
                    let _ = store::save_model(&app.est.model);
                }
                let hist: Vec<HistPoint> = app.est.history().iter().copied().collect();
                let _ = store::save_history(&hist);
            }
            let light = system_uses_light_theme();
            if light != app.light_theme {
                app.light_theme = light;
                app.icons.clear();
                app.last_icon = None;
                if let Some(est) = app.last.clone() {
                    update_tray(app, &est);
                }
            }
            0
        }
        WM_POWERBROADCAST => {
            if wp as u32 == PBT_POWERSETTINGCHANGE {
                let s = &*(lp as *const POWERBROADCAST_SETTING);
                if s.PowerSetting.data1 == GUID_CONSOLE_DISPLAY_STATE.data1 {
                    let on = *s.Data.as_ptr() != 0;
                    if let Some(app) = app_from(hwnd) {
                        app.display_on.store(on, Ordering::Relaxed);
                    }
                }
            }
            1
        }
        WM_CLOSE | WM_DESTROY | WM_ENDSESSION => {
            if let Some(app) = app_from(hwnd) {
                let _ = store::save_model(&app.est.model);
                let _ = store::save_settings(&app.settings);
                let hist: Vec<HistPoint> = app.est.history().iter().copied().collect();
                let _ = store::save_history(&hist);
                let nid = notify_data(hwnd);
                Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn lparam_point(lp: LPARAM) -> (i32, i32) {
    ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32)
}

unsafe fn track_leave(hwnd: HWND) {
    let mut t = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    TrackMouseEvent(&mut t);
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(app) = app_from(hwnd) {
                let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetClientRect(hwnd, &mut rc);
                let est = app.last.clone().unwrap_or(Estimates {
                    phase: Phase::Unknown,
                    soc: 0.0,
                    capacity_mwh: 0,
                    full_mwh: 0,
                    watts: 0.0,
                    to_empty: None,
                    to_80: None,
                    to_full: None,
                    note: None,
                    confidence: 0.0,
                });
                let hist: Vec<HistPoint> = app.est.history().iter().copied().collect();
                panel::render(
                    hdc,
                    rc.right,
                    rc.bottom,
                    app.scale,
                    &app.fonts,
                    &est,
                    &hist,
                    &app.settings,
                    battery_win::now_ms(),
                    app.tooltip_at,
                );
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_TIMER => {
            let Some(app) = app_from(hwnd) else { return 0 };
            match wp {
                // Repaint so the graph keeps scrolling even between samples.
                TIMER_PANEL => {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                TIMER_TOOLTIP => {
                    KillTimer(hwnd, TIMER_TOOLTIP);
                    if let Some(pos) = app.hover_pos {
                        app.tooltip_at = Some(pos);
                        InvalidateRect(hwnd, std::ptr::null(), 0);
                    }
                }
                _ => {}
            }
            0
        }
        WM_MOUSEMOVE => {
            if let Some(app) = app_from(hwnd) {
                let (x, y) = lparam_point(lp);
                let r = panel::time_row_rect(app.scale);
                let over = x >= r.left && x < r.right && y >= r.top && y < r.bottom;
                // Any movement dismisses a showing tooltip and restarts the
                // dwell timer, so it only appears when the pointer settles.
                if app.tooltip_at.take().is_some() {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                KillTimer(hwnd, TIMER_TOOLTIP);
                if over {
                    app.hover_pos = Some((x, y));
                    SetTimer(hwnd, TIMER_TOOLTIP, TOOLTIP_DELAY_MS, None);
                } else {
                    app.hover_pos = None;
                }
                track_leave(hwnd);
            }
            0
        }
        WM_MOUSELEAVE => {
            if let Some(app) = app_from(hwnd) {
                clear_tooltip(app);
            }
            0
        }
        WM_LBUTTONDOWN => 0,
        WM_LBUTTONUP => 0,
        WM_ACTIVATE => {
            trace(&format!("panel WM_ACTIVATE wp={}", wp & 0xFFFF));
            if (wp & 0xFFFF) as u32 == WA_INACTIVE {
                if let Some(app) = app_from(hwnd) {
                    hide_panel(app);
                }
            }
            0
        }
        WM_ERASEBKGND => 1,
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn settings_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let code = ((wp >> 16) & 0xFFFF) as u32;
            let id = (wp & 0xFFFF) as u16;
            if code == BN_CLICKED {
                if let (Some(app), Some(action)) = (app_from(hwnd), settings_ui::action_for(id)) {
                    apply_action(app, action);
                }
            }
            0
        }
        WM_NOTIFY => {
            let hdr = &*(lp as *const NMHDR);
            if hdr.code == TCN_SELCHANGE {
                if let Some(app) = app_from(hwnd) {
                    if let Some(ui) = &mut app.settings_ui {
                        let sel = ui.selected_tab();
                        ui.select_tab(sel);
                    }
                }
            }
            0
        }
        // Keep the window alive so reopening is instant.
        WM_CLOSE => {
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn taskbar_created_message() -> u32 {
    use std::sync::OnceLock;
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) })
}

fn drain_messages(app: &mut App) {
    let mut latest = None;
    while let Ok(m) = app.rx.try_recv() {
        match m {
            Msg::Sample(s) => latest = Some(app.est.update(s)),
            Msg::Info(i) => {
                app.health = Health {
                    full_wh: i.full_mwh as f64 / 1000.0,
                    design_wh: i.designed_mwh as f64 / 1000.0,
                    cycles: i.cycle_count,
                    chemistry: i.chemistry.clone(),
                };
            }
        }
    }
    let Some(est) = latest else { return };
    update_tray(app, &est);
    app.last = Some(est);

    // The window shrinks when there is no graph worth showing, rather than
    // leaving an empty band.
    let hist: Vec<HistPoint> = app.est.history().iter().copied().collect();
    let has = panel::has_graph(&hist, app.settings.graph, battery_win::now_ms());
    if has != app.panel_has_graph {
        app.panel_has_graph = has;
        if app.panel_visible {
            position_panel(app);
        }
    }

    unsafe {
        if app.panel_visible {
            InvalidateRect(app.panel, std::ptr::null(), 0);
        }
        if IsWindowVisible(app.settings_hwnd) != 0 {
            refresh_settings_readouts(app);
        }
    }
}

// ---------------------------------------------------------------- startup

fn spawn_sampler(
    hwnd: SendHwnd,
    tx: Sender<Msg>,
    display_on: Arc<AtomicBool>,
    interval: Arc<AtomicU32>,
) {
    std::thread::spawn(move || {
        let Some(mut sampler) = Sampler::new() else {
            return;
        };
        let _ = tx.send(Msg::Info(sampler.info().clone()));
        unsafe { PostMessageW(hwnd.0 as HWND, WM_APP_SAMPLE, 0, 0) };
        loop {
            let wait = interval.load(Ordering::Relaxed);
            let Some(s) = sampler.next_sample(wait, display_on.load(Ordering::Relaxed)) else {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            };
            if tx.send(Msg::Sample(s)).is_err() {
                return;
            }
            unsafe { PostMessageW(hwnd.0 as HWND, WM_APP_SAMPLE, 0, 0) };
        }
    });
}

unsafe fn register_class(name: &[u16], proc: WNDPROC, hinst: HMODULE, cursor: bool, bg: i32) {
    let mut wc: WNDCLASSW = std::mem::zeroed();
    wc.lpfnWndProc = proc;
    wc.hInstance = hinst;
    wc.lpszClassName = name.as_ptr();
    if cursor {
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
    }
    if bg >= 0 {
        wc.hbrBackground = (bg + 1) as HBRUSH;
    }
    RegisterClassW(&wc);
}

fn main() {
    unsafe {
        // The manifest already declares per-monitor v2; this is a no-op then,
        // and a fallback if the manifest failed to embed.
        install_panic_log();
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let mutex = CreateMutexW(std::ptr::null(), 1, wide("BatteryTraySingleton").as_ptr());
        if mutex.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        let hinst = GetModuleHandleW(std::ptr::null());
        let main_class = wide("BatteryTrayWnd");
        let panel_class = wide("BatteryTrayPanel");
        let settings_class = wide("BatteryTraySettings");
        register_class(&main_class, Some(wnd_proc), hinst, false, -1);
        register_class(&panel_class, Some(panel_proc), hinst, true, -1);
        register_class(
            &settings_class,
            Some(settings_proc),
            hinst,
            true,
            COLOR_WINDOW as i32,
        );

        let hwnd = CreateWindowExW(
            0,
            main_class.as_ptr(),
            wide("Battery Time Remaining").as_ptr(),
            WS_OVERLAPPED,
            0, 0, 0, 0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }

        let panel_hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            panel_class.as_ptr(),
            wide("Battery").as_ptr(),
            WS_POPUP,
            0, 0, 100, 100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        let round: u32 = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            panel_hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &round as *const u32 as *const _,
            4,
        );
        let dark: u32 = 1;
        DwmSetWindowAttribute(
            panel_hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const u32 as *const _,
            4,
        );

        let settings_hwnd = CreateWindowExW(
            0,
            settings_class.as_ptr(),
            wide("Battery Settings").as_ptr(),
            WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT, CW_USEDEFAULT, 400, 500,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );

        let big = icon::window_icon(32);
        let small = icon::window_icon(16);
        for w in [hwnd, settings_hwnd] {
            SendMessageW(w, WM_SETICON, ICON_BIG as usize, big as isize);
            SendMessageW(w, WM_SETICON, ICON_SMALL as usize, small as isize);
        }

        let scale = dpi_scale(hwnd);
        let (tx, rx) = channel::<Msg>();
        let display_on = Arc::new(AtomicBool::new(true));
        let sample_interval = Arc::new(AtomicU32::new(SAMPLE_BACKGROUND_MS));

        let mut est = Estimator::new(store::load_model());
        est.load_history(store::load_history());

        let app = Box::new(App {
            est,
            settings: store::load_settings(),
            icons: IconCache::default(),
            fonts: panel::Fonts::new(scale),
            last: None,
            health: Health::default(),
            hwnd,
            panel: panel_hwnd,
            settings_hwnd,
            settings_ui: None,
            panel_visible: false,
            panel_hidden_at: None,
            panel_has_graph: true,
            hover_pos: None,
            tooltip_at: None,
            display_on: display_on.clone(),
            sample_interval: sample_interval.clone(),
            rx,
            scale,
            last_icon: None,
            last_save: Instant::now(),
            light_theme: system_uses_light_theme(),
            seeded_result: Arc::new(std::sync::Mutex::new(None)),
        });
        let app_ptr = Box::into_raw(app);
        for w in [hwnd, panel_hwnd, settings_hwnd] {
            SetWindowLongPtrW(w, GWLP_USERDATA, app_ptr as isize);
        }
        let app = &mut *app_ptr;
        app.settings_ui = Some(SettingsWindow::create(
            settings_hwnd,
            hinst,
            dpi_scale(settings_hwnd),
        ));

        add_tray(app, hwnd);
        RegisterPowerSettingNotification(
            hwnd as HANDLE,
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        SetTimer(hwnd, TIMER_HOUSEKEEPING, 30_000, None);

        if !app.est.model.seeded {
            seedjob::spawn(hwnd, app.seeded_result.clone());
        }
        spawn_sampler(SendHwnd(hwnd as isize), tx, display_on, sample_interval);

        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--show-panel") {
            show_panel(app);
        }
        if args.iter().any(|a| a == "--settings") {
            show_settings(app);
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            // Lets Tab and arrow keys move between the settings controls.
            if IsWindowVisible(settings_hwnd) != 0 && IsDialogMessageW(settings_hwnd, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
