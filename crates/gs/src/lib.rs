//! The PS2 Graphics Synthesizer, independent of any host graphics API.
//!
//! [`Gs`] consumes GIF packets (from PATH1/2/3), keeps the register state and
//! the 4 MB local memory, performs image transfers and assembles primitives.
//! Assembled primitives, memory writes and display changes go to a
//! [`Backend`], which renders them (the Vulkan renderer lives in
//! `gs-vulkan`).

pub mod mem;
pub mod regs;
pub mod tex;

use mem::{Psm, Vram};
use regs::*;

/// A vertex in window coordinates (XYOFFSET already applied).
#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
    pub z: u32,
    pub rgba: [u8; 4],
    /// STQ (perspective) texture coordinates.
    pub s: f32,
    pub t: f32,
    pub q: f32,
    /// UV texel coordinates (FST mode).
    pub u: f32,
    pub v: f32,
    pub fog: u8,
}

/// Primitive shapes after assembly. Strips and fans arrive as triangles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Point,
    Line,
    Triangle,
    /// Axis-aligned rectangle from two corners.
    Sprite,
}

/// A rectangle of local memory that changed outside of drawing.
#[derive(Clone, Copy, Debug)]
pub struct MemRect {
    pub bp: u32,
    pub bw: u32,
    pub psm: Psm,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Receives the GS's output.
pub trait Backend {
    /// Draw one primitive using the current state of `gs`.
    fn draw(&mut self, gs: &Gs, shape: Shape, v: &[Vertex]);
    /// Local memory was written by a host transfer or local copy.
    fn mem_written(&mut self, gs: &Gs, r: MemRect);
    /// Local memory is about to be read by the GS itself (texture/CLUT
    /// loads, local copies, readback). Backends that keep render targets
    /// on the GPU write them back here.
    fn mem_read(&mut self, _gs: &Gs, _r: MemRect) {}
}

/// Messages from the EE side to the GS (which runs on the render thread).
pub enum Msg {
    /// GIF packet data (PATH2 or PATH3).
    Gif(Vec<u128>),
    /// Write to a privileged register, by offset from 0x1200_0000.
    Priv(u32, u64),
    /// A vertical blank happened (with its number since boot): present
    /// the current display.
    Vsync(u64),
    /// A full-screen movie frame, shown instead of the GS display until
    /// `Movie(None)` ends the movie.
    Movie(Option<Box<MovieFrame>>),
}

/// One decoded movie frame: `w` x `h` RGBA8 pixels, rows top to bottom.
pub struct MovieFrame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u32>,
}

/// Privileged (memory-mapped) registers that control video output.
#[derive(Clone, Copy, Debug, Default)]
pub struct Display {
    pub pmode: u64,
    pub smode2: u64,
    pub dispfb: [u64; 2],
    pub display: [u64; 2],
    pub bgcolor: u64,
}

impl Display {
    /// Enabled read circuits.
    pub fn enabled(&self, n: usize) -> bool {
        self.pmode & (1 << n) != 0
    }
}

#[derive(Default)]
struct Transfer {
    active: bool,
    psm: Option<Psm>,
    bp: u32,
    bw: u32,
    x0: u32,
    y0: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// Bytes carried between packets when pixels straddle qwords.
    pending: Vec<u8>,
    /// First row not yet reported to the backend.
    reported: u32,
}

/// Where a GIF stream is in its current tag.
#[derive(Default)]
struct GifState {
    nloop: u32,
    nreg: u32,
    regs: u64,
    flg: u32,
    reg_index: u32,
}

pub struct Gs {
    pub vram: Vram,
    pub ctx: [Context; 2],
    pub prim: Prim,
    pub prmode: Prim,
    pub prmodecont: bool,
    pub rgbaq: [u8; 4],
    pub q: f32,
    pub st: (f32, f32),
    pub uv: (f32, f32),
    pub fog: u8,
    pub fogcol: u32,
    pub texa: Texa,
    pub texclut: u64,
    pub colclamp: bool,
    pub bitbltbuf: u64,
    pub trxpos: u64,
    pub trxreg: u64,
    /// CLUT buffer: 256 entries, raw 32-bit or 16-bit colours.
    pub clut: [u32; 256],
    pub clut_psm: Psm,
    /// Bumped on every CLUT load, so renderers can cache work on it.
    pub clut_gen: u64,
    cbp_cache: [u32; 2],
    pub display: Display,
    queue: Vec<Vertex>,
    transfer: Transfer,
    gif: GifState,
    /// Internal Q from the most recent packed ST write.
    packed_q: f32,
    pub signal: Option<u64>,
    pub finish: bool,
    pub stats: Stats,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct Stats {
    pub prims: u64,
    pub transfers: u64,
    pub packets: u64,
}

impl Default for Gs {
    fn default() -> Self {
        Self::new()
    }
}

impl Gs {
    pub fn new() -> Gs {
        Gs {
            vram: Vram::new(),
            ctx: [Context::default(); 2],
            prim: Prim(0),
            prmode: Prim(0),
            prmodecont: true,
            rgbaq: [0x80, 0x80, 0x80, 0x80],
            q: 1.0,
            st: (0.0, 0.0),
            uv: (0.0, 0.0),
            fog: 0,
            fogcol: 0,
            texa: Texa(0),
            texclut: 0,
            colclamp: true,
            bitbltbuf: 0,
            trxpos: 0,
            trxreg: 0,
            clut: [0; 256],
            clut_psm: Psm::Ct32,
            clut_gen: 0,
            cbp_cache: [u32::MAX; 2],
            display: Display::default(),
            queue: Vec::with_capacity(4),
            transfer: Transfer::default(),
            gif: GifState::default(),
            packed_q: 1.0,
            signal: None,
            finish: false,
            stats: Stats::default(),
        }
    }

    /// Privileged register write (PMODE, DISPFB, DISPLAY, BGCOLOR, ...).
    pub fn write_priv(&mut self, offset: u32, v: u64) {
        let d = &mut self.display;
        match offset {
            0x00 => d.pmode = v,
            0x20 => d.smode2 = v,
            0x70 => d.dispfb[0] = v,
            0x80 => d.display[0] = v,
            0x90 => d.dispfb[1] = v,
            0xa0 => d.display[1] = v,
            0xe0 => d.bgcolor = v,
            _ => {}
        }
    }

    /// The attributes in force for drawing (PRIM or PRMODE).
    pub fn attrs(&self) -> Prim {
        if self.prmodecont {
            self.prim
        } else {
            Prim((self.prim.0 & 7) | (self.prmode.0 & !7))
        }
    }

    /// The drawing context selected by the current attributes.
    pub fn context(&self) -> &Context {
        &self.ctx[self.attrs().context()]
    }

    // ---- GIF ----

    /// Consume GIF data (one or more packets, possibly split anywhere at
    /// qword boundaries).
    pub fn write_gif(&mut self, data: &[u128], b: &mut dyn Backend) {
        let mut i = 0;
        while i < data.len() {
            if self.gif.nloop == 0 {
                self.start_tag(data[i]);
                i += 1;
                continue;
            }
            match self.gif.flg {
                0 => {
                    let q = data[i];
                    i += 1;
                    let reg = (self.gif.regs >> (4 * self.gif.reg_index)) & 15;
                    self.packed(reg as u8, q, b);
                    self.next_reg();
                }
                1 => {
                    // REGLIST: two 64-bit register writes per qword.
                    let q = data[i];
                    i += 1;
                    for half in [q as u64, (q >> 64) as u64] {
                        if self.gif.nloop == 0 {
                            break; // odd count: the upper half is padding
                        }
                        let reg = ((self.gif.regs >> (4 * self.gif.reg_index)) & 15) as u8;
                        if reg != 0xe && reg != 0xf {
                            self.write_reg(reg, half, b);
                        }
                        self.next_reg();
                    }
                }
                _ => {
                    // IMAGE: NLOOP qwords of transfer data.
                    let n = (self.gif.nloop as usize).min(data.len() - i);
                    self.image_data(&data[i..i + n], b);
                    self.gif.nloop -= n as u32;
                    i += n;
                }
            }
        }
        self.flush_transfer(b);
    }

    fn start_tag(&mut self, tag: u128) {
        self.stats.packets += 1;
        let lo = tag as u64;
        self.gif.nloop = (lo & 0x7fff) as u32;
        let pre = lo >> 46 & 1 != 0;
        self.gif.flg = (lo >> 58 & 3) as u32;
        self.gif.nreg = match (lo >> 60) as u32 {
            0 => 16,
            n => n,
        };
        self.gif.regs = (tag >> 64) as u64;
        self.gif.reg_index = 0;
        if pre && self.gif.flg == 0 {
            self.set_prim((lo >> 47) & 0x7ff);
        }
        // A packed tag restarts Q at 1.0 for RGBAQ writes.
        self.packed_q = 1.0;
    }

    fn next_reg(&mut self) {
        self.gif.reg_index += 1;
        if self.gif.reg_index == self.gif.nreg {
            self.gif.reg_index = 0;
            self.gif.nloop -= 1;
        }
    }

    fn packed(&mut self, reg: u8, q: u128, b: &mut dyn Backend) {
        let lo = q as u64;
        let hi = (q >> 64) as u64;
        let field = |shift: u32, n: u32| ((q >> shift) & ((1u128 << n) - 1)) as u64;
        match reg {
            0x0 => self.set_prim(lo & 0x7ff),
            0x1 => {
                let v = field(0, 8) | field(32, 8) << 8 | field(64, 8) << 16 | field(96, 8) << 24
                    | (self.packed_q.to_bits() as u64) << 32;
                self.write_reg(RGBAQ, v, b);
            }
            0x2 => {
                self.packed_q = f32::from_bits(hi as u32);
                self.write_reg(ST, lo, b);
            }
            0x3 => self.write_reg(UV, field(0, 14) | field(32, 14) << 16, b),
            0x4 | 0xc => {
                let v = field(0, 16) | field(32, 16) << 16 | field(68, 24) << 32 | field(100, 8) << 56;
                let no_kick = reg == 0xc || q >> 111 & 1 != 0;
                self.write_reg(if no_kick { XYZF3 } else { XYZF2 }, v, b);
            }
            0x5 | 0xd => {
                let v = field(0, 16) | field(32, 16) << 16 | field(64, 32) << 32;
                let no_kick = reg == 0xd || q >> 111 & 1 != 0;
                self.write_reg(if no_kick { XYZ3 } else { XYZ2 }, v, b);
            }
            0x6..=0x9 => self.write_reg(reg, lo, b),
            0xa => self.write_reg(FOG, field(100, 8) << 56, b),
            0xe => self.write_reg((hi & 0xff) as u8, lo, b),
            _ => {}
        }
    }

    fn set_prim(&mut self, v: u64) {
        self.prim = Prim(v);
        self.queue.clear();
    }

    /// Write a GS register (A+D, REGLIST or packed after conversion).
    pub fn write_reg(&mut self, reg: u8, v: u64, b: &mut dyn Backend) {
        match reg {
            PRIM => self.set_prim(v & 0x7ff),
            RGBAQ => {
                self.rgbaq = (v as u32).to_le_bytes();
                self.q = f32::from_bits((v >> 32) as u32);
            }
            ST => self.st = (f32::from_bits(v as u32), f32::from_bits((v >> 32) as u32)),
            UV => self.uv = ((v & 0x3fff) as f32 / 16.0, ((v >> 16) & 0x3fff) as f32 / 16.0),
            XYZF2 | XYZF3 => {
                self.fog = (v >> 56) as u8;
                self.vertex((v & 0xffff) as u32, (v >> 16 & 0xffff) as u32, (v >> 32 & 0xff_ffff) as u32, reg == XYZF2, b);
            }
            XYZ2 | XYZ3 => {
                self.vertex((v & 0xffff) as u32, (v >> 16 & 0xffff) as u32, (v >> 32) as u32, reg == XYZ2, b);
            }
            FOG => self.fog = (v >> 56) as u8,
            TEX0_1 | TEX0_2 => {
                let n = (reg - TEX0_1) as usize;
                self.ctx[n].tex0 = Tex0(v);
                self.load_clut(Tex0(v), b);
            }
            TEX2_1 | TEX2_2 => {
                // TEX2 updates only the format and CLUT fields of TEX0.
                let n = (reg - TEX2_1) as usize;
                let mask = 0x3f << 20 | !0u64 << 37;
                let t = Tex0((self.ctx[n].tex0.0 & !mask) | (v & mask));
                self.ctx[n].tex0 = t;
                self.load_clut(t, b);
            }
            CLAMP_1 | CLAMP_2 => self.ctx[(reg - CLAMP_1) as usize].clamp = Clamp(v),
            TEX1_1 | TEX1_2 => self.ctx[(reg - TEX1_1) as usize].tex1 = Tex1(v),
            XYOFFSET_1 | XYOFFSET_2 => self.ctx[(reg - XYOFFSET_1) as usize].xyoffset = v,
            PRMODECONT => self.prmodecont = v & 1 != 0,
            PRMODE => self.prmode = Prim(v & 0x7f8),
            TEXCLUT => self.texclut = v,
            TEXA => self.texa = Texa(v),
            FOGCOL => self.fogcol = v as u32 & 0xff_ffff,
            SCISSOR_1 | SCISSOR_2 => self.ctx[(reg - SCISSOR_1) as usize].scissor = Scissor(v),
            ALPHA_1 | ALPHA_2 => self.ctx[(reg - ALPHA_1) as usize].alpha = Alpha(v),
            COLCLAMP => self.colclamp = v & 1 != 0,
            TEST_1 | TEST_2 => self.ctx[(reg - TEST_1) as usize].test = Test(v),
            FBA_1 | FBA_2 => self.ctx[(reg - FBA_1) as usize].fba = v & 1 != 0,
            FRAME_1 | FRAME_2 => self.ctx[(reg - FRAME_1) as usize].frame = Frame(v),
            ZBUF_1 | ZBUF_2 => self.ctx[(reg - ZBUF_1) as usize].zbuf = Zbuf(v),
            BITBLTBUF => self.bitbltbuf = v,
            TRXPOS => self.trxpos = v,
            TRXREG => self.trxreg = v,
            TRXDIR => self.start_transfer(v as u32 & 3, b),
            HWREG => self.image_data(&[v as u128], b),
            SIGNAL => self.signal = Some(v),
            FINISH => self.finish = true,
            _ => {}
        }
    }

    // ---- vertices and primitives ----

    fn vertex(&mut self, x: u32, y: u32, z: u32, kick: bool, b: &mut dyn Backend) {
        let ctx = self.context();
        let (ox, oy) = ctx.offset();
        let v = Vertex {
            x: x as f32 / 16.0 - ox,
            y: y as f32 / 16.0 - oy,
            z,
            rgba: self.rgbaq,
            s: self.st.0,
            t: self.st.1,
            q: self.q,
            u: self.uv.0,
            v: self.uv.1,
            fog: self.fog,
        };
        self.queue.push(v);
        let kind = self.prim.kind();
        let need = match kind {
            0 => 1,
            1 | 2 | 6 => 2,
            3..=5 => 3,
            _ => {
                self.queue.clear();
                return;
            }
        };
        if self.queue.len() < need {
            return;
        }
        if kick {
            let shape = match kind {
                0 => Shape::Point,
                1 | 2 => Shape::Line,
                6 => Shape::Sprite,
                _ => Shape::Triangle,
            };
            let mut verts = [Vertex::default(); 3];
            verts[..need].copy_from_slice(&self.queue[self.queue.len() - need..]);
            // Flat shading uses the last vertex's colour.
            if !self.attrs().gouraud() || shape == Shape::Sprite {
                let c = verts[need - 1].rgba;
                for v in &mut verts[..need] {
                    v.rgba = c;
                }
            }
            self.stats.prims += 1;
            b.draw(self, shape, &verts[..need]);
        }
        // Keep what the next primitive reuses.
        match kind {
            2 => {
                self.queue.drain(..self.queue.len() - 1);
            }
            4 => {
                self.queue.drain(..self.queue.len() - 2);
            }
            5 => {
                let first = self.queue[0];
                let last = *self.queue.last().unwrap();
                self.queue.clear();
                self.queue.extend([first, last]);
            }
            _ => self.queue.clear(),
        }
    }

    // ---- CLUT ----

    fn load_clut(&mut self, t: Tex0, b: &mut dyn Backend) {
        if !t.psm().is_indexed() {
            return;
        }
        let load = match t.cld() {
            1 => true,
            2 => {
                self.cbp_cache[0] = t.cbp();
                true
            }
            3 => {
                self.cbp_cache[1] = t.cbp();
                true
            }
            4 => std::mem::replace(&mut self.cbp_cache[0], t.cbp()) != t.cbp(),
            5 => std::mem::replace(&mut self.cbp_cache[1], t.cbp()) != t.cbp(),
            _ => false,
        };
        if !load {
            return;
        }
        let eight = matches!(t.psm(), Psm::T8 | Psm::T8h);
        let n = if eight { 256 } else { 16 };
        let cpsm = t.cpsm();
        let cpsm = if cpsm == Psm::Ct32 || cpsm == Psm::Ct24 { Psm::Ct32 } else { cpsm };
        // 4-bit CLUTs of 16-bit colours can sit at any of 32 offsets;
        // 32-bit ones at 16.
        let offset = if eight { 0 } else { t.csa() as usize * 16 % 256 };
        b.mem_read(self, MemRect { bp: t.cbp(), bw: 1, psm: cpsm, x: 0, y: 0, w: 16, h: 16 });
        for i in 0..n {
            let (x, y) = if t.csm2() {
                // CSM2: one linear row at (COU*16, COV).
                let cou = ((self.texclut >> 6) & 0x3f) as u32 * 16;
                let cov = ((self.texclut >> 12) & 0x3ff) as u32;
                (cou + i as u32, cov)
            } else if eight {
                // CSM1: bits 3 and 4 of the index are swapped.
                let j = (i & 0xe7) | (i & 0x08) << 1 | (i & 0x10) >> 1;
                ((j & 15) as u32, (j >> 4) as u32)
            } else {
                ((i & 7) as u32, (i >> 3) as u32)
            };
            let bw = if t.csm2() { (self.texclut & 0x3f) as u32 } else { 1 };
            self.clut[(offset + i) % 256] = self.vram.read(cpsm, t.cbp(), bw, x, y);
        }
        self.clut_psm = cpsm;
        self.clut_gen += 1;
    }

    // ---- transfers ----

    fn start_transfer(&mut self, dir: u32, b: &mut dyn Backend) {
        self.flush_transfer(b);
        let bb = self.bitbltbuf;
        let pos = self.trxpos;
        let (w, h) = ((self.trxreg & 0xfff) as u32, ((self.trxreg >> 32) & 0xfff) as u32);
        match dir {
            0 => {
                let (dx, dy) = (((pos >> 32) & 0x7ff) as u32, ((pos >> 48) & 0x7ff) as u32);
                self.transfer = Transfer {
                    active: w > 0 && h > 0,
                    psm: Some(Psm::from_bits(bb >> 56)),
                    bp: ((bb >> 32) & 0x3fff) as u32,
                    bw: ((bb >> 48) & 0x3f) as u32,
                    x0: dx,
                    y0: dy,
                    x: dx,
                    y: dy,
                    w,
                    h,
                    pending: Vec::new(),
                    reported: dy,
                };
                self.stats.transfers += 1;
            }
            2 => {
                let (sbp, sbw, spsm) = ((bb & 0x3fff) as u32, ((bb >> 16) & 0x3f) as u32, Psm::from_bits(bb >> 24));
                let (dbp, dbw, dpsm) = (((bb >> 32) & 0x3fff) as u32, ((bb >> 48) & 0x3f) as u32, Psm::from_bits(bb >> 56));
                let (sx, sy) = ((pos & 0x7ff) as u32, ((pos >> 16) & 0x7ff) as u32);
                let (dx, dy) = (((pos >> 32) & 0x7ff) as u32, ((pos >> 48) & 0x7ff) as u32);
                b.mem_read(self, MemRect { bp: sbp, bw: sbw, psm: spsm, x: sx, y: sy, w, h });
                let mut px = Vec::with_capacity((w * h) as usize);
                for y in 0..h {
                    for x in 0..w {
                        px.push(self.vram.read(spsm, sbp, sbw, (sx + x) & 2047, (sy + y) & 2047));
                    }
                }
                for y in 0..h {
                    for x in 0..w {
                        self.vram.write(dpsm, dbp, dbw, (dx + x) & 2047, (dy + y) & 2047, px[(y * w + x) as usize]);
                    }
                }
                b.mem_written(self, MemRect { bp: dbp, bw: dbw, psm: dpsm, x: dx, y: dy, w, h });
            }
            _ => {
                // Local-to-host readback: the EE would read the data from the
                // GIF FIFO. Not supported yet.
                self.transfer = Transfer::default();
            }
        }
    }

    fn image_data(&mut self, data: &[u128], b: &mut dyn Backend) {
        if !self.transfer.active {
            return;
        }
        let Gs { transfer: t, vram, .. } = self;
        let psm = t.psm.unwrap();
        let bits = psm.transfer_bits() as usize;
        let mut bytes = std::mem::take(&mut t.pending);
        for q in data {
            bytes.extend_from_slice(&q.to_le_bytes());
        }
        let mut put = |t: &mut Transfer, v: u32| {
            vram.write(psm, t.bp, t.bw, t.x & 2047, t.y & 2047, v);
            advance(t);
        };
        let mut pos = 0;
        while t.active {
            if bits == 4 {
                // Two pixels per byte, low nibble first.
                let Some(&byte) = bytes.get(pos) else { break };
                pos += 1;
                put(t, byte as u32 & 15);
                if t.active {
                    put(t, byte as u32 >> 4);
                }
            } else {
                let n = bits / 8;
                if pos + n > bytes.len() {
                    break;
                }
                let mut v = [0u8; 4];
                v[..n].copy_from_slice(&bytes[pos..pos + n]);
                pos += n;
                put(t, u32::from_le_bytes(v));
            }
        }
        t.pending = bytes[pos..].to_vec();
        if !t.active {
            self.flush_transfer(b);
        }

        fn advance(t: &mut Transfer) {
            t.x += 1;
            if t.x == t.x0 + t.w {
                t.x = t.x0;
                t.y += 1;
                if t.y == t.y0 + t.h {
                    t.active = false;
                }
            }
        }
    }

    /// Report the rows a host transfer has written since the last report.
    fn flush_transfer(&mut self, b: &mut dyn Backend) {
        let t = &mut self.transfer;
        let Some(psm) = t.psm else { return };
        // A partly written row is reported now and again when completed.
        let end = if !t.active {
            t.y0 + t.h
        } else if t.x != t.x0 {
            t.y + 1
        } else {
            t.y
        };
        let r = MemRect { bp: t.bp, bw: t.bw, psm, x: t.x0, y: t.reported, w: t.w, h: end.saturating_sub(t.reported) };
        t.reported = if t.active { t.y } else { end };
        if !t.active {
            t.psm = None;
        }
        if r.h > 0 {
            b.mem_written(self, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        draws: Vec<(Shape, Vec<Vertex>)>,
        writes: Vec<MemRect>,
    }

    impl Backend for Rec {
        fn draw(&mut self, _gs: &Gs, shape: Shape, v: &[Vertex]) {
            self.draws.push((shape, v.to_vec()));
        }
        fn mem_written(&mut self, _gs: &Gs, r: MemRect) {
            self.writes.push(r);
        }
    }

    fn tag(nloop: u64, eop: bool, pre_prim: Option<u64>, flg: u64, regs: &[u64]) -> u128 {
        let mut lo = nloop | (eop as u64) << 15 | flg << 58 | (regs.len() as u64 & 15) << 60;
        if let Some(p) = pre_prim {
            lo |= 1 << 46 | p << 47;
        }
        let hi = regs.iter().enumerate().fold(0u64, |a, (n, r)| a | r << (4 * n));
        lo as u128 | (hi as u128) << 64
    }

    fn ad(reg: u8, v: u64) -> u128 {
        v as u128 | (reg as u128) << 64
    }

    #[test]
    fn image_transfer_across_packets() {
        let mut gs = Gs::new();
        let mut r = Rec::default();
        // 4x2 CT32 at (8, 4) of buffer bp=0 bw=1.
        gs.write_gif(
            &[
                tag(4, false, None, 0, &[0xe]),
                ad(BITBLTBUF, (0u64 << 32) | 1 << 48),
                ad(TRXPOS, 8u64 << 32 | 4u64 << 48),
                ad(TRXREG, 4 | 2u64 << 32),
                ad(TRXDIR, 0),
                tag(2, true, None, 2, &[]),
            ],
            &mut r,
        );
        let px = |n: u32| 0x1000 + n;
        let q = |a: u32| (px(a) as u128) | (px(a + 1) as u128) << 32 | (px(a + 2) as u128) << 64 | (px(a + 3) as u128) << 96;
        gs.write_gif(&[q(0)], &mut r); // first qword in one call, rest in another
        gs.write_gif(&[q(4)], &mut r);
        for n in 0..8 {
            assert_eq!(gs.vram.read(Psm::Ct32, 0, 1, 8 + n % 4, 4 + n / 4), px(n));
        }
        let rows: u32 = r.writes.iter().map(|w| w.h).sum();
        assert!(rows >= 2 && r.writes.iter().all(|w| w.x == 8 && w.w == 4));
    }

    #[test]
    fn packed_triangle_strip_with_offset() {
        let mut gs = Gs::new();
        let mut r = Rec::default();
        let xyz = |x: u32, y: u32| (x as u128 * 16) | ((y as u128 * 16) << 32) | (7u128 << 64);
        let rgba = |c: u8| c as u128 | (c as u128) << 32 | (c as u128) << 64 | 0x80u128 << 96;
        gs.write_gif(
            &[
                tag(1, false, None, 0, &[0xe]),
                ad(XYOFFSET_1, (100 * 16) | (200u64 * 16) << 32),
                // Gouraud triangle strip (kind 4), four vertices.
                tag(4, true, Some(4 | 1 << 3), 0, &[0x1, 0x5]),
                rgba(10), xyz(100, 200),
                rgba(20), xyz(110, 200),
                rgba(30), xyz(100, 210),
                rgba(40), xyz(110, 210),
            ],
            &mut r,
        );
        assert_eq!(r.draws.len(), 2);
        let (shape, v) = &r.draws[1];
        assert_eq!(*shape, Shape::Triangle);
        assert_eq!((v[0].x, v[0].y, v[2].x, v[2].y), (10.0, 0.0, 10.0, 10.0));
        assert_eq!(v.iter().map(|v| v.rgba[0]).collect::<Vec<_>>(), [20, 30, 40]);
        assert_eq!(v[0].z, 7);
    }

    #[test]
    fn sprite_uses_last_colour() {
        let mut gs = Gs::new();
        let mut r = Rec::default();
        gs.write_gif(
            &[
                tag(5, true, None, 0, &[0xe]),
                ad(PRIM, 6),
                ad(RGBAQ, 0x80_10_20_30),
                ad(XYZ2, 0),
                ad(RGBAQ, 0x80_40_50_60),
                ad(XYZ2, 64 * 16 | (32u64 * 16) << 16),
            ],
            &mut r,
        );
        let (shape, v) = &r.draws[0];
        assert_eq!(*shape, Shape::Sprite);
        assert_eq!(v[0].rgba, [0x60, 0x50, 0x40, 0x80]);
        assert_eq!((v[1].x, v[1].y), (64.0, 32.0));
    }
}
