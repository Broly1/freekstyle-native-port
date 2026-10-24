//! GS register addresses and field accessors.

use crate::mem::Psm;

pub const PRIM: u8 = 0x00;
pub const RGBAQ: u8 = 0x01;
pub const ST: u8 = 0x02;
pub const UV: u8 = 0x03;
pub const XYZF2: u8 = 0x04;
pub const XYZ2: u8 = 0x05;
pub const TEX0_1: u8 = 0x06;
pub const TEX0_2: u8 = 0x07;
pub const CLAMP_1: u8 = 0x08;
pub const CLAMP_2: u8 = 0x09;
pub const FOG: u8 = 0x0a;
pub const XYZF3: u8 = 0x0c;
pub const XYZ3: u8 = 0x0d;
pub const TEX1_1: u8 = 0x14;
pub const TEX1_2: u8 = 0x15;
pub const TEX2_1: u8 = 0x16;
pub const TEX2_2: u8 = 0x17;
pub const XYOFFSET_1: u8 = 0x18;
pub const XYOFFSET_2: u8 = 0x19;
pub const PRMODECONT: u8 = 0x1a;
pub const PRMODE: u8 = 0x1b;
pub const TEXCLUT: u8 = 0x1c;
pub const TEXA: u8 = 0x3b;
pub const FOGCOL: u8 = 0x3d;
pub const SCISSOR_1: u8 = 0x40;
pub const SCISSOR_2: u8 = 0x41;
pub const ALPHA_1: u8 = 0x42;
pub const ALPHA_2: u8 = 0x43;
pub const COLCLAMP: u8 = 0x46;
pub const TEST_1: u8 = 0x47;
pub const TEST_2: u8 = 0x48;
pub const FBA_1: u8 = 0x4a;
pub const FBA_2: u8 = 0x4b;
pub const FRAME_1: u8 = 0x4c;
pub const FRAME_2: u8 = 0x4d;
pub const ZBUF_1: u8 = 0x4e;
pub const ZBUF_2: u8 = 0x4f;
pub const BITBLTBUF: u8 = 0x50;
pub const TRXPOS: u8 = 0x51;
pub const TRXREG: u8 = 0x52;
pub const TRXDIR: u8 = 0x53;
pub const HWREG: u8 = 0x54;
pub const SIGNAL: u8 = 0x60;
pub const FINISH: u8 = 0x61;
pub const LABEL: u8 = 0x62;

fn bits(v: u64, lo: u32, n: u32) -> u32 {
    ((v >> lo) & ((1u64 << n) - 1)) as u32
}

/// PRIM / PRMODE attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Prim(pub u64);

impl Prim {
    pub fn kind(self) -> u32 {
        bits(self.0, 0, 3)
    }
    pub fn gouraud(self) -> bool {
        self.0 & 1 << 3 != 0
    }
    pub fn textured(self) -> bool {
        self.0 & 1 << 4 != 0
    }
    pub fn fog(self) -> bool {
        self.0 & 1 << 5 != 0
    }
    pub fn blend(self) -> bool {
        self.0 & 1 << 6 != 0
    }
    /// Texture coordinates come from UV (texel units) instead of STQ.
    pub fn fst(self) -> bool {
        self.0 & 1 << 8 != 0
    }
    pub fn context(self) -> usize {
        bits(self.0, 9, 1) as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tex0(pub u64);

impl Tex0 {
    pub fn tbp0(self) -> u32 {
        bits(self.0, 0, 14)
    }
    pub fn tbw(self) -> u32 {
        bits(self.0, 14, 6)
    }
    pub fn psm(self) -> Psm {
        Psm::from_bits(bits(self.0, 20, 6) as u64)
    }
    pub fn tw(self) -> u32 {
        bits(self.0, 26, 4).min(10)
    }
    pub fn th(self) -> u32 {
        bits(self.0, 30, 4).min(10)
    }
    /// Texture alpha is used (TCC).
    pub fn tcc(self) -> bool {
        self.0 & 1 << 34 != 0
    }
    pub fn tfx(self) -> u32 {
        bits(self.0, 35, 2)
    }
    pub fn cbp(self) -> u32 {
        bits(self.0, 37, 14)
    }
    pub fn cpsm(self) -> Psm {
        Psm::from_bits(bits(self.0, 51, 4) as u64)
    }
    /// CLUT storage mode 2 (linear, via TEXCLUT).
    pub fn csm2(self) -> bool {
        self.0 & 1 << 55 != 0
    }
    pub fn csa(self) -> u32 {
        bits(self.0, 56, 5)
    }
    pub fn cld(self) -> u32 {
        bits(self.0, 61, 3)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame(pub u64);

impl Frame {
    /// Base pointer in blocks (FBP is in pages).
    pub fn bp(self) -> u32 {
        bits(self.0, 0, 9) * 32
    }
    pub fn bw(self) -> u32 {
        bits(self.0, 16, 6)
    }
    pub fn psm(self) -> Psm {
        Psm::from_bits(bits(self.0, 24, 6) as u64)
    }
    /// Bits that are *not* written.
    pub fn mask(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Zbuf(pub u64);

impl Zbuf {
    pub fn bp(self) -> u32 {
        bits(self.0, 0, 9) * 32
    }
    pub fn psm(self) -> Psm {
        Psm::from_bits(0x30 | bits(self.0, 24, 4) as u64)
    }
    /// Depth writes disabled.
    pub fn masked(self) -> bool {
        self.0 & 1 << 32 != 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Alpha(pub u64);

impl Alpha {
    /// (A, B, C, D) of (A - B) * C >> 7 + D.
    pub fn abcd(self) -> (u32, u32, u32, u32) {
        (bits(self.0, 0, 2), bits(self.0, 2, 2), bits(self.0, 4, 2), bits(self.0, 6, 2))
    }
    pub fn fix(self) -> u32 {
        bits(self.0, 32, 8)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Test(pub u64);

impl Test {
    pub fn alpha_test(self) -> bool {
        self.0 & 1 != 0
    }
    pub fn atst(self) -> u32 {
        bits(self.0, 1, 3)
    }
    pub fn aref(self) -> u32 {
        bits(self.0, 4, 8)
    }
    pub fn afail(self) -> u32 {
        bits(self.0, 12, 2)
    }
    pub fn date(self) -> bool {
        self.0 & 1 << 14 != 0
    }
    pub fn datm(self) -> bool {
        self.0 & 1 << 15 != 0
    }
    pub fn zte(self) -> bool {
        self.0 & 1 << 16 != 0
    }
    pub fn ztst(self) -> u32 {
        bits(self.0, 17, 2)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scissor(pub u64);

impl Scissor {
    pub fn rect(self) -> (u32, u32, u32, u32) {
        (bits(self.0, 0, 11), bits(self.0, 32, 11), bits(self.0, 16, 11), bits(self.0, 48, 11))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tex1(pub u64);

impl Tex1 {
    pub fn mag_linear(self) -> bool {
        self.0 & 1 << 5 != 0
    }
    pub fn min_linear(self) -> bool {
        // MMIN 1 (LINEAR) and the mip modes 3 and 5 sample bilinearly.
        matches!(bits(self.0, 6, 3), 1 | 3 | 5)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Clamp(pub u64);

impl Clamp {
    pub fn wms(self) -> u32 {
        bits(self.0, 0, 2)
    }
    pub fn wmt(self) -> u32 {
        bits(self.0, 2, 2)
    }
    pub fn minu(self) -> u32 {
        bits(self.0, 4, 10)
    }
    pub fn maxu(self) -> u32 {
        bits(self.0, 14, 10)
    }
    pub fn minv(self) -> u32 {
        bits(self.0, 24, 10)
    }
    pub fn maxv(self) -> u32 {
        bits(self.0, 34, 10)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Texa(pub u64);

impl Texa {
    pub fn ta0(self) -> u32 {
        bits(self.0, 0, 8)
    }
    /// Alpha expansion: RGB of zero gives alpha zero.
    pub fn aem(self) -> bool {
        self.0 & 1 << 15 != 0
    }
    pub fn ta1(self) -> u32 {
        bits(self.0, 32, 8)
    }
}

/// One drawing context's registers (PRIM.CTXT selects 1 or 2).
#[derive(Clone, Copy, Debug, Default)]
pub struct Context {
    pub tex0: Tex0,
    pub tex1: Tex1,
    pub clamp: Clamp,
    pub xyoffset: u64,
    pub scissor: Scissor,
    pub alpha: Alpha,
    pub test: Test,
    pub fba: bool,
    pub frame: Frame,
    pub zbuf: Zbuf,
}

impl Context {
    pub fn offset(&self) -> (f32, f32) {
        (bits(self.xyoffset, 0, 16) as f32 / 16.0, bits(self.xyoffset, 32, 16) as f32 / 16.0)
    }
}
