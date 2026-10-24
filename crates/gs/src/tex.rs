//! Texture decoding: GS local memory to RGBA8 for the host renderer.
//!
//! Alpha keeps the GS meaning (0x80 is fully opaque); the renderer scales it.

use crate::mem::{Psm, Vram};
use crate::regs::{Tex0, Texa};

/// Expand a 16-bit A1B5G5R5 colour.
pub fn rgba16(v: u32, texa: Texa) -> u32 {
    let (r, g, b) = ((v & 31) << 3, ((v >> 5) & 31) << 3, ((v >> 10) & 31) << 3);
    let rgb = r | g << 8 | b << 16;
    let a = if v & 0x8000 != 0 {
        texa.ta1()
    } else if texa.aem() && v & 0x7fff == 0 {
        0
    } else {
        texa.ta0()
    };
    rgb | a << 24
}

/// Expand a 24-bit colour (alpha from TEXA).
pub fn rgba24(v: u32, texa: Texa) -> u32 {
    let a = if texa.aem() && v & 0xff_ffff == 0 { 0 } else { texa.ta0() };
    (v & 0xff_ffff) | a << 24
}

/// Decode a whole texture level into RGBA8 (R in the low byte).
pub fn decode(vram: &Vram, t: Tex0, texa: Texa, clut: &[u32; 256], clut_psm: Psm) -> (u32, u32, Vec<u32>) {
    let (w, h) = (1u32 << t.tw(), 1u32 << t.th());
    let (bp, bw, psm) = (t.tbp0(), t.tbw(), t.psm());
    let csa = if matches!(psm, Psm::T4 | Psm::T4hl | Psm::T4hh) { t.csa() as usize * 16 } else { 0 };
    let lookup = |i: u32| {
        let c = clut[(csa + i as usize) % 256];
        if clut_psm == Psm::Ct32 { c } else { rgba16(c, texa) }
    };
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let raw = vram.read(psm, bp, bw, x, y);
            out.push(match psm {
                Psm::Ct32 | Psm::Z32 => raw,
                Psm::Ct24 | Psm::Z24 => rgba24(raw, texa),
                Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => rgba16(raw, texa),
                _ => lookup(raw),
            });
        }
    }
    (w, h, out)
}

/// Decode a rectangle of a frame buffer (used to seed render targets from
/// memory written by transfers).
pub fn decode_rect(vram: &Vram, psm: Psm, bp: u32, bw: u32, x0: u32, y0: u32, w: u32, h: u32, texa: Texa) -> Vec<u32> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let raw = vram.read(psm, bp, bw, x & 2047, y & 2047);
            out.push(match psm.bpp() {
                32 if psm == Psm::Ct24 || psm == Psm::Z24 => rgba24(raw, texa),
                32 => raw,
                16 => rgba16(raw, texa),
                _ => raw * 0x0101_0101,
            });
        }
    }
    out
}
