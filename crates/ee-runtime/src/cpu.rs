//! EE CPU state and the helpers recompiled code calls.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use crate::iop::Iop;
use crate::kernel::Kernel;
use crate::mem::Memory;
use crate::vu::Vu0;

pub type Func = fn(&mut Cpu);

const FCC: u32 = 1 << 23;

/// Last guest function entered and total entries so far, published for the
/// watchdog thread in the frontend.
pub static LAST_ENTERED: AtomicU32 = AtomicU32::new(0);
pub static ENTER_COUNT: AtomicU64 = AtomicU64::new(0);
/// Microseconds the game has spent spinning on hardware registers.
pub static POLL_US: AtomicU64 = AtomicU64::new(0);

/// Registers saved across a thread switch. VU0 is shared, as on hardware
/// (the kernel does not save COP2 state).
#[derive(Clone)]
pub struct Context {
    pub gpr: [[u64; 2]; 32],
    pub hi: [u64; 2],
    pub lo: [u64; 2],
    pub sa: u32,
    pub fpr: [f32; 32],
    pub acc: f32,
    pub fcr31: u32,
}

pub struct Cpu {
    /// 128-bit GPRs as [low 64, high 64]. Ordinary instructions only touch
    /// the low half; LQ/SQ and MMI use both.
    pub gpr: [[u64; 2]; 32],
    /// HI/LO for pipeline 0 in [0] and pipeline 1 (HI1/LO1) in [1].
    pub hi: [u64; 2],
    pub lo: [u64; 2],
    /// Funnel-shift amount, in bits.
    pub sa: u32,
    pub fpr: [f32; 32],
    pub acc: f32,
    pub fcr31: u32,
    pub cop0: [u32; 32],
    pub vu0: Vu0,
    pub mem: Memory,
    pub kernel: Kernel,
    pub iop: Iop,
    funcs: &'static [(u32, Func)],
    /// Recently entered functions, newest last, for crash reports.
    recent: [u32; 32],
    recent_pos: usize,
    calls: u64,
    in_interrupt: bool,
    /// Loop back-edges taken, for `backedge` polling.
    spins: u32,
    started: std::time::Instant,
}

impl Cpu {
    pub fn new(funcs: &'static [(u32, Func)]) -> Cpu {
        debug_assert!(funcs.windows(2).all(|w| w[0].0 < w[1].0));
        Cpu {
            gpr: [[0; 2]; 32],
            hi: [0; 2],
            lo: [0; 2],
            sa: 0,
            fpr: [0.0; 32],
            acc: 0.0,
            fcr31: 0,
            cop0: [0; 32],
            vu0: Vu0::new(),
            mem: Memory::new(),
            kernel: Kernel::new(),
            iop: Iop::default(),
            funcs,
            recent: [0; 32],
            recent_pos: 0,
            calls: 0,
            in_interrupt: false,
            spins: 0,
            started: std::time::Instant::now(),
        }
    }

    // ---- registers ----

    #[inline(always)]
    pub fn r(&self, r: usize) -> u64 {
        self.gpr[r][0]
    }

    #[inline(always)]
    pub fn r32(&self, r: usize) -> u32 {
        self.gpr[r][0] as u32
    }

    #[inline(always)]
    pub fn w(&mut self, r: usize, v: u64) {
        if r != 0 {
            self.gpr[r][0] = v;
        }
    }

    /// Write a 32-bit result sign-extended to 64 bits.
    #[inline(always)]
    pub fn ws(&mut self, r: usize, v: u32) {
        self.w(r, v as i32 as i64 as u64)
    }

    #[inline(always)]
    pub fn q(&self, r: usize) -> u128 {
        self.gpr[r][0] as u128 | (self.gpr[r][1] as u128) << 64
    }

    #[inline(always)]
    pub fn wq(&mut self, r: usize, v: u128) {
        if r != 0 {
            self.gpr[r] = [v as u64, (v >> 64) as u64];
        }
    }

    #[inline(always)]
    pub fn ea(&self, base: usize, off: i32) -> u32 {
        self.r32(base).wrapping_add(off as u32)
    }

    // ---- calls ----

    #[inline(always)]
    pub fn enter(&mut self, addr: u32) {
        self.recent[self.recent_pos] = addr;
        self.recent_pos = (self.recent_pos + 1) % self.recent.len();
        self.calls += 1;
        LAST_ENTERED.store(addr, Relaxed);
        ENTER_COUNT.store(self.calls, Relaxed);
        if self.calls & 0xfff == 0 {
            POLL_US.store(self.mem.io.poll_time.as_micros() as u64, Relaxed);
        }
        if self.calls & 0xfff == 0 && (self.poll_interrupts() || self.kernel.need_resched) && !self.in_interrupt {
            crate::kernel::reschedule(self);
        }
    }

    /// Loop back-edge in recompiled code: spin-waits must still see
    /// interrupts (and IOP replies), so poll now and then.
    #[inline(always)]
    pub fn backedge(&mut self) {
        self.spins = self.spins.wrapping_add(1);
        if self.spins & 0x3fff == 0 {
            self.spin_poll();
        }
    }

    #[cold]
    fn spin_poll(&mut self) {
        if (self.poll_interrupts() || self.kernel.need_resched) && !self.in_interrupt {
            crate::kernel::reschedule(self);
        }
    }

    pub fn save_context(&self) -> Context {
        Context { gpr: self.gpr, hi: self.hi, lo: self.lo, sa: self.sa, fpr: self.fpr, acc: self.acc, fcr31: self.fcr31 }
    }

    pub fn load_context(&mut self, x: &Context) {
        (self.gpr, self.hi, self.lo, self.sa) = (x.gpr, x.hi, x.lo, x.sa);
        (self.fpr, self.acc, self.fcr31) = (x.fpr, x.acc, x.fcr31);
    }

    /// Deliver due alarms and pending, unmasked INTC interrupts to the
    /// handlers the game registered. Called at function entries and
    /// syscalls, which are safe points for recompiled code. Returns whether
    /// any handler ran (it may have readied a thread).
    pub fn poll_interrupts(&mut self) -> bool {
        use crate::io::{D_STAT, INTC_MASK, INTC_STAT, INT_DMAC};
        if self.in_interrupt || self.cop0[12] & (1 << 16) == 0 {
            return false;
        }
        let mut ran = self.sif_receive();
        for a in crate::kernel::due_alarms(&mut self.kernel) {
            // Alarm handlers take (alarm id, time, arg).
            self.run_handler(a.id, a.func, (a.ticks as u32, a.arg));
            ran = true;
        }
        self.mem.io.tick();
        self.mem.service_vblank();
        // The kernel keeps the DMAC line enabled and dispatches per channel
        // through D_STAT's own mask, so it is never masked at the INTC.
        let pending = self.mem.io.reg(INTC_STAT) & (self.mem.io.reg(INTC_MASK) | 1 << INT_DMAC);
        if pending == 0 {
            return ran;
        }
        // Acknowledge, as the kernel's dispatcher does before running handlers.
        let stat = self.mem.io.reg(INTC_STAT) & !pending;
        self.mem.io.set_reg(INTC_STAT, stat);

        let mut calls = Vec::new();
        for cause in 0..16 {
            if pending & (1 << cause) == 0 {
                continue;
            }
            if cause == INT_DMAC {
                let d = self.mem.io.reg(D_STAT);
                let channels = d & (d >> 16) & 0x3ff;
                self.mem.io.set_reg(D_STAT, d & !channels);
                for h in self.kernel.handlers.iter().filter(|h| h.enabled && h.dmac && channels & (1 << h.cause) != 0) {
                    calls.push((h.cause, h.func, h.arg));
                }
            } else {
                for h in self.kernel.handlers.iter().filter(|h| h.enabled && !h.dmac && h.cause == cause) {
                    calls.push((h.cause, h.func, h.arg));
                }
            }
        }
        for (cause, func, arg) in calls {
            self.run_handler(cause, func, (arg, 0));
        }
        true
    }

    /// Run a guest interrupt handler with the interrupted state preserved.
    /// Handlers are called as `func(a0, a1, a2)`.
    pub fn run_handler(&mut self, a0: u32, func: u32, (a1, a2): (u32, u32)) {
        let saved = (self.gpr, self.hi, self.lo, self.sa, self.fpr, self.acc, self.fcr31);
        self.in_interrupt = true;
        // Run below the interrupted code's stack; MIPS has no red zone.
        let sp = (self.r32(29) - 0x400) & !15;
        self.w(29, sp as i32 as i64 as u64);
        self.w(4, a0 as u64);
        self.w(5, a1 as i32 as i64 as u64);
        self.w(6, a2 as i32 as i64 as u64);
        self.call(func);
        self.in_interrupt = false;
        (self.gpr, self.hi, self.lo, self.sa, self.fpr, self.acc, self.fcr31) = saved;
    }

    pub fn lookup(&self, addr: u32) -> Option<Func> {
        self.funcs
            .binary_search_by_key(&addr, |e| e.0)
            .ok()
            .map(|i| self.funcs[i].1)
    }

    /// Call through a pointer (jalr, unresolved jr, cross-function jumps).
    pub fn call(&mut self, addr: u32) {
        match self.lookup(addr) {
            Some(f) => f(self),
            None => self.fatal(&format!(
                "call to unknown function {addr:#010x}\n  add `{addr:08x}` to freekstyle/seeds.txt and re-run ee-recomp"
            )),
        }
    }

    /// Execution reached the end of a function body the analyzer found.
    pub fn fell_off(&mut self, addr: u32) {
        self.fatal(&format!("fell off the end of recompiled code at {addr:#010x}"))
    }

    pub fn unimplemented(&mut self, pc: u32, raw: u32) {
        let text = r5900::decode(raw).disasm(pc).to_string();
        self.fatal(&format!("unimplemented instruction at {pc:#010x}: {text}"))
    }

    pub fn fatal(&mut self, msg: &str) -> ! {
        eprintln!("\n[fatal] {msg}");
        eprintln!("recently entered functions (oldest first):");
        for n in 0..self.recent.len() {
            let a = self.recent[(self.recent_pos + n) % self.recent.len()];
            if a != 0 {
                eprintln!("  {a:#010x}");
            }
        }
        self.dump_regs();
        std::process::exit(101);
    }

    pub fn dump_regs(&self) {
        for r in 0..32 {
            eprint!("{:>4}={:016x}{}", r5900::gpr_name(r as u8), self.gpr[r][0], if r % 4 == 3 { "\n" } else { " " });
        }
    }

    // ---- multiply / divide ----

    #[inline]
    fn set_hilo(&mut self, p: usize, lo: u32, hi: u32) {
        self.lo[p] = lo as i32 as i64 as u64;
        self.hi[p] = hi as i32 as i64 as u64;
    }

    #[inline]
    pub fn mult(&mut self, rd: usize, rs: usize, rt: usize, p: usize) {
        let v = (self.r32(rs) as i32 as i64).wrapping_mul(self.r32(rt) as i32 as i64);
        self.set_hilo(p, v as u32, (v >> 32) as u32);
        self.w(rd, self.lo[p]);
    }

    #[inline]
    pub fn multu(&mut self, rd: usize, rs: usize, rt: usize, p: usize) {
        let v = self.r32(rs) as u64 * self.r32(rt) as u64;
        self.set_hilo(p, v as u32, (v >> 32) as u32);
        self.w(rd, self.lo[p]);
    }

    #[inline]
    pub fn madd(&mut self, rd: usize, rs: usize, rt: usize, p: usize) {
        let acc = ((self.hi[p] as u32 as u64) << 32 | self.lo[p] as u32 as u64) as i64;
        let v = acc.wrapping_add((self.r32(rs) as i32 as i64).wrapping_mul(self.r32(rt) as i32 as i64));
        self.set_hilo(p, v as u32, (v >> 32) as u32);
        self.w(rd, self.lo[p]);
    }

    #[inline]
    pub fn maddu(&mut self, rd: usize, rs: usize, rt: usize, p: usize) {
        let acc = (self.hi[p] as u32 as u64) << 32 | self.lo[p] as u32 as u64;
        let v = acc.wrapping_add(self.r32(rs) as u64 * self.r32(rt) as u64);
        self.set_hilo(p, v as u32, (v >> 32) as u32);
        self.w(rd, self.lo[p]);
    }

    #[inline]
    pub fn div(&mut self, rs: usize, rt: usize, p: usize) {
        let (n, d) = (self.r32(rs) as i32, self.r32(rt) as i32);
        let (q, r) = match d {
            0 => (if n < 0 { 1 } else { -1 }, n),
            -1 if n == i32::MIN => (i32::MIN, 0),
            _ => (n / d, n % d),
        };
        self.set_hilo(p, q as u32, r as u32);
    }

    #[inline]
    pub fn divu(&mut self, rs: usize, rt: usize, p: usize) {
        let (n, d) = (self.r32(rs), self.r32(rt));
        let (q, r) = if d == 0 { (u32::MAX, n) } else { (n / d, n % d) };
        self.set_hilo(p, q, r);
    }

    // ---- unaligned loads and stores (little-endian) ----

    pub fn lwl(&mut self, rt: usize, a: u32) {
        let s = (a & 3) * 8;
        let m = self.mem.r32(a & !3);
        let v = (self.r32(rt) as u64 & (0x00ff_ffffu64 >> s)) as u32 | (m << (24 - s));
        self.ws(rt, v);
    }

    pub fn lwr(&mut self, rt: usize, a: u32) {
        let s = (a & 3) * 8;
        let m = self.mem.r32(a & !3);
        let keep = ((0xffff_ff00u64 << (24 - s)) & 0xffff_ffff) as u32;
        let v = (self.r32(rt) & keep) | (m >> s);
        if s == 0 {
            self.ws(rt, v);
        } else {
            self.w(rt, (self.r(rt) & !0xffff_ffff) | v as u64);
        }
    }

    pub fn swl(&mut self, rt: usize, a: u32) {
        let s = (a & 3) * 8;
        let m = self.mem.r32(a & !3);
        let keep = ((0xffff_ff00u64 << s) & 0xffff_ffff) as u32;
        let v = (m & keep) | (self.r32(rt) >> (24 - s));
        self.mem.w32(a & !3, v);
    }

    pub fn swr(&mut self, rt: usize, a: u32) {
        let s = (a & 3) * 8;
        let m = self.mem.r32(a & !3);
        let keep = (0x00ff_ffffu64 >> (24 - s)) as u32;
        let v = (m & keep) | (self.r32(rt) << s);
        self.mem.w32(a & !3, v);
    }

    pub fn ldl(&mut self, rt: usize, a: u32) {
        let s = (a & 7) * 8;
        let m = self.mem.r64(a & !7);
        let keep = (0x00ff_ffff_ffff_ffffu128 >> s) as u64;
        let v = (self.r(rt) & keep) | ((m as u128) << (56 - s)) as u64;
        self.w(rt, v);
    }

    pub fn ldr(&mut self, rt: usize, a: u32) {
        let s = (a & 7) * 8;
        let m = self.mem.r64(a & !7);
        let keep = (0xffff_ffff_ffff_ff00u128 << (56 - s)) as u64;
        self.w(rt, (self.r(rt) & keep) | (m >> s));
    }

    pub fn sdl(&mut self, rt: usize, a: u32) {
        let s = (a & 7) * 8;
        let m = self.mem.r64(a & !7);
        let keep = (0xffff_ffff_ffff_ff00u128 << s) as u64;
        self.mem.w64(a & !7, (m & keep) | (self.r(rt) >> (56 - s)));
    }

    pub fn sdr(&mut self, rt: usize, a: u32) {
        let s = (a & 7) * 8;
        let m = self.mem.r64(a & !7);
        let keep = (0x00ff_ffff_ffff_ffffu128 >> (56 - s)) as u64;
        self.mem.w64(a & !7, (m & keep) | (self.r(rt) << s));
    }

    // ---- COP0 ----

    pub fn mfc0(&mut self, rt: usize, rd: usize) {
        let v = match rd {
            // Count runs at the EE clock, 294.912 MHz.
            9 => (self.started.elapsed().as_nanos() as u64 * 294_912 / 1_000_000) as u32,
            15 => 0x2e20, // PRId: R5900
            _ => self.cop0[rd],
        };
        self.ws(rt, v);
    }

    pub fn mtc0(&mut self, rt: usize, rd: usize) {
        self.cop0[rd] = self.r32(rt);
    }

    pub fn ei(&mut self) {
        self.cop0[12] |= 1 << 16;
    }

    pub fn di(&mut self) {
        self.cop0[12] &= !(1 << 16);
    }

    // ---- COP1 ----

    #[inline(always)]
    pub fn fcc(&self) -> bool {
        self.fcr31 & FCC != 0
    }

    #[inline(always)]
    pub fn set_fcc(&mut self, c: bool) {
        if c {
            self.fcr31 |= FCC;
        } else {
            self.fcr31 &= !FCC;
        }
    }

    pub fn cfc1(&self, r: usize) -> u32 {
        match r {
            0 => 0x2e00,
            31 => self.fcr31,
            _ => 0,
        }
    }

    pub fn ctc1(&mut self, r: usize, v: u32) {
        if r == 31 {
            self.fcr31 = v;
        }
    }

    // ---- COP2 ----

    pub fn lqc2(&mut self, ft: usize, a: u32) {
        let v = self.mem.r128(a & !15);
        self.vu0.set_vf_q(ft, v);
    }

    pub fn sqc2(&mut self, ft: usize, a: u32) {
        let v = self.vu0.vf_q(ft);
        self.mem.w128(a & !15, v);
    }

    /// vlqi: load from VU0 data memory at vi[is], then post-increment.
    pub fn vlqi(&mut self, dest: u8, ft: usize, is: usize) {
        let a = 0x1100_4000 + (self.vu0.vi[is] as u32 & 0xff) * 16;
        let v = self.mem.r128(a);
        let v: [f32; 4] = std::array::from_fn(|n| f32::from_bits((v >> (32 * n)) as u32));
        self.vu0.vw(ft, dest, v);
        if is != 0 {
            self.vu0.vi[is] = self.vu0.vi[is].wrapping_add(1);
        }
    }

    /// vsqi: store the masked fields of fs to vi[it], then post-increment.
    pub fn vsqi(&mut self, dest: u8, fs: usize, it: usize) {
        let a = 0x1100_4000 + (self.vu0.vi[it] as u32 & 0xff) * 16;
        let v = self.vu0.vf[fs];
        for n in 0..4 {
            if dest & (8 >> n) != 0 {
                self.mem.w32(a + 4 * n as u32, v[n].to_bits());
            }
        }
        if it != 0 {
            self.vu0.vi[it] = self.vu0.vi[it].wrapping_add(1);
        }
    }

    pub fn ctc2(&mut self, r: usize, v: u32) {
        self.vu0.ctc2(r, v);
    }

    /// vcallms/vcallmsr: run a VU0 microprogram on the COP2 registers.
    pub fn vu0_callms(&mut self, addr: u32) {
        let m = &mut self.mem;
        m.vu_cache[0].sync(m.vu_code_gen[0], m.vu0_code.len() / 8);
        let env = crate::vumicro::Env { code: &m.vu0_code, data: &mut m.vu0_data, top: 0, itop: 0, kick: None };
        crate::vumicro::run(&mut self.vu0, env, &mut m.vu_cache[0], 0, &m.vu_programs, addr / 8);
    }

    // ---- traps into the runtime ----

    pub fn syscall(&mut self, pc: u32) {
        crate::kernel::syscall(self, pc);
        if (self.poll_interrupts() || self.kernel.need_resched) && !self.in_interrupt {
            crate::kernel::reschedule(self);
        }
    }

    pub fn brk(&mut self, pc: u32) {
        self.fatal(&format!("break at {pc:#010x}"))
    }

    pub fn tlb_op(&mut self, pc: u32) {
        self.kernel.warn_once(&format!("TLB instruction at {pc:#010x} ignored"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unaligned_pairs_match_plain_access() {
        let mut c = Cpu::new(&[]);
        let bytes: Vec<u8> = (0x10..0x30).collect();
        c.mem.write_bytes(0x1000, &bytes);
        for off in 0..8u32 {
            let a = 0x1000 + off;
            // lwl/lwr pair, as compilers emit for an unaligned 32-bit load.
            c.lwl(8, a + 3);
            c.lwr(8, a);
            let want = u32::from_le_bytes(bytes[off as usize..off as usize + 4].try_into().unwrap());
            assert_eq!(c.r32(8), want, "lwl/lwr at +{off}");
            // ldl/ldr pair for 64-bit.
            c.ldl(9, a + 7);
            c.ldr(9, a);
            let want64 = u64::from_le_bytes(bytes[off as usize..off as usize + 8].try_into().unwrap());
            assert_eq!(c.r(9), want64, "ldl/ldr at +{off}");
            // Stores write the same bytes back to a fresh buffer.
            c.swl(8, 0x2003 + off);
            c.swr(8, 0x2000 + off);
            assert_eq!(c.mem.r32(0x2000 + off).to_le_bytes(), want.to_le_bytes());
            c.sdl(9, 0x3007 + off);
            c.sdr(9, 0x3000 + off);
            let mut got = [0u8; 8];
            for (n, b) in got.iter_mut().enumerate() {
                *b = c.mem.r8(0x3000 + off + n as u32);
            }
            assert_eq!(u64::from_le_bytes(got), want64);
        }
    }

    #[test]
    fn mult_div_edge_cases() {
        let mut c = Cpu::new(&[]);
        c.ws(4, 0x8000_0000);
        c.ws(5, u32::MAX);
        c.div(4, 5, 0);
        assert_eq!((c.lo[0] as u32, c.hi[0] as u32), (0x8000_0000, 0));
        c.w(5, 0);
        c.div(4, 5, 0);
        assert_eq!(c.lo[0] as u32, 1);
        c.ws(4, 0xffff_fffe);
        c.ws(5, 3);
        c.mult(2, 4, 5, 1);
        assert_eq!(c.r(2) as i64, -6);
        assert_eq!(c.lo[1] as i64, -6);
    }
}
