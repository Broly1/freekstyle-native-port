//! Recursive-descent function discovery over the EE executable.
//!
//! Starts from the ELF entry point and follows `jal`/`bal` call targets,
//! branches and resolvable jump tables. A second phase seeds functions that
//! are only reached through pointers: code addresses built with `lui`/`addiu`,
//! and words in unexplored regions that look like function starts. Phases
//! repeat until nothing new is found.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use r5900::{decode, Insn, Mnemonic};

use crate::elf::Elf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Entry,
    Call,
    TailJump,
    CodePointer,
    AfterReturn,
    Prologue,
}

pub struct JumpTable {
    pub jr: u32,
    pub table: u32,
    pub bounded: bool,
    pub targets: Vec<u32>,
}

pub struct Function {
    pub start: u32,
    pub origin: Origin,
    pub insns: BTreeSet<u32>,
    pub calls: BTreeSet<u32>,
    pub indirect_calls: Vec<u32>,
    pub jump_tables: Vec<JumpTable>,
    pub unresolved_jumps: Vec<u32>,
    pub invalid: Vec<u32>,
    /// (pc, syscall number if `$v1` was a known constant).
    pub syscalls: Vec<(u32, Option<u32>)>,
}

pub struct Analysis {
    pub code_lo: u32,
    pub code_hi: u32,
    pub functions: BTreeMap<u32, Function>,
    pub covered: HashSet<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Val {
    Unknown,
    Const(u32),
    /// A known base plus some runtime index.
    Indexed(u32),
    /// A word loaded from a known table address plus an index.
    TableLoad(u32),
}

const RA: u8 = 31;
const RA_IDX: usize = RA as usize;
const SP: u8 = 29;
const V1: u8 = 3;
const JR_RA: u32 = 0x03e0_0008;

struct Walker<'a> {
    elf: &'a Elf,
    code_lo: u32,
    code_hi: u32,
}

impl Walker<'_> {
    fn in_code(&self, a: u32) -> bool {
        a % 4 == 0 && (self.code_lo..self.code_hi).contains(&a)
    }

    fn insn(&self, pc: u32) -> Insn {
        decode(self.elf.read_u32(pc).unwrap_or(0xffff_ffff))
    }

    fn read_table(&self, table: u32, bound: Option<u32>) -> Vec<u32> {
        let limit = bound.unwrap_or(1024);
        let mut out = Vec::new();
        for n in 0..limit {
            match self.elf.read_u32(table + n * 4) {
                Some(t) if self.in_code(t) => out.push(t),
                _ => break,
            }
        }
        out
    }

    /// Explore one function body. Newly found call targets and code
    /// pointers are appended to `found`.
    fn explore(&self, start: u32, origin: Origin, known: &BTreeMap<u32, Function>, found: &mut Vec<(u32, Origin)>) -> Function {
        let mut f = Function {
            start,
            origin,
            insns: BTreeSet::new(),
            calls: BTreeSet::new(),
            indirect_calls: Vec::new(),
            jump_tables: Vec::new(),
            unresolved_jumps: Vec::new(),
            invalid: Vec::new(),
            syscalls: Vec::new(),
        };
        let mut work = vec![start];
        while let Some(block) = work.pop() {
            let mut pc = block;
            let mut regs = [Val::Unknown; 32];
            regs[0] = Val::Const(0);
            let mut bound = None;
            loop {
                if !self.in_code(pc) || !f.insns.insert(pc) {
                    break;
                }
                let i = self.insn(pc);
                if i.m == Mnemonic::Invalid {
                    f.invalid.push(pc);
                    break;
                }
                if !i.has_delay_slot() {
                    match i.m {
                        Mnemonic::Eret => break,
                        Mnemonic::Syscall => {
                            let n = match regs[V1 as usize] {
                                Val::Const(n) => Some(n),
                                _ => None,
                            };
                            f.syscalls.push((pc, n));
                        }
                        Mnemonic::Sltiu => bound = Some(i.imm() as i32 as u32),
                        _ => {}
                    }
                    self.track(&i, &mut regs, found);
                    pc += 4;
                    continue;
                }

                // Control transfer: the delay slot executes first.
                let slot = pc + 4;
                if self.in_code(slot) {
                    f.insns.insert(slot);
                    let s = self.insn(slot);
                    if s.m == Mnemonic::Invalid {
                        f.invalid.push(slot);
                    }
                    self.track(&s, &mut regs, found);
                }
                use Mnemonic::*;
                match i.m {
                    Jal => {
                        let t = i.jump_target(pc).unwrap();
                        f.calls.insert(t);
                        found.push((t, Origin::Call));
                        clobber_call(&mut regs);
                        pc += 8;
                    }
                    Bltzal | Bgezal | Bltzall | Bgezall => {
                        let t = i.branch_target(pc).unwrap();
                        f.calls.insert(t);
                        found.push((t, Origin::Call));
                        clobber_call(&mut regs);
                        pc += 8;
                    }
                    Jalr => {
                        f.indirect_calls.push(pc);
                        clobber_call(&mut regs);
                        pc += 8;
                    }
                    J => {
                        let t = i.jump_target(pc).unwrap();
                        if known.contains_key(&t) {
                            f.calls.insert(t);
                            found.push((t, Origin::TailJump));
                        } else {
                            work.push(t);
                        }
                        break;
                    }
                    Jr if i.rs() == RA => break,
                    Jr => {
                        if let Val::TableLoad(table) = regs[i.rs() as usize] {
                            let targets = self.read_table(table, bound);
                            work.extend(&targets);
                            f.jump_tables.push(JumpTable { jr: pc, table, bounded: bound.is_some(), targets });
                        } else {
                            f.unresolved_jumps.push(pc);
                        }
                        break;
                    }
                    _ => {
                        let t = i.branch_target(pc).unwrap();
                        work.push(t);
                        if i.is_unconditional_branch() {
                            break;
                        }
                        pc += 8;
                    }
                }
            }
        }
        f
    }

    /// Minimal constant propagation: enough to resolve `lui`/`addiu`
    /// address pairs, syscall numbers and switch jump tables.
    fn track(&self, i: &Insn, regs: &mut [Val; 32], found: &mut Vec<(u32, Origin)>) {
        use Mnemonic::*;
        let get = |r: u8| regs[r as usize];
        let (dst, val) = match i.m {
            Lui => (i.rt(), Val::Const((i.uimm() as u32) << 16)),
            Addiu | Daddiu => (
                i.rt(),
                match get(i.rs()) {
                    Val::Const(c) => {
                        let v = c.wrapping_add(i.imm() as i32 as u32);
                        if i.rs() != 0 && i.rt() != SP && self.in_code(v) {
                            found.push((v, Origin::CodePointer));
                        }
                        Val::Const(v)
                    }
                    _ => Val::Unknown,
                },
            ),
            Ori => (
                i.rt(),
                match get(i.rs()) {
                    Val::Const(c) => Val::Const(c | i.uimm() as u32),
                    _ => Val::Unknown,
                },
            ),
            Addu | Daddu => (
                i.rd(),
                match (get(i.rs()), get(i.rt())) {
                    (Val::Const(a), Val::Const(b)) => Val::Const(a.wrapping_add(b)),
                    (Val::Const(a), Val::Unknown) | (Val::Unknown, Val::Const(a)) => Val::Indexed(a),
                    _ => Val::Unknown,
                },
            ),
            Lw => (
                i.rt(),
                match get(i.rs()) {
                    Val::Indexed(a) => Val::TableLoad(a.wrapping_add(i.imm() as i32 as u32)),
                    _ => Val::Unknown,
                },
            ),
            _ => {
                if let Some(d) = written_gpr(i) {
                    regs[d as usize] = Val::Unknown;
                }
                return;
            }
        };
        if dst != 0 {
            regs[dst as usize] = val;
        }
    }
}

/// After a call only callee-saved registers keep their values.
fn clobber_call(regs: &mut [Val; 32]) {
    for r in (1..16).chain(24..26).chain([RA_IDX]) {
        regs[r] = Val::Unknown;
    }
}

/// The GPR an instruction writes, for invalidating tracked constants.
fn written_gpr(i: &Insn) -> Option<u8> {
    use r5900::Format as F;
    match i.m.format() {
        F::RdRsRt | F::RdRtRs | F::RdRtSa | F::RdRs | F::RdRt | F::Rd => Some(i.rd()),
        F::RtRsImm | F::RtRsUimm | F::RtUimm | F::Mem | F::RtCop0 | F::RtFpr | F::RtFcr | F::RtVf | F::RtVi => {
            let stores = matches!(
                i.m,
                Mnemonic::Sb | Mnemonic::Sh | Mnemonic::Sw | Mnemonic::Swl | Mnemonic::Swr
                    | Mnemonic::Sd | Mnemonic::Sdl | Mnemonic::Sdr | Mnemonic::Sq
                    | Mnemonic::Mtc0 | Mnemonic::Mtc1 | Mnemonic::Ctc1 | Mnemonic::Qmtc2
                    | Mnemonic::Ctc2
            );
            (!stores).then(|| i.rt())
        }
        _ => None,
    }
}

pub fn analyze(elf: &Elf) -> Analysis {
    let seg = elf.segments.iter().find(|s| s.executable).expect("no executable segment");
    let w = Walker { elf, code_lo: seg.vaddr, code_hi: seg.vaddr + seg.file_len as u32 };

    let mut functions: BTreeMap<u32, Function> = BTreeMap::new();
    let mut covered = HashSet::new();
    let mut queue = vec![(elf.entry, Origin::Entry)];
    let mut pointers: BTreeSet<u32> = BTreeSet::new();

    loop {
        while let Some((start, origin)) = queue.pop() {
            if functions.contains_key(&start) {
                continue;
            }
            let mut found = Vec::new();
            let f = w.explore(start, origin, &functions, &mut found);
            covered.extend(f.insns.iter().copied());
            functions.insert(start, f);
            for (a, o) in found {
                if o == Origin::CodePointer {
                    pointers.insert(a);
                } else {
                    queue.push((a, o));
                }
            }
        }

        // Pointer-only functions: accept a code address that nothing has
        // explored yet when it looks like a function start.
        let looks_like_start = |a: u32| {
            let i = w.insn(a);
            let prologue = i.m == Mnemonic::Addiu && i.rt() == SP && i.rs() == SP && i.imm() < 0;
            prologue || w.elf.read_u32(a.wrapping_sub(8)) == Some(JR_RA)
        };
        for &p in &pointers {
            if !covered.contains(&p) && looks_like_start(p) {
                queue.push((p, Origin::CodePointer));
            }
        }
        // Unexplored code directly after a known function's return, or
        // starting with a stack-frame prologue.
        if queue.is_empty() {
            let mut a = w.code_lo;
            while a < w.code_hi {
                if !covered.contains(&a) {
                    if covered.contains(&a.wrapping_sub(4)) && w.elf.read_u32(a - 8) == Some(JR_RA) {
                        if w.insn(a).is_canonical() && w.insn(a).raw != 0 {
                            queue.push((a, Origin::AfterReturn));
                        }
                    } else if looks_like_start(a) && w.elf.read_u32(a.wrapping_sub(8)) != Some(JR_RA) {
                        queue.push((a, Origin::Prologue));
                    }
                }
                a += 4;
            }
        }
        queue.retain(|(a, _)| !functions.contains_key(a));
        if queue.is_empty() {
            break;
        }
    }

    Analysis { code_lo: w.code_lo, code_hi: w.code_hi, functions, covered }
}
