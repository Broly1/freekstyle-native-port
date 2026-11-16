//! The texture pack: textures the game recorded while playing, upscaled 4x
//! by Real-ESRGAN (was tools/hd_pack.py).
//!
//! Colour and alpha both go through the model: cut-out edges are
//! low-resolution masks of two or three levels, and only a model turns
//! their stairs into clean outlines. 2D sprites (fonts, HUD, menu frames)
//! use the line-art model and 3D surfaces the photo model. Full-screen
//! pictures are left out: they have their own replacements in
//! game/images/hd. The chrome menu frames and the boost gear are redrawn
//! instead (see `redraw`).
//!
//! Textures already in a pack are left alone, so each run only adds what is
//! missing and hand-made replacements survive.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use image::{GrayImage, Rgb, RgbImage, RgbaImage};

use crate::esrgan;

/// Effect ramps and lookup tables: their exact texels matter.
const MIN_SIDE: u32 = 16;
/// Wrapped border around a texture while it is scaled, so tiling surfaces
/// keep matching at their edges.
const PAD: u32 = 16;
const SCALE: u32 = 4;
/// Textures per model run: between runs the build checks whether the game
/// has started again, and stops to leave it the GPU.
const BATCH: usize = 24;

/// The 16 hex digits a pack file's name starts with.
pub fn hash_of(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    (name.ends_with(".png") && name.len() > 16 && name.as_bytes()[..16].iter().all(u8::is_ascii_hexdigit)).then(|| &name[..16])
}

/// Hashes of the textures in these packs.
pub fn hashes(dirs: &[PathBuf]) -> HashSet<String> {
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| hash_of(&e.path()).map(str::to_string))
        .collect()
}

fn wants(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let picture = name.contains("_sprite") && name.contains("_Ct32_");
    match image::image_dimensions(path) {
        Ok((w, h)) => w.min(h) >= MIN_SIDE && !picture,
        Err(_) => false,
    }
}

/// Spread colour into see-through texels so that scaling does not drag
/// whatever colour they hold into the edges of cut-outs.
fn fill_transparent(rgb: &mut [[f32; 3]], alpha: &[u8], w: usize, h: usize) {
    // In a hard mask the half-covered edge texels are dark with the colour
    // of whatever the art was cut from; in soft smoke and glows they are
    // the art.
    let levels: HashSet<u8> = alpha.iter().copied().collect();
    let hard = levels.len() <= 4 && levels.contains(&255);
    let mut known: Vec<bool> = alpha.iter().map(|&a| if hard { a == 255 } else { a > 0 }).collect();
    if known.iter().all(|&k| k) || !known.iter().any(|&k| k) {
        return;
    }
    for _ in 0..w.max(h) {
        if known.iter().all(|&k| k) {
            break;
        }
        let mut grow = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if known[i] {
                    continue;
                }
                let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
                // Neighbours wrap around, as numpy's roll does.
                for j in [((y + h - 1) % h) * w + x, ((y + 1) % h) * w + x, y * w + (x + w - 1) % w, y * w + (x + 1) % w] {
                    if known[j] {
                        for c in 0..3 {
                            sum[c] += rgb[j][c];
                        }
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    grow.push((i, sum.map(|s| s / n)));
                }
            }
        }
        for (i, c) in grow {
            rgb[i] = c;
            known[i] = true;
        }
    }
}

/// The picture with a PAD-texel border copied from the opposite edges.
fn wrapped(px: &[[f32; 3]], w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w + 2 * PAD, h + 2 * PAD, |x, y| {
        let sx = (x + w * PAD - PAD) % w;
        let sy = (y + h * PAD - PAD) % h;
        Rgb(px[(sy * w + sx) as usize].map(|c| c.round().clamp(0.0, 255.0) as u8))
    })
}

fn alpha_name(name: &str) -> String {
    format!("{}.alpha.png", name.trim_end_matches(".png"))
}

/// Save the colour of a texture, wrapped and with its alpha filled, and
/// its alpha as a grey picture when it has any.
fn prepare(path: &Path, src: &Path) -> Result<(), String> {
    let im = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.to_rgba8();
    let (w, h) = im.dimensions();
    let alpha: Vec<u8> = im.pixels().map(|p| p[3]).collect();
    let mut rgb: Vec<[f32; 3]> = im.pixels().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    fill_transparent(&mut rgb, &alpha, w as usize, h as usize);
    let name = path.file_name().unwrap().to_str().unwrap();
    wrapped(&rgb, w, h).save(src.join(name)).map_err(|e| e.to_string())?;
    if alpha.iter().any(|&a| a < 255) {
        let grey: Vec<[f32; 3]> = alpha.iter().map(|&a| [a as f32; 3]).collect();
        wrapped(&grey, w, h).save(src.join(alpha_name(name))).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn assemble(original: &Path, dst: &Path, out: &Path) -> Result<(), String> {
    let (w, h) = image::image_dimensions(original).map_err(|e| e.to_string())?;
    let name = original.file_name().unwrap().to_str().unwrap();
    let (pad, sw, sh) = (PAD * SCALE, w * SCALE, h * SCALE);
    let colour = image::open(dst.join(name)).map_err(|e| e.to_string())?.to_rgb8();
    let alpha: Option<GrayImage> = image::open(dst.join(alpha_name(name))).ok().map(|a| crate::plane::to_gray(&a.to_rgb8()));
    let im = RgbaImage::from_fn(sw, sh, |x, y| {
        let c = colour.get_pixel(x + pad, y + pad).0;
        let a = alpha.as_ref().map_or(255, |a| a.get_pixel(x + pad, y + pad)[0]);
        image::Rgba([c[0], c[1], c[2], a])
    });
    im.save(out).map_err(|e| e.to_string())
}

/// Build what the pack in `out` lacks from the textures in `dump`. Hashes
/// in `skip` (other packs) are left out too. Stops early, with the rest
/// left for next time, when `stop` says so. Returns how many were added.
pub fn build(exe: &Path, dump: &Path, out: &Path, skip: &[PathBuf], tmp: &Path, stop: &dyn Fn() -> bool) -> Result<usize, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut have = hashes(&[out.to_path_buf()]);
    have.extend(hashes(skip));
    let mut todo: Vec<PathBuf> = std::fs::read_dir(dump)
        .map_err(|e| format!("{}: {e}", dump.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| hash_of(p).is_some_and(|h| !have.contains(h) && !crate::redraw::is_redrawn(h)))
        .filter(|p| wants(p))
        .collect();
    todo.sort();
    let mut added = 0;
    for batch in todo.chunks(BATCH) {
        if stop() {
            break;
        }
        let (src, dst) = (tmp.join("in"), tmp.join("out"));
        for d in [&src, &dst] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        for f in batch {
            prepare(f, &src)?;
        }
        // One run per model, each on its own folder.
        for (model, kind) in [(esrgan::ANIME, "_sprite"), (esrgan::PHOTO, "_surface")] {
            let part = src.join(kind);
            std::fs::create_dir_all(&part).map_err(|e| e.to_string())?;
            let mut any = false;
            for e in std::fs::read_dir(&src).map_err(|e| e.to_string())?.flatten() {
                let p = e.path();
                if p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(kind)) {
                    std::fs::rename(&p, part.join(p.file_name().unwrap())).map_err(|e| e.to_string())?;
                    any = true;
                }
            }
            if any {
                esrgan::run(exe, model, &part, &dst)?;
            }
        }
        for f in batch {
            let name = f.file_name().unwrap();
            if dst.join(name).is_file() {
                assemble(f, &dst, &out.join(name))?;
                added += 1;
            } else {
                eprintln!("no upscale came out for {}", name.to_string_lossy());
            }
        }
    }
    Ok(added)
}
