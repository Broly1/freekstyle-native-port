//! High-level emulation of the EE kernel: syscalls, threads, semaphores,
//! interrupt handler registration and alarms.
//!
//! The syscall number is in $v1; negative numbers are the interrupt-safe
//! `i*` variants of the same call. Arguments are in $a0-$a3 and $t0-$t3,
//! and the result goes in $v0.
//!
//! # Threads
//!
//! The EE is single-core with a priority scheduler (lower number = higher
//! priority, FIFO among equals, no time slicing). Each guest thread runs on
//! its own host thread so that recompiled code can block in the middle of
//! a native call stack, but only the thread holding the baton runs: a
//! switch saves the outgoing registers, loads the incoming ones, hands the
//! baton over and waits to get it back. All guest state lives in the one
//! `Cpu`, which is therefore touched by exactly one host thread at a time.

use std::collections::{HashSet, VecDeque};
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::cpu::{Context, Cpu};

const V0: usize = 2;
const V1: usize = 3;
const A0: usize = 4;

/// Stack reserved per guest thread on the host. Recompiled frames are
/// larger than the guest's, and deep guest call chains are native recursion.
const HOST_STACK: usize = 256 << 20;

/// NTSC horizontal sync rate: the unit of alarm times.
const HSYNC_HZ: f64 = 15_734.25;

/// Kernel error codes returned in $v0.
const KE_SEMA_ZERO: i32 = -419;
const KE_ILLEGAL_THID: i32 = -406;
const KE_UNKNOWN_SEMID: i32 = -408;

/// ThreadStatus values reported by ReferThreadStatus.
const THS_RUN: u32 = 0x01;
const THS_READY: u32 = 0x02;
const THS_WAIT: u32 = 0x04;
const THS_SUSPEND: u32 = 0x08;
const THS_DORMANT: u32 = 0x10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Dormant,
    Ready,
    Running,
    Waiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    None,
    Sleep,
    Sema(u32),
}

/// One host thread's turn to run guest code.
#[derive(Default)]
pub struct Baton {
    go: Mutex<bool>,
    cv: Condvar,
}

impl Baton {
    fn give(&self) {
        *self.go.lock().unwrap() = true;
        self.cv.notify_one();
    }

    fn take(&self) {
        let mut go = self.go.lock().unwrap();
        while !*go {
            go = self.cv.wait(go).unwrap();
        }
        *go = false;
    }
}

pub struct Thread {
    pub entry: u32,
    pub stack: u32,
    pub stack_size: u32,
    pub gp: u32,
    pub init_priority: u32,
    pub priority: u32,
    pub attr: u32,
    pub option: u32,
    pub state: State,
    pub wait: Wait,
    pub suspended: bool,
    pub wakeups: u32,
    /// Registers while not running.
    ctx: Option<Box<Context>>,
    /// Order among ready threads of equal priority.
    seq: i64,
    baton: Arc<Baton>,
    host_running: bool,
    /// Set when terminated while blocked; the host thread unwinds if it
    /// ever runs again.
    killed: bool,
    deleted: bool,
}

pub struct Sema {
    pub count: i32,
    pub max: i32,
    pub init: i32,
    pub attr: u32,
    pub option: u32,
    pub waiters: VecDeque<usize>,
}

pub struct Handler {
    pub cause: u32,
    pub func: u32,
    pub arg: u32,
    pub dmac: bool,
    pub enabled: bool,
}

pub struct Alarm {
    pub id: u32,
    pub due: Instant,
    pub ticks: u16,
    pub func: u32,
    pub arg: u32,
}

impl Thread {
    fn new(entry: u32, stack: u32, stack_size: u32, gp: u32, priority: u32) -> Thread {
        Thread {
            entry,
            stack,
            stack_size,
            gp,
            init_priority: priority,
            priority,
            attr: 0,
            option: 0,
            state: State::Dormant,
            wait: Wait::None,
            suspended: false,
            wakeups: 0,
            ctx: None,
            seq: 0,
            baton: Arc::new(Baton::default()),
            host_running: false,
            killed: false,
            deleted: false,
        }
    }
}

/// Payload used to unwind a guest thread's host stack on ExitThread.
struct ThreadExit;

pub struct Kernel {
    pub threads: Vec<Thread>,
    pub current: usize,
    pub semas: Vec<Option<Sema>>,
    pub handlers: Vec<Handler>,
    pub alarms: Vec<Alarm>,
    next_alarm: u32,
    pub heap_end: u32,
    pub gs_imr: u32,
    pub sif_regs: [u32; 32],
    next_seq: i64,
    /// A thread was readied from interrupt context and may preempt the
    /// current one at the next safe point.
    pub need_resched: bool,
    warned: HashSet<String>,
    pub trace: bool,
}

impl Kernel {
    pub fn new() -> Kernel {
        // The main thread is already running on the host thread that boots
        // the game; its stack and gp are filled in by SetupThread.
        let mut main = Thread::new(0, 0, 0, 0, 0);
        main.state = State::Running;
        main.host_running = true;
        let mut idle = Thread::new(0, 0, 0, 0, 127);
        idle.deleted = true;
        Kernel {
            // Thread 0 is the kernel's idle thread; the game's main thread is 1.
            threads: vec![idle, main],
            current: 1,
            semas: Vec::new(),
            handlers: Vec::new(),
            alarms: Vec::new(),
            next_alarm: 1,
            heap_end: 0,
            gs_imr: 0,
            sif_regs: [0; 32],
            next_seq: 1,
            need_resched: false,
            warned: HashSet::new(),
            trace: std::env::var_os("FREEK_SYSCALL_LOG").is_some(),
        }
    }

    pub fn warn_once(&mut self, msg: &str) {
        if self.warned.insert(msg.to_string()) {
            eprintln!("[warn] {msg}");
        }
    }

    fn seq(&mut self) -> i64 {
        self.next_seq += 1;
        self.next_seq
    }

    fn make_ready(&mut self, t: usize) {
        self.threads[t].state = State::Ready;
        self.threads[t].wait = Wait::None;
        self.threads[t].seq = self.seq();
        if self.threads[t].priority < self.threads[self.current].priority || self.threads[self.current].state != State::Running {
            self.need_resched = true;
        }
    }

    /// Highest-priority ready thread, FIFO among equals.
    fn best_ready(&self) -> Option<usize> {
        self.threads
            .iter()
            .enumerate()
            .filter(|(_, t)| t.state == State::Ready && !t.suspended)
            .min_by_key(|(_, t)| (t.priority, t.seq))
            .map(|(i, _)| i)
    }

    fn thread_index(&self, id: u32) -> Option<usize> {
        let i = if id == 0 { self.current } else { id as usize };
        self.threads.get(i).filter(|t| !t.deleted).map(|_| i)
    }

    fn sema(&mut self, id: u32) -> Option<&mut Sema> {
        self.semas.get_mut((id as usize).wrapping_sub(1)).and_then(Option::as_mut)
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

// ---- scheduling ----

/// Give the CPU to the best runnable thread if it should preempt the
/// current one. Call at points where a switch is allowed (not inside an
/// interrupt handler).
pub fn reschedule(c: &mut Cpu) {
    c.kernel.need_resched = false;
    loop {
        let cur = c.kernel.current;
        let running = c.kernel.threads[cur].state == State::Running && !c.kernel.threads[cur].suspended;
        match c.kernel.best_ready() {
            Some(b) if !running || c.kernel.threads[b].priority < c.kernel.threads[cur].priority => {
                if running {
                    // Preempted threads go to the front of their priority level.
                    c.kernel.threads[cur].state = State::Ready;
                    c.kernel.threads[cur].seq = -c.kernel.seq();
                }
                switch_to(c, b);
                return;
            }
            Some(_) => return,
            None if running => return,
            None => idle(c),
        }
    }
}

/// Every thread is blocked: let time pass until an interrupt handler or
/// alarm makes one ready.
fn idle(c: &mut Cpu) {
    let start = Instant::now();
    let mut warned = false;
    while c.kernel.best_ready().is_none() {
        std::thread::sleep(Duration::from_micros(200));
        c.poll_interrupts();
        if !warned && start.elapsed() > Duration::from_secs(5) {
            warned = true;
            eprintln!("[kernel] all threads blocked for 5s:");
            for (i, t) in c.kernel.threads.iter().enumerate().filter(|(_, t)| !t.deleted) {
                eprintln!("  thread {i}: {:?} {:?} prio={} entry={:#x}", t.state, t.wait, t.priority, t.entry);
            }
        }
    }
}

/// Switch from the current thread to `to` and block until scheduled again.
fn switch_to(c: &mut Cpu, to: usize) {
    let from = c.kernel.current;
    let ctx = Box::new(c.save_context());
    c.kernel.threads[from].ctx = Some(ctx);
    hand_over(c, to);
    let me = c.kernel.threads[from].baton.clone();
    me.take();
    // Resumed: whoever switched back loaded our registers.
    if c.kernel.threads[from].killed {
        panic::resume_unwind(Box::new(ThreadExit));
    }
}

/// Load `to`'s registers, mark it running and pass it the baton (starting
/// its host thread if needed). Does not wait.
fn hand_over(c: &mut Cpu, to: usize) {
    let ctx = c.kernel.threads[to].ctx.take().expect("ready thread without saved context");
    c.load_context(&ctx);
    c.kernel.current = to;
    c.kernel.threads[to].state = State::Running;
    if c.kernel.trace {
        eprintln!("[sched] -> thread {to}");
    }
    if !c.kernel.threads[to].host_running {
        c.kernel.threads[to].host_running = true;
        spawn_host(c, to);
    }
    c.kernel.threads[to].baton.give();
}

struct CpuPtr(*mut Cpu);
// SAFETY: the pointer is only dereferenced by the host thread that holds the
// baton, so the Cpu is never accessed from two threads at once.
unsafe impl Send for CpuPtr {}

fn spawn_host(c: &mut Cpu, t: usize) {
    let ptr = CpuPtr(c as *mut Cpu);
    let baton = c.kernel.threads[t].baton.clone();
    let entry = c.kernel.threads[t].entry;
    std::thread::Builder::new()
        .name(format!("ee-thread-{t}"))
        .stack_size(HOST_STACK)
        .spawn(move || {
            let ptr = ptr;
            baton.take();
            // SAFETY: we hold the baton; see CpuPtr.
            let c = unsafe { &mut *ptr.0 };
            run_thread_body(c, entry);
            exit_current(c);
        })
        .expect("spawn guest thread");
}

/// Run a guest thread's entry function, turning ExitThread unwinds into a
/// normal return. Used for every thread including main.
pub fn run_thread_body(c: &mut Cpu, entry: u32) {
    let result = panic::catch_unwind(AssertUnwindSafe(|| c.call(entry)));
    if let Err(payload) = result {
        if !payload.is::<ThreadExit>() {
            panic::resume_unwind(payload);
        }
    }
}

/// The current thread has finished: make it dormant and pass the CPU on.
/// The calling host thread then ends.
fn exit_current(c: &mut Cpu) {
    let cur = c.kernel.current;
    let t = &mut c.kernel.threads[cur];
    t.state = State::Dormant;
    t.host_running = false;
    t.killed = false;
    if c.kernel.trace {
        eprintln!("[sched] thread {cur} exited");
    }
    loop {
        if let Some(b) = c.kernel.best_ready() {
            hand_over(c, b);
            return;
        }
        idle(c);
    }
}

/// Block the current thread on `wait` and run others until it is woken.
fn block(c: &mut Cpu, wait: Wait) {
    let cur = c.kernel.current;
    c.kernel.threads[cur].state = State::Waiting;
    c.kernel.threads[cur].wait = wait;
    reschedule(c);
}

fn fresh_context(c: &Cpu, t: &Thread, arg: u32) -> Context {
    let mut ctx = c.save_context();
    ctx.gpr = [[0; 2]; 32];
    let sext = |v: u32| v as i32 as i64 as u64;
    ctx.gpr[A0][0] = sext(arg);
    ctx.gpr[28][0] = sext(t.gp);
    ctx.gpr[29][0] = sext((t.stack + t.stack_size - 0x2a0) & !15);
    ctx
}

// ---- syscalls ----

fn arg(c: &Cpu, n: usize) -> u32 {
    // $a0-$a3 are r4-r7, $t0-$t3 are r8-r11.
    c.r32(A0 + n)
}

pub fn syscall(c: &mut Cpu, pc: u32) {
    let num = c.r32(V1) as i32;
    let interrupt_safe = num < 0;
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
            let h = Handler { cause: arg(c, 0), func: arg(c, 1), arg: arg(c, 3), dmac, enabled: true };
            log(c, if dmac { "AddDmacHandler" } else { "AddIntcHandler" }, &format!("cause={} func={:#x}", h.cause, h.func));
            c.kernel.handlers.push(h);
            c.kernel.handlers.len() as u32
        }
        0x11 | 0x13 => {
            let id = arg(c, 1);
            if let Some(h) = c.kernel.handlers.get_mut((id as usize).wrapping_sub(1)) {
                h.enabled = false;
            }
            0
        }
        // Enable/DisableIntc, Enable/DisableDmac set mask bits directly.
        0x14 | 0x15 => {
            let bit = 1 << (arg(c, 0) & 31);
            let mask = c.mem.io.reg(crate::io::INTC_MASK);
            let new = if num.unsigned_abs() == 0x14 { mask | bit } else { mask & !bit };
            c.mem.io.set_reg(crate::io::INTC_MASK, new);
            (mask & bit != 0) as u32
        }
        0x16 | 0x17 => {
            let bit = 1 << (16 + (arg(c, 0) & 15));
            let stat = c.mem.io.reg(crate::io::D_STAT);
            let new = if num.unsigned_abs() == 0x16 { stat | bit } else { stat & !bit };
            c.mem.io.set_reg(crate::io::D_STAT, new);
            (stat & bit != 0) as u32
        }
        0x20 => create_thread(c),
        0x21 => match c.kernel.thread_index(arg(c, 0)) {
            Some(t) if c.kernel.threads[t].state == State::Dormant => {
                c.kernel.threads[t].deleted = true;
                0
            }
            _ => KE_ILLEGAL_THID as u32,
        },
        0x22 => start_thread(c, interrupt_safe),
        0x23 | 0x24 => {
            if num.unsigned_abs() == 0x24 {
                let cur = c.kernel.current;
                c.kernel.threads[cur].deleted = true;
            }
            panic::resume_unwind(Box::new(ThreadExit));
        }
        0x25 | 0x26 => terminate_thread(c),
        0x27 | 0x28 => 0, // Disable/EnableDispatchThread
        0x29 | 0x2a => {
            let Some(t) = c.kernel.thread_index(arg(c, 0)) else {
                return finish(c, num, pc, KE_ILLEGAL_THID as u32);
            };
            let prio = arg(c, 1);
            let old = std::mem::replace(&mut c.kernel.threads[t].priority, prio);
            if !interrupt_safe {
                reschedule(c);
            }
            old
        }
        0x2b | 0x2c => {
            // RotateThreadReadyQueue: the current thread yields to equals.
            let prio = arg(c, 0);
            let cur = c.kernel.current;
            if c.kernel.threads[cur].priority == prio && c.kernel.threads[cur].state == State::Running {
                let others = c.kernel.threads.iter().any(|t| t.state == State::Ready && t.priority == prio && !t.suspended);
                if others {
                    c.kernel.make_ready(cur);
                    if !interrupt_safe {
                        reschedule(c);
                    }
                }
            }
            0
        }
        0x2d | 0x2e => {
            let Some(t) = c.kernel.thread_index(arg(c, 0)) else {
                return finish(c, num, pc, KE_ILLEGAL_THID as u32);
            };
            if c.kernel.threads[t].state == State::Waiting {
                remove_waiter(c, t);
                c.kernel.make_ready(t);
                if !interrupt_safe {
                    reschedule(c);
                }
            }
            0
        }
        0x2f => c.kernel.current as u32,
        0x30 | 0x31 => refer_thread_status(c),
        0x32 => {
            let cur = c.kernel.current;
            if c.kernel.threads[cur].wakeups > 0 {
                c.kernel.threads[cur].wakeups -= 1;
            } else {
                block(c, Wait::Sleep);
            }
            0
        }
        0x33 | 0x34 => {
            let Some(t) = c.kernel.thread_index(arg(c, 0)) else {
                return finish(c, num, pc, KE_ILLEGAL_THID as u32);
            };
            if c.kernel.threads[t].wait == Wait::Sleep {
                c.kernel.make_ready(t);
                if !interrupt_safe {
                    reschedule(c);
                }
            } else {
                c.kernel.threads[t].wakeups += 1;
            }
            0
        }
        0x35 | 0x36 => match c.kernel.thread_index(arg(c, 0)) {
            Some(t) => std::mem::take(&mut c.kernel.threads[t].wakeups),
            None => KE_ILLEGAL_THID as u32,
        },
        0x37 | 0x38 => {
            if let Some(t) = c.kernel.thread_index(arg(c, 0)) {
                c.kernel.threads[t].suspended = true;
                if t == c.kernel.current && !interrupt_safe {
                    reschedule(c);
                }
            }
            0
        }
        0x39 | 0x3a => {
            if let Some(t) = c.kernel.thread_index(arg(c, 0)) {
                c.kernel.threads[t].suspended = false;
                if !interrupt_safe {
                    reschedule(c);
                }
            }
            0
        }
        0x3c => {
            // SetupThread(gp, stack, stack_size, args, root): initial sp.
            let (stack, size) = (arg(c, 1), arg(c, 2));
            let top = if stack == u32::MAX { 0x0200_0000 } else { stack + size };
            c.kernel.heap_end = top - size;
            let gp = arg(c, 0);
            let main = &mut c.kernel.threads[1];
            (main.gp, main.stack, main.stack_size) = (gp, top - size, size);
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
            let s = Sema {
                count: c.mem.r32(p + 8) as i32,
                max: c.mem.r32(p + 4) as i32,
                init: c.mem.r32(p + 8) as i32,
                attr: c.mem.r32(p + 16),
                option: c.mem.r32(p + 20),
                waiters: VecDeque::new(),
            };
            log(c, "CreateSema", &format!("init={} max={}", s.count, s.max));
            c.kernel.semas.push(Some(s));
            c.kernel.semas.len() as u32
        }
        0x41 => {
            let id = arg(c, 0);
            let Some(slot) = c.kernel.semas.get_mut((id as usize).wrapping_sub(1)) else {
                return finish(c, num, pc, KE_UNKNOWN_SEMID as u32);
            };
            if let Some(s) = slot.take() {
                for t in s.waiters {
                    c.kernel.make_ready(t);
                }
                reschedule(c);
            }
            id
        }
        0x42 | 0x43 => {
            let id = arg(c, 0);
            let Some(s) = c.kernel.sema(id) else {
                return finish(c, num, pc, KE_UNKNOWN_SEMID as u32);
            };
            match s.waiters.pop_front() {
                Some(t) => {
                    c.kernel.make_ready(t);
                    if !interrupt_safe {
                        reschedule(c);
                    }
                }
                None => s.count += 1,
            }
            id
        }
        0x44 => {
            let id = arg(c, 0);
            let cur = c.kernel.current;
            let Some(s) = c.kernel.sema(id) else {
                return finish(c, num, pc, KE_UNKNOWN_SEMID as u32);
            };
            if s.count > 0 {
                s.count -= 1;
            } else {
                s.waiters.push_back(cur);
                block(c, Wait::Sema(id));
            }
            id
        }
        0x45 | 0x46 => {
            let id = arg(c, 0);
            match c.kernel.sema(id) {
                Some(s) if s.count > 0 => {
                    s.count -= 1;
                    id
                }
                Some(_) => KE_SEMA_ZERO as u32,
                None => KE_UNKNOWN_SEMID as u32,
            }
        }
        0x47 | 0x48 => {
            let (id, p) = (arg(c, 0), arg(c, 1));
            let Some(s) = c.kernel.sema(id) else {
                return finish(c, num, pc, KE_UNKNOWN_SEMID as u32);
            };
            let v = [s.count as u32, s.max as u32, s.init as u32, s.waiters.len() as u32, s.attr, s.option];
            for (n, w) in v.iter().enumerate() {
                c.mem.w32(p + 4 * n as u32, *w);
            }
            id
        }
        // Kernel patching done by the game's libkernel (SetSyscall, kernel
        // memcpy, entry lookup). Our kernel implements the patched calls
        // natively, so these are no-ops.
        0x5a | 0x5b | 0x74 => 0,
        0x64 | 0x68 => 0, // FlushCache: no caches to flush.
        0x70 => c.kernel.gs_imr,
        0x71 => {
            let imr = arg(c, 0);
            std::mem::replace(&mut c.kernel.gs_imr, imr)
        }
        0x76 => u32::MAX, // SifDmaStat: transfer already complete.
        0x77 => {
            // SifSetDma(transfers, count): { src, dest (IOP), size, attr }.
            let (list, count) = (arg(c, 0), arg(c, 1));
            log(c, "SifSetDma", &format!("transfers={list:#x} count={count}"));
            for i in 0..count {
                let t = list + i * 16;
                let (src, dest, size) = (c.mem.r32(t), c.mem.r32(t + 4), c.mem.r32(t + 8));
                let data = (0..size).map(|n| c.mem.r8(src + n)).collect();
                if let Some(link) = &c.iop.link {
                    link.send(ps2_iop::ToIop::Dma { dest, data });
                }
            }
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
        // Alarms, installed by libkernel as custom syscalls 0xfc-0xff.
        0xfc | 0xfd => set_alarm(c),
        0xfe | 0xff => {
            let id = arg(c, 0);
            let before = c.kernel.alarms.len();
            c.kernel.alarms.retain(|a| a.id != id);
            if c.kernel.alarms.len() < before { id } else { u32::MAX }
        }
        n => {
            c.kernel.warn_once(&format!("unhandled syscall {num:#x} (|{n:#x}|) at {pc:#x}, returning 0"));
            0
        }
    };
    finish(c, num, pc, ret);
}

fn finish(c: &mut Cpu, num: i32, pc: u32, ret: u32) {
    if c.kernel.trace {
        eprintln!("[syscall] {num:#x} at {pc:#x} -> {ret:#x} (thread {})", c.kernel.current);
    }
    c.ws(V0, ret);
}

/// CreateThread(ThreadParam*): { status, func, stack, stack_size, gp,
/// initial_priority, current_priority, attr, option }.
fn create_thread(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let mut r = |o: u32| c.mem.r32(p + o);
    let mut t = Thread::new(r(4), r(8), r(12), r(16), r(20));
    (t.attr, t.option) = (r(28), r(32));
    log(c, "CreateThread", &format!("func={:#x} stack={:#x}+{:#x} prio={}", t.entry, t.stack, t.stack_size, t.priority));
    // Reuse a deleted slot (never 0 or 1).
    if let Some(i) = (2..c.kernel.threads.len()).find(|&i| c.kernel.threads[i].deleted && !c.kernel.threads[i].host_running) {
        c.kernel.threads[i] = t;
        return i as u32;
    }
    c.kernel.threads.push(t);
    (c.kernel.threads.len() - 1) as u32
}

fn start_thread(c: &mut Cpu, interrupt_safe: bool) -> u32 {
    let a = arg(c, 1);
    let Some(t) = c.kernel.thread_index(arg(c, 0)) else {
        return KE_ILLEGAL_THID as u32;
    };
    if c.kernel.threads[t].state != State::Dormant {
        return KE_ILLEGAL_THID as u32;
    }
    log(c, "StartThread", &format!("id={t} entry={:#x} arg={a:#x}", c.kernel.threads[t].entry));
    let ctx = fresh_context(c, &c.kernel.threads[t], a);
    let th = &mut c.kernel.threads[t];
    th.ctx = Some(Box::new(ctx));
    th.priority = th.init_priority;
    th.wakeups = 0;
    // A previous host thread that was killed while blocked never runs again;
    // give the restarted thread a fresh baton.
    if th.host_running {
        th.baton = Arc::new(Baton::default());
        th.host_running = false;
    }
    c.kernel.make_ready(t);
    if !interrupt_safe {
        // Our own return value must be in place before we might be switched out.
        c.ws(V0, 0);
        reschedule(c);
    }
    0
}

fn remove_waiter(c: &mut Cpu, t: usize) {
    if let Wait::Sema(id) = c.kernel.threads[t].wait {
        if let Some(s) = c.kernel.sema(id) {
            s.waiters.retain(|&w| w != t);
        }
    }
}

fn terminate_thread(c: &mut Cpu) -> u32 {
    let Some(t) = c.kernel.thread_index(arg(c, 0)) else {
        return KE_ILLEGAL_THID as u32;
    };
    if t == c.kernel.current {
        return KE_ILLEGAL_THID as u32;
    }
    remove_waiter(c, t);
    let th = &mut c.kernel.threads[t];
    if th.host_running {
        th.killed = true;
    }
    th.state = State::Dormant;
    th.wait = Wait::None;
    th.ctx = None;
    0
}

/// ReferThreadStatus(id, ThreadParam*): returns the status bits.
fn refer_thread_status(c: &mut Cpu) -> u32 {
    let (id, p) = (arg(c, 0), arg(c, 1));
    let Some(t) = c.kernel.thread_index(id) else {
        return KE_ILLEGAL_THID as u32;
    };
    let th = &c.kernel.threads[t];
    let mut status = match th.state {
        State::Running => THS_RUN,
        State::Ready => THS_READY,
        State::Waiting => THS_WAIT,
        State::Dormant => THS_DORMANT,
    };
    if th.suspended {
        status |= THS_SUSPEND;
    }
    let (wait_type, wait_id) = match th.wait {
        Wait::None => (0, 0),
        Wait::Sleep => (1, 0),
        Wait::Sema(s) => (2, s),
    };
    if p != 0 {
        let v = [
            status,
            th.entry,
            th.stack,
            th.stack_size,
            th.gp,
            th.init_priority,
            th.priority,
            th.attr,
            th.option,
            wait_type,
            wait_id,
            th.wakeups,
        ];
        for (n, w) in v.iter().enumerate() {
            c.mem.w32(p + 4 * n as u32, *w);
        }
    }
    status
}

/// SetAlarm(time in H-lines, handler(id, time, arg), arg).
fn set_alarm(c: &mut Cpu) -> u32 {
    let ticks = arg(c, 0) as u16;
    let id = c.kernel.next_alarm;
    c.kernel.next_alarm += 1;
    let due = Instant::now() + Duration::from_secs_f64(ticks as f64 / HSYNC_HZ);
    c.kernel.alarms.push(Alarm { id, due, ticks, func: arg(c, 1), arg: arg(c, 2) });
    id
}

/// Alarms whose time has come, removed from the list.
pub fn due_alarms(k: &mut Kernel) -> Vec<Alarm> {
    let now = Instant::now();
    let (due, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut k.alarms).into_iter().partition(|a| a.due <= now);
    k.alarms = pending;
    due
}

fn log(c: &Cpu, name: &str, detail: &str) {
    if c.kernel.trace || std::env::var_os("FREEK_QUIET").is_none() {
        eprintln!("[kernel] {name} {detail}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    static LOG: StdMutex<Vec<&'static str>> = StdMutex::new(Vec::new());
    const WORKER: u32 = 0x1000;
    const SEMA_PARAM: u32 = 0x8000;
    const THREAD_PARAM: u32 = 0x8100;

    fn note(s: &'static str) {
        LOG.lock().unwrap().push(s);
    }

    fn sys(c: &mut Cpu, num: i32, args: &[u32]) -> u32 {
        for (n, a) in args.iter().enumerate() {
            c.ws(A0 + n, *a);
        }
        c.ws(V1, num as u32);
        c.syscall(0);
        c.r32(V0)
    }

    /// Higher-priority worker: waits on the semaphore twice, then exits.
    fn worker(c: &mut Cpu) {
        let sema = c.r32(A0);
        note("worker start");
        sys(c, 0x44, &[sema]);
        note("worker woke");
        sys(c, 0x44, &[sema]);
        note("worker woke again");
        sys(c, 0x23, &[]); // ExitThread: unwinds
        unreachable!();
    }

    static FUNCS: [(u32, crate::Func); 1] = [(WORKER, worker)];

    #[test]
    fn priority_semaphores_and_exit() {
        let mut c = Box::new(Cpu::new(&FUNCS));
        c.ei();
        // Main thread priority 10 via ChangeThreadPriority(0, 10).
        sys(&mut c, 0x29, &[0, 10]);
        // CreateSema(init 0, max 2)
        c.mem.w32(SEMA_PARAM + 4, 2);
        c.mem.w32(SEMA_PARAM + 8, 0);
        let sema = sys(&mut c, 0x40, &[SEMA_PARAM]);
        // CreateThread(worker, stack 0x10000+0x1000, prio 5)
        for (o, v) in [(4, WORKER), (8, 0x10000), (12, 0x1000), (16, 0), (20, 5)] {
            c.mem.w32(THREAD_PARAM + o, v);
        }
        let tid = sys(&mut c, 0x20, &[THREAD_PARAM]);
        note("main start worker");
        // The worker preempts immediately and blocks on the semaphore.
        sys(&mut c, 0x22, &[tid, sema]);
        note("main after start");
        sys(&mut c, 0x42, &[sema]);
        note("main after signal 1");
        sys(&mut c, 0x42, &[sema]);
        note("main after signal 2");
        assert_eq!(
            *LOG.lock().unwrap(),
            [
                "main start worker",
                "worker start",
                "main after start",
                "worker woke",
                "main after signal 1",
                "worker woke again",
                "main after signal 2",
            ]
        );
        assert_eq!(c.kernel.threads[tid as usize].state, State::Dormant);
        assert_eq!(c.kernel.current, 1);
        // Leak: the worker's host thread has ended, but keep the Cpu alive
        // in case of stragglers.
        std::mem::forget(c);
    }
}
