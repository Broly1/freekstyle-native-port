//! Which of the pack's redrawn textures this tool made, and by which
//! version of their drawing, so that a better drawing reaches packs that
//! already exist, once, and hand-made files are left alone.
//!
//! `redrawn.txt` in the pack holds a line per texture: hash, version and a
//! digest of the file as written. A file that no longer matches its digest
//! was changed by hand. Packs from before the list have only the files: one
//! counts as made here when it is saved the way `pack::save` saves, and it
//! is copied to `replaced/` in the pack before it is drawn over (copied
//! back, it counts as hand-made and stays).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const LIST: &str = "redrawn.txt";
/// Where files are kept that were replaced without being in the list. The
/// game reads only the pack folder itself, not the folders in it.
pub const REPLACED: &str = "replaced";

pub struct Ledger {
    dir: PathBuf,
    made: BTreeMap<String, (u32, u64)>,
}

impl Ledger {
    pub fn read(dir: &Path) -> Ledger {
        let mut made = BTreeMap::new();
        for line in std::fs::read_to_string(dir.join(LIST)).unwrap_or_default().lines() {
            if let [hash, version, sum] = line.split_whitespace().collect::<Vec<_>>()[..] {
                if let (Ok(version), Ok(sum)) = (version.parse(), u64::from_str_radix(sum, 16)) {
                    made.insert(hash.to_string(), (version, sum));
                }
            }
        }
        Ledger { dir: dir.to_path_buf(), made }
    }

    /// Whether `file`, the pack's texture for the recorded `source`, was
    /// made here by a drawing older than `version`.
    pub fn outdated(&self, hash: &str, version: u32, file: &Path, source: &Path) -> bool {
        // The tool names its files as the recording does.
        if file.file_name() != source.file_name() {
            return false;
        }
        let Ok(bytes) = std::fs::read(file) else { return false };
        match self.made.get(hash) {
            Some(&(made, sum)) => made < version && sum == digest(&bytes),
            None => saved_here(&bytes, source),
        }
    }

    /// Before `file` is drawn over: keep a copy, unless it is in the list
    /// (then it can be drawn again).
    pub fn keep(&self, hash: &str, file: &Path) -> Result<(), String> {
        if self.made.contains_key(hash) || !file.is_file() {
            return Ok(());
        }
        let Some(name) = file.file_name() else { return Ok(()) };
        let dir = self.dir.join(REPLACED);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let copy = dir.join(name);
        if !copy.exists() {
            std::fs::copy(file, &copy).map_err(|e| format!("{}: {e}", copy.display()))?;
        }
        Ok(())
    }

    /// Note that `file` was just drawn by `version`. Written at once: the
    /// build may be stopped after any texture.
    pub fn note(&mut self, hash: &str, version: u32, file: &Path) -> Result<(), String> {
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        self.made.insert(hash.to_string(), (version, digest(&bytes)));
        let text: String = self.made.iter().map(|(h, (v, sum))| format!("{h} {v} {sum:016x}\n")).collect();
        let (list, part) = (self.dir.join(LIST), self.dir.join("redrawn.part"));
        std::fs::write(&part, text).map_err(|e| format!("{}: {e}", part.display()))?;
        std::fs::rename(&part, &list).map_err(|e| format!("{}: {e}", list.display()))
    }
}

/// FNV-1a: it has to come out the same in every build, which std's hasher
/// doesn't promise.
fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Whether a PNG looks like `pack::save`'s: 8-bit RGBA at 4x (upscaled) or
/// 8x (redrawn) of the recording, no chunks besides the picture's, which is
/// one chunk of fast-deflate. Picture editors and Pillow save differently
/// (more chunks, or another compression level).
fn saved_here(png: &[u8], source: &Path) -> bool {
    const HEADER: usize = 8 + 25;
    if png.len() < HEADER + 10 || &png[12..16] != b"IHDR" || png[24..29] != [8, 6, 0, 0, 0] {
        return false;
    }
    let word = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
    let Ok((w, h)) = image::image_dimensions(source) else { return false };
    if ![4, 8].iter().any(|&s| (word(16), word(20)) == (w * s, h * s)) {
        return false;
    }
    let end = HEADER + 12 + word(HEADER) as usize;
    &png[HEADER + 4..HEADER + 8] == b"IDAT"
        && png[HEADER + 8..HEADER + 10] == [0x78, 0x01]
        && png.len() == end + 12
        && &png[end + 4..end + 8] == b"IEND"
}
