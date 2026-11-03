//! VU1 and its VIF on a thread of their own, as on the real console where
//! VU1 runs in parallel with the EE.
//!
//! VIF1 transfers are queued in order and run by a worker thread; the EE
//! carries on. Anything the EE could use to see VU1's work waits for the
//! queue to drain first (`Vu1::get`, `Handle::wait`): VU1 memory, and the
//! GS (privileged registers, PATH3 packets, vsync), so the GS receives the
//! same stream in the same order as when everything ran on one thread.
//! FREEK_MTVU=0 runs VIF1 on the EE thread instead.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

use crate::dma::{Target, Vif, VifIo};
use crate::vu::VuRegs;
use crate::vumicro::{DecodeCache, Programs};

/// Microseconds the EE thread spent waiting for VU1, for FREEK_HEARTBEAT.
pub static WAIT_US: AtomicU64 = AtomicU64::new(0);

/// VU1's registers and memory, and the VIF that feeds it.
pub struct Unit {
    pub vif: Vif,
    pub regs: VuRegs,
    pub code: Box<[u8]>,
    pub data: Box<[u8]>,
    /// Bumped whenever program memory changes.
    pub code_gen: u64,
    pub cache: DecodeCache,
    pub programs: Programs,
}

impl Unit {
    pub fn new() -> Unit {
        Unit {
            vif: Vif::default(),
            regs: VuRegs::new(),
            code: vec![0; 16 << 10].into_boxed_slice(),
            data: vec![0; 16 << 10].into_boxed_slice(),
            code_gen: 1,
            cache: DecodeCache::default(),
            programs: Programs(&[]),
        }
    }

    /// Run VIF1 words through the command parser.
    pub fn run(&mut self, words: &[u32], io: &mut dyn VifIo) {
        let Unit { vif, regs, code, data, code_gen, cache, programs } = self;
        let mut t = Target { unit: 1, code, data, code_gen, vu1: Some((regs, cache, programs)) };
        vif.run(&mut t, io, words);
    }
}

impl Default for Unit {
    fn default() -> Unit {
        Unit::new()
    }
}

struct Shared {
    unit: UnsafeCell<Unit>,
    /// Transfers queued or running.
    pending: AtomicUsize,
    /// VU1 output wrote GS FINISH (see `io::writes_finish`).
    finish: AtomicBool,
}

// SAFETY: `unit` is used by the worker only while it runs a queued transfer
// (pending > 0) and by the EE thread only after `wait` saw pending == 0
// and before it queues the next one, so the two never overlap. The
// Acquire/Release pair on `pending` orders their accesses.
unsafe impl Sync for Shared {}

impl Shared {
    fn wait(&self) {
        if self.pending.load(Acquire) == 0 {
            return;
        }
        let t = Instant::now();
        let mut spins = 0u32;
        while self.pending.load(Acquire) != 0 {
            spins += 1;
            if spins < 2000 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        WAIT_US.fetch_add(t.elapsed().as_micros() as u64, Relaxed);
    }
}

/// The EE side's way to wait for VU1 without owning it (for `Io`).
#[derive(Clone)]
pub struct Handle(Arc<Shared>);

impl Handle {
    /// Wait until VU1 has finished everything queued. Returns whether its
    /// output wrote GS FINISH since the last call.
    pub fn wait(&self) -> bool {
        self.0.wait();
        self.0.finish.swap(false, Relaxed)
    }
}

/// VU1, owned by the EE side's `Memory`.
pub struct Vu1 {
    shared: Arc<Shared>,
    queue: Option<Sender<Vec<u32>>>,
    threaded: bool,
}

impl Vu1 {
    pub fn new() -> Vu1 {
        let threaded = std::env::var("FREEK_MTVU").map_or(true, |v| v != "0");
        let shared = Arc::new(Shared { unit: UnsafeCell::new(Unit::new()), pending: AtomicUsize::new(0), finish: AtomicBool::new(false) });
        Vu1 { shared, queue: None, threaded }
    }

    pub fn handle(&self) -> Handle {
        Handle(self.shared.clone())
    }

    /// VU1's state, once everything queued has run.
    pub fn get(&mut self) -> &mut Unit {
        self.shared.wait();
        // SAFETY: see `Shared`; nothing is queued and `&mut self` keeps
        // `submit` from running while this borrow lives.
        unsafe { &mut *self.shared.unit.get() }
    }

    /// Run a VIF1 transfer: queued for the worker, or right here with
    /// FREEK_MTVU=0. `io` gets the GIF output when run here; `gs` is the
    /// channel the worker sends it to.
    pub fn submit(&mut self, words: Vec<u32>, io: &mut dyn VifIo, gs: Option<&Sender<gs::Msg>>) {
        if !self.threaded {
            self.get().run(&words, io);
            return;
        }
        let queue = self.queue.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            let shared = self.shared.clone();
            let gs = gs.cloned();
            std::thread::Builder::new().name("vu1".into()).spawn(move || worker(shared, rx, gs)).expect("vu1 thread");
            tx
        });
        self.shared.pending.fetch_add(1, AcqRel);
        if queue.send(words).is_err() {
            self.shared.pending.fetch_sub(1, AcqRel);
        }
    }
}

impl Default for Vu1 {
    fn default() -> Vu1 {
        Vu1::new()
    }
}

/// GIF output from the worker: straight to the GS.
struct WorkerIo {
    gs: Option<Sender<gs::Msg>>,
    shared: Arc<Shared>,
    warned: std::collections::HashSet<String>,
}

impl VifIo for WorkerIo {
    fn send_gif(&mut self, data: Vec<u128>) {
        if crate::io::writes_finish(&data) {
            self.shared.finish.store(true, Relaxed);
        }
        if let Some(tx) = &self.gs {
            let _ = tx.send(gs::Msg::Gif(data));
        }
    }

    fn warn_once(&mut self, msg: &str) {
        if self.warned.insert(msg.to_string()) {
            eprintln!("[warn] {msg}");
        }
    }
}

fn worker(shared: Arc<Shared>, rx: Receiver<Vec<u32>>, gs: Option<Sender<gs::Msg>>) {
    let mut io = WorkerIo { gs, shared: shared.clone(), warned: Default::default() };
    let mut idle = 0u32;
    loop {
        let words = match rx.try_recv() {
            Ok(w) => w,
            // Transfers come in bursts: spin a little before sleeping.
            Err(TryRecvError::Empty) if idle < 20_000 => {
                idle += 1;
                std::hint::spin_loop();
                continue;
            }
            Err(TryRecvError::Empty) => match rx.recv() {
                Ok(w) => w,
                Err(_) => return,
            },
            Err(TryRecvError::Disconnected) => return,
        };
        idle = 0;
        // SAFETY: see `Shared`: this transfer is counted in `pending`, so
        // the EE thread stays away from the unit until it is done.
        let unit = unsafe { &mut *shared.unit.get() };
        unit.run(&words, &mut io);
        shared.pending.fetch_sub(1, Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::Io;

    /// VIF1 words for a DIRECT transfer of `q` (whole qwords).
    fn direct(q: &[u128]) -> Vec<u32> {
        let mut w = vec![0, 0, 0, 0x5000_0000 | q.len() as u32];
        for v in q {
            w.extend((0..4).map(|n| (*v >> (32 * n)) as u32));
        }
        w
    }

    /// A GIF packet writing FINISH through A+D.
    fn finish_packet() -> Vec<u128> {
        let tag = 1 | 1 << 15 | 1u128 << 60 | 0xeu128 << 64;
        vec![tag, 0x61u128 << 64]
    }

    fn threaded() -> (Vu1, Io, Receiver<gs::Msg>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut vu1 = Vu1::new();
        vu1.threaded = true;
        let mut io = Io::new();
        io.gs = Some(tx);
        io.vu1 = Some(vu1.handle());
        (vu1, io, rx)
    }

    /// The GS gets VU1's output and the EE's own packets in program order.
    #[test]
    fn gs_order_is_kept() {
        let (mut vu1, mut io, rx) = threaded();
        let gs = io.gs.clone();
        for i in 0..500u128 {
            vu1.submit(direct(&[i]), &mut io, gs.as_ref());
            io.send_gif(vec![1000 + i]);
        }
        io.sync_vu1();
        let got: Vec<u128> = rx.try_iter().filter_map(|m| if let gs::Msg::Gif(d) = m { Some(d[0]) } else { None }).collect();
        let want: Vec<u128> = (0..500).flat_map(|i| [i, 1000 + i]).collect();
        assert_eq!(got, want);
    }

    /// A FINISH written by VU1's output is visible in GS_CSR once read.
    #[test]
    fn finish_reaches_csr() {
        let (mut vu1, mut io, _rx) = threaded();
        let gs = io.gs.clone();
        vu1.submit(direct(&finish_packet()), &mut io, gs.as_ref());
        assert_ne!(io.read(0x1200_1000, 8) & 2, 0);
    }

    /// VU1 memory read by the EE reflects every queued transfer.
    #[test]
    fn memory_waits_for_queue() {
        let (mut vu1, mut io, _rx) = threaded();
        let gs = io.gs.clone();
        for n in 0..1000u32 {
            // STCYCL 1,1; UNPACK V1-32 one element to qword 7.
            vu1.submit(vec![0x0100_0101, 0x6001_0007, n], &mut io, gs.as_ref());
        }
        let d = &vu1.get().data;
        assert_eq!(u32::from_le_bytes(d[7 * 16..7 * 16 + 4].try_into().unwrap()), 999);
    }
}
