//! FREEK_PROFILE=<seconds after start>: sample the EE thread's program
//! counter for 10 s with SIGPROF and write `profile.txt` (raw addresses,
//! relative to the executable, ready for addr2line). FREEK_PROFILE_GS does
//! the same for the render thread into `profile_gs.txt`. Use one at a time.
//! A debugging aid.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Duration;

const N: usize = 1 << 16;
static SAMPLES: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_prof(_: libc::c_int, _: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: the kernel passes a ucontext_t to SA_SIGINFO handlers.
    let pc = unsafe { (*(ctx as *const libc::ucontext_t)).uc_mcontext.gregs[libc::REG_RIP as usize] } as u64;
    let i = COUNT.fetch_add(1, Relaxed);
    if i < N {
        SAMPLES[i].store(pc, Relaxed);
    }
}

/// Call from the thread to profile: `var` holds the delay, samples go to `out`.
pub fn start_if_requested(var: &str, out: &'static str) {
    let Some(delay) = std::env::var(var).ok().and_then(|v| v.parse::<u64>().ok()) else { return };
    // SAFETY: plain libc calls; the handler only touches atomics.
    let target = unsafe { libc::pthread_self() };
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_prof as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigaction(libc::SIGPROF, &sa, std::ptr::null_mut());
    }
    let path = out;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(delay));
        eprintln!("[profile] sampling for 10 s");
        for _ in 0..10_000 {
            // SAFETY: target is a live thread of this process.
            unsafe { libc::pthread_kill(target, libc::SIGPROF) };
            std::thread::sleep(Duration::from_millis(1));
        }
        let base = std::fs::read_to_string("/proc/self/maps")
            .ok()
            .and_then(|m| m.lines().find(|l| l.contains("freekstyle")).and_then(|l| u64::from_str_radix(l.split('-').next()?, 16).ok()))
            .unwrap_or(0);
        let n = COUNT.load(Relaxed).min(N);
        let text: String = (0..n).map(|i| format!("{:#x}\n", SAMPLES[i].load(Relaxed).wrapping_sub(base))).collect();
        let _ = std::fs::write(path, text);
        eprintln!("[profile] wrote {n} samples to {path}");
    });
}
