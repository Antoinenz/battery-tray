//! The settings window: standard Windows controls on a tab strip.
//!
//! Deliberately plain. The panel is custom-drawn because it has to be — it is a
//! chart — but settings are a form, and a form built from real `BUTTON`,
//! `STATIC`, `COMBOBOX` and `SysTabControl32` controls inherits correct
//! theming, keyboard navigation, focus rings, high-contrast modes and
//! screen-reader support for free. The manifest in `build.rs` pulls in Common
//! Controls v6 so these render themed rather than in the Windows 95 style.

use battery_core::model::Training;
use battery_core::settings::{GraphKind, PanelTheme, Settings, TrayMode, ALERT_LEVELS};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// Static-control alignment styles, which windows-sys does not surface.
const SS_LEFT: u32 = 0x0000;
const SS_RIGHT: u32 = 0x0002;

/// Client size in logical pixels.
pub const WIN_W: i32 = 400;
pub const WIN_H: i32 = 512;

pub const ID_CHK_STARTUP: u16 = 100;
pub const ID_CHK_DECIMALS: u16 = 101;
pub const ID_CHK_PIN: u16 = 102;
pub const ID_TRAY_BASE: u16 = 110;
pub const ID_GRAPH_BASE: u16 = 120;
pub const ID_BTN_RESET: u16 = 130;
pub const ID_THEME_BASE: u16 = 140;
pub const ID_CHK_ALERT_LOW: u16 = 150;
pub const ID_CHK_ALERT_CRITICAL: u16 = 151;
pub const ID_CHK_ALERT_80: u16 = 152;
pub const ID_CHK_ALERT_FULL: u16 = 153;
pub const ID_CBO_LOW: u16 = 160;
pub const ID_CBO_CRITICAL: u16 = 161;
pub const ID_CHK_SHOW_GRAPH: u16 = 170;
pub const ID_CHK_ZERO_LINE: u16 = 171;
pub const ID_CHK_AUTOFIT: u16 = 172;
pub const ID_BTN_GITHUB: u16 = 180;

const TABS: [&str; 6] = ["General", "Display", "Alerts", "Battery", "Learning", "About"];

/// The name the app goes by, and where it lives.
pub const APP_NAME: &str = "BatteryTray";
pub const PROJECT_URL: &str = "https://github.com/Antoinenz/battery-tray";

/// Set by `build.rs`: the release tag for a build made from one, otherwise the
/// crate version and the commit it came from.
pub const APP_VERSION: &str = env!("BUILD_VERSION");
/// Either `release` or `dev`. See `build.rs`.
pub const BUILD_CHANNEL: &str = env!("BUILD_CHANNEL");

/// What the About page calls this build.
fn build_label() -> &'static str {
    match BUILD_CHANNEL {
        "release" => "Release",
        _ => "Development",
    }
}

/// Battery facts shown in the Battery tab, already converted for display.
#[derive(Clone, Debug, Default)]
pub struct Health {
    pub full_wh: f64,
    pub design_wh: f64,
    pub cycles: u32,
    pub chemistry: String,
}

impl Health {
    pub fn health_pct(&self) -> Option<f64> {
        (self.design_wh > 0.0 && self.full_wh > 0.0).then(|| self.full_wh / self.design_wh * 100.0)
    }
}

/// What a click on a control means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SetTray(TrayMode),
    SetGraph(GraphKind),
    SetTheme(PanelTheme),
    ToggleDecimals,
    ToggleStartup,
    TogglePin,
    ToggleAlertLow,
    ToggleAlertCritical,
    ToggleAlert80,
    ToggleAlertFull,
    SetLowLevel(u8),
    SetCriticalLevel(u8),
    ToggleShowGraph,
    ToggleZeroLine,
    ToggleAutofit,
    ResetLearned,
    OpenProjectPage,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

static ORIG_TAB_PROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static PAGE_BRUSH: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// A brush matching the colour the theme actually paints the tab page with.
///
/// There is no reliable way to *ask* for this: `EnableThemeDialogTexture` only
/// works for real dialogs, and the tab body's fill colour is not exposed as a
/// theme property. So the page is sampled once, after it has been painted,
/// which is correct under any theme including dark mode. Measured on Windows 11
/// the page is #F9F9F9 while the default static brush is #F0F0F0 — close
/// enough to look like a mistake rather than a difference.
unsafe fn page_brush(tab: HWND) -> HBRUSH {
    let cached = PAGE_BRUSH.load(std::sync::atomic::Ordering::Relaxed);
    if cached != 0 {
        return cached as HBRUSH;
    }
    let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetClientRect(tab, &mut rc);
    let dc = GetDC(tab);
    // Just inside the page border, to the left of every control.
    let sampled = GetPixel(dc, 6, (rc.bottom - rc.top) / 2);
    ReleaseDC(tab, dc);
    if sampled == CLR_INVALID {
        // Not painted yet: use a sane colour now and sample again next time.
        return GetSysColorBrush(COLOR_WINDOW);
    }
    let brush = CreateSolidBrush(sampled);
    PAGE_BRUSH.store(brush as isize, std::sync::atomic::Ordering::Relaxed);
    brush
}

fn forget_page_brush() {
    let old = PAGE_BRUSH.swap(0, std::sync::atomic::Ordering::Relaxed);
    if old != 0 {
        unsafe {
            DeleteObject(old as HGDIOBJ);
        }
    }
}

/// Tab-control window procedure replacement.
///
/// The page controls are children of the tab control so that they sit on its
/// page rather than on the parent window. Two consequences are handled here:
/// their `WM_COMMAND` notifications must be passed on to the settings window,
/// and their backgrounds must be painted to match the page.
unsafe extern "system" fn tab_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => return SendMessageW(GetParent(hwnd), msg, wp, lp),
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            SetBkMode(wp as HDC, TRANSPARENT as i32);
            return page_brush(hwnd) as LRESULT;
        }
        WM_THEMECHANGED | WM_SYSCOLORCHANGE => forget_page_brush(),
        _ => {}
    }
    let orig = ORIG_TAB_PROC.load(std::sync::atomic::Ordering::Relaxed);
    CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(orig), hwnd, msg, wp, lp)
}

/// All the control handles, grouped by the tab page they belong to.
pub struct SettingsWindow {
    pub hwnd: HWND,
    tab: HWND,
    font: HFONT,
    /// A heavier face for the About page's heading. Kept alongside `font` so
    /// both are destroyed together.
    title_font: HFONT,
    pages: [Vec<HWND>; 6],
    chk_startup: HWND,
    chk_decimals: HWND,
    chk_pin: HWND,
    tray_radios: Vec<HWND>,
    graph_radios: Vec<HWND>,
    theme_radios: Vec<HWND>,
    chk_alert_low: HWND,
    chk_alert_critical: HWND,
    chk_alert_80: HWND,
    chk_alert_full: HWND,
    chk_show_graph: HWND,
    chk_zero_line: HWND,
    chk_autofit: HWND,
    cbo_low: HWND,
    cbo_critical: HWND,
    battery_values: Vec<HWND>,
    learning_values: Vec<HWND>,
    current: usize,
}

struct Builder {
    parent: HWND,
    hinst: HMODULE,
    font: HFONT,
    scale: f64,
    /// Controls are children of the tab control, so positions written in
    /// window coordinates are shifted by the tab's own origin.
    origin: (i32, i32),
}

impl Builder {
    fn px(&self, v: i32) -> i32 {
        (v as f64 * self.scale).round() as i32
    }

    #[allow(clippy::too_many_arguments)]
    fn make(&self, class: &str, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: u16) -> HWND {
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | style,
                self.px(x - self.origin.0),
                self.px(y - self.origin.1),
                self.px(w),
                self.px(h),
                self.parent,
                id as usize as HMENU,
                self.hinst,
                std::ptr::null(),
            );
            SendMessageW(hwnd, WM_SETFONT, self.font as usize, 1);
            hwnd
        }
    }

    fn label(&self, text: &str, x: i32, y: i32, w: i32) -> HWND {
        self.make("STATIC", text, SS_LEFT, x, y, w, 18, 0)
    }
    /// A label in a different face, for the About heading.
    fn label_in(&self, text: &str, x: i32, y: i32, w: i32, h: i32, font: HFONT) -> HWND {
        let hwnd = self.make("STATIC", text, SS_LEFT, x, y, w, h, 0);
        unsafe { SendMessageW(hwnd, WM_SETFONT, font as usize, 1) };
        hwnd
    }
    fn value(&self, x: i32, y: i32, w: i32) -> HWND {
        self.make("STATIC", "", SS_RIGHT, x, y, w, 18, 0)
    }
    fn group(&self, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
        self.make("BUTTON", text, BS_GROUPBOX as u32, x, y, w, h, 0)
    }
    fn checkbox(&self, text: &str, x: i32, y: i32, w: i32, id: u16) -> HWND {
        self.make("BUTTON", text, (BS_AUTOCHECKBOX as u32) | WS_TABSTOP, x, y, w, 22, id)
    }
    fn radio(&self, text: &str, x: i32, y: i32, w: i32, id: u16, first: bool) -> HWND {
        let mut style = (BS_AUTORADIOBUTTON as u32) | WS_TABSTOP;
        if first {
            // WS_GROUP starts a new radio group, so arrow keys move within one
            // set of options rather than across all of them.
            style |= WS_GROUP;
        }
        self.make("BUTTON", text, style, x, y, w, 20, id)
    }
    fn button(&self, text: &str, x: i32, y: i32, w: i32, h: i32, id: u16) -> HWND {
        self.make("BUTTON", text, (BS_PUSHBUTTON as u32) | WS_TABSTOP, x, y, w, h, id)
    }
    /// A percentage picker. The height given to a combo box is the height of
    /// its dropped list, not the closed control.
    fn level_combo(&self, x: i32, y: i32, id: u16, selected: u8) -> HWND {
        let cbo = self.make(
            "COMBOBOX",
            "",
            (CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
            x, y, 84, 200, id,
        );
        unsafe {
            for (i, lvl) in ALERT_LEVELS.iter().enumerate() {
                let item = wide(&format!("{lvl}%"));
                SendMessageW(cbo, CB_ADDSTRING, 0, item.as_ptr() as isize);
                if *lvl == selected {
                    SendMessageW(cbo, CB_SETCURSEL, i, 0);
                }
            }
        }
        cbo
    }
}

const BATTERY_ROWS: [&str; 5] = [
    "Full charge capacity",
    "Design capacity",
    "Health",
    "Charge cycles",
    "Chemistry",
];
/// Worth having to hand when something has gone wrong: what the app runs as,
/// where it keeps its state, and which kind of build it is.
fn about_rows() -> [(&'static str, &'static str); 4] {
    [
        ("Runs as", "battery-tray.exe"),
        ("Settings and data", "%LOCALAPPDATA%\\BatteryTray"),
        ("Build", build_label()),
        ("Licence", "MIT"),
    ]
}
const LEARNING_ROWS: [&str; 6] = [
    "Seeded from Windows history",
    "Charge curve learned",
    "Charger output",
    "Usage profiles learned",
    "Typical active draw",
    "Predictions graded",
];

impl SettingsWindow {
    pub fn create(hwnd: HWND, hinst: HMODULE, scale: f64, settings: &Settings) -> SettingsWindow {
        unsafe {
            let mut icc = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_TAB_CLASSES | ICC_STANDARD_CLASSES,
            };
            InitCommonControlsEx(&mut icc);
        }

        // 9pt Segoe UI is the standard Windows UI face; matching it is most of
        // what makes a window look native.
        let font = unsafe {
            CreateFontW(
                -((9.0 * scale * 96.0 / 72.0).round() as i32),
                0, 0, 0, FW_NORMAL as i32, 0, 0, 0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                wide("Segoe UI").as_ptr(),
            )
        };

        // The About heading, in the same face a step larger and semibold --
        // which is how Windows' own About pages set a product name.
        let title_font = unsafe {
            CreateFontW(
                -((15.0 * scale * 96.0 / 72.0).round() as i32),
                0, 0, 0, FW_SEMIBOLD as i32, 0, 0, 0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                wide("Segoe UI").as_ptr(),
            )
        };

        let outer = Builder { parent: hwnd, hinst, font, scale, origin: (0, 0) };
        let tab = outer.make(
            "SysTabControl32",
            "",
            WS_CLIPSIBLINGS | WS_TABSTOP,
            8, 8, WIN_W - 16, WIN_H - 16,
            0,
        );
        unsafe {
            ShowWindow(tab, SW_SHOW);
            for (i, name) in TABS.iter().enumerate() {
                let text = wide(name);
                let item = TCITEMW {
                    mask: TCIF_TEXT,
                    dwState: 0,
                    dwStateMask: 0,
                    pszText: text.as_ptr() as *mut u16,
                    cchTextMax: 0,
                    iImage: -1,
                    lParam: 0,
                };
                SendMessageW(tab, TCM_INSERTITEMW, i, &item as *const TCITEMW as isize);
            }
            let orig = SetWindowLongPtrW(tab, GWLP_WNDPROC, tab_proc as *const () as isize);
            ORIG_TAB_PROC.store(orig, std::sync::atomic::Ordering::Relaxed);
        }

        // Everything below is a child of the tab control.
        let b = Builder { parent: tab, hinst, font, scale, origin: (8, 8) };

        // Content origin, in window coordinates.
        let (cx, cy) = (28, 56);
        let row_w = 210;
        let val_w = 120;
        let val_x = cx + row_w + 10;
        let wide_w = 336;

        // --- General
        let chk_startup = b.checkbox("Start with Windows", cx, cy, wide_w, ID_CHK_STARTUP);
        let chk_decimals = b.checkbox(
            "Show decimal places in battery percentage",
            cx, cy + 32, wide_w, ID_CHK_DECIMALS,
        );
        let decimals_note = b.label(
            "The charge gauge moves in steps far smaller than 1%.",
            cx + 22, cy + 56, wide_w,
        );
        let chk_pin = b.checkbox("Keep the panel open until dismissed", cx, cy + 90, wide_w, ID_CHK_PIN);
        let pin_note = b.label(
            "Drag anywhere on the panel to move it.",
            cx + 22, cy + 114, wide_w,
        );
        let general = vec![chk_startup, chk_decimals, decimals_note, chk_pin, pin_note];

        // --- Display
        let mut display = Vec::new();
        let tray_h = 26 + TrayMode::ALL.len() as i32 * 24;
        display.push(b.group("Tray icon", cx - 8, cy - 8, wide_w, tray_h));
        let mut tray_radios = Vec::new();
        for (i, m) in TrayMode::ALL.iter().enumerate() {
            let r = b.radio(m.label(), cx + 8, cy + 14 + i as i32 * 24, 300, ID_TRAY_BASE + i as u16, i == 0);
            tray_radios.push(r);
            display.push(r);
        }
        let gy = cy - 8 + tray_h + 12;
        let graph_h = 26 + GraphKind::ALL.len() as i32 * 24 + 3 * 26;
        display.push(b.group("Graph", cx - 8, gy, wide_w, graph_h));
        let mut graph_radios = Vec::new();
        for (i, g) in GraphKind::ALL.iter().enumerate() {
            let r = b.radio(g.label(), cx + 8, gy + 22 + i as i32 * 24, 300, ID_GRAPH_BASE + i as u16, i == 0);
            graph_radios.push(r);
            display.push(r);
        }
        let chk_show_graph = b.checkbox("Show the graph", cx + 6, gy + 72, 300, ID_CHK_SHOW_GRAPH);
        let chk_zero_line =
            b.checkbox("Show the zero line", cx + 6, gy + 98, 300, ID_CHK_ZERO_LINE);
        let chk_autofit = b.checkbox(
            "Fit to one direction when nothing opposes it",
            cx + 6, gy + 124, 300, ID_CHK_AUTOFIT,
        );
        display.push(chk_show_graph);
        display.push(chk_zero_line);
        display.push(chk_autofit);
        let ty = gy + graph_h + 12;
        let theme_h = 26 + PanelTheme::ALL.len() as i32 * 24;
        display.push(b.group("Panel theme", cx - 8, ty, wide_w, theme_h));
        let mut theme_radios = Vec::new();
        for (i, t) in PanelTheme::ALL.iter().enumerate() {
            let r = b.radio(t.label(), cx + 8, ty + 22 + i as i32 * 24, 300, ID_THEME_BASE + i as u16, i == 0);
            theme_radios.push(r);
            display.push(r);
        }

        // --- Alerts
        let chk_alert_low = b.checkbox("Warn when the battery is low", cx, cy, 250, ID_CHK_ALERT_LOW);
        let cbo_low = b.level_combo(cx + 258, cy, ID_CBO_LOW, settings.alert_low_pct);
        let chk_alert_critical =
            b.checkbox("Warn again when critical", cx, cy + 34, 250, ID_CHK_ALERT_CRITICAL);
        let cbo_critical =
            b.level_combo(cx + 258, cy + 34, ID_CBO_CRITICAL, settings.alert_critical_pct);
        let chk_alert_80 =
            b.checkbox("Notify when charging reaches 80%", cx, cy + 78, wide_w, ID_CHK_ALERT_80);
        let alert_80_note = b.label(
            "Stopping near 80% is easier on the battery.",
            cx + 22, cy + 102, wide_w,
        );
        let chk_alert_full =
            b.checkbox("Notify when fully charged", cx, cy + 132, wide_w, ID_CHK_ALERT_FULL);
        let alerts = vec![
            chk_alert_low,
            cbo_low,
            chk_alert_critical,
            cbo_critical,
            chk_alert_80,
            alert_80_note,
            chk_alert_full,
        ];

        // --- Battery
        let mut battery = Vec::new();
        let mut battery_values = Vec::new();
        for (i, name) in BATTERY_ROWS.iter().enumerate() {
            let y = cy + i as i32 * 26;
            battery.push(b.label(name, cx, y, row_w));
            let v = b.value(val_x, y, val_w);
            battery_values.push(v);
            battery.push(v);
        }

        // --- Learning
        let mut learning = Vec::new();
        let mut learning_values = Vec::new();
        for (i, name) in LEARNING_ROWS.iter().enumerate() {
            let y = cy + i as i32 * 26;
            learning.push(b.label(name, cx, y, row_w));
            let v = b.value(val_x, y, val_w);
            learning_values.push(v);
            learning.push(v);
        }
        let reset_y = cy + LEARNING_ROWS.len() as i32 * 26 + 18;
        learning.push(b.label("Clears learned data and re-reads Windows history.", cx, reset_y, wide_w));
        learning.push(b.button("Reset learned data", cx, reset_y + 22, 160, 30, ID_BTN_RESET));

        // --- About
        let mut about = vec![
            b.label_in(APP_NAME, cx, cy - 6, wide_w, 28, title_font),
            b.label(&format!("Version {APP_VERSION}"), cx, cy + 24, wide_w),
            b.label(
                "Predicts how long the battery has left by",
                cx, cy + 58, wide_w,
            ),
            b.label(
                "learning how this machine actually behaves.",
                cx, cy + 76, wide_w,
            ),
        ];
        let rows = about_rows();
        for (i, (name, value)) in rows.iter().enumerate() {
            let y = cy + 112 + i as i32 * 24;
            about.push(b.label(name, cx, y, 108));
            about.push(b.label(value, cx + 112, y, wide_w - 112));
        }
        about.push(b.button(
            "View on GitHub",
            cx, cy + 112 + rows.len() as i32 * 24 + 20,
            150, 30, ID_BTN_GITHUB,
        ));

        let mut w = SettingsWindow {
            hwnd,
            tab,
            font,
            title_font,
            pages: [general, display, alerts, battery, learning, about],
            chk_startup,
            chk_decimals,
            chk_pin,
            tray_radios,
            graph_radios,
            theme_radios,
            chk_alert_low,
            chk_alert_critical,
            chk_alert_80,
            chk_alert_full,
            chk_show_graph,
            chk_zero_line,
            chk_autofit,
            cbo_low,
            cbo_critical,
            battery_values,
            learning_values,
            current: usize::MAX,
        };
        w.select_tab(0);
        w
    }

    /// Map a control notification to the change it represents.
    ///
    /// Combo boxes are read here rather than by the caller, since the selected
    /// index only means something alongside the list it came from.
    pub fn action_for(&self, id: u16, code: u32) -> Option<Action> {
        if code == CBN_SELCHANGE {
            let level = |h: HWND| -> Option<u8> {
                let i = unsafe { SendMessageW(h, CB_GETCURSEL, 0, 0) };
                (i >= 0).then(|| ALERT_LEVELS.get(i as usize).copied()).flatten()
            };
            return match id {
                ID_CBO_LOW => level(self.cbo_low).map(Action::SetLowLevel),
                ID_CBO_CRITICAL => level(self.cbo_critical).map(Action::SetCriticalLevel),
                _ => None,
            };
        }
        if code != BN_CLICKED {
            return None;
        }
        match id {
            ID_CHK_STARTUP => Some(Action::ToggleStartup),
            ID_CHK_DECIMALS => Some(Action::ToggleDecimals),
            ID_CHK_PIN => Some(Action::TogglePin),
            ID_CHK_ALERT_LOW => Some(Action::ToggleAlertLow),
            ID_CHK_ALERT_CRITICAL => Some(Action::ToggleAlertCritical),
            ID_CHK_ALERT_80 => Some(Action::ToggleAlert80),
            ID_CHK_ALERT_FULL => Some(Action::ToggleAlertFull),
            ID_CHK_SHOW_GRAPH => Some(Action::ToggleShowGraph),
            ID_CHK_ZERO_LINE => Some(Action::ToggleZeroLine),
            ID_CHK_AUTOFIT => Some(Action::ToggleAutofit),
            ID_BTN_RESET => Some(Action::ResetLearned),
            ID_BTN_GITHUB => Some(Action::OpenProjectPage),
            _ => {
                if (ID_TRAY_BASE..ID_TRAY_BASE + TrayMode::ALL.len() as u16).contains(&id) {
                    Some(Action::SetTray(TrayMode::ALL[(id - ID_TRAY_BASE) as usize]))
                } else if (ID_GRAPH_BASE..ID_GRAPH_BASE + GraphKind::ALL.len() as u16).contains(&id) {
                    Some(Action::SetGraph(GraphKind::ALL[(id - ID_GRAPH_BASE) as usize]))
                } else if (ID_THEME_BASE..ID_THEME_BASE + PanelTheme::ALL.len() as u16).contains(&id)
                {
                    Some(Action::SetTheme(PanelTheme::ALL[(id - ID_THEME_BASE) as usize]))
                } else {
                    None
                }
            }
        }
    }

    /// Show only the requested page's controls.
    pub fn select_tab(&mut self, index: usize) {
        if index == self.current || index >= self.pages.len() {
            return;
        }
        unsafe {
            for (i, page) in self.pages.iter().enumerate() {
                let cmd = if i == index { SW_SHOW } else { SW_HIDE };
                for &c in page {
                    ShowWindow(c, cmd);
                }
            }
            InvalidateRect(self.hwnd, std::ptr::null(), 1);
        }
        self.current = index;
    }

    pub fn selected_tab(&self) -> usize {
        unsafe { SendMessageW(self.tab, TCM_GETCURSEL, 0, 0) as usize }
    }

    /// Push current values into the controls.
    pub fn sync(&self, s: &Settings, startup: bool) {
        unsafe {
            let check = |h: HWND, on: bool| {
                SendMessageW(h, BM_SETCHECK, usize::from(on), 0);
            };
            check(self.chk_startup, startup);
            check(self.chk_decimals, s.decimals);
            check(self.chk_pin, s.pin_panel);
            check(self.chk_alert_low, s.alert_low);
            check(self.chk_alert_critical, s.alert_critical);
            check(self.chk_alert_80, s.alert_80);
            check(self.chk_alert_full, s.alert_full);
            check(self.chk_show_graph, s.show_graph);
            check(self.chk_zero_line, s.graph_zero_line);
            check(self.chk_autofit, s.graph_autofit);
            for (i, m) in TrayMode::ALL.iter().enumerate() {
                check(self.tray_radios[i], s.tray_mode == *m);
            }
            for (i, g) in GraphKind::ALL.iter().enumerate() {
                check(self.graph_radios[i], s.graph == *g);
            }
            for (i, t) in PanelTheme::ALL.iter().enumerate() {
                check(self.theme_radios[i], s.theme == *t);
            }
            let select = |h: HWND, level: u8| {
                if let Some(i) = ALERT_LEVELS.iter().position(|l| *l == level) {
                    SendMessageW(h, CB_SETCURSEL, i, 0);
                }
            };
            select(self.cbo_low, s.alert_low_pct);
            select(self.cbo_critical, s.alert_critical_pct);
            // A disabled alert has no level to pick.
            EnableWindow(self.cbo_low, i32::from(s.alert_low));
            EnableWindow(self.cbo_critical, i32::from(s.alert_critical));
            // Nothing under the graph matters when there is no graph.
            for h in [self.chk_zero_line, self.chk_autofit] {
                EnableWindow(h, i32::from(s.show_graph));
            }
            for h in self.graph_radios.iter() {
                EnableWindow(*h, i32::from(s.show_graph));
            }
        }
    }

    fn set_text(h: HWND, s: &str) {
        unsafe {
            SetWindowTextW(h, wide(s).as_ptr());
        }
    }

    pub fn update_readouts(&self, health: &Health, t: &Training) {
        let dash = || "--".to_string();
        let battery = [
            format!("{:.1} Wh", health.full_wh),
            format!("{:.1} Wh", health.design_wh),
            health.health_pct().map(|p| format!("{p:.0}%")).unwrap_or_else(dash),
            health.cycles.to_string(),
            if health.chemistry.is_empty() { dash() } else { health.chemistry.clone() },
        ];
        for (h, v) in self.battery_values.iter().zip(battery.iter()) {
            Self::set_text(*h, v);
        }

        let learning = [
            if t.seeded { "Yes".into() } else { "No".into() },
            format!("{:.0}%", t.curve_maturity * 100.0),
            t.charger_peak_w.map(|w| format!("{w:.1} W")).unwrap_or_else(dash),
            format!("{} of {}", t.contexts_learned, t.context_total),
            t.active_draw_w.map(|w| format!("{w:.1} W")).unwrap_or_else(dash),
            if t.graded_predictions < 1.0 {
                "none yet".into()
            } else {
                match t.typical_error {
                    Some(e) => format!("{:.0}  (±{:.0}%)", t.graded_predictions, e * 100.0),
                    None => format!("{:.0}", t.graded_predictions),
                }
            },
        ];
        for (h, v) in self.learning_values.iter().zip(learning.iter()) {
            Self::set_text(*h, v);
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.font as HGDIOBJ);
            DeleteObject(self.title_font as HGDIOBJ);
        }
    }
}
