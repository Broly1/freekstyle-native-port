//! Runtime for statically recompiled Emotion Engine code.
//!
//! Generated code works on a [`Cpu`]; everything a recompiled function needs
//! is re-exported from [`prelude`].

pub mod cpu;
pub mod disc;
pub mod dma;
pub mod fpu;
pub mod hle;
pub mod io;
pub mod iop;
pub mod kernel;
pub mod mc;
pub mod mem;
pub mod mmi;
pub mod pad;
pub mod text;
pub mod vu;
pub mod vumicro;

pub use cpu::{Cpu, Func};

pub mod prelude {
    pub use crate::cpu::Cpu;
    pub use crate::fpu::{cvt_w, fdiv, fmax, fmin, fsqrt, ps2f};
    pub use crate::mmi;
    pub use crate::vu;
}

/// Load an ELF's segments into guest RAM. Returns the entry point.
pub fn load_elf(c: &mut Cpu, bytes: &[u8]) -> Result<u32, String> {
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    if bytes.len() < 52 || &bytes[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    let (phoff, phentsize, phnum) = (u32_at(0x1c) as usize, u16_at(0x2a), u16_at(0x2c));
    for n in 0..phnum {
        let ph = phoff + n * phentsize;
        if u32_at(ph) != 1 {
            continue;
        }
        let (off, vaddr, filesz) = (u32_at(ph + 4) as usize, u32_at(ph + 8), u32_at(ph + 16) as usize);
        let data = bytes.get(off..off + filesz).ok_or("segment past end of file")?;
        c.mem.write_bytes(vaddr, data);
    }
    Ok(u32_at(0x18))
}

/// FNV-1a, used to check that generated code matches the executable.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// FREEK_HEARTBEAT is set: keep the timing statistics it prints. Reading
/// the clock is costly enough to matter in hot paths otherwise.
pub fn stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FREEK_HEARTBEAT").is_some())
}
