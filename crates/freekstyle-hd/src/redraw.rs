//! Menu art redrawn rather than upscaled (was tools/redraw_art.py): the
//! chrome frames. Their originals are too small for any upscaler to give
//! clean edges, so only their shapes are kept: each outline is cleaned by
//! the line-art model, smoothed at 8x and shaded from scratch as polished
//! steel. The boost HUD is drawn in `boost`; its gear and ball wait until
//! the big logo is recorded.

use std::path::{Path, PathBuf};

use image::{imageops::FilterType, GrayImage, Rgb, RgbImage, RgbaImage};

use crate::esrgan;
use crate::ledger::Ledger;
use crate::plane::{interp, resize_gray, resize_rgb, to_gray, Plane};

const SCALE: u32 = 8;
/// Pieces of the chrome menu frame, in the palettes the game shows them.
const FRAMES: [&str; 9] = [
    "2110315c89bbc353", "30bc28b9de8ef28d", "878e4a1e10974735", "a1d39d8de8a026bc", "a65dd65071a7a664", "d20c188c5378f1f0",
    "d5991324da5372a2", "dde07492c37a030f", "fef14d34934841d0",
];
/// Steel from shadow to highlight: position, then colour.
const STEEL_AT: [f32; 7] = [0.00, 0.28, 0.48, 0.62, 0.70, 0.80, 1.00];
const STEEL: [[f32; 7]; 3] = [
    [22.0, 74.0, 150.0, 236.0, 255.0, 170.0, 96.0],
    [25.0, 82.0, 160.0, 241.0, 255.0, 180.0, 106.0],
    [30.0, 94.0, 172.0, 246.0, 255.0, 192.0, 120.0],
];

/// Whether this texture is redrawn here rather than upscaled.
pub fn is_redrawn(hash: &str) -> bool {
    FRAMES.contains(&hash) || crate::boost::is_boost(hash)
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
pub(crate) fn steel(mask: &Plane) -> (Vec<[f32; 3]>, Plane) {
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

/// What in the dump is to be redrawn now: what neither `out` nor `skip`
/// has, and what `out` has from an older drawing of this tool. The gear and
/// the ball wait for the logo; until then the game shows its own.
pub fn todo(dump: &Path, out: &Path, skip: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dump).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
    files.sort();
    let has_logo = crate::boost::find_logo(dump).is_some();
    let ledger = Ledger::read(out);
    let packed: Vec<PathBuf> = std::fs::read_dir(out).into_iter().flatten().flatten().map(|e| e.path()).collect();
    let mut seen = crate::pack::hashes(skip);
    files.retain(|f| {
        let Some(hash) = crate::pack::hash_of(f) else { return false };
        if !is_redrawn(hash) || !seen.insert(hash.to_string()) || (crate::boost::needs_logo(hash) && !has_logo) {
            return false;
        }
        let there: Vec<&PathBuf> = packed.iter().filter(|p| crate::pack::hash_of(p) == Some(hash)).collect();
        match (there.as_slice(), crate::boost::version(hash)) {
            ([], _) => true,
            ([file], Some(version)) => ledger.outdated(hash, version, file, f),
            _ => false,
        }
    });
    Ok(files)
}

/// Redraw `todo` into `out`, calling `progress` after each one.
pub fn build(exe: &Path, todo: &[PathBuf], out: &Path, tmp: &Path, progress: &mut dyn FnMut(usize)) -> Result<usize, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut ledger = Ledger::read(out);
    for (n, f) in todo.iter().enumerate() {
        let hash = crate::pack::hash_of(f).unwrap_or_default();
        let dump = f.parent().unwrap_or(Path::new("."));
        let version = crate::boost::version(hash);
        let art = if version.is_some() { crate::boost::draw(hash, f, dump)? } else { frame(f, exe, tmp)? };
        let target = out.join(f.file_name().unwrap());
        if version.is_some() {
            ledger.keep(hash, &target)?;
        }
        crate::pack::save(&image::DynamicImage::ImageRgba8(art), &target)?;
        if let Some(version) = version {
            ledger.note(hash, version, &target)?;
        }
        progress(n + 1);
    }
    Ok(todo.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boost::{BALL, GEAR, LOGO, METER};
    use crate::ledger::REPLACED;

    struct Dirs {
        root: PathBuf,
        dump: PathBuf,
        out: PathBuf,
        hand: PathBuf,
    }

    impl Dirs {
        fn new(test: &str) -> Dirs {
            let root = std::env::temp_dir().join(format!("freekstyle-redraw-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let (dump, out, hand) = (root.join("dump"), root.join("out"), root.join("hand"));
            for d in [&dump, &out, &hand] {
                std::fs::create_dir_all(d).unwrap();
            }
            Dirs { root, dump, out, hand }
        }
        fn name(hash: &str) -> String {
            format!("{hash}_T8_16x16_surface.png")
        }
        fn put(dir: &Path, hash: &str, side: u32, shade: u8) -> PathBuf {
            let path = dir.join(Dirs::name(hash));
            let im = RgbaImage::from_fn(side, side, |x, y| image::Rgba([shade, (x * 7) as u8, (y * 5) as u8, 255]));
            crate::pack::save(&image::DynamicImage::ImageRgba8(im), &path).unwrap();
            path
        }
        fn record(&self, hash: &str) {
            Dirs::put(&self.dump, hash, 16, 1);
        }
        fn todo(&self) -> Vec<String> {
            let found = todo(&self.dump, &self.out, std::slice::from_ref(&self.hand)).unwrap();
            found.iter().map(|f| crate::pack::hash_of(f).unwrap().to_string()).collect()
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn gear_and_ball_wait_for_the_logo() {
        let d = Dirs::new("wait");
        for h in [GEAR, BALL, METER] {
            d.record(h);
        }
        assert_eq!(d.todo(), [METER]);
        d.record(LOGO);
        assert_eq!(d.todo(), [GEAR, BALL, METER, LOGO]);
    }

    #[test]
    fn packs_from_before_the_list_are_redrawn_once() {
        let d = Dirs::new("once");
        for (h, side) in [(LOGO, 64), (METER, 128)] {
            d.record(h);
            Dirs::put(&d.out, h, side, 2);
        }
        assert_eq!(d.todo(), [METER, LOGO]);
        let mut ledger = Ledger::read(&d.out);
        for h in [METER, LOGO] {
            let file = d.out.join(Dirs::name(h));
            ledger.keep(h, &file).unwrap();
            ledger.note(h, 1, &file).unwrap();
            assert!(d.out.join(REPLACED).join(Dirs::name(h)).is_file());
        }
        assert!(d.todo().is_empty());
    }

    #[test]
    fn an_old_gear_stays_until_the_logo_is_recorded() {
        let d = Dirs::new("old-gear");
        d.record(GEAR);
        Dirs::put(&d.out, GEAR, 128, 2);
        assert!(d.todo().is_empty());
        d.record(LOGO);
        assert_eq!(d.todo(), [GEAR, LOGO]);
    }

    #[test]
    fn a_newer_drawing_replaces_only_its_own_file() {
        let d = Dirs::new("newer");
        d.record(METER);
        let file = Dirs::put(&d.out, METER, 128, 2);
        Ledger::read(&d.out).note(METER, 1, &file).unwrap();
        let source = d.dump.join(Dirs::name(METER));
        assert!(Ledger::read(&d.out).outdated(METER, 2, &file, &source));
        assert!(!Ledger::read(&d.out).outdated(METER, 1, &file, &source));
        Dirs::put(&d.out, METER, 128, 3); // changed by hand
        assert!(!Ledger::read(&d.out).outdated(METER, 2, &file, &source));
    }

    #[test]
    fn hand_made_files_are_left_alone() {
        use image::codecs::png::{CompressionType, FilterType, PngEncoder};
        let d = Dirs::new("hand");
        for h in [METER, LOGO, GEAR, BALL] {
            d.record(h);
        }
        // in the hand-made pack
        Dirs::put(&d.hand, METER, 128, 2);
        // saved by another program
        let file = std::fs::File::create(d.out.join(Dirs::name(LOGO))).unwrap();
        let encoder = PngEncoder::new_with_quality(file, CompressionType::Best, FilterType::Adaptive);
        RgbaImage::new(128, 128).write_with_encoder(encoder).unwrap();
        // at a size this tool never makes
        Dirs::put(&d.out, GEAR, 96, 2);
        // under a name it never gives
        let odd = Dirs::put(&d.out, BALL, 128, 2);
        std::fs::rename(&odd, d.out.join(format!("{BALL}.png"))).unwrap();
        assert!(d.todo().is_empty());
    }
}
