//! Recompile recorded VU microprograms to Rust.
//!
//! The runtime records each interpreted program image and its entry points
//! (FREEK_VU_DUMP). For every entry, this walks the reachable instruction
//! pairs and emits a function that calls `vumicro::pair` with constant
//! operands, so the compiler specialises each pair and no decoding remains.
//! Control flow is a `loop { match pc }` over blocks, as for EE code; jumps
//! to addresses not covered here continue in the interpreter.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use r5900::decode;

/// Mirror of `ee_runtime::vumicro::reads_flags` (kept here so the
/// recompiler does not depend on the runtime).
fn reads_flags(code: &[u8]) -> bool {
    code.chunks_exact(8).any(|p| {
        let lo = u32::from_le_bytes(p[0..4].try_into().unwrap());
        let hi = u32::from_le_bytes(p[4..8].try_into().unwrap());
        hi >> 31 == 0 && matches!(lo >> 25, 0x14..=0x18 | 0x1a | 0x1b)
    })
}

struct Image {
    unit: u8,
    hash: u64,
    code: Vec<u8>,
    entries: BTreeSet<u32>,
    /// The program reads MAC/status flags, so FMAC must compute them.
    flags: bool,
}

/// Lower-word branch kinds (with the I bit clear).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Branch {
    /// B: always taken, static target.
    Always,
    /// BAL: always taken, static target, returns after the delay slot.
    Call,
    /// JR: dynamic target.
    Jump,
    /// JALR: dynamic target, returns after the delay slot.
    JumpCall,
    /// IBxx: static target or fall through.
    Cond,
}

fn branch(lo: u32, hi: u32) -> Option<Branch> {
    if hi >> 31 != 0 {
        return None; // lower word is an immediate
    }
    Some(match lo >> 25 {
        0x20 => Branch::Always,
        0x21 => Branch::Call,
        0x24 => Branch::Jump,
        0x25 => Branch::JumpCall,
        0x28 | 0x29 | 0x2c..=0x2f => Branch::Cond,
        _ => return None,
    })
}

impl Image {
    fn mask(&self) -> u32 {
        (self.code.len() / 8) as u32 - 1
    }

    fn words(&self, pc: u32) -> (u32, u32) {
        let at = (pc & self.mask()) as usize * 8;
        (u32::from_le_bytes(self.code[at..at + 4].try_into().unwrap()), u32::from_le_bytes(self.code[at + 4..at + 8].try_into().unwrap()))
    }

    fn target(&self, pc: u32, lo: u32) -> u32 {
        let imm = ((lo & 0x7ff) as i32) << 21 >> 21;
        (pc as i32 + 1 + imm) as u32 & self.mask()
    }

    /// Block starts reachable from `entry`.
    fn blocks(&self, entry: u32) -> BTreeSet<u32> {
        let mask = self.mask();
        let mut starts = BTreeSet::from([entry]);
        let mut seen = BTreeSet::new();
        let mut work = vec![entry];
        while let Some(b) = work.pop() {
            let mut pc = b;
            while seen.insert(pc) {
                let (lo, hi) = self.words(pc);
                let end = hi & (1 << 30) != 0;
                if let Some(k) = branch(lo, hi) {
                    let mut next = Vec::new();
                    if !end {
                        match k {
                            Branch::Always => next.push(self.target(pc, lo)),
                            Branch::Call => next.extend([self.target(pc, lo), (pc + 2) & mask]),
                            Branch::Jump => {}
                            Branch::JumpCall => next.push((pc + 2) & mask),
                            Branch::Cond => next.extend([self.target(pc, lo), (pc + 2) & mask]),
                        }
                    }
                    for t in next {
                        if starts.insert(t) {
                            work.push(t);
                        }
                    }
                    break;
                }
                if end {
                    break;
                }
                pc = (pc + 1) & mask;
            }
        }
        starts
    }

    /// Whether any block ends in a JR/JALR (target unknown statically).
    fn has_dynamic_jumps(&self, starts: &BTreeSet<u32>) -> bool {
        let mask = self.mask();
        starts.iter().any(|&b| {
            let mut pc = b;
            for _ in 0..=mask {
                let (lo, hi) = self.words(pc);
                if let Some(k) = branch(lo, hi) {
                    return matches!(k, Branch::Jump | Branch::JumpCall);
                }
                if hi & (1 << 30) != 0 {
                    return false;
                }
                pc = (pc + 1) & mask;
            }
            false
        })
    }

    fn pair_call(&self, pc: u32, flags: bool) -> String {
        let (lo, hi) = self.words(pc);
        let m = decode(0x4a00_0000 | (hi & 0x01ff_ffff)).m;
        format!("pair::<{flags}>(c, {pc:#x}, {lo:#010x}, {hi:#010x}, M::{m:?})")
    }

    /// Whether code reachable from `entry` reads the MAC/status flags; if
    /// not, its FMAC instructions can skip computing them.
    fn entry_reads_flags(&self, entry: u32, starts: &BTreeSet<u32>) -> bool {
        let mask = self.mask();
        starts.iter().any(|&b| {
            let mut pc = b;
            for _ in 0..=mask {
                let (lo, hi) = self.words(pc);
                if hi >> 31 == 0 && matches!(lo >> 25, 0x14..=0x18 | 0x1a | 0x1b) {
                    return true;
                }
                // Through the end of the block (and its delay slot).
                if branch(lo, hi).is_some() || hi & (1 << 30) != 0 {
                    let (dlo, dhi) = self.words((pc + 1) & mask);
                    return dhi >> 31 == 0 && matches!(dlo >> 25, 0x14..=0x18 | 0x1a | 0x1b);
                }
                pc = (pc + 1) & mask;
                if pc != b && starts.contains(&pc) {
                    return false;
                }
            }
            false
        })
    }

    fn function(&self, entry: u32, out: &mut String) {
        let mask = self.mask();
        let starts = self.blocks(entry);
        // Jumps out of the analysed code continue in the interpreter, which
        // decides for itself; the image-wide answer is the safe fallback.
        let flags = self.flags && (self.entry_reads_flags(entry, &starts) || self.has_dynamic_jumps(&starts));
        let _ = writeln!(out, "fn vu{}_{:016x}_{entry:04x}(c: &mut Ctx) {{", self.unit, self.hash);
        let _ = writeln!(out, "    let mut pc: u32 = {entry:#x};\n    loop {{\n        match pc {{");
        for &b in &starts {
            let mut body = String::new();
            let mut pc = b;
            loop {
                if pc != b && starts.contains(&pc) {
                    let _ = writeln!(body, "pc = {pc:#x};");
                    break;
                }
                let (lo, hi) = self.words(pc);
                let end = hi & (1 << 30) != 0;
                let delay = (pc + 1) & mask;
                let (dlo, dhi) = self.words(delay);
                if let Some(k) = branch(lo, hi) {
                    // A branch or E bit in the delay slot: let the
                    // interpreter handle the whole sequence.
                    if branch(dlo, dhi).is_some() || dhi & (1 << 30) != 0 {
                        let _ = writeln!(body, "interpret_from(c, {pc:#x});\nreturn;");
                        break;
                    }
                    let _ = writeln!(body, "let t = {};\n{};", self.pair_call(pc, flags), self.pair_call(delay, flags));
                    if end {
                        // A branch in the E-bit pair sets where MSCNT resumes.
                        let _ = writeln!(body, "c.finish(t.unwrap_or({:#x}));\nreturn;", (pc + 2) & mask);
                    } else if k == Branch::Cond {
                        let _ = writeln!(body, "pc = t.unwrap_or({:#x});", (pc + 2) & mask);
                    } else {
                        let _ = writeln!(body, "pc = t.unwrap();");
                    }
                    break;
                }
                if end {
                    let _ = writeln!(body, "{};\n{};\nc.finish({:#x});\nreturn;", self.pair_call(pc, flags), self.pair_call(delay, flags), (pc + 2) & mask);
                    break;
                }
                let _ = writeln!(body, "{};", self.pair_call(pc, flags));
                pc = (pc + 1) & mask;
            }
            let body: String = body.lines().map(|l| format!("                {l}\n")).collect();
            let _ = writeln!(out, "            {b:#x} => {{\n{body}            }}");
        }
        let _ = writeln!(out, "            _ => {{\n                interpret_from(c, pc);\n                return;\n            }}");
        let _ = writeln!(out, "        }}\n    }}\n}}\n");
    }
}

fn load(dir: &Path) -> Vec<Image> {
    let mut images = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return images };
    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "bin")).collect();
    paths.sort();
    for p in paths {
        let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
        // vu<unit>_<hash>
        let Some((u, h)) = stem.strip_prefix("vu").and_then(|s| s.split_once('_')) else { continue };
        let (Ok(unit), Ok(hash)) = (u.parse::<u8>(), u64::from_str_radix(h, 16)) else { continue };
        let Ok(code) = std::fs::read(&p) else { continue };
        if !(code.len() / 8).is_power_of_two() {
            continue;
        }
        let entries = std::fs::read_to_string(p.with_extension("entries"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| u32::from_str_radix(l.trim(), 16).ok())
            .collect();
        let flags = reads_flags(&code);
        images.push(Image { unit, hash, code, entries, flags });
    }
    images
}

/// Rust source for all recorded programs, with the VU_PROGRAMS table.
/// Returns (source, number of programs, number of entry points).
pub fn generate(dir: Option<&Path>) -> (String, usize, usize) {
    let images = dir.map(load).unwrap_or_default();
    let mut out = String::from(
        "// Generated by ee-recomp from recorded VU microprograms. Do not edit.\n\
         #![allow(unused, unreachable_code, clippy::all)]\n\
         use ee_runtime::vumicro::{interpret_from, pair, Ctx, NativeFn};\n\
         use r5900::Mnemonic as M;\n\n",
    );
    let mut table = Vec::new();
    for img in &images {
        for &e in &img.entries {
            img.function(e, &mut out);
            table.push((img.unit, img.hash, e));
        }
    }
    table.sort();
    let _ = writeln!(out, "pub static VU_PROGRAMS: [(u8, u64, u32, NativeFn); {}] = [", table.len());
    for (u, h, e) in &table {
        let _ = writeln!(out, "    ({u}, {h:#018x}, {e:#x}, vu{u}_{h:016x}_{e:04x}),");
    }
    out.push_str("];\n");
    (out, images.len(), table.len())
}
