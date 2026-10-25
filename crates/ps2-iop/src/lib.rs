//! The PS2's I/O processor, as far as Freekstyle's sound needs it.
//!
//! The game's sound driver is EA's AUDIO.IRX, built on Sony's LIBSD.IRX.
//! Both modules run unchanged on an R3000 interpreter (`cpu`). The IOP
//! kernel libraries they import (threads, interrupts, SIF commands, memory)
//! are implemented natively (`kernel`), and the sound hardware is the
//! `spu2` model. The machine runs on its own host thread, paced by the
//! audio output, and talks to the EE through a `Link`.

pub mod cpu;
pub mod irx;
mod kernel;
pub mod spu2;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpu::{Bus, Regs};

/// IOP clock: 36.864 MHz, 768 cycles per 48 kHz sample.
pub const CLOCK: u64 = 36_864_000;
const CYCLES_PER_SAMPLE: u64 = CLOCK / spu2::RATE as u64;

pub const RAM_SIZE: usize = 2 << 20;

/// Messages from the EE.
pub enum ToIop {
    /// Load and start a module (file contents, argument block).
    Load { name: String, image: Vec<u8>, args: Vec<u8> },
    /// A SIF command; `extra` is data to place in IOP memory first.
    Cmd { cid: u32, packet: Vec<u8>, extra: Option<(u32, Vec<u8>)> },
    /// SIF DMA into IOP memory.
    Dma { dest: u32, data: Vec<u8> },
}

/// Messages to the EE.
#[derive(Debug)]
pub enum ToEe {
    Cmd { cid: u32, packet: Vec<u8>, extra: Option<(u32, Vec<u8>)> },
    Dma { dest: u32, data: Vec<u8> },
}

/// The SIF between EE and IOP: two mailboxes.
#[derive(Default)]
pub struct Link {
    to_iop: Mutex<VecDeque<ToIop>>,
    to_ee: Mutex<VecDeque<ToEe>>,
    ee_mail: AtomicBool,
    /// Messages posted to the EE, and how many it has fully handled.
    posted: AtomicU64,
    handled: AtomicU64,
    /// Modules loaded and started.
    loaded: AtomicU64,
    iop_mail: AtomicBool,
}

impl Link {
    pub fn send(&self, m: ToIop) {
        self.to_iop.lock().unwrap().push_back(m);
        self.iop_mail.store(true, Ordering::Release);
    }

    /// Cheap check for messages to the EE.
    pub fn has_mail(&self) -> bool {
        self.ee_mail.load(Ordering::Acquire)
    }

    /// Take the messages for the EE. Call `handled` with the result's
    /// count once their handlers have run (and sent any replies).
    pub fn take(&self) -> Vec<ToEe> {
        let mut q = self.to_ee.lock().unwrap();
        self.ee_mail.store(false, Ordering::Release);
        q.drain(..).collect()
    }

    /// Modules loaded so far; LoadModule returns once its module started.
    pub fn loaded(&self) -> u64 {
        self.loaded.load(Ordering::Acquire)
    }

    pub fn handled(&self, n: usize) {
        self.handled.fetch_add(n as u64, Ordering::AcqRel);
    }

    fn post(&self, m: ToEe) {
        let mut q = self.to_ee.lock().unwrap();
        q.push_back(m);
        self.posted.fetch_add(1, Ordering::AcqRel);
        self.ee_mail.store(true, Ordering::Release);
    }

    /// Wait (briefly) until the EE has handled everything posted. On the
    /// real machine the EE answers SIF commands within microseconds, and
    /// drivers rely on that.
    fn sync(&self) {
        let start = std::time::Instant::now();
        while self.handled.load(Ordering::Acquire) < self.posted.load(Ordering::Acquire) {
            if start.elapsed() > std::time::Duration::from_millis(5) {
                return;
            }
            std::thread::yield_now();
        }
    }
}

/// Stereo samples from the SPU2 to the audio device.
#[derive(Default)]
pub struct AudioRing {
    pub buf: Mutex<VecDeque<[i16; 2]>>,
}

impl AudioRing {
    pub fn len(&self) -> usize {
        self.buf.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct Mem {
    pub ram: Vec<u8>,
    scratch: Vec<u8>,
    pub spu: spu2::Spu2,
    /// DMA channel registers (MADR, BCR, CHCR, TADR) for channels 0-13.
    dma: [[u32; 4]; 14],
    dma_ctrl: [u32; 4],
    /// Raised interrupt lines (IOP interrupt numbers, DMA channels at 32+).
    pub raised: u64,
}

fn dma_channel(a: u32) -> Option<(usize, usize)> {
    match a {
        0x1f80_1080..=0x1f80_10ef => Some((((a - 0x1f80_1080) >> 4) as usize, ((a >> 2) & 3) as usize)),
        0x1f80_1500..=0x1f80_155f => Some((7 + ((a - 0x1f80_1500) >> 4) as usize, ((a >> 2) & 3) as usize)),
        _ => None,
    }
}

impl Mem {
    fn new() -> Mem {
        Mem { ram: vec![0; RAM_SIZE], scratch: vec![0; 1024], spu: spu2::Spu2::new(), dma: [[0; 4]; 14], dma_ctrl: [0; 4], raised: 0 }
    }

    fn io_read(&mut self, a: u32) -> u32 {
        if let Some((ch, r)) = dma_channel(a) {
            return self.dma[ch][r];
        }
        match a {
            0x1f80_10f0 => self.dma_ctrl[0],
            0x1f80_10f4 => self.dma_ctrl[1],
            0x1f80_1570 => self.dma_ctrl[2],
            0x1f80_1574 => self.dma_ctrl[3],
            _ => 0,
        }
    }

    fn io_write(&mut self, a: u32, v: u32) {
        if let Some((ch, r)) = dma_channel(a) {
            self.dma[ch][r] = v;
            if r == 2 && v & (1 << 24) != 0 {
                self.run_dma(ch);
            }
            return;
        }
        match a {
            0x1f80_10f0 => self.dma_ctrl[0] = v,
            0x1f80_10f4 => self.dma_ctrl[1] = v,
            0x1f80_1570 => self.dma_ctrl[2] = v,
            0x1f80_1574 => self.dma_ctrl[3] = v,
            _ => {}
        }
    }

    /// A narrow write into a 32-bit register.
    fn io_write_part(&mut self, p: u32, v: u32, mask: u32) {
        let sh = (p & 3) * 8;
        let old = self.io_read(p & !3);
        self.io_write(p & !3, (old & !(mask << sh)) | (v & mask) << sh);
    }

    /// Transfers complete at once; the channel's interrupt follows.
    fn run_dma(&mut self, ch: usize) {
        let [madr, bcr, chcr, _] = self.dma[ch];
        let (bs, ba) = (bcr & 0xffff, (bcr >> 16).max(1));
        let len = (bs * ba * 4) as usize;
        let at = madr as usize & (RAM_SIZE - 1);
        match ch {
            4 | 7 => {
                let core = (ch == 7) as usize;
                let end = (at + len).min(RAM_SIZE);
                if chcr & 1 != 0 {
                    let data = self.ram[at..end].to_vec();
                    self.spu.dma_write(core, &data);
                } else {
                    let data = self.spu.dma_read(core, end - at);
                    self.ram[at..end].copy_from_slice(&data);
                }
            }
            _ => {}
        }
        self.dma[ch][0] = madr.wrapping_add(len as u32);
        self.dma[ch][2] = chcr & !(1 << 24);
        self.raised |= 1 << (32 + ch);
    }

    pub fn read_bytes(&self, a: u32, len: usize) -> Vec<u8> {
        (0..len).map(|i| self.ram[(a as usize + i) & (RAM_SIZE - 1)]).collect()
    }

    pub fn write_bytes(&mut self, a: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.ram[(a as usize + i) & (RAM_SIZE - 1)] = b;
        }
    }

    pub fn cstr(&self, a: u32) -> String {
        (0..4096).map(|i| self.ram[(a as usize + i) & (RAM_SIZE - 1)]).take_while(|&b| b != 0).map(|b| b as char).collect()
    }
}

impl Bus for Mem {
    fn r8(&mut self, a: u32) -> u8 {
        let p = a & 0x1fff_ffff;
        match p {
            0..=0x7f_ffff => self.ram[p as usize & (RAM_SIZE - 1)],
            0x1f80_0000..=0x1f80_03ff => self.scratch[(p & 0x3ff) as usize],
            0x1f90_0000..=0x1f90_07ff => (self.spu.read(p & 0x7fe) >> ((p & 1) * 8)) as u8,
            _ => self.io_read(p & !3).to_le_bytes()[(p & 3) as usize],
        }
    }

    fn r16(&mut self, a: u32) -> u16 {
        let p = a & 0x1fff_ffff;
        match p {
            0..=0x7f_ffff => {
                let i = p as usize & (RAM_SIZE - 1);
                u16::from_le_bytes([self.ram[i], self.ram[i + 1]])
            }
            0x1f90_0000..=0x1f90_07ff => self.spu.read(p & 0x7fe),
            _ => (self.r8(a) as u16) | (self.r8(a + 1) as u16) << 8,
        }
    }

    fn r32(&mut self, a: u32) -> u32 {
        let p = a & 0x1fff_ffff;
        match p {
            0..=0x7f_ffff => {
                let i = p as usize & (RAM_SIZE - 1);
                u32::from_le_bytes(self.ram[i..i + 4].try_into().unwrap())
            }
            0x1f80_0000..=0x1f80_03ff => {
                let i = (p & 0x3fc) as usize;
                u32::from_le_bytes(self.scratch[i..i + 4].try_into().unwrap())
            }
            0x1f90_0000..=0x1f90_07ff => self.spu.read(p & 0x7fe) as u32 | (self.spu.read((p & 0x7fe) + 2) as u32) << 16,
            _ => self.io_read(p),
        }
    }

    fn w8(&mut self, a: u32, v: u8) {
        let p = a & 0x1fff_ffff;
        match p {
            0..=0x7f_ffff => self.ram[p as usize & (RAM_SIZE - 1)] = v,
            0x1f80_0000..=0x1f80_03ff => self.scratch[(p & 0x3ff) as usize] = v,
            0x1f90_0000..=0x1f90_07ff => {}
            _ => self.io_write_part(p, v as u32, 0xff),
        }
    }

    fn w16(&mut self, a: u32, v: u16) {
        let p = a & 0x1fff_ffff;
        match p {
            0..=0x7f_ffff => {
                let i = p as usize & (RAM_SIZE - 1);
                self.ram[i..i + 2].copy_from_slice(&v.to_le_bytes());
            }
            0x1f90_0000..=0x1f90_07ff => self.spu.write(p & 0x7fe, v),
            0x1f80_0000..=0x1f80_03ff => {
                self.w8(a, v as u8);
                self.w8(a + 1, (v >> 8) as u8);
            }
            _ => self.io_write_part(p, v as u32, 0xffff),
        }
    }

    fn w32(&mut self, a: u32, v: u32) {
        let p = a & 0x1fff_ffff;
        match p {
            0..=0x7f_ffff => {
                let i = p as usize & (RAM_SIZE - 1);
                self.ram[i..i + 4].copy_from_slice(&v.to_le_bytes());
            }
            0x1f80_0000..=0x1f80_03ff => {
                let i = (p & 0x3fc) as usize;
                self.scratch[i..i + 4].copy_from_slice(&v.to_le_bytes());
            }
            0x1f90_0000..=0x1f90_07ff => {
                self.spu.write(p & 0x7fe, v as u16);
                self.spu.write((p & 0x7fe) + 2, (v >> 16) as u16);
            }
            _ => self.io_write(p, v),
        }
    }
}

pub struct Machine {
    pub mem: Mem,
    pub cpu: Regs,
    k: kernel::Kernel,
    link: Arc<Link>,
    /// IOP cycles since boot (one instruction counts as one cycle).
    pub cycles: u64,
    next_vblank: u64,
}

/// IOP vertical blank rate, as the EE's: NTSC 59.94 Hz.
const VBLANK_CYCLES: u64 = CLOCK * 1001 / 60_000;

impl Machine {
    pub fn new(link: Arc<Link>) -> Machine {
        Machine { mem: Mem::new(), cpu: Regs::default(), k: kernel::Kernel::new(), link, cycles: 0, next_vblank: VBLANK_CYCLES }
    }

    /// After commands to the EE: let it handle them and answer. The EE
    /// usually answers at once (it spin-waits for the IOP), and drivers
    /// count on replies arriving within the microseconds that takes, so
    /// give it a moment before running on.
    fn sync_with_ee(&mut self) {
        self.link.sync();
        let start = std::time::Instant::now();
        while !self.link.iop_mail.load(Ordering::Acquire) && start.elapsed() < std::time::Duration::from_millis(2) {
            std::thread::yield_now();
        }
        self.mail();
    }

    pub(crate) fn has_mail(&self) -> bool {
        self.link.iop_mail.load(Ordering::Acquire)
    }

    pub(crate) fn mail(&mut self) {
        let msgs: Vec<ToIop> = {
            let mut q = self.link.to_iop.lock().unwrap();
            self.link.iop_mail.store(false, Ordering::Release);
            q.drain(..).collect()
        };
        for m in msgs {
            match m {
                ToIop::Load { name, image, args } => {
                    self.load_module(&name, &image, &args);
                    // The module's threads outrank the loader on the real
                    // IOP: they run until they first wait before
                    // LoadModule returns to the EE.
                    for _ in 0..100 {
                        if !self.run_threads(self.cycles + 1_000_000) {
                            break;
                        }
                    }
                    self.link.loaded.fetch_add(1, Ordering::AcqRel);
                }
                ToIop::Cmd { cid, packet, extra } => {
                    if let Some((dest, data)) = extra {
                        self.mem.write_bytes(dest, &data);
                    }
                    self.deliver_cmd(cid, &packet);
                }
                ToIop::Dma { dest, data } => self.mem.write_bytes(dest, &data),
            }
        }
    }

    /// Run the IOP for `cycles`, delivering interrupts and timers.
    pub fn run_for(&mut self, cycles: u64) {
        let end = self.cycles + cycles;
        while self.cycles < end {
            if self.cycles >= self.next_vblank {
                self.next_vblank += VBLANK_CYCLES;
                if self.k.trace && (self.next_vblank / VBLANK_CYCLES).is_multiple_of(60) {
                    eprintln!("[spu2] {}", self.mem.spu.summary());
                }
                self.vblank();
            }
            self.wake_timers();
            self.deliver_interrupts();
            let slice_end = end.min(self.next_vblank);
            if !self.run_threads(slice_end) {
                // Everyone is waiting: skip to the next event.
                self.cycles = slice_end.min(self.k.next_timer().unwrap_or(u64::MAX)).max(self.cycles + 1);
            }
        }
    }

    /// Produce `n` output samples, running the IOP in step with them.
    pub fn render(&mut self, n: usize, out: &mut Vec<[i16; 2]>) {
        self.mail();
        for _ in 0..n {
            self.run_for(CYCLES_PER_SAMPLE);
            let (l, r) = self.mem.spu.tick();
            if self.mem.spu.irq {
                self.mem.spu.irq = false;
                self.mem.raised |= 1 << spu2::INTR_SPU;
            }
            out.push([l, r]);
        }
    }
}

/// Run the IOP on a host thread, feeding `ring` and keeping about
/// `latency` samples queued ahead of the audio device.
pub fn spawn(link: Arc<Link>, ring: Arc<AudioRing>, latency: usize) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("iop".into())
        .spawn(move || {
            let mut m = Machine::new(link);
            let mut chunk = Vec::with_capacity(64);
            // IOP_WAV=path: also record the output (debugging aid).
            let mut wav = std::env::var_os("IOP_WAV").map(|p| (p, Vec::<[i16; 2]>::new()));
            loop {
                while ring.len() > latency {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                chunk.clear();
                m.render(48, &mut chunk);
                ring.buf.lock().unwrap().extend(chunk.iter().copied());
                if let Some((path, rec)) = wav.as_mut() {
                    rec.extend_from_slice(&chunk);
                    if rec.len() % (spu2::RATE as usize * 5) == 0 {
                        let _ = std::fs::write(&path, wav_file(rec));
                    }
                }
            }
        })
        .expect("spawn IOP thread")
}

/// A 48 kHz stereo 16-bit WAV file.
pub fn wav_file(samples: &[[i16; 2]]) -> Vec<u8> {
    let data = samples.len() as u32 * 4;
    let mut f = Vec::with_capacity(44 + data as usize);
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&spu2::RATE.to_le_bytes());
    f.extend_from_slice(&(spu2::RATE * 4).to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        f.extend_from_slice(&s[0].to_le_bytes());
        f.extend_from_slice(&s[1].to_le_bytes());
    }
    f
}
