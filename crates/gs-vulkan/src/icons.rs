//! PlayStation face-button icons, redrawn as Xbox buttons.
//!
//! The game draws each button prompt from a small texture: a dark round
//! button with a coloured symbol (pink square, red circle, blue cross,
//! green triangle) and transparent corners. Such textures are recognised
//! by content and replaced with a high-resolution Xbox button in the same
//! style, mapped by position: Cross -> A, Circle -> B, Square -> X,
//! Triangle -> Y. Texture coordinates are normalised, so the larger
//! replacement drops in unchanged.

/// Replacement resolution: sharp even at 5x internal scale.
const OUT: u32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Cross,
    Circle,
    Square,
    Triangle,
}

// Texels hold GS colours with GS alpha (0x80 = opaque).
fn rgba(p: u32) -> (f32, f32, f32, f32) {
    let c = |s: u32| ((p >> s) & 0xff) as f32 / 255.0;
    (c(0), c(8), c(16), ((p >> 24).min(128)) as f32 / 128.0)
}

/// Recognise a PlayStation face-button icon.
pub fn classify(w: u32, h: u32, px: &[u32]) -> Option<Face> {
    if w != h || !(16..=64).contains(&w) {
        return None;
    }
    let at = |x: u32, y: u32| rgba(px[(y * w + x) as usize]);
    // Transparent corners, opaque centre.
    let corners = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)];
    if corners.iter().any(|&(x, y)| at(x, y).3 > 0.1) || at(w / 2, h / 2).3 < 0.9 {
        return None;
    }
    // Inside the disc: mostly dark, plus a saturated symbol.
    let (mut opaque, mut dark, mut symbol) = (0, 0, 0);
    let mut sum = (0.0, 0.0, 0.0);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b, a) = at(x, y);
            if a < 0.5 {
                continue;
            }
            opaque += 1;
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            if max < 0.35 {
                dark += 1;
            }
            if max > 0.35 && (max - min) / max > 0.45 {
                symbol += 1;
                sum = (sum.0 + r, sum.1 + g, sum.2 + b);
            }
        }
    }
    let n = (w * h) as f32;
    if (opaque as f32) < n * 0.5 || (dark as f32) < opaque as f32 * 0.3 || symbol < 6 || symbol as f32 > opaque as f32 * 0.45 {
        return None;
    }
    let (r, g, b) = (sum.0 / symbol as f32, sum.1 / symbol as f32, sum.2 / symbol as f32);
    Some(if g > r && g > b {
        Face::Triangle
    } else if b > r && b > g {
        Face::Cross
    } else if b > 0.55 * r {
        Face::Square // pink
    } else {
        Face::Circle // red
    })
}

/// Distance from point p to segment ab.
fn seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Letter strokes in a unit box (y down), as polylines.
fn strokes(face: Face) -> Vec<Vec<(f32, f32)>> {
    match face {
        // A
        Face::Cross => vec![vec![(0.12, 1.0), (0.5, 0.0), (0.88, 1.0)], vec![(0.27, 0.62), (0.73, 0.62)]],
        // B: stem and two bowls approximated with short arcs.
        Face::Circle => {
            let bowl = |top: f32, bottom: f32, right: f32| {
                let (cy, r) = ((top + bottom) / 2.0, (bottom - top) / 2.0);
                let mut v = vec![(0.18, top), (right - r, top)];
                for i in 0..=8 {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 8.0;
                    v.push((right - r + r * a.cos(), cy + r * a.sin()));
                }
                v.push((0.18, bottom));
                v
            };
            vec![vec![(0.18, 0.0), (0.18, 1.0)], bowl(0.0, 0.48, 0.74), bowl(0.48, 1.0, 0.84)]
        }
        // X
        Face::Square => vec![vec![(0.14, 0.0), (0.86, 1.0)], vec![(0.86, 0.0), (0.14, 1.0)]],
        // Y
        Face::Triangle => vec![vec![(0.12, 0.0), (0.5, 0.5), (0.88, 0.0)], vec![(0.5, 0.5), (0.5, 1.0)]],
    }
}

fn colour(face: Face) -> (f32, f32, f32) {
    match face {
        Face::Cross => (0.36, 0.80, 0.25),    // A green
        Face::Circle => (0.89, 0.24, 0.20),   // B red
        Face::Square => (0.20, 0.52, 0.92),   // X blue
        Face::Triangle => (0.95, 0.76, 0.18), // Y yellow
    }
}

/// Draw an Xbox button: dark disc, light rim, coloured letter.
pub fn draw(face: Face) -> (u32, u32, Vec<u32>) {
    let n = OUT as f32;
    let lines = strokes(face);
    let (lr, lg, lb) = colour(face);
    let mut out = Vec::with_capacity((OUT * OUT) as usize);
    const SS: u32 = 4;
    for y in 0..OUT {
        for x in 0..OUT {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) / n;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) / n;
                    let d = ((px - 0.5).powi(2) + (py - 0.5).powi(2)).sqrt();
                    if d > 0.47 {
                        continue;
                    }
                    // Rim, then a top-lit dark face.
                    let (mut cr, mut cg, mut cb) = if d > 0.42 {
                        (0.62, 0.62, 0.64)
                    } else {
                        let shade = 0.10 + 0.10 * (0.5 - py).max(0.0) * 2.0;
                        (shade, shade, shade * 1.05)
                    };
                    // Letter inside a 0.42-wide box centred on the face.
                    let (lx, ly) = ((px - 0.29) / 0.42, (py - 0.29) / 0.42);
                    let dist = lines
                        .iter()
                        .flat_map(|l| l.windows(2).map(move |w| seg((lx, ly), w[0], w[1])))
                        .fold(f32::MAX, f32::min);
                    if dist < 0.085 {
                        (cr, cg, cb) = (lr, lg, lb);
                    }
                    r += cr;
                    g += cg;
                    b += cb;
                    a += 1.0;
                }
            }
            let k = (SS * SS) as f32;
            let cov = a / k;
            let (r, g, b) = if a > 0.0 { (r / a, g / a, b / a) } else { (0.0, 0.0, 0.0) };
            let byte = |v: f32| (v * 255.0).round() as u32;
            // GS alpha: 0x80 is opaque.
            out.push(byte(r) | byte(g) << 8 | byte(b) << 16 | ((cov * 128.0).round() as u32) << 24);
        }
    }
    (OUT, OUT, out)
}

/// Replacement for a decoded texture, if it is a PlayStation button icon.
pub fn replace(w: u32, h: u32, px: &[u32]) -> Option<(u32, u32, Vec<u32>)> {
    classify(w, h, px).map(draw)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic PlayStation icon: dark disc, coloured symbol.
    fn icon(sym: (u32, u32, u32), shape: impl Fn(f32, f32) -> bool) -> Vec<u32> {
        let mut v = Vec::new();
        for y in 0..32 {
            for x in 0..32 {
                let (fx, fy) = (x as f32 / 31.0 - 0.5, y as f32 / 31.0 - 0.5);
                let p = if (fx * fx + fy * fy).sqrt() > 0.48 {
                    0
                } else if shape(fx, fy) {
                    sym.0 | sym.1 << 8 | sym.2 << 16 | 0x80 << 24
                } else {
                    0x20 | 0x20 << 8 | 0x20 << 16 | 0x80 << 24
                };
                v.push(p);
            }
        }
        v
    }

    #[test]
    fn classifies_by_symbol_colour() {
        let ring = |fx: f32, fy: f32| (fx.abs().max(fy.abs()) - 0.2).abs() < 0.05;
        assert_eq!(classify(32, 32, &icon((230, 90, 170), ring)), Some(Face::Square));
        assert_eq!(classify(32, 32, &icon((230, 40, 40), ring)), Some(Face::Circle));
        assert_eq!(classify(32, 32, &icon((60, 110, 230), ring)), Some(Face::Cross));
        assert_eq!(classify(32, 32, &icon((60, 200, 90), ring)), Some(Face::Triangle));
    }

    #[test]
    fn ignores_other_textures() {
        // Opaque texture (no transparent corners).
        assert_eq!(classify(32, 32, &vec![0x8000_0040; 1024]), None);
        // Non-square or large textures.
        assert_eq!(classify(128, 64, &vec![0; 128 * 64]), None);
        // Disc without a coloured symbol.
        assert_eq!(classify(32, 32, &icon((0, 0, 0), |_, _| false)), None);
    }

    #[test]
    fn draws_opaque_button_with_letter() {
        let (w, h, px) = draw(Face::Cross);
        assert_eq!((w, h), (OUT, OUT));
        assert_eq!(px[0] >> 24, 0, "corner transparent");
        // Some texels are A-green.
        assert!(px.iter().any(|p| (p >> 8) & 0xff > 150 && p & 0xff < 120));
    }
}
