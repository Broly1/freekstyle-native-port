//! Vector unit register state (VU0, shared with COP2 macro instructions, and
//! VU1), plus the 4-lane helpers the generated code and the microcode
//! interpreter use. Destination masks use bit 3 = x .. bit 0 = w.

use crate::fpu::{fdiv, ps2f};

pub type V4 = [f32; 4];

#[derive(Clone)]
pub struct Vu0 {
    pub vf: [V4; 32],
    pub vi: [u16; 16],
    pub acc: V4,
    pub q: f32,
    pub i: f32,
    pub r: u32,
    /// EFU result register.
    pub p: f32,
    pub clip: u32,
    pub status: u32,
    pub mac: u32,
    pub fbrst: u32,
    pub cmsar0: u32,
    /// Where the last microprogram ended (MSCNT continues here).
    pub tpc: u32,
}

/// VU1 has the same registers as VU0.
pub type VuRegs = Vu0;

impl Vu0 {
    pub fn new() -> Vu0 {
        let mut vf = [[0.0; 4]; 32];
        vf[0] = [0.0, 0.0, 0.0, 1.0];
        Vu0 {
            vf,
            vi: [0; 16],
            acc: [0.0; 4],
            q: 0.0,
            i: 0.0,
            r: 0,
            p: 0.0,
            clip: 0,
            status: 0,
            mac: 0,
            fbrst: 0,
            cmsar0: 0,
            tpc: 0,
        }
    }

    /// Masked write to a VF register; vf0 is hardwired.
    #[inline(always)]
    pub fn vw(&mut self, d: usize, mask: u8, v: V4) {
        if d == 0 {
            return;
        }
        for (n, x) in v.into_iter().enumerate() {
            if mask & (8 >> n) != 0 {
                self.vf[d][n] = x;
            }
        }
    }

    #[inline(always)]
    pub fn accw(&mut self, mask: u8, v: V4) {
        for (n, x) in v.into_iter().enumerate() {
            if mask & (8 >> n) != 0 {
                self.acc[n] = x;
            }
        }
    }

    pub fn vf_q(&self, r: usize) -> u128 {
        let v = self.vf[r];
        (0..4).fold(0u128, |a, n| a | (v[n].to_bits() as u128) << (32 * n))
    }

    pub fn set_vf_q(&mut self, r: usize, q: u128) {
        if r != 0 {
            self.vf[r] = std::array::from_fn(|n| f32::from_bits((q >> (32 * n)) as u32));
        }
    }

    /// Control register read (cfc2). 0-15 are the integer registers.
    pub fn cfc2(&self, r: usize) -> u32 {
        match r {
            0..=15 => self.vi[r] as u32,
            16 => self.status,
            17 => self.mac,
            18 => self.clip,
            20 => self.r,
            21 => self.i.to_bits(),
            22 => self.q.to_bits(),
            27 => self.cmsar0,
            28 => self.fbrst,
            // VPU-STAT: VU0 microprograms run synchronously, so never busy.
            29 => 0,
            _ => 0,
        }
    }

    pub fn ctc2(&mut self, r: usize, v: u32) {
        match r {
            0 => {}
            1..=15 => self.vi[r] = v as u16,
            16 => self.status = v & 0xfc0 | self.status & 0x3f,
            18 => self.clip = v & 0xff_ffff,
            20 => self.r = v & 0x7f_ffff,
            21 => self.i = f32::from_bits(v),
            22 => self.q = f32::from_bits(v),
            27 => self.cmsar0 = v & 0xffff,
            28 => self.fbrst = v & 0x0c0c,
            _ => {}
        }
    }

    /// vclipw: shift the clip flags and add six judgements of fs.xyz
    /// against ±|ft.w|.
    pub fn clip(&mut self, fs: usize, ft: usize) {
        let w = self.vf[ft][3].abs();
        let s = self.vf[fs];
        let mut f = 0;
        for n in 0..3 {
            if s[n] > w {
                f |= 1 << (n * 2);
            }
            if s[n] < -w {
                f |= 2 << (n * 2);
            }
        }
        self.clip = ((self.clip << 6) | f) & 0xff_ffff;
    }
}

impl Default for Vu0 {
    fn default() -> Self {
        Self::new()
    }
}

/// PS2 float clamp without branches: infinities and NaNs become +/-MAX,
/// denormals become signed zero.
#[inline(always)]
fn clamp(x: f32) -> f32 {
    let b = x.to_bits();
    let e = b & 0x7f80_0000;
    let sign = b & 0x8000_0000;
    let b = if e == 0x7f80_0000 { sign | 0x7f7f_ffff } else { b };
    let b = if e == 0 { sign } else { b };
    f32::from_bits(b)
}

#[inline(always)]
pub fn splat(x: f32) -> V4 {
    [x; 4]
}

#[inline(always)]
pub fn add(a: V4, b: V4) -> V4 {
    [clamp(a[0] + b[0]), clamp(a[1] + b[1]), clamp(a[2] + b[2]), clamp(a[3] + b[3])]
}

#[inline(always)]
pub fn sub(a: V4, b: V4) -> V4 {
    [clamp(a[0] - b[0]), clamp(a[1] - b[1]), clamp(a[2] - b[2]), clamp(a[3] - b[3])]
}

#[inline(always)]
pub fn mul(a: V4, b: V4) -> V4 {
    [clamp(a[0] * b[0]), clamp(a[1] * b[1]), clamp(a[2] * b[2]), clamp(a[3] * b[3])]
}

#[inline(always)]
pub fn madd(acc: V4, a: V4, b: V4) -> V4 {
    add(acc, mul(a, b))
}

#[inline(always)]
pub fn msub(acc: V4, a: V4, b: V4) -> V4 {
    sub(acc, mul(a, b))
}

#[inline(always)]
pub fn max(a: V4, b: V4) -> V4 {
    std::array::from_fn(|n| crate::fpu::fmax(a[n], b[n]))
}

#[inline(always)]
pub fn mini(a: V4, b: V4) -> V4 {
    std::array::from_fn(|n| crate::fpu::fmin(a[n], b[n]))
}

#[inline(always)]
pub fn abs(a: V4) -> V4 {
    a.map(f32::abs)
}

/// Outer product terms used by vopmula/vopmsub (xyz only).
#[inline(always)]
pub fn cross(a: V4, b: V4) -> V4 {
    [
        ps2f(a[1] * b[2]),
        ps2f(a[2] * b[0]),
        ps2f(a[0] * b[1]),
        0.0,
    ]
}

#[inline(always)]
pub fn mr32(a: V4) -> V4 {
    [a[1], a[2], a[3], a[0]]
}

/// vitofN: fixed point with N fraction bits to float.
#[inline(always)]
pub fn itof(a: V4, frac: u32) -> V4 {
    let scale = 1.0 / (1u32 << frac) as f32;
    a.map(|x| x.to_bits() as i32 as f32 * scale)
}

/// vftoiN: float to fixed point with N fraction bits, saturating.
#[inline(always)]
pub fn ftoi(a: V4, frac: u32) -> V4 {
    let scale = (1u32 << frac) as f32;
    a.map(|x| f32::from_bits((x * scale) as i32 as u32))
}

#[inline(always)]
pub fn div(a: f32, b: f32) -> f32 {
    fdiv(a, b)
}
