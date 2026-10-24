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
        Chunk::Data(src, qwc) => {
            for q in 0..qwc {
                let v = m.r128(src + q * 16);
                words.extend((0..4).map(|n| (v >> (32 * n)) as u32));
            }
        }
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
    unpack_bits: u64,
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
    /// Position within the current qword, for DIRECT alignment.
    word_pos: usize,
}

impl Vif {
    fn run(&mut self, m: &mut Memory, unit: usize, words: &[u32]) {
        let mut i = 0;
        while i < words.len() {
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
        let Memory { vu1, vu1_code, vu1_data, io, vu_cache, vu_code_gen, vu_programs, .. } = m;
        let start = if addr == u32::MAX { vu1.tpc } else { addr / 8 };
        let mut kick = |p: Vec<u128>| io.send_gif(p);
        vu_cache[1].sync(vu_code_gen[1], vu1_code.len() / 8);
        let env = crate::vumicro::Env { code: vu1_code, data: vu1_data, top: self.tops, itop: self.itop, kick: Some(&mut kick) };
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

    fn flush_direct(&mut self, m: &mut Memory) {
        if !self.direct.is_empty() {
            m.io.send_gif(std::mem::take(&mut self.direct));
        }
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

    fn unpack_word(&mut self, m: &mut Memory, unit: usize, w: u32) {
        self.unpack_bits |= (w as u64) << self.unpack_nbits;
        self.unpack_nbits += 32;
        let eb = self.element_bits();
        while self.unpack_nbits >= eb && self.unpack_done < self.unpack_target {
            let e = self.unpack_bits & ((1u64 << eb) - 1);
            self.unpack_bits = if eb == 64 { 0 } else { self.unpack_bits >> eb };
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
    fn write_element(&mut self, m: &mut Memory, unit: usize, e: Option<u64>) {
        let (vn, vl) = self.unpack_format();
        let usn = self.code & 0x4000 != 0;
        let masked = self.cmd & 0x10 != 0;
        let mut v = [0u32; 4];
        if let Some(e) = e {
            let bits = 32 >> vl;
            let comp = |n: u32| -> u32 {
                let raw = ((e >> (n * bits)) & ((1u64 << bits) - 1)) as u32;
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
