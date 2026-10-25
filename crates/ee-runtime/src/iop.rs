//! Native IOP: the services the game reaches over SIF RPC, implemented in
//! Rust instead of emulating the I/O processor and its modules.
//!
//! Request and reply layouts follow what the game's libraries actually send
//! (dump them with FREEK_RPC_DUMP=1).

use crate::cpu::Cpu;
use crate::disc::Disc;

pub const CD_INIT: u32 = 0x8000_0592;
pub const CD_SCMD: u32 = 0x8000_0593;
pub const CD_NCMD: u32 = 0x8000_0595;
pub const CD_SEARCH_FILE: u32 = 0x8000_0597;
pub const CD_DISK_READY: u32 = 0x8000_059a;
pub const PADMAN: u32 = 0x8000_0100;
pub const PADMAN_EXT: u32 = 0x8000_0101;
pub const MCSERV: u32 = 0x8000_0400;

/// sceCdGetDiskType: PlayStation 2 CD-ROM.
const DISK_TYPE_PS2_CD: u32 = 0x12;
/// sceCdDiskReady: SCECdComplete.
const DISK_READY: u32 = 2;

#[derive(Default)]
pub struct Iop {
    pub disc: Option<Disc>,
    pub mc: crate::mc::Card,
    /// libmc command awaiting sceMcSync: (sceMcFunc, result).
    pub mc_pending: Option<(i32, i32)>,
    /// Files served with changes: (first sector, patched contents).
    pub patched: Vec<(u32, Vec<u8>)>,
    /// The emulated IOP running the sound driver, if audio is enabled.
    pub link: Option<std::sync::Arc<ps2_iop::Link>>,
    /// Rotates the buffers that IOP command packets are delivered in.
    pub cmd_slot: u32,
    /// Converted pictures (see `images`) and their index, loaded on use.
    pub images: std::path::PathBuf,
    pub image_index: Option<std::collections::HashMap<u64, String>>,
}

impl Iop {
    /// A whole file from the disc, by ISO path (e.g. `\\MODULES\\AUDIO.IRX`).
    pub fn read_file(&mut self, path: &str) -> Option<Vec<u8>> {
        let disc = self.disc.as_mut()?;
        let e = disc.find(path)?;
        let sector = crate::disc::SECTOR;
        let mut data = vec![0u8; e.size.div_ceil(sector as u32) as usize * sector];
        for (n, chunk) in data.chunks_mut(sector).enumerate() {
            disc.read_into(e.lsn + n as u32, chunk).ok()?;
        }
        data.truncate(e.size as usize);
        Some(data)
    }

    /// Prepare file patches for the PC version (memory card wording).
    pub fn patch_disc_files(&mut self) {
        let Some(disc) = self.disc.as_mut() else { return };
        let Some(e) = disc.find("\\SXDATA\\FEND\\ENGLISH.DAT") else { return };
        let mut data = vec![0u8; e.size.div_ceil(crate::disc::SECTOR as u32) as usize * crate::disc::SECTOR];
        for (n, chunk) in data.chunks_mut(crate::disc::SECTOR).enumerate() {
            if disc.read_into(e.lsn + n as u32, chunk).is_err() {
                return;
            }
        }
        let n = crate::text::patch_strings(&mut data[..e.size as usize]);
        eprintln!("[text] rewrote {n} memory card messages in ENGLISH.DAT");
        self.patched.push((e.lsn, data));
    }
}

/// Whether a server ID is one we serve.
pub fn bind(c: &mut Cpu, id: u32) -> bool {
    let name = server_name(id);
    c.kernel.warn_once(&format!("IOP bind {id:#010x} ({name})"));
    true
}

fn word(v: u32, rsize: u32) -> Vec<u8> {
    let mut r = vec![0; rsize.max(4) as usize];
    r[..4].copy_from_slice(&v.to_le_bytes());
    r
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).unwrap_or(0)
}

/// Serve one RPC request; returns the reply bytes for the receive buffer.
pub fn call(c: &mut Cpu, server: u32, fno: u32, request: &[u8], rsize: u32) -> Vec<u8> {
    if std::env::var_os("FREEK_RPC_DUMP").is_some() {
        dump(server, fno, request, rsize);
    }
    match (server, fno) {
        (CD_INIT, _) => word(1, rsize),
        (CD_SCMD, 0x03) => word(DISK_TYPE_PS2_CD, rsize),
        (CD_SCMD, 0x22) => word(1, rsize), // sceCdMmode
        (CD_DISK_READY, _) => word(DISK_READY, rsize),
        (CD_SEARCH_FILE, _) => search_file(c, request, rsize),
        (CD_NCMD, 0x01) => cd_read(c, request, rsize),
        // Unidentified status query issued before reads; 0 = no error/idle.
        (CD_NCMD, 0x0e) => word(0, rsize),
        (PADMAN, _) | (PADMAN_EXT, _) => c.mem.pad.rpc(request, rsize),
        // mcserv/mcman versions for sceMcInit (needs >= 522 and >= 526);
        // the other libmc calls are replaced at the EE level.
        (MCSERV, 0xfe) => {
            let mut r = vec![0u8; rsize.max(12) as usize];
            r[4..8].copy_from_slice(&0x20eu32.to_le_bytes());
            r[8..12].copy_from_slice(&0x20eu32.to_le_bytes());
            r.truncate(rsize as usize);
            r
        }
        _ => {
            let name = server_name(server);
            c.kernel.warn_once(&format!("IOP call {server:#010x} ({name}) fno={fno:#x}: not implemented, replying zeros"));
            vec![0; rsize as usize]
        }
    }
}

/// sceCdSearchFile. Request: a 32-byte sceCdlFILE, 4 bytes, the path at
/// +0x24 (256 bytes), then the EE address of the sceCdlFILE to fill.
/// Reply: 1 if found.
fn search_file(c: &mut Cpu, request: &[u8], rsize: u32) -> Vec<u8> {
    let path_bytes = request.get(0x24..0x124).unwrap_or(&[]);
    let path: String = path_bytes.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
    let dest = u32_at(request, 0x124);
    let Some(disc) = c.iop.disc.as_mut() else {
        c.kernel.warn_once("sceCdSearchFile: no disc image loaded");
        return word(0, rsize);
    };
    let Some(e) = disc.find(&path) else {
        eprintln!("[cd] search {path}: not found");
        return word(0, rsize);
    };
    if c.kernel.trace {
        eprintln!("[cd] search {path}: lsn={} size={}", e.lsn, e.size);
    }
    // sceCdlFILE { lsn, size, name[16], date[8] }
    c.mem.w32(dest, e.lsn);
    c.mem.w32(dest + 4, e.size);
    let name = path.rsplit('\\').next().unwrap_or("");
    for n in 0..16 {
        c.mem.w8(dest + 8 + n, *name.as_bytes().get(n as usize).unwrap_or(&0));
    }
    // date: [0, sec, min, hour, day, month, year lo, year hi]
    let [y, mo, d, h, mi, s, _] = e.date;
    let year = 1900 + y as u16;
    let date = [0, s, mi, h, d, mo, year as u8, (year >> 8) as u8];
    for (n, b) in date.iter().enumerate() {
        c.mem.w8(dest + 24 + n as u32, *b);
    }
    word(1, rsize)
}

/// sceCdRead. Request: lsn, sector count, EE buffer, read mode, then two EE
/// scratch areas the IOP uses for unaligned head/tail bytes. We write the
/// whole transfer straight to the buffer and leave the scratch areas empty
/// (their size words stay 0, so the completion callback copies nothing).
fn cd_read(c: &mut Cpu, request: &[u8], rsize: u32) -> Vec<u8> {
    let (lsn, count, buf) = (u32_at(request, 0), u32_at(request, 4), u32_at(request, 8));
    if c.kernel.trace {
        eprintln!("[cd] read lsn={lsn} sectors={count} -> {buf:#x}");
    }
    let Some(disc) = c.iop.disc.as_mut() else {
        c.kernel.warn_once("sceCdRead: no disc image loaded");
        return vec![0; rsize as usize];
    };
    let mut data = vec![0u8; count as usize * crate::disc::SECTOR];
    for (n, chunk) in data.chunks_mut(crate::disc::SECTOR).enumerate() {
        if let Err(e) = disc.read_into(lsn + n as u32, chunk) {
            eprintln!("[cd] read error: {e}");
            break;
        }
    }
    // Overlay patched files.
    for (start, file) in &c.iop.patched {
        let file_sectors = (file.len() / crate::disc::SECTOR) as u32;
        for n in 0..count {
            let s = lsn + n;
            if (*start..*start + file_sectors).contains(&s) {
                let off = ((s - start) as usize) * crate::disc::SECTOR;
                let dst = n as usize * crate::disc::SECTOR;
                data[dst..dst + crate::disc::SECTOR].copy_from_slice(&file[off..off + crate::disc::SECTOR]);
            }
        }
    }
    c.mem.write_bytes(buf, &data);
    vec![0; rsize as usize]
}

fn dump(server: u32, fno: u32, request: &[u8], rsize: u32) {
    eprintln!("[rpc] {server:#010x} fno={fno:#x} rsize={rsize} request:");
    for (n, chunk) in request.chunks(16).enumerate().take(24) {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let text: String = chunk.iter().map(|&b| if (32..127).contains(&b) { b as char } else { '.' }).collect();
        eprintln!("  {:04x}: {:<48} {text}", n * 16, hex.join(" "));
    }
}

pub fn server_name(id: u32) -> &'static str {
    match id {
        0x8000_0001 => "fileio",
        0x8000_0003 => "iop heap",
        0x8000_0006 => "module loader",
        0x8000_0100 | 0x8000_0101 => "pad",
        0x8000_0400 => "memory card",
        CD_INIT => "cdvd init",
        CD_SCMD => "cdvd s-command",
        CD_NCMD => "cdvd n-command",
        CD_SEARCH_FILE => "cdvd search file",
        CD_DISK_READY => "cdvd disk ready",
        _ => "unknown",
    }
}
