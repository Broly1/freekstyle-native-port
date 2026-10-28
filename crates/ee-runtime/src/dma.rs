//! DMA channels that feed the GS and the vector units: source-chain tag
//! walking, and the VIF command stream (UNPACK, MPG, DIRECT, ...).
//!
//! Transfers run to completion when started. GIF data is forwarded to the
//! GS thread; VU1 microprograms started by MSCAL run immediately.

use crate::io::DmaStart;
use crate::mem::Memory;

pub const CH_VIF0: usize = 0;
pub const CH_VIF1: usize = 1;
pub const CH_GIF: usize = 2;

/// XGKICKed packets and VU1 program runs, for FREEK_HEARTBEAT.
pub static PATH1_PACKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static VU1_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Guard against runaway tag chains in broken lists.
const MAX_TAGS: usize = 1 << 16;

/// Pieces of a transfer, in order.
enum Chunk {
    /// The upper 64 bits of a tag, sent when CHCR.TTE is set (VIF codes).
    TagWords([u32; 2]),
    /// `qwc` qwords starting at an address.
    Data(u32, u32),
}

/// Walk a transfer (normal or source chain) and hand over its pieces.
fn gather(m: &mut Memory, d: &DmaStart, out: &mut dyn FnMut(&mut Memory, Chunk)) {
    let addr = |a: u32| if a & 0x8000_0000 != 0 { 0x7000_0000 | (a & 0x3ff0) } else { a & 0x01ff_fff0 };
    if d.mode() == 0 {
        out(m, Chunk::Data(addr(d.madr), d.qwc));
        return;
    }
    let tte = d.chcr & 0x40 != 0;
    let mut tadr = d.tadr;
    let mut stack: Vec<u32> = Vec::new();
    for _ in 0..MAX_TAGS {
        let tag = m.r128(addr(tadr));
        let qwc = (tag & 0xffff) as u32;
        let id = ((tag >> 28) & 7) as u32;
        let ta = ((tag >> 32) & 0x7fff_fff0) as u32 | ((tag >> 32) as u32 & 0x8000_0000);
        if tte {
            out(m, Chunk::TagWords([(tag >> 64) as u32, (tag >> 96) as u32]));
        }
        let after = tadr + 16;
        let (src, next, end) = match id {
            0 => (ta, 0, true),                          // refe
            1 => (after, after + qwc * 16, false),      // cnt
            2 => (after, ta, false),                     // next
            3 | 4 => (ta, after, false),                 // ref, refs
            5 => {
                stack.push(after + qwc * 16);            // call
                (after, ta, false)
            }
            6 => match stack.pop() {
                Some(r) => (after, r, false),            // ret
                None => (after, 0, true),
            },
            _ => (after, 0, true),                       // end
        };
        out(m, Chunk::Data(addr(src), qwc));
        let irq = tag >> 31 & 1 != 0 && d.chcr & 0x80 != 0;
        if end || irq {
            return;
        }
        tadr = next;
    }
    eprintln!("[dma] tag chain too long at {tadr:#x}; aborting");
}

/// Run a GIF (PATH3) transfer.
pub fn gif(m: &mut Memory, d: &DmaStart) {
    let mut packet = Vec::new();
    gather(m, d, &mut |m, c| {
        if let Chunk::Data(src, qwc) = c {
            packet.extend((0..qwc).map(|q| m.r128(src + q * 16)));
        }
    });
    m.io.send_gif(packet);
}

/// Run a VIF0/VIF1 transfer through that unit's command parser.
pub fn vif(m: &mut Memory, d: &DmaStart) {
    let unit = d.channel;
    let mut words = Vec::new();
    gather(m, d, &mut |m, c| match c {
        Chunk::TagWords(w) => words.extend_from_slice(&w),
        Chunk::Data(src, qwc) => match m.backing(src, qwc as usize * 16) {
            Some(b) => words.extend(b.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap()))),
            None => {
                for q in 0..qwc {
                    let v = m.r128(src + q * 16);
                    words.extend((0..4).map(|n| (v >> (32 * n)) as u32));
                }
            }
        },
    });
    let mut vif = std::mem::take(&mut m.vif[unit]);
    vif.run(m, unit, &words);
    m.vif[unit] = vif;
}

/// Parser state for one VIF, kept across transfers because a command's
/// data may continue in the next DMA packet.
#[derive(Default)]
pub struct Vif {
    /// Current command and the data words it still expects.
    cmd: u32,
    code: u32,
    left: usize,
    /// UNPACK progress.
    unpack_addr: u32,
    unpack_done: u32,
    unpack_target: u32,
    unpack_cycle: u32,
    /// Pending input bits; an element is up to 128 bits (V4-32).
    unpack_bits: u128,
    unpack_nbits: u32,
    /// Words received for multi-word commands (STROW etc.).
    buf: Vec<u32>,
    pub cl: u32,
    pub wl: u32,
    pub mode: u32,
    pub mask: u32,
    pub row: [u32; 4],
    pub col: [u32; 4],
    pub base: u32,
    pub ofst: u32,
    pub tops: u32,
    pub itop: u32,
    pub mpg_addr: u32,
    /// DIRECT data collected for the GIF.
    direct: Vec<u128>,
    /// GIF output of this transfer (XGKICK and DIRECT, in order), sent to
    /// the GS in one piece when the transfer ends.
    gif: Vec<u128>,
    /// Position within the current qword, for DIRECT alignment.
    word_pos: usize,
}

impl Vif {
    fn run(&mut self, m: &mut Memory, unit: usize, words: &[u32]) {
        let mut i = 0;
        while i < words.len() {
            if self.left > 0 && self.cmd >= 0x60 {
                // UNPACK data: as many words as it still takes at once.
                let k = self.left.min(words.len() - i);
                self.unpack_words(m, unit, &words[i..i + k]);
                i += k;
                self.word_pos = (self.word_pos + k) % 4;
                self.left -= k;
                continue;
            }
            if self.left == 0 {
                self.code = words[i];
                i += 1;
                self.word_pos = (self.word_pos + 1) % 4;
                self.begin(m, unit);
                continue;
            }
            let w = words[i];
            i += 1;
            self.word_pos = (self.word_pos + 1) % 4;
            self.left -= 1;
            self.data(m, unit, w);
            if self.left == 0 {
                self.end(m, unit);
            }
        }
        self.flush_direct(m);
        if !self.gif.is_empty() {
            m.io.send_gif(std::mem::take(&mut self.gif));
        }
    }

    fn begin(&mut self, m: &mut Memory, unit: usize) {
        let code = self.code;
        let imm = code & 0xffff;
        let num = (code >> 16) & 0xff;
        self.cmd = (code >> 24) & 0x7f;
        self.buf.clear();
        match self.cmd {
            0x00 | 0x07 | 0x10 | 0x11 | 0x13 => {} // NOP, MARK, FLUSHE/FLUSH/FLUSHA
            0x01 => (self.cl, self.wl) = (imm & 0xff, imm >> 8),
            0x02 => self.ofst = imm & 0x3ff,
            0x03 => self.base = imm & 0x3ff,
            0x04 => self.itop = imm & 0x3ff,
            0x05 => self.mode = imm & 3,
            0x06 => {} // MSKPATH3
            0x14 | 0x15 => self.mscal(m, unit, imm * 8),
            0x17 => self.mscal(m, unit, u32::MAX),
            0x20 => self.left = 1,
            0x30 | 0x31 => self.left = 4,
            0x4a => {
                self.mpg_addr = imm * 8;
                self.left = if num == 0 { 512 } else { num as usize * 2 };
            }
            0x50 | 0x51 => {
                // DIRECT data starts on a qword boundary.
                let q = if imm == 0 { 65536 } else { imm as usize };
                self.left = q * 4;
                self.direct.reserve(q);
                self.buf.clear();
            }
            0x60..=0x7f => self.begin_unpack(),
            _ => m.io.warn_once(&format!("VIF{unit}: unknown command {code:#010x}")),
        }
        // Double-buffer swap on MSCAL: TOPS alternates between BASE and
        // BASE+OFST (VIF1 only).
        if unit == 1 && matches!(self.cmd, 0x14 | 0x15 | 0x17) {
            self.tops = if self.tops == self.base { self.base + self.ofst } else { self.base };
        }
    }

    /// Start a microprogram (MSCAL/MSCALF at `addr`, MSCNT at u32::MAX)
    /// and run it to completion. VU1 output goes to the GS (PATH1).
    fn mscal(&mut self, m: &mut Memory, unit: usize, addr: u32) {
        if unit == 0 {
            // VU0 registers live in the CPU; VIF0-started programs are rare.
            m.io.warn_once("VIF0: MSCAL to VU0 is not supported yet");
            return;
        }
        let Memory { vu1, vu1_code, vu1_data, vu_cache, vu_code_gen, vu_programs, .. } = m;
        let start = if addr == u32::MAX { vu1.tpc } else { addr / 8 };
        VU1_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vu_cache[1].sync(vu_code_gen[1], vu1_code.len() / 8);
        let env = crate::vumicro::Env { code: vu1_code, data: vu1_data, top: self.tops, itop: self.itop, kick: Some(&mut self.gif) };
        crate::vumicro::run(vu1, env, &mut vu_cache[1], 1, vu_programs, start);
    }

    fn data(&mut self, m: &mut Memory, unit: usize, w: u32) {
        match self.cmd {
            0x20 => self.mask = w,
            0x30 | 0x31 => self.buf.push(w),
            0x4a => {
                let code = if unit == 0 { &mut m.vu0_code } else { &mut m.vu1_code };
                let a = (self.mpg_addr as usize) % code.len();
                // Games re-upload the same microcode constantly; only a real
                // change invalidates decoded and recompiled programs.
                if code[a..a + 4] != w.to_le_bytes() {
                    code[a..a + 4].copy_from_slice(&w.to_le_bytes());
                    m.vu_code_gen[unit] += 1;
                }
                self.mpg_addr += 4;
            }
            0x50 | 0x51 => {
                self.buf.push(w);
                if self.buf.len() == 4 {
                    let q = self.buf.iter().enumerate().fold(0u128, |a, (n, &w)| a | (w as u128) << (32 * n));
                    self.direct.push(q);
                    self.buf.clear();
                }
            }
            0x60..=0x7f => self.unpack_word(m, unit, w),
            _ => {}
        }
    }

    fn end(&mut self, m: &mut Memory, _unit: usize) {
        match self.cmd {
            0x30 => self.row.copy_from_slice(&self.buf),
            0x31 => self.col.copy_from_slice(&self.buf),
            0x50 | 0x51 => self.flush_direct(m),
            _ => {}
        }
    }

    fn flush_direct(&mut self, _m: &mut Memory) {
        self.gif.append(&mut self.direct);
    }

    // ---- UNPACK ----

    fn unpack_format(&self) -> (u32, u32) {
        let vl = self.cmd & 3;
        let vn = (self.cmd >> 2) & 3;
        (vn, vl)
    }

    fn element_bits(&self) -> u32 {
        let (vn, vl) = self.unpack_format();
        if vn == 3 && vl == 3 {
            16 // V4-5: one RGBA5551 halfword
        } else {
            (32 >> vl) * (vn + 1)
        }
    }

    fn begin_unpack(&mut self) {
        let imm = self.code & 0xffff;
        let num = match (self.code >> 16) & 0xff {
            0 => 256,
            n => n,
        };
        let flg = imm & 0x8000 != 0;
        self.unpack_addr = (imm & 0x3ff) + if flg { self.tops } else { 0 };
        self.unpack_done = 0;
        self.unpack_cycle = 0;
        self.unpack_bits = 0;
        self.unpack_nbits = 0;
        // Elements actually read from the stream: in fill mode (WL > CL)
        // only CL of every WL writes consume data.
        let (cl, wl) = (self.cl.max(1), self.wl.max(1));
        let reads = if wl <= cl { num } else { num / wl * cl + (num % wl).min(cl) };
        let words = (reads * self.element_bits()).div_ceil(32);
        self.left = words as usize;
        self.unpack_target = num;
    }

    /// UNPACK data words. Whole elements starting on a word boundary are
    /// decoded in bulk when no addition mode or fill applies; the rest
    /// goes through `unpack_word`.
    fn unpack_words(&mut self, m: &mut Memory, unit: usize, ws: &[u32]) {
        let (vn, vl) = self.unpack_format();
        let eb = self.element_bits() as usize;
        let masked = self.cmd & 0x10 != 0;
        let (cl, wl) = (self.cl.max(1), self.wl.max(1));
        let mut used = 0;
        if self.unpack_nbits == 0 && self.mode == 0 && wl <= cl && vl < 3 {
            let (row, col) = (self.row, self.col);
            let left = (self.unpack_target - self.unpack_done) as usize;
            let mut k = (ws.len() * 32 / eb).min(left);
            if k < left {
                // Stop on a word boundary so `unpack_word` can go on.
                while k * eb % 32 != 0 {
                    k -= 1;
                }
            }
            let bits = 32usize >> vl;
            let usn = self.code & 0x4000 != 0;
            // Per cycle row in use, lane masks for data and kept fields and
            // the constant (row or column) fields, so a write is
            // (data & d) | constant | (old & keep). STMASK has 2 bits per
            // field: 0 data, 1 row, 2 col, 3 keep (see `write_element`).
            const UNMASKED: [BulkRow; 4] = [BulkRow { data: [!0; 4], constant: [0; 4], keep: [0; 4] }; 4];
            let mut rows = UNMASKED;
            if masked {
                for (r, row_masks) in rows.iter_mut().enumerate().take(wl.min(4) as usize) {
                    row_masks.data = [0; 4];
                    for n in 0..4 {
                        match (self.mask >> (r * 8 + n * 2)) & 3 {
                            0 => row_masks.data[n] = !0,
                            1 => row_masks.constant[n] = row[n],
                            2 => row_masks.constant[n] = col[r],
                            _ => row_masks.keep[n] = !0,
                        }
                    }
                }
            }
            let data = if unit == 0 { &mut m.vu0_data } else { &mut m.vu1_data };
            let at = BulkAt { done: self.unpack_done, addr: self.unpack_addr, cl, wl };
            macro_rules! go {
                ($vn:literal, $bits:literal, $usn:literal) => {
                    bulk_unpack::<$vn, $bits, $usn>(ws, k, data, at, &rows)
                };
            }
            match (vn as usize, bits, usn) {
                (0, 32, _) => go!(0, 32, false),
                (0, 16, false) => go!(0, 16, false),
                (0, 16, true) => go!(0, 16, true),
                (0, 8, false) => go!(0, 8, false),
                (0, 8, true) => go!(0, 8, true),
                (1, 32, _) => go!(1, 32, false),
                (1, 16, false) => go!(1, 16, false),
                (1, 16, true) => go!(1, 16, true),
                (1, 8, false) => go!(1, 8, false),
                (1, 8, true) => go!(1, 8, true),
                (2, 32, _) => go!(2, 32, false),
                (2, 16, false) => go!(2, 16, false),
                (2, 16, true) => go!(2, 16, true),
                (2, 8, false) => go!(2, 8, false),
                (2, 8, true) => go!(2, 8, true),
                (3, 32, _) => go!(3, 32, false),
                (3, 16, false) => go!(3, 16, false),
                (3, 16, true) => go!(3, 16, true),
                (3, 8, false) => go!(3, 8, false),
                (3, 8, true) => go!(3, 8, true),
                _ => unreachable!("vl < 3"),
            }
            self.unpack_done += k as u32;
            self.unpack_cycle = self.unpack_done % wl;
            used = (k * eb).div_ceil(32);
        }
        for &w in &ws[used..] {
            self.unpack_word(m, unit, w);
        }
    }

    fn unpack_word(&mut self, m: &mut Memory, unit: usize, w: u32) {
        self.unpack_bits |= (w as u128) << self.unpack_nbits;
        self.unpack_nbits += 32;
        let eb = self.element_bits();
        while self.unpack_nbits >= eb && self.unpack_done < self.unpack_target {
            let e = if eb == 128 { self.unpack_bits } else { self.unpack_bits & ((1u128 << eb) - 1) };
            self.unpack_bits = if eb == 128 { 0 } else { self.unpack_bits >> eb };
            self.unpack_nbits -= eb;
            self.write_element(m, unit, Some(e));
            // Fill mode: after CL data writes, WL-CL filler writes.
            let (cl, wl) = (self.cl.max(1), self.wl.max(1));
            while wl > cl && self.unpack_cycle >= cl && self.unpack_cycle < wl && self.unpack_done < self.unpack_target {
                self.write_element(m, unit, None);
            }
        }
    }

    /// Write one unpacked qword (or a filler when `e` is None).
    fn write_element(&mut self, m: &mut Memory, unit: usize, e: Option<u128>) {
        let (vn, vl) = self.unpack_format();
        let usn = self.code & 0x4000 != 0;
        let masked = self.cmd & 0x10 != 0;
        let mut v = [0u32; 4];
        if let Some(e) = e {
            let bits = 32 >> vl;
            let comp = |n: u32| -> u32 {
                let raw = ((e >> (n * bits)) & ((1u128 << bits) - 1)) as u32;
                if usn || bits == 32 { raw } else { ((raw << (32 - bits)) as i32 >> (32 - bits)) as u32 }
            };
            if vn == 3 && vl == 3 {
                let h = e as u32;
                v = [(h & 31) << 3, ((h >> 5) & 31) << 3, ((h >> 10) & 31) << 3, ((h >> 15) & 1) << 7];
            } else {
                match vn {
                    0 => v = [comp(0); 4],
                    1 => v = [comp(0), comp(1), comp(0), comp(1)],
                    2 => v = [comp(0), comp(1), comp(2), 0],
                    _ => v = [comp(0), comp(1), comp(2), comp(3)],
                }
            }
        }
        let addr = self.unpack_addr + self.unpack_done_offset();
        let data = if unit == 0 { &mut m.vu0_data } else { &mut m.vu1_data };
        let qwords = data.len() / 16;
        let base = (addr as usize % qwords) * 16;
        for (n, x) in v.iter().enumerate() {
            // STMASK: 2 bits per field per cycle row (0 data, 1 row, 2 col, 3 keep).
            let sel = if masked { (self.mask >> ((self.unpack_cycle.min(3) * 8) + n as u32 * 2)) & 3 } else { 0 };
            let sel = if e.is_none() && sel == 0 { 1 } else { sel };
            let value = match sel {
                0 => match self.mode {
                    1 => x.wrapping_add(self.row[n]),
                    2 => {
                        self.row[n] = x.wrapping_add(self.row[n]);
                        self.row[n]
                    }
                    _ => *x,
                },
                1 => self.row[n],
                2 => self.col[self.unpack_cycle.min(3) as usize],
                _ => continue,
            };
            data[base + n * 4..base + n * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        self.unpack_done += 1;
        self.unpack_cycle += 1;
        if self.unpack_cycle == self.wl.max(1) {
            self.unpack_cycle = 0;
        }
    }

    /// Destination offset of the next write, honouring the CL/WL skip.
    fn unpack_done_offset(&self) -> u32 {
        let (cl, wl) = (self.cl.max(1), self.wl.max(1));
        if wl <= cl {
            (self.unpack_done / wl) * cl + self.unpack_done % wl
        } else {
            self.unpack_done
        }
    }
}

/// Lane masks for one STMASK cycle row (see `Vif::unpack_words`).
#[derive(Clone, Copy, Default)]
struct BulkRow {
    data: [u32; 4],
    constant: [u32; 4],
    keep: [u32; 4],
}

/// Where a bulk unpack starts: elements done so far, the UNPACK address
/// and the write cycle (CL, WL with WL <= CL).
#[derive(Clone, Copy)]
struct BulkAt {
    done: u32,
    addr: u32,
    cl: u32,
    wl: u32,
}

/// Unpack `k` elements of VN+1 components of BITS bits each from `ws`,
/// specialised per format so the decode is a few shifts per element.
fn bulk_unpack<const VN: usize, const BITS: usize, const USN: bool>(ws: &[u32], k: usize, data: &mut [u8], at: BulkAt, rows: &[BulkRow; 4]) {
    let eb = BITS * (VN + 1);
    let comp = |j: usize, c: usize| -> u32 {
        let bit = j * eb + c * BITS;
        let raw = ws[bit / 32] >> (bit % 32);
        match BITS {
            32 => raw,
            16 if USN => raw & 0xffff,
            16 => raw as i16 as i32 as u32,
            _ if USN => raw & 0xff,
            _ => raw as i8 as i32 as u32,
        }
    };
    let qwords = data.len() / 16;
    debug_assert!(qwords.is_power_of_two());
    let mut cycle = at.done % at.wl;
    let mut addr = at.addr + (at.done / at.wl) * at.cl + cycle;
    for j in 0..k {
        let v = match VN {
            0 => [comp(j, 0); 4],
            1 => [comp(j, 0), comp(j, 1), comp(j, 0), comp(j, 1)],
            2 => [comp(j, 0), comp(j, 1), comp(j, 2), 0],
            _ => [comp(j, 0), comp(j, 1), comp(j, 2), comp(j, 3)],
        };
        let r = &rows[cycle.min(3) as usize];
        let base = (addr as usize & (qwords - 1)) * 16;
        let q: &mut [u8; 16] = (&mut data[base..base + 16]).try_into().unwrap();
        for n in 0..4 {
            let old = u32::from_le_bytes(q[n * 4..n * 4 + 4].try_into().unwrap());
            let value = (v[n] & r.data[n]) | r.constant[n] | (old & r.keep[n]);
            q[n * 4..n * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        cycle += 1;
        addr += 1;
        if cycle == at.wl {
            cycle = 0;
            addr += at.cl - at.wl;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run VIF1 words through the parser and return VU1 data qword `q`.
    fn unpack(words: &[u32], q: usize) -> [u32; 4] {
        let mut m = Memory::new();
        let mut vif = Vif::default();
        vif.run(&mut m, 1, words);
        std::array::from_fn(|n| u32::from_le_bytes(m.vu1_data[q * 16 + n * 4..q * 16 + n * 4 + 4].try_into().unwrap()))
    }

    #[test]
    fn unpack_v4_32() {
        // STCYCL 1,1 ; UNPACK V4-32 num=2 addr=5
        let w = [0x0100_0101, 0x6c02_0005, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(unpack(&w, 5), [1, 2, 3, 4]);
        assert_eq!(unpack(&w, 6), [5, 6, 7, 8]);
    }

    #[test]
    fn unpack_v3_32_and_v2_32() {
        // V3-32: x, y, z per element; V2-32: x, y (z, w repeat x, y).
        let w = [0x0100_0101, 0x6802_0000, 1, 2, 3, 4, 5, 6];
        assert_eq!(&unpack(&w, 0)[..3], &[1, 2, 3]);
        assert_eq!(&unpack(&w, 1)[..3], &[4, 5, 6]);
        let w = [0x0100_0101, 0x6401_0000, 9, 10];
        assert_eq!(&unpack(&w, 0)[..2], &[9, 10]);
    }

    /// The bulk path matches the word-at-a-time path for every format,
    /// sign mode, mask and a skipping write cycle, with odd element counts.
    #[test]
    fn bulk_unpack_matches_word_path() {
        for fmt in 0..16u32 {
            for usn in [0, 0x4000] {
                for (cl, wl) in [(1, 1), (2, 2), (4, 4), (4, 2), (3, 1)] {
                    let num = 7u32;
                    let eb = if fmt == 15 { 16 } else { (32 >> (fmt & 3)) * ((fmt >> 2) + 1) };
                    let n = (num * eb).div_ceil(32);
                    for mask in [0u32, 0xe4e4_1b39] {
                        // STMASK, STROW, STCOL, STCYCL, UNPACK (masked when mask != 0).
                        let mut w = vec![0x2000_0000, mask, 0x3000_0000, 11, 12, 13, 14, 0x3100_0000, 21, 22, 23, 24];
                        let mk = if mask != 0 { 0x10 } else { 0 };
                        w.extend([wl << 8 | cl | 0x0100_0000, (0x60 | mk | fmt) << 24 | num << 16 | usn | 3]);
                        w.extend((0..n).map(|k| 0x8f3a_c561u32.wrapping_mul(k + 1).rotate_left(k)));
                        let mut bulk = Memory::new();
                        let mut vif = Vif::default();
                        vif.run(&mut bulk, 1, &w);
                        let mut single = Memory::new();
                        let mut vif = Vif::default();
                        for x in &w {
                            vif.run(&mut single, 1, std::slice::from_ref(x));
                        }
                        assert!(bulk.vu1_data.iter().any(|&b| b != 0));
                        assert!(bulk.vu1_data == single.vu1_data, "format {fmt:#x} usn {usn:#x} cl {cl} wl {wl} mask {mask:#x}");
                    }
                }
            }
        }
    }

    #[test]
    fn unpack_v4_16_sign_extends() {
        // V4-16 signed: 0xffff -> -1.
        let w = [0x0100_0101, 0x6d01_0000, 0x0002_ffff, 0x0004_0003];
        assert_eq!(unpack(&w, 0), [u32::MAX, 2, 3, 4]);
    }
}
