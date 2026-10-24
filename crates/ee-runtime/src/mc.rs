//! Saves without memory cards: the game's libmc calls are served from a
//! directory on the host, like a PC game's save folder.
//!
//! Port 0 behaves as an always-inserted, formatted 8 MB card whose root is
//! `$XDG_DATA_HOME/freekstyle/saves` (default `~/.local/share/...`). Port 1
//! is empty. Paths are confined to the save folder.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// sceMcRes* result codes.
pub const NO_CARD: i32 = -10;
pub const NO_ENTRY: i32 = -4;
pub const NOT_EMPTY: i32 = -6;
pub const TOO_MANY_HANDLES: i32 = -7;
pub const DENIED: i32 = -5;

/// Open flags.
const O_WRONLY: u32 = 0x2;
const O_RDWR: u32 = 0x3;
const O_CREAT: u32 = 0x200;
const O_TRUNC: u32 = 0x400;
const O_MKDIR: u32 = 0x40;

/// Free space reported, in 1 KB clusters (an empty 8 MB card).
const FREE_CLUSTERS: i32 = 8000;
const MAX_FILES: usize = 16;

pub struct Card {
    root: PathBuf,
    cwd: String,
    files: Vec<Option<File>>,
    /// The first GetInfo reports a newly inserted card, later ones the same card.
    seen: bool,
    /// Pending GetDir enumeration (for mode 1 = continue).
    dir_iter: Vec<DirEntry>,
}

pub struct DirEntry {
    pub name: String,
    pub size: u32,
    pub dir: bool,
    pub modified: SystemTime,
}

impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}

impl Card {
    pub fn new() -> Card {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        Card { root: base.join("freekstyle/saves"), cwd: "/".into(), files: Vec::new(), seen: false, dir_iter: Vec::new() }
    }

    /// Host path for a card path, relative to the current directory. Rejects
    /// anything that would leave the save folder.
    fn path(&self, name: &str) -> Option<PathBuf> {
        let full = if name.starts_with('/') { name.to_string() } else { format!("{}/{name}", self.cwd) };
        let mut out = self.root.clone();
        for c in Path::new(&full).components() {
            match c {
                Component::Normal(p) => out.push(p),
                Component::RootDir | Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() || !out.starts_with(&self.root) {
                        return None;
                    }
                }
                Component::Prefix(_) => return None,
            }
        }
        out.starts_with(&self.root).then_some(out)
    }

    pub fn get_info(&mut self, port: u32) -> (i32, i32, i32, i32) {
        if port != 0 {
            return (NO_CARD, 0, 0, 0);
        }
        let _ = fs::create_dir_all(&self.root);
        let result = if std::mem::replace(&mut self.seen, true) { 0 } else { -1 };
        // (result, type = PS2 card, free clusters, formatted)
        (result, 2, FREE_CLUSTERS, 1)
    }

    pub fn open(&mut self, name: &str, flags: u32) -> i32 {
        let Some(p) = self.path(name) else { return NO_ENTRY };
        if flags & O_MKDIR != 0 {
            return match fs::create_dir(&p) {
                Ok(()) => 0,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => DENIED,
                Err(_) => NO_ENTRY,
            };
        }
        let write = flags & 3 == O_WRONLY || flags & 3 == O_RDWR;
        let f = OpenOptions::new()
            .read(flags & 3 != O_WRONLY)
            .write(write || flags & O_CREAT != 0)
            .create(flags & O_CREAT != 0)
            .truncate(flags & O_TRUNC != 0)
            .open(&p);
        let Ok(f) = f else { return NO_ENTRY };
        if p.is_dir() {
            return NO_ENTRY;
        }
        let slot = match self.files.iter().position(Option::is_none) {
            Some(i) => i,
            None if self.files.len() < MAX_FILES => {
                self.files.push(None);
                self.files.len() - 1
            }
            None => return TOO_MANY_HANDLES,
        };
        self.files[slot] = Some(f);
        slot as i32
    }

    pub fn close(&mut self, fd: i32) -> i32 {
        match self.files.get_mut(fd as usize).and_then(Option::take) {
            Some(_) => 0,
            None => NO_ENTRY,
        }
    }

    fn file(&mut self, fd: i32) -> Option<&mut File> {
        self.files.get_mut(fd as usize).and_then(Option::as_mut)
    }

    pub fn seek(&mut self, fd: i32, offset: i32, origin: u32) -> i32 {
        let Some(f) = self.file(fd) else { return NO_ENTRY };
        let to = match origin {
            1 => SeekFrom::Current(offset as i64),
            2 => SeekFrom::End(offset as i64),
            _ => SeekFrom::Start(offset.max(0) as u64),
        };
        f.seek(to).map(|p| p as i32).unwrap_or(DENIED)
    }

    pub fn read(&mut self, fd: i32, buf: &mut [u8]) -> i32 {
        let Some(f) = self.file(fd) else { return NO_ENTRY };
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => n += k,
            }
        }
        n as i32
    }

    pub fn write(&mut self, fd: i32, data: &[u8]) -> i32 {
        let Some(f) = self.file(fd) else { return NO_ENTRY };
        f.write_all(data).map(|_| data.len() as i32).unwrap_or(DENIED)
    }

    pub fn flush(&mut self, fd: i32) -> i32 {
        let Some(f) = self.file(fd) else { return NO_ENTRY };
        let _ = f.sync_all();
        0
    }

    pub fn chdir(&mut self, name: &str) -> (i32, String) {
        let old = self.cwd.clone();
        match self.path(name) {
            Some(p) if p.is_dir() => {
                let rel = p.strip_prefix(&self.root).unwrap().to_string_lossy().into_owned();
                self.cwd = format!("/{rel}");
                (0, old)
            }
            _ => (NO_ENTRY, old),
        }
    }

    /// GetDir with a pattern such as "/BASLUS-20367*" or "/SAVE/*".
    /// `cont` continues a previous listing. Returns up to `max` entries, or
    /// NO_ENTRY when the directory itself does not exist.
    pub fn get_dir(&mut self, pattern: &str, cont: bool, max: usize) -> Result<Vec<DirEntry>, i32> {
        if !cont {
            self.dir_iter.clear();
            let (dir, mask) = match pattern.rfind('/') {
                Some(i) => (&pattern[..i.max(1)], &pattern[i + 1..]),
                None => (".", pattern),
            };
            let dir = if dir.is_empty() { "/" } else { dir };
            match self.path(dir) {
                Some(p) if p.is_dir() => {}
                _ => return Err(NO_ENTRY),
            }
            if let Some(p) = self.path(dir) {
                let in_subdir = p != self.root;
                if in_subdir && wildcard(mask, ".") {
                    self.dir_iter.push(DirEntry { name: ".".into(), size: 0, dir: true, modified: SystemTime::now() });
                    self.dir_iter.push(DirEntry { name: "..".into(), size: 0, dir: true, modified: SystemTime::now() });
                }
                if let Ok(rd) = fs::read_dir(&p) {
                    let mut entries: Vec<DirEntry> = rd
                        .flatten()
                        .filter_map(|e| {
                            let name = e.file_name().to_string_lossy().into_owned();
                            let m = e.metadata().ok()?;
                            wildcard(mask, &name).then(|| DirEntry {
                                name,
                                size: m.len() as u32,
                                dir: m.is_dir(),
                                modified: m.modified().unwrap_or(SystemTime::now()),
                            })
                        })
                        .collect();
                    entries.sort_by(|a, b| a.name.cmp(&b.name));
                    self.dir_iter.extend(entries);
                }
            }
            self.dir_iter.reverse();
        }
        let mut out = Vec::new();
        while out.len() < max {
            match self.dir_iter.pop() {
                Some(e) => out.push(e),
                None => break,
            }
        }
        Ok(out)
    }

    pub fn delete(&mut self, name: &str) -> i32 {
        let Some(p) = self.path(name) else { return NO_ENTRY };
        let r = if p.is_dir() { fs::remove_dir(&p) } else { fs::remove_file(&p) };
        match r {
            Ok(()) => 0,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => NO_ENTRY,
            Err(_) => NOT_EMPTY,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// `*` and `?` matching, case-insensitive like the card's file system.
fn wildcard(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.to_ascii_uppercase().chars().collect(), name.to_ascii_uppercase().chars().collect());
    fn m(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => m(&p[1..], n) || (!n.is_empty() && m(p, &n[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &n[1..]),
            _ => false,
        }
    }
    m(&p, &n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Card {
        let dir = std::env::temp_dir().join(format!("freekstyle-mc-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Card { root: dir, ..Card::new() }
    }

    #[test]
    fn save_roundtrip_and_listing() {
        let mut c = card();
        assert_eq!(c.get_info(0).0, -1);
        assert_eq!(c.get_info(0).0, 0);
        assert_eq!(c.get_info(1).0, NO_CARD);
        assert_eq!(c.open("/BASLUS-20367FREEK", O_MKDIR), 0);
        assert_eq!(c.chdir("/BASLUS-20367FREEK").0, 0);
        let fd = c.open("SAVE.DAT", O_CREAT | O_WRONLY);
        assert!(fd >= 0);
        assert_eq!(c.write(fd, b"hello"), 5);
        assert_eq!(c.close(fd), 0);
        let fd = c.open("/BASLUS-20367FREEK/SAVE.DAT", 1);
        let mut buf = [0u8; 8];
        assert_eq!(c.read(fd, &mut buf), 5);
        assert_eq!(&buf[..5], b"hello");
        let names: Vec<String> = c.get_dir("/BASLUS-20367FREEK/*", false, 10).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, [".", "..", "SAVE.DAT"]);
        assert_eq!(c.get_dir("/BAS*", false, 10).unwrap().len(), 1);
        // A missing directory is an error, not an empty listing.
        assert_eq!(c.get_dir("/MISSING/*", false, 10).err(), Some(NO_ENTRY));
        // Escaping the save folder is refused.
        assert_eq!(c.open("/../../etc/passwd", 1), NO_ENTRY);
        let _ = fs::remove_dir_all(c.root());
    }
}
