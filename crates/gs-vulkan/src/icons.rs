//! PlayStation button icons, redrawn as Xbox buttons.
//!
//! The game draws each button prompt from a small texture: a dark round
//! button with a coloured symbol (pink square, red circle, blue cross,
//! green triangle) and transparent corners. Such textures are recognised
//! by content and replaced with a high-resolution Xbox button in the same
//! style, mapped by position: Cross -> A, Circle -> B, Square -> X,
//! Triangle -> Y. Texture coordinates are normalised, so the larger
//! replacement drops in unchanged. The shoulder buttons are recognised by
//! shape and label: L1/R1 become LB/RB, L2/R2 become LT/RT. The D-pad,
//! four separate arrows with some in gold, becomes a solid cross.

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shoulder {
    Lb,
    Rb,
    Lt,
    Rt,
}

/// D-pad arms, for gold highlights: up, down, left, right.
pub type Arms = [bool; 4];
pub const UP: Arms = [true, false, false, false];
pub const DOWN: Arms = [false, true, false, false];
pub const LEFT: Arms = [false, false, true, false];
pub const RIGHT: Arms = [false, false, false, true];

/// Highest alpha in a texture: some icons are half transparent throughout
/// (the Cross of the help screens).
fn peak_alpha(px: &[u32]) -> f32 {
    px.iter().map(|&p| rgba(p).3).fold(0.0, f32::max)
}

/// Recognise a PlayStation face-button icon.
pub fn classify(w: u32, h: u32, px: &[u32]) -> Option<Face> {
    if w != h || !(16..=64).contains(&w) {
        return None;
    }
    let peak = peak_alpha(px);
    if peak < 0.25 {
        return None;
    }
    let at = |x: u32, y: u32| {
        let (r, g, b, a) = rgba(px[(y * w + x) as usize]);
        (r, g, b, a / peak)
    };
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
            // Saturated, or the pale lavender cross of the silver-rimmed
            // prompts ("PRESS X TO CONTINUE").
            let sat = (max - min) / max.max(1e-6);
            if max > 0.35 && sat > 0.45 || max > 0.5 && sat > 0.2 && b > r && b > g {
                symbol += 1;
                sum = (sum.0 + r, sum.1 + g, sum.2 + b);
            }
        }
    }
    // Some discs fill only a third of their texture (the line-up's prompt).
    let n = (w * h) as f32;
    if (opaque as f32) < n * 0.3 || (dark as f32) < opaque as f32 * 0.3 || symbol < 6 || symbol as f32 > opaque as f32 * 0.45 {
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

/// Recognise a PlayStation shoulder-button icon: a grey plate with a light
/// label, wide and flat for L1/R1, tall for L2/R2. The top of the first
/// letter tells L (empty on the right) from R (its bowl).
pub fn classify_shoulder(w: u32, h: u32, px: &[u32]) -> Option<Shoulder> {
    if w != h || !(16..=64).contains(&w) {
        return None;
    }
    let at = |x: u32, y: u32| rgba(px[(y * w + x) as usize]);
    let solid = |x: u32, y: u32| at(x, y).3 >= 0.5;
    if [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)].iter().any(|&(x, y)| at(x, y).3 > 0.1) {
        return None;
    }
    let rows: Vec<u32> = (0..h).filter(|&y| (0..w).any(|x| solid(x, y))).collect();
    let (top, bottom) = (*rows.first()?, *rows.last()?);
    let (left, right) = ((0..w).find(|&x| (0..h).any(|y| solid(x, y)))?, (0..w).rfind(|&x| (0..h).any(|y| solid(x, y)))?);
    // A flat top and nearly the full width: not a round button.
    let top_width = (0..w).filter(|&x| solid(x, top + 1)).count() as u32;
    if top_width * 100 < w * 55 || (right - left + 1) * 10 < w * 8 {
        return None;
    }
    let inset = w * 5 / 32;
    let (mut label, mut inside, mut coloured) = (Vec::new(), 0, 0);
    for y in 0..h {
        for x in 0..w {
            if !solid(x, y) {
                continue;
            }
            let (r, g, b, _) = at(x, y);
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            coloured += (max > 0.35 && (max - min) / max > 0.25) as u32;
            if (top + inset..=bottom.saturating_sub(inset)).contains(&y) && (left + inset..=right.saturating_sub(inset)).contains(&x) {
                inside += 1;
                if max >= 0.27 {
                    label.push((x, y));
                }
            }
        }
    }
    if coloured > w * h / 50 || label.len() * 100 < inside * 8 || label.len() * 100 > inside * 60 {
        return None;
    }
    let (lx0, lx1) = (label.iter().map(|p| p.0).min()?, label.iter().map(|p| p.0).max()?);
    let (ly0, ly1) = (label.iter().map(|p| p.1).min()?, label.iter().map(|p| p.1).max()?);
    // The first letter fills the left 45% of the label.
    let glyph = (lx1 - lx0 + 1) * 45 / 100;
    let bowl = label.iter().filter(|&&(x, y)| x >= lx0 + glyph / 2 && x < lx0 + glyph && y <= ly0 + (ly1 - ly0) / 3).count();
    let tall = (bottom - top + 1) * 10 >= h * 7;
    Some(match (bowl >= 3, tall) {
        (false, false) => Shoulder::Lb,
        (true, false) => Shoulder::Rb,
        (false, true) => Shoulder::Lt,
        (true, true) => Shoulder::Rt,
    })
}

/// A D-pad in a texture: its gold arms and the box it fills (x0, y0, x1, y1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dpad {
    pub gold: Arms,
    pub area: (u32, u32, u32, u32),
}

/// Recognise a PlayStation D-pad icon: four solid grey or gold arms around
/// an empty middle, with empty corners. It need not be centred in its
/// texture, so everything is measured from the box it fills.
pub fn classify_dpad(w: u32, h: u32, px: &[u32]) -> Option<Dpad> {
    if w != h || !(16..=64).contains(&w) {
        return None;
    }
    // Solid arrows: half-transparent icons are the help screens' Cross.
    if peak_alpha(px) < 0.9 {
        return None;
    }
    let at = |x: u32, y: u32| rgba(px[(y * w + x) as usize]);
    let solid = |x: u32, y: u32| at(x, y).3 >= 0.5;
    let x0 = (0..w).find(|&x| (0..h).any(|y| solid(x, y)))?;
    let x1 = (0..w).rfind(|&x| (0..h).any(|y| solid(x, y)))?;
    let y0 = (0..h).find(|&y| (0..w).any(|x| solid(x, y)))?;
    let y1 = (0..h).rfind(|&y| (0..w).any(|x| solid(x, y)))?;
    let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
    if bw * 2 < w || bh * 2 < h {
        return None;
    }
    let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
    let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
    if corners.iter().any(|&(x, y)| solid(x, y)) || (0..3).any(|i| (0..3).any(|j| solid(cx + i - 1, cy + j - 1))) {
        return None;
    }
    let (near_x, near_y) = (bw * 3 / 20, bh * 3 / 20);
    let (mut gold, centres) = ([false; 4], [(cx, y0 + near_y), (cx, y1 - near_y), (x0 + near_x, cy), (x1 - near_x, cy)]);
    for (i, (ax, ay)) in centres.into_iter().enumerate() {
        let (mut n, mut warm) = (0, 0.0);
        for j in 0..3 {
            for k in 0..3 {
                let (r, _, b, a) = at(ax + j - 1, ay + k - 1);
                if a >= 0.5 {
                    n += 1;
                    warm += r - b;
                }
            }
        }
        if n < 8 {
            return None;
        }
        gold[i] = warm / n as f32 > 0.1;
    }
    Some(Dpad { gold, area: (x0, y0, x1, y1) })
}

/// Distance from point p to segment ab.
fn seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Letter strokes in a unit box (y down), as polylines.
fn strokes(letter: char) -> Vec<Vec<(f32, f32)>> {
    match letter {
        'A' => vec![vec![(0.12, 1.0), (0.5, 0.0), (0.88, 1.0)], vec![(0.27, 0.62), (0.73, 0.62)]],
        'L' => vec![vec![(0.2, 0.0), (0.2, 1.0), (0.85, 1.0)]],
        'T' => vec![vec![(0.1, 0.0), (0.9, 0.0)], vec![(0.5, 0.0), (0.5, 1.0)]],
        // B and R: a stem and bowls approximated with short arcs.
        'B' | 'R' => {
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
            if letter == 'R' {
                return vec![vec![(0.18, 0.0), (0.18, 1.0)], bowl(0.0, 0.54, 0.8), vec![(0.46, 0.54), (0.86, 1.0)]];
            }
            vec![vec![(0.18, 0.0), (0.18, 1.0)], bowl(0.0, 0.48, 0.74), bowl(0.48, 1.0, 0.84)]
        }
        'X' => vec![vec![(0.14, 0.0), (0.86, 1.0)], vec![(0.86, 0.0), (0.14, 1.0)]],
        _ => vec![vec![(0.12, 0.0), (0.5, 0.5), (0.88, 0.0)], vec![(0.5, 0.5), (0.5, 1.0)]],
    }
}

/// The Xbox button at a PlayStation face button's position.
fn letter(face: Face) -> char {
    match face {
        Face::Cross => 'A',
        Face::Circle => 'B',
        Face::Square => 'X',
        Face::Triangle => 'Y',
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
    (OUT, OUT, draw_sized(face, OUT, OUT))
}

/// [`draw`] at `w` x `h` texels; the button is stretched to fill them.
pub fn draw_sized(face: Face, w: u32, h: u32) -> Vec<u32> {
    let lines = strokes(letter(face));
    let (lr, lg, lb) = colour(face);
    let mut out = Vec::with_capacity((w * h) as usize);
    const SS: u32 = 4;
    for y in 0..h {
        for x in 0..w {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) / w as f32;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) / h as f32;
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
    out
}

/// Draw a shoulder button: a dark rounded plate, light rim, white label.
/// The plate covers the part of the texture the original does.
pub fn draw_shoulder(which: Shoulder) -> (u32, u32, Vec<u32>) {
    let (label, top) = match which {
        Shoulder::Lb => (['L', 'B'], 0.31),
        Shoulder::Rb => (['R', 'B'], 0.31),
        Shoulder::Lt => (['L', 'T'], 0.02),
        Shoulder::Rt => (['R', 'T'], 0.02),
    };
    let lines = label.map(strokes);
    let (left, right, bottom, radius) = (0.06, 0.94, 0.97, 0.13);
    let (cx, cy, hw, hh) = (0.5, (top + bottom) / 2.0, (right - left) / 2.0, (bottom - top) / 2.0);
    // Two letters, side by side in the middle of the plate.
    let (lh, lw) = (0.34, 0.27);
    let boxes = [(0.5 - lw - 0.03, cy - lh / 2.0), (0.53, cy - lh / 2.0)];
    let mut out = Vec::with_capacity((OUT * OUT) as usize);
    const SS: u32 = 4;
    for y in 0..OUT {
        for x in 0..OUT {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) / OUT as f32;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) / OUT as f32;
                    // Signed distance to the rounded plate.
                    let (qx, qy) = ((px - cx).abs() - hw + radius, (py - cy).abs() - hh + radius);
                    let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
                    if d > 0.0 {
                        continue;
                    }
                    let shade = 0.10 + 0.10 * (0.5 - py).max(0.0) * 2.0;
                    let (mut cr, mut cg, mut cb) = if d > -0.045 { (0.62, 0.62, 0.64) } else { (shade, shade, shade * 1.05) };
                    for (l, (bx, by)) in lines.iter().zip(boxes) {
                        let (lx, ly) = ((px - bx) / lw, (py - by) / lh);
                        let dist = l.iter().flat_map(|l| l.windows(2).map(move |w| seg((lx, ly), w[0], w[1]))).fold(f32::MAX, f32::min);
                        if dist < 0.1 {
                            (cr, cg, cb) = (0.93, 0.93, 0.95);
                        }
                    }
                    r += cr;
                    g += cg;
                    b += cb;
                    a += 1.0;
                }
            }
            let cov = a / (SS * SS) as f32;
            let (r, g, b) = if a > 0.0 { (r / a, g / a, b / a) } else { (0.0, 0.0, 0.0) };
            let byte = |v: f32| (v * 255.0).round() as u32;
            out.push(byte(r) | byte(g) << 8 | byte(b) << 16 | ((cov * 128.0).round() as u32) << 24);
        }
    }
    (OUT, OUT, out)
}

/// Draw a D-pad at `w` x `h` texels: a dark solid cross with a light rim,
/// `gold` arms lit.
pub fn draw_dpad(gold: Arms, w: u32, h: u32) -> Vec<u32> {
    const REACH: f32 = 0.47;
    const WIDTH: f32 = 0.2;
    const RIM: f32 = 0.035;
    let mut out = Vec::with_capacity((w * h) as usize);
    const SS: u32 = 4;
    for y in 0..h {
        for x in 0..w {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let (fx, fy) = ((x as f32 + (sx as f32 + 0.5) / SS as f32) / w as f32 - 0.5, (y as f32 + (sy as f32 + 0.5) / SS as f32) / h as f32 - 0.5);
                    // Signed distance to the cross: the nearer of its bars.
                    let bar = |long: f32, short: f32| (long.abs() - REACH).max(short.abs() - WIDTH);
                    let d = bar(fx, fy).min(bar(fy, fx));
                    if d > 0.0 {
                        continue;
                    }
                    // The arm this point is on, if it is past the centre.
                    let arm = if fx.abs() < WIDTH && fy < -WIDTH {
                        Some(0)
                    } else if fx.abs() < WIDTH && fy > WIDTH {
                        Some(1)
                    } else if fy.abs() < WIDTH && fx < -WIDTH {
                        Some(2)
                    } else if fy.abs() < WIDTH && fx > WIDTH {
                        Some(3)
                    } else {
                        None
                    };
                    let (cr, cg, cb) = if d > -RIM {
                        (0.62, 0.62, 0.64)
                    } else if arm.is_some_and(|i| gold[i]) {
                        (0.86, 0.70, 0.30)
                    } else {
                        let shade = 0.13 + 0.10 * (-fy).max(0.0);
                        (shade, shade, shade * 1.05)
                    };
                    r += cr;
                    g += cg;
                    b += cb;
                    a += 1.0;
                }
            }
            let cov = a / (SS * SS) as f32;
            let (r, g, b) = if a > 0.0 { (r / a, g / a, b / a) } else { (0.0, 0.0, 0.0) };
            let byte = |v: f32| (v * 255.0).round() as u32;
            out.push(byte(r) | byte(g) << 8 | byte(b) << 16 | ((cov * 128.0).round() as u32) << 24);
        }
    }
    out
}

/// Replacement for a decoded texture, if it is a PlayStation button icon.
pub fn replace(w: u32, h: u32, px: &[u32]) -> Option<(u32, u32, Vec<u32>)> {
    if let Some(which) = classify_shoulder(w, h, px) {
        return Some(draw_shoulder(which));
    }
    let Some(face) = classify(w, h, px) else {
        let dpad = classify_dpad(w, h, px)?;
        // In the box the original fills, so the sprite stays where it was.
        let (x0, y0, x1, y1) = dpad.area;
        let scale = |v: u32| v * OUT / w;
        let (bw, bh) = (scale(x1 - x0 + 1).max(1), scale(y1 - y0 + 1).max(1));
        let button = draw_dpad(dpad.gold, bw, bh);
        let mut out = vec![0; (OUT * OUT) as usize];
        for (y, row) in button.chunks_exact(bw as usize).enumerate() {
            let at = ((scale(y0) + y as u32) * OUT + scale(x0)) as usize;
            out[at..at + bw as usize].copy_from_slice(row);
        }
        return Some((OUT, OUT, out));
    };
    // Match the original disc's size: from its area, its diameter as a
    // fraction of the texture (the button fills 0.94 of its square).
    let peak = peak_alpha(px);
    let opaque = px.iter().filter(|&&p| rgba(p).3 >= peak / 2.0).count() as f32;
    let fill = ((opaque / (w * h) as f32) * 4.0 / std::f32::consts::PI).sqrt() / 0.94;
    if fill >= 0.9 {
        return Some(draw(face));
    }
    let k = ((OUT as f32 * fill).round() as u32).max(1);
    let button = draw_sized(face, k, k);
    let mut out = vec![0; (OUT * OUT) as usize];
    let o = (OUT - k) / 2;
    for (y, row) in button.chunks_exact(k as usize).enumerate() {
        let at = ((o + y as u32) * OUT + o) as usize;
        out[at..at + k as usize].copy_from_slice(row);
    }
    Some((OUT, OUT, out))
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
    fn classifies_a_half_transparent_cross() {
        let cross = |fx: f32, fy: f32| (fx.abs() - fy.abs()).abs() < 0.05 && fx.abs() < 0.22;
        let halved: Vec<u32> = icon((150, 148, 200), cross).iter().map(|p| if p >> 24 == 0 { 0 } else { p & 0xff_ffff | 0x38 << 24 }).collect();
        assert_eq!(classify(32, 32, &halved), Some(Face::Cross));
    }

    /// Plates traced from the game's textures: ' ' transparent, then
    /// dark, grey and light texels.
    fn plate(rows: &[&str]) -> Vec<u32> {
        let mut v = vec![0; 32 * 32];
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                let g = match c {
                    '.' => 0x30,
                    '+' => 0x60,
                    '#' => 0xc0,
                    _ => continue,
                };
                v[y * 32 + x] = g | g << 8 | g << 16 | 0x80 << 24;
            }
        }
        v
    }

    const FLAT: &str = "  ++.......................###  ";

    fn flat(label: &[&str]) -> Vec<u32> {
        let mut rows = vec![""; 10];
        rows.extend(["     .+  ++++++++++++++++++     ", "   ++++######################   ", "  ++++++++####################  ", "  +++++++++++++++++++++++++###  ", "  +++++....................###  "]);
        rows.extend_from_slice(label);
        rows.extend([FLAT, "  +++......................###  ", "  ++++++++++++++++###########   ", "   +++++++++++++++##########    "]);
        plate(&rows)
    }

    #[test]
    fn recognises_shoulder_buttons() {
        let l1 = flat(&["  ++......+................###  ", "  ++......#+.......+#+.....###  ", "  ++......++......+#++.....###  ", "  ++......++......++++.....###  ", "  ++......++........++.....###  ", "  ++......++........++.....###  ", "  ++......++........++.....###  ", "  ++......++........++.....###  ", "  ++......++++++....++.....###  ", "  ++......++###+....+++....###  ", FLAT]);
        let r1 = flat(&["  ++.....+++++.............+##  ", "  ++.....#++++++....##+....+##  ", "  ++.....+++..++..+#+++....+##  ", "  ++.....++...++..+++++....+##  ", "  ++.....+++.+++.....++....+##  ", "  ++.....++++++......++....+##  ", "  ++.....+++..+......++....+##  ", "  ++.....++...++....+++....+##  ", "  ++.....++...++.....++....+##  ", "  ++.....++...+++....++....+##  ", FLAT]);
        let r2 = plate(&["     .++++++++++++++++++++      ", "    +++####################     ", "   +++++++++++##########+###    ", "   +++++++++++++++++++++.+##    ", "  .++++..................+##    ", "  .++.....................##    ", "  .++.....................##    ", "  .++.....................##    ", "  .++.....................##    ", "  .++....+++++....+++.....##    ", "  .++...+++++++..+#+++....##    ", "  .++....++..++..++..++...##    ", "  .++....+...++.....+++...##    ", "  .++....+++++.....+##....##    ", "  .++....+++++....++++....##    ", "  .++....++..++..+++......##    ", "  .++...+++..++++#+++++...##    ", "  .++...+++..+#+.+####+...##    ", "  .++.....................##    ", "  .++.....................##    ", "  .++.....................##    ", "  .++.....................##    ", "  .++.....................##    ", "  .++....................+##    ", "   ++....................+##    ", "   ++....................##+    ", "   +++..................+##     ", "    +++................+##+     ", "     ++++............++##+      ", "      +++++++++++++++###        ", "        +++++++#######+         ", "           .+++++++             "]);
        assert_eq!(classify_shoulder(32, 32, &l1), Some(Shoulder::Lb));
        assert_eq!(classify_shoulder(32, 32, &r1), Some(Shoulder::Rb));
        assert_eq!(classify_shoulder(32, 32, &r2), Some(Shoulder::Rt));
    }

    /// R1's bold label covers almost half of the plate: real texels, as
    /// the green channel in hex.
    #[test]
    fn recognises_the_bold_r1() {
        let rows = ["      4  555             88     ", "   667889abbbbcccccddddddeeeb   ", "  6665667888899999aaaaaaaabeeb  ", "  66467666666666666666666438ed  ", "  66454434444444444444443215ed  ", "  66442225555542222223222115ed  ", "  66332138888775211399521115ed  ", "  66332137642355315986421115ed  ", "  66332136531256414544421115dd  ", "  66332136544575211135421115dd  ", "  66332136567642111135421115dd  ", "  66332136642344211146421115dd  ", "  66332136631354211146421115dd  ", "  66332126631366311146521115dd  ", "  66332126831268511137621115dd  ", "  66321111111111111111111114dd  ", "  66211111111111111111111116dd  ", "  6641100000000000000000002acc  ", "  4666667777788888889999999bb   ", "   466667777788888889999999a    "];
        let mut v = vec![0; 32 * 32];
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if let Some(d) = c.to_digit(16) {
                    let g = d * 17;
                    v[(y + 10) * 32 + x] = g | g << 8 | g << 16 | 0x80 << 24;
                }
            }
        }
        assert_eq!(classify_shoulder(32, 32, &v), Some(Shoulder::Rb));
    }

    #[test]
    fn recognises_a_dpad_and_its_gold_arms() {
        // Four arrows around a hole, the left and right ones gold.
        let mut v = vec![0; 32 * 32];
        for y in 0..32i32 {
            for x in 0..32i32 {
                let (dx, dy) = (x - 16, y - 16);
                let across = dx.abs() < 6 && dy.abs() >= 5 && dy.abs() < 15;
                let along = dy.abs() < 6 && dx.abs() >= 5 && dx.abs() < 15;
                if across {
                    v[(y * 32 + x) as usize] = 0x80a0_a0a0;
                } else if along {
                    v[(y * 32 + x) as usize] = 0x8040_90c0;
                }
            }
        }
        assert_eq!(classify_dpad(32, 32, &v).map(|d| d.gold), Some([false, false, true, true]));
        let ring = |fx: f32, fy: f32| (fx.abs().max(fy.abs()) - 0.2).abs() < 0.05;
        assert_eq!(classify_dpad(32, 32, &icon((230, 40, 40), ring)), None);
        // The same shape half transparent is not a D-pad.
        let faded: Vec<u32> = v.iter().map(|p| if p >> 24 == 0 { 0 } else { p & 0xff_ffff | 0x38 << 24 }).collect();
        assert_eq!(classify_dpad(32, 32, &faded), None);
    }

    /// Real texels of the game's D-pad, which sits off-centre: `G` gold.
    #[test]
    fn recognises_the_off_centre_dpad() {
        let rows = ["", "", "         GGGGGGGG", "         GGGGGGGG", "         GGGGGGGG", "         GGGGGGGG", "         GGGGGGGG", "         GGGGGGGG", "         GGGGGGGG", "         GGGGGGGG", "          GGGGGG", "  GGGGGG   GGGG", "  GGGGGG    GG     GGGGGG", "  GGGGGGG         GGGGGGG", "  GGGGGGGG       GGGGGGGG", "  GGGGGGGG       GGGGGGGG", "  GGGGGGGGG     GGGGGGGGG", "  GGGGGGGG       GGGGGGGG", "  GGGGGGGG       GGGGGGGG", "  GGGGGGG    G    GGGGGGG", "  GGGGGG    GGG    GGGGGG", "           GGGGG", "          GGGGGGG", "          GGGGGGG", "          GGGGGGG", "          GGGGGGG", "          GGGGGGG", "          GGGGGGG", "          GGGGGGG", "          GGGGGGG"];
        let mut v = vec![0; 32 * 32];
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if c == 'G' {
                    v[y * 32 + x] = 0x8040_90c0;
                }
            }
        }
        let d = classify_dpad(32, 32, &v).expect("a D-pad");
        assert_eq!(d.gold, [true; 4]);
        assert_eq!(d.area, (2, 2, 24, 29));
        let (w, _, out) = replace(32, 32, &v).expect("a replacement");
        // Drawn where the original was: nothing right of its box.
        assert!((0..w).all(|y| out[(y * w + w - 8) as usize] >> 24 == 0));
    }

    #[test]
    fn half_transparent_cross_stays_a_face_button() {
        let cross = |fx: f32, fy: f32| (fx.abs() - fy.abs()).abs() < 0.05 && fx.abs() < 0.22;
        let halved: Vec<u32> = icon((150, 148, 200), cross).iter().map(|p| if p >> 24 == 0 { 0 } else { p & 0xff_ffff | 0x38 << 24 }).collect();
        let (_, _, out) = replace(32, 32, &halved).expect("a replacement");
        // A-green texels, as on the prompts.
        assert!(out.iter().any(|p| (p >> 8) & 0xff > 150 && p & 0xff < 120));
    }

    #[test]
    fn round_buttons_are_not_shoulders() {
        let ring = |fx: f32, fy: f32| (fx.abs().max(fy.abs()) - 0.2).abs() < 0.05;
        assert_eq!(classify_shoulder(32, 32, &icon((230, 230, 230), ring)), None);
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
