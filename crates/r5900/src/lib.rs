//! Decoder for the PlayStation 2 Emotion Engine CPU (MIPS R5900).
//!
//! Covers the MIPS III subset the EE implements, the EE-specific additions
//! (three-operand MULT, LQ/SQ, the MMI 128-bit SIMD group, pipeline-1 HI/LO,
//! SA register), the COP0 subset, the single-precision COP1 FPU, and COP2 in
//! VU0 macro mode.
//!
//! Decoding is lenient in the same way the hardware is: fields that should be
//! zero are not checked. [`Insn::is_canonical`] reports whether those fields
//! are clean, which helps tell code from data during analysis.

mod display;

pub use display::{gpr_name, Disasm};

macro_rules! mnemonics {
    ($($v:ident = $s:literal, $f:ident;)*) => {
        /// Every instruction the decoder can produce.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Mnemonic { $($v),* }

        impl Mnemonic {
            pub const ALL: &'static [Mnemonic] = &[$(Mnemonic::$v),*];

            /// Assembler name. VU broadcast forms append the field letter
            /// at display time (`vadd` + `x` = `vaddx`).
            pub fn name(self) -> &'static str {
                match self { $(Mnemonic::$v => $s),* }
            }

            /// Inlined so that callers with a constant mnemonic (recompiled
            /// VU code) fold it away.
            #[inline(always)]
            pub fn format(self) -> Format {
                match self { $(Mnemonic::$v => Format::$f),* }
            }
        }
    };
}

/// Operand layout, used for display and by tools that need to know which
/// fields an instruction reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Raw,
    None,
    RdRsRt,
    RdRtRs,
    RdRtSa,
    RdRs,
    RdRt,
    Rd,
    Rs,
    RsRt,
    RtRsImm,
    RtRsUimm,
    RtUimm,
    RsRtOff,
    RsOff,
    RsImm,
    Off,
    Target,
    Mem,
    FMem,
    VMem,
    CacheOp,
    Sync,
    RtCop0,
    RtFpr,
    RtFcr,
    RtVf,
    RtVi,
    FdFsFt,
    FdFs,
    FdFt,
    FsFt,
    VFdFsFt,
    VFdFsFtBc,
    VAccFsFt,
    VAccFsFtBc,
    VFdFsQ,
    VFdFsI,
    VAccFsQ,
    VAccFsI,
    VFtFs,
    VIdIsIt,
    VItIsImm5,
    VCallms,
    VDiv,
    VSqrt,
    VClip,
    VLqi,
    VSqi,
    VLqd,
    VSqd,
    VMtir,
    VMfir,
    VIlwr,
    VIswr,
    VRnext,
    VRinit,
}

mnemonics! {
    Invalid = "invalid", Raw;

    // SPECIAL
    Sll = "sll", RdRtSa;
    Srl = "srl", RdRtSa;
    Sra = "sra", RdRtSa;
    Sllv = "sllv", RdRtRs;
    Srlv = "srlv", RdRtRs;
    Srav = "srav", RdRtRs;
    Jr = "jr", Rs;
    Jalr = "jalr", RdRs;
    Movz = "movz", RdRsRt;
    Movn = "movn", RdRsRt;
    Syscall = "syscall", None;
    Break = "break", None;
    Sync = "sync", Sync;
    Mfhi = "mfhi", Rd;
    Mthi = "mthi", Rs;
    Mflo = "mflo", Rd;
    Mtlo = "mtlo", Rs;
    Dsllv = "dsllv", RdRtRs;
    Dsrlv = "dsrlv", RdRtRs;
    Dsrav = "dsrav", RdRtRs;
    Mult = "mult", RdRsRt;
    Multu = "multu", RdRsRt;
    Div = "div", RsRt;
    Divu = "divu", RsRt;
    Add = "add", RdRsRt;
    Addu = "addu", RdRsRt;
    Sub = "sub", RdRsRt;
    Subu = "subu", RdRsRt;
    And = "and", RdRsRt;
    Or = "or", RdRsRt;
    Xor = "xor", RdRsRt;
    Nor = "nor", RdRsRt;
    Mfsa = "mfsa", Rd;
    Mtsa = "mtsa", Rs;
    Slt = "slt", RdRsRt;
    Sltu = "sltu", RdRsRt;
    Dadd = "dadd", RdRsRt;
    Daddu = "daddu", RdRsRt;
    Dsub = "dsub", RdRsRt;
    Dsubu = "dsubu", RdRsRt;
    Tge = "tge", RsRt;
    Tgeu = "tgeu", RsRt;
    Tlt = "tlt", RsRt;
    Tltu = "tltu", RsRt;
    Teq = "teq", RsRt;
    Tne = "tne", RsRt;
    Dsll = "dsll", RdRtSa;
    Dsrl = "dsrl", RdRtSa;
    Dsra = "dsra", RdRtSa;
    Dsll32 = "dsll32", RdRtSa;
    Dsrl32 = "dsrl32", RdRtSa;
    Dsra32 = "dsra32", RdRtSa;

    // REGIMM
    Bltz = "bltz", RsOff;
    Bgez = "bgez", RsOff;
    Bltzl = "bltzl", RsOff;
    Bgezl = "bgezl", RsOff;
    Tgei = "tgei", RsImm;
    Tgeiu = "tgeiu", RsImm;
    Tlti = "tlti", RsImm;
    Tltiu = "tltiu", RsImm;
    Teqi = "teqi", RsImm;
    Tnei = "tnei", RsImm;
    Bltzal = "bltzal", RsOff;
    Bgezal = "bgezal", RsOff;
    Bltzall = "bltzall", RsOff;
    Bgezall = "bgezall", RsOff;
    Mtsab = "mtsab", RsImm;
    Mtsah = "mtsah", RsImm;

    // Primary opcodes
    J = "j", Target;
    Jal = "jal", Target;
    Beq = "beq", RsRtOff;
    Bne = "bne", RsRtOff;
    Blez = "blez", RsOff;
    Bgtz = "bgtz", RsOff;
    Addi = "addi", RtRsImm;
    Addiu = "addiu", RtRsImm;
    Slti = "slti", RtRsImm;
    Sltiu = "sltiu", RtRsImm;
    Andi = "andi", RtRsUimm;
    Ori = "ori", RtRsUimm;
    Xori = "xori", RtRsUimm;
    Lui = "lui", RtUimm;
    Beql = "beql", RsRtOff;
    Bnel = "bnel", RsRtOff;
    Blezl = "blezl", RsOff;
    Bgtzl = "bgtzl", RsOff;
    Daddi = "daddi", RtRsImm;
    Daddiu = "daddiu", RtRsImm;
    Ldl = "ldl", Mem;
    Ldr = "ldr", Mem;
    Lq = "lq", Mem;
    Sq = "sq", Mem;
    Lb = "lb", Mem;
    Lh = "lh", Mem;
    Lwl = "lwl", Mem;
    Lw = "lw", Mem;
    Lbu = "lbu", Mem;
    Lhu = "lhu", Mem;
    Lwr = "lwr", Mem;
    Lwu = "lwu", Mem;
    Sb = "sb", Mem;
    Sh = "sh", Mem;
    Swl = "swl", Mem;
    Sw = "sw", Mem;
    Sdl = "sdl", Mem;
    Sdr = "sdr", Mem;
    Swr = "swr", Mem;
    Cache = "cache", CacheOp;
    Lwc1 = "lwc1", FMem;
    Pref = "pref", CacheOp;
    Lqc2 = "lqc2", VMem;
    Ld = "ld", Mem;
    Swc1 = "swc1", FMem;
    Sqc2 = "sqc2", VMem;
    Sd = "sd", Mem;

    // MMI
    Madd = "madd", RdRsRt;
    Maddu = "maddu", RdRsRt;
    Plzcw = "plzcw", RdRs;
    Mfhi1 = "mfhi1", Rd;
    Mthi1 = "mthi1", Rs;
    Mflo1 = "mflo1", Rd;
    Mtlo1 = "mtlo1", Rs;
    Mult1 = "mult1", RdRsRt;
    Multu1 = "multu1", RdRsRt;
    Div1 = "div1", RsRt;
    Divu1 = "divu1", RsRt;
    Madd1 = "madd1", RdRsRt;
    Maddu1 = "maddu1", RdRsRt;
    PmfhlLw = "pmfhl.lw", Rd;
    PmfhlUw = "pmfhl.uw", Rd;
    PmfhlSlw = "pmfhl.slw", Rd;
    PmfhlLh = "pmfhl.lh", Rd;
    PmfhlSh = "pmfhl.sh", Rd;
    PmthlLw = "pmthl.lw", Rs;
    Psllh = "psllh", RdRtSa;
    Psrlh = "psrlh", RdRtSa;
    Psrah = "psrah", RdRtSa;
    Psllw = "psllw", RdRtSa;
    Psrlw = "psrlw", RdRtSa;
    Psraw = "psraw", RdRtSa;
    // MMI0
    Paddw = "paddw", RdRsRt;
    Psubw = "psubw", RdRsRt;
    Pcgtw = "pcgtw", RdRsRt;
    Pmaxw = "pmaxw", RdRsRt;
    Paddh = "paddh", RdRsRt;
    Psubh = "psubh", RdRsRt;
    Pcgth = "pcgth", RdRsRt;
    Pmaxh = "pmaxh", RdRsRt;
    Paddb = "paddb", RdRsRt;
    Psubb = "psubb", RdRsRt;
    Pcgtb = "pcgtb", RdRsRt;
    Paddsw = "paddsw", RdRsRt;
    Psubsw = "psubsw", RdRsRt;
    Pextlw = "pextlw", RdRsRt;
    Ppacw = "ppacw", RdRsRt;
    Paddsh = "paddsh", RdRsRt;
    Psubsh = "psubsh", RdRsRt;
    Pextlh = "pextlh", RdRsRt;
    Ppach = "ppach", RdRsRt;
    Paddsb = "paddsb", RdRsRt;
    Psubsb = "psubsb", RdRsRt;
    Pextlb = "pextlb", RdRsRt;
    Ppacb = "ppacb", RdRsRt;
    Pext5 = "pext5", RdRt;
    Ppac5 = "ppac5", RdRt;
    // MMI1
    Pabsw = "pabsw", RdRt;
    Pceqw = "pceqw", RdRsRt;
    Pminw = "pminw", RdRsRt;
    Padsbh = "padsbh", RdRsRt;
    Pabsh = "pabsh", RdRt;
    Pceqh = "pceqh", RdRsRt;
    Pminh = "pminh", RdRsRt;
    Pceqb = "pceqb", RdRsRt;
    Padduw = "padduw", RdRsRt;
    Psubuw = "psubuw", RdRsRt;
    Pextuw = "pextuw", RdRsRt;
    Padduh = "padduh", RdRsRt;
    Psubuh = "psubuh", RdRsRt;
    Pextuh = "pextuh", RdRsRt;
    Paddub = "paddub", RdRsRt;
    Psubub = "psubub", RdRsRt;
    Pextub = "pextub", RdRsRt;
    Qfsrv = "qfsrv", RdRsRt;
    // MMI2
    Pmaddw = "pmaddw", RdRsRt;
    Psllvw = "psllvw", RdRtRs;
    Psrlvw = "psrlvw", RdRtRs;
    Pmsubw = "pmsubw", RdRsRt;
    Pmfhi = "pmfhi", Rd;
    Pmflo = "pmflo", Rd;
    Pinth = "pinth", RdRsRt;
    Pmultw = "pmultw", RdRsRt;
    Pdivw = "pdivw", RsRt;
    Pcpyld = "pcpyld", RdRsRt;
    Pmaddh = "pmaddh", RdRsRt;
    Phmadh = "phmadh", RdRsRt;
    Pand = "pand", RdRsRt;
    Pxor = "pxor", RdRsRt;
    Pmsubh = "pmsubh", RdRsRt;
    Phmsbh = "phmsbh", RdRsRt;
    Pexeh = "pexeh", RdRt;
    Prevh = "prevh", RdRt;
    Pmulth = "pmulth", RdRsRt;
    Pdivbw = "pdivbw", RsRt;
    Pexew = "pexew", RdRt;
    Prot3w = "prot3w", RdRt;
    // MMI3
    Pmadduw = "pmadduw", RdRsRt;
    Psravw = "psravw", RdRtRs;
    Pmthi = "pmthi", Rs;
    Pmtlo = "pmtlo", Rs;
    Pinteh = "pinteh", RdRsRt;
    Pmultuw = "pmultuw", RdRsRt;
    Pdivuw = "pdivuw", RsRt;
    Pcpyud = "pcpyud", RdRsRt;
    Por = "por", RdRsRt;
    Pnor = "pnor", RdRsRt;
    Pexch = "pexch", RdRt;
    Pcpyh = "pcpyh", RdRt;
    Pexcw = "pexcw", RdRt;

    // COP0
    Mfc0 = "mfc0", RtCop0;
    Mtc0 = "mtc0", RtCop0;
    Bc0f = "bc0f", Off;
    Bc0t = "bc0t", Off;
    Bc0fl = "bc0fl", Off;
    Bc0tl = "bc0tl", Off;
    Tlbr = "tlbr", None;
    Tlbwi = "tlbwi", None;
    Tlbwr = "tlbwr", None;
    Tlbp = "tlbp", None;
    Eret = "eret", None;
    Ei = "ei", None;
    Di = "di", None;

    // COP1
    Mfc1 = "mfc1", RtFpr;
    Cfc1 = "cfc1", RtFcr;
    Mtc1 = "mtc1", RtFpr;
    Ctc1 = "ctc1", RtFcr;
    Bc1f = "bc1f", Off;
    Bc1t = "bc1t", Off;
    Bc1fl = "bc1fl", Off;
    Bc1tl = "bc1tl", Off;
    AddS = "add.s", FdFsFt;
    SubS = "sub.s", FdFsFt;
    MulS = "mul.s", FdFsFt;
    DivS = "div.s", FdFsFt;
    SqrtS = "sqrt.s", FdFt;
    AbsS = "abs.s", FdFs;
    MovS = "mov.s", FdFs;
    NegS = "neg.s", FdFs;
    RsqrtS = "rsqrt.s", FdFsFt;
    AddaS = "adda.s", FsFt;
    SubaS = "suba.s", FsFt;
    MulaS = "mula.s", FsFt;
    MaddS = "madd.s", FdFsFt;
    MsubS = "msub.s", FdFsFt;
    MaddaS = "madda.s", FsFt;
    MsubaS = "msuba.s", FsFt;
    CvtWS = "cvt.w.s", FdFs;
    MaxS = "max.s", FdFsFt;
    MinS = "min.s", FdFsFt;
    CFS = "c.f.s", FsFt;
    CEqS = "c.eq.s", FsFt;
    CLtS = "c.lt.s", FsFt;
    CLeS = "c.le.s", FsFt;
    CvtSW = "cvt.s.w", FdFs;

    // COP2 transfers and branches
    Qmfc2 = "qmfc2", RtVf;
    Cfc2 = "cfc2", RtVi;
    Qmtc2 = "qmtc2", RtVf;
    Ctc2 = "ctc2", RtVi;
    Bc2f = "bc2f", Off;
    Bc2t = "bc2t", Off;
    Bc2fl = "bc2fl", Off;
    Bc2tl = "bc2tl", Off;

    // VU0 macro, special1
    VaddBc = "vadd", VFdFsFtBc;
    VsubBc = "vsub", VFdFsFtBc;
    VmaddBc = "vmadd", VFdFsFtBc;
    VmsubBc = "vmsub", VFdFsFtBc;
    VmaxBc = "vmax", VFdFsFtBc;
    VminiBc = "vmini", VFdFsFtBc;
    VmulBc = "vmul", VFdFsFtBc;
    Vmulq = "vmulq", VFdFsQ;
    Vmaxi = "vmaxi", VFdFsI;
    Vmuli = "vmuli", VFdFsI;
    Vminii = "vminii", VFdFsI;
    Vaddq = "vaddq", VFdFsQ;
    Vmaddq = "vmaddq", VFdFsQ;
    Vaddi = "vaddi", VFdFsI;
    Vmaddi = "vmaddi", VFdFsI;
    Vsubq = "vsubq", VFdFsQ;
    Vmsubq = "vmsubq", VFdFsQ;
    Vsubi = "vsubi", VFdFsI;
    Vmsubi = "vmsubi", VFdFsI;
    Vadd = "vadd", VFdFsFt;
    Vmadd = "vmadd", VFdFsFt;
    Vmul = "vmul", VFdFsFt;
    Vmax = "vmax", VFdFsFt;
    Vsub = "vsub", VFdFsFt;
    Vmsub = "vmsub", VFdFsFt;
    Vopmsub = "vopmsub", VFdFsFt;
    Vmini = "vmini", VFdFsFt;
    Viadd = "viadd", VIdIsIt;
    Visub = "visub", VIdIsIt;
    Viaddi = "viaddi", VItIsImm5;
    Viand = "viand", VIdIsIt;
    Vior = "vior", VIdIsIt;
    Vcallms = "vcallms", VCallms;
    Vcallmsr = "vcallmsr", None;

    // VU0 macro, special2
    VaddaBc = "vadda", VAccFsFtBc;
    VsubaBc = "vsuba", VAccFsFtBc;
    VmaddaBc = "vmadda", VAccFsFtBc;
    VmsubaBc = "vmsuba", VAccFsFtBc;
    Vitof0 = "vitof0", VFtFs;
    Vitof4 = "vitof4", VFtFs;
    Vitof12 = "vitof12", VFtFs;
    Vitof15 = "vitof15", VFtFs;
    Vftoi0 = "vftoi0", VFtFs;
    Vftoi4 = "vftoi4", VFtFs;
    Vftoi12 = "vftoi12", VFtFs;
    Vftoi15 = "vftoi15", VFtFs;
    VmulaBc = "vmula", VAccFsFtBc;
    Vmulaq = "vmulaq", VAccFsQ;
    Vabs = "vabs", VFtFs;
    Vmulai = "vmulai", VAccFsI;
    Vclipw = "vclipw", VClip;
    Vaddaq = "vaddaq", VAccFsQ;
    Vmaddaq = "vmaddaq", VAccFsQ;
    Vaddai = "vaddai", VAccFsI;
    Vmaddai = "vmaddai", VAccFsI;
    Vsubaq = "vsubaq", VAccFsQ;
    Vmsubaq = "vmsubaq", VAccFsQ;
    Vsubai = "vsubai", VAccFsI;
    Vmsubai = "vmsubai", VAccFsI;
    Vadda = "vadda", VAccFsFt;
    Vmadda = "vmadda", VAccFsFt;
    Vmula = "vmula", VAccFsFt;
    Vsuba = "vsuba", VAccFsFt;
    Vmsuba = "vmsuba", VAccFsFt;
    Vopmula = "vopmula", VAccFsFt;
    Vnop = "vnop", None;
    Vmove = "vmove", VFtFs;
    Vmr32 = "vmr32", VFtFs;
    Vlqi = "vlqi", VLqi;
    Vsqi = "vsqi", VSqi;
    Vlqd = "vlqd", VLqd;
    Vsqd = "vsqd", VSqd;
    Vdiv = "vdiv", VDiv;
    Vsqrt = "vsqrt", VSqrt;
    Vrsqrt = "vrsqrt", VDiv;
    Vwaitq = "vwaitq", None;
    Vmtir = "vmtir", VMtir;
    Vmfir = "vmfir", VMfir;
    Vilwr = "vilwr", VIlwr;
    Viswr = "viswr", VIswr;
    Vrnext = "vrnext", VRnext;
    Vrget = "vrget", VRnext;
    Vrinit = "vrinit", VRinit;
    Vrxor = "vrxor", VRinit;
}

/// One decoded instruction: the raw word plus its mnemonic. Field accessors
/// read straight from the word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    pub raw: u32,
    pub m: Mnemonic,
}

impl Insn {
    pub fn op(&self) -> u32 {
        self.raw >> 26
    }
    pub fn rs(&self) -> u8 {
        ((self.raw >> 21) & 31) as u8
    }
    pub fn rt(&self) -> u8 {
        ((self.raw >> 16) & 31) as u8
    }
    pub fn rd(&self) -> u8 {
        ((self.raw >> 11) & 31) as u8
    }
    pub fn sa(&self) -> u8 {
        ((self.raw >> 6) & 31) as u8
    }
    pub fn funct(&self) -> u32 {
        self.raw & 63
    }
    pub fn imm(&self) -> i16 {
        self.raw as u16 as i16
    }
    pub fn uimm(&self) -> u16 {
        self.raw as u16
    }

    // FPU and VU register fields share the rt/rd/sa bit positions.
    pub fn ft(&self) -> u8 {
        self.rt()
    }
    pub fn fs(&self) -> u8 {
        self.rd()
    }
    pub fn fd(&self) -> u8 {
        self.sa()
    }

    /// VU destination mask, bit 3 = x .. bit 0 = w.
    pub fn dest(&self) -> u8 {
        ((self.raw >> 21) & 15) as u8
    }
    /// VU broadcast field: 0 = x, 1 = y, 2 = z, 3 = w.
    pub fn bc(&self) -> u8 {
        (self.raw & 3) as u8
    }
    /// VU single-field selectors used by vdiv/vsqrt/vrsqrt/vmtir/vrinit.
    pub fn fsf(&self) -> u8 {
        ((self.raw >> 21) & 3) as u8
    }
    pub fn ftf(&self) -> u8 {
        ((self.raw >> 23) & 3) as u8
    }
    /// viaddi immediate, sign-extended from 5 bits.
    pub fn imm5(&self) -> i8 {
        ((self.sa() << 3) as i8) >> 3
    }
    /// vcallms microprogram address in bytes.
    pub fn callms_addr(&self) -> u32 {
        ((self.raw >> 6) & 0x7fff) * 8
    }
    /// COP2 transfer interlock bit (qmfc2.i / qmtc2.i / cfc2.i / ctc2.i).
    pub fn interlock(&self) -> bool {
        self.raw & 1 != 0
    }

    pub fn is_branch(&self) -> bool {
        use Mnemonic::*;
        matches!(
            self.m,
            Beq | Bne | Blez | Bgtz | Beql | Bnel | Blezl | Bgtzl | Bltz | Bgez | Bltzl
                | Bgezl | Bltzal | Bgezal | Bltzall | Bgezall | Bc0f | Bc0t | Bc0fl
                | Bc0tl | Bc1f | Bc1t | Bc1fl | Bc1tl | Bc2f | Bc2t | Bc2fl | Bc2tl
        )
    }

    /// Branch-likely forms nullify the delay slot when not taken.
    pub fn is_likely(&self) -> bool {
        use Mnemonic::*;
        matches!(
            self.m,
            Beql | Bnel | Blezl | Bgtzl | Bltzl | Bgezl | Bltzall | Bgezall | Bc0fl
                | Bc0tl | Bc1fl | Bc1tl | Bc2fl | Bc2tl
        )
    }

    /// True for every instruction followed by a delay slot.
    pub fn has_delay_slot(&self) -> bool {
        use Mnemonic::*;
        self.is_branch() || matches!(self.m, J | Jal | Jr | Jalr)
    }

    /// Unconditional branch encoded as `beq $x, $x` or `bgez $zero`.
    pub fn is_unconditional_branch(&self) -> bool {
        use Mnemonic::*;
        match self.m {
            Beq | Beql => self.rs() == self.rt(),
            Bgez | Bgezl | Bgezal | Bgezall => self.rs() == 0,
            _ => false,
        }
    }

    /// Target of a PC-relative branch at `pc`.
    pub fn branch_target(&self, pc: u32) -> Option<u32> {
        self.is_branch()
            .then(|| pc.wrapping_add(4).wrapping_add(((self.imm() as i32) << 2) as u32))
    }

    /// Target of a `j` or `jal` at `pc`.
    pub fn jump_target(&self, pc: u32) -> Option<u32> {
        matches!(self.m, Mnemonic::J | Mnemonic::Jal)
            .then(|| (pc.wrapping_add(4) & 0xf000_0000) | ((self.raw & 0x03ff_ffff) << 2))
    }

    /// Whether fields the hardware ignores are zero, as an assembler would
    /// emit them. Non-canonical words in "code" are usually data.
    pub fn is_canonical(&self) -> bool {
        use Format::*;
        let (rs, rt, rd, sa) = (self.rs(), self.rt(), self.rd(), self.sa());
        // MMI sub-opcodes live in the sa field, so it is not "unused" there.
        let mmi = self.op() == 28;
        match self.m.format() {
            Raw => false,
            RdRtSa => rs == 0,
            RdRsRt | RdRtRs => mmi || sa == 0,
            Rd => rs == 0 && rt == 0 && (mmi || sa == 0),
            Rs => rt == 0 && rd == 0 && (mmi || sa == 0),
            RdRs => rt == 0 && (mmi || sa == 0),
            RdRt => rs == 0 && (mmi || sa == 0),
            RsRt => rd == 0 && (mmi || sa == 0),
            RtUimm => rs == 0,
            Sync => rs == 0 && rt == 0 && rd == 0,
            RtCop0 | RtFpr | RtFcr => self.raw & 0x7ff == 0,
            _ => true,
        }
    }
}

/// Decode one 32-bit instruction word.
pub fn decode(raw: u32) -> Insn {
    use Mnemonic::*;
    let rt = (raw >> 16) & 31;
    let m = match raw >> 26 {
        0 => special(raw),
        1 => regimm(rt),
        2 => J,
        3 => Jal,
        4 => Beq,
        5 => Bne,
        6 => Blez,
        7 => Bgtz,
        8 => Addi,
        9 => Addiu,
        10 => Slti,
        11 => Sltiu,
        12 => Andi,
        13 => Ori,
        14 => Xori,
        15 => Lui,
        16 => cop0(raw),
        17 => cop1(raw),
        18 => cop2(raw),
        20 => Beql,
        21 => Bnel,
        22 => Blezl,
        23 => Bgtzl,
        24 => Daddi,
        25 => Daddiu,
        26 => Ldl,
        27 => Ldr,
        28 => mmi(raw),
        30 => Lq,
        31 => Sq,
        32 => Lb,
        33 => Lh,
        34 => Lwl,
        35 => Lw,
        36 => Lbu,
        37 => Lhu,
        38 => Lwr,
        39 => Lwu,
        40 => Sb,
        41 => Sh,
        42 => Swl,
        43 => Sw,
        44 => Sdl,
        45 => Sdr,
        46 => Swr,
        47 => Cache,
        49 => Lwc1,
        51 => Pref,
        54 => Lqc2,
        55 => Ld,
        57 => Swc1,
        62 => Sqc2,
        63 => Sd,
        _ => Invalid,
    };
    Insn { raw, m }
}

fn special(raw: u32) -> Mnemonic {
    use Mnemonic::*;
    match raw & 63 {
        0 => Sll,
        2 => Srl,
        3 => Sra,
        4 => Sllv,
        6 => Srlv,
        7 => Srav,
        8 => Jr,
        9 => Jalr,
        10 => Movz,
        11 => Movn,
        12 => Syscall,
        13 => Break,
        15 => Sync,
        16 => Mfhi,
        17 => Mthi,
        18 => Mflo,
        19 => Mtlo,
        20 => Dsllv,
        22 => Dsrlv,
        23 => Dsrav,
        24 => Mult,
        25 => Multu,
        26 => Div,
        27 => Divu,
        32 => Add,
        33 => Addu,
        34 => Sub,
        35 => Subu,
        36 => And,
        37 => Or,
        38 => Xor,
        39 => Nor,
        40 => Mfsa,
        41 => Mtsa,
        42 => Slt,
        43 => Sltu,
        44 => Dadd,
        45 => Daddu,
        46 => Dsub,
        47 => Dsubu,
        48 => Tge,
        49 => Tgeu,
        50 => Tlt,
        51 => Tltu,
        52 => Teq,
        54 => Tne,
        56 => Dsll,
        58 => Dsrl,
        59 => Dsra,
        60 => Dsll32,
        62 => Dsrl32,
        63 => Dsra32,
        _ => Invalid,
    }
}

fn regimm(rt: u32) -> Mnemonic {
    use Mnemonic::*;
    match rt {
        0 => Bltz,
        1 => Bgez,
        2 => Bltzl,
        3 => Bgezl,
        8 => Tgei,
        9 => Tgeiu,
        10 => Tlti,
        11 => Tltiu,
        12 => Teqi,
        14 => Tnei,
        16 => Bltzal,
        17 => Bgezal,
        18 => Bltzall,
        19 => Bgezall,
        24 => Mtsab,
        25 => Mtsah,
        _ => Invalid,
    }
}

fn mmi(raw: u32) -> Mnemonic {
    use Mnemonic::*;
    let sa = (raw >> 6) & 31;
    match raw & 63 {
        0 => Madd,
        1 => Maddu,
        4 => Plzcw,
        8 => mmi0(sa),
        9 => mmi2(sa),
        16 => Mfhi1,
        17 => Mthi1,
        18 => Mflo1,
        19 => Mtlo1,
        24 => Mult1,
        25 => Multu1,
        26 => Div1,
        27 => Divu1,
        32 => Madd1,
        33 => Maddu1,
        40 => mmi1(sa),
        41 => mmi3(sa),
        48 => match sa {
            0 => PmfhlLw,
            1 => PmfhlUw,
            2 => PmfhlSlw,
            3 => PmfhlLh,
            4 => PmfhlSh,
            _ => Invalid,
        },
        49 if sa == 0 => PmthlLw,
        52 => Psllh,
        54 => Psrlh,
        55 => Psrah,
        60 => Psllw,
        62 => Psrlw,
        63 => Psraw,
        _ => Invalid,
    }
}

fn mmi0(sa: u32) -> Mnemonic {
    use Mnemonic::*;
    match sa {
        0 => Paddw,
        1 => Psubw,
        2 => Pcgtw,
        3 => Pmaxw,
        4 => Paddh,
        5 => Psubh,
        6 => Pcgth,
        7 => Pmaxh,
        8 => Paddb,
        9 => Psubb,
        10 => Pcgtb,
        16 => Paddsw,
        17 => Psubsw,
        18 => Pextlw,
        19 => Ppacw,
        20 => Paddsh,
        21 => Psubsh,
        22 => Pextlh,
        23 => Ppach,
        24 => Paddsb,
        25 => Psubsb,
        26 => Pextlb,
        27 => Ppacb,
        30 => Pext5,
        31 => Ppac5,
        _ => Invalid,
    }
}

fn mmi1(sa: u32) -> Mnemonic {
    use Mnemonic::*;
    match sa {
        1 => Pabsw,
        2 => Pceqw,
        3 => Pminw,
        4 => Padsbh,
        5 => Pabsh,
        6 => Pceqh,
        7 => Pminh,
        10 => Pceqb,
        16 => Padduw,
        17 => Psubuw,
        18 => Pextuw,
        20 => Padduh,
        21 => Psubuh,
        22 => Pextuh,
        24 => Paddub,
        25 => Psubub,
        26 => Pextub,
        27 => Qfsrv,
        _ => Invalid,
    }
}

fn mmi2(sa: u32) -> Mnemonic {
    use Mnemonic::*;
    match sa {
        0 => Pmaddw,
        2 => Psllvw,
        3 => Psrlvw,
        4 => Pmsubw,
        8 => Pmfhi,
        9 => Pmflo,
        10 => Pinth,
        12 => Pmultw,
        13 => Pdivw,
        14 => Pcpyld,
        16 => Pmaddh,
        17 => Phmadh,
        18 => Pand,
        19 => Pxor,
        20 => Pmsubh,
        21 => Phmsbh,
        26 => Pexeh,
        27 => Prevh,
        28 => Pmulth,
        29 => Pdivbw,
        30 => Pexew,
        31 => Prot3w,
        _ => Invalid,
    }
}

fn mmi3(sa: u32) -> Mnemonic {
    use Mnemonic::*;
    match sa {
        0 => Pmadduw,
        3 => Psravw,
        8 => Pmthi,
        9 => Pmtlo,
        10 => Pinteh,
        12 => Pmultuw,
        13 => Pdivuw,
        14 => Pcpyud,
        18 => Por,
        19 => Pnor,
        26 => Pexch,
        27 => Pcpyh,
        30 => Pexcw,
        _ => Invalid,
    }
}

fn cop0(raw: u32) -> Mnemonic {
    use Mnemonic::*;
    match (raw >> 21) & 31 {
        0 => Mfc0,
        4 => Mtc0,
        8 => match (raw >> 16) & 31 {
            0 => Bc0f,
            1 => Bc0t,
            2 => Bc0fl,
            3 => Bc0tl,
            _ => Invalid,
        },
        16 => match raw & 63 {
            1 => Tlbr,
            2 => Tlbwi,
            6 => Tlbwr,
            8 => Tlbp,
            24 => Eret,
            56 => Ei,
            57 => Di,
            _ => Invalid,
        },
        _ => Invalid,
    }
}

fn cop1(raw: u32) -> Mnemonic {
    use Mnemonic::*;
    match (raw >> 21) & 31 {
        0 => Mfc1,
        2 => Cfc1,
        4 => Mtc1,
        6 => Ctc1,
        8 => match (raw >> 16) & 31 {
            0 => Bc1f,
            1 => Bc1t,
            2 => Bc1fl,
            3 => Bc1tl,
            _ => Invalid,
        },
        16 => match raw & 63 {
            0 => AddS,
            1 => SubS,
            2 => MulS,
            3 => DivS,
            4 => SqrtS,
            5 => AbsS,
            6 => MovS,
            7 => NegS,
            22 => RsqrtS,
            24 => AddaS,
            25 => SubaS,
            26 => MulaS,
            28 => MaddS,
            29 => MsubS,
            30 => MaddaS,
            31 => MsubaS,
            36 => CvtWS,
            40 => MaxS,
            41 => MinS,
            48 => CFS,
            50 => CEqS,
            52 => CLtS,
            54 => CLeS,
            _ => Invalid,
        },
        20 if raw & 63 == 32 => CvtSW,
        _ => Invalid,
    }
}

fn cop2(raw: u32) -> Mnemonic {
    use Mnemonic::*;
    let rs = (raw >> 21) & 31;
    if rs < 16 {
        return match rs {
            1 => Qmfc2,
            2 => Cfc2,
            5 => Qmtc2,
            6 => Ctc2,
            8 => match (raw >> 16) & 31 {
                0 => Bc2f,
                1 => Bc2t,
                2 => Bc2fl,
                3 => Bc2tl,
                _ => Invalid,
            },
            _ => Invalid,
        };
    }
    let funct = raw & 63;
    if funct < 60 {
        return match funct {
            0..=3 => VaddBc,
            4..=7 => VsubBc,
            8..=11 => VmaddBc,
            12..=15 => VmsubBc,
            16..=19 => VmaxBc,
            20..=23 => VminiBc,
            24..=27 => VmulBc,
            28 => Vmulq,
            29 => Vmaxi,
            30 => Vmuli,
            31 => Vminii,
            32 => Vaddq,
            33 => Vmaddq,
            34 => Vaddi,
            35 => Vmaddi,
            36 => Vsubq,
            37 => Vmsubq,
            38 => Vsubi,
            39 => Vmsubi,
            40 => Vadd,
            41 => Vmadd,
            42 => Vmul,
            43 => Vmax,
            44 => Vsub,
            45 => Vmsub,
            46 => Vopmsub,
            47 => Vmini,
            48 => Viadd,
            49 => Visub,
            50 => Viaddi,
            52 => Viand,
            53 => Vior,
            56 => Vcallms,
            57 => Vcallmsr,
            _ => Invalid,
        };
    }
    match (((raw >> 6) & 31) << 2) | (funct & 3) {
        0..=3 => VaddaBc,
        4..=7 => VsubaBc,
        8..=11 => VmaddaBc,
        12..=15 => VmsubaBc,
        16 => Vitof0,
        17 => Vitof4,
        18 => Vitof12,
        19 => Vitof15,
        20 => Vftoi0,
        21 => Vftoi4,
        22 => Vftoi12,
        23 => Vftoi15,
        24..=27 => VmulaBc,
        28 => Vmulaq,
        29 => Vabs,
        30 => Vmulai,
        31 => Vclipw,
        32 => Vaddaq,
        33 => Vmaddaq,
        34 => Vaddai,
        35 => Vmaddai,
        36 => Vsubaq,
        37 => Vmsubaq,
        38 => Vsubai,
        39 => Vmsubai,
        40 => Vadda,
        41 => Vmadda,
        42 => Vmula,
        44 => Vsuba,
        45 => Vmsuba,
        46 => Vopmula,
        47 => Vnop,
        48 => Vmove,
        49 => Vmr32,
        52 => Vlqi,
        53 => Vsqi,
        54 => Vlqd,
        55 => Vsqd,
        56 => Vdiv,
        57 => Vsqrt,
        58 => Vrsqrt,
        59 => Vwaitq,
        60 => Vmtir,
        61 => Vmfir,
        62 => Vilwr,
        63 => Viswr,
        64 => Vrnext,
        65 => Vrget,
        66 => Vrinit,
        67 => Vrxor,
        _ => Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Mnemonic::*;

    fn d(raw: u32) -> String {
        decode(raw).disasm(0x100000).to_string()
    }

    #[test]
    fn crt0_prologue() {
        // Words taken from the game's entry point.
        assert_eq!(d(0x3c020024), "lui $v0, 0x24");
        assert_eq!(d(0x7c400000), "sq $zero, 0($v0)");
        assert_eq!(d(0x0080e02d), "daddu $gp, $a0, $zero");
        assert_eq!(d(0x0000000c), "syscall");
        assert_eq!(decode(0x0c044780).jump_target(0x100084), Some(0x111e00));
    }

    #[test]
    fn ee_specific() {
        assert_eq!(decode(0x7a000000).m, Lq);
        assert_eq!(decode(0x70000018).m, Mult1);
        // pcpyld $a0, $a1, $a2
        assert_eq!(d(0x70a62389), "pcpyld $a0, $a1, $a2");
        // por $v0, $zero, $zero
        assert_eq!(decode(0x700014a9).m, Por);
        // three-operand mult $v0, $a0, $a1
        assert_eq!(d(0x00851018), "mult $v0, $a0, $a1");
    }

    #[test]
    fn vu0_macro() {
        // vmulax.xyzw ACC, vf1, vf2x  (special2 idx 24, dest 0xf)
        let w = (18 << 26) | (1 << 25) | (0xf << 21) | (2 << 16) | (1 << 11) | (6 << 6) | 0x3c;
        assert_eq!(d(w), "vmulax.xyzw ACC, $vf1, $vf2x");
        // vadd.xyz vf3, vf1, vf2
        let w = (18 << 26) | (1 << 25) | (0xe << 21) | (2 << 16) | (1 << 11) | (3 << 6) | 40;
        assert_eq!(d(w), "vadd.xyz $vf3, $vf1, $vf2");
        // vcallms 0x100
        let w = (18 << 26) | (1 << 25) | ((0x100 / 8) << 6) | 56;
        assert_eq!(d(w), "vcallms 0x100");
        assert_eq!(decode((18 << 26) | (1 << 21) | (5 << 16) | (3 << 11) | 1).m, Qmfc2);
    }

    #[test]
    fn every_mnemonic_reachable() {
        // Brute force the whole opcode space once; every variant except
        // Invalid must be produced by some word.
        let mut seen = std::collections::HashSet::new();
        for op in 0u32..64 {
            for rs in 0u32..32 {
                for rt in 0u32..32 {
                    for sa in 0u32..32 {
                        for f in 0u32..64 {
                            let w = (op << 26) | (rs << 21) | (rt << 16) | (sa << 6) | f;
                            seen.insert(decode(w).m);
                        }
                    }
                }
            }
        }
        for m in Mnemonic::ALL {
            assert!(seen.contains(m), "{m:?} never decoded");
        }
    }
}
