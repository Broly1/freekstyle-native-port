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
const GP: usize = 28;

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

// ---- SIF commands and IOP modules ----
//
// The IOP is emulated by `ps2_iop` on its own thread. Commands and DMA go
// to it through the link; commands from it are delivered by
// `sif_receive` at the next interrupt check, as the SIF0 interrupt would.

/// libkernel's software register array (sceSifGetSreg reads it).
const EE_SREG: u32 = 0x0024_6e00;
/// libkernel's user command table: pointer, then entry count.
const EE_CMD_TABLE: u32 = 0x0024_6cec;
/// Packets from the IOP are placed here (unused kernel memory).
const EE_CMD_BUF: u32 = 0x000f_f000;
const CMD_SLOT: u32 = 128;

/// sceSifSendCmd / isceSifSendCmd(cid, packet, psize, src, dest, size)
pub fn sif_send_cmd(c: &mut Cpu) {
    let (cid, pkt, psize, src) = (c.r32(A0), c.r32(A1), c.r32(A2).min(CMD_SLOT), c.r32(A3));
    let (dest, size) = (c.r32(T0), c.r32(T1));
    // Header: psize:8 dsize:24, dest, cid; opt is the sender's.
    c.mem.w32(pkt, psize | size << 8);
    c.mem.w32(pkt + 4, dest);
    c.mem.w32(pkt + 8, cid);
    let packet: Vec<u8> = (0..psize).map(|i| c.mem.r8(pkt + i)).collect();
    let extra = (size > 0).then(|| (dest, (0..size).map(|i| c.mem.r8(src + i)).collect()));
    match &c.iop.link {
        Some(link) => link.send(ps2_iop::ToIop::Cmd { cid, packet, extra }),
        None => c.kernel.warn_once("SIF command to the IOP dropped: audio disabled"),
    }
    c.ws(V0, 1);
}

/// sceSifLoadModule(path, arglen, args). The sound modules run on the
/// emulated IOP; other modules' services are native and always present.
pub fn sif_load_module(c: &mut Cpu) {
    let (path_addr, arglen, args) = (c.r32(A0), c.r32(A1), c.r32(A2));
    let path: String = (0..256).map(|i| c.mem.r8(path_addr + i)).take_while(|&b| b != 0).map(|b| b as char).collect();
    let file = path.rsplit(['\\', '/', ':']).next().unwrap_or("").split(';').next().unwrap_or("").to_ascii_uppercase();
    if matches!(file.as_str(), "LIBSD.IRX" | "AUDIO.IRX") {
        if let Some(link) = c.iop.link.clone() {
            match c.iop.read_file(&format!("\\MODULES\\{file}")) {
                Some(image) => {
                    let args = (0..arglen).map(|i| c.mem.r8(args + i)).collect();
                    let target = link.loaded() + 1;
                    link.send(ps2_iop::ToIop::Load { name: file.clone(), image, args });
                    // LoadModule returns after the module has started.
                    let start = std::time::Instant::now();
                    while link.loaded() < target && start.elapsed() < std::time::Duration::from_secs(5) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
                None => eprintln!("[iop] {path}: not found on the disc"),
            }
        }
    }
    if c.kernel.trace {
        eprintln!("[sif] load module {path}");
    }
    c.ws(V0, 0);
}

impl Cpu {
    /// Deliver messages from the IOP: DMA writes and SIF commands.
    pub fn sif_receive(&mut self) -> bool {
        let Some(link) = self.iop.link.clone() else { return false };
        if !link.has_mail() {
            return false;
        }
        let msgs = link.take();
        let n = msgs.len();
        for m in msgs {
            match m {
                ps2_iop::ToEe::Dma { dest, data } => self.write_bytes(dest, &data),
                ps2_iop::ToEe::Cmd { cid, packet, extra } => {
                    if let Some((dest, data)) = extra {
                        self.write_bytes(dest, &data);
                    }
                    self.sif_command(cid, &packet);
                }
            }
        }
        link.handled(n);
        true
    }

    fn write_bytes(&mut self, dest: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.mem.w8(dest + i as u32, b);
        }
    }

    fn sif_command(&mut self, cid: u32, packet: &[u8]) {
        let word = |o: usize| packet.get(o..o + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
        if cid & 0x8000_0000 != 0 {
            // SIF_CMD_SET_SREG; the others concern the SIF's own setup.
            if cid == 0x8000_0001 {
                self.mem.w32(EE_SREG + (word(16) & 31) * 4, word(20));
            }
            return;
        }
        let (table, n) = (self.mem.r32(EE_CMD_TABLE), self.mem.r32(EE_CMD_TABLE + 4));
        let (func, data) = if table != 0 && cid < n { (self.mem.r32(table + cid * 8), self.mem.r32(table + cid * 8 + 4)) } else { (0, 0) };
        if func == 0 {
            self.kernel.warn_once(&format!("SIF command {cid:#x} from the IOP has no handler"));
            return;
        }
        let slot = EE_CMD_BUF + (self.iop.cmd_slot % 16) * CMD_SLOT;
        self.iop.cmd_slot += 1;
        self.write_bytes(slot, &packet[..packet.len().min(CMD_SLOT as usize)]);
        self.run_handler(slot, func, (data, 0));
    }
}

// ---- Pictures (.LSC) ----

/// The game's picture decoder, `0x192090(data, size)`: decodes an "ipum"
/// MPEG still with the IPU into a buffer of 16x16 macroblocks, uploads it
/// to GS memory as a texture and returns a 36-byte picture record. Here
/// the pixels come from `game/images/<NAME>.png` (see `images`), so the
/// IPU is not involved; allocation and upload still use the game's own
/// functions.
///
/// Record: +0 GS address, +4 0, +8 width, +12 height, +16 texture buffer
/// width (64-pixel units), +20/+24 log2 of the texture size, +28/+32 the
/// used fraction of it (float).
pub fn lsc_decode(c: &mut Cpu) {
    let (data, size) = (c.r32(A0), c.r32(A1));
    if data == 0 {
        c.ws(V0, 0);
        return;
    }
    let call = |c: &mut Cpu, f: u32, args: &[u32]| -> u32 {
        for (i, &a) in args.iter().enumerate() {
            c.ws(A0 + i, a);
        }
        c.call(f);
        c.r32(V0)
    };
    let bytes: Vec<u8> = (0..size).map(|i| c.mem.r8(data + i)).collect();
    let (w, h) = (u16::from_le_bytes([bytes[8], bytes[9]]) as u32, u16::from_le_bytes([bytes[10], bytes[11]]) as u32);
    let px = picture_pixels(c, &bytes, w, h);

    call(c, 0x191e00, &[]);
    let rec = call(c, 0x122e10, &[36, 2, 16]);
    let (mbw, mbh) = (w.div_ceil(16), h.div_ceil(16));
    let buf = call(c, 0x122e10, &[mbw * mbh * 1024, 1, 64]);
    // Macroblock order, as the IPU outputs it.
    for mb in 0..mbw * mbh {
        let (mx, my) = ((mb % mbw) * 16, (mb / mbw) * 16);
        for i in 0..256 {
            let (x, y) = (mx + i % 16, my + i / 16);
            let p = if x < w && y < h { px[(y * w + x) as usize] } else { 0 };
            c.mem.w32(buf + mb * 1024 + i * 4, p);
        }
    }
    let log2 = |v: u32| 32 - (v.max(1) - 1).leading_zeros();
    let (tw, th) = (log2(w), log2(h));
    c.mem.w32(rec + 8, w);
    c.mem.w32(rec + 12, h);
    c.mem.w32(rec + 4, 0);
    c.mem.w32(rec + 20, tw);
    c.mem.w32(rec + 24, th);
    c.mem.w32(rec + 16, (1u32 << tw).div_ceil(64));
    let vram = call(c, 0x12e640, &[0x0024_d330, ((1u32 << tw) * (1 << th) * 4) / 256]);
    c.mem.w32(rec, vram);
    c.mem.w32(rec + 28, (w as f32 / (1u32 << tw) as f32).to_bits());
    c.mem.w32(rec + 32, (h as f32 / (1u32 << th) as f32).to_bits());
    let gp = c.r32(GP);
    let ctx = c.mem.r32(gp.wrapping_sub(18064)) + 4;
    call(c, 0x191c90, &[ctx, buf, vram, w, h, 1]);
    let ctx = c.mem.r32(gp.wrapping_sub(18064)) + 4;
    call(c, 0x191bb0, &[ctx]);
    let ctx = c.mem.r32(gp.wrapping_sub(18064)) + 4;
    call(c, 0x191c60, &[ctx]);
    call(c, 0x191e40, &[]);
    call(c, 0x1231a0, &[buf]);
    c.ws(V0, rec);
}

/// The picture's pixels: its PNG if present (scaled if replaced at
/// another size), else decoded from the original data.
fn picture_pixels(c: &mut Cpu, bytes: &[u8], w: u32, h: u32) -> Vec<u32> {
    let dir = c.iop.images.clone();
    let index = c.iop.image_index.get_or_insert_with(|| crate::images::load_index(&dir));
    if let Some(name) = index.get(&crate::images::content_hash(bytes)).cloned() {
        match crate::images::load_png(&dir.join(&name)) {
            Ok((pw, ph, px)) if (pw, ph) == (w, h) => return px,
            Ok((pw, ph, px)) => return crate::images::resize(pw, ph, &px, w, h),
            Err(e) => eprintln!("[images] {}: {e}; decoding the original", dir.join(&name).display()),
        }
    } else {
        c.kernel.warn_once("pictures not converted to PNG (run lsc2png); decoding the originals");
    }
    crate::ipu::decode_picture(bytes).map(|p| p.2).unwrap_or_else(|| vec![0; (w * h) as usize])
}

// ---- Widescreen ----

/// Set by the frontend: render the race camera 16:9 (shown stretched).
pub static WIDESCREEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Camera projection scale (0x145210): `(camera or 0 for the current
/// one, sx in $f12, sy in $f13, update)` stores sx, sy and their
/// reciprocals at +1144..+1156 and, if `update`, rebuilds the camera's
/// matrices (0x144200).
///
/// In widescreen the race camera's setup (the call returning to
/// 0x1443c4, sx = 1.0) gets sx = 0.75: a third more view horizontally,
/// squeezed into the 4:3 frame and stretched back out on display. This
/// is the same change as PCSX2's widescreen patch for this game.
pub fn camera_scale(c: &mut Cpu) {
    const RA: usize = 31;
    let mut cam = c.r32(A0);
    if cam == 0 {
        cam = c.mem.r32(c.r32(GP).wrapping_sub(5928));
        c.ws(A0, cam);
    }
    let mut sx = c.fpr[12];
    if c.r32(RA) == 0x1443c4 && WIDESCREEN.load(std::sync::atomic::Ordering::Relaxed) {
        sx *= 0.75;
    }
    let sy = c.fpr[13];
    c.mem.w32(cam + 1144, sx.to_bits());
    c.mem.w32(cam + 1148, sy.to_bits());
    c.mem.w32(cam + 1152, crate::fpu::fdiv(1.0, sx).to_bits());
    c.mem.w32(cam + 1156, crate::fpu::fdiv(1.0, sy).to_bits());
    if c.r32(A1) != 0 {
        c.call(0x144200);
    }
}

/// The VIF1 DMA-end handler (0x12f9f0) times each frame's rendering
/// from the frame start (gp-6064) and stores it at gp-6056; past 90% of
/// a field the frame loop raises a load factor (gp-6052) that lowers
/// detail, like `frame_time` below. Here VU1 runs as slow code on the EE
/// thread, which a PS2 does in parallel, so the time means nothing:
/// report the rendering as instant.
pub fn render_done(c: &mut Cpu) {
    let gp = c.r32(GP);
    if c.mem.r32(gp.wrapping_sub(6048)) != 0 {
        c.mfc0(V0, 9);
        let now = c.r32(V0);
        c.mem.w32(gp.wrapping_sub(6056), 0);
        c.mem.w32(gp.wrapping_sub(6060), now);
        c.mem.w32(gp.wrapping_sub(6048), 0);
    }
    c.ei();
    c.ws(V0, 0);
}

/// The race's frame timer (0x1b1300, just before the vsync wait):
/// averages the EE time since the last flip over 16 frames into gp+5548,
/// in percent of a 60 Hz frame. Past 80% the game cuts detail: opponents
/// drop to their lowest model without bikes, scenery switches to simpler
/// models and back. Here that time includes the VU1 microcode, which a
/// PS2 runs alongside the EE, so it reads 150-200% in races: report 50%,
/// the value the game starts from.
pub fn frame_time(c: &mut Cpu) {
    let gp = c.r32(GP);
    c.mem.w32(gp.wrapping_add(5548), 50);
}

// ---- Movies ----

/// The game's movie API (EA .FLM via the IPU): open(path) (0x1ac580),
/// blocking play (0x1ab670) and close (0x1ab620). The IPU is not
/// emulated: play shows the movie's `.mkv` conversion instead (see
/// [`crate::movie`]), or skips it when there is none.
/// The game's disc stream control: 0 pauses streaming, 1 restarts it.
const STREAM_STATE: u32 = 0x193440;

pub fn movie_open(c: &mut Cpu) {
    let path = cstr(c, c.r32(A0));
    *crate::movie::OPENED.lock().unwrap() = Some(path);
    c.ws(V0, 0);
}

pub fn movie_play(c: &mut Cpu) {
    let opened = crate::movie::OPENED.lock().unwrap().clone();
    // Converted movies live next to the pictures (game/movies).
    let dir = c.iop.images.parent().unwrap_or(std::path::Path::new(".")).join("movies");
    match (opened.as_deref().and_then(|p| crate::movie::find(&dir, p)), c.mem.io.gs.clone()) {
        (Some(file), Some(gs)) => {
            // The real open pauses the game's disc streaming (0x193440(0))
            // for as long as the movie plays and restarts it after
            // (0x193440(1)). Without that, reads that finish while the game
            // is blocked are lost and the stream queue stalls for good
            // (the Circuit intro froze on the loading screen after it).
            c.ws(A0, 0);
            c.call(STREAM_STATE);
            if let Err(e) = crate::movie::play(&file, &gs) {
                eprintln!("[movie] {e}");
            }
            // The game was blocked meanwhile, as on the PS2.
            c.mem.io.resync();
            c.ws(A0, 1);
            c.call(STREAM_STATE);
        }
        (None, _) => eprintln!("[movie] no conversion of {} in {} (see flm2mkv); skipped", opened.as_deref().unwrap_or("?"), dir.display()),
        _ => {}
    }
    c.ws(V0, 0);
}

pub fn movie_close(c: &mut Cpu) {
    *crate::movie::OPENED.lock().unwrap() = None;
    c.ws(V0, 0);
}
