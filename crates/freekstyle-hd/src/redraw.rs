//! The chrome menu frames and the boost gear, redrawn (was
//! tools/redraw_art.py).
//!
//! Their originals are too small for any upscaler to give clean edges, so
//! only their shapes are kept: each outline is cleaned by the line-art
//! model, smoothed at 8x and shaded from scratch as polished steel. The
//! mascot's face keeps its art, sharpened toward ink.

use std::path::{Path, PathBuf};

use image::{imageops::FilterType, GrayImage, Rgb, RgbImage, RgbaImage};

use crate::esrgan;
use crate::plane::{interp, resize_gray, resize_rgb, to_gray, Plane};

const SCALE: u32 = 8;
/// Pieces of the chrome menu frame, in the palettes the game shows them.
const FRAMES: [&str; 9] = [
    "2110315c89bbc353", "30bc28b9de8ef28d", "878e4a1e10974735", "a1d39d8de8a026bc", "a65dd65071a7a664", "d20c188c5378f1f0",
    "d5991324da5372a2", "dde07492c37a030f", "fef14d34934841d0",
];
const GEAR: &str = "04f1ae1a30c0cc38";
/// Steel from shadow to highlight: position, then colour.
const STEEL_AT: [f32; 7] = [0.00, 0.28, 0.48, 0.62, 0.70, 0.80, 1.00];
const STEEL: [[f32; 7]; 3] = [
    [22.0, 74.0, 150.0, 236.0, 255.0, 170.0, 96.0],
    [25.0, 82.0, 160.0, 241.0, 255.0, 180.0, 106.0],
    [30.0, 94.0, 172.0, 246.0, 255.0, 192.0, 120.0],
];

/// Whether this texture is redrawn here rather than upscaled.
pub fn is_redrawn(hash: &str) -> bool {
    hash == GEAR || FRAMES.contains(&hash)
}

/// Run pictures through the line-art model.
fn model(exe: &Path, images: &[RgbImage], tmp: &Path) -> Result<Vec<RgbImage>, String> {
    let (src, dst) = (tmp.join("redraw-in"), tmp.join("redraw-out"));
    for d in [&src, &dst] {
        let _ = std::fs::remove_dir_all(d);
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    for (i, im) in images.iter().enumerate() {
        im.save(src.join(format!("{i}.png"))).map_err(|e| e.to_string())?;
    }
    esrgan::run(exe, esrgan::ANIME, &src, &dst)?;
    (0..images.len()).map(|i| image::open(dst.join(format!("{i}.png"))).map(|im| im.to_rgb8()).map_err(|e| e.to_string())).collect()
}

/// A clean edge along a mask: blurring melts what stairs are left and a
/// tight threshold brings the edge back, anti-aliased.
fn outline(a: &Plane) -> Plane {
    let sigma = SCALE as f32 * 0.5;
    a.blur(sigma).map(|v| ((v - 0.5) * sigma * 2.0 + 0.5).clamp(0.0, 1.0))
}

/// Shade a mask as bevelled, polished steel, lit from the upper left:
/// colour, and a thin dark rim around the metal.
fn steel(mask: &Plane) -> (Vec<[f32; 3]>, Plane) {
    let s = SCALE as f32;
    let (b1, b2, b3) = (mask.blur(s * 0.5), mask.blur(s * 1.4), mask.blur(s * 3.5));
    let height = Plane::from_fn(mask.w, mask.h, |x, y| 0.5 * b1.at(x, y) + 0.3 * b2.at(x, y) + 0.2 * b3.at(x, y));
    let (dy, dx) = height.gradient();
    let k = s * 3.0;
    let (w, h) = (mask.w as f32, mask.h as f32);
    let mut rgb = Vec::with_capacity(mask.v.len());
    for y in 0..mask.h {
        for x in 0..mask.w {
            // A broad sheen across the piece keeps flat areas from looking dead.
            let sheen = 0.09 * ((x as f32 / w * 5.0 + y as f32 / h * 2.2) * std::f32::consts::PI).sin();
            let t = (0.60 + (dx.at(x, y) * k + dy.at(x, y) * k) * 0.75 + sheen).clamp(0.0, 1.0);
            rgb.push([0, 1, 2].map(|c| interp(t, &STEEL_AT, &STEEL[c])));
        }
    }
    let rim = mask.blur(s * 0.28).zip(mask, |b, m| (b * (1.0 - m) * 2.5).clamp(0.0, 1.0));
    (rgb, rim)
}

/// Mask and colour of a texture, cleaned by the model, at 8x.
fn source(path: &Path, exe: &Path, tmp: &Path) -> Result<(Plane, RgbImage), String> {
    let im = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.to_rgba8();
    let (w, h) = (im.width() * SCALE, im.height() * SCALE);
    let mask = RgbImage::from_fn(im.width(), im.height(), |x, y| Rgb([if im.get_pixel(x, y)[3] > 64 { 255 } else { 0 }; 3]));
    let black = RgbImage::from_fn(im.width(), im.height(), |x, y| {
        let p = im.get_pixel(x, y);
        if p[3] > 64 { Rgb([p[0], p[1], p[2]]) } else { Rgb([0; 3]) }
    });
    let out = model(exe, &[mask, black], tmp)?;
    let mask = Plane::from_gray(&resize_gray(&to_gray(&out[0]), w, h, FilterType::CatmullRom));
    Ok((mask, resize_rgb(&out[1], w, h, FilterType::Lanczos3)))
}

fn rgba(rgb: &[[f32; 3]], alpha: &Plane) -> RgbaImage {
    RgbaImage::from_fn(alpha.w as u32, alpha.h as u32, |x, y| {
        let i = y as usize * alpha.w + x as usize;
        let c = rgb[i].map(|v| v.round().clamp(0.0, 255.0) as u8);
        image::Rgba([c[0], c[1], c[2], (alpha.v[i] * 255.0).round().clamp(0.0, 255.0) as u8])
    })
}

fn frame(path: &Path, exe: &Path, tmp: &Path) -> Result<RgbaImage, String> {
    let (mask, colour) = source(path, exe, tmp)?;
    let lum: GrayImage = to_gray(&colour);
    let lum = Plane::from_gray(&lum);
    let metal = outline(&mask.zip(&lum, |m, l| if l > 0.24 { m.clamp(0.0, 1.0) } else { 0.0 }));
    let solid = outline(&mask);
    let (steel_rgb, rim) = steel(&metal);
    let inlay = [9.0, 10.0, 13.0];
    let rgb: Vec<[f32; 3]> = steel_rgb.iter().zip(&metal.v).map(|(c, &m)| [0, 1, 2].map(|i| c[i] * m + inlay[i] * (1.0 - m))).collect();
    let a = Plane::from_fn(mask.w, mask.h, |x, y| metal.at(x, y).max(solid.at(x, y) * 0.94).max(rim.at(x, y)));
    Ok(rgba(&rgb, &a))
}

/// Line art sharpened toward ink, keeping the shading in the face.
fn inked(p: [u8; 3]) -> [f32; 3] {
    let a = p.map(|v| v as f32 / 255.0);
    let ink = (((a[0] + a[1] + a[2]) / 3.0 - 0.40) * 3.2 + 0.5).clamp(0.0, 1.0);
    let ink = ink * ink * (3.0 - 2.0 * ink);
    a.map(|v| (v * 0.45 + ink * 0.55) * 255.0)
}

fn gear(path: &Path, exe: &Path, tmp: &Path) -> Result<RgbaImage, String> {
    let (mask, colour) = source(path, exe, tmp)?;
    let solid = outline(&mask);
    let (mut sx, mut sy, mut total) = (0.0f64, 0.0f64, 0.0f64);
    for y in 0..solid.h {
        for x in 0..solid.w {
            let v = solid.at(x, y) as f64;
            sx += x as f64 * v;
            sy += y as f64 * v;
            total += v;
        }
    }
    let (cx, cy) = ((sx / total.max(1e-9)) as f32, (sy / total.max(1e-9)) as f32);
    let (steel_rgb, _) = steel(&solid);
    let h = solid.h as f32;
    let rgb: Vec<[f32; 3]> = (0..solid.v.len())
        .map(|i| {
            let (x, y) = ((i % solid.w) as f32, (i / solid.w) as f32);
            let r = (x - cx).hypot(y - cy);
            // The mascot sits inside the ring: fade from steel to its art there.
            let keep = ((0.30 * h - r) / (SCALE as f32 * 1.5)).clamp(0.0, 1.0);
            let ink = inked(colour.get_pixel(x as u32, y as u32).0);
            [0, 1, 2].map(|c| steel_rgb[i][c] * (1.0 - keep) + ink[c] * keep)
        })
        .collect();
    Ok(rgba(&rgb, &solid))
}

/// The frames and the gear the dump holds and neither `out` nor `skip` has.
pub fn todo(dump: &Path, out: &Path, skip: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut have = crate::pack::hashes(&[out.to_path_buf()]);
    have.extend(crate::pack::hashes(skip));
    let mut files: Vec<PathBuf> = std::fs::read_dir(dump).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
    files.sort();
    files.retain(|f| crate::pack::hash_of(f).is_some_and(|h| is_redrawn(h) && have.insert(h.to_string())));
    Ok(files)
}

/// Redraw `todo` into `out`, calling `progress` after each one.
pub fn build(exe: &Path, todo: &[PathBuf], out: &Path, tmp: &Path, progress: &mut dyn FnMut(usize)) -> Result<usize, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (n, f) in todo.iter().enumerate() {
        let hash = crate::pack::hash_of(f).unwrap_or_default();
        let art = if hash == GEAR { gear(f, exe, tmp)? } else { frame(f, exe, tmp)? };
        crate::pack::save(&image::DynamicImage::ImageRgba8(art), &out.join(f.file_name().unwrap()))?;
        progress(n + 1);
    }
    Ok(todo.len())
}
