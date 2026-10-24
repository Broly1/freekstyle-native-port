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

/// sceCdGetDiskType: PlayStation 2 CD-ROM.
const DISK_TYPE_PS2_CD: u32 = 0x12;
/// sceCdDiskReady: SCECdComplete.
const DISK_READY: u32 = 2;

#[derive(Default)]
pub struct Iop {
    pub disc: Option<Disc>,
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
