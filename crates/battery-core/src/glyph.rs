//! Pure glyph rasterisation: the battery mark and the app logo.
//!
//! Deliberately free of any platform dependency. The tray turns these buffers
//! into `HICON`s, and the build script turns the logo into the `.ico` embedded
//! in the executable, so there is exactly one definition of what the app looks
//! like.
//!
//! Shapes are drawn by supersampled coverage testing rather than a graphics
//! library: at tray sizes (16-24 px) that is both smaller and sharper than
//! scaling down a vector path.

/// Straight (non-premultiplied) RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba(0, 0, 0, 0);
    pub fn lerp(a: Rgba, b: Rgba, t: f64) -> Rgba {
        let t = t.clamp(0.0, 1.0);
        let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
        Rgba(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2), m(a.3, b.3))
    }
}

pub const GREEN: Rgba = Rgba(0x4A, 0xDE, 0x80, 0xFF);
pub const TEAL: Rgba = Rgba(0x2D, 0xD4, 0xBF, 0xFF);
pub const AMBER: Rgba = Rgba(0xFB, 0xBF, 0x24, 0xFF);
pub const RED: Rgba = Rgba(0xF8, 0x71, 0x71, 0xFF);
pub const BLUE: Rgba = Rgba(0x60, 0xA5, 0xFA, 0xFF);
pub const FG_ON_DARK: Rgba = Rgba(0xEC, 0xEC, 0xEC, 0xFF);
pub const FG_ON_LIGHT: Rgba = Rgba(0x1A, 0x1A, 0x1A, 0xFF);

/// Supersampling factor. 4x gives 16 coverage levels per axis, which is ample
/// for a 16 px glyph and costs only a few thousand samples.
const SS: i32 = 4;

/// Rounded-rectangle hit test in normalised coordinates.
pub fn in_round_rect(x: f64, y: f64, x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

pub fn in_polygon(x: f64, y: f64, pts: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Rasterise `shade` over a `size` x `size` RGBA buffer, supersampled.
///
/// `shade` receives normalised coordinates in 0..1 and returns the colour at
/// that point, or `None` for transparent.
pub fn rasterise<F>(size: i32, shade: F) -> Vec<u8>
where
    F: Fn(f64, f64) -> Option<Rgba>,
{
    let size = size.max(1);
    let big = size * SS;
    let s = big as f64;
    let mut hi = vec![0u8; (big * big * 4) as usize];
    for py in 0..big {
        for px in 0..big {
            if let Some(c) = shade((px as f64 + 0.5) / s, (py as f64 + 0.5) / s) {
                let o = ((py * big + px) * 4) as usize;
                hi[o] = c.0;
                hi[o + 1] = c.1;
                hi[o + 2] = c.2;
                hi[o + 3] = c.3;
            }
        }
    }

    let mut out = vec![0u8; (size * size * 4) as usize];
    let n = (SS * SS) as u32;
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..SS {
                for dx in 0..SS {
                    let o = (((y * SS + dy) * big + (x * SS + dx)) * 4) as usize;
                    let pa = hi[o + 3] as u32;
                    // Weight colour by coverage so edges do not darken toward
                    // black where the glyph fades out.
                    r += hi[o] as u32 * pa;
                    g += hi[o + 1] as u32 * pa;
                    b += hi[o + 2] as u32 * pa;
                    a += pa;
                }
            }
            let o = ((y * size + x) * 4) as usize;
            if a > 0 {
                out[o] = (r / a) as u8;
                out[o + 1] = (g / a) as u8;
                out[o + 2] = (b / a) as u8;
                out[o + 3] = (a / n) as u8;
            }
        }
    }
    out
}

/// The lightning bolt, in normalised glyph coordinates.
const BOLT: [(f64, f64); 6] = [
    (0.44, 0.26),
    (0.24, 0.545),
    (0.355, 0.545),
    (0.30, 0.80),
    (0.51, 0.455),
    (0.395, 0.455),
];

struct BatteryBox {
    bx0: f64,
    by0: f64,
    bx1: f64,
    by1: f64,
    cx0: f64,
    cy0: f64,
    cx1: f64,
    cy1: f64,
    stroke: f64,
    radius: f64,
    gap: f64,
}

fn battery_box() -> BatteryBox {
    BatteryBox {
        bx0: 0.07,
        by0: 0.28,
        bx1: 0.79,
        by1: 0.72,
        cx0: 0.81,
        cy0: 0.41,
        cx1: 0.92,
        cy1: 0.59,
        stroke: 0.055,
        radius: 0.07,
        gap: 0.028,
    }
}

/// Which battery state the glyph should depict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphState {
    Discharging,
    Charging,
    Plateau,
    Full,
    Unknown,
}

fn level_colour(soc: f64, state: GlyphState, fg: Rgba) -> Rgba {
    match state {
        GlyphState::Charging | GlyphState::Full => GREEN,
        GlyphState::Plateau => BLUE,
        _ if soc < 0.10 => RED,
        _ if soc < 0.25 => AMBER,
        _ => fg,
    }
}

/// The battery preview: outline, terminal, and a fill proportional to charge.
/// While charging, a bolt is knocked out of the glyph so it reads as a shape
/// rather than competing with the fill colour for contrast.
pub fn battery_rgba(size: i32, soc: f64, state: GlyphState, on_light_taskbar: bool) -> Vec<u8> {
    let b = battery_box();
    let fg = if on_light_taskbar { FG_ON_LIGHT } else { FG_ON_DARK };
    let fill = level_colour(soc, state, fg);
    let (ix0, iy0, ix1, iy1) = (
        b.bx0 + b.stroke + b.gap,
        b.by0 + b.stroke + b.gap,
        b.bx1 - b.stroke - b.gap,
        b.by1 - b.stroke - b.gap,
    );
    let level_x1 = ix0 + (ix1 - ix0) * soc.clamp(0.0, 1.0);
    let charging = state == GlyphState::Charging;

    rasterise(size, move |x, y| {
        if charging && in_polygon(x, y, &BOLT) {
            return None;
        }
        let outer = in_round_rect(x, y, b.bx0, b.by0, b.bx1, b.by1, b.radius);
        let inner = in_round_rect(
            x,
            y,
            b.bx0 + b.stroke,
            b.by0 + b.stroke,
            b.bx1 - b.stroke,
            b.by1 - b.stroke,
            (b.radius - b.stroke).max(0.0),
        );
        let cap = in_round_rect(x, y, b.cx0, b.cy0, b.cx1, b.cy1, 0.02);
        if (outer && !inner) || cap {
            Some(fg)
        } else if inner && x >= ix0 && x <= level_x1 && y >= iy0 && y <= iy1 {
            Some(fill)
        } else {
            None
        }
    })
}

/// The app mark: a battery outline with a bolt filling it.
///
/// Distinct from [`battery_rgba`] on purpose -- the preview's fill moves with
/// charge, so it makes a poor identity. This one never changes.
fn mark(x: f64, y: f64, body: Rgba) -> Option<Rgba> {
    let b = battery_box();
    let outer = in_round_rect(x, y, b.bx0, b.by0, b.bx1, b.by1, b.radius);
    let inner = in_round_rect(
        x,
        y,
        b.bx0 + b.stroke,
        b.by0 + b.stroke,
        b.bx1 - b.stroke,
        b.by1 - b.stroke,
        (b.radius - b.stroke).max(0.0),
    );
    let cap = in_round_rect(x, y, b.cx0, b.cy0, b.cx1, b.cy1, 0.02);
    if (outer && !inner) || cap || (inner && in_polygon(x, y, &BOLT)) {
        Some(body)
    } else {
        None
    }
}

/// Flat logo for the tray, in a single colour.
pub fn logo_rgba(size: i32, colour: Rgba) -> Vec<u8> {
    rasterise(size, move |x, y| mark(x, y, colour))
}

/// Full logo on a rounded gradient tile, for the window and file icon.
pub fn logo_tile_rgba(size: i32) -> Vec<u8> {
    rasterise(size, |x, y| {
        // The mark is inset within the tile and drawn in white.
        let inset = 0.14;
        let span = 1.0 - inset * 2.0;
        let (mx, my) = ((x - inset) / span, (y - inset) / span);
        if (0.0..1.0).contains(&mx) && (0.0..1.0).contains(&my) {
            if let Some(c) = mark(mx, my, Rgba(0xFF, 0xFF, 0xFF, 0xFF)) {
                return Some(c);
            }
        }
        if in_round_rect(x, y, 0.02, 0.02, 0.98, 0.98, 0.22) {
            Some(Rgba::lerp(GREEN, TEAL, y))
        } else {
            None
        }
    })
}

/// Encode RGBA buffers as a Windows `.ico`.
///
/// Uses 32bpp BMP entries rather than PNG so no compressor is needed. Each
/// entry carries a BITMAPINFOHEADER of doubled height (colour plus mask), the
/// colour rows bottom-up, then an all-zero AND mask -- zero means "defer to the
/// alpha channel", which is what modern Windows uses.
pub fn encode_ico(images: &[(i32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut bodies: Vec<Vec<u8>> = Vec::new();
    for (size, rgba) in images {
        let (w, h) = (*size, *size);
        let mask_row = ((w + 31) / 32 * 4) as usize;
        let mut body = Vec::new();
        // BITMAPINFOHEADER
        body.extend_from_slice(&40u32.to_le_bytes());
        body.extend_from_slice(&w.to_le_bytes());
        body.extend_from_slice(&(h * 2).to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&32u16.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        body.extend_from_slice(&((w * h * 4) as u32 + (mask_row * h as usize) as u32).to_le_bytes());
        body.extend_from_slice(&[0u8; 16]); // resolution, palette counts
        for row in (0..h).rev() {
            for col in 0..w {
                let o = ((row * w + col) * 4) as usize;
                body.push(rgba[o + 2]); // B
                body.push(rgba[o + 1]); // G
                body.push(rgba[o]); // R
                body.push(rgba[o + 3]); // A
            }
        }
        body.extend(std::iter::repeat(0u8).take(mask_row * h as usize));
        bodies.push(body);
    }

    let mut offset = 6 + 16 * images.len();
    for (i, (size, _)) in images.iter().enumerate() {
        // 256 is encoded as 0 in the directory.
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0); // palette
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(bodies[i].len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += bodies[i].len();
    }
    for b in bodies {
        out.extend(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(buf: &[u8], size: i32, x: i32, y: i32) -> u8 {
        buf[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn rasterise_produces_the_requested_size() {
        let b = battery_rgba(16, 0.5, GlyphState::Discharging, false);
        assert_eq!(b.len(), 16 * 16 * 4);
    }

    #[test]
    fn a_full_battery_covers_more_than_an_empty_one() {
        let empty = battery_rgba(32, 0.0, GlyphState::Discharging, false);
        let full = battery_rgba(32, 1.0, GlyphState::Discharging, false);
        let ink = |b: &[u8]| b.chunks(4).map(|p| p[3] as u32).sum::<u32>();
        assert!(ink(&full) > ink(&empty), "fill should scale with charge");
    }

    #[test]
    fn the_glyph_stays_inside_its_box() {
        let b = battery_rgba(32, 1.0, GlyphState::Charging, false);
        for i in 0..32 {
            assert_eq!(alpha_at(&b, 32, i, 0), 0, "top row must be clear");
            assert_eq!(alpha_at(&b, 32, i, 31), 0, "bottom row must be clear");
            assert_eq!(alpha_at(&b, 32, 0, i), 0, "left column must be clear");
        }
    }

    #[test]
    fn charging_knocks_the_bolt_out_of_the_fill() {
        let plain = battery_rgba(32, 1.0, GlyphState::Discharging, false);
        let bolt = battery_rgba(32, 1.0, GlyphState::Charging, false);
        let ink = |b: &[u8]| b.chunks(4).map(|p| p[3] as u32).sum::<u32>();
        assert!(ink(&bolt) < ink(&plain), "the bolt removes coverage");
    }

    #[test]
    fn low_charge_is_red_and_healthy_charge_is_not() {
        let low = battery_rgba(32, 0.05, GlyphState::Discharging, false);
        let ok = battery_rgba(32, 0.90, GlyphState::Discharging, false);
        // At 5% the fill is under a pixel wide, so its coverage -- and hence
        // alpha -- is partial. Test the hue, not full opacity.
        let has_red = |b: &[u8]| {
            b.chunks(4)
                .any(|p| p[3] > 100 && p[0] as i32 > p[1] as i32 + 60 && p[0] as i32 > p[2] as i32 + 60)
        };
        assert!(has_red(&low), "a nearly flat battery should warn");
        assert!(!has_red(&ok));
    }

    #[test]
    fn the_logo_does_not_change_with_charge() {
        let a = logo_rgba(32, GREEN);
        let b = logo_rgba(32, GREEN);
        assert_eq!(a, b);
        // And it differs from the level-dependent preview.
        assert_ne!(a, battery_rgba(32, 0.5, GlyphState::Discharging, false));
    }

    #[test]
    fn the_tile_logo_is_opaque_in_the_middle_and_rounded_at_corners() {
        let t = logo_tile_rgba(64);
        assert_eq!(alpha_at(&t, 64, 32, 32), 255, "centre must be solid");
        assert_eq!(alpha_at(&t, 64, 0, 0), 0, "corner must be rounded away");
    }

    #[test]
    fn ico_has_a_valid_header_and_consistent_offsets() {
        let images = vec![(16, logo_tile_rgba(16)), (32, logo_tile_rgba(32))];
        let ico = encode_ico(&images);
        assert_eq!(&ico[0..2], &[0, 0], "reserved");
        assert_eq!(u16::from_le_bytes([ico[2], ico[3]]), 1, "type icon");
        assert_eq!(u16::from_le_bytes([ico[4], ico[5]]), 2, "two entries");
        for i in 0..2 {
            let e = 6 + i * 16;
            let len = u32::from_le_bytes(ico[e + 8..e + 12].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(ico[e + 12..e + 16].try_into().unwrap()) as usize;
            assert!(off + len <= ico.len(), "entry {i} runs past the end");
            assert_eq!(
                u32::from_le_bytes(ico[off..off + 4].try_into().unwrap()),
                40,
                "BITMAPINFOHEADER size"
            );
        }
    }

    #[test]
    fn ico_encodes_256_as_zero_in_the_directory() {
        let ico = encode_ico(&[(256, logo_tile_rgba(256))]);
        assert_eq!(ico[6], 0, "256 px is recorded as 0");
        assert_eq!(ico[7], 0);
    }
}
