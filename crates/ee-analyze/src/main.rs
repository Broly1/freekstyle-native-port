//! ee-analyze: map every function in the game's EE executable and report how
//! well the decoder and discovery cover it.
//!
//! Usage: ee-analyze <SLUS_203.67> [out-dir]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::{env, fs, process};

use ee_analyze::discover::{self, Origin};
use ee_analyze::elf;
use r5900::{decode, Mnemonic};

fn main() {
    let args: Vec<String> = env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: ee-analyze <elf> [out-dir]");
        process::exit(2);
    };
    let out_dir = args.get(2).map(String::as_str).unwrap_or("out");

    let bytes = fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        process::exit(1);
    });
    let elf = elf::Elf::parse(&bytes).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        process::exit(1);
    });
    let a = discover::analyze(&elf, &[]);

    // Summary
    let code_words = (a.code_hi - a.code_lo) / 4;
    let highest = a.covered.iter().max().copied().unwrap_or(a.code_lo);
    let text_words = (highest + 4 - a.code_lo) / 4;
    println!("segment      0x{:08x}..0x{:08x} ({} words)", a.code_lo, a.code_hi, code_words);
    println!("entry        0x{:08x}", elf.entry);
    println!("highest code 0x{highest:08x} (text likely ends here; rodata/data follow)");
    println!(
        "covered      {} words = {:.1}% of the text range",
        a.covered.len(),
        100.0 * a.covered.len() as f64 / text_words as f64
    );
    println!("functions    {}", a.functions.len());

    let mut by_origin: BTreeMap<Origin, (usize, usize)> = BTreeMap::new();
    for f in a.functions.values() {
        let e = by_origin.entry(f.origin).or_default();
        e.0 += 1;
        e.1 += f.invalid.len();
    }
    for (o, (n, bad)) in &by_origin {
        println!("  {o:<12?} {n:>5} functions, {bad} invalid instructions reached");
    }

    let tables: Vec<_> = a.functions.values().flat_map(|f| &f.jump_tables).collect();
    let unresolved: usize = a.functions.values().map(|f| f.unresolved_jumps.len()).sum();
    let jalr: usize = a.functions.values().map(|f| f.indirect_calls.len()).sum();
    println!(
        "jump tables  {} resolved ({} bounded), {} unresolved indirect jumps",
        tables.len(),
        tables.iter().filter(|t| t.bounded).count(),
        unresolved
    );
    println!("jalr         {jalr} indirect call sites");

    let mut syscalls: BTreeMap<Option<u32>, usize> = BTreeMap::new();
    for f in a.functions.values() {
        for (_, n) in &f.syscalls {
            *syscalls.entry(*n).or_default() += 1;
        }
    }
    let list: Vec<String> = syscalls
        .iter()
        .map(|(n, c)| match n {
            Some(n) if (*n as i32) < 0 => format!("-0x{:x}x{c}", (*n as i32).unsigned_abs()),
            Some(n) => format!("0x{n:x}x{c}"),
            None => format!("?x{c}"),
        })
        .collect();
    println!("syscalls     {}", list.join(" "));

    let mut hist: BTreeMap<Mnemonic, usize> = BTreeMap::new();
    let mut noncanon = 0;
    for &pc in &a.covered {
        let i = decode(elf.read_u32(pc).unwrap());
        *hist.entry(i.m).or_default() += 1;
        noncanon += usize::from(!i.is_canonical());
    }
    println!("non-canonical encodings in covered code: {noncanon}");
    println!("distinct mnemonics used: {}", hist.len());

    let bad: Vec<_> = a
        .functions
        .values()
        .flat_map(|f| f.invalid.iter().map(move |pc| (f.start, f.origin, *pc)))
        .collect();
    for (start, origin, pc) in bad.iter().take(15) {
        let w = elf.read_u32(*pc).unwrap();
        println!("  invalid 0x{w:08x} at 0x{pc:08x} in fn 0x{start:08x} ({origin:?})");
    }

    // Detailed outputs
    fs::create_dir_all(out_dir).expect("create out dir");
    let mut tsv = String::from("start\tlast\tinsns\torigin\tcalls\tjalr\ttables\tunresolved\tinvalid\n");
    let mut asm = String::new();
    for f in a.functions.values() {
        let last = f.insns.iter().max().copied().unwrap_or(f.start);
        writeln!(
            tsv,
            "{:08x}\t{:08x}\t{}\t{:?}\t{}\t{}\t{}\t{}\t{}",
            f.start,
            last,
            f.insns.len(),
            f.origin,
            f.calls.len(),
            f.indirect_calls.len(),
            f.jump_tables.len(),
            f.unresolved_jumps.len(),
            f.invalid.len()
        )
        .unwrap();
        writeln!(asm, "\nfn_{:08x}:  # {:?}", f.start, f.origin).unwrap();
        for &pc in &f.insns {
            let i = decode(elf.read_u32(pc).unwrap());
            writeln!(asm, "  {pc:08x}: {:08x}  {}", i.raw, i.disasm(pc)).unwrap();
        }
    }
    let mut jt = String::from("jr\ttable\tbounded\ttargets\n");
    for t in &tables {
        let targets: Vec<String> = t.targets.iter().map(|a| format!("{a:08x}")).collect();
        writeln!(jt, "{:08x}\t{:08x}\t{}\t{}", t.jr, t.table, t.bounded, targets.join(",")).unwrap();
    }
    fs::write(format!("{out_dir}/jumptables.tsv"), jt).unwrap();
    let mut mn = String::new();
    let mut hv: Vec<_> = hist.into_iter().collect();
    hv.sort_by(|a, b| b.1.cmp(&a.1));
    for (m, n) in hv {
        writeln!(mn, "{n}\t{}", m.name()).unwrap();
    }
    fs::write(format!("{out_dir}/functions.tsv"), tsv).unwrap();
    fs::write(format!("{out_dir}/disasm.s"), asm).unwrap();
    fs::write(format!("{out_dir}/mnemonics.tsv"), mn).unwrap();
    println!("wrote {out_dir}/functions.tsv, disasm.s, mnemonics.tsv, jumptables.tsv");
}
