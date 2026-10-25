//! High-resolution title and loading pictures.
//!
//! The game uploads each picture (`game/images/<NAME>.png`, see
//! ee-runtime's `images`) as a CT32 texture with the picture in its
//! top-left corner and draws it from there. Such textures are recognised
//! by hashing that corner, and `game/images/hd/<NAME>.png` is drawn
//! instead. The replacement may have any size (it covers the same 4:3
//! screen area); it is resampled to the internal resolution so that its
//! texels land 1:1 on target pixels, whatever filter the game samples
//! with. Texture coordinates are normalised, so the larger texture drops
//! in unchanged.
//!
//! In widescreen the frame is stretched to 16:9 for display, so a 16:9
//! version, `game/images/hd/wide/<NAME>.png`, is preferred: squeezed into
//! the frame like everything else, it shows at its own shape.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Texels hashed first to reject other textures cheaply.
const PREFIX: usize = 64;

struct Original {
    name: String,
    prefix: u64,
    /// Which replacements exist: 4:3 (`hd`) and 16:9 (`hd/wide`).
    hd: bool,
    wide: bool,
}

pub struct Pictures {
    hd: PathBuf,
    /// Original picture size -> full hash -> picture.
    known: HashMap<(u32, u32), HashMap<u64, Original>>,
}

/// FNV-1a over colours; alpha is ignored.
fn hash(px: impl Iterator<Item = u32>) -> u64 {
    px.fold(0xcbf2_9ce4_8422_2325u64, |h, p| (h ^ (p & 0xff_ffff) as u64).wrapping_mul(0x100_0000_01b3))
}

/// The top-left w x h texels of a texture `stride` texels wide.
fn corner(px: &[u32], stride: u32, w: u32, h: u32) -> impl Iterator<Item = u32> + '_ {
    (0..h).flat_map(move |y| px[(y * stride) as usize..(y * stride + w) as usize].iter().copied())
}

/// RGBA8 pixels of a PNG file.
fn load_png(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..(w * h * 4) as usize].to_vec(),
        png::ColorType::GrayscaleAlpha => buf[..(w * h * 2) as usize].chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        c => return Err(format!("unsupported PNG colour type {c:?}")),
    };
    Ok((w, h, rgba))
}

impl Pictures {
    /// Index the originals in `dir` that have a replacement in `dir/hd`.
    pub fn load(dir: &Path) -> Pictures {
        let hd = dir.join("hd");
        let pngs = |dir: &Path| -> Vec<String> {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.to_ascii_lowercase().ends_with(".png"))
                .collect()
        };
        let (plain, wide) = (pngs(&hd), pngs(&hd.join("wide")));
        let mut names: Vec<String> = plain.iter().chain(&wide).cloned().collect();
        names.sort();
        names.dedup();
        let mut known: HashMap<(u32, u32), HashMap<u64, Original>> = HashMap::new();
        for name in names {
            match load_png(&dir.join(&name)) {
                Ok((w, h, rgba)) => {
                    let px: Vec<u32> = rgba.chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])).collect();
                    let original = Original { prefix: hash(px[..PREFIX.min(px.len())].iter().copied()), hd: plain.contains(&name), wide: wide.contains(&name), name };
                    known.entry((w, h)).or_default().insert(hash(px.into_iter()), original);
                }
                Err(e) => eprintln!("[pictures] {}: {e}; hd/{name} is not used", dir.join(&name).display()),
            }
        }
        let count = |f: fn(&Original) -> bool| known.values().flat_map(|m| m.values()).filter(|o| f(o)).count();
        let (n, w) = (count(|o| o.hd), count(|o| o.wide));
        if n + w > 0 {
            eprintln!("[pictures] high-resolution pictures in {}: {n} at 4:3, {w} at 16:9", hd.display());
        }
        Pictures { hd, known }
    }

    /// Replacement for a decoded `w` x `h` texture holding a known
    /// picture, at `(sx, sy)` texels per GS pixel; the 16:9 version if
    /// `wide` and there is one.
    pub fn replace(&self, w: u32, h: u32, px: &[u32], (sx, sy): (u32, u32), wide: bool) -> Option<(u32, u32, Vec<u32>)> {
        for (&(pw, ph), by_hash) in &self.known {
            if pw > w || ph > h || (pw as usize) < PREFIX {
                continue;
            }
            let prefix = hash(px[..PREFIX].iter().copied());
            if !by_hash.values().any(|o| o.prefix == prefix) {
                continue;
            }
            let Some(o) = by_hash.get(&hash(corner(px, w, pw, ph))) else { continue };
            let path = if wide && o.wide || !o.hd { self.hd.join("wide").join(&o.name) } else { self.hd.join(&o.name) };
            let t = std::time::Instant::now();
            match load_png(&path) {
                Ok((sw, sh, rgba)) => {
                    let out = place(&rgba, sw, sh, pw * sx, ph * sy, w * sx, h * sy);
                    if std::env::var_os("FREEK_GS_LOG").is_some() {
                        eprintln!("[pictures] {} {sw}x{sh} -> {}x{} in {} ms", path.display(), pw * sx, ph * sy, t.elapsed().as_millis());
                    }
                    return Some((w * sx, h * sy, out));
                }
                Err(e) => eprintln!("[pictures] {}: {e}", path.display()),
            }
        }
        None
    }
}

/// Resample an RGBA8 picture to `pw` x `ph` in the top-left corner of a
/// `w` x `h` texture with GS alpha. The rest repeats the picture's edges,
/// so filtering at its border never pulls in black.
fn place(rgba: &[u8], sw: u32, sh: u32, pw: u32, ph: u32, w: u32, h: u32) -> Vec<u32> {
    let pic = resample(rgba, sw, sh, pw, ph);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let row = &pic[(y.min(ph - 1) * pw * 3) as usize..][..(pw * 3) as usize];
        for x in 0..w {
            let p = &row[(x.min(pw - 1) * 3) as usize..][..3];
            out.push(p[0] as u32 | (p[1] as u32) << 8 | (p[2] as u32) << 16 | 0x80 << 24);
        }
    }
    out
}

fn lanczos3(x: f32) -> f32 {
    if x == 0.0 {
        return 1.0;
    }
    if x.abs() >= 3.0 {
        return 0.0;
    }
    let px = std::f32::consts::PI * x;
    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
}

/// Lanczos-3 filter taps for resizing `n` samples to `m`: (first source
/// index, weights) per output sample.
fn taps(n: u32, m: u32) -> Vec<(usize, Vec<f32>)> {
    let ratio = n as f32 / m as f32;
    // Widen the filter when shrinking, so it averages instead of aliasing.
    let scale = ratio.max(1.0);
    let support = 3.0 * scale;
    (0..m)
        .map(|i| {
            let centre = (i as f32 + 0.5) * ratio - 0.5;
            let first = (centre - support).ceil() as i64;
            let last = (centre + support).floor() as i64;
            let mut wts: Vec<f32> = (first..=last).map(|j| lanczos3((j as f32 - centre) / scale)).collect();
            // Fold taps outside the picture onto its edge.
            let clamp = |j: i64| j.clamp(0, n as i64 - 1) as usize;
            let lo = clamp(first);
            let mut folded = vec![0.0; clamp(last) - lo + 1];
            for (k, w) in wts.drain(..).enumerate() {
                folded[clamp(first + k as i64) - lo] += w;
            }
            let sum: f32 = folded.iter().sum();
            folded.iter_mut().for_each(|w| *w /= sum);
            (lo, folded)
        })
        .collect()
}

/// Separable Lanczos-3 resize of RGBA8 to RGB8, rows split over threads.
fn resample(rgba: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    // Horizontal: sh rows of dw RGB floats.
    let hx = taps(sw, dw);
    let mut mid = vec![0f32; (dw * sh * 3) as usize];
    let rows_per = (sh as usize).div_ceil(threads);
    std::thread::scope(|s| {
        for (c, chunk) in mid.chunks_mut(rows_per * dw as usize * 3).enumerate() {
            let hx = &hx;
            s.spawn(move || {
                for (r, row) in chunk.chunks_mut(dw as usize * 3).enumerate() {
                    let src = &rgba[(c * rows_per + r) * sw as usize * 4..][..sw as usize * 4];
                    for (x, (first, wts)) in hx.iter().enumerate() {
                        let mut acc = [0f32; 3];
                        for (k, w) in wts.iter().enumerate() {
                            let p = &src[(first + k) * 4..][..3];
                            acc.iter_mut().zip(p).for_each(|(a, &v)| *a += w * v as f32);
                        }
                        row[x * 3..x * 3 + 3].copy_from_slice(&acc);
                    }
                }
            });
        }
    });
    // Vertical: dh rows of dw RGB bytes.
    let vy = taps(sh, dh);
    let mut out = vec![0u8; (dw * dh * 3) as usize];
    let rows_per = (dh as usize).div_ceil(threads);
    let stride = dw as usize * 3;
    std::thread::scope(|s| {
        for (c, chunk) in out.chunks_mut(rows_per * stride).enumerate() {
            let (vy, mid) = (&vy, &mid);
            s.spawn(move || {
                for (r, row) in chunk.chunks_mut(stride).enumerate() {
                    let (first, wts) = &vy[c * rows_per + r];
                    for (i, o) in row.iter_mut().enumerate() {
                        let v: f32 = wts.iter().enumerate().map(|(k, w)| w * mid[(first + k) * stride + i]).sum();
                        *o = v.round().clamp(0.0, 255.0) as u8;
                    }
                }
            });
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_are_normalised_and_in_range() {
        for (n, m) in [(2880, 1536), (2160, 2240), (7, 7), (3, 40)] {
            for (first, w) in taps(n, m) {
                assert!(first + w.len() <= n as usize);
                assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn flat_picture_stays_flat() {
        let rgba: Vec<u8> = (0..40 * 30).flat_map(|_| [200, 100, 50, 255]).collect();
        let out = resample(&rgba, 40, 30, 23, 61);
        assert!(out.chunks_exact(3).all(|p| p == [200, 100, 50]));
    }

    #[test]
    fn placed_picture_repeats_its_edges() {
        let rgba = [10, 20, 30, 255, 40, 50, 60, 255];
        let out = place(&rgba, 2, 1, 2, 1, 4, 2);
        assert_eq!(out.len(), 8);
        assert_eq!(out[3], out[1]);
        assert_eq!(out[7], out[1]);
        assert_eq!(out[0] >> 24, 0x80);
    }

    #[test]
    fn recognises_the_corner_of_a_texture() {
        let pic: Vec<u32> = (0..80 * 4).map(|i| i * 7919).collect();
        let mut tex = vec![0u32; 128 * 8];
        for y in 0..4 {
            tex[y * 128..y * 128 + 80].copy_from_slice(&pic[y * 80..y * 80 + 80]);
        }
        assert_eq!(hash(corner(&tex, 128, 80, 4)), hash(pic.into_iter()));
    }
}
