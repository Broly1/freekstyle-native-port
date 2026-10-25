//! Disassemble a range of a raw file: dis <file> <file offset of addr base> <base> <start> <end>
use std::env;

fn main() {
    let a: Vec<String> = env::args().collect();
    let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    let data = std::fs::read(&a[1]).unwrap();
    let (off, base, start, end) = (hex(&a[2]), hex(&a[3]), hex(&a[4]), hex(&a[5]));
    let mut pc = start;
    while pc < end {
        let o = (pc - base + off) as usize;
        let w = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        println!("{pc:08x}  {w:08x}  {}", r5900::decode(w).disasm(pc));
        pc += 4;
    }
}
