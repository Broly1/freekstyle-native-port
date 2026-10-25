//! MIPS R3000A (MIPS I) interpreter for IOP modules.
//!
//! The IOP runs small driver modules, so a plain interpreter is plenty:
//! the sound driver needs a few million instructions per second at most.
//! Load delay slots are not modelled; compiled code does not rely on them.

/// Memory as seen by the CPU. Addresses are virtual; implementations map
/// kseg0/kseg1 onto physical memory.
pub trait Bus {
    fn r8(&mut self, a: u32) -> u8;
    fn r16(&mut self, a: u32) -> u16;
    fn r32(&mut self, a: u32) -> u32;
    fn w8(&mut self, a: u32, v: u8);
    fn w16(&mut self, a: u32, v: u16);
    fn w32(&mut self, a: u32, v: u32);
}

/// What stopped a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// SYSCALL with this code: an HLE kernel call (see `irx::link`).
    Syscall(u32),
    /// BREAK or an instruction the IOP does not have.
    Fault(u32),
}

#[derive(Clone, Debug, Default)]
pub struct Regs {
    pub r: [u32; 32],
    pub hi: u32,
    pub lo: u32,
    /// Address of the next instruction to execute.
    pub pc: u32,
    /// Address after it (differs from pc + 4 in a delay slot).
    pub npc: u32,
    pub cop0: [u32; 16],
}

pub const A0: usize = 4;
pub const V0: usize = 2;
pub const V1: usize = 3;
pub const GP: usize = 28;
pub const SP: usize = 29;
pub const FP: usize = 30;
pub const RA: usize = 31;

impl Regs {
    pub fn jump(&mut self, to: u32) {
        self.pc = to;
        self.npc = to.wrapping_add(4);
    }

    pub fn arg(&self, n: usize) -> u32 {
        self.r[A0 + n]
    }

    #[inline]
    fn set(&mut self, r: u32, v: u32) {
        if r != 0 {
            self.r[r as usize] = v;
        }
    }

    /// Execute one instruction.
    pub fn step(&mut self, bus: &mut impl Bus) -> Option<Event> {
        let pc = self.pc;
        let w = bus.r32(pc);
        self.pc = self.npc;
        self.npc = self.pc.wrapping_add(4);

        let op = w >> 26;
        let rs = (w >> 21) & 31;
        let rt = (w >> 16) & 31;
        let rd = (w >> 11) & 31;
        let sa = (w >> 6) & 31;
        let imm = w as u16 as i16 as i32 as u32;
        let uimm = w & 0xffff;
        let s = self.r[rs as usize];
        let t = self.r[rt as usize];
        let branch = |me: &mut Regs, take: bool| {
            if take {
                me.npc = pc.wrapping_add(4).wrapping_add(imm << 2);
            }
        };
        match op {
            0x00 => match w & 63 {
                0x00 => self.set(rd, t << sa),
                0x02 => self.set(rd, t >> sa),
                0x03 => self.set(rd, ((t as i32) >> sa) as u32),
                0x04 => self.set(rd, t << (s & 31)),
                0x06 => self.set(rd, t >> (s & 31)),
                0x07 => self.set(rd, ((t as i32) >> (s & 31)) as u32),
                0x08 => self.npc = s,
                0x09 => {
                    self.set(rd, pc.wrapping_add(8));
                    self.npc = s;
                }
                0x0c => return Some(Event::Syscall((w >> 6) & 0xfffff)),
                0x0d => return Some(Event::Fault(pc)),
                0x10 => self.set(rd, self.hi),
                0x11 => self.hi = s,
                0x12 => self.set(rd, self.lo),
                0x13 => self.lo = s,
                0x18 => {
                    let p = (s as i32 as i64) * (t as i32 as i64);
                    (self.hi, self.lo) = ((p >> 32) as u32, p as u32);
                }
                0x19 => {
                    let p = (s as u64) * (t as u64);
                    (self.hi, self.lo) = ((p >> 32) as u32, p as u32);
                }
                0x1a => {
                    let (n, d) = (s as i32, t as i32);
                    if d == 0 {
                        (self.hi, self.lo) = (n as u32, if n >= 0 { u32::MAX } else { 1 });
                    } else if n == i32::MIN && d == -1 {
                        (self.hi, self.lo) = (0, n as u32);
                    } else {
                        (self.hi, self.lo) = ((n % d) as u32, (n / d) as u32);
                    }
                }
                0x1b => {
                    if t == 0 {
                        (self.hi, self.lo) = (s, u32::MAX);
                    } else {
                        (self.hi, self.lo) = (s % t, s / t);
                    }
                }
                0x20 | 0x21 => self.set(rd, s.wrapping_add(t)),
                0x22 | 0x23 => self.set(rd, s.wrapping_sub(t)),
                0x24 => self.set(rd, s & t),
                0x25 => self.set(rd, s | t),
                0x26 => self.set(rd, s ^ t),
                0x27 => self.set(rd, !(s | t)),
                0x2a => self.set(rd, ((s as i32) < (t as i32)) as u32),
                0x2b => self.set(rd, (s < t) as u32),
                _ => return Some(Event::Fault(pc)),
            },
            0x01 => {
                let take = if rt & 1 == 0 { (s as i32) < 0 } else { (s as i32) >= 0 };
                if rt & 0x10 != 0 {
                    self.r[31] = pc.wrapping_add(8);
                }
                branch(self, take);
            }
            0x02 => self.npc = (pc & 0xf000_0000) | ((w & 0x03ff_ffff) << 2),
            0x03 => {
                self.r[31] = pc.wrapping_add(8);
                self.npc = (pc & 0xf000_0000) | ((w & 0x03ff_ffff) << 2);
            }
            0x04 => branch(self, s == t),
            0x05 => branch(self, s != t),
            0x06 => branch(self, (s as i32) <= 0),
            0x07 => branch(self, (s as i32) > 0),
            0x08 | 0x09 => self.set(rt, s.wrapping_add(imm)),
            0x0a => self.set(rt, ((s as i32) < (imm as i32)) as u32),
            0x0b => self.set(rt, (s < imm) as u32),
            0x0c => self.set(rt, s & uimm),
            0x0d => self.set(rt, s | uimm),
            0x0e => self.set(rt, s ^ uimm),
            0x0f => self.set(rt, uimm << 16),
            0x10 => match rs {
                0x00 => self.set(rt, self.cop0[rd as usize & 15]),
                0x04 => self.cop0[rd as usize & 15] = t,
                // RFE: pop the interrupt-enable/mode stack.
                0x10 => self.cop0[12] = (self.cop0[12] & !0xf) | ((self.cop0[12] >> 2) & 0xf),
                _ => return Some(Event::Fault(pc)),
            },
            0x20 => {
                let v = bus.r8(s.wrapping_add(imm)) as i8 as i32 as u32;
                self.set(rt, v);
            }
            0x21 => {
                let v = bus.r16(s.wrapping_add(imm)) as i16 as i32 as u32;
                self.set(rt, v);
            }
            0x22 => {
                let a = s.wrapping_add(imm);
                let m = bus.r32(a & !3);
                let sh = (3 - (a & 3)) * 8;
                let v = (t & !(u32::MAX << sh)) | (m << sh);
                self.set(rt, v);
            }
            0x23 => {
                let v = bus.r32(s.wrapping_add(imm));
                self.set(rt, v);
            }
            0x24 => {
                let v = bus.r8(s.wrapping_add(imm)) as u32;
                self.set(rt, v);
            }
            0x25 => {
                let v = bus.r16(s.wrapping_add(imm)) as u32;
                self.set(rt, v);
            }
            0x26 => {
                let a = s.wrapping_add(imm);
                let m = bus.r32(a & !3);
                let sh = (a & 3) * 8;
                let v = (t & !(u32::MAX >> sh)) | (m >> sh);
                self.set(rt, v);
            }
            0x28 => bus.w8(s.wrapping_add(imm), t as u8),
            0x29 => bus.w16(s.wrapping_add(imm), t as u16),
            0x2a => {
                let a = s.wrapping_add(imm);
                let m = bus.r32(a & !3);
                let sh = (3 - (a & 3)) * 8;
                bus.w32(a & !3, (m & !(u32::MAX >> sh)) | (t >> sh));
            }
            0x2b => bus.w32(s.wrapping_add(imm), t),
            0x2e => {
                let a = s.wrapping_add(imm);
                let m = bus.r32(a & !3);
                let sh = (a & 3) * 8;
                bus.w32(a & !3, (m & !(u32::MAX << sh)) | (t << sh));
            }
            _ => return Some(Event::Fault(pc)),
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u8>);
    impl Bus for Ram {
        fn r8(&mut self, a: u32) -> u8 {
            self.0[a as usize]
        }
        fn r16(&mut self, a: u32) -> u16 {
            u16::from_le_bytes([self.0[a as usize], self.0[a as usize + 1]])
        }
        fn r32(&mut self, a: u32) -> u32 {
            u32::from_le_bytes(self.0[a as usize..a as usize + 4].try_into().unwrap())
        }
        fn w8(&mut self, a: u32, v: u8) {
            self.0[a as usize] = v;
        }
        fn w16(&mut self, a: u32, v: u16) {
            self.0[a as usize..a as usize + 2].copy_from_slice(&v.to_le_bytes());
        }
        fn w32(&mut self, a: u32, v: u32) {
            self.0[a as usize..a as usize + 4].copy_from_slice(&v.to_le_bytes());
        }
    }

    fn run(code: &[u32], steps: usize) -> (Regs, Ram) {
        let mut ram = Ram(vec![0; 0x1000]);
        for (i, w) in code.iter().enumerate() {
            ram.w32(i as u32 * 4, *w);
        }
        let mut r = Regs::default();
        r.jump(0);
        for _ in 0..steps {
            r.step(&mut ram);
        }
        (r, ram)
    }

    #[test]
    fn loop_with_delay_slot() {
        // v0 = 0; a0 = 5; loop: v0 += a0; a0 -= 1; bnez a0, loop (slot: nop)
        let code = [0x2402_0000, 0x2404_0005, 0x0044_1021, 0x2084_ffff, 0x1480_fffd, 0x0000_0000];
        let (r, _) = run(&code, 2 + 5 * 4);
        assert_eq!(r.r[V0], 15);
        assert_eq!(r.r[A0], 0);
    }

    #[test]
    fn unaligned_word_pairs() {
        // a0 = 0x100; lwl/lwr pair loading the word at 0x101.
        let code = [0x2404_0100, 0x8882_0004, 0x9882_0001];
        let mut ram = Ram(vec![0; 0x200]);
        for (i, w) in code.iter().enumerate() {
            ram.w32(i as u32 * 4, *w);
        }
        for (i, b) in [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66].iter().enumerate() {
            ram.w8(0x100 + i as u32, *b);
        }
        let mut r = Regs::default();
        r.jump(0);
        for _ in 0..3 {
            r.step(&mut ram);
        }
        assert_eq!(r.r[V0], 0x5544_3322);
    }

    #[test]
    fn syscall_code() {
        let (mut r, mut ram) = run(&[0x0000_000c | (0x1234 << 6)], 0);
        assert_eq!(r.step(&mut ram), Some(Event::Syscall(0x1234)));
    }
}
