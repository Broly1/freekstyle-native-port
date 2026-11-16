//! A grey picture as floats, and the few operations the tools need on it
//! (numpy's, as the Python tools these replace used them).

use image::{imageops::FilterType, GrayImage, RgbImage};

#[derive(Clone)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub v: Vec<f32>,
}

impl Plane {
    pub fn new(w: usize, h: usize) -> Plane {
        Plane { w, h, v: vec![0.0; w * h] }
    }

    pub fn from_fn(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Plane {
        let mut p = Plane::new(w, h);
        for y in 0..h {
            for x in 0..w {
                p.v[y * w + x] = f(x, y);
            }
        }
        p
    }

    /// 0..1 from a grey picture.
    pub fn from_gray(im: &GrayImage) -> Plane {
        Plane { w: im.width() as usize, h: im.height() as usize, v: im.as_raw().iter().map(|&b| b as f32 / 255.0).collect() }
    }

    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.v[y * self.w + x]
    }

    pub fn map(&self, f: impl Fn(f32) -> f32) -> Plane {
        Plane { w: self.w, h: self.h, v: self.v.iter().map(|&a| f(a)).collect() }
    }

    pub fn zip(&self, o: &Plane, f: impl Fn(f32, f32) -> f32) -> Plane {
        Plane { w: self.w, h: self.h, v: self.v.iter().zip(&o.v).map(|(&a, &b)| f(a, b)).collect() }
    }

    /// Mean over a 2r+1 window along one axis, edges repeated.
    fn box_axis(&self, r: usize, vertical: bool) -> Plane {
        let (n, lines) = if vertical { (self.h, self.w) } else { (self.w, self.h) };
        let idx = |line: usize, i: usize| if vertical { i * self.w + line } else { line * self.w + i };
        let mut out = Plane::new(self.w, self.h);
        let norm = 1.0 / (2 * r + 1) as f64;
        for line in 0..lines {
            let get = |i: isize| self.v[idx(line, i.clamp(0, n as isize - 1) as usize)] as f64;
            let mut sum: f64 = (-(r as isize)..=r as isize).map(get).sum();
            for i in 0..n {
                out.v[idx(line, i)] = (sum * norm) as f32;
                sum += get(i as isize + r as isize + 1) - get(i as isize - r as isize);
            }
        }
        out
    }

    /// Gaussian blur, as three box blurs.
    pub fn blur(&self, sigma: f32) -> Plane {
        let r = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as usize;
        let mut a = self.clone();
        for _ in 0..3 {
            a = a.box_axis(r, true).box_axis(r, false);
        }
        a
    }

    /// (d/dy, d/dx): central differences, one-sided at the edges.
    pub fn gradient(&self) -> (Plane, Plane) {
        let d = |n: usize, i: usize, f: &dyn Fn(usize) -> f32| -> f32 {
            if n < 2 {
                0.0
            } else if i == 0 {
                f(1) - f(0)
            } else if i == n - 1 {
                f(n - 1) - f(n - 2)
            } else {
                (f(i + 1) - f(i - 1)) / 2.0
            }
        };
        let dy = Plane::from_fn(self.w, self.h, |x, y| d(self.h, y, &|j| self.at(x, j)));
        let dx = Plane::from_fn(self.w, self.h, |x, y| d(self.w, x, &|i| self.at(i, y)));
        (dy, dx)
    }
}

/// Grey value as PIL's `convert("L")` makes it.
pub fn luma(p: [u8; 3]) -> u8 {
    ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8
}

pub fn to_gray(im: &RgbImage) -> GrayImage {
    GrayImage::from_fn(im.width(), im.height(), |x, y| image::Luma([luma(im.get_pixel(x, y).0)]))
}

pub fn resize_rgb(im: &RgbImage, w: u32, h: u32, filter: FilterType) -> RgbImage {
    image::imageops::resize(im, w, h, filter)
}

pub fn resize_gray(im: &GrayImage, w: u32, h: u32, filter: FilterType) -> GrayImage {
    image::imageops::resize(im, w, h, filter)
}

/// numpy's `interp`: piecewise linear through (xp, fp), clamped at the ends.
pub fn interp(t: f32, xp: &[f32], fp: &[f32]) -> f32 {
    if t <= xp[0] {
        return fp[0];
    }
    for i in 1..xp.len() {
        if t <= xp[i] {
            let f = (t - xp[i - 1]) / (xp[i] - xp[i - 1]);
            return fp[i - 1] + (fp[i] - fp[i - 1]) * f;
        }
    }
    fp[fp.len() - 1]
}
