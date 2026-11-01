//! Texture replacement packs: HD versions of the game's textures.
//!
//! A PS2 game swaps textures through its 4 MB of video memory all the time,
//! so an address says nothing about what a texture is. Replacements are
//! found by a hash of the decoded pixels instead, which also tells palette
//! variants apart.
//!
//! `FREEK_DUMP_TEXTURES=<dir>` writes every texture the game uses as
//! `<hash>_<format>_<w>x<h>_<sprite|surface>.png`. A pack is a folder
//! (`game/textures/hd`) of PNGs whose names start with that hash; anything
//! after the hash is ignored. A replacement keeps the original's aspect
//! ratio. PNG alpha 255 is the texture's strongest alpha: 0x80, fully opaque,
//! or more for textures such as fonts that brighten what they draw.
//!
//! PNGs are read on a worker thread and the original shows until one is
//! ready, so a texture appearing mid-race never stalls a frame.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

/// Replacements bigger than this per side are refused.
const MAX_SIDE: u32 = 4096;

/// Identity of a decoded texture: stable between runs, wherever it sits in
/// video memory.
pub fn content_hash(w: u32, h: u32, px: &[u32]) -> u64 {
    let start = (w as u64) << 32 | h as u64;
    let hash = px.iter().fold(start ^ 0xcbf2_9ce4_8422_2325, |a, &p| (a.rotate_left(5) ^ p as u64).wrapping_mul(0x517c_c1b7_2722_0a95));
    hash ^ hash >> 29
}

/// One mip level, biggest first.
pub struct Level {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

pub struct Loaded {
    pub hash: u64,
    pub levels: Vec<Level>,
}

pub struct Pack {
    files: HashMap<u64, PathBuf>,
    jobs: Sender<(u64, PathBuf, u32, u32, u32)>,
    done: Receiver<Result<Loaded, u64>>,
    /// Queued, or refused and not to be tried again.
    asked: HashSet<u64>,
    /// Strongest alpha of each original seen.
    peaks: HashMap<u64, u32>,
}

impl Pack {
    /// Index the pack in `dir`. None without a usable pack, so a game with
    /// no pack does no hashing.
    pub fn open(dir: &Path) -> Option<Pack> {
        let files: HashMap<u64, PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let name = path.file_stem()?.to_str()?;
                let hash = u64::from_str_radix(name.get(..16)?, 16).ok()?;
                (path.extension()? == "png").then_some((hash, path))
            })
            .collect();
        if files.is_empty() {
            return None;
        }
        eprintln!("[textures] {} HD textures in {}", files.len(), dir.display());
        let (jobs, rx) = channel::<(u64, PathBuf, u32, u32, u32)>();
        let (tx, done) = channel();
        std::thread::Builder::new()
            .name("hd-textures".into())
            .spawn(move || {
                for (hash, path, w, h, peak) in rx {
                    let loaded = load(&path, w, h, peak).map_err(|e| eprintln!("[textures] {}: {e}", path.display()));
                    if tx.send(loaded.map(|levels| Loaded { hash, levels }).map_err(|()| hash)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn HD texture thread");
        Some(Pack { files, jobs, done, asked: HashSet::new(), peaks: HashMap::new() })
    }

    pub fn has(&self, hash: u64) -> bool {
        self.files.contains_key(&hash)
    }

    /// Remember the strongest alpha of an original, before asking for it.
    pub fn note_peak(&mut self, hash: u64, px: &[u32]) {
        self.peaks.entry(hash).or_insert_with(|| px.iter().map(|p| p >> 24).max().unwrap_or(0));
    }

    /// Ask for a replacement of a `w` x `h` original to be loaded.
    pub fn request(&mut self, hash: u64, w: u32, h: u32) {
        if let Some(path) = self.files.get(&hash).filter(|_| self.asked.insert(hash)) {
            let peak = self.peaks.get(&hash).copied().unwrap_or(0).max(0x80);
            let _ = self.jobs.send((hash, path.clone(), w, h, peak));
        }
    }

    /// A finished replacement, if there is one. Failures stay in `asked`,
    /// so a bad file is reported once.
    pub fn next_loaded(&mut self) -> Option<Loaded> {
        self.done.try_iter().find_map(Result::ok)
    }

    /// An evicted replacement can be loaded again.
    pub fn forget(&mut self, hash: u64) {
        self.asked.remove(&hash);
    }
}

fn load(path: &Path, w: u32, h: u32, peak: u32) -> Result<Vec<Level>, String> {
    let (rw, rh, rgba) = crate::pictures::load_png(path)?;
    if rw as u64 * h as u64 != rh as u64 * w as u64 {
        return Err(format!("{rw}x{rh} does not have the aspect ratio of the {w}x{h} original"));
    }
    if rw.max(rh) > MAX_SIDE {
        return Err(format!("{rw}x{rh} is over the {MAX_SIDE} limit"));
    }
    let px: Vec<u32> = rgba.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], ((c[3] as u32 * peak + 127) / 255) as u8])).collect();
    let mut levels = vec![Level { w: rw, h: rh, px }];
    while let Some(next) = shrink(levels.last().unwrap()) {
        levels.push(next);
    }
    Ok(levels)
}

/// Next mip level: 2x2 boxes weighted by alpha, so the colour of see-through
/// texels does not leak into the edges of cut-outs.
fn shrink(l: &Level) -> Option<Level> {
    if l.w == 1 && l.h == 1 {
        return None;
    }
    let (nw, nh) = ((l.w / 2).max(1), (l.h / 2).max(1));
    let at = |x: u32, y: u32| l.px[(y.min(l.h - 1) * l.w + x.min(l.w - 1)) as usize];
    let mut px = Vec::with_capacity((nw * nh) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let (sx, sy) = if (l.w, l.h) == (nw, nh) { (x, y) } else { (x * l.w / nw, y * l.h / nh) };
            let (mut c, mut alpha) = ([0u32; 3], 0u32);
            for p in [at(sx, sy), at(sx + 1, sy), at(sx, sy + 1), at(sx + 1, sy + 1)] {
                let a = p >> 24;
                alpha += a;
                for (i, v) in c.iter_mut().enumerate() {
                    *v += (p >> (8 * i) & 0xff) * a;
                }
            }
            let [r, g, b] = c.map(|v| if alpha == 0 { 0 } else { (v + alpha / 2) / alpha });
            px.push(r | g << 8 | b << 16 | (alpha + 2) / 4 << 24);
        }
    }
    Some(Level { w: nw, h: nh, px })
}

/// Writes textures as the game first uses them, on its own thread.
pub struct Dumper {
    seen: HashSet<u64>,
    jobs: Sender<(String, u32, u32, Vec<u8>)>,
}

impl Dumper {
    pub fn new(dir: PathBuf) -> Dumper {
        let _ = std::fs::create_dir_all(&dir);
        let (jobs, rx) = channel::<(String, u32, u32, Vec<u8>)>();
        std::thread::Builder::new()
            .name("texture-dump".into())
            .spawn(move || {
                for (name, w, h, rgba) in rx {
                    if let Err(e) = write_png(&dir.join(name), w, h, &rgba) {
                        eprintln!("[textures] dump: {e}");
                    }
                }
            })
            .expect("spawn texture dump thread");
        Dumper { seen: HashSet::new(), jobs }
    }

    pub fn dump(&mut self, hash: u64, psm: gs::mem::Psm, sprite: bool, w: u32, h: u32, px: &[u32]) {
        if self.seen.insert(hash) {
            let rgba = px.iter().flat_map(|p| {
                let [r, g, b, a] = p.to_le_bytes();
                [r, g, b, (a as u32 * 255 / 128).min(255) as u8]
            });
            let kind = if sprite { "sprite" } else { "surface" };
            let _ = self.jobs.send((format!("{hash:016x}_{psm:?}_{w}x{h}_{kind}.png"), w, h, rgba.collect()));
        }
    }
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}
