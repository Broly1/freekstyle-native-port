//! Native sound: answers the game's sound protocol directly, with no
//! emulated IOP or SPU2. The default; FREEK_AUDIO=emulated runs the
//! game's own driver on the emulated IOP and SPU2 instead.
//!
//! This file holds the protocol and music; `sfx` plays sound effects. The
//! protocol and data formats are described in docs/native-audio.md.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::sfx::Sfx;
use crate::{Link, ToEe, ToIop};

const STATUS_SIZE: usize = 0x2c0;
const STATUS_COMMANDS: usize = 10;
pub const ENTRY: usize = 0x40;
const BLOCK_SIZE: usize = 0x22c0;
pub const MAX_COMMANDS: usize = 128;

/// SIF registers the sound driver sets at startup; the EE waits for them.
const SREG_STATUS: u32 = 0x1e;
const STARTUP_SREGS: [(u32, u32); 2] = [(SREG_STATUS, 0x80000), (0x1f, 0x100000)];
const SIF_SET_SREG: u32 = 0x8000_0001;

const KIND_TRANSFER: u8 = 2;
const KIND_ACK: u8 = 3;
const TYPE_BANK: [u8; 3] = [0, 1, 2];
const TYPE_MUSIC: u8 = 3;
const TYPE_STREAM: u8 = 4;
const TYPE_BLOCK: u8 = 5;

const OP_START_MUSIC: u8 = 0x01;
pub const OP_RESUME_AUDIO: u8 = 0x04;
pub const OP_STOP_AUDIO: u8 = 0x05;
pub const OP_STOP_AUDIO_2: u8 = 0x06;
pub const OP_STOP_ALL_NORMAL: u8 = 0x1c;
const OP_STOP_MUSIC: u8 = 0x0b;
const OP_SET_VOLUME: u8 = 0x0c;
const OP_STEM_VOLUME: u8 = 0x0d;
const VOLUME_MUSIC: u8 = 2;

const REPLY_CHUNK_DONE: u8 = 0x08;
const REPLY_MUSIC_RESET: u8 = 0x09;
const REPLY_MUSIC_POSITION: u8 = 0x29;

/// A music chunk: header, then each channel of one stereo stem.
const CHUNK_HEADER: usize = 0x40;
const CHUNK_CHANNEL: usize = 0x2fc0;
const CHUNK_STEM: usize = 0x0c;
const CHUNK_NUMBER: usize = 0x16;

const MAX_STEMS: usize = 3;
const MUSIC_RATE: f64 = 22_050.0;
const OUTPUT_RATE: f64 = crate::spu2::RATE as f64;

pub const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// Core and master volume the driver sets (0x3000 of 0x4000).
pub const MASTER: f32 = 0.75;
/// Per-sample approach of the stem gains: about 10 ms.
const GAIN_SMOOTH: f32 = 0.002;
/// Per-sample decay of the tail that hides a stop: about 5 ms.
const TAIL_DECAY: f32 = 0.996;

pub const TAPS: usize = 16;
pub const PHASES: usize = 256;

/// Windowed-sinc interpolation filter for upsampling.
pub fn sinc_table() -> Vec<[f32; TAPS]> {
    (0..PHASES)
        .map(|phase| {
            let frac = phase as f64 / PHASES as f64;
            let mut row = [0.0; TAPS];
            for (i, c) in row.iter_mut().enumerate() {
                let x = i as f64 - (TAPS as f64 / 2.0 - 1.0) - frac;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let t = (x / (TAPS as f64 / 2.0)).clamp(-1.0, 1.0);
                // Blackman window.
                let w = 0.42 + 0.5 * (std::f64::consts::PI * t).cos() + 0.08 * (2.0 * std::f64::consts::PI * t).cos();
                *c = (sinc * w) as f32;
            }
            let sum: f32 = row.iter().sum();
            row.map(|c| c / sum)
        })
        .collect()
}

/// PS2 ADPCM history of one channel.
#[derive(Clone, Copy, Default)]
struct Adpcm {
    prev: (i32, i32),
}

impl Adpcm {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        let mut out = Vec::with_capacity(data.len() / 16 * 28);
        let (mut p1, mut p2) = self.prev;
        for frame in data.chunks_exact(16) {
            let shift = (frame[0] & 15) as i32;
            let shift = if shift > 12 { 9 } else { shift };
            let (f0, f1) = FILTERS[((frame[0] >> 4) as usize).min(4)];
            for byte in &frame[2..] {
                for nibble in [byte & 15, byte >> 4] {
                    let raw = ((nibble as i32) << 28 >> 16) >> shift;
                    let s = (raw + ((p1 * f0 + p2 * f1 + 32) >> 6)).clamp(-0x8000, 0x7fff);
                    out.push(s as i16);
                    (p2, p1) = (p1, s);
                }
            }
        }
        self.prev = (p1, p2);
        out
    }
}

/// One stereo stem as it arrives; chunks are indexed by the chunk's stem
/// field, which counts down to 0 within a set.
#[derive(Default)]
struct Queue {
    chunks: VecDeque<(u16, Vec<i16>, Vec<i16>)>,
    left: Adpcm,
    right: Adpcm,
}

#[derive(Clone, Copy)]
struct Stem {
    /// 0-255, as the game sets it.
    level: u8,
    target: u8,
    step: u8,
    gain: f32,
}

pub struct Music {
    queues: [Queue; MAX_STEMS],
    stems: [Stem; MAX_STEMS],
    /// Stems in the playing song; 0 when stopped.
    count: usize,
    music_volume: u8,
    mono: bool,
    /// Position in the front chunk of each queue.
    pos: usize,
    /// Input samples owed, as a fraction.
    acc: f64,
    /// Recent input per stem, oldest first (a ring at `head`).
    hist: [[[f32; 2]; TAPS]; MAX_STEMS],
    head: usize,
    table: Vec<[f32; TAPS]>,
    last: [f32; 2],
    tail: [f32; 2],
    starving: bool,
    /// Replies for the EE's next status block.
    replies: VecDeque<[u8; ENTRY]>,
    pub underruns: u64,
    /// Turned down for a voice-over, as the driver does.
    duck: bool,
}

pub fn reply(op: u8, word: u32, word2: u32) -> [u8; ENTRY] {
    let mut e = [0; ENTRY];
    e[0] = op;
    e[4..8].copy_from_slice(&word.to_le_bytes());
    e[8..12].copy_from_slice(&word2.to_le_bytes());
    e
}

impl Music {
    fn new() -> Music {
        Music {
            queues: Default::default(),
            stems: [Stem { level: 255, target: 255, step: 0, gain: 0.0 }; MAX_STEMS],
            count: 0,
            music_volume: 0,
            mono: false,
            pos: 0,
            acc: 0.0,
            hist: [[[0.0; 2]; TAPS]; MAX_STEMS],
            head: 0,
            table: sinc_table(),
            last: [0.0; 2],
            tail: [0.0; 2],
            starving: false,
            replies: VecDeque::new(),
            underruns: 0,
            duck: false,
        }
    }

    fn chunk(&mut self, data: &[u8]) {
        if data.len() < CHUNK_HEADER + 2 * CHUNK_CHANNEL {
            return;
        }
        let c = u32::from_le_bytes(data[CHUNK_STEM..CHUNK_STEM + 4].try_into().unwrap()) as usize;
        let number = u16::from_le_bytes(data[CHUNK_NUMBER..CHUNK_NUMBER + 2].try_into().unwrap());
        let Some(q) = self.queues.get_mut(c) else { return };
        let l = q.left.decode(&data[CHUNK_HEADER..CHUNK_HEADER + CHUNK_CHANNEL]);
        let r = q.right.decode(&data[CHUNK_HEADER + CHUNK_CHANNEL..CHUNK_HEADER + 2 * CHUNK_CHANNEL]);
        q.chunks.push_back((number, l, r));
    }

    fn start(&mut self, stems: usize) {
        self.count = stems.min(MAX_STEMS);
        self.pos = 0;
        self.starving = false;
    }

    fn stop(&mut self) {
        if self.count > 0 {
            let number = self.queues[0].chunks.front().map_or(0, |c| c.0);
            self.replies.push_back(reply(REPLY_MUSIC_POSITION, number as u32, 0));
        }
        self.replies.push_back(reply(REPLY_MUSIC_RESET, 0, 0));
        self.count = 0;
        self.pos = 0;
        for q in &mut self.queues {
            *q = Queue::default();
        }
        self.hist = [[[0.0; 2]; TAPS]; MAX_STEMS];
        self.tail = self.last;
    }

    /// The command block of one game frame.
    fn block(&mut self, b: &[u8]) {
        if b.len() < BLOCK_SIZE {
            return;
        }
        self.mono = b[5] == 0;
        self.music_volume = b[9];
        let n = (u32::from_le_bytes(b[..4].try_into().unwrap()) as usize).min(MAX_COMMANDS);
        for e in b[ENTRY..].chunks_exact(ENTRY).take(n) {
            match e[0] {
                OP_START_MUSIC => self.start(e[1] as usize),
                OP_STOP_MUSIC | OP_STOP_AUDIO | OP_STOP_AUDIO_2 | OP_STOP_ALL_NORMAL => self.stop(),
                OP_RESUME_AUDIO => {}
                OP_SET_VOLUME if e[1] == VOLUME_MUSIC => self.music_volume = e[2],
                OP_STEM_VOLUME if (1..=MAX_STEMS as u8).contains(&e[1]) => {
                    let s = &mut self.stems[e[1] as usize - 1];
                    (s.target, s.step) = (e[2], e[3]);
                }
                _ => {}
            }
        }
        for s in &mut self.stems {
            if s.level < s.target {
                s.level = s.level.saturating_add(s.step).min(s.target);
            } else {
                s.level = s.level.saturating_sub(s.step).max(s.target);
            }
        }
    }

    /// Next input frame of the stems, in stem order, or None when any stem
    /// has no data.
    fn next_input(&mut self) -> Option<[[f32; 2]; MAX_STEMS]> {
        if self.count == 0 || (0..self.count).any(|c| self.queues[c].chunks.is_empty()) {
            return None;
        }
        let mut frame = [[0.0; 2]; MAX_STEMS];
        let pos = self.pos;
        let mut done = false;
        for c in 0..self.count {
            let (_, l, r) = &self.queues[c].chunks[0];
            // The stem field counts down: its last set member is stem 0.
            frame[self.count - 1 - c] = [l[pos] as f32 / 32768.0, r[pos] as f32 / 32768.0];
            done |= pos + 1 >= l.len();
        }
        self.pos += 1;
        if done {
            self.pos = 0;
            for c in 0..self.count {
                self.queues[c].chunks.pop_front();
                self.replies.push_back(reply(REPLY_CHUNK_DONE, 0, 0));
            }
        }
        Some(frame)
    }

    fn sample(&mut self) -> [f32; 2] {
        self.acc += MUSIC_RATE / OUTPUT_RATE;
        while self.acc >= 1.0 {
            self.acc -= 1.0;
            let frame = self.next_input();
            if self.count > 0 && frame.is_none() {
                if !self.starving {
                    self.starving = true;
                    self.underruns += 1;
                    self.tail = self.last;
                }
            } else if frame.is_some() {
                self.starving = false;
            }
            let frame = frame.unwrap_or_default();
            for (h, f) in self.hist.iter_mut().zip(frame) {
                h[self.head] = f;
            }
            self.head = (self.head + 1) % TAPS;
        }
        let phase = ((self.acc * PHASES as f64) as usize).min(PHASES - 1);
        let filter = &self.table[phase];
        let mut out = [0f32; 2];
        for stem in 0..MAX_STEMS {
            let duck = if self.duck { crate::sfx::DUCK } else { 1.0 };
            let want = ((self.stems[stem].level as u32 * self.music_volume as u32) >> 1) as f32 / 16384.0 * MASTER * duck;
            let gain = &mut self.stems[stem].gain;
            *gain += (want - *gain) * GAIN_SMOOTH;
            let gain = *gain;
            for ch in 0..2 {
                let mut v = 0.0;
                for (t, c) in filter.iter().enumerate() {
                    v += c * self.hist[stem][(self.head + t) % TAPS][ch];
                }
                out[ch] += v * gain;
            }
        }
        if self.mono {
            out = [(out[0] + out[1]) * 0.5; 2];
        }
        for ch in 0..2 {
            out[ch] += self.tail[ch];
            self.tail[ch] *= TAIL_DECAY;
        }
        self.last = out;
        out
    }
}

/// The game's side of the protocol, without the sound driver.
pub struct Native {
    link: Arc<Link>,
    music: Music,
    sfx: Sfx,
    status_addr: u32,
    status_slot: u8,
}

impl Native {
    pub fn new(link: Arc<Link>) -> Native {
        Native { link, music: Music::new(), sfx: Sfx::new(), status_addr: 0, status_slot: 0 }
    }

    fn mail(&mut self) {
        if !self.link.iop_mail.load(Ordering::Acquire) {
            return;
        }
        let msgs: Vec<ToIop> = {
            let mut q = self.link.to_iop.lock().unwrap();
            self.link.iop_mail.store(false, Ordering::Release);
            q.drain(..).collect()
        };
        for m in msgs {
            match m {
                ToIop::Load { name, .. } => {
                    if name == "AUDIO.IRX" {
                        for (reg, val) in STARTUP_SREGS {
                            let mut p = vec![0u8; 24];
                            p[0] = p.len() as u8;
                            p[8..12].copy_from_slice(&SIF_SET_SREG.to_le_bytes());
                            p[16..20].copy_from_slice(&reg.to_le_bytes());
                            p[20..24].copy_from_slice(&val.to_le_bytes());
                            self.link.post(ToEe::Cmd { cid: SIF_SET_SREG, packet: p, extra: None });
                        }
                    }
                    self.link.loaded.fetch_add(1, Ordering::AcqRel);
                }
                ToIop::Cmd { cid, packet, extra } => self.command(cid, &packet, extra.map(|(_, d)| d)),
                ToIop::Dma { .. } => {}
            }
        }
    }

    fn command(&mut self, cid: u32, packet: &[u8], data: Option<Vec<u8>>) {
        if cid == SIF_SET_SREG && packet.len() >= 24 {
            let reg = u32::from_le_bytes(packet[16..20].try_into().unwrap());
            if reg == SREG_STATUS {
                self.status_addr = u32::from_le_bytes(packet[20..24].try_into().unwrap());
            }
            return;
        }
        if cid != 0 || packet.len() < 0x20 || packet[0x1c] != KIND_TRANSFER {
            return;
        }
        let mut ack = vec![0u8; ENTRY];
        ack[0] = ENTRY as u8;
        ack[0x1c] = KIND_ACK;
        ack[0x1e] = packet[0x1e];
        self.link.post(ToEe::Cmd { cid: 0, packet: ack, extra: None });
        let data = data.unwrap_or_default();
        match packet[0x1d] {
            TYPE_MUSIC => self.music.chunk(&data),
            TYPE_STREAM => self.sfx.stream_chunk(&data),
            TYPE_BLOCK => {
                self.music.block(&data);
                if data.len() >= BLOCK_SIZE {
                    self.sfx.block(&data);
                }
                self.status();
            }
            t if TYPE_BANK.contains(&t) => {
                let source = u32::from_le_bytes(packet[0x10..0x14].try_into().unwrap());
                self.sfx.transfer(source, data);
            }
            _ => {}
        }
    }

    /// The reply the EE waits for after each command block.
    fn status(&mut self) {
        self.music.replies.extend(self.sfx.take_replies());
        let mut d = vec![0u8; STATUS_SIZE];
        let n = self.music.replies.len().min(STATUS_COMMANDS);
        d[..4].copy_from_slice(&(n as u32).to_le_bytes());
        for (i, e) in self.music.replies.drain(..n).enumerate() {
            d[ENTRY + i * ENTRY..ENTRY * 2 + i * ENTRY].copy_from_slice(&e);
        }
        let mut p = vec![0u8; ENTRY];
        p[..4].copy_from_slice(&(ENTRY as u32 | (STATUS_SIZE as u32) << 8).to_le_bytes());
        p[4..8].copy_from_slice(&self.status_addr.to_le_bytes());
        p[0x14..0x18].copy_from_slice(&(STATUS_SIZE as u32).to_le_bytes());
        p[0x1c] = KIND_TRANSFER;
        p[0x1d] = TYPE_BLOCK;
        p[0x1e] = self.status_slot;
        self.status_slot = (self.status_slot + 1) & 7;
        p[0x20..0x24].copy_from_slice(&self.status_addr.to_le_bytes());
        self.link.post(ToEe::Cmd { cid: 0, packet: p, extra: Some((self.status_addr, d)) });
    }

    pub fn render(&mut self, n: usize, out: &mut Vec<[i16; 2]>) {
        self.mail();
        for _ in 0..n {
            self.music.duck = self.sfx.ducking();
            let mut mix = self.music.sample();
            self.sfx.mix(&mut mix);
            out.push(mix.map(|v| (v * 32768.0).clamp(-32768.0, 32767.0) as i16));
        }
    }

    pub fn sfx_counts(&self) -> (usize, usize, usize) {
        self.sfx.counts()
    }

    pub fn underruns(&self) -> u64 {
        self.music.underruns
    }
}

/// Native unless FREEK_AUDIO=emulated asks for the emulated IOP and SPU2.
pub fn enabled() -> bool {
    std::env::var("FREEK_AUDIO").map_or(true, |v| v != "emulated")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(stem: u32, number: u16, frame0: u8) -> Vec<u8> {
        let mut d = vec![0u8; CHUNK_HEADER + 2 * CHUNK_CHANNEL];
        d[CHUNK_STEM..CHUNK_STEM + 4].copy_from_slice(&stem.to_le_bytes());
        d[CHUNK_NUMBER..CHUNK_NUMBER + 2].copy_from_slice(&number.to_le_bytes());
        for f in 0..CHUNK_CHANNEL / 16 {
            let o = CHUNK_HEADER + f * 16;
            d[o] = 2; // shift 2, no filter
            d[o + 2..o + 16].fill(frame0);
        }
        d
    }

    fn block(music: u8, ops: &[&[u8]]) -> Vec<u8> {
        let mut b = vec![0u8; BLOCK_SIZE];
        b[..4].copy_from_slice(&(ops.len() as u32).to_le_bytes());
        b[5] = 1;
        b[9] = music;
        for (i, e) in ops.iter().enumerate() {
            b[ENTRY + i * ENTRY..ENTRY + i * ENTRY + e.len()].copy_from_slice(e);
        }
        b
    }

    #[test]
    fn decodes_like_the_spu() {
        let mut f = [0u8; 16];
        f[0] = 0x0c;
        f[2] = 0x71;
        f[3] = 0x0f;
        let s = Adpcm::default().decode(&f);
        assert_eq!(&s[..4], &[1, 7, -1, 0]);
    }

    #[test]
    fn plays_and_reports_finished_chunks() {
        let mut m = Music::new();
        m.chunk(&chunk(0, 1, 0x11));
        m.chunk(&chunk(0, 2, 0x11));
        m.block(&block(0x52, &[&[OP_START_MUSIC, 1]]));
        let out: Vec<[f32; 2]> = (0..50_000).map(|_| m.sample()).collect();
        assert!(out[1000..].iter().any(|s| s[0] != 0.0));
        assert_eq!(m.replies.iter().filter(|r| r[0] == REPLY_CHUNK_DONE).count(), 1);
        assert_eq!(m.underruns, 0);
    }

    #[test]
    fn stem_gains_match_the_driver() {
        // The driver wrote voice volume 0x28d7 for these values.
        assert_eq!((255u32 * 0x52) >> 1, 0x28d7);
    }

    #[test]
    fn waits_for_every_stem() {
        let mut m = Music::new();
        m.chunk(&chunk(1, 1, 0x11));
        m.block(&block(0x7f, &[&[OP_START_MUSIC, 2]]));
        assert!(m.next_input().is_none());
        m.chunk(&chunk(0, 1, 0x11));
        assert!(m.next_input().is_some());
    }

    #[test]
    fn stop_clears_queues_and_replies() {
        let mut m = Music::new();
        m.chunk(&chunk(0, 7, 0x11));
        m.block(&block(0x7f, &[&[OP_START_MUSIC, 1], &[OP_STOP_MUSIC]]));
        assert!(m.queues[0].chunks.is_empty());
        let ops: Vec<u8> = m.replies.iter().map(|r| r[0]).collect();
        assert_eq!(ops, [REPLY_MUSIC_POSITION, REPLY_MUSIC_RESET]);
    }
}
