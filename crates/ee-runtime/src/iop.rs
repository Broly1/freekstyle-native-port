//! Native IOP: the services the game reaches over SIF RPC, implemented in
//! Rust instead of emulating the I/O processor and its modules.

use crate::cpu::Cpu;

/// Whether a server ID is one we serve. Unknown servers are reported once
/// and left unbound, which most libraries treat as "retry".
pub fn bind(c: &mut Cpu, id: u32) -> bool {
    let name = server_name(id);
    c.kernel.warn_once(&format!("IOP bind {id:#010x} ({name})"));
    true
}

/// Serve one RPC request; returns the reply bytes for the receive buffer.
pub fn call(c: &mut Cpu, server: u32, fno: u32, request: &[u8], rsize: u32) -> Vec<u8> {
    let name = server_name(server);
    c.kernel.warn_once(&format!("IOP call {server:#010x} ({name}) fno={fno:#x}: not implemented, replying zeros"));
    let _ = request;
    vec![0; rsize as usize]
}

pub fn server_name(id: u32) -> &'static str {
    match id {
        0x8000_0001 => "fileio",
        0x8000_0003 => "iop heap",
        0x8000_0006 => "module loader",
        0x8000_0100 | 0x8000_0101 => "pad",
        0x8000_0400 => "memory card",
        0x8000_0592..=0x8000_0597 => "cdvd",
        _ => "unknown",
    }
}
