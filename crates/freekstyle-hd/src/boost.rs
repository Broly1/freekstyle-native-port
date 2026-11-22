//! The boost HUD, redrawn from the player's own recording: the gear, the
//! big devil logo, the face on the boost ball and the red and gold meter.
//!
//! The logo, ball face and meter are a few flat colours, so no upscaler is
//! needed (it only smears them): each colour becomes its own smooth shape
//! at 8x and the shapes are put back together with crisp, anti-aliased
//! edges. The ball's face is too small to redraw from itself, so it takes
//! its lines from the big logo, fitted to where the ball's face sits. The
//! gear keeps its own outline (one tooth, repeated), gets steel, see-through
//! holes with a soft shadow, two thin X bars, and the logo's face. Nothing
//! of the game's art is kept in the source: it all comes from the recording.

use std::path::Path;

use image::RgbaImage;

use crate::plane::Plane;

const F: usize = 8;
pub const LOGO: &str = "9606d5c1f9478577";
pub const BALL: &str = "17941779c37d9812";
pub const METER: &str = "72ec3c5f7b4eef11";

/// Logo colours: white, orange-red outline, dark-red lines.
const LOGO_PAL: [[f32; 3]; 3] = [[248.0, 248.0, 248.0], [248.0, 40.0, 0.0], [96.0, 0.0, 0.0]];
const METER_PAL: [[f32; 3]; 2] = [[188.0, 0.0, 16.0], [212.0, 180.0, 96.0]];
const BALL_LINE: [f32; 3] = [125.0, 18.0, 22.0];

pub fn is_boost(hash: &str) -> bool {
    [LOGO, BALL, METER].contains(&hash)
}

/// Redraw one of the three; `dump` is the recording it came from.
pub fn draw(hash: &str, path: &Path, dump: &Path) -> Result<RgbaImage, String> {
    let src = open(path)?;
    match hash {
        LOGO => Ok(logo(&src)),
        METER => Ok(meter(&src)),
        _ => {
            match find_logo(dump) {
                Some(l) => Ok(ball(&src, &open(&l)?)),
                // The logo is recorded the first time boost is used, as the
                // ball is; without it the ball is only scaled smoothly.
                None => Ok(image::imageops::resize(&src, src.width() * F as u32, src.height() * F as u32, image::imageops::FilterType::CatmullRom)),
            }
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

/// Each logo colour's share as a smooth 8x field.
fn logo_fields(src: &RgbaImage) -> Vec<Plane> {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let mut cache = std::collections::HashMap::new();
    let mut planes = vec![Plane::new(w, h), Plane::new(w, h), Plane::new(w, h)];
    for y in 0..h {
        for x in 0..w {
            let p = src.get_pixel(x as u32, y as u32).0;
            let s = *cache.entry([p[0], p[1], p[2]]).or_insert_with(|| shares([p[0] as f32, p[1] as f32, p[2] as f32], &LOGO_PAL));
            for c in 0..3 {
                planes[c].v[y * w + x] = s[c];
            }
        }
    }
    planes.iter().map(|p| up(p).blur(6.0)).collect()
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
    let mut rgb = vec![[0f32; 3]; w * h];
    let mut alpha = Plane::new(w, h);
    for (k, c) in METER_PAL.iter().enumerate() {
        let o = 1 - k;
        let m = Mask::from_fn(w, h, |x, y| {
            let i = y * w + x;
            fields[k].v[i] > 0.5 && fields[k].v[i] >= fields[o].v[i]
        });
        if !m.any() {
            continue;
        }
        let a = m.largest().aa(1.5);
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

fn ball(src: &RgbaImage, logo_src: &RgbaImage) -> RgbaImage {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (w, h) = (sw * F, sh * F);
    let px = |x: usize, y: usize| src.get_pixel(x as u32, y as u32).0;

    // The logo: its face (with outline) and where it sits.
    let fields = logo_fields(logo_src);
    let (lw, lh) = (fields[0].w, fields[0].h);
    let lab_l = argmax(&fields);
    let dark_l = Mask { w: lw, h: lh, v: lab_l.iter().map(|&l| l == 2).collect() };
    let face_l = dark_l.close(4.5).fill_holes();
    let lb = face_l.bbox();
    let face_lp = face_l.plane();

    // The ball's face: its solid silhouette, without stray texels.
    let sil = Mask::from_fn(sw, sh, |x, y| px(x, y)[3] >= 200);
    let er = Mask::from_fn(sw, sh, |x, y| sil.v[y * sw + x] && x + 1 < sw && y + 1 < sh && sil.v[y * sw + x + 1] && sil.v[(y + 1) * sw + x] && sil.v[(y + 1) * sw + x + 1]);
    let opened = Mask::from_fn(sw, sh, |x, y| {
        let g = |dx: usize, dy: usize| x >= dx && y >= dy && er.v[(y - dy) * sw + x - dx];
        g(0, 0) || g(1, 0) || g(0, 1) || g(1, 1)
    });
    if !opened.any() {
        return image::imageops::resize(src, w as u32, h as u32, image::imageops::FilterType::CatmullRom);
    }
    let b = opened.largest().bbox();
    let bb = (b.0 + 0.5, b.1 + 0.5, b.2 + 0.5, b.3 + 0.5);

    // Logo coordinates of every output texel.
    let to_logo = |x: usize, y: usize| {
        let (bx, by) = (x as f32 / F as f32, y as f32 / F as f32);
        let lx = lb.0 + (bx - bb.0) * (lb.1 - lb.0) / (bb.1 - bb.0);
        let ly = lb.2 + (by - bb.2) * (lb.3 - lb.2) / (bb.3 - bb.2);
        (lx, ly)
    };
    let mut lines = Mask::from_fn(w, h, |_, _| false);
    let mut face = Mask::from_fn(w, h, |_, _| false);
    for y in 0..h {
        for x in 0..w {
            let (lx, ly) = to_logo(x, y);
            let s: Vec<f32> = (0..3).map(|c| sample(&fields[c], lx, ly, if c == 0 { 1.0 } else { 0.0 })).collect();
            let i = y * w + x;
            lines.v[i] = s[2] > s[0] && s[2] > s[1];
            face.v[i] = sample(&face_lp, lx, ly, 0.0) > 0.5;
        }
    }
    // A thin white rim round the outline, like the original.
    let face = face.dilate(3.0);
    // The logo's lines are drawn bolder than the ball's: thin them.
    let lines = lines.and(&face).erode(4.0).drop_small(150).plane().blur(2.5);
    let lines = Mask { w, h, v: lines.v.iter().map(|&v| v > 0.5).collect() };

    // The ball's shading, from the white of the original.
    let red = Mask::from_fn(sw, sh, |x, y| {
        let p = px(x, y);
        p[3] > 0 && p[0] as i32 - p[1] as i32 > 45 && (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0 < 170.0
    });
    let white = Mask::from_fn(sw, sh, |x, y| px(x, y)[3] > 0 && !red.v[y * sw + x]);
    let mut lum: Vec<[f32; 3]> = (0..sw * sh)
        .map(|i| {
            let p = px(i % sw, i / sw);
            [(p[0] as f32 + p[1] as f32 + p[2] as f32) / 765.0; 3]
        })
        .collect();
    bleed(&mut lum, &white);
    let lum = Plane { w: sw, h: sh, v: lum.iter().map(|c| c[0]).collect() }.blur(1.2);
    let shade = up(&lum).blur(10.0);
    let mut inner: Vec<f32> = (0..w * h).filter(|&i| face.v[i] && !lines.v[i]).map(|i| shade.v[i]).collect();
    inner.sort_by(f32::total_cmp);
    let top = inner.get(inner.len() * 97 / 100).copied().unwrap_or(1.0).max(1e-3);
    let shade = shade.map(|v| (v / top).clamp(0.7, 1.0).powf(0.8));

    let la = lines.aa(1.4);
    let alpha = face.aa(1.5);
    let mut rgb: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let wv = 250.0 * shade.v[i];
            [0, 1, 2].map(|c| wv * (1.0 - la.v[i]) + BALL_LINE[c] * la.v[i])
        })
        .collect();
    bleed(&mut rgb, &Mask { w, h, v: alpha.v.iter().map(|&a| a > 0.5).collect() });
    image(&rgb, &alpha)
}

// ---------- the gear ----------

/// The recorded big logo, if boost has been used yet.
pub fn find_logo(dump: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dump).ok()?.flatten().map(|e| e.path()).find(|p| crate::pack::hash_of(p) == Some(LOGO))
}

const TEETH: usize = 12;
/// Width of the steel rim round the face, in original texels.
const RIM_SRC: f32 = 1.5;
/// How much the logo's ink lines are thinned for the gear (8x texels).
const INK_THIN: f32 = 3.0;

/// The boost gear: twelve identical smooth teeth and the ring from the
/// gear's own outline, shaded as polished steel, a soft shadow in the
/// holes, and the mascot's face with crisp ink lines taken from the big
/// logo, set in a steel rim.
pub fn gear(src: &RgbaImage, logo_src: &RgbaImage) -> RgbaImage {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (w, h) = (sw * F, sh * F);
    let peak = src.pixels().map(|p| p[3]).max().unwrap_or(255).max(1) as f32;
    let alpha_src = Plane::from_fn(sw, sh, |x, y| src.get_pixel(x as u32, y as u32)[3] as f32 / peak);
    let solid_src = Mask::from_fn(sw, sh, |x, y| alpha_src.at(x, y) > 0.75);

    // ---- outline: one tooth profile, repeated
    let ah = up(&alpha_src).blur(12.0);
    let filled = Mask { w, h, v: ah.v.iter().map(|&a| a > 0.42).collect() }.fill_holes().largest();
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
        let r = 5usize;
        fold = (0..per).map(|j| (0..=2 * r).map(|d| fold[(j + per * 2 + d - r) % per]).sum::<f32>() / (2 * r + 1) as f32).collect();
    }
    let outer = Plane::from_fn(w, h, |x, y| {
        let (u, v) = (x as f32 - cx, (y as f32 - cy) / k);
        let fi = (v.atan2(u) + std::f32::consts::PI) / std::f32::consts::TAU * A as f32;
        let j0 = fi.floor() as usize % A;
        let f = fi - fi.floor();
        let r = fold[j0 % per] * (1.0 - f) + fold[(j0 + 1) % per] * f;
        r - (u * u + v * v).sqrt()
    });
    let inside_outline = Mask { w, h, v: outer.v.iter().map(|&d| d > 0.0).collect() };

    // ---- the face: where it sits in the gear, drawn from the logo
    let holes_src = solid_src.fill_holes().and(&solid_src.not()).drop_small(40);
    let face_box = if holes_src.any() {
        let hb = holes_src.bbox();
        let (ex, ey, rx, ry) = ((hb.0 + hb.1) / 2.0, (hb.2 + hb.3) / 2.0, (hb.1 - hb.0) / 2.0, (hb.3 - hb.2) / 2.0);
        let inner = Mask::from_fn(sw, sh, |x, y| {
            let (dx, dy) = ((x as f32 - ex) / rx.max(1.0), (y as f32 - ey) / ry.max(1.0));
            solid_src.v[y * sw + x] && dx * dx + dy * dy < 0.95
        });
        let blob = inner.erode(2.5);
        if blob.any() { Some(blob.largest().dilate(2.5).bbox()) } else { None }
    } else {
        None
    };
    // ---- inside the ring: an ellipse, see-through but for two small X's
    // (one above the face, one below) and the face
    let hb = if holes_src.any() { holes_src.bbox() } else { (sw as f32 * 0.25, sw as f32 * 0.75, sh as f32 * 0.25, sh as f32 * 0.75) };
    let (ex, ey, rx, ry) = ((hb.0 + hb.1) / 2.0 + 0.5, (hb.2 + hb.3) / 2.0 + 0.5, (hb.1 - hb.0) / 2.0 + 0.5, (hb.3 - hb.2) / 2.0 + 0.5);
    let inner = Plane::from_fn(w, h, |x, y| {
        let (dx, dy) = (((x as f32 + 0.5) / F as f32 - ex) / rx, ((y as f32 + 0.5) / F as f32 - ey) / ry);
        dx * dx + dy * dy
    });
    let inner = Mask { w, h, v: inner.v.iter().map(|&d| d < 1.0).collect() };
    // width of the solid run through the middle of a row of the original
    let run = |y: usize| -> f32 {
        let c = ex as usize;
        if !solid_src.v[y * sw + c] {
            return 0.0;
        }
        let (mut l, mut r) = (c, c);
        while l > 0 && solid_src.v[y * sw + l - 1] { l -= 1 }
        while r + 1 < sw && solid_src.v[y * sw + r + 1] { r += 1 }
        (r - l + 1) as f32
    };
    let (ft, fbm) = face_box.map(|b| (b.2, b.3)).unwrap_or((ey - ry * 0.4, ey + ry * 0.4));
    let top_rows = (hb.2 as usize + 2)..(ft as usize).max(hb.2 as usize + 3);
    let bot_rows = (fbm as usize + 1)..(hb.3 as usize).saturating_sub(1).max(fbm as usize + 2);
    let narrowest = |rows: std::ops::Range<usize>| rows.filter(|&y| y < sh && run(y) > 0.0).min_by(|&a, &b| run(a).total_cmp(&run(b)));
    let mut bars = Mask::from_fn(w, h, |_, _| false);
    // each X runs from the ring to just behind the face, no further
    let (stop_top, stop_bot) = ((ft + (fbm - ft) * 0.25) * F as f32, (fbm - (fbm - ft) * 0.25) * F as f32);
    for (rows, edge_row, upper) in [(top_rows, hb.2 as usize + 2, true), (bot_rows, (hb.3 as usize).saturating_sub(2), false)] {
        let Some(yc) = narrowest(rows) else { continue };
        let wc = run(yc);
        let we = run(edge_row).max(wc + 2.0);
        let spread = ((we - wc) / 2.0 / (yc as f32 - edge_row as f32).abs().max(1.0)).clamp(0.2, 1.2);
        let bw = (wc * 0.4).clamp(1.2, 2.5) * F as f32 / 2.0;
        let (px, py) = (ex * F as f32, (yc as f32 + 0.5) * F as f32);
        for sgn in [-1.0f32, 1.0] {
            let (dx, dy) = (sgn * spread, 1.0);
            let len = (dx * dx + dy * dy).sqrt();
            let (nx, ny) = (-dy / len, dx / len);
            for y in 0..h {
                for x in 0..w {
                    let d = ((x as f32 + 0.5 - px) * nx + (y as f32 + 0.5 - py) * ny).abs();
                    if d < bw && if upper { (y as f32) < stop_top } else { (y as f32) > stop_bot } {
                        bars.v[y * w + x] = true;
                    }
                }
            }
        }
    }
    let bars = bars.and(&inner);

    let fields = logo_fields(logo_src);
    let (lw, lh) = (fields[0].w, fields[0].h);
    let lab_l = argmax(&fields);
    let dark_l = Mask { w: lw, h: lh, v: lab_l.iter().map(|&l| l == 2).collect() };
    let face_l = dark_l.close(4.5).fill_holes();
    let lb = face_l.bbox();
    let face_lp = face_l.plane();
    let mut face = Mask::from_fn(w, h, |_, _| false);
    let mut ink = Mask::from_fn(w, h, |_, _| false);
    if let Some(b) = face_box {
        let m = RIM_SRC;
        let bb = (b.0 + 0.5 + m, b.1 + 0.5 - m, b.2 + 0.5 + m, b.3 + 0.5 - m);
        for y in 0..h {
            for x in 0..w {
                let (bx, by) = (x as f32 / F as f32, y as f32 / F as f32);
                let lx = lb.0 + (bx - bb.0) * (lb.1 - lb.0) / (bb.1 - bb.0);
                let ly = lb.2 + (by - bb.2) * (lb.3 - lb.2) / (bb.3 - bb.2);
                let s: Vec<f32> = (0..3).map(|c| sample(&fields[c], lx, ly, if c == 0 { 1.0 } else { 0.0 })).collect();
                let i = y * w + x;
                ink.v[i] = s[2] > s[0] && s[2] > s[1];
                face.v[i] = sample(&face_lp, lx, ly, 0.0) > 0.5;
            }
        }
    }
    let ink = ink.and(&face).erode(INK_THIN).drop_small(150);
    let ink = Mask { w, h, v: ink.plane().blur(2.0).v.iter().map(|&v| v > 0.5).collect() };
    let rim = face.dilate(RIM_SRC * F as f32);

    // ---- steel: ring, teeth, X bars and the rim round the face
    let gear_shape = Mask { w, h, v: inside_outline.v.iter().zip(&inner.v).zip(&bars.v).map(|((&o, &i), &x)| o && (!i || x)).collect() };
    let metal = Mask { w, h, v: gear_shape.v.iter().zip(&rim.v).zip(&face.v).map(|((&g, &r), &f)| (g || r) && !f).collect() };
    let metal_a = metal.aa(1.5);
    // the frames' steel, a little softer: highlights brought down, shadows up
    let steel_rgb: Vec<[f32; 3]> = crate::redraw::steel(&metal_a).0.into_iter().map(|c| c.map(|v| 38.0 + v * 0.78)).collect();
    let face_a = face.aa(1.5);
    let ink_a = ink.aa(1.4);
    let fbx = face.bbox();
    let (fcx, fcy) = (fbx.0 * 0.62 + fbx.1 * 0.38, fbx.2 * 0.7 + fbx.3 * 0.3);
    let fr = (fbx.1 - fbx.0).max(fbx.3 - fbx.2).max(1.0);
    let mut rgb = vec![[0f32; 3]; w * h];
    let mut alpha = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            // brushed silver face: a broad soft highlight up and to the left
            let d = (((x as f32 - fcx).powi(2) + (y as f32 - fcy).powi(2)).sqrt() / fr).min(1.0);
            let l = 0.66 + 0.30 * (1.0 - d * d);
            let silver = [222.0 * l, 225.0 * l, 232.0 * l];
            let fc: [f32; 3] = [0, 1, 2].map(|c| silver[c] * (1.0 - ink_a.v[i]) + 12.0 * ink_a.v[i]);
            let m = metal_a.v[i];
            let f = face_a.v[i] * (1.0 - m);
            let cover = (m + f).min(1.0);
            let a = cover;
            if a > 0.0 {
                rgb[i] = [0, 1, 2].map(|c| (steel_rgb[i][c] * m + fc[c] * f) / a);
            }
            alpha.v[i] = a;
        }
    }
    // ---- soft shadow in the holes, cast down and to the right
    let solid_now = Mask { w, h, v: alpha.v.iter().map(|&a| a > 0.5).collect() };
    let (ox, oy) = (9usize, 12usize);
    let cast = Plane::from_fn(w, h, |x, y| if x >= ox && y >= oy && solid_now.v[(y - oy) * w + x - ox] { 1.0 } else { 0.0 }).blur(6.0);
    let in_holes = filled.and(&solid_now.not());
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
    bleed(&mut rgb, &Mask { w, h, v: alpha.v.iter().map(|&a| a > 0.02).collect() });
    let alpha = alpha.map(|a| a * peak / 255.0);
    image(&rgb, &alpha)
}
