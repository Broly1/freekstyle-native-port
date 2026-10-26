//! SPU2: the PS2 sound processor.
//!
//! Two cores of 24 voices each play PS-ADPCM from 2 MB of sound RAM, with
//! ADSR envelopes, per-voice pitch and volume, and loop points embedded in
//! the sample data. Core 0's output feeds core 1, whose output goes to the
//! speakers at 48 kHz. Reverb, noise and pitch modulation are not modelled.
//!
//! Registers are 16 bits wide at 0x1f900000-0x1f9007ff. Addresses into
//! sound RAM count 16-bit words and are split into high/low register pairs.

pub const RAM_WORDS: usize = 1 << 20;
pub const RATE: u32 = 48_000;

/// IOP interrupt raised when a voice or transfer touches IRQA.
pub const INTR_SPU: u32 = 9;

const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Phase {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Default)]
struct Voice {
    /// Next block to decode (word address).
    next: u32,
    /// Loop start (word address); set by software or by a loop-start flag.
    lsa: u32,
    /// Software wrote LSA since key on, so loop-start flags don't move it.
    lsa_fixed: bool,
    /// Decoded samples of the current block.
    block: [i16; 28],
    /// Flags of the current block.
    flags: u8,
    /// Position in `block` and 12-bit fraction.
    pos: usize,
    frac: u32,
    /// ADPCM filter history.
    prev: (i32, i32),
    /// Last four output samples, for interpolation.
    hist: [i32; 4],
    phase: Phase,
    level: i32,
    wait: u32,
    /// Sample output after envelope (for ENVX/VOLX reads).
    out: i32,
}

#[derive(Default)]
struct Core {
    voices: Vec<Voice>,
    tsa: u32,
    irq_flag: bool,
}

pub struct Spu2 {
    pub ram: Vec<u16>,
    regs: Vec<u16>,
    cores: [Core; 2],
    /// Pending interrupt for the IOP (INTR_SPU).
    pub irq: bool,
    /// Activity counters, for debugging.
    pub keyons: u64,
    pub dma_bytes: u64,
}

/// Byte offsets of per-core registers (core 1 adds 0x400).
mod reg {
    pub const PMON: u32 = 0x180;
    pub const VMIXL: u32 = 0x188;
    pub const VMIXR: u32 = 0x190;
    pub const ATTR: u32 = 0x19a;
    pub const IRQA: u32 = 0x19c;
    pub const KON: u32 = 0x1a0;
    pub const KOFF: u32 = 0x1a4;
    pub const TSA: u32 = 0x1a8;
    pub const STD: u32 = 0x1ac;
    pub const VADDR: u32 = 0x1c0;
    pub const ENDX: u32 = 0x340;
    pub const STATX: u32 = 0x344;
    /// Master volume, per core at 0x760 + core * 0x28.
    pub const MVOL: u32 = 0x760;
    pub const IRQINFO: u32 = 0x7c2;
}

fn vol(v: u16) -> i32 {
    if v & 0x8000 == 0 {
        (v << 1) as i16 as i32
    } else {
        // Sweep mode is not modelled: hold at full scale in its direction.
        if v & 0x1000 != 0 {
            -0x7fff
        } else {
            0x7fff
        }
    }
}

impl Default for Spu2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Spu2 {
    pub fn new() -> Spu2 {
        let core = || Core { voices: vec![Voice::default(); 24], ..Default::default() };
        let mut s = Spu2 { ram: vec![0; RAM_WORDS], regs: vec![0; 0x400], cores: [core(), core()], irq: false, keyons: 0, dma_bytes: 0 };
        // DMA ready, not busy.
        s.set(reg::STATX, 0x80);
        s.set(reg::STATX + 0x400, 0x80);
        s
    }

    fn get(&self, off: u32) -> u16 {
        self.regs[(off as usize >> 1) & 0x3ff]
    }

    fn set(&mut self, off: u32, v: u16) {
        self.regs[(off as usize >> 1) & 0x3ff] = v;
    }

    fn pair(&self, off: u32) -> u32 {
        ((self.get(off) as u32 & 0xf) << 16 | self.get(off + 2) as u32) & (RAM_WORDS as u32 - 1)
    }

    fn mask24(&self, off: u32) -> u32 {
        self.get(off) as u32 | (self.get(off + 2) as u32 & 0xff) << 16
    }

    /// Register read, `off` relative to 0x1f900000.
    pub fn read(&mut self, off: u32) -> u16 {
        let off = off & 0x7fe;
        if off < 0x760 {
            let c = (off >> 10) as usize;
            let r = off & 0x3ff;
            if r < 0x180 {
                let v = &self.cores[c].voices[(r >> 4) as usize];
                match r & 0xf {
                    0xa => return v.level as u16,
                    0xc | 0xe => return (v.out >> 1) as u16,
                    _ => {}
                }
            } else if (reg::VADDR..reg::VADDR + 24 * 12).contains(&r) {
                let (vi, field) = (((r - reg::VADDR) / 12) as usize, (r - reg::VADDR) % 12);
                let v = &self.cores[c].voices[vi];
                // NAX: the block being played.
                let nax = v.next.wrapping_sub(8) & (RAM_WORDS as u32 - 1);
                match field {
                    8 => return (nax >> 16) as u16,
                    10 => return nax as u16,
                    _ => {}
                }
            } else if r == reg::TSA {
                return (self.cores[c].tsa >> 16) as u16;
            } else if r == reg::TSA + 2 {
                return self.cores[c].tsa as u16;
            }
        } else if off == reg::IRQINFO {
            return (self.cores[0].irq_flag as u16) << 2 | (self.cores[1].irq_flag as u16) << 3;
        }
        self.get(off)
    }

    /// Register write, `off` relative to 0x1f900000.
    pub fn write(&mut self, off: u32, v: u16) {
        let off = off & 0x7fe;
        self.set(off, v);
        if off >= 0x760 {
            return;
        }
        let c = (off >> 10) as usize;
        let base = c as u32 * 0x400;
        let r = off & 0x3ff;
        match r {
            // Each half triggers its own voices: 0-15, then 16-23.
            reg::KON => self.key_on(c, v as u32),
            0x1a2 => self.key_on(c, (v as u32 & 0xff) << 16),
            reg::KOFF | 0x1a6 => {
                let bits = if r == reg::KOFF { v as u32 } else { (v as u32 & 0xff) << 16 };
                for vi in 0..24 {
                    if bits & (1 << vi) != 0 && self.cores[c].voices[vi].phase != Phase::Off {
                        self.cores[c].voices[vi].phase = Phase::Release;
                    }
                }
            }
            reg::TSA | 0x1aa => self.cores[c].tsa = self.pair(base + reg::TSA),
            reg::STD => {
                let a = self.cores[c].tsa as usize;
                self.ram[a] = v;
                self.check_irq(a as u32, 1);
                self.cores[c].tsa = (self.cores[c].tsa + 1) & (RAM_WORDS as u32 - 1);
            }
            reg::ATTR => {
                if v & 0x40 == 0 {
                    self.cores[c].irq_flag = false;
                }
            }
            reg::ENDX | 0x342 => self.set(off, 0),
            _ if (reg::VADDR..reg::VADDR + 24 * 12).contains(&r) => {
                let (vi, field) = (((r - reg::VADDR) / 12) as usize, (r - reg::VADDR) % 12);
                if field == 4 || field == 6 {
                    let lsa = self.pair(base + reg::VADDR + vi as u32 * 12 + 4);
                    let voice = &mut self.cores[c].voices[vi];
                    voice.lsa = lsa;
                    voice.lsa_fixed = true;
                }
            }
            _ => {}
        }
    }

    fn key_on(&mut self, c: usize, bits: u32) {
        let base = c as u32 * 0x400;
        for vi in 0..24 {
            if bits & (1 << vi) == 0 {
                continue;
            }
            self.keyons += 1;
            let ssa = self.pair(base + reg::VADDR + vi as u32 * 12);
            let v = &mut self.cores[c].voices[vi];
            *v = Voice { next: ssa, lsa: ssa, phase: Phase::Attack, ..Voice::default() };
            let endx = base + reg::ENDX + if vi >= 16 { 2 } else { 0 };
            let e = self.get(endx) & !(1 << (vi % 16));
            self.set(endx, e);
            self.decode_next(c, vi);
        }
    }

    /// Copy IOP memory into sound RAM at the core's transfer address.
    pub fn dma_write(&mut self, c: usize, data: &[u8]) {
        self.dma_bytes += data.len() as u64;
        let mut a = self.cores[c].tsa;
        let start = a;
        for h in data.as_chunks::<2>().0 {
            self.ram[a as usize] = u16::from_le_bytes([h[0], h[1]]);
            a = (a + 1) & (RAM_WORDS as u32 - 1);
        }
        self.check_irq(start, data.len() as u32 / 2);
        self.cores[c].tsa = a;
    }

    /// Copy sound RAM out (DMA read) from the transfer address.
    pub fn dma_read(&mut self, c: usize, len: usize) -> Vec<u8> {
        let mut a = self.cores[c].tsa;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len / 2 {
            out.extend_from_slice(&self.ram[a as usize].to_le_bytes());
            a = (a + 1) & (RAM_WORDS as u32 - 1);
        }
        self.cores[c].tsa = a;
        out
    }

    fn check_irq(&mut self, start: u32, words: u32) {
        for c in 0..2 {
            let base = c as u32 * 0x400;
            if self.get(base + reg::ATTR) & 0x40 == 0 {
                continue;
            }
            let irqa = self.pair(base + reg::IRQA);
            if irqa.wrapping_sub(start) & (RAM_WORDS as u32 - 1) < words {
                self.cores[c].irq_flag = true;
                self.irq = true;
            }
        }
    }

    fn decode_next(&mut self, c: usize, vi: usize) {
        let addr = self.cores[c].voices[vi].next;
        self.check_irq(addr, 8);
        let w = |i: u32| self.ram[((addr + i) & (RAM_WORDS as u32 - 1)) as usize];
        let head = w(0);
        let (shift, filter, flags) = ((head & 0xf) as i32, ((head >> 4) & 7) as usize, (head >> 8) as u8);
        let shift = if shift > 12 { 9 } else { shift };
        let (f0, f1) = FILTERS[filter.min(4)];
        let words: [u16; 7] = std::array::from_fn(|i| w(1 + i as u32));
        let v = &mut self.cores[c].voices[vi];
        let (mut p1, mut p2) = v.prev;
        for i in 0..28 {
            let nib = (words[i / 4] >> ((i % 4) * 4)) & 0xf;
            let s = ((nib as i32) << 28 >> 16) >> shift;
            let s = (s + ((p1 * f0 + p2 * f1 + 32) >> 6)).clamp(-0x8000, 0x7fff);
            v.block[i] = s as i16;
            (p2, p1) = (p1, s);
        }
        v.prev = (p1, p2);
        v.flags = flags;
        v.pos = 0;
        if flags & 4 != 0 && !v.lsa_fixed {
            v.lsa = addr;
        }
        v.next = (addr + 8) & (RAM_WORDS as u32 - 1);
    }

    /// Advance to the next block after the current one finished.
    fn end_block(&mut self, c: usize, vi: usize) {
        let flags = self.cores[c].voices[vi].flags;
        if flags & 1 != 0 {
            let endx = c as u32 * 0x400 + reg::ENDX + if vi >= 16 { 2 } else { 0 };
            let e = self.get(endx) | 1 << (vi % 16);
            self.set(endx, e);
            let v = &mut self.cores[c].voices[vi];
            v.next = v.lsa;
            if flags & 2 == 0 {
                v.phase = Phase::Off;
                v.level = 0;
            }
        }
        self.decode_next(c, vi);
    }

    fn envelope(&mut self, c: usize, vi: usize) {
        let base = c as u32 * 0x400 + vi as u32 * 0x10;
        let (a1, a2) = (self.get(base + 6), self.get(base + 8));
        let v = &mut self.cores[c].voices[vi];
        let sustain = ((a1 & 0xf) as i32 + 1) * 0x800;
        // (exponential, decreasing, rate)
        let (exp, dec, rate) = match v.phase {
            Phase::Off => return,
            Phase::Attack => (a1 & 0x8000 != 0, false, (a1 >> 8) & 0x7f),
            Phase::Decay => (true, true, ((a1 >> 4) & 0xf) * 4),
            Phase::Sustain => (a2 & 0x8000 != 0, a2 & 0x4000 != 0, (a2 >> 6) & 0x7f),
            Phase::Release => (a2 & 0x20 != 0, true, (a2 & 0x1f) * 4),
        };
        if v.wait > 0 {
            v.wait -= 1;
            return;
        }
        let shift = (rate >> 2) as i32;
        let base_step = if dec { -8 + (rate & 3) as i32 } else { 7 - (rate & 3) as i32 };
        let mut cycles = 1u32 << (shift - 11).max(0);
        let mut step = base_step << (11 - shift).max(0);
        if exp && !dec && v.level > 0x6000 {
            cycles *= 4;
        }
        if exp && dec {
            step = (step * v.level) >> 15;
        }
        v.wait = cycles - 1;
        v.level = (v.level + step).clamp(0, 0x7fff);
        match v.phase {
            Phase::Attack if v.level >= 0x7fff => v.phase = Phase::Decay,
            Phase::Decay if v.level <= sustain => v.phase = Phase::Sustain,
            Phase::Release if v.level == 0 => v.phase = Phase::Off,
            _ => {}
        }
    }

    /// One voice's next sample, before volume.
    fn voice_sample(&mut self, c: usize, vi: usize) -> i32 {
        let pitch = (self.get(c as u32 * 0x400 + vi as u32 * 0x10 + 4) as u32).min(0x3fff);
        self.envelope(c, vi);
        let (h, frac, level) = {
            let v = &self.cores[c].voices[vi];
            (v.hist, v.frac, v.level)
        };
        // Cubic (Catmull-Rom) interpolation between hist[1] and hist[2].
        let t = frac as i64;
        let (p0, p1, p2, p3) = (h[0] as i64, h[1] as i64, h[2] as i64, h[3] as i64);
        let a = -p0 + 3 * p1 - 3 * p2 + p3;
        let b = 2 * p0 - 5 * p1 + 4 * p2 - p3;
        let cc = -p0 + p2;
        let s = (((((((a * t) >> 12) + b) * t) >> 12) + cc) * t) >> 13;
        let s = (p1 + s).clamp(-0x8000, 0x7fff) as i32;

        let mut steps = {
            let v = &mut self.cores[c].voices[vi];
            v.frac += pitch;
            let n = v.frac >> 12;
            v.frac &= 0xfff;
            n
        };
        while steps > 0 {
            steps -= 1;
            let v = &mut self.cores[c].voices[vi];
            let x = v.block[v.pos] as i32;
            v.hist = [v.hist[1], v.hist[2], v.hist[3], x];
            v.pos += 1;
            if v.pos == 28 {
                self.end_block(c, vi);
                if self.cores[c].voices[vi].phase == Phase::Off {
                    break;
                }
            }
        }
        let out = (s * level) >> 15;
        self.cores[c].voices[vi].out = out;
        out
    }

    /// One-line state summary, for debugging.
    pub fn summary(&self) -> String {
        let mut s = format!("keyons {} dma {} KB", self.keyons, self.dma_bytes >> 10);
        for c in 0..2u32 {
            let b = c * 0x400;
            let active = self.cores[c as usize].voices.iter().filter(|v| v.phase != Phase::Off).count();
            s += &format!(
                " | core{c}: {active} on, attr {:04x} mvol {:04x}/{:04x} vmix {:06x}/{:06x} mmix {:04x}",
                self.get(b + reg::ATTR),
                self.get(reg::MVOL + c * 0x28),
                self.get(reg::MVOL + c * 0x28 + 2),
                self.mask24(b + reg::VMIXL),
                self.mask24(b + reg::VMIXR),
                self.get(b + 0x198)
            );
            // Active voices: number, volume L/R, envelope level, pitch.
            for (vi, v) in self.cores[c as usize].voices.iter().enumerate().filter(|(_, v)| v.phase != Phase::Off) {
                let vb = b + vi as u32 * 0x10;
                s += &format!(" v{vi}:{:04x}/{:04x}/{:04x}/{:04x}", self.get(vb), self.get(vb + 2), v.level, self.get(vb + 4));
            }
        }
        s
    }

    /// Mix one stereo output sample.
    pub fn tick(&mut self) -> (i16, i16) {
        let mut core_out = [(0i32, 0i32); 2];
        for c in 0..2 {
            let base = c as u32 * 0x400;
            let (ml, mr) = (self.mask24(base + reg::VMIXL), self.mask24(base + reg::VMIXR));
            let _pmon = self.mask24(base + reg::PMON);
            let (mut l, mut r) = (0i32, 0i32);
            for vi in 0..24 {
                if self.cores[c].voices[vi].phase == Phase::Off {
                    self.cores[c].voices[vi].out = 0;
                    continue;
                }
                let s = self.voice_sample(c, vi);
                let vb = base + vi as u32 * 0x10;
                if ml & (1 << vi) != 0 {
                    l += (s * vol(self.get(vb))) >> 15;
                }
                if mr & (1 << vi) != 0 {
                    r += (s * vol(self.get(vb + 2))) >> 15;
                }
            }
            core_out[c] = (l, r);
        }
        let mv = |c: u32, side: u32| vol(self.get(reg::MVOL + c * 0x28 + side * 2));
        // Core 0 feeds core 1's input; core 1's master volume applies last.
        let l0 = (core_out[0].0 * mv(0, 0)) >> 15;
        let r0 = (core_out[0].1 * mv(0, 1)) >> 15;
        let l = ((core_out[1].0 + l0) * mv(1, 0)) >> 15;
        let r = ((core_out[1].1 + r0) * mv(1, 1)) >> 15;
        (l.clamp(-0x8000, 0x7fff) as i16, r.clamp(-0x8000, 0x7fff) as i16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A looping ADPCM block of a square-ish wave at full volume.
    fn setup_voice(s: &mut Spu2) {
        // Block at word 0x1000: shift 0, filter 0, flags loop start|end|repeat.
        s.ram[0x1000] = 0x0700;
        for i in 1..8 {
            s.ram[0x1000 + i] = if i < 4 { 0x7777 } else { 0x9999 };
        }
        let vb = 0x1c0; // core 0 voice 0 addresses
        s.write(vb, 0);
        s.write(vb + 2, 0x1000);
        s.write(0x0, 0x3fff); // VOLL
        s.write(0x2, 0x3fff); // VOLR
        s.write(0x4, 0x1000); // pitch 1.0
        s.write(0x6, 0x000f); // ADSR1: fastest attack, sustain max
        s.write(0x8, 0x0000);
        s.write(0x188, 1); // VMIXL voice 0
        s.write(0x190, 1); // VMIXR voice 0
        for (c, base) in [(0u32, 0x760u32), (1, 0x788)] {
            let _ = c;
            s.write(base, 0x3fff);
            s.write(base + 2, 0x3fff);
        }
        s.write(0x1a0, 1);
    }

    #[test]
    fn voice_plays_and_loops() {
        let mut s = Spu2::new();
        setup_voice(&mut s);
        let samples: Vec<(i16, i16)> = (0..2000).map(|_| s.tick()).collect();
        let peak = samples.iter().map(|x| x.0.unsigned_abs()).max().unwrap();
        assert!(peak > 1000, "peak {peak}");
        // Still playing after many loops of the 28-sample block.
        assert!(samples[1900..].iter().any(|x| x.0 != 0));
        // ENDX set by the loop end flag.
        assert_eq!(s.read(0x340) & 1, 1);
    }

    #[test]
    fn key_off_releases() {
        let mut s = Spu2::new();
        setup_voice(&mut s);
        s.write(0x8, 0x0005); // fast linear release... rate 5*4
        for _ in 0..500 {
            s.tick();
        }
        s.write(0x1a4, 1);
        for _ in 0..50_000 {
            s.tick();
        }
        assert_eq!(s.read(0xa), 0, "ENVX after release");
    }

    #[test]
    fn std_port_writes_ram() {
        let mut s = Spu2::new();
        s.write(0x1a8, 0);
        s.write(0x1aa, 0x2000);
        s.write(0x1ac, 0xbeef);
        s.write(0x1ac, 0x1234);
        assert_eq!(&s.ram[0x2000..0x2002], &[0xbeef, 0x1234]);
    }
}
