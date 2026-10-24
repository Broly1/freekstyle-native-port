//! Freekstyle native: boots the recompiled EE executable.
//!
//! Generate the code first:
//!   cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt
//! then:
//!   cargo run --release -p freekstyle -- game/SLUS_203.67 [disc image]
//!
//! The disc image (raw .bin or .iso) defaults to game/Freekstyle.bin.

#[allow(clippy::all)]
mod gen;

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use std::{env, fs, process, thread};

use ee_runtime::Cpu;

/// Recompiled code uses the host stack for guest calls; give it room.
const STACK_SIZE: usize = 512 << 20;

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| "game/SLUS_203.67".into());
    let disc_path = env::args().nth(2).unwrap_or_else(|| "game/Freekstyle.bin".into());
    let disc = ee_runtime::disc::Disc::open(disc_path.as_ref()).unwrap_or_else(|e| {
        eprintln!("disc image: {e}");
        process::exit(1);
    });
    let bytes = fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        process::exit(1);
    });
    if ee_runtime::fnv1a(&bytes) != gen::ELF_HASH {
        eprintln!("{path} is not the executable the code was generated from; re-run ee-recomp");
        process::exit(1);
    }

    let game = thread::Builder::new()
        .name("ee-main".into())
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let mut c = Cpu::new(&gen::FUNCS);
            c.iop.disc = Some(disc);
            let entry = ee_runtime::load_elf(&mut c, &bytes).expect("load ELF");
            eprintln!("[boot] {} functions, entry {entry:#x}", gen::FUNCS.len());
            ee_runtime::kernel::run_thread_body(&mut c, entry);
            eprintln!("[boot] main thread returned");
            // Other guest threads may still hold references to `c`.
            process::exit(0);
        })
        .expect("spawn EE thread");
    thread::spawn(watchdog);
    if game.join().is_err() {
        process::exit(101);
    }
}

/// Report when guest code stops entering new functions, which usually means
/// it is spinning on hardware or a kernel object we don't emulate yet.
fn watchdog() {
    use ee_runtime::cpu::{ENTER_COUNT, LAST_ENTERED};
    let mut last = 0;
    let mut reported = 0;
    let heartbeat = env::var_os("FREEK_HEARTBEAT").is_some();
    loop {
        thread::sleep(Duration::from_secs(2));
        let n = ENTER_COUNT.load(Relaxed);
        let f = LAST_ENTERED.load(Relaxed);
        if n == last && f != reported {
            eprintln!("[watchdog] no function calls for 2s; stuck inside or after fn {f:#010x} ({n} calls so far)");
            reported = f;
        }
        if heartbeat {
            eprintln!("[heartbeat] in fn {f:#010x}, {} calls in the last 2s", n - last);
        }
        last = n;
    }
}
