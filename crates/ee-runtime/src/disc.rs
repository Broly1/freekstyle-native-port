//! Read-only access to the game disc image: raw MODE2 `.bin` (2352-byte
//! sectors) or plain `.iso` (2048-byte sectors), with ISO 9660 lookup.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const SECTOR: usize = 2048;
const RAW_SECTOR: u64 = 2352;
/// User data offset in a MODE2 Form 1 raw sector (sync + header + subheader).
const RAW_DATA: u64 = 24;

pub struct Disc {
    file: File,
    raw: bool,
    pub sectors: u32,
    root: (u32, u32),
}

/// A file found on the disc.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub lsn: u32,
    pub size: u32,
    pub dir: bool,
    /// Recording date as stored by ISO 9660: years since 1900, month, day,
    /// hour, minute, second, GMT offset.
    pub date: [u8; 7],
}

impl Disc {
    pub fn open(path: &Path) -> Result<Disc, String> {
        let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let mut sync = [0u8; 12];
        file.read_exact(&mut sync).map_err(|e| e.to_string())?;
        let raw = sync == [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0];
        let sectors = (len / if raw { RAW_SECTOR } else { SECTOR as u64 }) as u32;
        let mut disc = Disc { file, raw, sectors, root: (0, 0) };

        let pvd = disc.read_sector(16)?;
        if &pvd[1..6] != b"CD001" {
            return Err(format!("{}: no ISO 9660 volume descriptor", path.display()));
        }
        let root = parse_record(&pvd[156..]).ok_or("bad root directory record")?.1;
        disc.root = (root.lsn, root.size);
        Ok(disc)
    }

    pub fn read_sector(&mut self, lsn: u32) -> Result<[u8; SECTOR], String> {
        let mut buf = [0u8; SECTOR];
        self.read_into(lsn, &mut buf)?;
        Ok(buf)
    }

    pub fn read_into(&mut self, lsn: u32, buf: &mut [u8]) -> Result<(), String> {
        if lsn >= self.sectors {
            return Err(format!("sector {lsn} is past the end of the disc"));
        }
        let off = if self.raw { lsn as u64 * RAW_SECTOR + RAW_DATA } else { lsn as u64 * SECTOR as u64 };
        self.file.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        self.file.read_exact(&mut buf[..SECTOR]).map_err(|e| e.to_string())
    }

    /// Look up a path such as `\SXDATA\AV\ALL.AV;1` (case-insensitive;
    /// the `;1` version suffix is optional).
    pub fn find(&mut self, path: &str) -> Option<Entry> {
        let mut dir = self.root;
        let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
        let mut found = None;
        for (n, part) in parts.iter().enumerate() {
            let want = part.split(';').next().unwrap().to_ascii_uppercase();
            let e = self.list(dir).into_iter().find(|(name, _)| *name == want)?.1;
            if n + 1 < parts.len() {
                if !e.dir {
                    return None;
                }
                dir = (e.lsn, e.size);
            }
            found = Some(e);
        }
        found
    }

    /// Entries of a directory extent, names without the version suffix.
    fn list(&mut self, (lsn, size): (u32, u32)) -> Vec<(String, Entry)> {
        let mut out = Vec::new();
        for s in 0..size.div_ceil(SECTOR as u32) {
            let Ok(sector) = self.read_sector(lsn + s) else { break };
            let mut off = 0;
            // Records never cross sectors; a zero length pads to the next one.
            while off < SECTOR && sector[off] != 0 {
                if let Some((name, e)) = parse_record(&sector[off..]) {
                    out.push((name, e));
                }
                off += sector[off] as usize;
            }
        }
        out
    }
}

fn parse_record(r: &[u8]) -> Option<(String, Entry)> {
    let len = *r.first()? as usize;
    if len < 34 || r.len() < len {
        return None;
    }
    let u32le = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
    let name_len = r[32] as usize;
    let name = &r[33..33 + name_len];
    let name = match name {
        [0] => ".".to_string(),
        [1] => "..".to_string(),
        n => String::from_utf8_lossy(n).split(';').next().unwrap().to_ascii_uppercase(),
    };
    let mut date = [0u8; 7];
    date.copy_from_slice(&r[18..25]);
    Some((name, Entry { lsn: u32le(2), size: u32le(10), dir: r[25] & 2 != 0, date }))
}
