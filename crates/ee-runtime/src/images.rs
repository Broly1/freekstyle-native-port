//! Pictures as PNG files instead of the IPU.
//!
//! The game's title, menu and loading backgrounds are MPEG-2 stills
//! (.LSC) that the PS2 decodes with its IPU. `lsc2png` converts them once
//! into `game/images/<NAME>.png` plus `index.txt` (content hash -> file),
//! and the game's picture decoder is replaced by `hle::lsc_decode`, which
//! loads the PNG. The PNGs can be edited freely; a missing or unreadable
//! one falls back to decoding the original data.

use std::collections::HashMap;
use std::path::Path;

/// Identifies a picture by its file contents.
pub fn content_hash(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// `index.txt`: one "<hash hex> <file name>" per line.
pub fn load_index(dir: &Path) -> HashMap<u64, String> {
    std::fs::read_to_string(dir.join("index.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (h, name) = l.split_once(' ')?;
            Some((u64::from_str_radix(h, 16).ok()?, name.trim().to_string()))
        })
        .collect()
}

/// PNG to pixels with PS2 alpha (0x80 = opaque).
pub fn load_png(path: &Path) -> Result<(u32, u32, Vec<u32>), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width, info.height);
    let px: Vec<u32> = match info.color_type {
        png::ColorType::Rgba => buf[..(w * h * 4) as usize].chunks_exact(4).map(|p| rgba(p[0], p[1], p[2], p[3])).collect(),
        png::ColorType::GrayscaleAlpha => buf[..(w * h * 2) as usize].chunks_exact(2).map(|p| rgba(p[0], p[0], p[0], p[1])).collect(),
        c => return Err(format!("unsupported PNG colour type {c:?}")),
    };
    Ok((w, h, px))
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let a = (a as u32 * 128 + 127) / 255;
    r as u32 | (g as u32) << 8 | (b as u32) << 16 | a << 24
}

/// Pixels with PS2 alpha to a PNG file.
pub fn save_png(path: &Path, w: u32, h: u32, px: &[u32]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    let data: Vec<u8> = px
        .iter()
        .flat_map(|&p| {
            let a = ((p >> 24).min(128) * 255 / 128) as u8;
            [p as u8, (p >> 8) as u8, (p >> 16) as u8, a]
        })
        .collect();
    wr.write_image_data(&data).map_err(|e| e.to_string())
}

/// Scale to `w` x `h` (nearest), for replacement pictures of another size.
pub fn resize(sw: u32, sh: u32, px: &[u32], w: u32, h: u32) -> Vec<u32> {
    (0..h).flat_map(|y| (0..w).map(move |x| px[((y * sh / h) * sw + x * sw / w) as usize])).collect()
}
