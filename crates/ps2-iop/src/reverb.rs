//! Native reverb: a feedback delay network for the room types and a filtered
//! feedback echo, set from the game's reverb table (see docs/native-audio.md).
//! It does not copy the SPU2's reverb circuit; it aims at the same rooms.

use crate::spu2::RATE;

/// The libsd effect modes the game uses.
pub const MODE_OFF: i32 = 0;
pub const MODE_STUDIO_A: i32 = 2;
pub const MODE_STUDIO_C: i32 = 4;
pub const MODE_HALL: i32 = 5;
pub const MODE_ECHO: i32 = 7;

const LINES: usize = 8;
/// Delay line lengths at 48 kHz for size 1.0, mutually prime.
const LENGTHS: [usize; LINES] = [1433, 1601, 1867, 2053, 2251, 2399, 2617, 2797];
/// Longest echo the delay parameter (0-127) reaches.
const ECHO_MAX_SECONDS: f32 = 0.75;
/// Rate of the gain ramp when the depth or the room changes: about 50 ms.
const RAMP: f32 = 1.0 / 2400.0;

/// One room: decay time, size, pre-delay and high-frequency damping.
#[derive(Clone, Copy, PartialEq)]
struct Room {
    rt60: f32,
    size: f32,
    predelay: f32,
    damping: f32,
}

fn room(mode: i32) -> Option<Room> {
    match mode {
        MODE_STUDIO_A => Some(Room { rt60: 0.7, size: 0.6, predelay: 0.008, damping: 0.35 }),
        MODE_STUDIO_C => Some(Room { rt60: 1.4, size: 0.85, predelay: 0.015, damping: 0.4 }),
        MODE_HALL => Some(Room { rt60: 2.2, size: 1.0, predelay: 0.025, damping: 0.5 }),
        _ => None,
    }
}

struct Delay {
    buf: Vec<f32>,
    pos: usize,
}

impl Delay {
    fn new(len: usize) -> Delay {
        Delay { buf: vec![0.0; len.max(1)], pos: 0 }
    }
    fn read(&self, back: usize) -> f32 {
        let n = self.buf.len();
        self.buf[(self.pos + n - back.min(n - 1).max(1)) % n]
    }
    fn write(&mut self, v: f32) {
        self.buf[self.pos] = v;
        self.pos = (self.pos + 1) % self.buf.len();
    }
}

pub struct Reverb {
    lines: Vec<Delay>,
    lengths: [usize; LINES],
    gains: [f32; LINES],
    damp_state: [f32; LINES],
    damping: f32,
    pre: Delay,
    predelay: usize,
    echo: [Delay; 2],
    echo_len: usize,
    echo_feedback: f32,
    echo_lp: [f32; 2],
    /// Input high-pass, so low rumble does not boom.
    hp: [f32; 2],
    room_level: f32,
    echo_level: f32,
    room_target: f32,
    echo_target: f32,
    mode: i32,
}

impl Reverb {
    pub fn new() -> Reverb {
        let max = (LENGTHS[LINES - 1] as f32 * 1.1) as usize + 1;
        let echo_max = (ECHO_MAX_SECONDS * RATE as f32) as usize + 1;
        Reverb {
            lines: (0..LINES).map(|_| Delay::new(max)).collect(),
            lengths: LENGTHS,
            gains: [0.0; LINES],
            damp_state: [0.0; LINES],
            damping: 0.0,
            pre: Delay::new((0.05 * RATE as f32) as usize),
            predelay: 1,
            echo: [Delay::new(echo_max), Delay::new(echo_max)],
            echo_len: 1,
            echo_feedback: 0.0,
            echo_lp: [0.0; 2],
            hp: [0.0; 2],
            room_level: 0.0,
            echo_level: 0.0,
            room_target: 0.0,
            echo_target: 0.0,
            mode: MODE_OFF,
        }
    }

    /// Sets the effect. `depth` is the wet level (0-1), `delay` and
    /// `feedback` the echo's 0-127 parameters.
    pub fn set(&mut self, mode: i32, depth: f32, delay: i32, feedback: i32) {
        self.mode = mode;
        self.room_target = 0.0;
        self.echo_target = 0.0;
        if let Some(r) = room(mode) {
            for i in 0..LINES {
                self.lengths[i] = ((LENGTHS[i] as f32 * r.size) as usize).max(16);
                let seconds = self.lengths[i] as f32 / RATE as f32;
                self.gains[i] = 10f32.powf(-3.0 * seconds / r.rt60);
            }
            self.damping = r.damping;
            self.predelay = ((r.predelay * RATE as f32) as usize).max(1);
            self.room_target = depth;
        } else if mode == MODE_ECHO {
            let seconds = (delay.clamp(1, 127) as f32 / 127.0) * ECHO_MAX_SECONDS;
            self.echo_len = ((seconds * RATE as f32) as usize).max(1);
            self.echo_feedback = feedback.clamp(0, 127) as f32 / 128.0;
            self.echo_target = depth;
        }
    }

    pub fn active(&self) -> bool {
        self.room_target > 0.0 || self.echo_target > 0.0 || self.room_level > 1e-4 || self.echo_level > 1e-4
    }

    /// One sample: `input` is the sum of the voices sent to the reverb, the
    /// result is the wet signal to add to the mix.
    pub fn process(&mut self, input: [f32; 2]) -> [f32; 2] {
        self.room_level += (self.room_target - self.room_level) * RAMP;
        self.echo_level += (self.echo_target - self.echo_level) * RAMP;
        let mut x = [0.0; 2];
        for ch in 0..2 {
            // About 100 Hz.
            self.hp[ch] += (input[ch] - self.hp[ch]) * 0.013;
            x[ch] = input[ch] - self.hp[ch];
        }
        let mut out = [0.0; 2];

        // Room: pre-delay, then an 8-line network mixed by a Householder
        // matrix, each line damped and decaying to the room's time.
        self.pre.write((x[0] + x[1]) * 0.5);
        let feed = self.pre.read(self.predelay);
        let mut taps = [0.0f32; LINES];
        for i in 0..LINES {
            let v = self.lines[i].read(self.lengths[i]);
            self.damp_state[i] += (v - self.damp_state[i]) * (1.0 - self.damping);
            taps[i] = self.damp_state[i] * self.gains[i];
        }
        let sum: f32 = taps.iter().sum::<f32>() * (2.0 / LINES as f32);
        for i in 0..LINES {
            // Alternate signs so the two outputs differ.
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            self.lines[i].write(taps[i] - sum + feed * sign);
        }
        let room_l = taps[0] + taps[2] + taps[4] + taps[6];
        let room_r = taps[1] + taps[3] + taps[5] + taps[7];
        out[0] += room_l * 0.5 * self.room_level;
        out[1] += room_r * 0.5 * self.room_level;

        // Echo: each side repeats after the delay, softened a little each
        // time; the right side is offset slightly for width.
        for ch in 0..2 {
            let len = if ch == 0 { self.echo_len } else { self.echo_len + self.echo_len / 17 };
            let v = self.echo[ch].read(len);
            self.echo_lp[ch] += (v - self.echo_lp[ch]) * 0.6;
            self.echo[ch].write(x[ch] + self.echo_lp[ch] * self.echo_feedback);
            out[ch] += self.echo_lp[ch] * self.echo_level;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(mode: i32, after: usize) -> f32 {
        let mut r = Reverb::new();
        r.set(mode, 1.0, 70, 13);
        r.room_level = r.room_target;
        r.echo_level = r.echo_target;
        r.process([1.0, 1.0]);
        let mut energy = 0.0;
        for n in 0..after + 4800 {
            let o = r.process([0.0, 0.0]);
            if n >= after {
                energy += o[0] * o[0] + o[1] * o[1];
            }
        }
        energy
    }

    #[test]
    fn halls_ring_longer_than_studios_and_stay_stable() {
        let hall = tail(MODE_HALL, 48_000);
        let studio = tail(MODE_STUDIO_A, 48_000);
        assert!(hall > studio * 10.0, "{hall} {studio}");
        assert!(tail(MODE_HALL, 480_000) < 1e-6);
    }

    #[test]
    fn echo_repeats_after_its_delay() {
        let mut r = Reverb::new();
        r.set(MODE_ECHO, 1.0, 70, 13);
        r.echo_level = 1.0;
        r.process([1.0, 1.0]);
        let delay = (70.0 / 127.0 * ECHO_MAX_SECONDS * RATE as f32) as usize;
        let mut first = None;
        for n in 1..delay * 2 {
            if r.process([0.0, 0.0])[0].abs() > 0.05 && first.is_none() {
                first = Some(n);
            }
        }
        let first = first.unwrap();
        assert!((first as i64 - delay as i64).abs() < 4, "{first} {delay}");
    }

    #[test]
    fn off_is_silent() {
        assert_eq!(tail(MODE_OFF, 0), 0.0);
    }
}
