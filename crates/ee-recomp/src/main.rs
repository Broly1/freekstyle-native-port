//! ee-recomp: translate every function in the EE executable into Rust.
//!
//! Usage: ee-recomp <SLUS_203.67> <out-dir> [hle.txt [seeds.txt [vu-dumps]]]
//!
//! Each guest function becomes `pub fn f_XXXXXXXX(c: &mut Cpu)`. Its body is
//! a `loop { match pc { ... } }` with one arm per basic block, so any
//! intra-function control flow (loops, jump tables, branches into delay
//! slots) translates directly. Calls are native Rust calls; returns are
//! `return`. Indirect calls and jumps that leave the function go through
//! `Cpu::call`, which looks the address up in the generated `FUNCS` table.
//!
//! Functions listed in the optional HLE file are not translated; their body
//! becomes a call to the named replacement in `ee_runtime::hle`.

mod emit;
mod vu;
mod vuc;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::{env, fs, process};

use ee_analyze::discover::{self, Function};
use ee_analyze::elf::Elf;
use r5900::{decode, Insn, Mnemonic as M};

/// Functions per generated module. Smaller modules compile in parallel.
const PER_MODULE: usize = 64;

struct Ctx<'a> {
    elf: &'a Elf,
    starts: &'a BTreeSet<u32>,
    unimplemented: BTreeMap<M, usize>,
}

impl Ctx<'_> {
    fn insn(&self, pc: u32) -> Insn {
        decode(self.elf.read_u32(pc).expect("pc outside image"))
    }

    /// Code for a straight-line instruction, or a runtime trap.
    fn straight(&mut self, i: &Insn, pc: u32) -> String {
        match emit::insn(i, pc) {
            Some(s) => s,
            None => {
                *self.unimplemented.entry(i.m).or_default() += 1;
                format!("c.unimplemented({pc:#x}, {:#x});", i.raw)
            }
        }
    }

    /// Statement that calls `t` and returns to the caller's continuation.
    fn call(&self, t: u32) -> String {
        if self.starts.contains(&t) {
            format!("f_{t:08x}(c);")
        } else {
            format!("c.call({t:#x});")
        }
    }
}

fn block_starts(f: &Function, ctx: &Ctx) -> BTreeSet<u32> {
    let mut starts = BTreeSet::from([f.start]);
    for &pc in &f.insns {
        let i = ctx.insn(pc);
        if !i.has_delay_slot() {
            continue;
        }
        let after = pc + 8;
        match i.m {
            M::J => {
                let t = i.jump_target(pc).unwrap();
                if f.insns.contains(&t) && !ctx.starts.contains(&t) {
                    starts.insert(t);
                }
            }
            M::Jal | M::Jalr | M::Jr => {}
            _ => {
                let t = i.branch_target(pc).unwrap();
                if f.insns.contains(&t) {
                    starts.insert(t);
                }
                if f.insns.contains(&after) {
                    starts.insert(after);
                }
            }
        }
    }
    for t in &f.jump_tables {
        starts.extend(t.targets.iter().filter(|a| f.insns.contains(a)));
    }
    starts
}

fn emit_function(f: &Function, ctx: &mut Ctx, out: &mut String) {
    let starts = block_starts(f, ctx);
    let _ = writeln!(out, "pub fn f_{:08x}(c: &mut Cpu) {{", f.start);
    let _ = writeln!(out, "    c.enter({:#x});", f.start);
    let _ = writeln!(out, "    let mut pc: u32 = {:#x};", f.start);
    let _ = writeln!(out, "    loop {{\n        match pc {{");

    for &b in &starts {
        let mut body = String::new();
        let mut pc = b;
        loop {
            if pc != b && starts.contains(&pc) {
                let _ = writeln!(body, "pc = {pc:#x};");
                break;
            }
            if !f.insns.contains(&pc) {
                let _ = writeln!(body, "c.fell_off({pc:#x}); return;");
                break;
            }
            let i = ctx.insn(pc);
            if i.m == M::Eret {
                body.push_str("return;\n");
                break;
            }
            if !i.has_delay_slot() {
                let s = ctx.straight(&i, pc);
                if !s.is_empty() {
                    let _ = writeln!(body, "{s}");
                }
                pc += 4;
                continue;
            }
            let (done, next) = control(&i, pc, f, ctx, &mut body);
            if done {
                break;
            }
            pc = next;
        }
        let _ = writeln!(out, "            {b:#x} => {{\n{}            }}", indent(&body, 16));
    }
    let _ = writeln!(out, "            _ => {{ c.call(pc); return; }}");
    let _ = writeln!(out, "        }}\n    }}\n}}\n");
}

/// Emit a control transfer and its delay slot. Returns (block ended, next pc).
fn control(i: &Insn, pc: u32, f: &Function, ctx: &mut Ctx, body: &mut String) -> (bool, u32) {
    let slot_pc = pc + 4;
    let slot_insn = ctx.insn(slot_pc);
    let slot = if slot_insn.has_delay_slot() {
        // A jump in a delay slot is undefined on MIPS; trap if it ever runs.
        format!("c.unimplemented({slot_pc:#x}, {:#x});", slot_insn.raw)
    } else {
        ctx.straight(&slot_insn, slot_pc)
    };
    let after = pc + 8;
    // Where control goes when execution continues at `t`: a block in this
    // function, or a tail call.
    let goto = |t: u32, ctx: &Ctx| {
        if f.insns.contains(&t) && !(ctx.starts.contains(&t) && t != f.start) {
            // Backward jumps may be spin-waits; let them see interrupts.
            if t <= pc {
                format!("c.backedge(); pc = {t:#x};")
            } else {
                format!("pc = {t:#x};")
            }
        } else {
            format!("{} return;", ctx.call(t))
        }
    };

    match i.m {
        M::Jal => {
            let t = i.jump_target(pc).unwrap();
            let _ = writeln!(body, "c.w(31, {after:#x});\n{slot}\n{}", ctx.call(t));
            (false, after)
        }
        M::Jalr => {
            let (rs, rd) = (i.rs(), i.rd());
            let _ = writeln!(body, "let t = c.r32({rs});\nc.w({rd}, {after:#x});\n{slot}\nc.call(t);");
            (false, after)
        }
        M::Jr if i.rs() == 31 => {
            let _ = writeln!(body, "{slot}\nreturn;");
            (true, 0)
        }
        M::Jr => {
            // Jump tables land on block arms; anything else hits the
            // default arm, which tail-calls through the function table.
            let _ = writeln!(body, "let t = c.r32({});\n{slot}\npc = t;", i.rs());
            (true, 0)
        }
        M::J => {
            let t = i.jump_target(pc).unwrap();
            let _ = writeln!(body, "{slot}\n{}", goto(t, ctx));
            (true, 0)
        }
        M::Bltzal | M::Bgezal | M::Bltzall | M::Bgezall => {
            let t = i.branch_target(pc).unwrap();
            let cond = emit::condition(i).unwrap();
            let likely = i.is_likely();
            let call = ctx.call(t);
            if likely {
                let _ = writeln!(body, "c.w(31, {after:#x});\nif {cond} {{\n{slot}\n{call}\n}}");
            } else {
                let _ = writeln!(body, "let t = {cond};\nc.w(31, {after:#x});\n{slot}\nif t {{ {call} }}");
            }
            (false, after)
        }
        _ => {
            let t = i.branch_target(pc).unwrap();
            let Some(cond) = emit::condition(i) else {
                *ctx.unimplemented.entry(i.m).or_default() += 1;
                let _ = writeln!(body, "c.unimplemented({pc:#x}, {:#x}); return;", i.raw);
                return (true, 0);
            };
            let taken = goto(t, ctx);
            let fall = goto(after, ctx);
            if cond == "true" {
                let _ = writeln!(body, "{slot}\n{taken}");
            } else if i.is_likely() {
                let _ = writeln!(body, "if {cond} {{\n{slot}\n{taken}\n}} else {{\n{fall}\n}}");
            } else {
                let _ = writeln!(body, "let t = {cond};\n{slot}\nif t {{ {taken} }} else {{ {fall} }}");
            }
            (true, 0)
        }
    }
}

fn indent(s: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    s.lines()
        .filter(|l| !l.is_empty())
        .map(|l| format!("{pad}{l}\n"))
        .collect()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if !(3..=6).contains(&args.len()) {
        eprintln!("usage: ee-recomp <elf> <out-dir> [hle.txt [seeds.txt [vu-dumps]]]");
        process::exit(2);
    }
    let hle = args.get(3).map(|p| read_list(p)).unwrap_or_default();
    let seeds: Vec<u32> = args.get(4).map(|p| read_list(p).into_keys().collect()).unwrap_or_default();
    let bytes = fs::read(&args[1]).unwrap_or_else(|e| {
        eprintln!("{}: {e}", args[1]);
        process::exit(1);
    });
    let elf = Elf::parse(&bytes).unwrap_or_else(|e| {
        eprintln!("{}: {e}", args[1]);
        process::exit(1);
    });
    let analysis = discover::analyze(&elf, &seeds);
    let starts: BTreeSet<u32> = analysis.functions.keys().copied().collect();
    let mut ctx = Ctx { elf: &elf, starts: &starts, unimplemented: BTreeMap::new() };

    let out_dir = &args[2];
    fs::create_dir_all(out_dir).expect("create out dir");
    let funcs: Vec<&Function> = analysis.functions.values().collect();
    let mut modules = Vec::new();
    for (n, chunk) in funcs.chunks(PER_MODULE).enumerate() {
        let name = format!("m{n:03}");
        let mut src = String::from(
            "// Generated by ee-recomp. Do not edit.\n\
             #![allow(unused, unreachable_code, clippy::all)]\n\
             use super::*;\n\n",
        );
        for f in chunk {
            match hle.get(&f.start) {
                Some(name) => {
                    let _ = writeln!(
                        src,
                        "pub fn f_{0:08x}(c: &mut Cpu) {{\n    c.enter({0:#x});\n    ee_runtime::hle::{name}(c);\n}}\n",
                        f.start
                    );
                }
                None => emit_function(f, &mut ctx, &mut src),
            }
        }
        fs::write(format!("{out_dir}/{name}.rs"), src).expect("write module");
        modules.push(name);
    }

    let mut root = String::from(
        "// Generated by ee-recomp. Do not edit.\n\
         #![allow(unused)]\n\
         use ee_runtime::prelude::*;\n\n",
    );
    for m in &modules {
        let _ = writeln!(root, "mod {m};\npub use {m}::*;");
    }
    let _ = writeln!(root, "pub mod vu_programs;");
    let (vu_src, vu_images, vu_entries) = vu::generate(args.get(5).map(std::path::Path::new), &bytes);
    fs::write(format!("{out_dir}/vu_programs.rs"), vu_src).expect("write vu_programs.rs");
    let _ = writeln!(root, "\n/// FNV-1a of the executable this code was generated from.");
    let _ = writeln!(root, "pub const ELF_HASH: u64 = {:#x};", ee_runtime_fnv(&bytes));
    let _ = writeln!(root, "pub const ENTRY: u32 = {:#x};\n", elf.entry);
    let _ = writeln!(root, "pub static FUNCS: [(u32, ee_runtime::Func); {}] = [", funcs.len());
    for f in &funcs {
        let _ = writeln!(root, "    ({0:#x}, f_{0:08x}),", f.start);
    }
    root.push_str("];\n");
    fs::write(format!("{out_dir}/mod.rs"), root).expect("write mod.rs");

    for a in hle.keys().filter(|a| !starts.contains(a)) {
        eprintln!("warning: HLE address {a:#010x} is not a discovered function");
    }
    eprintln!(
        "recompiled {} functions ({} replaced by HLE) into {} modules in {out_dir}",
        funcs.len(),
        hle.keys().filter(|a| starts.contains(a)).count(),
        modules.len()
    );
    eprintln!("recompiled {vu_entries} VU microprogram entry points from {vu_images} recorded images");
    if ctx.unimplemented.is_empty() {
        eprintln!("all reached instructions have implementations");
    } else {
        eprintln!("instructions emitted as runtime traps:");
        for (m, n) in &ctx.unimplemented {
            eprintln!("  {:<10} x{n}", m.name());
        }
    }
}

/// Parse `<hex address> [name]` lines; `#` starts a comment.
fn read_list(path: &str) -> BTreeMap<u32, String> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        process::exit(1);
    });
    let mut map = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(addr), name, None) = (parts.next(), parts.next(), parts.next()) else {
            eprintln!("{path}:{}: expected `<address> [name]`", n + 1);
            process::exit(1);
        };
        let Ok(addr) = u32::from_str_radix(addr.trim_start_matches("0x"), 16) else {
            eprintln!("{path}:{}: bad address {addr}", n + 1);
            process::exit(1);
        };
        map.insert(addr, name.unwrap_or_default().to_string());
    }
    map
}

/// Same hash as `ee_runtime::fnv1a`; duplicated so the recompiler doesn't
/// depend on the runtime.
fn ee_runtime_fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}
