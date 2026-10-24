//! Native replacements for guest functions (high-level emulation).
//!
//! `freekstyle/hle.txt` maps guest function addresses to the functions here;
//! ee-recomp emits a call to the replacement instead of translating the
//! original body. Each replacement reads the MIPS argument registers and
//! writes its result to $v0, like the code it replaces.

use std::io::Write;

use crate::cpu::Cpu;

const V0: usize = 2;
const A0: usize = 4;
const A1: usize = 5;

/// Debug console write, normally sent to the devkit host over DECI2
/// (`tty_write(buf, len)`). Prints to stdout and returns `len`.
pub fn tty_write(c: &mut Cpu) {
    let (buf, len) = (c.r32(A0), c.r32(A1));
    let bytes: Vec<u8> = (0..len).map(|n| c.mem.r8(buf + n)).collect();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
    c.ws(V0, len);
}

// ---- SIF RPC: the EE side of every IOP request ----
//
// Client structure (sceSifClientData), as used by the game's libsif:
//   +0  pkt_addr   +4 rpc_id   +8 sema_id   +12 mode
//   +16 command    +20 buff    +24 cbuff    +28 end_function
//   +32 end_param  +36 server
// Requests are served synchronously by `crate::iop`, so a client is never
// left busy: pkt_addr is cleared and sceSifCheckStatRpc sees it idle.

const A2: usize = 6;
const A3: usize = 7;
const T0: usize = 8;
const T1: usize = 9;
const T2: usize = 10;
const T3: usize = 11;
const SP: usize = 29;

/// sceSifInitCmd / sceSifInitRpc: the IOP side is native, nothing to set up.
pub fn sif_init(c: &mut Cpu) {
    c.ws(V0, 0);
}

/// sceSifBindRpc(client, server_id, mode)
pub fn sif_bind_rpc(c: &mut Cpu) {
    let (cd, id, mode) = (c.r32(A0), c.r32(A1), c.r32(A2));
    let known = crate::iop::bind(c, id);
    c.mem.w32(cd, 0);
    c.mem.w32(cd + 16, 0);
    // A non-zero server field tells the caller the bind succeeded; storing
    // the ID itself lets sceSifCallRpc route later requests.
    c.mem.w32(cd + 36, if known { id } else { 0 });
    if c.kernel.trace {
        eprintln!("[sif] bind {id:#x} mode={mode} client={cd:#x} -> {}", if known { "ok" } else { "unknown server" });
    }
    c.ws(V0, 0);
}

/// sceSifCallRpc(client, fno, mode, send, ssize, recv, rsize, end_func, end_param)
pub fn sif_call_rpc(c: &mut Cpu) {
    let cd = c.r32(A0);
    let (fno, mode, send, ssize) = (c.r32(A1), c.r32(A2), c.r32(A3), c.r32(T0));
    let (recv, rsize, end_func) = (c.r32(T1), c.r32(T2), c.r32(T3));
    let end_param = c.mem.r32(c.r32(SP));
    let server = c.mem.r32(cd + 36);

    let request: Vec<u8> = (0..ssize).map(|n| c.mem.r8(send + n)).collect();
    let reply = crate::iop::call(c, server, fno, &request, rsize);
    for (n, b) in reply.iter().take(rsize as usize).enumerate() {
        c.mem.w8(recv + n as u32, *b);
    }
    c.mem.w32(cd, 0);
    c.mem.w32(cd + 32, end_param);
    if c.kernel.trace {
        eprintln!("[sif] call {server:#x} fno={fno:#x} mode={mode} send={ssize}B recv={rsize}B end={end_func:#x}");
    }
    c.ws(V0, 0);
    if end_func != 0 {
        // On hardware the end function runs in the SIF DMA interrupt, and
        // the kernel reschedules when the interrupt returns.
        c.run_handler(end_param, end_func, (0, 0));
        crate::kernel::reschedule(c);
    }
}

// ---- libmc: saves go to a host folder (see `crate::mc`) ----
//
// libmc calls are asynchronous: each starts a command and returns 0, and
// sceMcSync later reports (command, result). Ours finish immediately.

/// sceMcFunc* command numbers reported by sceMcSync.
mod mcfunc {
    pub const GET_INFO: i32 = 1;
    pub const OPEN: i32 = 2;
    pub const CLOSE: i32 = 3;
    pub const SEEK: i32 = 4;
    pub const READ: i32 = 5;
    pub const WRITE: i32 = 6;
    pub const FLUSH: i32 = 10;
    pub const MKDIR: i32 = 11;
    pub const CHDIR: i32 = 12;
    pub const GET_DIR: i32 = 13;
    pub const SET_FILE_INFO: i32 = 14;
    pub const DELETE: i32 = 15;
    pub const FORMAT: i32 = 16;
    pub const UNFORMAT: i32 = 17;
    pub const GET_ENT_SPACE: i32 = 18;
}

fn mc_done(c: &mut Cpu, func: i32, result: i32) {
    if c.kernel.trace {
        eprintln!("[mc] func {func} -> {result}");
    }
    c.iop.mc_pending = Some((func, result));
    c.ws(V0, 0);
}

fn cstr(c: &mut Cpu, addr: u32) -> String {
    c.mem.read_cstr(addr)
}

pub fn mc_get_info(c: &mut Cpu) {
    let (port, ty, free, format) = (c.r32(A0), c.r32(A2), c.r32(A3), c.r32(T0));
    let (result, t, f, fm) = c.iop.mc.get_info(port);
    for (p, v) in [(ty, t), (free, f), (format, fm)] {
        if p != 0 {
            c.mem.w32(p, v as u32);
        }
    }
    mc_done(c, mcfunc::GET_INFO, result);
}

pub fn mc_open(c: &mut Cpu) {
    let (port, name, mode) = (c.r32(A0), c.r32(A2), c.r32(A3));
    let name = cstr(c, name);
    let r = if port == 0 { c.iop.mc.open(&name, mode) } else { crate::mc::NO_CARD };
    mc_done(c, mcfunc::OPEN, r);
}

pub fn mc_mkdir(c: &mut Cpu) {
    let (port, name) = (c.r32(A0), c.r32(A2));
    let name = cstr(c, name);
    let r = if port == 0 { c.iop.mc.open(&name, 0x40) } else { crate::mc::NO_CARD };
    mc_done(c, mcfunc::MKDIR, r);
}

pub fn mc_close(c: &mut Cpu) {
    let r = c.iop.mc.close(c.r32(A0) as i32);
    mc_done(c, mcfunc::CLOSE, r);
}

pub fn mc_seek(c: &mut Cpu) {
    let r = c.iop.mc.seek(c.r32(A0) as i32, c.r32(A1) as i32, c.r32(A2));
    mc_done(c, mcfunc::SEEK, r);
}

pub fn mc_read(c: &mut Cpu) {
    let (fd, buf, size) = (c.r32(A0) as i32, c.r32(A1), c.r32(A2));
    let mut data = vec![0u8; size as usize];
    let n = c.iop.mc.read(fd, &mut data);
    if n > 0 {
        c.mem.write_bytes(buf, &data[..n as usize]);
    }
    mc_done(c, mcfunc::READ, n);
}

pub fn mc_write(c: &mut Cpu) {
    let (fd, buf, size) = (c.r32(A0) as i32, c.r32(A1), c.r32(A2));
    let data: Vec<u8> = (0..size).map(|n| c.mem.r8(buf + n)).collect();
    let n = c.iop.mc.write(fd, &data);
    mc_done(c, mcfunc::WRITE, n);
}

pub fn mc_flush(c: &mut Cpu) {
    let r = c.iop.mc.flush(c.r32(A0) as i32);
    mc_done(c, mcfunc::FLUSH, r);
}

pub fn mc_chdir(c: &mut Cpu) {
    let (port, name, cur) = (c.r32(A0), c.r32(A2), c.r32(A3));
    let name = cstr(c, name);
    if port != 0 {
        return mc_done(c, mcfunc::CHDIR, crate::mc::NO_CARD);
    }
    let (r, old) = c.iop.mc.chdir(&name);
    if cur != 0 {
        let mut bytes = old.into_bytes();
        bytes.push(0);
        c.mem.write_bytes(cur, &bytes);
    }
    mc_done(c, mcfunc::CHDIR, r);
}

/// sceMcGetDir(port, slot, name, mode, maxent, table): 64-byte entries.
pub fn mc_get_dir(c: &mut Cpu) {
    let (port, name, mode, max, table) = (c.r32(A0), c.r32(A2), c.r32(A3), c.r32(T0), c.r32(T1));
    let pattern = cstr(c, name);
    if port != 0 {
        return mc_done(c, mcfunc::GET_DIR, crate::mc::NO_CARD);
    }
    if c.kernel.trace {
        eprintln!("[mc] getdir {pattern} mode={mode} max={max}");
    }
    let entries = match c.iop.mc.get_dir(&pattern, mode != 0, max as usize) {
        Ok(e) => e,
        Err(code) => return mc_done(c, mcfunc::GET_DIR, code),
    };
    for (n, e) in entries.iter().enumerate() {
        let mut rec = [0u8; 64];
        let t = jst_datetime(e.modified);
        rec[0..8].copy_from_slice(&t);
        rec[8..16].copy_from_slice(&t);
        rec[16..20].copy_from_slice(&e.size.to_le_bytes());
        let attr: u16 = if e.dir { 0x8427 } else { 0x8497 };
        rec[20..22].copy_from_slice(&attr.to_le_bytes());
        let name = e.name.as_bytes();
        rec[32..32 + name.len().min(31)].copy_from_slice(&name[..name.len().min(31)]);
        c.mem.write_bytes(table + n as u32 * 64, &rec);
    }
    mc_done(c, mcfunc::GET_DIR, entries.len() as i32);
}

pub fn mc_set_file_info(c: &mut Cpu) {
    mc_done(c, mcfunc::SET_FILE_INFO, 0);
}

pub fn mc_delete(c: &mut Cpu) {
    let (port, name) = (c.r32(A0), c.r32(A2));
    let name = cstr(c, name);
    let r = if port == 0 { c.iop.mc.delete(&name) } else { crate::mc::NO_CARD };
    mc_done(c, mcfunc::DELETE, r);
}

pub fn mc_format(c: &mut Cpu) {
    mc_done(c, mcfunc::FORMAT, 0);
}

pub fn mc_unformat(c: &mut Cpu) {
    mc_done(c, mcfunc::UNFORMAT, 0);
}

pub fn mc_get_ent_space(c: &mut Cpu) {
    mc_done(c, mcfunc::GET_ENT_SPACE, 1024);
}

/// sceMcSync(mode, &cmd, &result): 1 when a command finished, -1 if none.
pub fn mc_sync(c: &mut Cpu) {
    let (cmd, result) = (c.r32(A1), c.r32(A2));
    match c.iop.mc_pending.take() {
        Some((func, r)) => {
            if cmd != 0 {
                c.mem.w32(cmd, func as u32);
            }
            if result != 0 {
                c.mem.w32(result, r as u32);
            }
            c.ws(V0, 1);
        }
        None => c.ws(V0, u32::MAX),
    }
}

/// sceMcStDateTime in Japan time (UTC+9), as the PS2 stores it.
fn jst_datetime(t: std::time::SystemTime) -> [u8; 8] {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) + 9 * 3600;
    let (days, rem) = (secs / 86400, secs % 86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as u16;
    let y = year.to_le_bytes();
    [0, (rem % 60) as u8, (rem / 60 % 60) as u8, (rem / 3600) as u8, day, month, y[0], y[1]]
}

// ---- EA audio driver (AUDIO.IRX) ----
//
// Each frame the game sends an audio command packet to EA's IOP driver
// (0x192f00) and then waits for the driver's reply callback (0x192d20)
// to set a flag. Until the driver is implemented natively, the wait
// completes at once with no events, as if the IOP acknowledged silently.

const GP: usize = 28;
/// gp-relative flags: request outstanding, reply received.
const EA_AUDIO_PENDING: u32 = 2732;
const EA_AUDIO_REPLIED: u32 = 2736;
/// Driver ready for the next packet (set by the reply callback).
const EA_AUDIO_READY: u32 = 3080;

pub fn ea_audio_sync(c: &mut Cpu) {
    let gp = c.r32(GP);
    c.mem.w32(gp + EA_AUDIO_PENDING, 0);
    c.mem.w32(gp + EA_AUDIO_REPLIED, 0);
    c.mem.w32(gp + EA_AUDIO_READY, 1);
    c.kernel.warn_once("EA audio driver not implemented: running silent");
    c.ws(V0, 0);
}

// ---- Movies ----

/// The game's movie API (EA .FLM via the IPU): open (0x1ac580), blocking
/// play (0x1ab670) and close (0x1ab620). The IPU is not emulated yet, so
/// all three do nothing and movies end immediately instead of hanging on
/// the decoder.
pub fn movie_play(c: &mut Cpu) {
    c.kernel.warn_once("movies are skipped until the IPU (MPEG decoder) is implemented");
    c.ws(V0, 0);
}
