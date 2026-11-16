//! High-resolution versions of the title, menu and loading pictures
//! (`game/images/*.png`, which the installers make from the disc), and the
//! 16:9 versions the renderer prefers in widescreen (was the README's
//! upscale loop and tools/widen-pictures.py).

use std::path::Path;

use image::{imageops::FilterType, Rgb, RgbImage};

use crate::esrgan;
use crate::plane::resize_rgb;

/// The 4:3 size: 4K's height. The originals are 512x448 shown at 4:3.
const HD: (u32, u32) = (2880, 2160);
const WIDE: (u32, u32) = (3840, 2160);
/// Slope of the stretch curve at the centre. Must match WIDE_CENTRE in
/// crates/gs-vulkan/src/pictures.rs, which moves the game's overlays on a
/// picture (loading bar, "PRESS START") by the same curve.
const WIDE_CENTRE: f32 = 1.2;

fn pngs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.to_ascii_lowercase().ends_with(".png"))
        .collect();
    names.sort();
    names
}

/// Upscale the pictures in `images` that `images/hd` lacks, then make the
/// 16:9 versions `images/hd/wide` lacks. Returns how many of each.
pub fn build(exe: &Path, images: &Path, tmp: &Path) -> Result<(usize, usize), String> {
    let hd = images.join("hd");
    let wide = hd.join("wide");
    std::fs::create_dir_all(&wide).map_err(|e| format!("{}: {e}", wide.display()))?;
    let have = pngs(&hd);
    let todo: Vec<String> = pngs(images).into_iter().filter(|n| !have.contains(n)).collect();
    if !todo.is_empty() {
        let (src, dst) = (tmp.join("pictures-in"), tmp.join("pictures-out"));
        for d in [&src, &dst] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        for n in &todo {
            std::fs::copy(images.join(n), src.join(n)).map_err(|e| e.to_string())?;
        }
        esrgan::run(exe, esrgan::ANIME, &src, &dst)?;
        for n in &todo {
            let up = image::open(dst.join(n)).map_err(|e| format!("{n}: {e}"))?.to_rgb8();
            resize_rgb(&up, HD.0, HD.1, FilterType::Lanczos3).save(hd.join(n)).map_err(|e| e.to_string())?;
        }
    }
    let have_wide = pngs(&wide);
    let mut widened = 0;
    for n in pngs(&hd).into_iter().filter(|n| !have_wide.contains(n)) {
        let pic = image::open(hd.join(&n)).map_err(|e| format!("{n}: {e}"))?.to_rgb8();
        widen(&pic).save(wide.join(&n)).map_err(|e| e.to_string())?;
        widened += 1;
    }
    Ok((todo.len(), widened))
}

/// Stretch a 4:3 picture to 16:9, the edges more than the centre: a point
/// at v (-1..1 across the 4:3 picture) lands at u across the wide one,
/// where v = A*u + (1 - A)*u^3. Every column is magnified, so a cubic
/// filter samples it without aliasing.
fn widen(pic: &RgbImage) -> RgbImage {
    let pic = if pic.dimensions() == HD { pic.clone() } else { resize_rgb(pic, HD.0, HD.1, FilterType::Lanczos3) };
    let (sw, ow) = (HD.0 as f32, WIDE.0);
    // Source position and the four Catmull-Rom weights of each output column.
    let columns: Vec<(i64, [f32; 4])> = (0..ow)
        .map(|x| {
            let u = (x as f32 + 0.5) / ow as f32 * 2.0 - 1.0;
            let v = WIDE_CENTRE * u + (1.0 - WIDE_CENTRE) * u * u * u;
            let s = (v + 1.0) / 2.0 * sw - 0.5;
            let i = s.floor();
            let t = s - i;
            let w = [
                ((-t + 2.0) * t - 1.0) * t / 2.0,
                ((3.0 * t - 5.0) * t * t + 2.0) / 2.0,
                ((-3.0 * t + 4.0) * t + 1.0) * t / 2.0,
                (t - 1.0) * t * t / 2.0,
            ];
            (i as i64, w)
        })
        .collect();
    RgbImage::from_fn(WIDE.0, WIDE.1, |x, y| {
        let (i, w) = columns[x as usize];
        let mut c = [0.0f32; 3];
        for (k, wk) in w.iter().enumerate() {
            let sx = (i + k as i64 - 1).clamp(0, HD.0 as i64 - 1) as u32;
            let p = pic.get_pixel(sx, y).0;
            for ch in 0..3 {
                c[ch] += p[ch] as f32 * wk;
            }
        }
        Rgb(c.map(|v| v.round().clamp(0.0, 255.0) as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widen_keeps_a_flat_picture_flat_and_the_centre_in_place() {
        let mut pic = RgbImage::from_pixel(HD.0, HD.1, Rgb([90, 90, 90]));
        // A one-column line in the middle stays in the middle.
        for y in 0..HD.1 {
            pic.put_pixel(HD.0 / 2, y, Rgb([255, 255, 255]));
        }
        let out = widen(&pic);
        assert_eq!(out.dimensions(), WIDE);
        assert_eq!(out.get_pixel(10, 10).0, [90, 90, 90]);
        let brightest = (0..WIDE.0).max_by_key(|&x| out.get_pixel(x, 100)[0]).unwrap();
        assert!((brightest as i64 - WIDE.0 as i64 / 2).abs() <= 1, "{brightest}");
    }
}
