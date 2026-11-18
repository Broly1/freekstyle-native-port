//! Runtime for statically recompiled Emotion Engine code.
//!
//! Generated code works on a [`Cpu`]; everything a recompiled function needs
//! is re-exported from [`prelude`].

pub mod cpu;
pub mod disc;
pub mod dma;
pub mod fpu;
pub mod hle;
pub mod images;
pub mod io;
pub mod iop;
pub mod ipu;
pub mod kernel;
pub mod mc;
pub mod mem;
pub mod mmi;
pub mod movie;
pub mod pad;
pub mod text;
pub mod vu;
pub mod vu1;
pub mod vumicro;
#[cfg(target_arch = "x86_64")]
pub mod vux;

pub use cpu::{Cpu, Func};

pub mod prelude {
    pub use crate::cpu::Cpu;
    pub use crate::fpu::{cvt_w, fdiv, fmax, fmin, fsqrt, ps2f};
    pub use crate::mmi;
    pub use crate::vu;
}

/// Load an ELF's segments into guest RAM. Returns the entry point.
pub fn load_elf(c: &mut Cpu, bytes: &[u8]) -> Result<u32, String> {
    // Reads past the end are errors, not panics: the file may be damaged.
    let u16_at = |o: usize| bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or("ELF cut short");
    let u32_at = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or("ELF cut short");
    if bytes.len() < 52 || &bytes[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    let (phoff, phentsize, phnum) = (u32_at(0x1c)? as usize, u16_at(0x2a)?, u16_at(0x2c)?);
    for n in 0..phnum {
        let ph = phoff + n * phentsize;
        if u32_at(ph)? != 1 {
            continue;
        }
        let (off, vaddr, filesz) = (u32_at(ph + 4)? as usize, u32_at(ph + 8)?, u32_at(ph + 16)? as usize);
        let data = off.checked_add(filesz).and_then(|end| bytes.get(off..end)).ok_or("segment past end of file")?;
        c.mem.write_bytes(vaddr, data);
    }
    Ok(u32_at(0x18)?)
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

/// FREEK_QUIET is set (and not "0"): suppress routine startup and stub noise
/// so targeted logs like FREEK_PAD_LOG are easy to read.
pub fn quiet() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FREEK_QUIET").is_some_and(|v| v != "0"))
}

/// FREEK_PAD_LOG is set (and not "0"): log controller states and rumble.
pub fn pad_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FREEK_PAD_LOG").is_some_and(|v| v != "0"))
}
