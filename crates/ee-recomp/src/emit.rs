//! Rust source for one non-control-flow instruction.
//!
//! Control transfers (branches, jumps, calls) are handled by the function
//! emitter in `main.rs`; this module covers everything else, plus the
//! condition expressions of branches.

use r5900::{Insn, Mnemonic as M};

/// Rust statements for `i` at `pc`, or None if the recompiler doesn't
/// support the instruction (the caller emits a runtime trap).
pub fn insn(i: &Insn, pc: u32) -> Option<String> {
    let (rs, rt, rd, sa) = (i.rs(), i.rt(), i.rd(), i.sa());
    let (fs, ft, fd) = (i.fs(), i.ft(), i.fd());
    let imm = i.imm() as i32;
    let uimm = i.uimm();
    let ea = format!("c.ea({rs}, {imm})");
    let dest = i.dest();
    let bc = i.bc();

    // 64-bit write, 32-bit sign-extended write, 128-bit write.
    let w = |r: u8, e: String| if r == 0 { String::new() } else { format!("c.w({r}, {e});") };
    let ws = |r: u8, e: String| if r == 0 { String::new() } else { format!("c.ws({r}, {e});") };
    let wq = |r: u8, e: String| if r == 0 { String::new() } else { format!("c.wq({r}, {e});") };
    // Memory reads borrow `c` mutably, so bind the value before writing it.
    let load = |r: u8, e: String| if r == 0 { format!("let _ = {e};") } else { format!("{{ let v = {e}; c.w({r}, v); }}") };

    let f = |n: u8| format!("c.fpr[{n}]");
    let vf = |n: u8| format!("c.vu0.vf[{n}]");
    let vfbc = |n: u8| format!("vu::splat(c.vu0.vf[{n}][{bc}])");
    let vfd = |e: String| format!("{{ let v = {e}; c.vu0.vw({fd}, {dest}, v); }}");
    let vft = |e: String| format!("{{ let v = {e}; c.vu0.vw({ft}, {dest}, v); }}");
    let vacc = |e: String| format!("{{ let v = {e}; c.vu0.accw({dest}, v); }}");
    let (s, t) = (vf(fs), vf(ft));
    let (q, ii) = ("vu::splat(c.vu0.q)".to_string(), "vu::splat(c.vu0.i)".to_string());
    let acc = "c.vu0.acc";
    let mmi2 = |name: &str| wq(rd, format!("mmi::{name}(c.q({rs}), c.q({rt}))"));

    Some(match i.m {
        M::Sll if i.raw == 0 => String::new(),
        M::Sll => ws(rd, format!("c.r32({rt}) << {sa}")),
        M::Srl => ws(rd, format!("c.r32({rt}) >> {sa}")),
        M::Sra => ws(rd, format!("((c.r32({rt}) as i32) >> {sa}) as u32")),
        M::Sllv => ws(rd, format!("c.r32({rt}) << (c.r32({rs}) & 31)")),
        M::Srlv => ws(rd, format!("c.r32({rt}) >> (c.r32({rs}) & 31)")),
        M::Srav => ws(rd, format!("((c.r32({rt}) as i32) >> (c.r32({rs}) & 31)) as u32")),
        M::Dsll => w(rd, format!("c.r({rt}) << {sa}")),
        M::Dsrl => w(rd, format!("c.r({rt}) >> {sa}")),
        M::Dsra => w(rd, format!("((c.r({rt}) as i64) >> {sa}) as u64")),
        M::Dsll32 => w(rd, format!("c.r({rt}) << {}", sa + 32)),
        M::Dsrl32 => w(rd, format!("c.r({rt}) >> {}", sa + 32)),
        M::Dsra32 => w(rd, format!("((c.r({rt}) as i64) >> {}) as u64", sa + 32)),
        M::Dsllv => w(rd, format!("c.r({rt}) << (c.r({rs}) & 63)")),
        M::Dsrlv => w(rd, format!("c.r({rt}) >> (c.r({rs}) & 63)")),
        M::Dsrav => w(rd, format!("((c.r({rt}) as i64) >> (c.r({rs}) & 63)) as u64")),
        M::Movz if rd != 0 => format!("if c.r({rt}) == 0 {{ c.w({rd}, c.r({rs})); }}"),
        M::Movn if rd != 0 => format!("if c.r({rt}) != 0 {{ c.w({rd}, c.r({rs})); }}"),
        M::Movz | M::Movn => String::new(),
        M::Mfhi => w(rd, "c.hi[0]".into()),
        M::Mflo => w(rd, "c.lo[0]".into()),
        M::Mfhi1 => w(rd, "c.hi[1]".into()),
        M::Mflo1 => w(rd, "c.lo[1]".into()),
        M::Mthi => format!("c.hi[0] = c.r({rs});"),
        M::Mtlo => format!("c.lo[0] = c.r({rs});"),
        M::Mthi1 => format!("c.hi[1] = c.r({rs});"),
        M::Mtlo1 => format!("c.lo[1] = c.r({rs});"),
        M::Mult => format!("c.mult({rd}, {rs}, {rt}, 0);"),
        M::Multu => format!("c.multu({rd}, {rs}, {rt}, 0);"),
        M::Mult1 => format!("c.mult({rd}, {rs}, {rt}, 1);"),
        M::Multu1 => format!("c.multu({rd}, {rs}, {rt}, 1);"),
        M::Madd => format!("c.madd({rd}, {rs}, {rt}, 0);"),
        M::Maddu => format!("c.maddu({rd}, {rs}, {rt}, 0);"),
        M::Madd1 => format!("c.madd({rd}, {rs}, {rt}, 1);"),
        M::Maddu1 => format!("c.maddu({rd}, {rs}, {rt}, 1);"),
        M::Div => format!("c.div({rs}, {rt}, 0);"),
        M::Divu => format!("c.divu({rs}, {rt}, 0);"),
        M::Div1 => format!("c.div({rs}, {rt}, 1);"),
        M::Divu1 => format!("c.divu({rs}, {rt}, 1);"),
        // Overflow traps on add/sub are not emulated; no shipped game relies on them.
        M::Add | M::Addu => ws(rd, format!("c.r32({rs}).wrapping_add(c.r32({rt}))")),
        M::Sub | M::Subu => ws(rd, format!("c.r32({rs}).wrapping_sub(c.r32({rt}))")),
        M::Dadd | M::Daddu => w(rd, format!("c.r({rs}).wrapping_add(c.r({rt}))")),
        M::Dsub | M::Dsubu => w(rd, format!("c.r({rs}).wrapping_sub(c.r({rt}))")),
        M::And => w(rd, format!("c.r({rs}) & c.r({rt})")),
        M::Or => w(rd, format!("c.r({rs}) | c.r({rt})")),
        M::Xor => w(rd, format!("c.r({rs}) ^ c.r({rt})")),
        M::Nor => w(rd, format!("!(c.r({rs}) | c.r({rt}))")),
        M::Slt => w(rd, format!("((c.r({rs}) as i64) < (c.r({rt}) as i64)) as u64")),
        M::Sltu => w(rd, format!("(c.r({rs}) < c.r({rt})) as u64")),
        M::Mfsa => w(rd, "c.sa as u64".into()),
        M::Mtsa => format!("c.sa = c.r32({rs});"),
        M::Mtsab => format!("c.sa = ((c.r32({rs}) ^ {uimm}) & 15) * 8;"),
        M::Mtsah => format!("c.sa = ((c.r32({rs}) ^ {uimm}) & 7) * 16;"),

        M::Addi | M::Addiu => ws(rt, format!("c.r32({rs}).wrapping_add({imm}i32 as u32)")),
        M::Daddi | M::Daddiu => w(rt, format!("c.r({rs}).wrapping_add({imm}i64 as u64)")),
        M::Slti => w(rt, format!("((c.r({rs}) as i64) < {imm}i64) as u64")),
        M::Sltiu => w(rt, format!("(c.r({rs}) < {imm}i64 as u64) as u64")),
        M::Andi => w(rt, format!("c.r({rs}) & {uimm:#x}")),
        M::Ori => w(rt, format!("c.r({rs}) | {uimm:#x}")),
        M::Xori => w(rt, format!("c.r({rs}) ^ {uimm:#x}")),
        M::Lui => w(rt, format!("{:#x}", ((uimm as u32) << 16) as i32 as i64 as u64)),

        M::Lb => load(rt, format!("c.mem.r8({ea}) as i8 as i64 as u64")),
        M::Lh => load(rt, format!("c.mem.r16({ea}) as i16 as i64 as u64")),
        M::Lw => load(rt, format!("c.mem.r32({ea}) as i32 as i64 as u64")),
        M::Lbu => load(rt, format!("c.mem.r8({ea}) as u64")),
        M::Lhu => load(rt, format!("c.mem.r16({ea}) as u64")),
        M::Lwu => load(rt, format!("c.mem.r32({ea}) as u64")),
        M::Ld => load(rt, format!("c.mem.r64({ea})")),
        M::Lq if rt == 0 => format!("let _ = c.mem.r128({ea} & !15);"),
        M::Lq => format!("{{ let v = c.mem.r128({ea} & !15); c.wq({rt}, v); }}"),
        M::Sb => format!("{{ let v = c.r({rt}) as u8; c.mem.w8({ea}, v); }}"),
        M::Sh => format!("{{ let v = c.r({rt}) as u16; c.mem.w16({ea}, v); }}"),
        M::Sw => format!("{{ let v = c.r32({rt}); c.mem.w32({ea}, v); }}"),
        M::Sd => format!("{{ let v = c.r({rt}); c.mem.w64({ea}, v); }}"),
        M::Sq => format!("{{ let v = c.q({rt}); c.mem.w128({ea} & !15, v); }}"),
        M::Lwl => format!("c.lwl({rt}, {ea});"),
        M::Lwr => format!("c.lwr({rt}, {ea});"),
        M::Ldl => format!("c.ldl({rt}, {ea});"),
        M::Ldr => format!("c.ldr({rt}, {ea});"),
        M::Swl => format!("c.swl({rt}, {ea});"),
        M::Swr => format!("c.swr({rt}, {ea});"),
        M::Sdl => format!("c.sdl({rt}, {ea});"),
        M::Sdr => format!("c.sdr({rt}, {ea});"),
        M::Lwc1 => format!("c.fpr[{ft}] = f32::from_bits(c.mem.r32({ea}));"),
        M::Swc1 => format!("{{ let v = c.fpr[{ft}].to_bits(); c.mem.w32({ea}, v); }}"),
        M::Lqc2 => format!("c.lqc2({ft}, {ea});"),
        M::Sqc2 => format!("c.sqc2({ft}, {ea});"),
        M::Cache | M::Pref | M::Sync => String::new(),

        M::Syscall => format!("c.syscall({pc:#x});"),
        M::Break => format!("c.brk({pc:#x});"),
        M::Mfc0 => format!("c.mfc0({rt}, {rd});"),
        M::Mtc0 => format!("c.mtc0({rt}, {rd});"),
        M::Ei => "c.ei();".into(),
        M::Di => "c.di();".into(),
        M::Tlbr | M::Tlbwi | M::Tlbwr | M::Tlbp => format!("c.tlb_op({pc:#x});"),

        M::Mfc1 => ws(rt, format!("c.fpr[{fs}].to_bits()")),
        M::Mtc1 => format!("c.fpr[{fs}] = f32::from_bits(c.r32({rt}));"),
        M::Cfc1 => ws(rt, format!("c.cfc1({fs})")),
        M::Ctc1 => format!("c.ctc1({fs}, c.r32({rt}));"),
        M::AddS => format!("{} = ps2f({} + {});", f(fd), f(fs), f(ft)),
        M::SubS => format!("{} = ps2f({} - {});", f(fd), f(fs), f(ft)),
        M::MulS => format!("{} = ps2f({} * {});", f(fd), f(fs), f(ft)),
        M::DivS => format!("{} = fdiv({}, {});", f(fd), f(fs), f(ft)),
        M::SqrtS => format!("{} = fsqrt({});", f(fd), f(ft)),
        M::RsqrtS => format!("{} = fdiv({}, fsqrt({}));", f(fd), f(fs), f(ft)),
        M::AbsS => format!("{} = {}.abs();", f(fd), f(fs)),
        M::MovS => format!("{} = {};", f(fd), f(fs)),
        M::NegS => format!("{} = -{};", f(fd), f(fs)),
        M::AddaS => format!("c.acc = ps2f({} + {});", f(fs), f(ft)),
        M::SubaS => format!("c.acc = ps2f({} - {});", f(fs), f(ft)),
        M::MulaS => format!("c.acc = ps2f({} * {});", f(fs), f(ft)),
        M::MaddS => format!("{} = ps2f(c.acc + ps2f({} * {}));", f(fd), f(fs), f(ft)),
        M::MsubS => format!("{} = ps2f(c.acc - ps2f({} * {}));", f(fd), f(fs), f(ft)),
        M::MaddaS => format!("c.acc = ps2f(c.acc + ps2f({} * {}));", f(fs), f(ft)),
        M::MsubaS => format!("c.acc = ps2f(c.acc - ps2f({} * {}));", f(fs), f(ft)),
        M::MaxS => format!("{} = fmax({}, {});", f(fd), f(fs), f(ft)),
        M::MinS => format!("{} = fmin({}, {});", f(fd), f(fs), f(ft)),
        M::CvtWS => format!("{} = f32::from_bits(cvt_w({}));", f(fd), f(fs)),
        M::CvtSW => format!("{} = ({}.to_bits() as i32) as f32;", f(fd), f(fs)),
        M::CFS => "c.set_fcc(false);".into(),
        M::CEqS => format!("c.set_fcc({} == {});", f(fs), f(ft)),
        M::CLtS => format!("c.set_fcc({} < {});", f(fs), f(ft)),
        M::CLeS => format!("c.set_fcc({} <= {});", f(fs), f(ft)),

        // COP2 transfers. VU0 runs synchronously, so interlocks are no-ops.
        M::Qmfc2 => wq(rt, format!("c.vu0.vf_q({fs})")),
        M::Qmtc2 => format!("c.vu0.set_vf_q({fs}, c.q({rt}));"),
        M::Cfc2 => ws(rt, format!("c.vu0.cfc2({fs})")),
        M::Ctc2 => format!("c.ctc2({fs}, c.r32({rt}));"),

        M::VaddBc => vfd(format!("vu::add({s}, {})", vfbc(ft))),
        M::VsubBc => vfd(format!("vu::sub({s}, {})", vfbc(ft))),
        M::VmulBc => vfd(format!("vu::mul({s}, {})", vfbc(ft))),
        M::VmaddBc => vfd(format!("vu::madd({acc}, {s}, {})", vfbc(ft))),
        M::VmsubBc => vfd(format!("vu::msub({acc}, {s}, {})", vfbc(ft))),
        M::VmaxBc => vfd(format!("vu::max({s}, {})", vfbc(ft))),
        M::VminiBc => vfd(format!("vu::mini({s}, {})", vfbc(ft))),
        M::Vadd => vfd(format!("vu::add({s}, {t})")),
        M::Vsub => vfd(format!("vu::sub({s}, {t})")),
        M::Vmul => vfd(format!("vu::mul({s}, {t})")),
        M::Vmadd => vfd(format!("vu::madd({acc}, {s}, {t})")),
        M::Vmsub => vfd(format!("vu::msub({acc}, {s}, {t})")),
        M::Vmax => vfd(format!("vu::max({s}, {t})")),
        M::Vmini => vfd(format!("vu::mini({s}, {t})")),
        M::Vaddq => vfd(format!("vu::add({s}, {q})")),
        M::Vsubq => vfd(format!("vu::sub({s}, {q})")),
        M::Vmulq => vfd(format!("vu::mul({s}, {q})")),
        M::Vmaddq => vfd(format!("vu::madd({acc}, {s}, {q})")),
        M::Vmsubq => vfd(format!("vu::msub({acc}, {s}, {q})")),
        M::Vaddi => vfd(format!("vu::add({s}, {ii})")),
        M::Vsubi => vfd(format!("vu::sub({s}, {ii})")),
        M::Vmuli => vfd(format!("vu::mul({s}, {ii})")),
        M::Vmaddi => vfd(format!("vu::madd({acc}, {s}, {ii})")),
        M::Vmsubi => vfd(format!("vu::msub({acc}, {s}, {ii})")),
        M::Vmaxi => vfd(format!("vu::max({s}, {ii})")),
        M::Vminii => vfd(format!("vu::mini({s}, {ii})")),
        M::Vopmsub => vfd(format!("vu::sub({acc}, vu::cross({s}, {t}))")),
        M::VaddaBc => vacc(format!("vu::add({s}, {})", vfbc(ft))),
        M::VsubaBc => vacc(format!("vu::sub({s}, {})", vfbc(ft))),
        M::VmulaBc => vacc(format!("vu::mul({s}, {})", vfbc(ft))),
        M::VmaddaBc => vacc(format!("vu::madd({acc}, {s}, {})", vfbc(ft))),
        M::VmsubaBc => vacc(format!("vu::msub({acc}, {s}, {})", vfbc(ft))),
        M::Vadda => vacc(format!("vu::add({s}, {t})")),
        M::Vsuba => vacc(format!("vu::sub({s}, {t})")),
        M::Vmula => vacc(format!("vu::mul({s}, {t})")),
        M::Vmadda => vacc(format!("vu::madd({acc}, {s}, {t})")),
        M::Vmsuba => vacc(format!("vu::msub({acc}, {s}, {t})")),
        M::Vaddaq => vacc(format!("vu::add({s}, {q})")),
        M::Vmulaq => vacc(format!("vu::mul({s}, {q})")),
        M::Vmaddaq => vacc(format!("vu::madd({acc}, {s}, {q})")),
        M::Vmulai => vacc(format!("vu::mul({s}, {ii})")),
        M::Vaddai => vacc(format!("vu::add({s}, {ii})")),
        M::Vmaddai => vacc(format!("vu::madd({acc}, {s}, {ii})")),
        M::Vopmula => vacc(format!("vu::cross({s}, {t})")),
        M::Vmove => vft(s),
        M::Vmr32 => vft(format!("vu::mr32({s})")),
        M::Vabs => vft(format!("vu::abs({s})")),
        M::Vitof0 => vft(format!("vu::itof({s}, 0)")),
        M::Vitof4 => vft(format!("vu::itof({s}, 4)")),
        M::Vitof12 => vft(format!("vu::itof({s}, 12)")),
        M::Vitof15 => vft(format!("vu::itof({s}, 15)")),
        M::Vftoi0 => vft(format!("vu::ftoi({s}, 0)")),
        M::Vftoi4 => vft(format!("vu::ftoi({s}, 4)")),
        M::Vftoi12 => vft(format!("vu::ftoi({s}, 12)")),
        M::Vftoi15 => vft(format!("vu::ftoi({s}, 15)")),
        M::Vclipw => format!("c.vu0.clip({fs}, {ft});"),
        M::Vdiv => format!("c.vu0.q = vu::div({s}[{}], {t}[{}]);", i.fsf(), i.ftf()),
        M::Vsqrt => format!("c.vu0.q = fsqrt({t}[{}]);", i.ftf()),
        M::Vrsqrt => format!("c.vu0.q = vu::div({s}[{}], fsqrt({t}[{}]));", i.fsf(), i.ftf()),
        M::Vwaitq | M::Vnop => String::new(),
        M::Vlqi => format!("c.vlqi({dest}, {ft}, {fs});"),
        M::Vsqi => format!("c.vsqi({dest}, {fs}, {ft});"),
        M::Vcallms => format!("c.vu0_callms({:#x});", i.callms_addr()),
        M::Vcallmsr => "c.vu0_callms(c.vu0.cmsar0 * 8);".into(),

        M::Paddub if rt == 0 => wq(rd, format!("c.q({rs})")),
        M::Paddub => mmi2("paddub"),
        M::Psubb => mmi2("psubb"),
        M::Paddh => mmi2("paddh"),
        M::Paddw => mmi2("paddw"),
        M::Psubw => mmi2("psubw"),
        M::Pcgtw => mmi2("pcgtw"),
        M::Pmaxh => mmi2("pmaxh"),
        M::Pminh => mmi2("pminh"),
        M::Pand => mmi2("pand"),
        M::Por => mmi2("por"),
        M::Pxor => mmi2("pxor"),
        M::Pnor => mmi2("pnor"),
        M::Pextlw => mmi2("pextlw"),
        M::Pextlh => mmi2("pextlh"),
        M::Pextlb => mmi2("pextlb"),
        M::Ppacb => mmi2("ppacb"),
        M::Ppach => mmi2("ppach"),
        M::Pcpyld => mmi2("pcpyld"),
        M::Pcpyud => mmi2("pcpyud"),
        M::Pcpyh => wq(rd, format!("mmi::pcpyh(c.q({rt}))")),
        M::Pexch => wq(rd, format!("mmi::pexch(c.q({rt}))")),
        M::Pexcw => wq(rd, format!("mmi::pexcw(c.q({rt}))")),
        M::Psllw => wq(rd, format!("mmi::psllw(c.q({rt}), {sa})")),
        M::Psrlw => wq(rd, format!("mmi::psrlw(c.q({rt}), {sa})")),
        M::Psraw => wq(rd, format!("mmi::psraw(c.q({rt}), {sa})")),
        M::Pcgth => mmi2("pcgth"),
        M::Pextub => mmi2("pextub"),
        M::Psllh => wq(rd, format!("mmi::psllh(c.q({rt}), {sa})")),
        M::Psrlh => wq(rd, format!("mmi::psrlh(c.q({rt}), {sa})")),
        M::Psrah => wq(rd, format!("mmi::psrah(c.q({rt}), {sa})")),
        M::Qfsrv => wq(rd, format!("mmi::qfsrv(c.q({rs}), c.q({rt}), c.sa)")),
        M::Plzcw => w(rd, format!("mmi::plzcw(c.r({rs}))")),

        _ => return None,
    })
}

/// Condition expression for a conditional branch, evaluated before its
/// delay slot runs.
pub fn condition(i: &Insn) -> Option<String> {
    let (rs, rt) = (i.rs(), i.rt());
    Some(match i.m {
        M::Beq | M::Beql if rs == rt => "true".into(),
        M::Beq | M::Beql => format!("c.r({rs}) == c.r({rt})"),
        M::Bne | M::Bnel => format!("c.r({rs}) != c.r({rt})"),
        M::Blez | M::Blezl => format!("(c.r({rs}) as i64) <= 0"),
        M::Bgtz | M::Bgtzl => format!("(c.r({rs}) as i64) > 0"),
        M::Bltz | M::Bltzl | M::Bltzal | M::Bltzall => format!("(c.r({rs}) as i64) < 0"),
        M::Bgez | M::Bgezl | M::Bgezal | M::Bgezall if rs == 0 => "true".into(),
        M::Bgez | M::Bgezl | M::Bgezal | M::Bgezall => format!("(c.r({rs}) as i64) >= 0"),
        M::Bc1t | M::Bc1tl => "c.fcc()".into(),
        M::Bc1f | M::Bc1fl => "!c.fcc()".into(),
        // VU0 never reports busy because microprograms run synchronously.
        M::Bc2t | M::Bc2tl => "false".into(),
        M::Bc2f | M::Bc2fl => "true".into(),
        _ => return None,
    })
}
