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
    if end_func != 0 {
        let saved = c.gpr;
        c.w(A0, end_param as i32 as i64 as u64);
        c.call(end_func);
        c.gpr = saved;
    }
    c.ws(V0, 0);
}
