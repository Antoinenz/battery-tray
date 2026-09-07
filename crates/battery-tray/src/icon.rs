//! Tray icon construction.
//!
//! Shape rasterisation lives in `battery_core::glyph`; this module turns those
//! buffers into `HICON`s, renders the text-based modes with GDI, and caches by
//! appearance so a sample that does not change what is displayed draws nothing.

use battery_core::glyph::{self, GlyphState, Rgba};
use battery_core::settings::TrayMode;
use battery_core::types::Phase;
use std::collections::HashMap;
use windows_sys::Win32::Foundation::SIZE;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Beyond this the cache is cleared wholesale. Text modes generate a new label
/// every minute, so without a bound this would grow all day.
const CACHE_LIMIT: usize = 128;

pub fn phase_to_glyph(p: Phase) -> GlyphState {
    match p {
        Phase::Charging => GlyphState::Charging,
        Phase::Discharging => GlyphState::Discharging,
        Phase::Plateau => GlyphState::Plateau,
        Phase::Full => GlyphState::Full,
        Phase::Unknown => GlyphState::Unknown,
    }
}

/// Everything that determines what the icon looks like.
#[derive(Debug)]
pub struct IconSpec {
    pub mode: TrayMode,
    pub size: i32,
    pub soc: f64,
    pub state: GlyphState,
    pub watts: f64,
    /// Seconds to the currently relevant milestone, if there is one.
    pub secs: Option<f64>,
    pub light_taskbar: bool,
}

/// Time split for a 16-24 px icon.
///
/// Four characters across 20 px leaves each glyph about 5 px wide, which is
/// unreadable. `2:45` becomes two stacked lines instead -- hours over minutes,
/// like a clock -- so every glyph gets roughly twice the width.
fn time_lines(secs: f64) -> Vec<String> {
    if !secs.is_finite() || secs < 0.0 {
        return vec!["--".into()];
    }
    let total = secs.round() as i64;
    let (h, m) = (total / 3600, (total % 3600) / 60);
    if h >= 10 {
        vec![format!("{h}h")]
    } else if h > 0 {
        vec![h.to_string(), format!("{m:02}")]
    } else {
        vec![format!("{}", m.max(1))]
    }
}

impl IconSpec {
    /// The text a text mode should show, or `None` for the drawn glyphs.
    ///
    /// Percentages stay whole here even when the decimals preference is on:
    /// four glyphs across 16 px is illegible, and the panel is where the finer
    /// reading belongs.
    /// The lines a text mode should show, or `None` to fall back to a glyph.
    ///
    /// Percentages stay whole here even when the decimals preference is on:
    /// extra digits at 20 px cost more legibility than they add information,
    /// and the panel is where the finer reading belongs.
    fn label(&self) -> Option<Vec<String>> {
        match self.mode {
            TrayMode::Percent => Some(vec![format!("{:.0}", self.soc * 100.0)]),
            TrayMode::Watts => Some(vec![format!("{:.0}", self.watts.abs())]),
            // With no live estimate -- when full, or still measuring -- a dash
            // says nothing. Showing the battery instead still says something.
            TrayMode::Time => self.secs.map(time_lines),
            _ => None,
        }
    }

    fn colour(&self) -> Rgba {
        let fg = if self.light_taskbar {
            glyph::FG_ON_LIGHT
        } else {
            glyph::FG_ON_DARK
        };
        match self.state {
            GlyphState::Charging | GlyphState::Full => glyph::GREEN,
            GlyphState::Plateau => glyph::BLUE,
            _ if self.soc < 0.10 => glyph::RED,
            _ if self.soc < 0.25 => glyph::AMBER,
            _ => fg,
        }
    }

    /// Identity of the rendered result, for caching.
    fn key(&self) -> String {
        let who = match self.mode {
            // Only the displayed level matters, and only to 2%.
            TrayMode::Battery => format!("b{}", (self.soc * 50.0).round() as i32),
            TrayMode::Logo => "logo".to_string(),
            _ => self.label().map(|l| l.join("/")).unwrap_or_else(|| "glyph".into()),
        };
        format!(
            "{who}|{}|{}|{}",
            self.size,
            self.state as u8,
            u8::from(self.light_taskbar)
        )
    }
}

fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        let o = i * 4;
        out[o] = px[2];
        out[o + 1] = px[1];
        out[o + 2] = px[0];
        out[o + 3] = px[3];
    }
    out
}

/// Create a top-down 32bpp DIB and hand back its pixel pointer.
unsafe fn make_dib(size: i32) -> (HBITMAP, *mut u8) {
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = size;
    bmi.bmiHeader.biHeight = -size;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let dib = CreateDIBSection(
        std::ptr::null_mut(),
        &bmi,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    (dib, bits as *mut u8)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Render one or more centred lines of text as an icon.
///
/// GDI cannot draw into an alpha channel, so the text is drawn white on black
/// and the resulting luminance is read back *as* coverage. That recovers proper
/// anti-aliased alpha and lets the glyph be tinted afterwards.
///
/// The face is chosen by shrinking until every line fits, then drawn at the
/// icon's own size rather than supersampled: at these sizes GDI's hinting
/// produces noticeably crisper stems than downsampling a larger render would.
fn render_text(size: i32, lines: &[String], colour: Rgba) -> Vec<u8> {
    unsafe {
        let (dib, bits) = make_dib(size);
        if dib.is_null() || bits.is_null() {
            return vec![0; (size * size * 4) as usize];
        }
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let old_bmp = SelectObject(dc, dib as HGDIOBJ);
        std::ptr::write_bytes(bits, 0, (size * size * 4) as usize);

        let face = wide("Segoe UI");
        let wides: Vec<Vec<u16>> = lines.iter().map(|l| wide(l)).collect();
        let counts: Vec<i32> = lines.iter().map(|l| l.chars().count() as i32).collect();
        let longest = counts.iter().copied().max().unwrap_or(1);
        // Two stacked lines each get half the height; a single line gets it all,
        // and a bolder weight when it is short enough to carry one.
        let weight = if longest <= 2 { 700 } else { 600 };
        let ceiling = (size / lines.len().max(1) as i32).max(6);

        let mut chosen: HFONT = std::ptr::null_mut();
        let mut sizes: Vec<SIZE> = Vec::new();
        for h in (5..=ceiling).rev() {
            let f = CreateFontW(
                -h, 0, 0, 0, weight, 0, 0, 0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                ANTIALIASED_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                face.as_ptr(),
            );
            let prev = SelectObject(dc, f as HGDIOBJ);
            let mut measured = Vec::with_capacity(lines.len());
            for (w, n) in wides.iter().zip(&counts) {
                let mut sz = SIZE { cx: 0, cy: 0 };
                GetTextExtentPoint32W(dc, w.as_ptr(), *n, &mut sz);
                measured.push(sz);
            }
            SelectObject(dc, prev);
            let widest = measured.iter().map(|s| s.cx).max().unwrap_or(0);
            let total_h: i32 = measured.iter().map(|s| s.cy).sum();
            if widest <= size && total_h <= size {
                chosen = f;
                sizes = measured;
                break;
            }
            DeleteObject(f as HGDIOBJ);
        }

        if !chosen.is_null() {
            let prev = SelectObject(dc, chosen as HGDIOBJ);
            SetTextColor(dc, 0x00FF_FFFF);
            SetBkMode(dc, TRANSPARENT as i32);
            let total_h: i32 = sizes.iter().map(|s| s.cy).sum();
            let mut y = (size - total_h) / 2;
            for ((w, n), sz) in wides.iter().zip(&counts).zip(&sizes) {
                TextOutW(dc, (size - sz.cx) / 2, y, w.as_ptr(), *n);
                y += sz.cy;
            }
            SelectObject(dc, prev);
            DeleteObject(chosen as HGDIOBJ);
        }

        // Luminance becomes alpha; colour becomes the requested tint.
        let px = std::slice::from_raw_parts_mut(bits, (size * size * 4) as usize);
        for p in px.chunks_exact_mut(4) {
            let cov = p[0].max(p[1]).max(p[2]);
            p[0] = colour.2;
            p[1] = colour.1;
            p[2] = colour.0;
            p[3] = cov;
        }
        let out = px.to_vec();

        SelectObject(dc, old_bmp);
        DeleteDC(dc);
        DeleteObject(dib as HGDIOBJ);
        out
    }
}

fn create_hicon(size: i32, bgra: &[u8]) -> HICON {
    unsafe {
        let (dib, bits) = make_dib(size);
        if dib.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits, bgra.len());
        // An all-zero mask means "use the colour bitmap's alpha channel".
        let mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: dib,
        };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(dib as HGDIOBJ);
        DeleteObject(mask as HGDIOBJ);
        icon
    }
}

/// Build the icon for a spec, without caching.
fn render(spec: &IconSpec) -> Vec<u8> {
    let colour = spec.colour();
    match spec.mode {
        TrayMode::Battery => rgba_to_bgra(&glyph::battery_rgba(
            spec.size,
            spec.soc,
            spec.state,
            spec.light_taskbar,
        )),
        TrayMode::Logo => rgba_to_bgra(&glyph::logo_rgba(spec.size, colour)),
        _ => match spec.label() {
            Some(lines) => render_text(spec.size, &lines, colour),
            None => rgba_to_bgra(&glyph::battery_rgba(
                spec.size,
                spec.soc,
                spec.state,
                spec.light_taskbar,
            )),
        },
    }
}

/// A window icon: the full logo on its tile.
pub fn window_icon(size: i32) -> HICON {
    create_hicon(size, &rgba_to_bgra(&glyph::logo_tile_rgba(size)))
}

#[derive(Default)]
pub struct IconCache {
    map: HashMap<String, HICON>,
}

impl IconCache {
    pub fn get(&mut self, spec: &IconSpec) -> HICON {
        let key = spec.key();
        if let Some(&h) = self.map.get(&key) {
            return h;
        }
        if self.map.len() >= CACHE_LIMIT {
            self.clear();
        }
        let h = create_hicon(spec.size, &render(spec));
        if !h.is_null() {
            self.map.insert(key, h);
        }
        h
    }

    pub fn clear(&mut self) {
        unsafe {
            for (_, h) in self.map.drain() {
                DestroyIcon(h);
            }
        }
    }
}

impl Drop for IconCache {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The tray icon size Windows wants at the current DPI.
pub fn tray_icon_size() -> i32 {
    unsafe { GetSystemMetrics(SM_CXSMICON).max(16) }
}
