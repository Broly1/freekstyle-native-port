//! Compile VU microprograms to Rust with the pipeline resolved at build time.
//!
//! The interpreter (`ee_runtime::vumicro`) models VU timing at run time:
//! hazard stalls, Q latency, flags visible 4 cycles late, branches seeing a
//! VI register's old value. All of that depends only on the path taken, not
//! on data, so this compiler simulates it while generating code. Each block
//! is compiled once per pipeline state it can be entered in (a "version"),
//! and the emitted Rust is plain arithmetic on locals with no bookkeeping:
//!
//! - registers live in locals, loaded at entry and stored at exit;
//! - stalls become constant cycle counts, pending Q / MAC / clip writes
//!   become locals that are copied into place when they land;
//! - MAC flags are computed only where an FMAND/FMEQ/FMOR can see them
//!   (backward liveness over the versions), or for the final MAC;
//! - BAL/JR return addresses are tracked as constants, so subroutine
//!   returns are direct jumps.
//!
//! The status flag is not modelled: an image that reads it (FSAND, FSEQ,
//! FSOR, FSSET) is left to `vu::Image::function`, as are branches or E bits
//! in delay slots. FREEK_VU_CHECK compares compiled runs with the
//! interpreter.

use std::collections::HashMap;
use std::fmt::Write as _;

use r5900::{decode, Format as F, Insn, Mnemonic as M};

use crate::vu::Image;

const FMAC_LATENCY: i32 = 4;
const FLAG_LATENCY: i32 = 4;
/// Give up on an entry (and use the pair-calling code) past this many versions.
const MAX_VERSIONS: usize = 3000;
/// Pending MAC or clip writes at once (one per cycle, 4 cycles each).
const MAX_PENDING: usize = 8;

/// Pipeline state at a pair boundary. Times are cycles from now, all > 0.
#[derive(Clone, PartialEq, Eq, Hash)]
struct State {
    ready: [[u8; 4]; 32],
    /// Cycles until the pending Q lands (0: none).
    q: u8,
    /// Pending MAC and clip writes, oldest first.
    mac: Vec<u8>,
    clip: Vec<u8>,
    /// VI register written by the previous pair (0: none); its old value is
    /// in the local `vio`.
    last_vi: u8,
    /// Known values of the registers used as JR/JALR targets.
    link: [Option<u16>; 16],
    /// Known old value of `last_vi`.
    last_old: Option<u16>,
}

impl State {
    fn empty() -> State {
        State { ready: [[0; 4]; 32], q: 0, mac: Vec::new(), clip: Vec::new(), last_vi: 0, link: [None; 16], last_old: None }
    }
}

/// The same state with absolute times inside one version.
struct Sim {
    t: i32,
    ready: [[i32; 4]; 32],
    q: Option<i32>,
    mac: Vec<i32>,
    clip: Vec<i32>,
    last_vi: u8,
    link: [Option<u16>; 16],
    last_old: Option<u16>,
}

impl Sim {
    fn new(s: &State) -> Sim {
        Sim {
            t: 0,
            ready: s.ready.map(|r| r.map(i32::from)),
            q: (s.q > 0).then_some(s.q as i32),
            mac: s.mac.iter().map(|&d| d as i32).collect(),
            clip: s.clip.iter().map(|&d| d as i32).collect(),
            last_vi: s.last_vi,
            link: s.link,
            last_old: s.last_old,
        }
    }

    /// State for the next pair (everything due must be committed already).
    fn state(&self) -> State {
        let rel = |a: i32| (a - self.t).max(0) as u8;
        State {
            ready: self.ready.map(|r| r.map(rel)),
            q: self.q.map_or(0, rel),
            mac: self.mac.iter().map(|&a| rel(a)).collect(),
            clip: self.clip.iter().map(|&a| rel(a)).collect(),
            last_vi: self.last_vi,
            link: self.link,
            last_old: self.last_old,
        }
    }
}

/// MAC flag dataflow inside a version, for liveness. Variables: bit 0 is
/// the MAC register, bit 1 + k the k-th pending write.
#[derive(Clone, Copy)]
enum MacEv {
    /// The oldest n pending writes land.
    Commit(usize),
    /// A new pending write.
    Def,
    /// The MAC register is read.
    Use,
    /// Everything is read (leaving for the interpreter).
    UseAll,
}

/// How a version ends.
enum Exit {
    Goto(u32, State),
    Finish,
}

struct Walked {
    code: String,
    mac: Vec<MacEv>,
    exits: Vec<Exit>,
}

/// Whether a lower word reads the status flag (not compiled here).
fn reads_status(lo: u32, hi: u32) -> bool {
    hi >> 31 == 0 && matches!(lo >> 25, 0x14..=0x17)
}

fn reads_mac(lo: u32, hi: u32) -> bool {
    hi >> 31 == 0 && matches!(lo >> 25, 0x18 | 0x1a | 0x1b)
}

fn is_branch(lo: u32, hi: u32) -> bool {
    hi >> 31 == 0 && matches!(lo >> 25, 0x20 | 0x21 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2c..=0x2f)
}

/// Mirror of `vumicro::reads`: VF fields a pair reads, as (register,
/// field mask with bit 3 = x), for hazard stalls.
fn reads(lo: u32, hi: u32, m: M) -> Vec<(usize, u8)> {
    let dest = ((hi >> 21) & 15) as u8;
    let (fs, ft) = (((hi >> 11) & 31) as usize, ((hi >> 16) & 31) as usize);
    let bc = 8u8 >> (hi & 3);
    let mut out = Vec::new();
    let mut push = |reg: usize, mask: u8| {
        if reg != 0 && mask != 0 {
            out.push((reg, mask));
        }
    };
    match m.format() {
        F::VFdFsFt | F::VAccFsFt => {
            push(fs, dest);
            push(ft, dest);
        }
        F::VFdFsFtBc | F::VAccFsFtBc => {
            push(fs, dest);
            push(ft, bc);
        }
        F::VFdFsQ | F::VFdFsI | F::VAccFsQ | F::VAccFsI | F::VFtFs => push(fs, dest),
        F::VClip => {
            push(fs, 0xe);
            push(ft, 0x1);
        }
        _ => {}
    }
    if hi >> 31 == 0 {
        let (lfs, lft) = (((lo >> 11) & 31) as usize, ((lo >> 16) & 31) as usize);
        let ldest = ((lo >> 21) & 15) as u8;
        let (fsf, ftf) = (8u8 >> ((lo >> 21) & 3), 8u8 >> ((lo >> 23) & 3));
        match lo >> 25 {
            0x01 => push(lfs, ldest),
            0x40 if lo & 63 >= 0x3c => match ((lo >> 6) & 31) << 2 | (lo & 3) {
                0x30 | 0x35 | 0x37 => push(lfs, ldest),
                0x31 => push(lfs, 0xf),
                0x38 | 0x3a => {
                    push(lfs, fsf);
                    push(lft, ftf);
                }
                0x39 => push(lft, ftf),
                0x3c | 0x42 | 0x43 | 0x78..=0x7e => push(lfs, fsf),
                0x70..=0x76 => push(lfs, 0xf),
                _ => {}
            },
            _ => {}
        }
    }
    out
}

fn vf(n: u32) -> String {
    if n == 0 {
        "vfz".into()
    } else {
        format!("vf{n}")
    }
}

fn vi(n: u32) -> String {
    match n & 15 {
        0 => "0u16".into(),
        n => format!("vi{n}"),
    }
}

/// Assign the `dest` fields of `src` to VF register `reg`.
fn assign(reg: u32, dest: u8, src: &str) -> String {
    match (reg, dest) {
        (0, _) | (_, 0) => String::new(),
        (_, 0xf) => format!("vf{reg} = {src};"),
        _ => format!("vf{reg} = vx::blend::<{dest:#x}>(vf{reg}, {src});"),
    }
}

/// Upper instruction: code computing `u`, and where it goes.
enum UpOut {
    Fd(u32),
    Acc,
    Ft(u32),
    Clip,
    None,
}

fn upper(hi: u32) -> (String, UpOut, bool) {
    let i = Insn { raw: 0x4a00_0000 | (hi & 0x01ff_ffff), m: decode(0x4a00_0000 | (hi & 0x01ff_ffff)).m };
    let (fs, ft, fd) = (i.fs() as u32, i.ft() as u32, i.fd() as u32);
    let (s, t) = (vf(fs), vf(ft));
    let bc = format!("{t}.bc::<{}>()", i.bc());
    let (q, ii) = ("X::splat(q)".to_string(), "X::splat(i)".to_string());
    use UpOut::*;
    let (expr, out, flags) = match i.m {
        M::VaddBc => (format!("vx::add({s}, {bc})"), Fd(fd), true),
        M::VsubBc => (format!("vx::sub({s}, {bc})"), Fd(fd), true),
        M::VmulBc => (format!("vx::mul({s}, {bc})"), Fd(fd), true),
        M::VmaddBc => (format!("vx::madd(acc, {s}, {bc})"), Fd(fd), true),
        M::VmsubBc => (format!("vx::msub(acc, {s}, {bc})"), Fd(fd), true),
        M::VmaxBc => (format!("vx::max({s}, {bc})"), Fd(fd), false),
        M::VminiBc => (format!("vx::mini({s}, {bc})"), Fd(fd), false),
        M::Vadd => (format!("vx::add({s}, {t})"), Fd(fd), true),
        M::Vsub => (format!("vx::sub({s}, {t})"), Fd(fd), true),
        M::Vmul => (format!("vx::mul({s}, {t})"), Fd(fd), true),
        M::Vmadd => (format!("vx::madd(acc, {s}, {t})"), Fd(fd), true),
        M::Vmsub => (format!("vx::msub(acc, {s}, {t})"), Fd(fd), true),
        M::Vmax => (format!("vx::max({s}, {t})"), Fd(fd), false),
        M::Vmini => (format!("vx::mini({s}, {t})"), Fd(fd), false),
        M::Vaddq => (format!("vx::add({s}, {q})"), Fd(fd), true),
        M::Vsubq => (format!("vx::sub({s}, {q})"), Fd(fd), true),
        M::Vmulq => (format!("vx::mul({s}, {q})"), Fd(fd), true),
        M::Vmaddq => (format!("vx::madd(acc, {s}, {q})"), Fd(fd), true),
        M::Vmsubq => (format!("vx::msub(acc, {s}, {q})"), Fd(fd), true),
        M::Vaddi => (format!("vx::add({s}, {ii})"), Fd(fd), true),
        M::Vsubi => (format!("vx::sub({s}, {ii})"), Fd(fd), true),
        M::Vmuli => (format!("vx::mul({s}, {ii})"), Fd(fd), true),
        M::Vmaddi => (format!("vx::madd(acc, {s}, {ii})"), Fd(fd), true),
        M::Vmsubi => (format!("vx::msub(acc, {s}, {ii})"), Fd(fd), true),
        M::Vmaxi => (format!("vx::max({s}, {ii})"), Fd(fd), false),
        M::Vminii => (format!("vx::mini({s}, {ii})"), Fd(fd), false),
        M::Vopmsub => (format!("vx::sub(acc, vx::cross({s}, {t}))"), Fd(fd), true),
        M::VaddaBc => (format!("vx::add({s}, {bc})"), Acc, true),
        M::VsubaBc => (format!("vx::sub({s}, {bc})"), Acc, true),
        M::VmulaBc => (format!("vx::mul({s}, {bc})"), Acc, true),
        M::VmaddaBc => (format!("vx::madd(acc, {s}, {bc})"), Acc, true),
        M::VmsubaBc => (format!("vx::msub(acc, {s}, {bc})"), Acc, true),
        M::Vadda => (format!("vx::add({s}, {t})"), Acc, true),
        M::Vsuba => (format!("vx::sub({s}, {t})"), Acc, true),
        M::Vmula => (format!("vx::mul({s}, {t})"), Acc, true),
        M::Vmadda => (format!("vx::madd(acc, {s}, {t})"), Acc, true),
        M::Vmsuba => (format!("vx::msub(acc, {s}, {t})"), Acc, true),
        M::Vaddaq => (format!("vx::add({s}, {q})"), Acc, true),
        M::Vsubaq => (format!("vx::sub({s}, {q})"), Acc, true),
        M::Vmulaq => (format!("vx::mul({s}, {q})"), Acc, true),
        M::Vmaddaq => (format!("vx::madd(acc, {s}, {q})"), Acc, true),
        M::Vmsubaq => (format!("vx::msub(acc, {s}, {q})"), Acc, true),
        M::Vaddai => (format!("vx::add({s}, {ii})"), Acc, true),
        M::Vsubai => (format!("vx::sub({s}, {ii})"), Acc, true),
        M::Vmulai => (format!("vx::mul({s}, {ii})"), Acc, true),
        M::Vmaddai => (format!("vx::madd(acc, {s}, {ii})"), Acc, true),
        M::Vmsubai => (format!("vx::msub(acc, {s}, {ii})"), Acc, true),
        M::Vopmula => (format!("vx::cross({s}, {t})"), Acc, true),
        M::Vabs => (format!("vx::abs({s})"), Ft(ft), false),
        M::Vitof0 => (format!("vx::itof({s}, 0)"), Ft(ft), false),
        M::Vitof4 => (format!("vx::itof({s}, 4)"), Ft(ft), false),
        M::Vitof12 => (format!("vx::itof({s}, 12)"), Ft(ft), false),
        M::Vitof15 => (format!("vx::itof({s}, 15)"), Ft(ft), false),
        M::Vftoi0 => (format!("vx::ftoi({s}, 0)"), Ft(ft), false),
        M::Vftoi4 => (format!("vx::ftoi({s}, 4)"), Ft(ft), false),
        M::Vftoi12 => (format!("vx::ftoi({s}, 12)"), Ft(ft), false),
        M::Vftoi15 => (format!("vx::ftoi({s}, 15)"), Ft(ft), false),
        M::Vclipw => (format!("vx::clip_judge({s}, {t})"), Clip, false),
        _ => (String::new(), None, false),
    };
    (expr, out, flags)
}

/// Branch at the end of a version.
enum Br {
    /// Static target; `cond` is an expression over locals (None: always).
    To(u32, Option<String>),
    /// JR/JALR at the given address to the value of `jt`, or to a known
    /// constant.
    Dyn(u32, Option<u32>),
}

/// The VU-image compiler for one image.
pub struct Compiler<'a> {
    img: &'a Image,
    mask: u32,
    /// Registers used as JR/JALR targets (bit n = vi n).
    link_regs: u16,
    /// The image reads MAC, so the final MAC must be exact.
    mac_exit: bool,
    starts: std::collections::BTreeSet<u32>,
}

/// Registers a compiled function touches.
#[derive(Default)]
struct Used {
    vf: u32,
    vf_w: u32,
    vi: u16,
    vi_w: u16,
}

impl<'a> Compiler<'a> {
    pub fn new(img: &'a Image) -> Option<Compiler<'a>> {
        let mask = img.mask();
        let mut link_regs = 0u16;
        let mut mac_exit = false;
        for pc in 0..=mask {
            let (lo, hi) = img.words(pc);
            if reads_status(lo, hi) {
                return None;
            }
            mac_exit |= reads_mac(lo, hi);
            if hi >> 31 == 0 && matches!(lo >> 25, 0x24 | 0x25) {
                link_regs |= 1 << ((lo >> 11) & 15);
            }
        }
        Some(Compiler { img, mask, link_regs: link_regs & !1, mac_exit, starts: Default::default() })
    }

    /// Compile the function for `entry`, or None to use the pair-calling code.
    pub fn function(&mut self, entry: u32, name: &str) -> Option<String> {
        self.starts = self.img.blocks(entry);
        // Branches or E bits in delay slots are left to the pair calls.
        for &b in &self.starts {
            let mut pc = b;
            for _ in 0..=self.mask {
                let (lo, hi) = self.img.words(pc);
                if is_branch(lo, hi) || hi & (1 << 30) != 0 {
                    let (dlo, dhi) = self.img.words((pc + 1) & self.mask);
                    if is_branch(dlo, dhi) || dhi & (1 << 30) != 0 {
                        return None;
                    }
                    break;
                }
                pc = (pc + 1) & self.mask;
            }
        }
        // Explore versions.
        let mut ids: HashMap<(u32, State), usize> = HashMap::new();
        let mut list: Vec<(u32, State)> = Vec::new();
        let mut walked: Vec<Walked> = Vec::new();
        ids.insert((entry, State::empty()), 0);
        list.push((entry, State::empty()));
        let mut n = 0;
        while n < list.len() {
            if list.len() > MAX_VERSIONS {
                eprintln!("vu: {name}: over {MAX_VERSIONS} versions; using pair calls");
                return None;
            }
            let (pc, st) = list[n].clone();
            let w = self.walk(pc, &st, None, &mut |_, _| String::new())?;
            for e in &w.exits {
                if let Exit::Goto(pc, s) = e {
                    if !ids.contains_key(&(*pc, s.clone())) {
                        ids.insert((*pc, s.clone()), list.len());
                        list.push((*pc, s.clone()));
                    }
                }
            }
            walked.push(w);
            n += 1;
        }
        // MAC liveness: live-in sets per version, to a fixed point.
        let succ: Vec<Vec<Option<usize>>> = walked
            .iter()
            .map(|w| {
                w.exits
                    .iter()
                    .map(|e| match e {
                        Exit::Goto(pc, s) => Some(ids[&(*pc, s.clone())]),
                        Exit::Finish => None,
                    })
                    .collect()
            })
            .collect();
        let mut live_in = vec![0u32; list.len()];
        let mut live_defs: Vec<Vec<bool>> = vec![Vec::new(); list.len()];
        loop {
            let mut changed = false;
            for v in (0..list.len()).rev() {
                let mut l = succ[v].iter().flatten().fold(0, |a, &s| a | live_in[s]);
                let (inn, defs) = backward(&walked[v].mac, l, list[v].1.mac.len());
                l = inn;
                live_defs[v] = defs;
                if l != live_in[v] {
                    live_in[v] = l;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Emit.
        let mut used = Used::default();
        let mut body = String::new();
        for (v, (pc, st)) in list.iter().enumerate() {
            let mut goto = |pc: u32, s: State| format!("{{ v = {}; continue; }}", ids[&(pc, s)]);
            let w = self.walk(*pc, st, Some((&live_defs[v], &mut used)), &mut goto)?;
            let code: String = w.code.lines().map(|l| format!("                {l}\n")).collect();
            let _ = writeln!(body, "            {v} => {{ // {pc:#x}\n{code}            }}");
        }
        Some(self.wrap(name, &used, &body, list.len()))
    }

    fn wrap(&self, name: &str, used: &Used, body: &str, versions: usize) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "// {versions} versions");
        let _ = writeln!(out, "fn {name}(c: &mut Ctx) {{");
        let mut store = String::new();
        for n in 1..32 {
            if used.vf & (1 << n) != 0 {
                let _ = writeln!(out, "    let mut vf{n}: X = X::load(c.r.vf[{n}]);");
            }
            if used.vf_w & (1 << n) != 0 {
                let _ = write!(store, "c.r.vf[{n}] = vf{n}.get(); ");
            }
        }
        for n in 1..16 {
            if used.vi & (1 << n) != 0 {
                let _ = writeln!(out, "    let mut vi{n}: u16 = c.r.vi[{n}];");
            }
            if used.vi_w & (1 << n) != 0 {
                let _ = write!(store, "c.r.vi[{n}] = vi{n}; ");
            }
        }
        out.push_str(
            "    let vfz = X::load([0.0, 0.0, 0.0, 1.0]);\n\
             \x20   let (mut acc, mut q, mut i, mut p, mut rr) = (X::load(c.r.acc), c.r.q, c.r.i, c.r.p, c.r.r);\n\
             \x20   let (mut mac, mut clip) = (c.r.mac, c.r.clip);\n\
             \x20   let mut qp = 0f32;\n\
             \x20   let mut vio = 0u16;\n\
             \x20   let mut cyc = 0u64;\n",
        );
        for k in 0..MAX_PENDING {
            let _ = writeln!(out, "    let (mut mac_p{k}, mut clip_p{k}) = (0u32, 0u32);");
        }
        let _ = writeln!(
            out,
            "    macro_rules! store {{ () => {{ {store}c.r.acc = acc.get(); c.r.q = q; c.r.i = i; c.r.p = p; c.r.r = rr; c.r.mac = mac; c.r.clip = clip; c.cycle = cyc; }} }}"
        );
        let _ = writeln!(out, "    let mut v = 0usize;\n    loop {{\n        match v {{");
        out.push_str(body);
        let _ = writeln!(out, "            _ => unreachable!(),\n        }}\n    }}\n}}\n");
        out
    }

    /// Simulate (and with `emit`, generate code for) one version.
    fn walk(&self, pc0: u32, st: &State, mut emit: Option<(&Vec<bool>, &mut Used)>, goto: &mut dyn FnMut(u32, State) -> String) -> Option<Walked> {
        let mask = self.mask;
        let mut s = Sim::new(st);
        let mut code = String::new();
        let mut mac = Vec::new();
        let mut exits = Vec::new();
        let mut def_n = 0usize;
        let mut pc = pc0;
        // Branch pending after the delay slot, and whether this pair ends.
        let mut branch: Option<Br> = None;
        let mut in_delay = false;
        let mut ending = false;
        let mut count = 0;
        loop {
            count += 1;
            if count > 4 * (mask as usize + 1) {
                return None;
            }
            // A new block starts here: continue in its version.
            if pc != pc0 && !in_delay && !ending && self.starts.contains(&pc) {
                let c = self.commit_due(&mut s, &mut mac);
                code.push_str(&c);
                let _ = writeln!(code, "cyc += {};", s.t);
                let next = s.state();
                exits.push(Exit::Goto(pc, next.clone()));
                code.push_str(&goto(pc, next));
                code.push('\n');
                break;
            }
            let (lo, hi) = self.img.words(pc);
            let m = decode(0x4a00_0000 | (hi & 0x01ff_ffff)).m;
            let _ = writeln!(code, "// {pc:#05x}: {hi:08x} {lo:08x} {m:?}");
            // ---- stalls ----
            let mut issue = s.t;
            for (reg, fm) in reads(lo, hi, m) {
                for f in 0..4 {
                    if fm & (8 >> f) != 0 {
                        issue = issue.max(s.ready[reg][f]);
                    }
                }
            }
            let lower_op = if hi >> 31 == 0 { lo >> 25 } else { 0x7f };
            let special = if lower_op == 0x40 && lo & 63 >= 0x3c { ((lo >> 6) & 31) << 2 | (lo & 3) } else { 0 };
            if matches!(special, 0x38..=0x3b) {
                if let Some(qr) = s.q {
                    issue = issue.max(qr);
                }
            }
            s.t = issue;
            code.push_str(&self.commit_due(&mut s, &mut mac));

            // ---- lower ----
            let mut lower_vf: Option<(u32, u8)> = None;
            let mut vi_write: Option<(u32, Option<u16>)> = None; // (reg, known new value)
            let flag_op = hi >> 31 == 0 && (0x10..=0x1c).contains(&lower_op);
            let mut l = String::new();
            let dest = ((lo >> 21) & 15) as u8;
            let (it, is, id) = ((lo >> 16) & 31, (lo >> 11) & 31, (lo >> 6) & 31);
            let imm11 = ((lo & 0x7ff) as i32) << 21 >> 21;
            let target = (pc as i32 + 1 + imm11) as u32 & mask;
            // Branch operands see the old value of the last pair's VI write.
            let (last_vi, last_old, link) = (s.last_vi as u32, s.last_old, s.link);
            let vib = move |n: u32| if n & 15 != 0 && last_vi == n & 15 { "vio".to_string() } else { vi(n) };
            let vib_known = move |n: u32| match n & 15 {
                0 => Some(0),
                n if n == last_vi => last_old,
                n => link[n as usize],
            };
            let mut wvi = |l: &mut String, n: u32, val: String, known: Option<u16>| {
                let n = n & 15;
                if n != 0 {
                    let _ = writeln!(l, "vio = vi{n}; vi{n} = {val};");
                    vi_write = Some((n, known));
                }
            };
            let fsf = (lo >> 21) & 3;
            let ftf = (lo >> 23) & 3;
            let dest_field = (0..4).find(|n| dest & (8 >> n) != 0).unwrap_or(0);
            if hi >> 31 != 0 {
                // I bit: the lower word is a float for I (after the upper).
            } else {
                match lower_op {
                    0x00 => {
                        let _ = writeln!(l, "let lv = X::load(c.lq(({} as i32 + {imm11}) as usize));", vi(is));
                        lower_vf = Some((it, dest));
                    }
                    0x01 => {
                        let _ = writeln!(l, "c.sq(({} as i32 + {imm11}) as usize, {dest:#x}, {}.get());", vi(it), vf(is));
                    }
                    0x04 => {
                        let v = format!("c.lw(({} as i32 + {imm11}) as usize, {dest_field}) as u16", vi(is));
                        wvi(&mut l, it, v, None);
                    }
                    0x05 => {
                        let _ = writeln!(l, "{{ let a = ({} as i32 + {imm11}) as usize; let v = {} as u32;", vi(is), vi(it));
                        for n in (0..4).filter(|n| dest & (8 >> n) != 0) {
                            let _ = write!(l, " c.sw(a, {n}, v);");
                        }
                        l.push_str(" }\n");
                    }
                    0x08 | 0x09 => {
                        let imm15 = (lo & 0x7ff) | ((lo >> 21) & 15) << 11;
                        let f = if lower_op == 0x08 { "wrapping_add" } else { "wrapping_sub" };
                        wvi(&mut l, it, format!("{}.{f}({imm15:#x})", vi(is)), None);
                    }
                    0x10 => wvi(&mut l, 1, format!("u16::from(clip & 0xff_ffff == {:#x})", lo & 0xff_ffff), None),
                    0x11 => {
                        // FCSET drops pending clip writes.
                        s.clip.clear();
                        let _ = writeln!(l, "clip = {:#x};", lo & 0xff_ffff);
                    }
                    0x12 => wvi(&mut l, 1, format!("u16::from(clip & {:#x} != 0)", lo & 0xff_ffff), None),
                    0x13 => wvi(&mut l, 1, format!("u16::from((clip | {:#x}) & 0xff_ffff == 0xff_ffff)", lo & 0xff_ffff), None),
                    0x18 | 0x1a | 0x1b => {
                        mac.push(MacEv::Use);
                        let v = match lower_op {
                            0x18 => format!("u16::from(mac as u16 == {})", vi(is)),
                            0x1a => format!("mac as u16 & {}", vi(is)),
                            _ => format!("u16::from((mac as u16 | {}) == 0xffff)", vi(is)),
                        };
                        wvi(&mut l, it, v, None);
                    }
                    0x1c => wvi(&mut l, it, "(clip & 0xfff) as u16".into(), None),
                    0x20 => branch = Some(Br::To(target, None)),
                    0x21 => {
                        let link = ((pc + 2) & mask) as u16;
                        wvi(&mut l, it, format!("{link:#x}"), Some(link));
                        branch = Some(Br::To(target, None));
                    }
                    0x24 | 0x25 => {
                        let known = vib_known(is);
                        if known.is_none() {
                            let _ = writeln!(l, "let jt = {} as u32 & {mask:#x};", vib(is));
                        }
                        if lower_op == 0x25 {
                            let link = ((pc + 2) & mask) as u16;
                            wvi(&mut l, it, format!("{link:#x}"), Some(link));
                        }
                        branch = Some(Br::Dyn(pc, known.map(|k| k as u32 & mask)));
                    }
                    0x28..=0x2f => {
                        let (a, b) = (format!("({} as i16)", vib(it)), format!("({} as i16)", vib(is)));
                        let cond = match lower_op {
                            0x28 => format!("{a} == {b}"),
                            0x29 => format!("{a} != {b}"),
                            0x2c => format!("{b} < 0"),
                            0x2d => format!("{b} > 0"),
                            0x2e => format!("{b} <= 0"),
                            0x2f => format!("{b} >= 0"),
                            _ => "false".into(),
                        };
                        let _ = writeln!(l, "let take = {cond};");
                        branch = Some(Br::To(target, Some("take".into())));
                    }
                    0x40 if lo & 63 < 0x3c => {
                        let d = (id & 15, vi(is), vi(it));
                        match lo & 63 {
                            0x30 => wvi(&mut l, d.0, format!("{}.wrapping_add({})", d.1, d.2), None),
                            0x31 => wvi(&mut l, d.0, format!("{}.wrapping_sub({})", d.1, d.2), None),
                            0x32 => {
                                let imm5 = ((id as i8) << 3 >> 3) as i16 as u16;
                                wvi(&mut l, it, format!("{}.wrapping_add({imm5:#x})", vi(is)), None);
                            }
                            0x34 => wvi(&mut l, d.0, format!("{} & {}", d.1, d.2), None),
                            0x35 => wvi(&mut l, d.0, format!("{} | {}", d.1, d.2), None),
                            _ => {}
                        }
                    }
                    0x40 => {
                        let (fs, ft) = (vf(is), vf(it));
                        match special {
                            0x30 => {
                                let _ = writeln!(l, "let lv = {fs};");
                                lower_vf = Some((it, dest));
                            }
                            0x31 => {
                                let _ = writeln!(l, "let lv = vx::mr32({fs});");
                                lower_vf = Some((it, dest));
                            }
                            0x34 => {
                                let _ = writeln!(l, "let lv = X::load(c.lq({} as usize));", vi(is));
                                lower_vf = Some((it, dest));
                                wvi(&mut l, is, format!("{}.wrapping_add(1)", vi(is)), None);
                            }
                            0x35 => {
                                let _ = writeln!(l, "c.sq({} as usize, {dest:#x}, {fs}.get());", vi(it));
                                wvi(&mut l, it, format!("{}.wrapping_add(1)", vi(it)), None);
                            }
                            0x36 => {
                                let _ = writeln!(l, "let a = {}.wrapping_sub(1); let lv = X::load(c.lq(a as usize));", vi(is));
                                lower_vf = Some((it, dest));
                                wvi(&mut l, is, "a".into(), None);
                            }
                            0x37 => {
                                let _ = writeln!(l, "let a = {}.wrapping_sub(1); c.sq(a as usize, {dest:#x}, {fs}.get());", vi(it));
                                wvi(&mut l, it, "a".into(), None);
                            }
                            0x38 | 0x39 | 0x3a => {
                                let (num, den) = (format!("{fs}.lane({fsf})"), format!("{ft}.lane({ftf})"));
                                let e = match special {
                                    0x38 => format!("fdiv({num}, {den})"),
                                    0x39 => format!("fsqrt({den})"),
                                    _ => format!("fdiv({num}, fsqrt({den}))"),
                                };
                                let _ = writeln!(l, "qp = {e};");
                                s.q = Some(s.t + if special == 0x3a { 13 } else { 7 });
                            }
                            0x3b => {} // WAITQ: the stall already landed Q
                            0x3c => wvi(&mut l, it, format!("{fs}.lane({fsf}).to_bits() as u16"), None),
                            0x3d => {
                                let _ = writeln!(l, "let lv = X::splat(f32::from_bits({} as i16 as i32 as u32));", vi(is));
                                lower_vf = Some((it, dest));
                            }
                            0x3e => wvi(&mut l, it, format!("c.lw({} as usize, {dest_field}) as u16", vi(is)), None),
                            0x3f => {
                                let _ = write!(l, "{{ let a = {} as usize; let v = {} as u32;", vi(is), vi(it));
                                for n in (0..4).filter(|n| dest & (8 >> n) != 0) {
                                    let _ = write!(l, " c.sw(a, {n}, v);");
                                }
                                l.push_str(" }\n");
                            }
                            0x40 => {
                                let _ = writeln!(l, "rr = rnext_value(rr); let lv = X::splat(f32::from_bits(rr));");
                                lower_vf = Some((it, dest));
                            }
                            0x41 => {
                                let _ = writeln!(l, "let lv = X::splat(f32::from_bits(rr));");
                                lower_vf = Some((it, dest));
                            }
                            0x42 => {
                                let _ = writeln!(l, "rr = 0x3f80_0000 | ({fs}.lane({fsf}).to_bits() & 0x7f_ffff);");
                            }
                            0x43 => {
                                let _ = writeln!(l, "rr = 0x3f80_0000 | ((rr ^ {fs}.lane({fsf}).to_bits()) & 0x7f_ffff);");
                            }
                            0x64 => {
                                let _ = writeln!(l, "let lv = X::splat(p);");
                                lower_vf = Some((it, dest));
                            }
                            0x68 => wvi(&mut l, it, "c.top() as u16".into(), None),
                            0x69 => wvi(&mut l, it, "c.itop() as u16".into(), None),
                            0x6c => {
                                let _ = writeln!(l, "c.xgkick({} as usize);", vi(is));
                            }
                            0x70..=0x7e => {
                                let _ = writeln!(l, "p = efu({special:#x}, {fs}.get(), {fsf});");
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            code.push_str(&l);

            // ---- upper ----
            let (expr, out, flags) = upper(hi);
            let udest = ((hi >> 21) & 15) as u8;
            let mut upper_reg = None;
            match out {
                UpOut::None => {}
                UpOut::Clip => {
                    if s.clip.len() >= MAX_PENDING {
                        return None;
                    }
                    let k = s.clip.len();
                    let latest = if k == 0 { "clip".to_string() } else { format!("clip_p{}", k - 1) };
                    let _ = writeln!(code, "clip_p{k} = (({latest} << 6) | {expr}) & 0xff_ffff;");
                    s.clip.push(s.t + FLAG_LATENCY);
                }
                _ => {
                    let _ = writeln!(code, "let u: X = {expr};");
                    match out {
                        UpOut::Fd(r) | UpOut::Ft(r) => {
                            code.push_str(&assign(r, udest, "u"));
                            code.push('\n');
                            upper_reg = Some(r);
                            if r != 0 {
                                if let Some((_, used)) = emit.as_mut() {
                                    used.vf_w |= 1 << r;
                                }
                            }
                            for f in 0..4 {
                                if udest & (8 >> f) != 0 {
                                    s.ready[r as usize][f] = s.t + FMAC_LATENCY;
                                }
                            }
                        }
                        UpOut::Acc => {
                            let _ = writeln!(code, "acc = vx::blend::<{udest:#x}>(acc, u);");
                        }
                        _ => {}
                    }
                    if flags {
                        if s.mac.len() >= MAX_PENDING {
                            return None;
                        }
                        let k = s.mac.len();
                        mac.push(MacEv::Def);
                        let live = emit.as_ref().map_or(true, |(defs, _)| defs[def_n]);
                        if live {
                            let _ = writeln!(code, "mac_p{k} = vx::mac_flags(u, {udest:#x});");
                        }
                        def_n += 1;
                        s.mac.push(s.t + FLAG_LATENCY);
                    }
                }
            }
            // Lower VF results land after the upper's unless both write the
            // same register, and readers stall for them as for FMAC results.
            if let Some((r, d)) = lower_vf {
                if upper_reg != Some(r) && r != 0 {
                    code.push_str(&assign(r, d, "lv"));
                    code.push('\n');
                    if let Some((_, used)) = emit.as_mut() {
                        used.vf_w |= 1 << r;
                    }
                    for f in 0..4 {
                        if d & (8 >> f) != 0 {
                            s.ready[r as usize][f] = s.t + FMAC_LATENCY;
                        }
                    }
                }
            }
            if hi >> 31 != 0 {
                let _ = writeln!(code, "i = f32::from_bits({lo:#010x});");
            }
            if let (Some((_, used)), Some((n, _))) = (emit.as_mut(), vi_write) {
                used.vi_w |= 1 << n;
            }
            // VI write bookkeeping for branches and link constants.
            let new_last = match vi_write {
                Some((n, known)) => {
                    let prev_known = if self.link_regs & (1 << n) != 0 { s.link[n as usize] } else { None };
                    s.link[n as usize] = if self.link_regs & (1 << n) != 0 { known } else { None };
                    if flag_op {
                        s.last_old = None;
                        0
                    } else {
                        s.last_old = prev_known;
                        n as u8
                    }
                }
                None => {
                    s.last_old = None;
                    0
                }
            };
            s.last_vi = new_last;
            s.t += 1;

            // ---- control flow ----
            if in_delay || ending {
                let br = branch.take();
                code.push_str(&self.commit_due(&mut s, &mut mac));
                let _ = writeln!(code, "cyc += {};", s.t);
                if ending {
                    // Pair after the E bit: finish. A branch in the E pair
                    // decides where MSCNT resumes.
                    let next = (pc + 1) & mask;
                    let to = match br {
                        None => format!("{next:#x}"),
                        Some(Br::To(t, None)) => format!("{t:#x}"),
                        Some(Br::To(t, Some(c))) => format!("if {c} {{ {t:#x} }} else {{ {next:#x} }}"),
                        Some(Br::Dyn(_, Some(k))) => format!("{k:#x}"),
                        Some(Br::Dyn(_, None)) => "jt".into(),
                    };
                    code.push_str(&self.finish(&mut s, &mut mac, &to));
                    exits.push(Exit::Finish);
                    break;
                }
                let next = (pc + 1) & mask;
                let st = s.state();
                match br {
                    Some(Br::To(t, None)) | Some(Br::Dyn(_, Some(t))) => {
                        exits.push(Exit::Goto(t, st.clone()));
                        code.push_str(&goto(t, st));
                        code.push('\n');
                    }
                    Some(Br::To(t, Some(c))) => {
                        exits.push(Exit::Goto(t, st.clone()));
                        exits.push(Exit::Goto(next, st.clone()));
                        let _ = writeln!(code, "if {c} {} else {}", goto(t, st.clone()), goto(next, st));
                    }
                    Some(Br::Dyn(site, None)) => {
                        // Targets seen at run time get versions; others go
                        // to the interpreter.
                        code.push_str("match jt {\n");
                        for &t in self.img.jumps.get(&site).into_iter().flatten() {
                            exits.push(Exit::Goto(t, st.clone()));
                            let _ = writeln!(code, "    {t:#x} => {}", goto(t, st.clone()));
                        }
                        let _ = writeln!(code, "    _ => {{\n{}    }}\n}}", self.deopt(&s, &st, &mut mac, site, "jt"));
                        exits.push(Exit::Finish);
                    }
                    _ => unreachable!(),
                }
                break;
            }
            if branch.is_some() {
                in_delay = true;
            }
            if hi & (1 << 30) != 0 {
                ending = true;
            }
            pc = (pc + 1) & mask;
        }
        // Registers the code mentions (loaded at entry).
        if let Some((_, used)) = emit.as_mut() {
            mark_used(&code, used);
        }
        Some(Walked { code, mac, exits })
    }

    /// Land everything due by the current cycle.
    fn commit_due(&self, s: &mut Sim, mac: &mut Vec<MacEv>) -> String {
        let mut out = String::new();
        if s.q.is_some_and(|r| r <= s.t) {
            out.push_str("q = qp;\n");
            s.q = None;
        }
        let n = s.mac.iter().take_while(|&&r| r <= s.t).count();
        if n > 0 {
            out.push_str(&shift("mac", n, s.mac.len()));
            mac.push(MacEv::Commit(n));
            s.mac.drain(..n);
        }
        let n = s.clip.iter().take_while(|&&r| r <= s.t).count();
        if n > 0 {
            out.push_str(&shift("clip", n, s.clip.len()));
            s.clip.drain(..n);
        }
        out
    }

    /// End of the program: land everything pending, store, set TPC.
    fn finish(&self, s: &mut Sim, mac: &mut Vec<MacEv>, to: &str) -> String {
        let mut out = String::new();
        if s.q.is_some() {
            out.push_str("q = qp;\n");
        }
        if !s.mac.is_empty() {
            out.push_str(&shift("mac", s.mac.len(), s.mac.len()));
            mac.push(MacEv::Commit(s.mac.len()));
        }
        if self.mac_exit {
            mac.push(MacEv::Use);
        }
        if !s.clip.is_empty() {
            out.push_str(&shift("clip", s.clip.len(), s.clip.len()));
        }
        let _ = writeln!(out, "let to: u32 = {to};\nstore!();\nc.finish(to);\nreturn;");
        out
    }

    /// Hand over to the interpreter at `jt`, with the pipeline state.
    fn deopt(&self, s: &Sim, st: &State, mac: &mut Vec<MacEv>, site: u32, to: &str) -> String {
        mac.push(MacEv::UseAll);
        let ready: Vec<String> = (0..32)
            .flat_map(|r| (0..4).map(move |f| (r, f)))
            .filter(|&(r, f)| st.ready[r][f] > 0)
            .map(|(r, f)| format!("({r}, {f}, {})", st.ready[r][f]))
            .collect();
        let q = if st.q > 0 { format!("Some(({}, qp))", st.q) } else { "None".into() };
        let macs: Vec<String> = st.mac.iter().enumerate().map(|(k, d)| format!("({d}, mac_p{k})")).collect();
        let clips: Vec<String> = st.clip.iter().enumerate().map(|(k, d)| format!("({d}, clip_p{k})")).collect();
        let last = if s.last_vi > 0 { format!("Some(({}, vio))", s.last_vi) } else { "None".into() };
        format!(
            "store!();\nc.resume({site:#x}, {to}, &[{}], {q}, &[{}], &[{}], {last});\nreturn;\n",
            ready.join(", "),
            macs.join(", "),
            clips.join(", ")
        )
    }
}

/// Land the oldest `n` of `len` pending writes to `reg`.
fn shift(reg: &str, n: usize, len: usize) -> String {
    let mut out = format!("{reg} = {reg}_p{};", n - 1);
    for j in 0..len - n {
        let _ = write!(out, " {reg}_p{j} = {reg}_p{};", j + n);
    }
    out.push('\n');
    out
}

/// Backward MAC liveness through one version. `out` is the live set after
/// it; returns the live set at entry and which defs are live, in order.
fn backward(evs: &[MacEv], out: u32, entry_pending: usize) -> (u32, Vec<bool>) {
    // Pending count before each event, forwards.
    let mut pend = Vec::with_capacity(evs.len());
    let mut p = entry_pending;
    for e in evs {
        pend.push(p);
        match e {
            MacEv::Commit(n) => p -= n,
            MacEv::Def => p += 1,
            _ => {}
        }
    }
    let mut l = out;
    let mut defs = Vec::new();
    for (e, &p) in evs.iter().zip(&pend).rev() {
        match *e {
            MacEv::Use => l |= 1,
            MacEv::UseAll => l |= (1 << (p + 1)) - 1,
            MacEv::Def => {
                let bit = 1 << (1 + p);
                defs.push(l & bit != 0);
                l &= !bit;
            }
            MacEv::Commit(n) => {
                // After: reg = old P[n-1], P[j] = old P[j+n].
                let reg = l & 1;
                let pend_after = l >> 1;
                l = (reg << n) | (pend_after << (n + 1));
            }
        }
    }
    defs.reverse();
    (l, defs)
}

/// Note which VF/VI locals a piece of code mentions.
fn mark_used(code: &str, used: &mut Used) {
    let b = code.as_bytes();
    let mut i = 0;
    while i + 2 < b.len() {
        let pre = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if pre && b[i] == b'v' && (b[i + 1] == b'f' || b[i + 1] == b'i') && b[i + 2].is_ascii_digit() {
            let mut j = i + 2;
            let mut n = 0u32;
            while j < b.len() && b[j].is_ascii_digit() {
                n = n * 10 + (b[j] - b'0') as u32;
                j += 1;
            }
            if b[i + 1] == b'f' {
                used.vf |= 1 << n;
            } else {
                used.vi |= 1 << n;
            }
            i = j;
        } else {
            i += 1;
        }
    }
}
