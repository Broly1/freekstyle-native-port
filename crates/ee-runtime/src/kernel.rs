//! High-level emulation of the EE kernel's syscalls.
//!
//! The number is in $v1; negative numbers are the interrupt-safe `i*`
//! variants of the same call. Arguments are in $a0-$a3 and $t0-$t3, and the
//! result goes in $v0. Threads are recorded but not scheduled yet: the main
//! thread runs alone, and a semaphore wait that would block only warns.

use std::collections::HashSet;

use crate::cpu::Cpu;

const V0: usize = 2;
const V1: usize = 3;
const A0: usize = 4;

pub struct Thread {
    pub func: u32,
    pub stack: u32,
    pub stack_size: u32,
    pub gp: u32,
    pub priority: u32,
    pub started: bool,
}

pub struct Sema {
    pub count: i32,
    pub max: i32,
}

pub struct Handler {
    pub cause: u32,
    pub func: u32,
    pub arg: u32,
    pub dmac: bool,
}

pub struct Kernel {
    pub threads: Vec<Thread>,
    pub current: usize,
    pub semas: Vec<Option<Sema>>,
    pub handlers: Vec<Handler>,
    pub heap_end: u32,
    pub gs_imr: u32,
    pub sif_regs: [u32; 32],
    warned: HashSet<String>,
    pub trace: bool,
}

impl Kernel {
    pub fn new() -> Kernel {
        Kernel {
            // Thread 0 is the kernel idle thread; the game's main thread is 1.
            threads: vec![
                Thread { func: 0, stack: 0, stack_size: 0, gp: 0, priority: 127, started: true },
                Thread { func: 0, stack: 0, stack_size: 0, gp: 0, priority: 0, started: true },
            ],
            current: 1,
            semas: Vec::new(),
            handlers: Vec::new(),
            heap_end: 0,
            gs_imr: 0,
            sif_regs: [0; 32],
            warned: HashSet::new(),
            trace: std::env::var_os("FREEK_SYSCALL_LOG").is_some(),
        }
    }

    pub fn warn_once(&mut self, msg: &str) {
        if self.warned.insert(msg.to_string()) {
            eprintln!("[warn] {msg}");
        }
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

fn arg(c: &Cpu, n: usize) -> u32 {
    // $a0-$a3 are r4-r7, $t0-$t3 are r8-r11.
    c.r32(A0 + n)
}

pub fn syscall(c: &mut Cpu, pc: u32) {
    let num = c.r32(V1) as i32;
    let ret: u32 = match num.unsigned_abs() {
        0x02 => {
            log(c, "SetGsCrt", &format!("interlace={} mode={:#x} field={}", arg(c, 0), arg(c, 1), arg(c, 2)));
            0
        }
        0x04 => {
            eprintln!("[kernel] Exit({})", arg(c, 0) as i32);
            std::process::exit(arg(c, 0) as i32);
        }
        0x10 | 0x12 => {
            let dmac = num.unsigned_abs() == 0x12;
            let h = Handler { cause: arg(c, 0), func: arg(c, 1), arg: arg(c, 3), dmac };
            log(c, if dmac { "AddDmacHandler" } else { "AddIntcHandler" }, &format!("cause={} func={:#x}", h.cause, h.func));
            c.kernel.handlers.push(h);
            c.kernel.handlers.len() as u32
        }
        0x11 | 0x13 => 0,
        // Enable/DisableIntc, Enable/DisableDmac: no interrupts are raised yet.
        0x14..=0x17 => 1,
        0x20 => {
            let p = arg(c, 0);
            let t = Thread {
                func: c.mem.r32(p + 4),
                stack: c.mem.r32(p + 8),
                stack_size: c.mem.r32(p + 12),
                gp: c.mem.r32(p + 16),
                priority: c.mem.r32(p + 20),
                started: false,
            };
            log(c, "CreateThread", &format!("func={:#x} stack={:#x}+{:#x} prio={}", t.func, t.stack, t.stack_size, t.priority));
            c.kernel.threads.push(t);
            (c.kernel.threads.len() - 1) as u32
        }
        0x22 => {
            let id = arg(c, 0) as usize;
            log(c, "StartThread", &format!("id={id} arg={:#x}", arg(c, 1)));
            if let Some(t) = c.kernel.threads.get_mut(id) {
                t.started = true;
            }
            c.kernel.warn_once("StartThread: threads are not scheduled yet; the new thread will not run");
            0
        }
        0x29 | 0x2a => {
            let id = match arg(c, 0) {
                0 => c.kernel.current,
                n => n as usize,
            };
            let prio = arg(c, 1);
            match c.kernel.threads.get_mut(id) {
                Some(t) => std::mem::replace(&mut t.priority, prio),
                None => u32::MAX,
            }
        }
        0x2f => c.kernel.current as u32,
        0x3c => {
            // SetupThread(gp, stack, stack_size, args, root): returns the
            // initial stack pointer.
            let (stack, size) = (arg(c, 1), arg(c, 2));
            let top = if stack == u32::MAX { 0x0200_0000 } else { stack + size };
            c.kernel.heap_end = top - size;
            log(c, "SetupThread", &format!("gp={:#x} stack={stack:#x} size={size:#x} -> sp={:#x}", arg(c, 0), top - 0x2a0));
            top - 0x2a0
        }
        0x3d => {
            // SetupHeap(start, size): size -1 means "up to the stack".
            let (start, size) = (arg(c, 0), arg(c, 1));
            if size != u32::MAX {
                c.kernel.heap_end = start + size;
            }
            log(c, "SetupHeap", &format!("start={start:#x} end={:#x}", c.kernel.heap_end));
            start
        }
        0x3e => c.kernel.heap_end,
        0x40 => {
            let p = arg(c, 0);
            let s = Sema { count: c.mem.r32(p + 8) as i32, max: c.mem.r32(p + 4) as i32 };
            log(c, "CreateSema", &format!("init={} max={}", s.count, s.max));
            c.kernel.semas.push(Some(s));
            c.kernel.semas.len() as u32
        }
        0x41 => {
            let id = arg(c, 0);
            if let Some(s) = c.kernel.semas.get_mut(sema_index(id)) {
                *s = None;
            }
            id
        }
        0x42 | 0x43 => {
            let id = arg(c, 0);
            if let Some(Some(s)) = c.kernel.semas.get_mut(sema_index(id)) {
                s.count += 1;
            }
            id
        }
        0x44 | 0x45 | 0x46 => {
            let id = arg(c, 0);
            let wait = num.unsigned_abs() == 0x44;
            match c.kernel.semas.get_mut(sema_index(id)) {
                Some(Some(s)) if s.count > 0 => {
                    s.count -= 1;
                    id
                }
                Some(Some(_)) if wait => {
                    c.kernel.warn_once(&format!("WaitSema({id}) would block; continuing (no scheduler yet) at {pc:#x}"));
                    id
                }
                _ => u32::MAX,
            }
        }
        0x64 | 0x68 => 0, // FlushCache: no caches to flush.
        0x70 => c.kernel.gs_imr,
        0x71 => {
            c.kernel.gs_imr = arg(c, 0);
            0
        }
        0x76 => u32::MAX, // SifDmaStat: transfer already complete.
        0x77 => {
            log(c, "SifSetDma", &format!("transfers={:#x} count={}", arg(c, 0), arg(c, 1)));
            1
        }
        0x78 => 0,
        0x79 => {
            c.kernel.sif_regs[arg(c, 0) as usize & 31] = arg(c, 1);
            0
        }
        0x7a => match arg(c, 0) {
            // SMFLG: IOP has booted and initialized SIF and its command layer.
            4 => 0x7_0000,
            r => c.kernel.sif_regs[r as usize & 31],
        },
        0x7f => 32 << 20,
        n => {
            c.kernel.warn_once(&format!("unhandled syscall {num:#x} (|{n:#x}|) at {pc:#x}, returning 0"));
            0
        }
    };
    if c.kernel.trace {
        eprintln!("[syscall] {num:#x} at {pc:#x} -> {ret:#x}");
    }
    c.ws(V0, ret);
}

/// Semaphore IDs start at 1; 0 and junk map to an out-of-range index.
fn sema_index(id: u32) -> usize {
    (id as usize).wrapping_sub(1)
}

fn log(c: &Cpu, name: &str, detail: &str) {
    if c.kernel.trace || std::env::var_os("FREEK_QUIET").is_none() {
        eprintln!("[kernel] {name} {detail}");
    }
}
