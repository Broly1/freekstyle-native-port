//! A model of the IPU (the EE's MPEG-2 decoder), used offline.
//!
//! Freekstyle stores its title, menu and loading backgrounds (`.LSC`,
//! Sony's "ipum" format) as MPEG-2 intra pictures decoded with IDEC: VLC
//! decoding, dequantisation, IDCT and colour conversion to RGBA32, one
//! 16x16 macroblock (64 qwords) after another. The port doesn't emulate
//! the IPU hardware: `lsc2png` uses this model to convert the pictures to
//! PNG once, and `hle::lsc_decode` loads those (falling back to
//! `decode_picture` if a PNG is missing).
//!
//! Implemented commands: BCLR, IDEC, FDEC, SETIQ, SETVQ, SETTH.

use std::collections::VecDeque;
use std::sync::OnceLock;

pub const IPU_CMD: u32 = 0x1000_2000;
pub const IPU_CTRL: u32 = 0x1000_2010;
pub const IPU_BP: u32 = 0x1000_2020;
pub const IPU_TOP: u32 = 0x1000_2030;
pub const IPU_OUT_FIFO: u32 = 0x1000_7000;
pub const IPU_IN_FIFO: u32 = 0x1000_7010;

const CTRL_ECD: u32 = 1 << 14;
const CTRL_SCD: u32 = 1 << 15;
const CTRL_RST: u32 = 1 << 30;

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
const ALTERNATE: [usize; 64] = [
    0, 8, 16, 24, 1, 9, 2, 10, 17, 25, 32, 40, 48, 56, 57, 49, 41, 33, 26, 18, 3, 11, 4, 12, 19, 27, 34, 42, 50, 58, 35, 43, 51, 59, 20, 28, 5, 13, 6, 14, 21, 29, 36, 44, 52, 60, 37, 45, 53, 61, 22, 30, 7, 15, 23, 31, 38, 46, 54, 62, 39, 47, 55, 63,
];
const NONLINEAR_QSCALE: [u32; 32] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 104, 112];
/// MPEG default intra matrix, in zigzag order.
const DEFAULT_INTRA: [u8; 64] = [
    8, 16, 16, 19, 16, 19, 22, 22, 22, 22, 22, 22, 26, 24, 26, 27, 27, 27, 26, 26, 26, 26, 27, 27, 27, 29, 29, 29, 34, 34, 34, 29, 29, 29, 27, 27, 29, 29, 32, 32, 34, 34, 37, 38, 37, 35, 35, 34, 35, 38, 38, 40, 40, 40, 48, 48, 46, 46, 56, 56, 58, 69, 69, 83,
];

// ---- VLC tables (ISO 13818-2 annex B) ----

/// dct_dc_size_luminance (B.12) and _chrominance (B.13): (code, size).
const DC_LUMA: [(&str, u8); 12] = [
    ("100", 0), ("00", 1), ("01", 2), ("101", 3), ("110", 4), ("1110", 5), ("11110", 6), ("111110", 7), ("1111110", 8), ("11111110", 9), ("111111110", 10), ("111111111", 11),
];
const DC_CHROMA: [(&str, u8); 12] = [
    ("00", 0), ("01", 1), ("10", 2), ("110", 3), ("1110", 4), ("11110", 5), ("111110", 6), ("1111110", 7), ("11111110", 8), ("111111110", 9), ("1111111110", 10), ("1111111111", 11),
];

/// Codes common to both coefficient tables (13 bits and longer).
const AC_LONG: &[(&str, u8, u16)] = &[
    ("000000011101", 0, 8), ("000000011000", 0, 9), ("000000010011", 0, 10), ("000000010000", 0, 11), ("000000011011", 1, 5), ("000000010100", 2, 4), ("000000011100", 3, 3),
    ("000000010010", 4, 3), ("000000011110", 6, 2), ("000000010101", 7, 2), ("000000010001", 8, 2), ("000000011111", 17, 1), ("000000011010", 18, 1), ("000000011001", 19, 1),
    ("000000010111", 20, 1), ("000000010110", 21, 1), ("0000000011010", 0, 12), ("0000000011001", 0, 13), ("0000000011000", 0, 14), ("0000000010111", 0, 15),
    ("0000000010110", 1, 6), ("0000000010101", 1, 7), ("0000000010100", 2, 5), ("0000000010011", 3, 4), ("0000000010010", 5, 3), ("0000000010001", 9, 2),
    ("0000000010000", 10, 2), ("0000000011111", 22, 1), ("0000000011110", 23, 1), ("0000000011101", 24, 1), ("0000000011100", 25, 1), ("0000000011011", 26, 1),
    ("00000000011111", 0, 16), ("00000000011110", 0, 17), ("00000000011101", 0, 18), ("00000000011100", 0, 19), ("00000000011011", 0, 20), ("00000000011010", 0, 21),
    ("00000000011001", 0, 22), ("00000000011000", 0, 23), ("00000000010111", 0, 24), ("00000000010110", 0, 25), ("00000000010101", 0, 26), ("00000000010100", 0, 27),
    ("00000000010011", 0, 28), ("00000000010010", 0, 29), ("00000000010001", 0, 30), ("00000000010000", 0, 31), ("000000000011000", 0, 32), ("000000000010111", 0, 33),
    ("000000000010110", 0, 34), ("000000000010101", 0, 35), ("000000000010100", 0, 36), ("000000000010011", 0, 37), ("000000000010010", 0, 38), ("000000000010001", 0, 39),
    ("000000000010000", 0, 40), ("000000000011111", 1, 8), ("000000000011110", 1, 9), ("000000000011101", 1, 10), ("000000000011100", 1, 11), ("000000000011011", 1, 12),
    ("000000000011010", 1, 13), ("000000000011001", 1, 14), ("0000000000010011", 1, 15), ("0000000000010010", 1, 16), ("0000000000010001", 1, 17), ("0000000000010000", 1, 18),
    ("0000000000010100", 6, 3), ("0000000000011010", 11, 2), ("0000000000011001", 12, 2), ("0000000000011000", 13, 2), ("0000000000010111", 14, 2), ("0000000000010110", 15, 2),
    ("0000000000010101", 16, 2), ("0000000000011111", 27, 1), ("0000000000011110", 28, 1), ("0000000000011101", 29, 1), ("0000000000011100", 30, 1), ("0000000000011011", 31, 1),
];

/// DCT coefficients table zero (B.14), codes up to 12 bits, sign excluded.
/// Run 255 marks end of block, 254 escape.
const AC_B14: &[(&str, u8, u16)] = &[
    ("10", 255, 0), ("11", 0, 1), ("011", 1, 1), ("0100", 0, 2), ("0101", 2, 1), ("00101", 0, 3), ("00111", 3, 1), ("00110", 4, 1), ("000110", 1, 2), ("000111", 5, 1),
    ("000101", 6, 1), ("000100", 7, 1), ("0000110", 0, 4), ("0000100", 2, 2), ("0000111", 8, 1), ("0000101", 9, 1), ("000001", 254, 0), ("00100110", 0, 5), ("00100001", 0, 6),
    ("00100101", 1, 3), ("00100100", 3, 2), ("00100111", 10, 1), ("00100011", 11, 1), ("00100010", 12, 1), ("00100000", 13, 1), ("0000001010", 0, 7), ("0000001100", 1, 4),
    ("0000001011", 2, 3), ("0000001111", 4, 2), ("0000001001", 5, 2), ("0000001110", 14, 1), ("0000001101", 15, 1), ("0000001000", 16, 1),
];

/// DCT coefficients table one (B.15), used with intra_vlc_format.
const AC_B15: &[(&str, u8, u16)] = &[
    ("0110", 255, 0), ("10", 0, 1), ("010", 1, 1), ("110", 0, 2), ("00101", 2, 1), ("0111", 0, 3), ("00111", 3, 1), ("000110", 4, 1), ("00110", 1, 2), ("000111", 5, 1),
    ("0000110", 6, 1), ("0000100", 7, 1), ("11100", 0, 4), ("0000111", 2, 2), ("0000101", 8, 1), ("1111000", 9, 1), ("000001", 254, 0), ("11101", 0, 5), ("000101", 0, 6),
    ("1111001", 1, 3), ("00100110", 3, 2), ("1111010", 10, 1), ("00100001", 11, 1), ("00100101", 12, 1), ("00100100", 13, 1), ("000100", 0, 7), ("00100111", 1, 4),
    ("11111100", 2, 3), ("11111101", 4, 2), ("000000100", 5, 2), ("000000101", 14, 1), ("000000111", 15, 1), ("0000001101", 16, 1), ("1111011", 0, 8), ("1111100", 0, 9),
    ("00100011", 0, 10), ("00100010", 0, 11), ("00100000", 1, 5), ("0000001100", 2, 4), ("11111010", 0, 12), ("11111011", 0, 13), ("11111110", 0, 14), ("11111111", 0, 15),
];

/// A 16-bit lookup table: entry for every possible next-16-bits value.
/// (code length, run, level); length 0 = invalid code.
struct Vlc(Vec<(u8, u8, u16)>);

impl Vlc {
    fn build(sets: &[&[(&str, u8, u16)]]) -> Vlc {
        let mut t = vec![(0u8, 0u8, 0u16); 1 << 16];
        for set in sets {
            for &(code, run, level) in *set {
                let len = code.len();
                let base = (u32::from_str_radix(code, 2).unwrap() << (16 - len)) as usize;
                for e in t.iter_mut().skip(base).take(1 << (16 - len)) {
                    *e = (len as u8, run, level);
                }
            }
        }
        Vlc(t)
    }
}

fn tables() -> &'static (Vlc, Vlc, Vlc, Vlc) {
    static T: OnceLock<(Vlc, Vlc, Vlc, Vlc)> = OnceLock::new();
    T.get_or_init(|| {
        fn dc(t: &[(&'static str, u8)]) -> Vec<(&'static str, u8, u16)> {
            t.iter().map(|&(c, s)| (c, s, 0u16)).collect()
        }
        (Vlc::build(&[&dc(&DC_LUMA)]), Vlc::build(&[&dc(&DC_CHROMA)]), Vlc::build(&[AC_B14, AC_LONG]), Vlc::build(&[AC_B15, AC_LONG]))
    })
}

/// MSB-first reader over the input FIFO contents.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn peek(&self, n: u32) -> u32 {
        let mut v = 0u64;
        let byte = self.pos / 8;
        for i in 0..5 {
            v = v << 8 | *self.data.get(byte + i).unwrap_or(&0) as u64;
        }
        let v = ((v << (self.pos % 8)) & ((1 << 40) - 1)) >> (40 - n);
        v as u32
    }
    fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }
    fn vlc(&mut self, t: &Vlc) -> Option<(u8, u16)> {
        let (len, run, level) = t.0[self.peek(16) as usize];
        if len == 0 {
            return None;
        }
        self.pos += len as usize;
        Some((run, level))
    }
    fn exhausted(&self) -> bool {
        self.pos >= self.data.len() * 8
    }
}

/// Decoding parameters from IPU_CTRL and the IDEC command.
struct Params {
    idp: u32,
    ivf: bool,
    qst: bool,
    mp1: bool,
    alternate: bool,
    dtd: bool,
    rgb16: bool,
    dither: bool,
}

pub struct Ipu {
    /// Input bytes not yet consumed, and the bit position in them.
    input: Vec<u8>,
    pos: usize,
    out: VecDeque<u128>,
    ctrl: u32,
    /// Last FDEC/VDEC result.
    result: u32,
    /// Intra and non-intra matrices in natural order.
    iq: [[u8; 64]; 2],
    vqclut: [u16; 16],
    th: (u32, u32),
}

impl Default for Ipu {
    fn default() -> Self {
        let mut iq = [[16u8; 64]; 2];
        for (i, &z) in ZIGZAG.iter().enumerate() {
            iq[0][z] = DEFAULT_INTRA[i];
        }
        Ipu { input: Vec::new(), pos: 0, out: VecDeque::new(), ctrl: 0, result: 0, iq, vqclut: [0; 16], th: (0, 0) }
    }
}

impl Ipu {
    pub fn feed(&mut self, data: &[u8]) {
        // Drop consumed bytes now and then to keep the buffer small.
        if self.pos >= 8 * 4096 {
            let n = self.pos / 8;
            self.input.drain(..n.min(self.input.len()));
            self.pos -= n * 8;
        }
        self.input.extend_from_slice(data);
    }

    pub fn read(&mut self, addr: u32) -> u128 {
        match addr {
            // DATA in the low word; BUSY (bit 63) never set.
            IPU_CMD => self.result as u128,
            IPU_CTRL => {
                let ifc = ((self.input.len() * 8).saturating_sub(self.pos) / 128).min(8) as u32;
                let ofc = self.out.len().min(8) as u32;
                (self.ctrl & !0xff | ifc | ofc << 4) as u128
            }
            IPU_BP => {
                let ifc = ((self.input.len() * 8).saturating_sub(self.pos) / 128).min(8) as u32;
                ((self.pos % 128) as u32 | ifc << 8) as u128
            }
            IPU_TOP => self.bits().peek(32) as u128,
            IPU_OUT_FIFO => self.out.pop_front().unwrap_or(0),
            _ => 0,
        }
    }

    fn bits(&self) -> Bits<'_> {
        Bits { data: &self.input, pos: self.pos }
    }

    /// Register write. Returns true when a command produced output.
    pub fn write(&mut self, addr: u32, v: u128) -> bool {
        match addr {
            IPU_CTRL => {
                let v = v as u32;
                if v & CTRL_RST != 0 {
                    *self = Ipu { iq: self.iq, vqclut: self.vqclut, th: self.th, ..Ipu::default() };
                }
                // Software sets IDP, AS, IVF, QST, MP1 and PCT.
                self.ctrl = (self.ctrl & (CTRL_ECD | CTRL_SCD)) | (v & 0x07f3_0000);
                false
            }
            IPU_IN_FIFO => {
                self.feed(&v.to_le_bytes());
                false
            }
            IPU_CMD => self.command(v as u32),
            _ => false,
        }
    }

    fn command(&mut self, cmd: u32) -> bool {
        let fb = (cmd & 0x3f) as usize;
        match cmd >> 28 {
            // BCLR: empty the input FIFO; BP selects the first bit.
            0 => {
                self.input.clear();
                self.pos = (cmd & 0x7f) as usize;
                self.ctrl &= !(CTRL_ECD | CTRL_SCD);
                false
            }
            1 => {
                self.pos += fb;
                self.idec(cmd);
                true
            }
            // FDEC: skip FB bits and show the next 32.
            4 => {
                self.pos += fb;
                self.result = self.bits().peek(32);
                false
            }
            // SETIQ: 64 matrix bytes in zigzag order (intra, or non-intra
            // with IQM, bit 27).
            5 => {
                self.pos += fb;
                let m = ((cmd >> 27) & 1) as usize;
                let mut b = Bits { data: &self.input, pos: self.pos };
                let mut q = [0u8; 64];
                for &z in ZIGZAG.iter() {
                    q[z] = b.get(8) as u8;
                }
                self.pos = b.pos;
                self.iq[m] = q;
                false
            }
            6 => {
                let mut b = Bits { data: &self.input, pos: self.pos };
                let clut: [u16; 16] = std::array::from_fn(|_| (b.get(16) as u16).swap_bytes());
                self.pos = b.pos;
                self.vqclut = clut;
                false
            }
            9 => {
                self.th = (cmd & 0x1ff, (cmd >> 16) & 0x1ff);
                false
            }
            n => {
                eprintln!("[ipu] command {n} ({cmd:#010x}) not implemented");
                false
            }
        }
    }

    /// IDEC: decode intra macroblocks until the bitstream stops with a
    /// start code (or ends), converting each to RGB.
    fn idec(&mut self, cmd: u32) {
        let p = Params {
            idp: (self.ctrl >> 16) & 3,
            alternate: self.ctrl & 1 << 20 != 0,
            ivf: self.ctrl & 1 << 21 != 0,
            // Bit 22 (QST, the non-linear quantiser scale) is set by the
            // game for every picture, but all 21 on the disc are encoded
            // with the linear scale (header flags 0): decoding them
            // non-linearly is visibly blocky, linearly matches FFmpeg's
            // reference decoder to 45-60 dB. Use the linear scale.
            qst: false,
            mp1: self.ctrl & 1 << 23 != 0,
            dtd: cmd & 1 << 24 != 0,
            dither: cmd & 1 << 26 != 0,
            rgb16: cmd & 1 << 27 != 0,
        };
        let mut qsc = (cmd >> 16) & 31;
        let (luma, chroma, b14, b15) = tables();
        let ac = if p.ivf { b15 } else { b14 };
        let input = std::mem::take(&mut self.input);
        let mut b = Bits { data: &input, pos: self.pos };
        let mut dc_pred = [128i32 << p.idp; 3];
        let mut first = true;
        self.ctrl &= !(CTRL_ECD | CTRL_SCD);
        loop {
            if !first {
                // macroblock_address_increment: anything but '1' (or an
                // escape before it) ends the run of macroblocks.
                while b.peek(11) == 0b000_0000_1000 {
                    b.get(11);
                }
                if b.peek(1) != 1 {
                    if b.peek(23) == 0 {
                        self.ctrl |= CTRL_SCD;
                    }
                    break;
                }
                b.get(1);
            }
            first = false;
            if b.exhausted() {
                self.ctrl |= CTRL_ECD;
                break;
            }
            // macroblock_type (I picture): '1' intra, '01' intra + quant.
            let quant = if b.get(1) == 1 {
                false
            } else if b.get(1) == 1 {
                true
            } else {
                self.ctrl |= CTRL_ECD;
                break;
            };
            let field_dct = p.dtd && b.get(1) == 1;
            if quant {
                qsc = b.get(5);
            }
            let qscale = if p.qst { NONLINEAR_QSCALE[qsc as usize] } else { qsc * 2 } as i32;
            let mut blocks = [[0i32; 64]; 6];
            let mut ok = true;
            for (n, blk) in blocks.iter_mut().enumerate() {
                let c = if n < 4 { 0 } else { n - 3 };
                let Some((size, _)) = b.vlc(if c == 0 { luma } else { chroma }) else {
                    ok = false;
                    break;
                };
                let diff = if size == 0 {
                    0
                } else {
                    let v = b.get(size as u32) as i32;
                    if v < 1 << (size - 1) {
                        v - (1 << size) + 1
                    } else {
                        v
                    }
                };
                dc_pred[c] += diff;
                if !self.block(&mut b, blk, dc_pred[c], qscale, ac, &p) {
                    ok = false;
                    break;
                }
            }
            if !ok {
                self.ctrl |= CTRL_ECD;
                break;
            }
            self.emit_macroblock(&blocks, field_dct, &p);
        }
        self.pos = b.pos;
        self.input = input;
    }

    /// Decode, dequantise and inverse-transform one intra block.
    fn block(&self, b: &mut Bits, blk: &mut [i32; 64], dc: i32, qscale: i32, ac: &Vlc, p: &Params) -> bool {
        let scan = if p.alternate { &ALTERNATE } else { &ZIGZAG };
        let mut coef = [0i32; 64];
        coef[0] = dc << (3 - p.idp);
        let mut i = 0usize;
        loop {
            let Some((run, level)) = b.vlc(ac) else { return false };
            let (run, level) = match run {
                255 => break,
                254 => {
                    let run = b.get(6) as usize;
                    let level = if p.mp1 {
                        let l = b.get(8) as i32;
                        match l {
                            0 => b.get(8) as i32,
                            128 => b.get(8) as i32 - 256,
                            _ => (l as i8) as i32,
                        }
                    } else {
                        ((b.get(12) << 20) as i32) >> 20
                    };
                    (run, level)
                }
                r => {
                    let s = b.get(1);
                    (r as usize, if s == 1 { -(level as i32) } else { level as i32 })
                }
            };
            i += run + 1;
            if i > 63 {
                return false;
            }
            let pos = scan[i];
            let w = self.iq[0][pos] as i32;
            let mut v = level * qscale * w / 16;
            if p.mp1 && v & 1 == 0 && v != 0 {
                v -= v.signum();
            }
            coef[pos] = v.clamp(-2048, 2047);
        }
        if !p.mp1 {
            // Mismatch control.
            if coef.iter().sum::<i32>() & 1 == 0 {
                coef[63] ^= 1;
            }
        }
        idct(&coef, blk);
        true
    }

    /// Colour-convert a macroblock and queue it as 16x16 RGBA32 (or
    /// RGBA16) pixels.
    fn emit_macroblock(&mut self, blocks: &[[i32; 64]; 6], field_dct: bool, p: &Params) {
        let y_at = |x: usize, y: usize| -> i32 {
            let blk = (y >= 8) as usize * 2 + (x >= 8) as usize;
            if field_dct {
                // Lines alternate between the top (0/1) and bottom (2/3)
                // field blocks.
                let blk = (y & 1) * 2 + (x >= 8) as usize;
                blocks[blk][(y >> 1) * 8 + (x & 7)]
            } else {
                blocks[blk][(y & 7) * 8 + (x & 7)]
            }
        };
        let mut px = [0u32; 256];
        for y in 0..16 {
            for x in 0..16 {
                let yy = y_at(x, y).clamp(0, 255) as f32;
                let cb = blocks[4][(y / 2) * 8 + x / 2].clamp(0, 255) as f32 - 128.0;
                let cr = blocks[5][(y / 2) * 8 + x / 2].clamp(0, 255) as f32 - 128.0;
                let l = 1.164 * (yy - 16.0);
                let r = (l + 1.596 * cr).round().clamp(0.0, 255.0) as u32;
                let g = (l - 0.391 * cb - 0.813 * cr).round().clamp(0.0, 255.0) as u32;
                let bl = (l + 2.018 * cb).round().clamp(0.0, 255.0) as u32;
                let (t0, t1) = self.th;
                let a = if r < t0 && g < t0 && bl < t0 {
                    0
                } else if r < t1 && g < t1 && bl < t1 {
                    0x40
                } else {
                    0x80
                };
                px[y * 16 + x] = r | g << 8 | bl << 16 | a << 24;
            }
        }
        if p.rgb16 {
            let _ = p.dither;
            let mut words = [0u16; 256];
            for (w, &c) in words.iter_mut().zip(px.iter()) {
                let (r, g, b, a) = (c & 0xff, (c >> 8) & 0xff, (c >> 16) & 0xff, c >> 24);
                *w = (r >> 3 | (g >> 3) << 5 | (b >> 3) << 10 | u32::from(a == 0x80) << 15) as u16;
            }
            for q in words.chunks_exact(8) {
                self.out.push_back(q.iter().enumerate().fold(0u128, |acc, (i, &w)| acc | (w as u128) << (16 * i)));
            }
        } else {
            for q in px.chunks_exact(4) {
                self.out.push_back(q.iter().enumerate().fold(0u128, |acc, (i, &w)| acc | (w as u128) << (32 * i)));
            }
        }
    }

    /// Take up to `qwc` qwords of output.
    pub fn take_output(&mut self, qwc: u32) -> Vec<u128> {
        let n = (qwc as usize).min(self.out.len());
        self.out.drain(..n).collect()
    }

    pub fn has_output(&self) -> bool {
        !self.out.is_empty()
    }
}

/// Separable floating-point 8x8 inverse DCT.
fn idct(coef: &[i32; 64], out: &mut [i32; 64]) {
    static COS: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    let c = COS.get_or_init(|| {
        let mut t = [[0f32; 8]; 8];
        for (x, row) in t.iter_mut().enumerate() {
            for (u, v) in row.iter_mut().enumerate() {
                let cu = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
                *v = cu * ((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / 16.0).cos() / 2.0;
            }
        }
        t
    });
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for x in 0..8 {
            tmp[y * 8 + x] = (0..8).map(|u| c[x][u] * coef[y * 8 + u] as f32).sum();
        }
    }
    for x in 0..8 {
        for y in 0..8 {
            let v: f32 = (0..8).map(|v| c[y][v] * tmp[v * 8 + x]).sum();
            out[y * 8 + x] = v.round() as i32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlc_tables_are_prefix_free() {
        for set in [vec![AC_B14, AC_LONG], vec![AC_B15, AC_LONG]] {
            let all: Vec<&str> = set.iter().flat_map(|s| s.iter().map(|e| e.0)).collect();
            for a in &all {
                for b in &all {
                    assert!(a == b || !b.starts_with(a), "{a} is a prefix of {b}");
                }
            }
        }
    }

    #[test]
    fn dc_only_block_is_flat() {
        let mut coef = [0i32; 64];
        coef[0] = 128 * 8;
        let mut out = [0i32; 64];
        idct(&coef, &mut out);
        assert!(out.iter().all(|&v| v == 128));
    }

    #[test]
    fn bits_read_msb_first() {
        let data = [0b1010_0000u8, 0xff];
        let mut b = Bits { data: &data, pos: 0 };
        assert_eq!(b.get(3), 0b101);
        assert_eq!(b.get(5), 0);
        assert_eq!(b.get(4), 0xf);
    }
}

/// Decode a whole "ipum" picture file (.LSC) the way Freekstyle drives the
/// IPU. Returns (width, height, pixels) in raster order, RGBA with PS2
/// alpha (0x80 = opaque).
pub fn decode_picture(file: &[u8]) -> Option<(u32, u32, Vec<u32>)> {
    if file.len() < 16 || &file[..4] != b"ipum" {
        return None;
    }
    let (w, h) = (u16::from_le_bytes([file[8], file[9]]) as u32, u16::from_le_bytes([file[10], file[11]]) as u32);
    let mut ipu = Ipu::default();
    ipu.feed(&file[16..]);
    // The first byte holds picture flags; macroblocks follow.
    ipu.write(IPU_CMD, 0x4000_0008);
    ipu.write(IPU_CMD, 0x1001_0000);
    let out = ipu.take_output(u32::MAX);
    let (mbw, mbh) = (w.div_ceil(16), h.div_ceil(16));
    if out.len() < (mbw * mbh * 64) as usize {
        return None;
    }
    let mut px = vec![0u32; (w * h) as usize];
    for (mb, q) in out.chunks(64).take((mbw * mbh) as usize).enumerate() {
        let (mx, my) = ((mb as u32 % mbw) * 16, (mb as u32 / mbw) * 16);
        for (i, p) in q.iter().flat_map(|q| (0..4).map(move |n| (*q >> (32 * n)) as u32)).enumerate() {
            let (x, y) = (mx + i as u32 % 16, my + i as u32 / 16);
            if x < w && y < h {
                px[(y * w + x) as usize] = p;
            }
        }
    }
    Some((w, h, px))
}
