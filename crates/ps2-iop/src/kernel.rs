//! The IOP kernel, implemented natively.
//!
//! Modules import kernel services by library name and ordinal. Each import
//! stub is patched to a SYSCALL whose code names the function (library
//! index << 8 | ordinal), so a call lands in `Machine::hle` with the
//! arguments in a0-a3 and the stack. Calls into other loaded modules (the
//! game's LIBSD) are linked directly.
//!
//! Threads keep full register sets and are scheduled by priority (lower
//! number first) when the running thread blocks. Interrupt, vblank and SIF
//! command handlers run to completion on a separate stack.

use std::collections::{HashMap, HashSet};

use crate::cpu::{Event, Regs, A0, GP, RA, SP, V0};
use crate::{irx, Machine, ToEe, CLOCK};

/// Libraries implemented here, by index in the syscall code.
const LIBRARIES: [&str; 11] = ["sysmem", "intrman", "loadcore", "sifcmd", "sifman", "stdio", "sysclib", "thbase", "thevent", "vblank", "thsemap"];
/// Code for imports from unknown libraries (index into `unknown`).
const UNKNOWN: u32 = 0x8_0000;

/// Where handler and module-start calls return to, and where thread entry
/// functions return to. Both are outside RAM, so never valid code.
const RETURN_MAGIC: u32 = 0x1fc0_0100;
const THREAD_EXIT: u32 = 0x1fc0_0200;

/// Memory layout: modules, then the sysmem heap, then HLE areas.
const MODULE_BASE: u32 = 0x1_0000;
const HEAP: (u32, u32) = (0x8_0000, 0x1e_0000);
const CMD_BUFS: u32 = 0x1e_0000;
const CMD_SLOTS: u32 = 32;
const CMD_SLOT: u32 = 128;
const INTR_STACK_TOP: u32 = 0x1f_8000;
const LOADER_STACK_TOP: u32 = 0x1f_fff0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Dormant,
    Ready,
    Sleeping,
    Delayed(u64),
    WaitEvf { id: usize, bits: u32, mode: u32, res: u32 },
    WaitSema(usize),
    Exited,
}

struct Thread {
    regs: Regs,
    prio: u32,
    state: State,
    wakeups: u32,
    func: u32,
    stack: (u32, u32),
    gp: u32,
    /// Order in which threads became ready, for FIFO among equals.
    seq: u64,
}

#[derive(Clone, Copy)]
struct Handler {
    func: u32,
    arg: u32,
    gp: u32,
}

pub struct Kernel {
    threads: Vec<Thread>,
    cur: Option<usize>,
    seq: u64,
    intr: HashMap<u32, Handler>,
    intr_enabled: u64,
    /// CpuSuspendIntr in effect.
    suspended: bool,
    in_intr: bool,
    vblank: Vec<(u32, Handler)>,
    evf: Vec<u32>,
    sema: Vec<(i32, i32)>,
    heap: Vec<(u32, u32, bool)>,
    sreg: [u32; 32],
    cmd: HashMap<u32, Handler>,
    cmd_slot: u32,
    next_module: u32,
    exports: HashMap<String, Vec<u32>>,
    unknown: Vec<String>,
    warned: HashSet<String>,
    line: String,
    dma_id: u32,
    /// A command went to the EE; sync before running on.
    pub(crate) ee_pending: bool,
    /// IOP_TRACE: log SIF traffic and kernel calls.
    pub(crate) trace: bool,
    trace_calls: bool,
}

impl Kernel {
    pub fn new() -> Kernel {
        Kernel {
            threads: Vec::new(),
            cur: None,
            seq: 0,
            intr: HashMap::new(),
            intr_enabled: 0,
            suspended: false,
            in_intr: false,
            vblank: Vec::new(),
            evf: Vec::new(),
            sema: Vec::new(),
            heap: vec![(HEAP.0, HEAP.1 - HEAP.0, false)],
            sreg: [0; 32],
            cmd: HashMap::new(),
            cmd_slot: 0,
            next_module: MODULE_BASE,
            exports: HashMap::new(),
            unknown: Vec::new(),
            warned: HashSet::new(),
            line: String::new(),
            dma_id: 0,
            ee_pending: false,
            trace: std::env::var_os("IOP_TRACE").is_some(),
            trace_calls: std::env::var("IOP_TRACE").is_ok_and(|v| v == "2"),
        }
    }

    pub fn next_timer(&self) -> Option<u64> {
        self.threads.iter().filter_map(|t| if let State::Delayed(u) = t.state { Some(u) } else { None }).min()
    }

    fn warn_once(&mut self, msg: &str) {
        if self.warned.insert(msg.to_string()) {
            eprintln!("[iop] {msg}");
        }
    }

    fn alloc(&mut self, size: u32, high: bool) -> u32 {
        let size = (size + 255) & !255;
        let pick = if high {
            self.heap.iter().rposition(|&(_, s, used)| !used && s >= size)
        } else {
            self.heap.iter().position(|&(_, s, used)| !used && s >= size)
        };
        let Some(i) = pick else { return 0 };
        let (start, s, _) = self.heap[i];
        if s == size {
            self.heap[i].2 = true;
            return start;
        }
        if high {
            self.heap[i].1 = s - size;
            self.heap.insert(i + 1, (start + s - size, size, true));
            start + s - size
        } else {
            self.heap[i] = (start, size, true);
            self.heap.insert(i + 1, (start + size, s - size, false));
            start
        }
    }

    fn free(&mut self, ptr: u32) -> bool {
        let Some(i) = self.heap.iter().position(|&(s, _, used)| used && s == ptr) else { return false };
        self.heap[i].2 = false;
        // Merge with free neighbours.
        if i + 1 < self.heap.len() && !self.heap[i + 1].2 {
            self.heap[i].1 += self.heap[i + 1].1;
            self.heap.remove(i + 1);
        }
        if i > 0 && !self.heap[i - 1].2 {
            self.heap[i - 1].1 += self.heap[i].1;
            self.heap.remove(i);
        }
        true
    }
}

/// Result of an HLE call.
enum Flow {
    Return(u32),
    /// The current thread blocked (its state is already set).
    Block,
    /// Registers were set directly (longjmp); don't touch v0.
    Jumped,
}

const KE_OK: u32 = 0;
const KE_ERROR: u32 = -1i32 as u32;
const KE_CPUDI: u32 = -102i32 as u32;
const KE_UNKNOWN_THID: u32 = -407i32 as u32;
const KE_CAN_NOT_WAIT: u32 = -410i32 as u32;

impl Machine {
    fn r32(&self, a: u32) -> u32 {
        let i = a as usize & (crate::RAM_SIZE - 1);
        u32::from_le_bytes(self.mem.ram[i..i + 4].try_into().unwrap())
    }

    fn w32(&mut self, a: u32, v: u32) {
        let i = a as usize & (crate::RAM_SIZE - 1);
        self.mem.ram[i..i + 4].copy_from_slice(&v.to_le_bytes());
    }

    // ---- modules ----

    pub(crate) fn load_module(&mut self, name: &str, file: &[u8], args: &[u8]) {
        let base = self.k.next_module;
        let m = match irx::load(file, base) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[iop] {name}: {e}");
                return;
            }
        };
        if base + m.size > HEAP.0 {
            eprintln!("[iop] {name}: no room to load ({} bytes)", m.size);
            return;
        }
        self.k.next_module = (base + m.size + 0xff) & !0xff;
        self.mem.write_bytes(base, &m.image);
        let (imports, exports) = irx::tables(&m);
        for e in exports {
            self.k.exports.insert(e.library, e.funcs);
        }
        let mut hle = 0;
        for imp in &imports {
            if let Some(&target) = self.k.exports.get(&imp.library).and_then(|f| f.get(imp.ordinal as usize)) {
                self.w32(imp.stub, irx::jump(target));
                self.w32(imp.stub + 4, irx::NOP);
                continue;
            }
            let code = match LIBRARIES.iter().position(|&l| l == imp.library) {
                Some(i) => (i as u32) << 8 | imp.ordinal,
                None => {
                    self.k.unknown.push(format!("{}:{}", imp.library, imp.ordinal));
                    UNKNOWN + self.k.unknown.len() as u32 - 1
                }
            };
            self.w32(imp.stub + 4, irx::syscall(code));
            hle += 1;
        }
        eprintln!("[iop] loaded {name} ({}) at {base:#x}, {} imports ({hle} native)", m.name, imports.len());

        // argv: the module path, then the NUL-separated argument strings.
        let mut strings = vec![name.as_bytes().to_vec()];
        strings.extend(args.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| s.to_vec()));
        let block = self.k.alloc(1024, true);
        let mut p = block + 4 * (strings.len() as u32 + 1);
        for (i, s) in strings.iter().enumerate() {
            self.w32(block + 4 * i as u32, p);
            self.mem.write_bytes(p, s);
            self.mem.write_bytes(p + s.len() as u32, &[0]);
            p += s.len() as u32 + 1;
        }
        self.w32(block + 4 * strings.len() as u32, 0);
        let r = self.call_guest(m.entry, &[strings.len() as u32, block], m.gp, LOADER_STACK_TOP);
        eprintln!("[iop] {name} start returned {r}");
    }

    /// Call guest code to completion: module entry points and handlers.
    pub(crate) fn call_guest(&mut self, func: u32, args: &[u32], gp: u32, sp: u32) -> u32 {
        let saved = std::mem::take(&mut self.cpu);
        let was_intr = self.k.in_intr;
        self.k.in_intr = true;
        self.cpu.cop0 = saved.cop0;
        for (i, &a) in args.iter().enumerate() {
            self.cpu.r[A0 + i] = a;
        }
        self.cpu.r[GP] = gp;
        self.cpu.r[SP] = sp;
        self.cpu.r[RA] = RETURN_MAGIC;
        self.cpu.jump(func);
        let mut n = 0u64;
        while self.cpu.pc != RETURN_MAGIC {
            match self.cpu.step(&mut self.mem) {
                None => {}
                Some(Event::Syscall(code)) => {
                    self.hle(code);
                }
                Some(Event::Fault(pc)) => {
                    eprintln!("[iop] fault at {pc:#x} in handler {func:#x}");
                    break;
                }
            }
            n += 1;
            if n > 200_000_000 {
                eprintln!("[iop] handler {func:#x} did not return");
                break;
            }
        }
        self.cycles += n;
        let r = self.cpu.r[V0];
        self.cpu = saved;
        self.k.in_intr = was_intr;
        r
    }

    // ---- scheduling ----

    fn pick(&self) -> Option<usize> {
        self.k.threads.iter().enumerate().filter(|(_, t)| t.state == State::Ready).min_by_key(|(_, t)| (t.prio, t.seq)).map(|(i, _)| i)
    }

    fn make_ready(&mut self, i: usize) {
        self.k.seq += 1;
        let t = &mut self.k.threads[i];
        t.state = State::Ready;
        t.seq = self.k.seq;
    }

    /// Stop running the current thread (it blocked or was preempted).
    fn switch_out(&mut self) {
        if let Some(c) = self.k.cur.take() {
            self.k.threads[c].regs = std::mem::take(&mut self.cpu);
        }
    }

    fn switch_in(&mut self, i: usize) {
        self.cpu = std::mem::take(&mut self.k.threads[i].regs);
        self.k.cur = Some(i);
    }

    /// Preempt the running thread if a higher-priority one is ready.
    fn preempt(&mut self) {
        if self.k.in_intr {
            return;
        }
        let (Some(c), Some(best)) = (self.k.cur, self.pick()) else { return };
        if best != c && self.k.threads[best].prio < self.k.threads[c].prio {
            self.make_ready(c);
            self.switch_out();
        }
    }

    fn block(&mut self, state: State) -> Flow {
        let Some(c) = self.k.cur.filter(|_| !self.k.in_intr) else {
            self.k.warn_once(&format!("blocking call ({state:?}) outside a thread; not waiting"));
            return Flow::Return(KE_CAN_NOT_WAIT);
        };
        self.k.threads[c].state = state;
        self.switch_out();
        Flow::Block
    }

    /// Wake a blocked thread with `v0` as its call's result.
    fn wake(&mut self, i: usize, v0: u32) {
        self.k.threads[i].regs.r[V0] = v0;
        self.make_ready(i);
    }

    fn deliverable(&self) -> u64 {
        if self.k.suspended || self.k.in_intr {
            0
        } else {
            self.mem.raised & self.k.intr_enabled
        }
    }

    /// Run threads until `until` or until all are blocked. Returns false
    /// if there was nothing to run.
    pub(crate) fn run_threads(&mut self, until: u64) -> bool {
        while self.cycles < until {
            if self.deliverable() != 0 {
                return true;
            }
            if self.cycles & 1023 == 0 && self.has_mail() {
                self.mail();
            }
            if self.k.cur.is_none() {
                match self.pick() {
                    Some(i) => self.switch_in(i),
                    None => return false,
                }
            }
            if self.cpu.pc == THREAD_EXIT {
                let c = self.k.cur.unwrap();
                self.k.threads[c].state = State::Exited;
                self.switch_out();
                continue;
            }
            self.cycles += 1;
            if self.k.trace && self.cycles.is_multiple_of(CLOCK) {
                eprintln!("[iop] t{} running at pc {:#x} (ra {:#x})", self.k.cur.unwrap() + 1, self.cpu.pc, self.cpu.r[RA]);
            }
            match self.cpu.step(&mut self.mem) {
                None => {}
                Some(Event::Syscall(code)) => {
                    self.hle(code);
                    if std::mem::take(&mut self.k.ee_pending) {
                        self.sync_with_ee();
                    }
                }
                Some(Event::Fault(pc)) => {
                    let c = self.k.cur.unwrap();
                    eprintln!("[iop] fault at {pc:#x} in thread {}; stopping it", c + 1);
                    self.k.threads[c].state = State::Exited;
                    self.switch_out();
                }
            }
        }
        true
    }

    pub(crate) fn wake_timers(&mut self) {
        for i in 0..self.k.threads.len() {
            if let State::Delayed(u) = self.k.threads[i].state {
                if u <= self.cycles {
                    self.wake(i, KE_OK);
                }
            }
        }
        self.preempt();
    }

    pub(crate) fn deliver_interrupts(&mut self) {
        if std::mem::take(&mut self.k.ee_pending) {
            self.sync_with_ee();
        }
        let mut pending = self.deliverable();
        while pending != 0 {
            let n = pending.trailing_zeros();
            pending &= !(1 << n);
            self.mem.raised &= !(1 << n);
            if let Some(h) = self.k.intr.get(&n).copied() {
                self.call_guest(h.func, &[h.arg], h.gp, INTR_STACK_TOP);
            }
        }
        self.preempt();
    }

    pub(crate) fn vblank(&mut self) {
        if self.k.suspended {
            return;
        }
        for edge in [0, 1] {
            let hs: Vec<(u32, Handler)> = self.k.vblank.iter().filter(|(e, _)| *e == edge).copied().collect();
            for (e, h) in hs {
                if self.call_guest(h.func, &[h.arg], h.gp, INTR_STACK_TOP) == 0 {
                    self.k.vblank.retain(|(e2, h2)| !(*e2 == e && h2.func == h.func));
                }
            }
        }
        self.preempt();
    }

    // ---- SIF ----

    pub(crate) fn deliver_cmd(&mut self, cid: u32, packet: &[u8]) {
        if self.k.trace {
            eprintln!("[iop] EE->IOP cmd {cid:#x} ({} bytes): {:02x?}", packet.len(), &packet[..packet.len().min(48)]);
        }
        if cid & 0x8000_0000 != 0 {
            if cid == 0x8000_0001 && packet.len() >= 24 {
                let reg = u32::from_le_bytes(packet[16..20].try_into().unwrap());
                let val = u32::from_le_bytes(packet[20..24].try_into().unwrap());
                self.k.sreg[reg as usize & 31] = val;
            }
            return;
        }
        let Some(h) = self.k.cmd.get(&cid).copied() else {
            self.k.warn_once(&format!("SIF command {cid:#x} from the EE has no handler"));
            return;
        };
        let slot = CMD_BUFS + (self.k.cmd_slot % CMD_SLOTS) * CMD_SLOT;
        self.k.cmd_slot += 1;
        let len = packet.len().min(CMD_SLOT as usize);
        self.mem.write_bytes(slot, &packet[..len]);
        self.call_guest(h.func, &[slot, h.arg], h.gp, INTR_STACK_TOP);
        self.preempt();
    }

    fn send_cmd(&mut self) -> u32 {
        let [cid, pkt, psize, src] = [0, 1, 2, 3].map(|i| self.cpu.arg(i));
        let sp = self.cpu.r[SP];
        let (dest, size) = (self.r32(sp + 16), self.r32(sp + 20));
        let psize = psize.min(CMD_SLOT);
        // Header: psize:8 dsize:24, dest, cid (opt is the sender's).
        self.w32(pkt, psize | size << 8);
        self.w32(pkt + 4, dest);
        self.w32(pkt + 8, cid);
        let packet = self.mem.read_bytes(pkt, psize as usize);
        let extra = (size > 0).then(|| (dest, self.mem.read_bytes(src, size as usize)));
        if self.k.trace {
            eprintln!("[iop] IOP->EE cmd {cid:#x} ({psize} bytes, {size} extra to {dest:#x}): {:02x?}", &packet[..packet.len().min(48)]);
        }
        self.link.post(ToEe::Cmd { cid, packet, extra });
        self.k.ee_pending = true;
        1
    }

    // ---- the HLE dispatcher ----

    pub(crate) fn hle(&mut self, code: u32) {
        let flow = self.call(code);
        match flow {
            Flow::Return(v) => {
                self.cpu.r[V0] = v;
                self.preempt();
            }
            Flow::Block | Flow::Jumped => {}
        }
    }

    fn arg(&self, n: usize) -> u32 {
        if n < 4 {
            self.cpu.arg(n)
        } else {
            self.r32(self.cpu.r[SP] + 4 * n as u32)
        }
    }

    fn call(&mut self, code: u32) -> Flow {
        use Flow::Return;
        let a = |m: &Machine, n: usize| m.arg(n);
        if code >= UNKNOWN {
            let name = self.k.unknown.get((code - UNKNOWN) as usize).cloned().unwrap_or_default();
            self.k.warn_once(&format!("unimplemented import {name}; returning 0"));
            return Return(0);
        }
        let (lib, ord) = (LIBRARIES.get((code >> 8) as usize).copied().unwrap_or("?"), code & 0xff);
        if self.k.trace_calls && !(lib == "sysclib" || lib == "stdio") {
            eprintln!("[iop] t{} {lib}:{ord}({:#x}, {:#x}, {:#x}, {:#x}) ra={:#x}{}", self.k.cur.map_or(0, |c| c + 1), self.cpu.arg(0), self.cpu.arg(1), self.cpu.arg(2), self.cpu.arg(3), self.cpu.r[RA], if self.k.in_intr { " [intr]" } else { "" });
        }
        match (lib, ord) {
            // sysmem
            ("sysmem", 4) => {
                let (mode, size, addr) = (a(self, 0), a(self, 1), a(self, 2));
                let p = if mode == 2 { addr } else { self.k.alloc(size, mode == 1) };
                if p == 0 {
                    self.k.warn_once(&format!("AllocSysMemory({size:#x}) failed"));
                }
                Return(p)
            }
            ("sysmem", 5) => {
                let p = a(self, 0);
                Return(if self.k.free(p) { KE_OK } else { KE_ERROR })
            }
            ("sysmem", 7) => Return(self.k.heap.iter().filter(|h| !h.2).map(|h| h.1).max().unwrap_or(0)),
            ("sysmem", 8) => Return(self.k.heap.iter().filter(|h| !h.2).map(|h| h.1).sum()),

            // intrman
            ("intrman", 4) => {
                let (n, _mode, func, arg) = (a(self, 0), a(self, 1), a(self, 2), a(self, 3));
                self.k.intr.insert(n, Handler { func, arg, gp: self.cpu.r[GP] });
                Return(KE_OK)
            }
            ("intrman", 5) => {
                self.k.intr.remove(&a(self, 0));
                Return(KE_OK)
            }
            ("intrman", 6) => {
                self.k.intr_enabled |= 1 << (a(self, 0) & 63);
                Return(KE_OK)
            }
            ("intrman", 7) => {
                let (n, res) = (a(self, 0), a(self, 1));
                self.k.intr_enabled &= !(1 << (n & 63));
                if res != 0 {
                    self.w32(res, n);
                }
                Return(KE_OK)
            }
            ("intrman", 8) => {
                self.k.suspended = true;
                Return(KE_OK)
            }
            ("intrman", 9) => {
                self.k.suspended = false;
                Return(KE_OK)
            }
            ("intrman", 17) => {
                let state = a(self, 0);
                let was = self.k.suspended;
                if state != 0 {
                    self.w32(state, if was { 0 } else { 1 });
                }
                self.k.suspended = true;
                Return(if was { KE_CPUDI } else { KE_OK })
            }
            ("intrman", 18) => {
                self.k.suspended = a(self, 0) == 0;
                Return(KE_OK)
            }
            ("intrman", 23) => Return(self.k.in_intr as u32),

            // loadcore
            ("loadcore", 4..=7) => Return(KE_OK),

            // sifcmd
            ("sifcmd", 4 | 5 | 14) => Return(KE_OK),
            ("sifcmd", 6) => Return(self.k.sreg[a(self, 0) as usize & 31]),
            ("sifcmd", 7) => {
                let (r, v) = (a(self, 0) as usize & 31, a(self, 1));
                let old = std::mem::replace(&mut self.k.sreg[r], v);
                Return(old)
            }
            // Command tables are kept natively; the buffer is unused.
            ("sifcmd", 8 | 9) => Return(0),
            ("sifcmd", 10) => {
                let (cid, func, arg) = (a(self, 0), a(self, 1), a(self, 2));
                self.k.cmd.insert(cid & 0x7fff_ffff, Handler { func, arg, gp: self.cpu.r[GP] });
                Return(KE_OK)
            }
            ("sifcmd", 11) => {
                self.k.cmd.remove(&(a(self, 0) & 0x7fff_ffff));
                Return(KE_OK)
            }
            ("sifcmd", 12 | 13) => Return(self.send_cmd()),

            // sifman
            ("sifman", 4 | 5) => Return(KE_OK),
            ("sifman", 7) => {
                let (list, count) = (a(self, 0), a(self, 1));
                for i in 0..count {
                    let t = list + i * 16;
                    let (src, dest, size) = (self.r32(t), self.r32(t + 4), self.r32(t + 8));
                    let data = self.mem.read_bytes(src, size as usize);
                    self.link.post(ToEe::Dma { dest, data });
                }
                self.k.dma_id = self.k.dma_id.wrapping_add(1).max(1);
                Return(self.k.dma_id)
            }
            // Transfers complete immediately.
            ("sifman", 8) => Return(-1i32 as u32),

            // stdio
            ("stdio", 4) => {
                let s = self.printf(0);
                self.k.line.push_str(&s);
                while let Some(nl) = self.k.line.find('\n') {
                    let l: String = self.k.line.drain(..=nl).collect();
                    eprint!("[iop] {l}");
                }
                Return(s.len() as u32)
            }

            // sysclib
            ("sysclib", 4) => {
                // setjmp: ra, sp, fp, s0-s7, gp.
                let b = a(self, 0);
                let regs = [RA, SP, 30, 16, 17, 18, 19, 20, 21, 22, 23, GP];
                for (i, &r) in regs.iter().enumerate() {
                    let v = self.cpu.r[r];
                    self.w32(b + 4 * i as u32, v);
                }
                Return(0)
            }
            ("sysclib", 5) => {
                let (b, v) = (a(self, 0), a(self, 1));
                let regs = [RA, SP, 30, 16, 17, 18, 19, 20, 21, 22, 23, GP];
                for (i, &r) in regs.iter().enumerate() {
                    self.cpu.r[r] = self.r32(b + 4 * i as u32);
                }
                self.cpu.r[V0] = if v == 0 { 1 } else { v };
                let ra = self.cpu.r[RA];
                self.cpu.jump(ra);
                Flow::Jumped
            }
            ("sysclib", 6) => Return((a(self, 0) as u8).to_ascii_uppercase() as u32),
            ("sysclib", 7) => Return((a(self, 0) as u8).to_ascii_lowercase() as u32),
            ("sysclib", 11 | 15) => {
                let (p, q, n) = (a(self, 0), a(self, 1), a(self, 2) as usize);
                let (x, y) = (self.mem.read_bytes(p, n), self.mem.read_bytes(q, n));
                Return(x.iter().zip(&y).find(|(a, b)| a != b).map_or(0, |(a, b)| (*a as i32 - *b as i32) as u32))
            }
            ("sysclib", 12 | 13) => {
                let (d, s, n) = (a(self, 0), a(self, 1), a(self, 2) as usize);
                let data = self.mem.read_bytes(s, n);
                self.mem.write_bytes(d, &data);
                Return(d)
            }
            ("sysclib", 14) => {
                let (d, c, n) = (a(self, 0), a(self, 1) as u8, a(self, 2) as usize);
                self.mem.write_bytes(d, &vec![c; n]);
                Return(d)
            }
            ("sysclib", 16) => {
                let (s, d, n) = (a(self, 0), a(self, 1), a(self, 2) as usize);
                let data = self.mem.read_bytes(s, n);
                self.mem.write_bytes(d, &data);
                Return(0)
            }
            ("sysclib", 17) => {
                let (d, n) = (a(self, 0), a(self, 1) as usize);
                self.mem.write_bytes(d, &vec![0; n]);
                Return(0)
            }
            ("sysclib", 28) => Return(self.mem.cstr(a(self, 0)).len() as u32),

            // thbase
            ("thbase", 4) => {
                let p = a(self, 0);
                let (func, stack_size, prio) = (self.r32(p + 8), self.r32(p + 12), self.r32(p + 16));
                let stack = self.k.alloc(stack_size, true);
                if stack == 0 {
                    return Return(KE_ERROR);
                }
                let gp = self.cpu.r[GP];
                self.k.threads.push(Thread { regs: Regs::default(), prio, state: State::Dormant, wakeups: 0, func, stack: (stack, stack_size), gp, seq: 0 });
                Return(self.k.threads.len() as u32)
            }
            ("thbase", 5) => Return(KE_OK),
            ("thbase", 6 | 7) => {
                let id = a(self, 0);
                let arg = if ord == 6 { a(self, 1) } else { a(self, 2) };
                let Some(t) = self.k.threads.get_mut((id as usize).wrapping_sub(1)) else { return Return(KE_UNKNOWN_THID) };
                let mut r = Regs::default();
                r.r[A0] = arg;
                r.r[SP] = (t.stack.0 + t.stack.1 - 16) & !15;
                r.r[GP] = t.gp;
                r.r[RA] = THREAD_EXIT;
                r.jump(t.func);
                t.regs = r;
                self.make_ready(id as usize - 1);
                Return(KE_OK)
            }
            ("thbase", 8 | 9) => {
                let Some(c) = self.k.cur.filter(|_| !self.k.in_intr) else { return Return(KE_CAN_NOT_WAIT) };
                self.k.threads[c].state = State::Exited;
                self.switch_out();
                Flow::Block
            }
            ("thbase", 14 | 15) => {
                let (id, prio) = (a(self, 0), a(self, 1));
                let i = if id == 0 { self.k.cur.unwrap_or(0) } else { id as usize - 1 };
                if let Some(t) = self.k.threads.get_mut(i) {
                    t.prio = prio;
                }
                Return(KE_OK)
            }
            ("thbase", 20) => Return(self.k.cur.map_or(0, |c| c as u32 + 1)),
            ("thbase", 24) => {
                let Some(c) = self.k.cur else { return Return(KE_CAN_NOT_WAIT) };
                if self.k.threads[c].wakeups > 0 {
                    self.k.threads[c].wakeups -= 1;
                    return Return(KE_OK);
                }
                self.block(State::Sleeping)
            }
            ("thbase", 25 | 26) => {
                let i = (a(self, 0) as usize).wrapping_sub(1);
                let Some(t) = self.k.threads.get_mut(i) else { return Return(KE_UNKNOWN_THID) };
                if t.state == State::Sleeping {
                    self.wake(i, KE_OK);
                } else {
                    t.wakeups += 1;
                }
                Return(KE_OK)
            }
            ("thbase", 27 | 28) => {
                let i = (a(self, 0) as usize).wrapping_sub(1);
                let i = if a(self, 0) == 0 { self.k.cur.unwrap_or(0) } else { i };
                let n = self.k.threads.get_mut(i).map_or(0, |t| std::mem::take(&mut t.wakeups));
                Return(n)
            }
            ("thbase", 33) => {
                let us = a(self, 0) as u64;
                if self.k.cur.is_none() || self.k.in_intr {
                    return Return(KE_CAN_NOT_WAIT);
                }
                let until = self.cycles + us * CLOCK / 1_000_000;
                self.block(State::Delayed(until))
            }
            ("thbase", 34) => {
                let p = a(self, 0);
                let c = self.cycles;
                self.w32(p, c as u32);
                self.w32(p + 4, (c >> 32) as u32);
                Return(KE_OK)
            }
            ("thbase", 39) => {
                let (us, p) = (a(self, 0) as u64, a(self, 1));
                let c = us * CLOCK / 1_000_000;
                self.w32(p, c as u32);
                self.w32(p + 4, (c >> 32) as u32);
                Return(KE_OK)
            }
            ("thbase", 40) => {
                let (p, sec, usec) = (a(self, 0), a(self, 1), a(self, 2));
                let c = self.r32(p) as u64 | (self.r32(p + 4) as u64) << 32;
                let us = c * 1_000_000 / CLOCK;
                if sec != 0 {
                    self.w32(sec, (us / 1_000_000) as u32);
                }
                if usec != 0 {
                    self.w32(usec, (us % 1_000_000) as u32);
                }
                Return(KE_OK)
            }

            // thevent
            ("thevent", 4) => {
                let p = a(self, 0);
                let init = self.r32(p + 8);
                self.k.evf.push(init);
                Return(self.k.evf.len() as u32)
            }
            ("thevent", 5) => Return(KE_OK),
            ("thevent", 6 | 7) => {
                let (id, bits) = (a(self, 0) as usize, a(self, 1));
                let Some(f) = self.k.evf.get_mut(id.wrapping_sub(1)) else { return Return(KE_ERROR) };
                *f |= bits;
                self.check_evf(id - 1);
                Return(KE_OK)
            }
            ("thevent", 8 | 9) => {
                let (id, bits) = (a(self, 0) as usize, a(self, 1));
                if let Some(f) = self.k.evf.get_mut(id.wrapping_sub(1)) {
                    *f &= bits;
                }
                Return(KE_OK)
            }
            ("thevent", 10 | 11) => {
                let (id, bits, mode, res) = (a(self, 0) as usize, a(self, 1), a(self, 2), a(self, 3));
                let Some(&f) = self.k.evf.get(id.wrapping_sub(1)) else { return Return(KE_ERROR) };
                if evf_match(f, bits, mode) {
                    if res != 0 {
                        self.w32(res, f);
                    }
                    if mode & 0x10 != 0 {
                        self.k.evf[id - 1] &= !bits;
                    }
                    return Return(KE_OK);
                }
                if ord == 11 {
                    return Return(-421i32 as u32); // KE_EVF_COND
                }
                if self.k.cur.is_none() || self.k.in_intr {
                    return Return(KE_CAN_NOT_WAIT);
                }
                self.block(State::WaitEvf { id: id - 1, bits, mode, res })
            }

            // thsemap
            ("thsemap", 4) => {
                let p = a(self, 0);
                let (init, max) = (self.r32(p + 8) as i32, self.r32(p + 12) as i32);
                self.k.sema.push((init, max));
                Return(self.k.sema.len() as u32)
            }
            ("thsemap", 5) => Return(KE_OK),
            ("thsemap", 6 | 7) => {
                let id = (a(self, 0) as usize).wrapping_sub(1);
                if id >= self.k.sema.len() {
                    return Return(KE_ERROR);
                }
                if let Some(i) = self.k.threads.iter().position(|t| t.state == State::WaitSema(id)) {
                    self.wake(i, KE_OK);
                } else {
                    self.k.sema[id].0 += 1;
                }
                Return(KE_OK)
            }
            ("thsemap", 8 | 9) => {
                let id = (a(self, 0) as usize).wrapping_sub(1);
                if id >= self.k.sema.len() {
                    return Return(KE_ERROR);
                }
                if self.k.sema[id].0 > 0 {
                    self.k.sema[id].0 -= 1;
                    return Return(KE_OK);
                }
                if ord == 9 {
                    return Return(-419i32 as u32); // KE_SEMA_ZERO
                }
                self.block(State::WaitSema(id))
            }

            // vblank
            ("vblank", 8) => {
                let (edge, _prio, func, arg) = (a(self, 0), a(self, 1), a(self, 2), a(self, 3));
                self.k.vblank.push((edge, Handler { func, arg, gp: self.cpu.r[GP] }));
                Return(KE_OK)
            }
            ("vblank", 9) => {
                let (edge, func) = (a(self, 0), a(self, 1));
                self.k.vblank.retain(|(e, h)| !(*e == edge && h.func == func));
                Return(KE_OK)
            }

            _ => {
                self.k.warn_once(&format!("unimplemented import {lib}:{ord}; returning 0"));
                Return(0)
            }
        }
    }

    fn check_evf(&mut self, id: usize) {
        for i in 0..self.k.threads.len() {
            if let State::WaitEvf { id: w, bits, mode, res } = self.k.threads[i].state {
                let f = self.k.evf[id];
                if w == id && evf_match(f, bits, mode) {
                    if res != 0 {
                        self.w32(res, f);
                    }
                    if mode & 0x10 != 0 {
                        self.k.evf[id] &= !bits;
                    }
                    self.wake(i, KE_OK);
                }
            }
        }
    }

    /// Format a printf call whose format string is argument `first`.
    fn printf(&self, first: usize) -> String {
        let fmt = self.mem.cstr(self.arg(first));
        let mut n = first + 1;
        let mut out = String::new();
        let mut it = fmt.chars().peekable();
        while let Some(ch) = it.next() {
            if ch != '%' {
                out.push(ch);
                continue;
            }
            let mut spec = String::new();
            while let Some(&c) = it.peek() {
                if c.is_ascii_digit() || matches!(c, '-' | '.' | 'l' | 'h' | '#' | ' ' | '+') {
                    spec.push(c);
                    it.next();
                } else {
                    break;
                }
            }
            let Some(conv) = it.next() else { break };
            let zero = spec.starts_with('0');
            let width: usize = spec.trim_start_matches(['-', '0']).split('.').next().unwrap_or("").trim_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(0);
            let s = match conv {
                '%' => "%".to_string(),
                'd' | 'i' => {
                    n += 1;
                    (self.arg(n - 1) as i32).to_string()
                }
                'u' => {
                    n += 1;
                    self.arg(n - 1).to_string()
                }
                'x' | 'p' => {
                    n += 1;
                    format!("{:x}", self.arg(n - 1))
                }
                'X' => {
                    n += 1;
                    format!("{:X}", self.arg(n - 1))
                }
                'c' => {
                    n += 1;
                    ((self.arg(n - 1) as u8) as char).to_string()
                }
                's' => {
                    n += 1;
                    self.mem.cstr(self.arg(n - 1))
                }
                c => format!("%{spec}{c}"),
            };
            let pad = width.saturating_sub(s.len());
            if spec.starts_with('-') {
                out.push_str(&s);
                out.push_str(&" ".repeat(pad));
            } else {
                out.push_str(&(if zero { "0" } else { " " }).repeat(pad));
                out.push_str(&s);
            }
        }
        out
    }
}

/// WaitEventFlag condition: mode bit 0 = OR, else AND.
fn evf_match(flags: u32, bits: u32, mode: u32) -> bool {
    if mode & 1 != 0 {
        flags & bits != 0
    } else {
        flags & bits == bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heap_alloc_and_merge() {
        let mut k = Kernel::new();
        let a = k.alloc(0x1000, false);
        let b = k.alloc(0x1000, false);
        let h = k.alloc(0x100, true);
        assert_eq!(a, HEAP.0);
        assert_eq!(b, HEAP.0 + 0x1000);
        assert_eq!(h, HEAP.1 - 0x100);
        assert!(k.free(a) && k.free(b) && k.free(h));
        assert_eq!(k.heap, vec![(HEAP.0, HEAP.1 - HEAP.0, false)]);
    }

    #[test]
    fn evf_modes() {
        assert!(evf_match(0b110, 0b100, 1));
        assert!(!evf_match(0b010, 0b101, 1));
        assert!(evf_match(0b111, 0b101, 0));
        assert!(!evf_match(0b100, 0b101, 0));
    }
}
