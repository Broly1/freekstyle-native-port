//! The boost HUD, redrawn from the player's own recording: the gear, the
//! big devil logo, the face on the boost ball and the red and gold meter.
//!
//! The logo, ball face and meter are a few flat colours, so no upscaler is
//! needed (it only smears them): each colour becomes its own smooth shape
//! at 8x and the shapes are put back together with crisp, anti-aliased
//! edges; the meter's bars run on under the gear, so no gap shows beside
//! them. The ball's face is too small to redraw from itself, so it takes
//! its horns and lines from the big logo, fitted to the original's. The
//! gear keeps the original's design (its outline with one tooth repeated,
//! the raised ring, the two solid wedges, the horned face fitted from the
//! big logo) in polished steel, with see-through holes and a soft shadow.
//! Nothing of the game's art is kept in the source: it all comes from the
//! recording.

use std::path::Path;

use image::RgbaImage;

use crate::plane::Plane;

const F: usize = 8;
pub const GEAR: &str = "04f1ae1a30c0cc38";
pub const LOGO: &str = "9606d5c1f9478577";
pub const BALL: &str = "17941779c37d9812";
pub const METER: &str = "72ec3c5f7b4eef11";

/// Logo colours: white, orange-red outline, dark-red lines.
const LOGO_PAL: [[f32; 3]; 3] = [[248.0, 248.0, 248.0], [248.0, 40.0, 0.0], [96.0, 0.0, 0.0]];
const METER_PAL: [[f32; 3]; 2] = [[188.0, 0.0, 16.0], [212.0, 180.0, 96.0]];
const BALL_LINE: [f32; 3] = [125.0, 18.0, 22.0];
/// How far the meter runs on under the gear, in original texels: the gear is
/// drawn over it, and its holes are a little larger than the meter's bars.
const METER_TUCK: f32 = 3.0;
/// How far the original's light and shade carries into the ball's face.
const BALL_SHADE: f32 = 0.5;

pub fn is_boost(hash: &str) -> bool {
    version(hash).is_some()
}

/// The version of each drawing. Raise one whenever what it draws changes:
/// packs holding an older one then get it redrawn once (see `ledger`).
pub fn version(hash: &str) -> Option<u32> {
    match hash {
        GEAR | LOGO | BALL | METER => Some(1),
        _ => None,
    }
}

/// Whether it takes its lines from the big logo, and so can't be drawn
/// until that has been recorded.
pub fn needs_logo(hash: &str) -> bool {
    hash == GEAR || hash == BALL
}

/// Redraw one of the four; `dump` is the recording it came from.
pub fn draw(hash: &str, path: &Path, dump: &Path) -> Result<RgbaImage, String> {
    let src = open(path)?;
    match hash {
        LOGO => Ok(logo(&src)),
        METER => Ok(meter(&src)),
        _ => {
            let logo = find_logo(dump).ok_or("the big logo isn't recorded yet")?;
            let logo = open(&logo)?;
            Ok(if hash == GEAR { gear(&src, &logo) } else { ball(&src, &logo) })
        }
    }
}

fn open(p: &Path) -> Result<RgbaImage, String> {
    Ok(image::open(p).map_err(|e| format!("{}: {e}", p.display()))?.to_rgba8())
}

// ---------- masks ----------

#[derive(Clone)]
struct Mask {
    w: usize,
    h: usize,
    v: Vec<bool>,
}

impl Mask {
    fn from_fn(w: usize, h: usize, f: impl Fn(usize, usize) -> bool) -> Mask {
        let mut v = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                v.push(f(x, y));
            }
        }
        Mask { w, h, v }
    }
    fn not(&self) -> Mask {
        Mask { w: self.w, h: self.h, v: self.v.iter().map(|b| !b).collect() }
    }
    fn and(&self, o: &Mask) -> Mask {
        Mask { w: self.w, h: self.h, v: self.v.iter().zip(&o.v).map(|(a, b)| *a && *b).collect() }
    }
    fn plane(&self) -> Plane {
        Plane { w: self.w, h: self.h, v: self.v.iter().map(|&b| b as u8 as f32).collect() }
    }
    fn any(&self) -> bool {
        self.v.iter().any(|&b| b)
    }
    /// Distance from every texel to the nearest set one (exact Euclidean).
    fn dist(&self) -> Vec<f32> {
        let (w, h) = (self.w, self.h);
        let inf = 1e20f32;
        let mut g: Vec<f32> = self.v.iter().map(|&b| if b { 0.0 } else { inf }).collect();
        let mut line = vec![0f32; w.max(h)];
        for x in 0..w {
            for y in 0..h {
                line[y] = g[y * w + x];
            }
            let d = dt1(&line[..h]);
            for y in 0..h {
                g[y * w + x] = d[y];
            }
        }
        for y in 0..h {
            let d = dt1(&g[y * w..y * w + w]);
            g[y * w..y * w + w].copy_from_slice(&d);
        }
        g.into_iter().map(f32::sqrt).collect()
    }
    fn dilate(&self, r: f32) -> Mask {
        let d = self.dist();
        Mask { w: self.w, h: self.h, v: d.iter().map(|&v| v <= r).collect() }
    }
    fn erode(&self, r: f32) -> Mask {
        self.not().dilate(r).not()
    }
    fn close(&self, r: f32) -> Mask {
        self.dilate(r).erode(r)
    }
    /// Connected pieces (4-neighbour): label per texel (0 = none), sizes.
    fn label(&self) -> (Vec<u32>, Vec<usize>) {
        let (w, h) = (self.w, self.h);
        let mut lab = vec![0u32; w * h];
        let mut sizes = vec![0usize];
        let mut stack = Vec::new();
        for s in 0..w * h {
            if !self.v[s] || lab[s] != 0 {
                continue;
            }
            let id = sizes.len() as u32;
            let mut n = 0;
            lab[s] = id;
            stack.push(s);
            while let Some(i) = stack.pop() {
                n += 1;
                let (x, y) = (i % w, i / w);
                let mut push = |j: usize| {
                    if self.v[j] && lab[j] == 0 {
                        lab[j] = id;
                        stack.push(j);
                    }
                };
                if x > 0 { push(i - 1) }
                if x + 1 < w { push(i + 1) }
                if y > 0 { push(i - w) }
                if y + 1 < h { push(i + w) }
            }
            sizes.push(n);
        }
        (lab, sizes)
    }
    fn largest(&self) -> Mask {
        let (lab, sizes) = self.label();
        let best = (1..sizes.len()).max_by_key(|&i| sizes[i]).unwrap_or(0) as u32;
        Mask { w: self.w, h: self.h, v: lab.iter().map(|&l| l != 0 && l == best).collect() }
    }
    fn drop_small(&self, min: usize) -> Mask {
        let (lab, sizes) = self.label();
        Mask { w: self.w, h: self.h, v: lab.iter().map(|&l| l != 0 && sizes[l as usize] > min).collect() }
    }
    /// Holes (unset areas not reaching the border) filled in.
    fn fill_holes(&self) -> Mask {
        let outside = self.not();
        let (lab, _) = outside.label();
        let (w, h) = (self.w, self.h);
        let mut edge = std::collections::HashSet::new();
        for x in 0..w {
            edge.insert(lab[x]);
            edge.insert(lab[(h - 1) * w + x]);
        }
        for y in 0..h {
            edge.insert(lab[y * w]);
            edge.insert(lab[y * w + w - 1]);
        }
        Mask { w, h, v: self.v.iter().zip(&lab).map(|(&b, l)| b || !edge.contains(l)).collect() }
    }
    fn bbox(&self) -> (f32, f32, f32, f32) {
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.v[y * self.w + x] {
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
            }
        }
        (x0 as f32, x1 as f32, y0 as f32, y1 as f32)
    }
    /// Coverage with a smooth edge `soft` texels wide (signed distance).
    fn aa(&self, soft: f32) -> Plane {
        let (inside, outside) = (self.not().dist(), self.dist());
        Plane { w: self.w, h: self.h, v: inside.iter().zip(&outside).map(|(i, o)| ((i - o) / soft + 0.5).clamp(0.0, 1.0)).collect() }
    }
}

/// 1D squared distance transform (Felzenszwalb & Huttenlocher).
fn dt1(f: &[f32]) -> Vec<f32> {
    let n = f.len();
    let mut d = vec![0f32; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0f32; n + 1];
    let mut k = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        let mut s;
        loop {
            let p = v[k];
            s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] { k -= 1 } else { break }
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        d[q] = (q as f32 - p as f32).powi(2) + f[p];
    }
    d
}

// ---------- planes ----------

/// Plane value at a fractional texel position (bilinear), `out` outside.
fn sample(p: &Plane, x: f32, y: f32, out: f32) -> f32 {
    if x < 0.0 || y < 0.0 || x > (p.w - 1) as f32 || y > (p.h - 1) as f32 {
        return out;
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(p.w - 1), (y0 + 1).min(p.h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let a = p.at(x0, y0) * (1.0 - fx) + p.at(x1, y0) * fx;
    let b = p.at(x0, y1) * (1.0 - fx) + p.at(x1, y1) * fx;
    a * (1.0 - fy) + b * fy
}

/// 8x larger, smooth (texel centres line up, edges repeated).
fn up(p: &Plane) -> Plane {
    Plane::from_fn(p.w * F, p.h * F, |x, y| {
        let sx = ((x as f32 + 0.5) / F as f32 - 0.5).clamp(0.0, (p.w - 1) as f32);
        let sy = ((y as f32 + 0.5) / F as f32 - 0.5).clamp(0.0, (p.h - 1) as f32);
        sample(p, sx, sy, 0.0)
    })
}

fn argmax(fields: &[Plane]) -> Vec<usize> {
    (0..fields[0].v.len())
        .map(|i| (0..fields.len()).max_by(|&a, &b| fields[a].v[i].total_cmp(&fields[b].v[i])).unwrap())
        .collect()
}

/// Colour under see-through texels = the nearest solid colour, so the
/// game's mipmaps and filtering do not pull dark fringes into the edges.
fn bleed(rgb: &mut [[f32; 3]], solid: &Mask) {
    if !solid.any() {
        return;
    }
    let (w, h) = (solid.w, solid.h);
    let mut known = solid.v.clone();
    let mut frontier: Vec<usize> = (0..w * h).filter(|&i| known[i]).collect();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for &i in &frontier {
            let (x, y) = (i % w, i / w);
            let mut go = |j: usize| {
                if !known[j] {
                    known[j] = true;
                    rgb[j] = rgb[i];
                    next.push(j);
                }
            };
            if x > 0 { go(i - 1) }
            if x + 1 < w { go(i + 1) }
            if y > 0 { go(i - w) }
            if y + 1 < h { go(i + w) }
        }
        frontier = next;
    }
}

fn image(rgb: &[[f32; 3]], alpha: &Plane) -> RgbaImage {
    RgbaImage::from_fn(alpha.w as u32, alpha.h as u32, |x, y| {
        let i = y as usize * alpha.w + x as usize;
        let c = rgb[i].map(|v| v.round().clamp(0.0, 255.0) as u8);
        image::Rgba([c[0], c[1], c[2], (alpha.v[i] * 255.0).round().clamp(0.0, 255.0) as u8])
    })
}

// ---------- the logo ----------

/// How much of each palette colour a texel is (edge texels are blends):
/// non-negative least squares, summing to one.
fn shares(c: [f32; 3], pal: &[[f32; 3]; 3]) -> [f32; 3] {
    let mut best = ([0.0f32; 3], f32::INFINITY);
    for set in 1u32..8 {
        let idx: Vec<usize> = (0..3).filter(|i| set & (1 << i) != 0).collect();
        // rows: three colour channels, plus a heavy "sum to one" row
        let rows: Vec<(Vec<f32>, f32)> = (0..4)
            .map(|r| if r < 3 { (idx.iter().map(|&i| pal[i][r]).collect(), c[r]) } else { (vec![100.0; idx.len()], 100.0) })
            .collect();
        let n = idx.len();
        let mut a = vec![vec![0f64; n + 1]; n];
        for (row, b) in &rows {
            for i in 0..n {
                for j in 0..n {
                    a[i][j] += (row[i] * row[j]) as f64;
                }
                a[i][n] += (row[i] * b) as f64;
            }
        }
        // Gauss-Jordan
        let mut ok = true;
        for i in 0..n {
            let p = (i..n).max_by(|&x, &y| a[x][i].abs().total_cmp(&a[y][i].abs())).unwrap();
            a.swap(i, p);
            if a[i][i].abs() < 1e-9 {
                ok = false;
                break;
            }
            for r in 0..n {
                if r != i {
                    let f = a[r][i] / a[i][i];
                    for k in i..=n {
                        a[r][k] -= f * a[i][k];
                    }
                }
            }
        }
        if !ok {
            continue;
        }
        let sol: Vec<f32> = (0..n).map(|i| (a[i][n] / a[i][i]) as f32).collect();
        if sol.iter().any(|&s| s < 0.0) {
            continue;
        }
        let mut w = [0f32; 3];
        for (k, &i) in idx.iter().enumerate() {
            w[i] = sol[k];
        }
        let err: f32 = (0..3).map(|r| (pal[0][r] * w[0] + pal[1][r] * w[1] + pal[2][r] * w[2] - c[r]).powi(2)).sum::<f32>()
            + (100.0 * (w[0] + w[1] + w[2] - 1.0)).powi(2);
        if err < best.1 {
            best = (w, err);
        }
    }
    let s: f32 = best.0.iter().sum::<f32>().max(1e-6);
    best.0.map(|v| v / s)
}

/// Each logo colour's share of every texel.
fn logo_shares(src: &RgbaImage) -> [Plane; 3] {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let mut cache = std::collections::HashMap::new();
    let mut planes = [Plane::new(w, h), Plane::new(w, h), Plane::new(w, h)];
    for y in 0..h {
        for x in 0..w {
            let p = src.get_pixel(x as u32, y as u32).0;
            let s = *cache.entry([p[0], p[1], p[2]]).or_insert_with(|| shares([p[0] as f32, p[1] as f32, p[2] as f32], &LOGO_PAL));
            for c in 0..3 {
                planes[c].v[y * w + x] = s[c];
            }
        }
    }
    planes
}

/// Each logo colour's share as a smooth 8x field.
fn logo_fields(src: &RgbaImage) -> Vec<Plane> {
    logo_shares(src).iter().map(|p| up(p).blur(6.0)).collect()
}

/// Flat colours from winner-takes-all shapes, anti-aliased.
fn compose(lab: &[usize], w: usize, h: usize, pal: &[[f32; 3]]) -> (Vec<[f32; 3]>, Plane) {
    let mut rgb = vec![[0f32; 3]; w * h];
    let mut cov = Plane::new(w, h);
    for (k, c) in pal.iter().enumerate() {
        let m = Mask { w, h, v: lab.iter().map(|&l| l == k).collect() };
        let a = m.aa(1.5);
        for i in 0..w * h {
            for ch in 0..3 {
                rgb[i][ch] += a.v[i] * c[ch];
            }
            cov.v[i] += a.v[i];
        }
    }
    for i in 0..w * h {
        let n = cov.v[i].max(1e-6);
        rgb[i] = rgb[i].map(|v| v / n);
    }
    (rgb, cov)
}

fn logo(src: &RgbaImage) -> RgbaImage {
    let fields = logo_fields(src);
    let (w, h) = (fields[0].w, fields[0].h);
    let (rgb, _) = compose(&argmax(&fields), w, h, &LOGO_PAL);
    image(&rgb, &Plane::from_fn(w, h, |_, _| 1.0))
}

// ---------- the meter ----------

fn meter(src: &RgbaImage) -> RgbaImage {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let fields: Vec<Plane> = METER_PAL
        .iter()
        .map(|c| {
            // The bar and its half see-through rim are one solid shape: the
            // rim, scaled up, turns into a ragged dark border.
            let own = Mask::from_fn(sw, sh, |x, y| {
                let p = src.get_pixel(x as u32, y as u32).0;
                p[3] > 0 && (0..3).map(|i| (p[i] as f32 - c[i]).abs()).sum::<f32>() < 5.0
            });
            up(&own.close(1.5).fill_holes().plane()).blur(12.0)
        })
        .collect();
    let (w, h) = (sw * F, sh * F);
    // Each bar is grown to run under the gear, as far as halfway to the other.
    let bars: Vec<Option<Vec<f32>>> = (0..METER_PAL.len())
        .map(|k| {
            let m = Mask::from_fn(w, h, |x, y| {
                let i = y * w + x;
                fields[k].v[i] > 0.5 && fields[k].v[i] >= fields[1 - k].v[i]
            });
            m.any().then(|| m.largest().dist())
        })
        .collect();
    let mut rgb = vec![[0f32; 3]; w * h];
    let mut alpha = Plane::new(w, h);
    for (k, c) in METER_PAL.iter().enumerate() {
        let Some(near) = &bars[k] else { continue };
        let m = Mask::from_fn(w, h, |x, y| {
            let i = y * w + x;
            let mine = bars[1 - k].as_ref().is_none_or(|other| near[i] < other[i] || (near[i] == other[i] && k == 0));
            mine && near[i] <= METER_TUCK * F as f32
        });
        let a = m.aa(1.5);
        for i in 0..w * h {
            for ch in 0..3 {
                rgb[i][ch] += a.v[i] * c[ch];
            }
            alpha.v[i] += a.v[i];
        }
    }
    for i in 0..w * h {
        let n = alpha.v[i].max(1e-6);
        rgb[i] = rgb[i].map(|v| v / n);
        alpha.v[i] = alpha.v[i].min(1.0);
    }
    bleed(&mut rgb, &Mask { w, h, v: alpha.v.iter().map(|&a| a > 0.5).collect() });
    image(&rgb, &alpha)
}

// ---------- the ball's face ----------

/// The face on the full boost's ball: the gear's face in red and white, with
/// the same horns, steel rim and crisp lines, rounded by the original's light.
fn ball(src: &RgbaImage, logo_src: &RgbaImage) -> RgbaImage {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (w, h) = (sw * F, sh * F);
    let px = |x: usize, y: usize| src.get_pixel(x as u32, y as u32).0;

    // The ball's face: its solid silhouette, without stray texels.
    let solid = Mask::from_fn(sw, sh, |x, y| px(x, y)[3] >= 200);
    let body = solid.erode(1.5);
    if !body.any() {
        return image::imageops::resize(src, w as u32, h as u32, image::imageops::FilterType::CatmullRom);
    }
    let body = body.largest().dilate(1.5).and(&solid);
    let face = fit_face(src, &body, body.bbox(), logo_src);

    // The shading, from the white of the original.
    let white = Mask::from_fn(sw, sh, |x, y| {
        let p = px(x, y);
        body.v[y * sw + x] && !(p[0] as i32 - p[1] as i32 > 45 && grey(src, x, y) < 170.0)
    });
    let mut lum: Vec<[f32; 3]> = (0..sw * sh).map(|i| [grey(src, i % sw, i / sw) / 255.0; 3]).collect();
    bleed(&mut lum, &white);
    let light = up(&Plane { w: sw, h: sh, v: lum.iter().map(|c| c[0]).collect() }.blur(3.0)).blur(10.0);
    let light = light.map(|v| 1.0 - BALL_SHADE + BALL_SHADE * (v * 1.35).min(1.0));

    let (shape_a, ink_a) = (face.shape.aa(1.5), face.ink.aa(1.4));
    let rim_rgb = polish(&shape_a);
    let inside_a = face.ink.fill_holes().aa(1.5);
    let mut rgb: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            [0, 1, 2].map(|c| {
                let v = rim_rgb[i][c] * (1.0 - inside_a.v[i]) + 250.0 * light.v[i] * inside_a.v[i];
                v * (1.0 - ink_a.v[i]) + BALL_LINE[c] * light.v[i] * ink_a.v[i]
            })
        })
        .collect();
    bleed(&mut rgb, &Mask { w, h, v: shape_a.v.iter().map(|&a| a > 0.02).collect() });
    image(&rgb, &shape_a)
}

// ---------- the gear ----------

/// The recorded big logo, if the game has shown it yet.
pub fn find_logo(dump: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dump).ok()?.flatten().map(|e| e.path()).find(|p| crate::pack::hash_of(p) == Some(LOGO))
}

const TEETH: usize = 12;
/// Width of a wedge's point, in original texels.
const NECK_SRC: f32 = 2.6;
/// How much the wedges' corners are rounded (8x texels).
const WEDGE_ROUND: f32 = 5.0;
/// The wedges' steel next to the ring's: at the ring, and at the point.
const WEDGE_SHADE: (f32, f32) = (0.94, 0.70);
/// The raised band round the holes: across and down, in original texels.
const BAND_SRC: (f32, f32) = (4.3, 5.0);
/// The black line inside the ring, along both holes, in original texels. The
/// game dims the holes with an oval up to 1.5 texels short of the ring, which
/// left a bright blotchy edge on the meter.
const RING_LINE: f32 = 1.8;
/// How far the original's light and shade carries into the silver face.
const FACE_SHADE: f32 = 0.36;

/// Least-squares line x = a + b*y through (y, x) points, as (a, b).
fn line_fit(points: impl Iterator<Item = (f32, f32)> + Clone) -> (f32, f32) {
    let n = points.clone().count().max(1) as f32;
    let my = points.clone().map(|p| p.0).sum::<f32>() / n;
    let mx = points.clone().map(|p| p.1).sum::<f32>() / n;
    let cov: f32 = points.clone().map(|(y, x)| (y - my) * (x - mx)).sum();
    let var: f32 = points.map(|(y, _)| (y - my) * (y - my)).sum();
    let b = cov / var.max(1e-6);
    (mx - b * my, b)
}

fn correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().max(1) as f32;
    let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b) {
        ab += (x - ma) * (y - mb);
        aa += (x - ma) * (x - ma);
        bb += (y - mb) * (y - mb);
    }
    ab / (aa * bb).sqrt().max(1e-6)
}

/// Where the big logo sits on the gear or the ball, and its two shapes.
struct Face {
    /// The face with its horns and rim.
    shape: Mask,
    /// The dark lines.
    ink: Mask,
}

/// Lay the big logo over the little face of the gear or the ball: stretched and moved until
/// its silhouette and its dark lines both match the original's, so horns,
/// eyes, teeth and tongue are where the game has them.
fn fit_face(src: &RgbaImage, solid: &Mask, at: (f32, f32, f32, f32), logo_src: &RgbaImage) -> Face {
    let (sw, sh) = (solid.w, solid.h);
    let [_, red, dark] = logo_shares(logo_src);
    let inside = Mask { w: red.w, h: red.h, v: red.v.iter().map(|&r| r >= 0.5).collect() }.fill_holes();
    // within the logo's orange outline
    let shape = Plane::from_fn(red.w, red.h, |x, y| if inside.v[y * red.w + x] { 1.0 - red.at(x, y) } else { 0.0 });
    let lb = Mask { w: red.w, h: red.h, v: shape.v.iter().map(|&v| v > 0.5).collect() }.bbox();

    // What to match: the silhouette, and what is darker than round about it.
    let cover = solid.plane();
    let lum = Plane::from_fn(sw, sh, |x, y| if solid.v[y * sw + x] { grey(src, x, y) } else { 0.0 });
    let around = lum.blur(2.0).zip(&cover.blur(2.0), |v, n| v / n.max(1e-4));
    let dark_lines = dark.zip(&dark.blur(4.0), |v, b| v - b);
    let (mut spots, mut want_shape, mut want_lines) = (Vec::new(), Vec::new(), Vec::new());
    for y in (at.2 as usize).saturating_sub(2)..(at.3 as usize + 3).min(sh) {
        for x in (at.0 as usize).saturating_sub(2)..(at.1 as usize + 3).min(sw) {
            let i = y * sw + x;
            spots.push((x as f32, y as f32));
            want_shape.push(cover.v[i]);
            want_lines.push((around.v[i] - lum.v[i]) * cover.v[i]);
        }
    }
    let (mx, my) = ((at.0 + at.1) / 2.0, (at.2 + at.3) / 2.0);
    // logo position = scale * (gear position - middle) + offset
    let to_logo = |p: &[f32; 4], x: f32, y: f32| (p[0] * (x - mx) + p[2], p[1] * (y - my) + p[3]);
    let score = |p: &[f32; 4]| {
        let (mut s, mut d) = (Vec::with_capacity(spots.len()), Vec::with_capacity(spots.len()));
        for &(x, y) in &spots {
            let (lx, ly) = to_logo(p, x, y);
            s.push(sample(&shape, lx, ly, 0.0));
            d.push(sample(&dark_lines, lx, ly, 0.0));
        }
        correlation(&s, &want_shape) + 1.6 * correlation(&d, &want_lines)
    };
    let mut p = [(lb.1 - lb.0) / (at.1 - at.0).max(1.0), (lb.3 - lb.2) / (at.3 - at.2).max(1.0), (lb.0 + lb.1) / 2.0, (lb.2 + lb.3) / 2.0];
    let mut step = [0.1f32, 0.1, 2.0, 2.0];
    let mut best = score(&p);
    for _ in 0..10 {
        for _ in 0..40 {
            let mut moved = false;
            for i in 0..4 {
                for sign in [1.0, -1.0] {
                    let mut t = p;
                    t[i] += sign * step[i];
                    let s = score(&t);
                    if s > best {
                        (p, best, moved) = (t, s, true);
                    }
                }
            }
            if !moved {
                break;
            }
        }
        step = step.map(|s| s * 0.6);
    }

    let drawn = |field: &Plane| {
        let big = up(field).blur(2.8);
        Mask::from_fn(sw * F, sh * F, |x, y| {
            let f = F as f32;
            let (lx, ly) = to_logo(&p, (x as f32 + 0.5) / f - 0.5, (y as f32 + 0.5) / f - 0.5);
            sample(&big, (lx + 0.5) * f - 0.5, (ly + 0.5) * f - 0.5, 0.0) > 0.5
        })
    };
    let shape = drawn(&shape);
    let ink = drawn(&dark).and(&shape);
    Face { shape, ink }
}

/// The frames' steel, a little softer: highlights brought down, shadows up.
fn polish(shape: &Plane) -> Vec<[f32; 3]> {
    crate::redraw::steel(shape).0.into_iter().map(|c| c.map(|v| 38.0 + v * 0.78)).collect()
}

fn grey(src: &RgbaImage, x: usize, y: usize) -> f32 {
    let p = src.get_pixel(x as u32, y as u32).0;
    (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0
}

/// The boost gear, as the original is built but clean: twelve identical
/// smooth teeth, the raised ring with its two wedges, and the mascot with
/// horns, rim and the big logo's crisp lines, all in polished steel with a
/// soft shadow in the holes.
fn gear(src: &RgbaImage, logo_src: &RgbaImage) -> RgbaImage {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (w, h) = (sw * F, sh * F);
    let fs = F as f32;
    let peak = src.pixels().map(|p| p[3]).max().unwrap_or(255).max(1) as f32;
    let alpha_src = Plane::from_fn(sw, sh, |x, y| src.get_pixel(x as u32, y as u32)[3] as f32 / peak);
    let solid_src = Mask::from_fn(sw, sh, |x, y| alpha_src.at(x, y) > 0.75);

    // ---- outline: one tooth profile, repeated
    let ah = up(&alpha_src).blur(6.5);
    let filled = Mask { w, h, v: ah.v.iter().map(|&a| a > 0.48).collect() }.fill_holes().largest();
    let (mut sx, mut sy, mut n) = (0f64, 0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            if filled.v[y * w + x] {
                sx += x as f64;
                sy += y as f64;
                n += 1.0;
            }
        }
    }
    let (cx, cy) = ((sx / n.max(1.0)) as f32, (sy / n.max(1.0)) as f32);
    let fb = filled.bbox();
    let k = (fb.3 - fb.2) / (fb.1 - fb.0).max(1.0);
    const A: usize = 3600;
    let rmax = w as f32 * 0.46;
    let prof: Vec<f32> = (0..A)
        .map(|i| {
            let t = -std::f32::consts::PI + i as f32 / A as f32 * std::f32::consts::TAU;
            let (c, s) = (t.cos(), t.sin());
            let mut best = 0.0;
            let mut r = rmax * 0.2;
            while r < rmax {
                let (px, py) = (cx + r * c, cy + r * s * k);
                if px >= 0.0 && py >= 0.0 && (px as usize) < w && (py as usize) < h && filled.v[py as usize * w + px as usize] {
                    best = r;
                }
                r += 0.5;
            }
            best
        })
        .collect();
    let per = A / TEETH;
    let mut fold: Vec<f32> = (0..per)
        .map(|j| {
            let mut v: Vec<f32> = (0..TEETH).map(|t| prof[t * per + j]).collect();
            v.sort_by(f32::total_cmp);
            (v[TEETH / 2 - 1] + v[TEETH / 2]) / 2.0
        })
        .collect();
    for _ in 0..3 {
        let r = 3usize;
        fold = (0..per).map(|j| (0..=2 * r).map(|d| fold[(j + per * 2 + d - r) % per]).sum::<f32>() / (2 * r + 1) as f32).collect();
    }
    let outline = Mask::from_fn(w, h, |x, y| {
        let (u, v) = (x as f32 - cx, (y as f32 - cy) / k);
        let fi = (v.atan2(u) + std::f32::consts::PI) / std::f32::consts::TAU * A as f32;
        let j0 = fi.floor() as usize % A;
        let f = fi - fi.floor();
        let r = fold[j0 % per] * (1.0 - f) + fold[(j0 + 1) % per] * f;
        r > (u * u + v * v).sqrt()
    });

    // ---- the ring: an ellipse through the holes' outer edges
    let holes_src = solid_src.fill_holes().and(&solid_src.not()).drop_small(40);
    let hb = if holes_src.any() { holes_src.bbox() } else { (sw as f32 * 0.25, sw as f32 * 0.75, sh as f32 * 0.25, sh as f32 * 0.75) };
    let (ex, ey, rx) = ((hb.0 + hb.1) / 2.0 + 0.5, (hb.2 + hb.3) / 2.0 + 0.5, (hb.1 - hb.0) / 2.0 + 0.5);
    // the holes end at the wedges' corners, short of the ellipse's top
    let corner: Vec<f32> = (0..sw).filter(|&x| holes_src.v[hb.2 as usize * sw + x]).map(|x| ((x as f32 + 0.5 - ex) / rx).abs()).collect();
    let corner = (corner.iter().sum::<f32>() / corner.len().max(1) as f32).min(0.6);
    let ry = ((hb.3 - hb.2) / 2.0 + 0.5) / (1.0 - corner * corner).sqrt();
    let ellipse = |rx: f32, ry: f32| {
        Mask::from_fn(w, h, |x, y| {
            let (dx, dy) = (((x as f32 + 0.5) / fs - ex) / rx, ((y as f32 + 0.5) / fs - ey) / ry);
            dx * dx + dy * dy < 1.0
        })
    };
    let inner = ellipse(rx, ry);

    // ---- where the face is
    let middle = Mask::from_fn(sw, sh, |x, y| {
        let (dx, dy) = ((x as f32 + 0.5 - ex) / rx, (y as f32 + 0.5 - ey) / ry);
        solid_src.v[y * sw + x] && dx * dx + dy * dy < 0.85
    })
    .erode(2.5);
    let face_box = middle.any().then(|| middle.largest().dilate(2.5).bbox());
    let (ft, fbm) = face_box.map(|b| (b.2, b.3)).unwrap_or((ey - ry * 0.4, ey + ry * 0.4));

    // ---- the two wedges, one above the face and one below
    // the solid run through the middle of a row of the original: its edges
    let run = |y: usize| -> Option<(f32, f32)> {
        let c = ex as usize;
        if !solid_src.v[y * sw + c] {
            return None;
        }
        let (mut l, mut r) = (c, c);
        while l > 0 && solid_src.v[y * sw + l - 1] { l -= 1 }
        while r + 1 < sw && solid_src.v[y * sw + r + 1] { r += 1 }
        // half a texel out on each side, into the half-covered edge
        Some((l as f32 - 0.5, r as f32 + 1.5))
    };
    let top_rows = (hb.2 as usize + 1)..(ft as usize).max(hb.2 as usize + 3);
    let bot_rows = (fbm as usize + 1)..(hb.3 as usize).max(fbm as usize + 2);
    let mut wedges = Mask::from_fn(w, h, |_, _| false);
    // each runs from the ring, narrowing, to just behind the face
    let (stop_top, stop_bot) = (ft + (fbm - ft) * 0.25, fbm - (fbm - ft) * 0.25);
    for (rows, upper) in [(top_rows, true), (bot_rows, false)] {
        let runs: Vec<(usize, (f32, f32))> = rows.filter(|&y| y < sh).filter_map(|y| Some((y, run(y)?))).collect();
        let Some(&(tip, _)) = runs.iter().min_by(|a, b| (a.1 .1 - a.1 .0).total_cmp(&(b.1 .1 - b.1 .0))) else { continue };
        let side: Vec<_> = runs.into_iter().filter(|&(y, _)| if upper { y <= tip } else { y >= tip }).collect();
        if side.len() < 2 {
            continue;
        }
        let left = line_fit(side.iter().map(|&(y, (l, _))| (y as f32 + 0.5, l)));
        let right = line_fit(side.iter().map(|&(y, (_, r))| (y as f32 + 0.5, r)));
        for y in 0..h {
            let sy = (y as f32 + 0.5) / fs;
            if (upper && sy >= stop_top) || (!upper && sy <= stop_bot) {
                continue;
            }
            // straight sides down to a narrow neck, which carries on to the face
            let (mut l, mut r) = (left.0 + left.1 * sy, right.0 + right.1 * sy);
            if r - l < NECK_SRC {
                (l, r) = (ex - NECK_SRC / 2.0, ex + NECK_SRC / 2.0);
            }
            for x in 0..w {
                let sx = (x as f32 + 0.5) / fs;
                if sx >= l && sx <= r {
                    wedges.v[y * w + x] = true;
                }
            }
        }
    }
    // rounded where they meet the ring, as the original's are
    let holes = inner.and(&wedges.not()).plane().blur(WEDGE_ROUND);
    let open = Mask { w, h, v: holes.v.iter().map(|&v| v > 0.5).collect() };
    let body = outline.and(&open.not());
    let line_a = ellipse(rx - RING_LINE, ry - RING_LINE).not().aa(1.5);
    let band = body.and(&ellipse(rx + BAND_SRC.0, ry + BAND_SRC.1));

    let face = match face_box {
        Some(at) => fit_face(src, &solid_src, at, logo_src),
        None => Face { shape: Mask::from_fn(w, h, |_, _| false), ink: Mask::from_fn(w, h, |_, _| false) },
    };

    // ---- polished steel, a piece at a time so each gets its own bevel:
    // teeth, then the band with the wedges, then the face's rim
    let (body_a, band_a, shape_a, ink_a) = (body.aa(1.5), band.aa(3.0), face.shape.aa(1.5), face.ink.aa(1.4));
    let (body_rgb, band_rgb, rim_rgb) = (polish(&body_a), polish(&band_a), polish(&shape_a));
    let in_ring = inner.aa(10.0);
    let (top, bottom) = ((ey - ry) * fs, (ey + ry) * fs);

    // the silver inside the face's dark outline, shaded a little as the original is
    let silver_a = face.ink.fill_holes().aa(1.5);
    let weight = solid_src.plane().blur(2.5);
    let lit = Plane::from_fn(sw, sh, |x, y| if solid_src.v[y * sw + x] { grey(src, x, y) / 255.0 } else { 0.0 });
    let lit = up(&lit.blur(2.5).zip(&weight, |v, n| v / n.max(1e-4))).blur(8.0);

    let mut rgb = vec![[0f32; 3]; w * h];
    let mut alpha = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let fy = y as f32;
            let depth = if fy < ey * fs { (fy - top) / (ft * fs - top).max(1.0) } else { (bottom - fy) / (bottom - fbm * fs).max(1.0) };
            let shade = WEDGE_SHADE.0 + (WEDGE_SHADE.1 - WEDGE_SHADE.0) * depth.clamp(0.0, 1.0);
            let shade = 1.0 - (1.0 - shade) * in_ring.v[i];
            let light = 1.0 - FACE_SHADE + FACE_SHADE * (lit.v[i] * 1.5).min(1.0);
            let silver = [236.0 * light, 239.0 * light, 246.0 * light];
            rgb[i] = [0, 1, 2].map(|c| {
                let mix = |under: f32, over: f32, a: f32| under * (1.0 - a) + over * a;
                let v = mix(body_rgb[i][c], band_rgb[i][c] * shade, band_a.v[i]);
                let v = mix(v, rim_rgb[i][c], shape_a.v[i]);
                let v = mix(v, silver[c], silver_a.v[i]);
                mix(v, 12.0, ink_a.v[i])
            });
            alpha.v[i] = body_a.v[i].max(shape_a.v[i]);
        }
    }
    // ---- soft shadow in the holes, cast down and to the right
    let solid_now = Mask { w, h, v: alpha.v.iter().map(|&a| a > 0.5).collect() };
    let (ox, oy) = (9usize, 12usize);
    let cast = Plane::from_fn(w, h, |x, y| if x >= ox && y >= oy && solid_now.v[(y - oy) * w + x - ox] { 1.0 } else { 0.0 }).blur(6.0);
    let in_holes = outline.and(&solid_now.not());
    for i in 0..w * h {
        if in_holes.v[i] {
            let a = alpha.v[i];
            let a2 = a + (1.0 - a) * cast.v[i] * 0.6;
            if a2 > 0.0 {
                rgb[i] = rgb[i].map(|c| c * a / a2);
            }
            alpha.v[i] = a2;
        }
    }
    // ---- the black line inside the ring, under the steel and the face
    let hole_a = open.dilate(2.0).aa(1.5);
    for i in 0..w * h {
        let metal = body_a.v[i].max(shape_a.v[i]);
        let l = line_a.v[i] * hole_a.v[i] * (1.0 - metal);
        let a = alpha.v[i] + (1.0 - alpha.v[i]) * l;
        if a > 0.0 && l > 0.0 {
            rgb[i] = rgb[i].map(|c| (c * alpha.v[i] * (1.0 - l) + 8.0 * l) / a);
            alpha.v[i] = a;
        }
    }
    bleed(&mut rgb, &Mask { w, h, v: alpha.v.iter().map(|&a| a > 0.02).collect() });
    let alpha = alpha.map(|a| a * peak / 255.0);
    image(&rgb, &alpha)
}
