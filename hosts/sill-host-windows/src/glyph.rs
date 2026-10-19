//! The tray icon's glyph (portable: unit-tested on any machine), Sill.app's StatusGlyph.swift redrawn
//! for Windows: an iMac's outline with an iPad tilted 5° in front of its screen; the iPad empty
//! (idle), with a dot (a device connected), its screen filled (streaming), or with an orange badge
//! at the bottom right (something needs the person). Drawn at the tray's pixel size, 4×4 samples a
//! pixel, as two coverage masks: the glyph (one colour, the taskbar's) and the badge (orange).

use crate::traymenu::Glyph;

/// The Mac's canvas, in its points: 21 wide, 18 tall (StatusGlyph.swift).
const CANVAS_W: f64 = 21.0;
const CANVAS_H: f64 = 18.0;
const SAMPLES: usize = 4;

/// Coverage, 0–255 per pixel, row by row from the top: the glyph and the badge.
pub struct Masks {
    pub size: usize,
    pub glyph: Vec<u8>,
    pub badge: Vec<u8>,
}

pub fn draw(state: Glyph, size: usize) -> Masks {
    let scale = size as f64 / CANVAS_W;
    let top = (size as f64 - CANVAS_H * scale) / 2.0;
    let mut glyph = vec![0u8; size * size];
    let mut badge = vec![0u8; size * size];
    let n = (SAMPLES * SAMPLES) as u32;
    for py in 0..size {
        for px in 0..size {
            let (mut g, mut b) = (0u32, 0u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    // The sample in the Mac's points.
                    let x = (px as f64 + (sx as f64 + 0.5) / SAMPLES as f64) / scale;
                    let y = (py as f64 + (sy as f64 + 0.5) / SAMPLES as f64 - top) / scale;
                    match sample(state, x, y) {
                        Ink::Glyph => g += 1,
                        Ink::Badge => b += 1,
                        Ink::None => {}
                    }
                }
            }
            glyph[py * size + px] = (g * 255 / n) as u8;
            badge[py * size + px] = (b * 255 / n) as u8;
        }
    }
    Masks { size, glyph, badge }
}

enum Ink {
    None,
    Glyph,
    Badge,
}

/// The badge: a circle 0.42 of the canvas's height wide at the bottom right, with a ring 1.5 pt
/// wider kept clear around it so it stands apart from the iMac.
const BADGE_CX: f64 = 21.0 - 3.78;
const BADGE_CY: f64 = 18.0 - 3.78;
const BADGE_R: f64 = 3.78;

fn sample(state: Glyph, x: f64, y: f64) -> Ink {
    if state == Glyph::Attention {
        let d = ((x - BADGE_CX).powi(2) + (y - BADGE_CY).powi(2)).sqrt();
        if d <= BADGE_R {
            return Ink::Badge;
        }
        if d <= BADGE_R + 1.5 {
            return Ink::None;
        }
    }
    // The iMac: its body's outline (1.5 pt), the neck, the foot.
    // (The SVG's 672×456 rectangle with corners of 60, stroked 50 wide inside it.)
    let body = rounded_rect(x, y, 0.47, 1.0, 20.53, 14.6, 1.79);
    let inside = rounded_rect(x, y, 0.47 + 1.5, 1.0 + 1.5, 20.53 - 1.5, 14.6 - 1.5, 0.3);
    if body && !inside {
        return Ink::Glyph;
    }
    if rounded_rect(x, y, 9.55, 14.55, 9.55 + 1.9, 14.55 + 1.5, 0.0) || rounded_rect(x, y, 6.3, 15.8, 6.3 + 8.4, 15.8 + 1.2, 0.6) {
        return Ink::Glyph;
    }
    // The iPad, in its own frame: centred at (10.5, 7.8), turned 5° anticlockwise.
    let (a_sin, a_cos) = (5f64.to_radians().sin(), 5f64.to_radians().cos());
    let (dx, dy) = (x - 10.5, y - 7.8);
    let (lx, ly) = (dx * a_cos - dy * a_sin, dx * a_sin + dy * a_cos);
    // 360×250 SVG units to the outside of its 40-unit border (10.75 × 7.46 pt, 1.2 pt), corners of
    // 24 on the stroke's middle line.
    let (hw, hh, stroke) = (10.75 / 2.0, 7.46 / 2.0, 1.2);
    let outer = rounded_rect(lx, ly, -hw, -hh, hw, hh, 0.72 + stroke / 2.0);
    let inner = rounded_rect(lx, ly, -hw + stroke, -hh + stroke, hw - stroke, hh - stroke, 0.12);
    if outer && !inner {
        return Ink::Glyph;
    }
    // What its screen shows.
    match state {
        Glyph::Connected if lx * lx + ly * ly <= 1.05 * 1.05 => Ink::Glyph,
        Glyph::Streaming if rounded_rect(lx, ly, -3.3, -1.65, 3.3, 1.65, 0.36) => Ink::Glyph,
        _ => Ink::None,
    }
}

/// Whether (x, y) is inside the rectangle (left, top)–(right, bottom) with corners of radius `r`.
fn rounded_rect(x: f64, y: f64, left: f64, top: f64, right: f64, bottom: f64, r: f64) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    // A radius at most half the shorter side, and ranges that never invert (8.4 × 1.2 with 0.6
    // corners leaves none in floating point).
    let r = r.min((right - left) / 2.0).min((bottom - top) / 2.0);
    let cx = x.max(left + r).min((right - r).max(left + r));
    let cy = y.max(top + r).min((bottom - r).max(top + r));
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// The masks as a picture in the terminal (tests and debugging): "#" ink, "+" part ink, "o" badge.
pub fn ascii(m: &Masks) -> String {
    let mut out = String::new();
    for y in 0..m.size {
        for x in 0..m.size {
            let i = y * m.size + x;
            out.push(if m.badge[i] > 127 {
                'o'
            } else if m.glyph[i] > 191 {
                '#'
            } else if m.glyph[i] > 63 {
                '+'
            } else {
                '.'
            });
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(m: &Masks) -> u32 {
        m.glyph.iter().map(|&v| v as u32).sum()
    }

    #[test]
    fn the_four_states_differ_where_they_should() {
        for size in [16, 20, 24, 28, 32] {
            let idle = draw(Glyph::Idle, size);
            let connected = draw(Glyph::Connected, size);
            let streaming = draw(Glyph::Streaming, size);
            let attention = draw(Glyph::Attention, size);
            assert_eq!(idle.glyph.len(), size * size);
            // The dot and the screen add ink, the screen more; only attention has a badge.
            assert!(ink(&connected) > ink(&idle), "{size}");
            assert!(ink(&streaming) > ink(&connected), "{size}");
            assert!(idle.badge.iter().all(|&v| v == 0));
            assert!(attention.badge.iter().filter(|&&v| v > 200).count() > size * size / 20, "{size}\n{}", ascii(&attention));
            // Nothing is drawn under the badge.
            for i in 0..size * size {
                assert!(!(attention.badge[i] > 0 && attention.glyph[i] > 0), "{size}: pixel {i}");
            }
        }
    }

    #[test]
    fn the_outline_fits_and_reads() {
        let m = draw(Glyph::Streaming, 32);
        let pic = ascii(&m);
        // The iMac spans the width; its top edge is a solid line; the foot sits at the bottom.
        let rows: Vec<&str> = pic.lines().collect();
        let top = rows.iter().position(|r| r.contains('#')).unwrap();
        assert!(rows[top].matches('#').count() > 24, "{pic}");
        let bottom = rows.iter().rposition(|r| r.contains('#') || r.contains('+')).unwrap();
        // 18 of 21 points tall: 27.4 px of 32, centred.
        assert!(bottom >= 26 && top <= 4, "{top} {bottom}\n{pic}");
    }
}
