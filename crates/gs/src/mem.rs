//! GS local memory: 4 MB organised in 8 KB pages of 32 256-byte blocks.
//!
//! Every pixel format stores pixels in its own swizzled order within a page
//! (block arrangement) and within a block (column arrangement). The layouts
//! below are closed-form equivalents of the hardware tables; tests check
//! them entry by entry against spot values from the tables.

pub const VRAM_SIZE: usize = 4 << 20;
pub const PAGE_SIZE: usize = 8 << 10;
pub const PAGES: usize = VRAM_SIZE / PAGE_SIZE;

/// Pixel storage formats (PSM field values).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Psm {
    Ct32,
    Ct24,
    Ct16,
    Ct16s,
    T8,
    T4,
    T8h,
    T4hl,
    T4hh,
    Z32,
    Z24,
    Z16,
    Z16s,
}

impl Psm {
    pub fn from_bits(v: u64) -> Psm {
        match v & 0x3f {
            0x00 => Psm::Ct32,
            0x01 => Psm::Ct24,
            0x02 => Psm::Ct16,
            0x0a => Psm::Ct16s,
            0x13 => Psm::T8,
            0x14 => Psm::T4,
            0x1b => Psm::T8h,
            0x24 => Psm::T4hl,
            0x2c => Psm::T4hh,
            0x30 => Psm::Z32,
            0x31 => Psm::Z24,
            0x32 => Psm::Z16,
            0x3a => Psm::Z16s,
            _ => Psm::Ct32,
        }
    }

    /// Bits per pixel as stored in memory.
    pub fn bpp(self) -> u32 {
        match self {
            Psm::Ct32 | Psm::Ct24 | Psm::T8h | Psm::T4hl | Psm::T4hh | Psm::Z32 | Psm::Z24 => 32,
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => 16,
            Psm::T8 => 8,
            Psm::T4 => 4,
        }
    }

    /// Bits per pixel in a host transfer stream.
    pub fn transfer_bits(self) -> u32 {
        match self {
            Psm::Ct24 | Psm::Z24 => 24,
            Psm::T8h => 8,
            Psm::T4hl | Psm::T4hh => 4,
            p => p.bpp(),
        }
    }

    /// Page dimensions in pixels.
    pub fn page_size(self) -> (u32, u32) {
        match self.bpp() {
            32 => (64, 32),
            16 => (64, 64),
            8 => (128, 64),
            _ => (128, 128),
        }
    }

    pub fn is_indexed(self) -> bool {
        matches!(self, Psm::T8 | Psm::T4 | Psm::T8h | Psm::T4hl | Psm::T4hh)
    }
}

fn block32(bx: u32, by: u32) -> u32 {
    (bx & 1) | (by & 1) << 1 | (bx & 2) << 1 | (by & 2) << 2 | (bx & 4) << 2
}

fn block16(bx: u32, by: u32) -> u32 {
    (by & 1) | (bx & 1) << 1 | (by & 2) << 1 | (bx & 2) << 2 | (by & 4) << 2
}

fn block16s(bx: u32, by: u32) -> u32 {
    (by & 1) | (bx & 1) << 1 | ((by >> 2) & 1) << 2 | ((by >> 1) & 1) << 3 | ((bx >> 1) & 1) << 4
}

fn col32(x: u32, y: u32) -> u32 {
    (y >> 1) * 16 + (y & 1) * 2 + (x >> 1) * 4 + (x & 1)
}

fn col16(x: u32, y: u32) -> u32 {
    (y >> 1) * 32 + (y & 1) * 4 + ((x >> 1) & 3) * 8 + (x & 1) * 2 + (x >> 3)
}

/// 8-bit and 4-bit columns alternate a 4-pixel swap between row pairs.
fn col8(x: u32, y: u32) -> u32 {
    let (c, r) = (y >> 2, y & 3);
    let x = x ^ (((c ^ (r >> 1)) & 1) << 2);
    c * 64 + (x & 1) * 4 + ((x >> 1) & 3) * 16 + (x >> 3) * 2 + (r & 1) * 8 + (r >> 1)
}

fn col4(x: u32, y: u32) -> u32 {
    let (c, r) = (y >> 2, y & 3);
    let x = x ^ (((c ^ (r >> 1)) & 1) << 2);
    c * 128 + (x & 1) * 8 + ((x >> 1) & 3) * 32 + ((x >> 3) & 3) * 2 + (r & 1) * 16 + (r >> 1)
}

/// Z formats use the colour layouts with the block number XORed by 0x18.
const ZXOR: u32 = 0x18;

/// Word index of a 32-bit pixel. `bp` is in blocks, `bw` in 64-pixel units.
pub fn addr32(bp: u32, bw: u32, x: u32, y: u32, z: bool) -> u32 {
    let page = (y >> 5) * bw.max(1) + (x >> 6);
    let block = block32((x >> 3) & 7, (y >> 3) & 3) ^ if z { ZXOR } else { 0 };
    ((bp + page * 32 + block) * 64 + col32(x & 7, y & 7)) & 0xf_ffff
}

/// Halfword index of a 16-bit pixel.
pub fn addr16(bp: u32, bw: u32, x: u32, y: u32, s: bool, z: bool) -> u32 {
    let page = (y >> 6) * bw.max(1) + (x >> 6);
    let (bx, by) = ((x >> 4) & 3, (y >> 3) & 7);
    let block = if s { block16s(bx, by) } else { block16(bx, by) } ^ if z { ZXOR } else { 0 };
    ((bp + page * 32 + block) * 128 + col16(x & 15, y & 7)) & 0x1f_ffff
}

/// Byte index of an 8-bit pixel. Pages are 128 pixels wide.
pub fn addr8(bp: u32, bw: u32, x: u32, y: u32) -> u32 {
    let page = (y >> 6) * (bw >> 1).max(1) + (x >> 7);
    let block = block32((x >> 4) & 7, (y >> 4) & 3);
    ((bp + page * 32 + block) * 256 + col8(x & 15, y & 15)) & 0x3f_ffff
}

/// Nibble index of a 4-bit pixel.
pub fn addr4(bp: u32, bw: u32, x: u32, y: u32) -> u32 {
    let page = (y >> 7) * (bw >> 1).max(1) + (x >> 7);
    let block = block16((x >> 5) & 3, (y >> 4) & 7);
    ((bp + page * 32 + block) * 512 + col4(x & 31, y & 15)) & 0x7f_ffff
}

pub struct Vram {
    data: Box<[u8]>,
    /// Write generation per page, for cache invalidation.
    pub page_gen: Box<[u32]>,
}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

impl Vram {
    pub fn new() -> Vram {
        Vram { data: vec![0; VRAM_SIZE].into_boxed_slice(), page_gen: vec![0; PAGES].into_boxed_slice() }
    }

    fn r32(&self, word: u32) -> u32 {
        let a = word as usize * 4;
        u32::from_le_bytes(self.data[a..a + 4].try_into().unwrap())
    }

    fn w32(&mut self, word: u32, v: u32) {
        let a = word as usize * 4;
        self.data[a..a + 4].copy_from_slice(&v.to_le_bytes());
        self.page_gen[a / PAGE_SIZE] = self.page_gen[a / PAGE_SIZE].wrapping_add(1);
    }

    fn r16(&self, half: u32) -> u32 {
        let a = half as usize * 2;
        u16::from_le_bytes([self.data[a], self.data[a + 1]]) as u32
    }

    fn w16(&mut self, half: u32, v: u32) {
        let a = half as usize * 2;
        self.data[a..a + 2].copy_from_slice(&(v as u16).to_le_bytes());
        self.page_gen[a / PAGE_SIZE] = self.page_gen[a / PAGE_SIZE].wrapping_add(1);
    }

    /// Read one pixel's stored value (colour bits, index or depth).
    pub fn read(&self, psm: Psm, bp: u32, bw: u32, x: u32, y: u32) -> u32 {
        match psm {
            Psm::Ct32 | Psm::Z32 => self.r32(addr32(bp, bw, x, y, psm == Psm::Z32)),
            Psm::Ct24 | Psm::Z24 => self.r32(addr32(bp, bw, x, y, psm == Psm::Z24)) & 0xff_ffff,
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => {
                let s = matches!(psm, Psm::Ct16s | Psm::Z16s);
                let z = matches!(psm, Psm::Z16 | Psm::Z16s);
                self.r16(addr16(bp, bw, x, y, s, z))
            }
            Psm::T8 => self.data[addr8(bp, bw, x, y) as usize] as u32,
            Psm::T4 => {
                let n = addr4(bp, bw, x, y);
                (self.data[n as usize >> 1] as u32 >> ((n & 1) * 4)) & 15
            }
            Psm::T8h => self.r32(addr32(bp, bw, x, y, false)) >> 24,
            Psm::T4hl => (self.r32(addr32(bp, bw, x, y, false)) >> 24) & 15,
            Psm::T4hh => self.r32(addr32(bp, bw, x, y, false)) >> 28,
        }
    }

    /// Write one pixel, preserving bits the format does not own.
    pub fn write(&mut self, psm: Psm, bp: u32, bw: u32, x: u32, y: u32, v: u32) {
        match psm {
            Psm::Ct32 | Psm::Z32 => self.w32(addr32(bp, bw, x, y, psm == Psm::Z32), v),
            Psm::Ct24 | Psm::Z24 => {
                let a = addr32(bp, bw, x, y, psm == Psm::Z24);
                let old = self.r32(a);
                self.w32(a, (old & 0xff00_0000) | (v & 0xff_ffff));
            }
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => {
                let s = matches!(psm, Psm::Ct16s | Psm::Z16s);
                let z = matches!(psm, Psm::Z16 | Psm::Z16s);
                self.w16(addr16(bp, bw, x, y, s, z), v);
            }
            Psm::T8 => {
                let a = addr8(bp, bw, x, y) as usize;
                self.data[a] = v as u8;
                self.page_gen[a / PAGE_SIZE] = self.page_gen[a / PAGE_SIZE].wrapping_add(1);
            }
            Psm::T4 => {
                let n = addr4(bp, bw, x, y);
                let a = n as usize >> 1;
                let shift = (n & 1) * 4;
                self.data[a] = (self.data[a] & !(15 << shift)) | (((v & 15) as u8) << shift);
                self.page_gen[a / PAGE_SIZE] = self.page_gen[a / PAGE_SIZE].wrapping_add(1);
            }
            Psm::T8h | Psm::T4hl | Psm::T4hh => {
                let a = addr32(bp, bw, x, y, false);
                let old = self.r32(a);
                let (shift, mask) = match psm {
                    Psm::T8h => (24, 0xff),
                    Psm::T4hl => (24, 0xf),
                    _ => (28, 0xf),
                };
                self.w32(a, (old & !(mask << shift)) | ((v & mask) << shift));
            }
        }
    }

    /// Pages a `w`x`h` rectangle at (0,0) of a buffer touches.
    pub fn pages_of(psm: Psm, bp: u32, bw: u32, w: u32, h: u32) -> impl Iterator<Item = usize> {
        let (pw, ph) = psm.page_size();
        let row = match psm.bpp() {
            8 | 4 => (bw >> 1).max(1),
            _ => bw.max(1),
        };
        let first = bp / 32;
        let (cols, rows) = (w.div_ceil(pw), h.div_ceil(ph));
        // Blocks need not be page-aligned; include the page the tail spills into.
        let spill = u32::from(bp % 32 != 0);
        (0..rows).flat_map(move |r| (0..cols + spill).map(move |c| ((first + r * row + c) as usize) % PAGES))
    }

    pub fn generation(&self, pages: impl Iterator<Item = usize>) -> u64 {
        pages.fold(0u64, |h, p| h.wrapping_mul(0x100_0000_01b3) ^ (p as u64) << 32 ^ self.page_gen[p] as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_permutations() {
        for (w, h, f) in [
            (8u32, 8u32, col32 as fn(u32, u32) -> u32),
            (16, 8, col16),
            (16, 16, col8),
            (32, 16, col4),
        ] {
            let mut seen = vec![false; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let i = f(x, y) as usize;
                    assert!(!seen[i], "duplicate {i}");
                    seen[i] = true;
                }
            }
        }
    }

    #[test]
    fn spot_values_match_hardware_tables() {
        // columnTable8 rows 2 and 4 and columnTable4 rows 2 and 4.
        let row = |f: fn(u32, u32) -> u32, y: u32, n: u32| (0..n).map(|x| f(x, y)).collect::<Vec<_>>();
        assert_eq!(row(col8, 2, 8), [33, 37, 49, 53, 1, 5, 17, 21]);
        assert_eq!(row(col8, 4, 8), [96, 100, 112, 116, 64, 68, 80, 84]);
        assert_eq!(row(col4, 2, 8), [65, 73, 97, 105, 1, 9, 33, 41]);
        assert_eq!(row(col4, 4, 8), [192, 200, 224, 232, 128, 136, 160, 168]);
        assert_eq!(row(col16, 1, 16), [4, 6, 12, 14, 20, 22, 28, 30, 5, 7, 13, 15, 21, 23, 29, 31]);
        assert_eq!((0..4).map(|bx| block16s(bx, 2)).collect::<Vec<_>>(), [8, 10, 24, 26]);
    }

    #[test]
    fn write_read_roundtrip_all_formats() {
        let mut v = Vram::new();
        for psm in [Psm::Ct32, Psm::Ct24, Psm::Ct16, Psm::Ct16s, Psm::T8, Psm::T4, Psm::T8h, Psm::T4hl, Psm::T4hh] {
            let mask = match psm.transfer_bits() {
                32 => u32::MAX,
                24 => 0xff_ffff,
                16 => 0xffff,
                8 => 0xff,
                _ => 0xf,
            };
            for y in 0..70 {
                for x in 0..130 {
                    v.write(psm, 64, 4, x, y, (x * 7919 + y * 104729) & mask);
                }
            }
            for y in 0..70 {
                for x in 0..130 {
                    assert_eq!(v.read(psm, 64, 4, x, y), (x * 7919 + y * 104729) & mask, "{psm:?} {x},{y}");
                }
            }
        }
    }
}
