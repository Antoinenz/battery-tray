//! The settings window: standard Windows controls on a tab strip.
//!
//! Deliberately plain. The panel is custom-drawn because it has to be — it is a
//! chart — but settings are a form, and a form built from real `BUTTON`,
//! `STATIC` and `SysTabControl32` controls inherits correct theming, keyboard
//! navigation, focus rings, high-contrast modes and screen-reader support for
//! free. The manifest in `build.rs` pulls in Common Controls v6 so these render
//! themed rather than in the Windows 95 style.

use battery_core::model::Training;
use battery_core::settings::{GraphKind, Settings, TrayMode};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// Static-control alignment styles, which windows-sys does not surface.
const SS_LEFT: u32 = 0x0000;
const SS_RIGHT: u32 = 0x0002;

/// Client size in logical pixels.
pub const WIN_W: i32 = 392;
pub const WIN_H: i32 = 372;

pub const ID_CHK_STARTUP: u16 = 100;
pub const ID_CHK_DECIMALS: u16 = 101;
pub const ID_TRAY_BASE: u16 = 110;
pub const ID_GRAPH_BASE: u16 = 120;
pub const ID_BTN_RESET: u16 = 130;

const TABS: [&str; 4] = ["General", "Display", "Battery", "Learning"];

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
    ToggleDecimals,
    ToggleStartup,
    ResetLearned,
}

pub fn action_for(id: u16) -> Option<Action> {
    match id {
        ID_CHK_STARTUP => Some(Action::ToggleStartup),
        ID_CHK_DECIMALS => Some(Action::ToggleDecimals),
        ID_BTN_RESET => Some(Action::ResetLearned),
        _ => {
            if (ID_TRAY_BASE..ID_TRAY_BASE + TrayMode::ALL.len() as u16).contains(&id) {
                Some(Action::SetTray(TrayMode::ALL[(id - ID_TRAY_BASE) as usize]))
            } else if (ID_GRAPH_BASE..ID_GRAPH_BASE + GraphKind::ALL.len() as u16).contains(&id) {
                Some(Action::SetGraph(GraphKind::ALL[(id - ID_GRAPH_BASE) as usize]))
            } else {
                None
            }
        }
    }
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
/// the page is #F9F9F9 while the default static brush is #F0F0F0 -- close
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
    pages: [Vec<HWND>; 4],
    chk_startup: HWND,
    chk_decimals: HWND,
    tray_radios: Vec<HWND>,
    graph_radios: Vec<HWND>,
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
    fn value(&self, x: i32, y: i32, w: i32) -> HWND {
        self.make("STATIC", "", SS_RIGHT, x, y, w, 18, 0)
    }
    fn group(&self, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
        self.make("BUTTON", text, BS_GROUPBOX as u32, x, y, w, h, 0)
    }
    fn checkbox(&self, text: &str, x: i32, y: i32, w: i32, id: u16) -> HWND {
        self.make(
            "BUTTON",
            text,
            (BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
            x, y, w, 22, id,
        )
    }
    fn radio(&self, text: &str, x: i32, y: i32, w: i32, id: u16, first: bool) -> HWND {
        let mut style = (BS_AUTORADIOBUTTON as u32) | WS_TABSTOP;
        if first {
            // WS_GROUP starts a new radio group, so arrow keys move within one
            // set of options rather than across both.
            style |= WS_GROUP;
        }
        self.make("BUTTON", text, style, x, y, w, 20, id)
    }
    fn button(&self, text: &str, x: i32, y: i32, w: i32, h: i32, id: u16) -> HWND {
        self.make(
            "BUTTON",
            text,
            (BS_PUSHBUTTON as u32) | WS_TABSTOP,
            x, y, w, h, id,
        )
    }
}

/// Rows shown on the Battery and Learning tabs.
const BATTERY_ROWS: [&str; 5] = [
    "Full charge capacity",
    "Design capacity",
    "Health",
    "Charge cycles",
    "Chemistry",
];
const LEARNING_ROWS: [&str; 6] = [
    "Seeded from Windows history",
    "Charge curve learned",
    "Charger output",
    "Usage profiles learned",
    "Typical active draw",
    "Predictions graded",
];

impl SettingsWindow {
    pub fn create(hwnd: HWND, hinst: HMODULE, scale: f64) -> SettingsWindow {
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

        // Lets the theme manager answer WM_CTLCOLOR* for these children with
        // the tab page's own background, so labels and checkboxes sit flush on
        // it instead of in faintly mismatched rectangles.
        unsafe {
            // ETDT_ENABLETAB is the documented name for these two together.
            EnableThemeDialogTexture(hwnd, ETDT_ENABLE | ETDT_USETABTEXTURE);
        }

        let b = Builder { parent: hwnd, hinst, font, scale, origin: (0, 0) };
        let tab = b.make(
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
        }

        unsafe {
            let orig = SetWindowLongPtrW(tab, GWLP_WNDPROC, tab_proc as *const () as isize);
            ORIG_TAB_PROC.store(orig, std::sync::atomic::Ordering::Relaxed);
        }
        // Everything from here is a child of the tab control.
        let b = Builder { parent: tab, hinst, font, scale, origin: (8, 8) };

        // Content origin, in window coordinates.
        let (cx, cy) = (28, 56);
        let row_w = 210;
        let val_w = 110;
        let val_x = cx + row_w + 10;

        // --- General
        let chk_startup = b.checkbox("Start with Windows", cx, cy, 320, ID_CHK_STARTUP);
        let chk_decimals = b.checkbox(
            "Show decimal places in battery percentage",
            cx, cy + 32, 320, ID_CHK_DECIMALS,
        );
        let general_note = b.label(
            "The charge gauge moves in steps far smaller than 1%.",
            cx + 22, cy + 56, 320,
        );
        let general = vec![chk_startup, chk_decimals, general_note];

        // --- Display
        let mut display = Vec::new();
        let tray_h = 26 + TrayMode::ALL.len() as i32 * 24;
        display.push(b.group("Tray icon", cx - 8, cy - 8, 336, tray_h));
        let mut tray_radios = Vec::new();
        for (i, m) in TrayMode::ALL.iter().enumerate() {
            let r = b.radio(
                m.label(),
                cx + 8,
                cy + 14 + i as i32 * 24,
                300,
                ID_TRAY_BASE + i as u16,
                i == 0,
            );
            tray_radios.push(r);
            display.push(r);
        }
        let gy = cy - 8 + tray_h + 16;
        let graph_h = 26 + GraphKind::ALL.len() as i32 * 24;
        display.push(b.group("Graph", cx - 8, gy, 336, graph_h));
        let mut graph_radios = Vec::new();
        for (i, g) in GraphKind::ALL.iter().enumerate() {
            let r = b.radio(
                g.label(),
                cx + 8,
                gy + 22 + i as i32 * 24,
                300,
                ID_GRAPH_BASE + i as u16,
                i == 0,
            );
            graph_radios.push(r);
            display.push(r);
        }

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
        let reset_note = b.label(
            "Clears learned data and re-reads Windows history.",
            cx, reset_y, 330,
        );
        let btn_reset = b.button("Reset learned data", cx, reset_y + 22, 160, 30, ID_BTN_RESET);
        learning.push(reset_note);
        learning.push(btn_reset);

        let mut w = SettingsWindow {
            hwnd,
            tab,
            font,
            pages: [general, display, battery, learning],
            chk_startup,
            chk_decimals,
            tray_radios,
            graph_radios,
            battery_values,
            learning_values,
            current: usize::MAX,
        };
        w.select_tab(0);
        w
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
    pub fn sync(&self, settings: &Settings, startup: bool) {
        unsafe {
            let check = |h: HWND, on: bool| {
                SendMessageW(h, BM_SETCHECK, usize::from(on), 0);
            };
            check(self.chk_startup, startup);
            check(self.chk_decimals, settings.decimals);
            for (i, m) in TrayMode::ALL.iter().enumerate() {
                check(self.tray_radios[i], settings.tray_mode == *m);
            }
            for (i, g) in GraphKind::ALL.iter().enumerate() {
                check(self.graph_radios[i], settings.graph == *g);
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
        }
    }
}
