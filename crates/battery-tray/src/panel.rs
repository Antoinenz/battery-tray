//! The click-out detail panel.
//!
//! Everything is drawn into one top-down 32bpp DIB: shapes are composited in
//! software (which gives a properly anti-aliased, smoothed graph without a
//! graphics library), then GDI draws text onto a DC selected over the same
//! pixels, and the result is blitted in one go. No flicker, one allocation per
//! paint.

use battery_core::estimator::HistPoint;
use battery_core::settings::{GraphKind, Settings};
use battery_core::types::{fmt_duration, Estimates, Phase};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;

pub const PANEL_W: i32 = 322;
/// Height of everything above the graph.
const HEADER_H: i32 = 134;
const GRAPH_H: i32 = 66;
/// With no graph to show, the window shrinks rather than leaving a void.
pub const PANEL_H_COMPACT: i32 = HEADER_H + 4;
pub const PANEL_H_FULL: i32 = HEADER_H + GRAPH_H;

const PAD: i32 = 18;
/// The oldest part of the graph fades out, so data scrolling off the left edge
/// leaves rather than being clipped mid-stroke.
const FADE_W: i32 = 54;

pub fn panel_height(has_graph: bool) -> i32 {
    if has_graph {
        PANEL_H_FULL
    } else {
        PANEL_H_COMPACT
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Rgb(pub u8, pub u8, pub u8);

/// The panel's colours. Two of these exist so the flyout can follow the system
/// theme; the accents differ between them because a green that reads well on
/// near-black is too pale to be legible on near-white.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Rgb,
    pub border: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    pub rule: Rgb,
    pub tip_bg: Rgb,
    pub green: Rgb,
    pub amber: Rgb,
    pub red: Rgb,
    pub blue: Rgb,
}

pub const DARK: Palette = Palette {
    bg: Rgb(0x18, 0x19, 0x1B),
    border: Rgb(0x33, 0x35, 0x39),
    text: Rgb(0xF3, 0xF3, 0xF4),
    dim: Rgb(0x9A, 0xA0, 0xA6),
    rule: Rgb(0x2A, 0x2C, 0x30),
    tip_bg: Rgb(0x2C, 0x2E, 0x33),
    green: Rgb(0x4A, 0xDE, 0x80),
    amber: Rgb(0xFB, 0xBF, 0x24),
    red: Rgb(0xF8, 0x71, 0x71),
    blue: Rgb(0x60, 0xA5, 0xFA),
};

pub const LIGHT: Palette = Palette {
    bg: Rgb(0xFB, 0xFB, 0xFC),
    border: Rgb(0xD6, 0xD9, 0xDE),
    text: Rgb(0x1A, 0x1C, 0x1F),
    dim: Rgb(0x5F, 0x66, 0x6E),
    rule: Rgb(0xE6, 0xE8, 0xEC),
    tip_bg: Rgb(0xFF, 0xFF, 0xFF),
    green: Rgb(0x15, 0x9E, 0x52),
    amber: Rgb(0xB4, 0x7A, 0x06),
    red: Rgb(0xD3, 0x30, 0x30),
    blue: Rgb(0x1D, 0x64, 0xD8),
};

pub fn palette(light: bool) -> Palette {
    if light {
        LIGHT
    } else {
        DARK
    }
}

pub(crate) fn colorref(c: Rgb) -> COLORREF {
    (c.0 as u32) | ((c.1 as u32) << 8) | ((c.2 as u32) << 16)
}

/// Colour of the live power reading: energy going in reads green, energy
/// leaving reads red, matching the two halves of the throughput graph.
fn flow_colour(est: &Estimates, p: &Palette) -> Rgb {
    match est.phase {
        Phase::Charging | Phase::Full => p.green,
        Phase::Plateau => p.blue,
        Phase::Discharging if est.soc < 0.10 => p.red,
        Phase::Discharging if est.soc < 0.25 => p.amber,
        Phase::Discharging => p.red,
        Phase::Unknown => p.dim,
    }
}

/// Colour for the battery-level graph, which has no sign to key off.
fn level_colour(est: &Estimates, p: &Palette) -> Rgb {
    match est.soc {
        s if s < 0.10 => p.red,
        s if s < 0.25 => p.amber,
        _ => p.green,
    }
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------------------------------ canvas

pub(crate) struct Canvas<'a> {
    pub px: &'a mut [u8],
    pub w: i32,
    pub h: i32,
}

impl Canvas<'_> {
    pub fn blend(&mut self, x: i32, y: i32, c: Rgb, a: f64) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let o = ((y * self.w + x) * 4) as usize;
        let mix = |d: u8, s: u8| (d as f64 * (1.0 - a) + s as f64 * a).round() as u8;
        self.px[o] = mix(self.px[o], c.2);
        self.px[o + 1] = mix(self.px[o + 1], c.1);
        self.px[o + 2] = mix(self.px[o + 2], c.0);
        self.px[o + 3] = 0xFF;
    }
    pub fn fill(&mut self, c: Rgb) {
        for i in (0..self.px.len()).step_by(4) {
            self.px[i] = c.2;
            self.px[i + 1] = c.1;
            self.px[i + 2] = c.0;
            self.px[i + 3] = 0xFF;
        }
    }
    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, c: Rgb, a: f64) {
        for x in x0..x1 {
            self.blend(x, y, c, a);
        }
    }
    pub fn rect_outline(&mut self, r: RECT, c: Rgb, a: f64) {
        for x in r.left..r.right {
            self.blend(x, r.top, c, a);
            self.blend(x, r.bottom - 1, c, a);
        }
        for y in r.top..r.bottom {
            self.blend(r.left, y, c, a);
            self.blend(r.right - 1, y, c, a);
        }
    }
    pub fn round_rect(&mut self, r: RECT, radius: f64, c: Rgb, a: f64) {
        let (x0, y0) = (r.left as f64, r.top as f64);
        let (x1, y1) = (r.right as f64 - 1.0, r.bottom as f64 - 1.0);
        let rr = radius.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                let (fx, fy) = (x as f64, y as f64);
                let cx = fx.clamp(x0 + rr, x1 - rr);
                let cy = fy.clamp(y0 + rr, y1 - rr);
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                let cov = (rr + 0.5 - d).clamp(0.0, 1.0);
                self.blend(x, y, c, a * cov);
            }
        }
    }
}

// ------------------------------------------------------------------ layout

pub(crate) fn scaled(v: i32, scale: f64) -> i32 {
    (v as f64 * scale).round() as i32
}

/// The graph runs edge to edge and flush to the bottom, so it reads as part of
/// the panel rather than a boxed-off chart.
pub fn chart_rect(width: i32, height: i32, scale: f64) -> RECT {
    RECT {
        left: 1,
        top: scaled(HEADER_H, scale).min(height - 2),
        right: width - 1,
        bottom: height - 1,
    }
}

pub fn time_row_rect(scale: f64) -> RECT {
    RECT {
        left: scaled(PAD, scale),
        top: scaled(98, scale),
        right: scaled(PANEL_W - PAD, scale),
        bottom: scaled(126, scale),
    }
}

// ------------------------------------------------------------------- graph

/// Resample history to one value per pixel column, averaging within a column.
fn columns(hist: &[HistPoint], kind: GraphKind, now_ms: i64, cols: i32) -> Vec<Option<f64>> {
    let span = kind.span_ms();
    let start = now_ms - span;
    let n = cols.max(1) as usize;
    let mut sum = vec![0.0f64; n];
    let mut count = vec![0u32; n];
    for p in hist.iter().filter(|p| p.t_ms >= start && p.t_ms <= now_ms) {
        let f = (p.t_ms - start) as f64 / span as f64;
        let i = ((f * (cols - 1) as f64).round() as i64).clamp(0, cols as i64 - 1) as usize;
        let v = match kind {
            GraphKind::Throughput => p.watts as f64,
            GraphKind::Level => p.soc as f64 * 100.0,
        };
        sum[i] += v;
        count[i] += 1;
    }
    (0..n)
        .map(|i| (count[i] > 0).then(|| sum[i] / count[i] as f64))
        .collect()
}

/// Interpolate across gaps, then smooth with a Gaussian kernel.
///
/// Only the span between the first and last real sample is produced; columns
/// outside it come back as `None`. Carrying the earliest reading backwards
/// would draw a flat line across hours the app was not running, which reads as
/// real history rather than absence of it.
fn densify_and_smooth(cols: &[Option<f64>], sigma: f64) -> Option<Vec<Option<f64>>> {
    let known: Vec<usize> = cols
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|_| i))
        .collect();
    if known.len() < 2 {
        return None;
    }
    let (first, last) = (known[0], known[known.len() - 1]);
    if last - first < 2 {
        return None;
    }

    let mut dense = vec![0.0f64; last - first + 1];
    for (offset, slot) in dense.iter_mut().enumerate() {
        let i = first + offset;
        *slot = match cols[i] {
            Some(v) => v,
            None => {
                let a = known.iter().rev().find(|&&k| k < i).copied()?;
                let b = known.iter().find(|&&k| k > i).copied()?;
                let t = (i - a) as f64 / (b - a) as f64;
                cols[a]? * (1.0 - t) + cols[b]? * t
            }
        };
    }

    let radius = (sigma * 3.0).ceil() as i32;
    let kernel: Vec<f64> = (-radius..=radius)
        .map(|d| (-(d as f64).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f64 = kernel.iter().sum();
    let n = dense.len() as i32;

    let mut out = vec![None; cols.len()];
    for i in 0..n {
        let mut acc = 0.0;
        for (j, k) in kernel.iter().enumerate() {
            let idx = (i + j as i32 - radius).clamp(0, n - 1) as usize;
            acc += dense[idx] * k;
        }
        out[first + i as usize] = Some(acc / total);
    }
    Some(out)
}

/// Whether there is enough recent history to be worth drawing.
pub fn has_graph(hist: &[HistPoint], kind: GraphKind, now_ms: i64) -> bool {
    let start = now_ms - kind.span_ms();
    hist.iter().filter(|p| p.t_ms >= start && p.t_ms <= now_ms).count() >= 4
}

#[allow(clippy::too_many_arguments)]
fn draw_chart(
    cv: &mut Canvas,
    r: RECT,
    hist: &[HistPoint],
    kind: GraphKind,
    now_ms: i64,
    est: &Estimates,
    fade_w: i32,
    p: &Palette,
) {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w < 8 || h < 8 {
        return;
    }
    let Some(vals) = densify_and_smooth(&columns(hist, kind, now_ms, w), 2.2) else {
        return;
    };

    let (mut lo, mut hi) = vals
        .iter()
        .flatten()
        .fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    if !lo.is_finite() || !hi.is_finite() {
        return;
    }

    let base_value = match kind {
        GraphKind::Throughput => {
            // Centred on zero so charge and drain read as opposite directions.
            let m = lo.abs().max(hi.abs()).max(0.5) * 1.2;
            lo = -m;
            hi = m;
            0.0
        }
        GraphKind::Level => {
            lo = (lo - 3.0).max(0.0);
            hi = (hi + 3.0).min(100.0);
            if (hi - lo) < 4.0 {
                hi = (lo + 4.0).min(100.0);
            }
            lo
        }
    };
    if (hi - lo).abs() < 1e-6 {
        hi = lo + 1.0;
    }

    let to_y = |v: f64| r.bottom as f64 - 1.0 - (v - lo) / (hi - lo) * (h - 2) as f64;
    let base_y = to_y(base_value);
    let level = level_colour(est, p);

    if kind == GraphKind::Throughput {
        for x in r.left..r.right {
            let f = ((x - r.left) as f64 / fade_w.max(1) as f64).clamp(0.0, 1.0);
            cv.blend(x, base_y.round() as i32, p.rule, 0.9 * f);
        }
    }

    for (i, v) in vals.iter().enumerate() {
        let Some(v) = *v else { continue };
        let x = r.left + i as i32;
        // Older data dissolves toward the left edge instead of being cut off.
        let fade = ((x - r.left) as f64 / fade_w.max(1) as f64).clamp(0.0, 1.0);
        if fade <= 0.0 {
            continue;
        }
        let y = to_y(v);
        // Throughput is coloured by direction: into the battery is green,
        // out of it is red, split at the zero line.
        let col = match kind {
            GraphKind::Throughput => {
                if v >= 0.0 {
                    p.green
                } else {
                    p.red
                }
            }
            GraphKind::Level => level,
        };

        let (top, bot) = if y < base_y { (y, base_y) } else { (base_y, y) };
        let span = (bot - top).max(1.0);
        for py in top.ceil() as i32..bot.floor() as i32 {
            let t = ((py as f64 - top) / span).clamp(0.0, 1.0);
            let a = if y < base_y { 0.24 * (1.0 - t) + 0.06 } else { 0.06 + 0.24 * t };
            cv.blend(x, py, col, a * fade);
        }
        for d in -2..=2 {
            let py = y.round() as i32 + d;
            let cov = (1.4 - (py as f64 - y).abs()).clamp(0.0, 1.0);
            cv.blend(x, py, col, cov * fade);
        }
    }
}

// -------------------------------------------------------------------- text

pub struct Fonts {
    big: HFONT,
    body: HFONT,
    value: HFONT,
    small: HFONT,
}

pub fn make_font(px: i32, weight: i32) -> HFONT {
    let face = wide("Segoe UI Variable Text");
    unsafe {
        CreateFontW(
            -px, 0, 0, 0, weight, 0, 0, 0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            face.as_ptr(),
        )
    }
}

impl Fonts {
    pub fn new(scale: f64) -> Fonts {
        let s = |v: f64| (v * scale).round() as i32;
        Fonts {
            big: make_font(s(30.0), 600),
            body: make_font(s(14.0), 400),
            value: make_font(s(15.0), 600),
            small: make_font(s(12.0), 400),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for f in [self.big, self.body, self.value, self.small] {
                DeleteObject(f as HGDIOBJ);
            }
        }
    }
}

pub(crate) unsafe fn text(
    hdc: HDC,
    s: &str,
    x: i32,
    y: i32,
    font: HFONT,
    c: Rgb,
    right_edge: Option<i32>,
) {
    SelectObject(hdc, font as HGDIOBJ);
    SetTextColor(hdc, colorref(c));
    SetBkMode(hdc, TRANSPARENT as i32);
    let mut w = wide(s);
    let mut r = RECT { left: x, top: y, right: right_edge.unwrap_or(x + 4000), bottom: y + 4000 };
    let flags = if right_edge.is_some() { DT_RIGHT } else { DT_LEFT };
    DrawTextW(hdc, w.as_mut_ptr(), -1, &mut r, flags | DT_SINGLELINE | DT_NOPREFIX);
}

pub(crate) unsafe fn text_width(hdc: HDC, s: &str, font: HFONT) -> i32 {
    SelectObject(hdc, font as HGDIOBJ);
    let w = wide(s);
    let mut sz = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(hdc, w.as_ptr(), s.chars().count() as i32, &mut sz);
    sz.cx
}

/// The headline under the percentage. When the battery is full this states so
/// once, here, and the row below is left out -- saying it twice was noise.
fn flow_line(est: &Estimates) -> String {
    match est.phase {
        Phase::Full => "Fully charged".into(),
        Phase::Plateau => format!("{:.1} W  held", est.watts.abs()),
        Phase::Charging => format!("{:.1} W  in", est.watts.abs()),
        Phase::Discharging => format!("{:.1} W  out", est.watts.abs()),
        Phase::Unknown => "measuring...".into(),
    }
}

fn confidence_word(c: f64) -> &'static str {
    match c {
        c if c >= 0.75 => "High confidence",
        c if c >= 0.45 => "Medium confidence",
        c if c > 0.0 => "Low confidence",
        _ => "Still learning",
    }
}

fn tooltip_lines(est: &Estimates) -> Vec<String> {
    match est.active() {
        Some(p) => vec![
            format!("{} - {}", fmt_duration(p.lo), fmt_duration(p.hi)),
            confidence_word(est.confidence).to_string(),
        ],
        None => vec!["Still learning this battery".into()],
    }
}

// ------------------------------------------------------------------ render

#[allow(clippy::too_many_arguments)]
pub fn render(
    hdc: HDC,
    width: i32,
    height: i32,
    scale: f64,
    fonts: &Fonts,
    est: &Estimates,
    hist: &[HistPoint],
    settings: &Settings,
    now_ms: i64,
    // Where to anchor the hover tooltip, in client coordinates.
    tooltip_at: Option<(i32, i32)>,
    light: bool,
) {
    let s = |v: i32| scaled(v, scale);
    unsafe {
        let mem = CreateCompatibleDC(hdc);
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = -height;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if dib.is_null() || bits.is_null() {
            DeleteDC(mem);
            return;
        }
        let old = SelectObject(mem, dib as HGDIOBJ);
        let p = palette(light);
        let flow = flow_colour(est, &p);
        let right = width - s(PAD);
        let show_graph = has_graph(hist, settings.graph, now_ms) && height > s(HEADER_H) + 8;

        {
            let px = std::slice::from_raw_parts_mut(bits as *mut u8, (width * height * 4) as usize);
            let mut cv = Canvas { px, w: width, h: height };
            cv.fill(p.bg);
            if show_graph {
                draw_chart(
                    &mut cv,
                    chart_rect(width, height, scale),
                    hist,
                    settings.graph,
                    now_ms,
                    est,
                    s(FADE_W),
                    &p,
                );
            }
            cv.hline(s(PAD), right, s(92), p.rule, 1.0);
            cv.rect_outline(RECT { left: 0, top: 0, right: width, bottom: height }, p.border, 1.0);
        }

        text(mem, &settings.format_soc(est.soc), s(PAD), s(10), fonts.big, p.text, None);
        text(mem, &flow_line(est), s(PAD), s(58), fonts.body, flow, None);
        text(
            mem,
            &format!(
                "{:.1} / {:.1} Wh",
                est.capacity_mwh as f64 / 1000.0,
                est.full_mwh as f64 / 1000.0
            ),
            0,
            s(61),
            fonts.small,
            p.dim,
            Some(right),
        );

        // One time row: while charging this is deliberately either the 80%
        // milestone or the full one, never both. A full battery has already
        // said so above, so nothing is repeated here.
        let row_y = s(102);
        match est.active() {
            Some(pred) => {
                text(mem, est.active_label(), s(PAD), row_y, fonts.body, p.dim, None);
                text(mem, &fmt_duration(pred.secs), 0, row_y - s(1), fonts.value, p.text, Some(right));
            }
            None if est.phase == Phase::Plateau => {
                let note = est.note.clone().unwrap_or_default();
                text(mem, &note, s(PAD), row_y, fonts.body, p.text, None);
            }
            None if est.phase != Phase::Full => {
                text(mem, "Measuring...", s(PAD), row_y, fonts.body, p.dim, None);
            }
            None => {}
        }

        if let Some((mx, my)) = tooltip_at {
            let lines = tooltip_lines(est);
            let tw = lines
                .iter()
                .map(|l| text_width(mem, l, fonts.small))
                .max()
                .unwrap_or(0);
            let line_h = s(17);
            let box_w = tw + s(20);
            let box_h = line_h * lines.len() as i32 + s(12);
            // Sit beside the pointer, nudged inside the panel when near an edge.
            let bx = (mx + s(14)).min(width - box_w - s(6)).max(s(6));
            let by = (my + s(18)).min(height - box_h - s(6)).max(s(6));
            {
                let px =
                    std::slice::from_raw_parts_mut(bits as *mut u8, (width * height * 4) as usize);
                let mut cv = Canvas { px, w: width, h: height };
                let tip = RECT { left: bx, top: by, right: bx + box_w, bottom: by + box_h };
                cv.round_rect(tip, s(7) as f64, p.tip_bg, 0.98);
                cv.round_rect(tip, s(7) as f64, p.border, 0.5);
                let inner = RECT {
                    left: tip.left + 1,
                    top: tip.top + 1,
                    right: tip.right - 1,
                    bottom: tip.bottom - 1,
                };
                cv.round_rect(inner, s(6) as f64, p.tip_bg, 1.0);
            }
            for (i, l) in lines.iter().enumerate() {
                let c = if i == 0 { p.text } else { p.dim };
                text(mem, l, bx + s(10), by + s(6) + line_h * i as i32, fonts.small, c, None);
            }
        }

        BitBlt(hdc, 0, 0, width, height, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(dib as HGDIOBJ);
        DeleteDC(mem);
    }
}
