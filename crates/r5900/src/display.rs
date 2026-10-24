use std::fmt;

use crate::{Format, Insn, Mnemonic};

const GPR: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6",
    "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp",
    "fp", "ra",
];

const FIELD: [char; 4] = ['x', 'y', 'z', 'w'];

pub fn gpr_name(r: u8) -> &'static str {
    GPR[r as usize & 31]
}

/// Display adapter that knows the instruction's address, so branch and jump
/// targets print as absolute addresses.
pub struct Disasm {
    insn: Insn,
    pc: u32,
}

impl Insn {
    pub fn disasm(&self, pc: u32) -> Disasm {
        Disasm { insn: *self, pc }
    }
}

fn dest(mask: u8) -> String {
    (0..4)
        .filter(|i| mask & (8 >> i) != 0)
        .map(|i| FIELD[i])
        .collect()
}

impl fmt::Display for Disasm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Format::*;
        let i = &self.insn;
        let g = |r: u8| format!("${}", gpr_name(r));
        let (rs, rt, rd) = (g(i.rs()), g(i.rt()), g(i.rd()));
        let (vfs, vft, vfd) = (
            format!("$vf{}", i.fs()),
            format!("$vf{}", i.ft()),
            format!("$vf{}", i.fd()),
        );
        let (vis, vit, vid) = (
            format!("$vi{}", i.fs()),
            format!("$vi{}", i.ft()),
            format!("$vi{}", i.fd()),
        );
        let bc = FIELD[i.bc() as usize];
        let d = dest(i.dest());

        let name = i.m.name();
        match i.m.format() {
            VFdFsFtBc | VAccFsFtBc => write!(f, "{name}{bc}.{d} ")?,
            VFdFsFt | VAccFsFt | VFdFsQ | VFdFsI | VAccFsQ | VAccFsI | VFtFs | VLqi
            | VSqi | VLqd | VSqd | VMfir | VIlwr | VIswr | VRnext => write!(f, "{name}.{d} ")?,
            Raw => return write!(f, ".word 0x{:08x}", i.raw),
            None if i.m == Mnemonic::Vnop || i.m == Mnemonic::Vwaitq => return f.write_str(name),
            None => return f.write_str(name),
            Sync if i.sa() & 0x10 != 0 => return f.write_str("sync.p"),
            Sync => return f.write_str("sync"),
            RtVf | RtVi => {
                let suffix = if i.interlock() { ".i" } else { ".ni" };
                write!(f, "{name}{suffix} ")?
            }
            _ => write!(f, "{name} ")?,
        }

        match i.m.format() {
            RdRsRt => write!(f, "{rd}, {rs}, {rt}"),
            RdRtRs => write!(f, "{rd}, {rt}, {rs}"),
            RdRtSa => write!(f, "{rd}, {rt}, {}", i.sa()),
            RdRs => write!(f, "{rd}, {rs}"),
            RdRt => write!(f, "{rd}, {rt}"),
            Rd => write!(f, "{rd}"),
            Rs => write!(f, "{rs}"),
            RsRt => write!(f, "{rs}, {rt}"),
            RtRsImm => write!(f, "{rt}, {rs}, {}", i.imm()),
            RtRsUimm => write!(f, "{rt}, {rs}, 0x{:x}", i.uimm()),
            RtUimm => write!(f, "{rt}, 0x{:x}", i.uimm()),
            RsRtOff => write!(f, "{rs}, {rt}, 0x{:x}", i.branch_target(self.pc).unwrap()),
            RsOff => write!(f, "{rs}, 0x{:x}", i.branch_target(self.pc).unwrap()),
            RsImm => write!(f, "{rs}, {}", i.imm()),
            Off => write!(f, "0x{:x}", i.branch_target(self.pc).unwrap()),
            Target => write!(f, "0x{:x}", i.jump_target(self.pc).unwrap()),
            Mem => write!(f, "{rt}, {}({rs})", i.imm()),
            FMem => write!(f, "$f{}, {}({rs})", i.rt(), i.imm()),
            VMem => write!(f, "{vft}, {}({rs})", i.imm()),
            CacheOp => write!(f, "0x{:x}, {}({rs})", i.rt(), i.imm()),
            RtCop0 => write!(f, "{rt}, ${}", i.rd()),
            RtFpr => write!(f, "{rt}, $f{}", i.rd()),
            RtFcr => write!(f, "{rt}, $fcr{}", i.rd()),
            RtVf => write!(f, "{rt}, {vfs}"),
            RtVi => write!(f, "{rt}, {vis}"),
            FdFsFt => write!(f, "$f{}, $f{}, $f{}", i.fd(), i.fs(), i.ft()),
            FdFs => write!(f, "$f{}, $f{}", i.fd(), i.fs()),
            FsFt => write!(f, "$f{}, $f{}", i.fs(), i.ft()),
            VFdFsFt => write!(f, "{vfd}, {vfs}, {vft}"),
            VFdFsFtBc => write!(f, "{vfd}, {vfs}, {vft}{bc}"),
            VAccFsFt => write!(f, "ACC, {vfs}, {vft}"),
            VAccFsFtBc => write!(f, "ACC, {vfs}, {vft}{bc}"),
            VFdFsQ => write!(f, "{vfd}, {vfs}, Q"),
            VFdFsI => write!(f, "{vfd}, {vfs}, I"),
            VAccFsQ => write!(f, "ACC, {vfs}, Q"),
            VAccFsI => write!(f, "ACC, {vfs}, I"),
            VFtFs => write!(f, "{vft}, {vfs}"),
            VIdIsIt => write!(f, "{vid}, {vis}, {vit}"),
            VItIsImm5 => write!(f, "{vit}, {vis}, {}", i.imm5()),
            VCallms => write!(f, "0x{:x}", i.callms_addr()),
            VDiv => write!(
                f,
                "Q, {vfs}{}, {vft}{}",
                FIELD[i.fsf() as usize],
                FIELD[i.ftf() as usize]
            ),
            VSqrt => write!(f, "Q, {vft}{}", FIELD[i.ftf() as usize]),
            VClip => write!(f, "{vfs}xyz, {vft}w"),
            VLqi => write!(f, "{vft}, ({vis}++)"),
            VSqi => write!(f, "{vfs}, ({vit}++)"),
            VLqd => write!(f, "{vft}, (--{vis})"),
            VSqd => write!(f, "{vfs}, (--{vit})"),
            VMtir => write!(f, "{vit}, {vfs}{}", FIELD[i.fsf() as usize]),
            VMfir => write!(f, "{vft}, {vis}"),
            VIlwr | VIswr => write!(f, "{vit}, ({vis})"),
            VRnext => write!(f, "{vft}, R"),
            VRinit => write!(f, "R, {vfs}{}", FIELD[i.fsf() as usize]),
            Raw | None | Sync => unreachable!(),
        }
    }
}
