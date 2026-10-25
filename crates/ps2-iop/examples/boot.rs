//! Boot IOP modules standalone: boot <module.irx>... ; runs one second.
use std::sync::Arc;

use ps2_iop::{Link, Machine, ToIop};

fn main() {
    let link = Arc::new(Link::default());
    for p in std::env::args().skip(1) {
        let image = std::fs::read(&p).expect("read module");
        link.send(ToIop::Load { name: p.clone(), image, args: Vec::new() });
    }
    let mut m = Machine::new(link.clone());
    let mut out = Vec::new();
    m.render(48_000, &mut out);
    for msg in link.take() {
        println!("to EE: {msg:?}");
    }
    let peak = out.iter().map(|s| s[0].unsigned_abs().max(s[1].unsigned_abs())).max().unwrap_or(0);
    println!("{} cycles, output peak {peak}", m.cycles);
}
