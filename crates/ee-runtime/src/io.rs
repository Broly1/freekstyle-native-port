//! EE hardware registers. For now this is a register file that logs
//! accesses and fakes the minimum the SDK needs to make progress: DMA
//! transfers complete instantly, and vblank is driven by the host clock.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::Instant;

/// NTSC field rate.
const VBLANK_HZ: f64 = 59.94;
/// INTC causes.
pub const INT_DMAC: u32 = 1;
pub const INT_VBLANK_START: u32 = 2;
pub const INT_VBLANK_END: u32 = 3;
const GS_CSR_VSINT: u128 = 1 << 3;
const GS_CSR_FIELD: u128 = 1 << 13;

const DMA_CHANNELS: [(u32, &str); 10] = [
    (0x1000_8000, "VIF0"),
    (0x1000_9000, "VIF1"),
    (0x1000_a000, "GIF"),
    (0x1000_b000, "fromIPU"),
    (0x1000_b400, "toIPU"),
    (0x1000_c000, "SIF0"),
    (0x1000_c400, "SIF1"),
    (0x1000_c800, "SIF2"),
    (0x1000_d000, "fromSPR"),
    (0x1000_d400, "toSPR"),
];

pub const D_STAT: u32 = 0x1000_e010;
pub const INTC_STAT: u32 = 0x1000_f000;
pub const INTC_MASK: u32 = 0x1000_f010;
const GS_CSR: u32 = 0x1200_1000;
const IPU_CMD: u32 = 0x1000_2000;
const IPU_CTRL: u32 = 0x1000_2010;

pub const CH_FROM_SPR: usize = 8;
pub const CH_TO_SPR: usize = 9;

/// A DMA transfer started by a CHCR write.
pub struct DmaStart {
    pub channel: usize,
    pub chcr: u32,
    pub madr: u32,
    pub qwc: u32,
    pub tadr: u32,
    pub sadr: u32,
}

impl DmaStart {
    /// CHCR.MOD: 0 normal, 1 chain, 2 interleave.
    pub fn mode(&self) -> u32 {
        (self.chcr >> 2) & 3
    }
}

/// Register storage: flat arrays for the EE (0x1000_xxxx) and GS
/// privileged (0x1200_0xxx/1xxx) ranges, which are hit on every DMA kick
/// and poll; a map for anything else.
struct Regs {
    ee: Vec<u128>,
    gs: Vec<u128>,
    other: HashMap<u32, u128>,
}

impl Regs {
    fn new() -> Regs {
        Regs { ee: vec![0; 0x1_0000 / 4], gs: vec![0; 0x2000 / 4], other: HashMap::new() }
    }

    fn slot(&mut self, addr: u32) -> &mut u128 {
        match addr {
            0x1000_0000..=0x1000_ffff => &mut self.ee[((addr - 0x1000_0000) >> 2) as usize],
            0x1200_0000..=0x1200_1fff => &mut self.gs[((addr - 0x1200_0000) >> 2) as usize],
            _ => self.other.entry(addr).or_insert(0),
        }
    }

    fn get(&self, addr: &u32) -> Option<&u128> {
        let addr = *addr;
        match addr {
            0x1000_0000..=0x1000_ffff => self.ee.get(((addr - 0x1000_0000) >> 2) as usize),
            0x1200_0000..=0x1200_1fff => self.gs.get(((addr - 0x1200_0000) >> 2) as usize),
            _ => self.other.get(&addr),
        }
    }

    fn insert(&mut self, addr: u32, v: u128) {
        *self.slot(addr) = v;
    }
}

pub struct Io {
    regs: Regs,
    logged: HashSet<(u32, bool)>,
    pub log: bool,
    /// Consecutive reads of one address, for spotting hardware poll loops.
    last_read: (u32, u64),
    start: Instant,
    pub vblanks: u64,
    /// Channel to the GS on the render thread; None runs headless.
    pub gs: Option<Sender<gs::Msg>>,
    pub gif_packets: u64,
    warned: HashSet<String>,
    /// Set on each new vblank; cleared by `Memory::service_vblank`.
    pub new_vblank: bool,
    /// Time spent spinning on one register (idle), for FREEK_HEARTBEAT.
    pub poll_time: std::time::Duration,
    last_read_at: Instant,
}

impl Io {
    pub fn new() -> Io {
        Io {
            regs: Regs::new(),
            logged: HashSet::new(),
            log: std::env::var_os("FREEK_IO_LOG").is_some(),
            last_read: (0, 0),
            start: Instant::now(),
            vblanks: 0,
            gs: None,
            gif_packets: 0,
            warned: HashSet::new(),
            new_vblank: false,
            poll_time: Default::default(),
            last_read_at: Instant::now(),
        }
    }

    pub fn send_gif(&mut self, data: Vec<u128>) {
        self.gif_packets += 1;
        if let Some(tx) = &self.gs {
            let _ = tx.send(gs::Msg::Gif(data));
        }
    }

    pub fn warn_once(&mut self, msg: &str) {
        if self.warned.insert(msg.to_string()) {
            eprintln!("[warn] {msg}");
        }
    }

    pub fn reg(&self, addr: u32) -> u32 {
        self.regs.get(&addr).copied().unwrap_or(0) as u32
    }

    pub fn set_reg(&mut self, addr: u32, v: u32) {
        self.regs.insert(addr, v as u128);
    }

    /// Advance hardware time to the host clock: raise vblank start/end in
    /// INTC_STAT and set the GS vsync flag for each new field.
    pub fn tick(&mut self) {
        let n = (self.start.elapsed().as_secs_f64() * VBLANK_HZ) as u64;
        if n > self.vblanks {
            self.vblanks = n;
            self.new_vblank = true;
            let stat = self.reg(INTC_STAT) | 1 << INT_VBLANK_START | 1 << INT_VBLANK_END;
            self.set_reg(INTC_STAT, stat);
            let csr = self.regs.get(&GS_CSR).copied().unwrap_or(0) | GS_CSR_VSINT;
            let csr = if n % 2 == 1 { csr | GS_CSR_FIELD } else { csr & !GS_CSR_FIELD };
            self.regs.insert(GS_CSR, csr);
            if let Some(tx) = &self.gs {
                let _ = tx.send(gs::Msg::Vsync(n));
            }
        }
    }

    /// Mark a DMA channel's transfer complete in D_STAT, raising the DMAC
    /// interrupt cause when that channel is unmasked.
    fn dma_done(&mut self, channel: usize) {
        let stat = self.reg(D_STAT) | 1 << channel;
        self.set_reg(D_STAT, stat);
        if stat & (stat >> 16) & 0x3ff != 0 {
            let intc = self.reg(INTC_STAT) | 1 << INT_DMAC;
            self.set_reg(INTC_STAT, intc);
        }
    }

    fn name(addr: u32) -> String {
        for (base, name) in DMA_CHANNELS {
            if (base..base + 0x400).contains(&addr) {
                let reg = match addr - base {
                    0x00 => "CHCR",
                    0x10 => "MADR",
                    0x20 => "QWC",
                    0x30 => "TADR",
                    0x80 => "SADR",
                    _ => "?",
                };
                return format!("D_{name}_{reg}");
            }
        }
        match addr {
            0x1000_0000..=0x1000_1fff => format!("TIMER{}", (addr >> 11) & 3),
            0x1000_e000..=0x1000_efff => format!("DMAC+{:x}", addr - 0x1000_e000),
            0x1000_f000..=0x1000_f0ff => format!("INTC+{:x}", addr - 0x1000_f000),
            0x1000_f200..=0x1000_f2ff => format!("SIF+{:x}", addr - 0x1000_f200),
            0x1200_0000..=0x1200_1fff => format!("GS+{:x}", addr - 0x1200_0000),
            _ => "unknown".into(),
        }
    }

    fn trace(&mut self, addr: u32, write: bool, n: usize, v: u128) {
        if self.log {
            let dir = if write { "W" } else { "R" };
            eprintln!("[io] {dir}{} {addr:08x} {:<16} = {v:#x}", n * 8, Self::name(addr));
        }
    }

    pub fn read(&mut self, addr: u32, n: usize) -> u128 {
        if matches!(addr, INTC_STAT | GS_CSR | D_STAT) {
            self.tick();
        }
        let v = match addr {
            // FIFO always empty; GS revision 0x55.
            GS_CSR => self.regs.get(&addr).copied().unwrap_or(0) | (1 << 14) | (0x55 << 24),
            0x1000_0000 | 0x1000_0800 | 0x1000_1000 | 0x1000_1800 => {
                // Timer counters: advance on every read.
                let c = self.regs.slot(addr);
                *c = (*c + 64) & 0xffff;
                *c
            }
            // IPU: no decoder yet, so never busy (BUSY bit 31, command
            // result busy bit 63 of IPU_CMD).
            IPU_CMD | IPU_CTRL => self.regs.get(&addr).copied().unwrap_or(0) & !(1u128 << 31) & !(1u128 << 63),
            _ => self.regs.get(&addr).copied().unwrap_or(0),
        };
        if self.last_read.0 == addr {
            self.last_read.1 += 1;
            // Repeated reads of one register are a poll loop: idle time.
            let now = Instant::now();
            self.poll_time += now - self.last_read_at;
            self.last_read_at = now;
            if self.last_read.1 == 5_000_000 {
                eprintln!("[io] warning: polling {addr:08x} ({}) for 5M reads, value {v:#x}", Self::name(addr));
            }
        } else {
            self.last_read = (addr, 1);
            self.last_read_at = Instant::now();
        }
        self.trace(addr, false, n, v);
        v
    }

    /// Register write. Returns a DMA transfer when this write started one;
    /// the caller performs it (it needs memory) and then calls `dma_done`.
    pub fn write(&mut self, addr: u32, n: usize, v: u128) -> Option<DmaStart> {
        self.trace(addr, true, n, v);
        match addr {
            // Status bits are write-1-to-clear; mask bits toggle.
            D_STAT => {
                let old = self.regs.get(&addr).copied().unwrap_or(0) as u32;
                let v = v as u32;
                let stat = old & 0xffff & !(v & 0xffff);
                let mask = (old ^ v) & 0xffff_0000;
                self.regs.insert(addr, (stat | mask) as u128);
            }
            INTC_STAT => {
                let old = self.regs.get(&addr).copied().unwrap_or(0);
                self.regs.insert(addr, old & !v);
            }
            INTC_MASK => {
                let old = self.regs.get(&addr).copied().unwrap_or(0);
                self.regs.insert(addr, old ^ v);
            }
            0x1200_0000..=0x1200_00ff => {
                self.regs.insert(addr, v);
                if let Some(tx) = &self.gs {
                    let _ = tx.send(gs::Msg::Priv(addr - 0x1200_0000, v as u64));
                }
            }
            GS_CSR => {
                // Event bits (0-4) are write-1-to-clear; keep FIELD.
                let old = self.regs.get(&addr).copied().unwrap_or(0);
                self.regs.insert(addr, old & !(v & 0x1f));
            }
            _ if DMA_CHANNELS.iter().any(|&(b, _)| addr == b) => {
                let channel = DMA_CHANNELS.iter().position(|&(b, _)| addr == b).unwrap();
                // CHCR: transfers run to completion as soon as they start.
                self.regs.insert(addr, v & !0x100);
                if v & 0x100 != 0 {
                    return Some(DmaStart {
                        channel,
                        chcr: v as u32,
                        madr: self.reg(addr + 0x10),
                        qwc: self.reg(addr + 0x20) & 0xffff,
                        tadr: self.reg(addr + 0x30),
                        sadr: self.reg(addr + 0x80),
                    });
                }
            }
            _ => {
                self.regs.insert(addr, v);
            }
        }
        None
    }

    /// After a transfer: advance MADR past the data, clear QWC and flag
    /// completion.
    pub fn dma_finish(&mut self, d: &DmaStart) {
        let base = DMA_CHANNELS[d.channel].0;
        self.set_reg(base + 0x10, d.madr.wrapping_add(d.qwc * 16));
        self.set_reg(base + 0x20, 0);
        self.dma_done(d.channel);
    }

    /// Report a transfer kind once.
    pub fn dma_note(&mut self, d: &DmaStart, what: &str) {
        if self.logged.insert((DMA_CHANNELS[d.channel].0 | 1, true)) {
            let name = DMA_CHANNELS[d.channel].1;
            eprintln!(
                "[dma] {name}: {what} (chcr={:#x} madr={:#x} qwc={} tadr={:#x})",
                d.chcr, d.madr, d.qwc, d.tadr
            );
        }
    }
}

impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}
