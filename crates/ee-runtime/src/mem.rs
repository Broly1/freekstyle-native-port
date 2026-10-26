//! EE physical memory map.
//!
//! | Range                      | What                         |
//! |----------------------------|------------------------------|
//! | 0x0000_0000..0x0200_0000   | 32 MB main RAM               |
//! | 0x1000_0000..0x1001_0000   | EE hardware registers        |
//! | 0x1100_0000..0x1101_0000   | VU0/VU1 micro and data memory|
//! | 0x1200_0000..0x1200_2000   | GS privileged registers      |
//! | 0x7000_0000..0x7000_4000   | 16 KB scratchpad             |
//!
//! RAM is also visible through the kseg0/kseg1 windows (0x8000_0000 and
//! 0xa000_0000) and the uncached user mirrors (0x2000_0000, 0x3000_0000).

use crate::dma::{self, Vif, CH_GIF, CH_VIF0, CH_VIF1};
use crate::io::{DmaStart, Io, CH_FROM_SPR, CH_TO_SPR};

pub const RAM_SIZE: usize = 32 << 20;
pub const SPR_BASE: u32 = 0x7000_0000;
pub const SPR_SIZE: usize = 16 << 10;

pub struct Memory {
    pub ram: Box<[u8]>,
    pub spr: Box<[u8]>,
    pub vu0_code: Box<[u8]>,
    pub vu0_data: Box<[u8]>,
    pub vu1_code: Box<[u8]>,
    pub vu1_data: Box<[u8]>,
    pub io: Io,
    pub vif: [Vif; 2],
    pub pad: crate::pad::Pad,
    /// VU1 registers (VU0's live in `Cpu::vu0`, shared with COP2).
    pub vu1: crate::vu::VuRegs,
    /// Bumped whenever VU0/VU1 program memory changes.
    pub vu_code_gen: [u64; 2],
    pub vu_cache: [crate::vumicro::DecodeCache; 2],
    /// Recompiled VU microprograms.
    pub vu_programs: crate::vumicro::Programs,
}

fn zeroed(n: usize) -> Box<[u8]> {
    vec![0; n].into_boxed_slice()
}

macro_rules! access {
    ($read:ident, $write:ident, $t:ty) => {
        #[inline(always)]
        pub fn $read(&mut self, addr: u32) -> $t {
            const N: usize = std::mem::size_of::<$t>();
            if (addr as usize) <= RAM_SIZE - N {
                let a = addr as usize;
                return <$t>::from_le_bytes(self.ram[a..a + N].try_into().unwrap());
            }
            self.read_slow(addr, N) as $t
        }

        #[inline(always)]
        pub fn $write(&mut self, addr: u32, v: $t) {
            const N: usize = std::mem::size_of::<$t>();
            if (addr as usize) <= RAM_SIZE - N {
                let a = addr as usize;
                self.ram[a..a + N].copy_from_slice(&v.to_le_bytes());
                return;
            }
            self.write_slow(addr, N, v as u128)
        }
    };
}

impl Memory {
    pub fn new() -> Memory {
        Memory {
            ram: zeroed(RAM_SIZE),
            spr: zeroed(SPR_SIZE),
            vu0_code: zeroed(4 << 10),
            vu0_data: zeroed(4 << 10),
            vu1_code: zeroed(16 << 10),
            vu1_data: zeroed(16 << 10),
            io: Io::new(),
            vif: Default::default(),
            pad: Default::default(),
            vu1: crate::vu::VuRegs::new(),
            vu_code_gen: [1, 1],
            vu_cache: Default::default(),
            vu_programs: crate::vumicro::Programs(&[]),
        }
    }

    access!(r8, w8, u8);
    access!(r16, w16, u16);
    access!(r32, w32, u32);
    access!(r64, w64, u64);
    access!(r128, w128, u128);

    /// Resolve an address to backing storage, or None for I/O and holes.
    pub(crate) fn backing(&mut self, addr: u32, n: usize) -> Option<&mut [u8]> {
        if (SPR_BASE..SPR_BASE + SPR_SIZE as u32).contains(&addr) {
            let a = (addr - SPR_BASE) as usize;
            return self.spr.get_mut(a..a + n);
        }
        let phys = match addr {
            0x2000_0000..=0x3fff_ffff => addr & 0x01ff_ffff,
            0x8000_0000..=0xbfff_ffff => addr & 0x1fff_ffff,
            _ => addr,
        };
        let (buf, base): (&mut [u8], u32) = match phys {
            0..=0x01ff_ffff => (&mut self.ram, 0),
            0x1100_0000..=0x1100_0fff => (&mut self.vu0_code, 0x1100_0000),
            0x1100_4000..=0x1100_4fff => (&mut self.vu0_data, 0x1100_4000),
            0x1100_8000..=0x1100_bfff => (&mut self.vu1_code, 0x1100_8000),
            0x1100_c000..=0x1100_ffff => (&mut self.vu1_data, 0x1100_c000),
            _ => return None,
        };
        let a = (phys - base) as usize;
        buf.get_mut(a..a + n)
    }

    #[cold]
    fn read_slow(&mut self, addr: u32, n: usize) -> u128 {
        if let Some(b) = self.backing(addr, n) {
            let mut v = [0u8; 16];
            v[..n].copy_from_slice(b);
            return u128::from_le_bytes(v);
        }
        let v = self.io.read(addr & 0x1fff_ffff, n);
        self.service_vblank();
        v
    }

    /// Work that happens once per field: controller frames.
    pub fn service_vblank(&mut self) {
        if !std::mem::take(&mut self.io.new_vblank) {
            return;
        }
        let mut pad = std::mem::take(&mut self.pad);
        let n = self.io.vblanks;
        pad.vblank(n, &mut |addr, bytes| self.write_bytes(addr, bytes));
        self.pad = pad;
    }

    #[cold]
    fn write_slow(&mut self, addr: u32, n: usize, v: u128) {
        // Writes into VU program memory invalidate decoded programs.
        match addr & 0x1fff_ffff {
            0x1100_0000..=0x1100_0fff => self.vu_code_gen[0] += 1,
            0x1100_8000..=0x1100_bfff => self.vu_code_gen[1] += 1,
            _ => {}
        }
        if let Some(b) = self.backing(addr, n) {
            b.copy_from_slice(&v.to_le_bytes()[..n]);
            return;
        }
        if let Some(d) = self.io.write(addr & 0x1fff_ffff, n, v) {
            self.run_dma(&d);
        }
    }

    /// Perform a DMA transfer. Scratchpad transfers are plain copies, GIF
    /// and VIF go through `dma`; IPU and SIF are not emulated yet.
    fn run_dma(&mut self, d: &DmaStart) {
        // MADR bit 31 selects the scratchpad instead of RAM.
        let mem_addr = |a: u32| if a & 0x8000_0000 != 0 { SPR_BASE | (a & 0x3ff0) } else { a & 0x01ff_fff0 };
        match d.channel {
            CH_FROM_SPR | CH_TO_SPR if d.mode() == 0 => {
                let ram = mem_addr(d.madr);
                let spr = SPR_BASE | (d.sadr & 0x3ff0);
                for q in 0..d.qwc {
                    // The scratchpad address wraps within its 16 KB.
                    let s = SPR_BASE | ((spr + q * 16) & 0x3ff0);
                    let (src, dst) = if d.channel == CH_FROM_SPR { (s, ram + q * 16) } else { (ram + q * 16, s) };
                    let v = self.r128(src);
                    self.w128(dst, v);
                }
            }
            CH_GIF => dma::gif(self, d),
            // VIF1 towards memory: a GS local-to-host transfer (BUSDIR).
            CH_VIF1 if d.chcr & 1 == 0 => {
                let ram = mem_addr(d.madr);
                for q in 0..d.qwc {
                    self.w128(ram + q * 16, 0);
                }
                self.io.warn_once("GS readback (StoreImage): returning zeros");
            }
            CH_VIF0 | CH_VIF1 => dma::vif(self, d),
            _ => self.io.dma_note(d, "not emulated yet; completed with no effect"),
        }
        self.io.dma_finish(d);
    }

    /// Copy bytes into guest memory (used by the ELF loader and HLE code).
    pub fn write_bytes(&mut self, addr: u32, bytes: &[u8]) {
        let phys = (addr & 0x1fff_ffff) as usize;
        if !(SPR_BASE..SPR_BASE + SPR_SIZE as u32).contains(&addr) && phys + bytes.len() <= RAM_SIZE {
            self.ram[phys..phys + bytes.len()].copy_from_slice(bytes);
            return;
        }
        for (n, b) in bytes.iter().enumerate() {
            self.w8(addr + n as u32, *b);
        }
    }

    /// Read a NUL-terminated string from guest memory.
    pub fn read_cstr(&mut self, mut addr: u32) -> String {
        let mut out = Vec::new();
        while out.len() < 4096 {
            let b = self.r8(addr);
            if b == 0 {
                break;
            }
            out.push(b);
            addr += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}
