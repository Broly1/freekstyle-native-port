//! Native sound effects: banks, one-shot and looping voices, ambient sounds
//! and 3D placement. Layouts are in docs/native-audio.md.
//!
//! Bikes are in `bikes`; the reverb itself is in `reverb`.

mod bikes;

use crate::reverb::{self, Reverb};

use std::collections::HashMap;
use std::sync::Arc;

use crate::native::{reply, sinc_table, FILTERS, OP_STOP_AUDIO, OP_STOP_AUDIO_2, OP_STOP_ALL_NORMAL, ENTRY, MASTER, MAX_COMMANDS, OP_RESUME_AUDIO, PHASES, TAPS};

const OP_STREAM_DONE: u8 = 0x02;
const OP_SAMPLE: u8 = 0x03;
const OP_SIDE_STOP: u8 = 0x07;
const OP_SET_VOLUME: u8 = 0x0c;
const OP_KILL_BIKES: u8 = 0x12;
const OP_INIT_MODULE: u8 = 0x13;
const OP_CLOSE_MODULE: u8 = 0x14;
const OP_COCKPIT: u8 = 0x16;
const OP_STREAM_PLACE: u8 = 0x17;
const OP_STREAM_MOVE: u8 = 0x18;
const OP_STREAM_INIT: u8 = 0x1a;
const OP_STREAM_PLAY: u8 = 0x1b;
const OP_COLLISION: u8 = 0x1d;
const OP_AMBIENT: u8 = 0x1f;
const OP_BIG_START: u8 = 0x20;
const OP_BIG_STOP: u8 = 0x21;
const OP_PLAYERS: u8 = 0x22;
const OP_MENU: u8 = 0x25;
const OP_PLAYERS_2: u8 = 0x2a;
const OP_AMBIENT_MUTE: u8 = 0x2b;

const TAG_SHD: &[u8; 4] = b"dhsC";
const TAG_VKH: &[u8; 4] = b"hkvC";
const TAG_VKB: &[u8; 4] = b"bkvC";
const COMPILED_VERSION: u16 = 7;

const SOUND_IDS: usize = 0x180;
const DEF_SIZE: usize = 0x34;
const TONE_SIZE: usize = 0x14;
const MAX_VARIANTS: usize = 8;

const MAX_VOICES: usize = 48;
const AMBIENT_SLOTS: usize = 128;
const BIG_SLOTS: usize = 6;
const BIG_SLOT_ONE_SHOT: i8 = 1;
const BIG_OFFSET: i32 = 0x40;

const REPLY_STREAM_DONE: u8 = 0x02;
const REPLY_STREAM_CHUNK: u8 = 0x0a;

const STREAM_HEADER: usize = 0x40;
const STREAM_TOTAL: usize = 0x14;
const STREAM_INDEX: usize = 0x16;
/// The driver's fixed voice-over pitch: 22,050 Hz.
const STREAM_PITCH: f64 = 0x759 as f64;
/// No shaping: straight to full level, held until stopped.
const STREAM_ADSR: (u16, u16) = (0x000f, 0x1fc0);
/// Command blocks to wait for a stream's last chunk.
const STREAM_WAIT: u32 = 10;
/// Level of other sounds during a voice-over.
pub const DUCK: f32 = 7.0 / 8.0;
const MICS: usize = 0x2240;
/// The driver's reverb table (0xd648): per reverb set (op 0x13 +1), four rooms of (libsd
/// effect mode, depth 16.16, echo delay, echo feedback). Set 9 is off.
const REVERB_SETS: [(i32, i32, i32, i32); 40] = [
    (7, 0x57e9, 0x46, 0xd), (7, 0xc28f, 0x46, 0xd), (5, 0xc28f, 0, 0), (4, 0x947a, 0, 0),
    (7, 0x57e9, 0x46, 0xd), (7, 0xa8f5, 0x46, 0xd), (5, 0x947a, 0, 0), (5, 0xc28f, 0, 0),
    (7, 0x57e9, 0x2d, 0x8), (7, 0xc000, 0x2d, 0x8), (5, 0x947a, 0, 0), (5, 0x947a, 0, 0),
    (7, 0x7897, 0x1e, 0xd), (7, 0xfff6, 0x1e, 0xd), (5, 0x947a, 0, 0), (4, 0xe353, 0, 0),
    (7, 0x77e9, 0x29, 0x12), (7, 0xa8f5, 0x46, 0xd), (7, 0xd645, 0x19, 0xf), (5, 0xa47a, 0, 0),
    (7, 0x57e9, 0x46, 0xd), (7, 0xa8f5, 0x46, 0xd), (5, 0x947a, 0, 0), (5, 0x947a, 0, 0),
    (5, 0x87e9, 0, 0), (7, 0xa8f5, 0x46, 0xd), (5, 0x947a, 0, 0), (5, 0x947a, 0, 0),
    (7, 0x57e9, 0x46, 0xd), (7, 0xa8f5, 0x46, 0xd), (2, 0xe353, 0, 0), (5, 0x947a, 0, 0),
    (7, 0x57e9, 0x21, 0xd), (7, 0xa8f5, 0x35, 0xd), (5, 0x947a, 0, 0), (5, 0x947a, 0, 0),
    (0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 0, 0),
];
/// Voice-overs 3 dB above the driver's level. In the emulated trace the
/// driver sometimes keyed a line on two voices half a second apart, making
/// it about twice as loud; this matches how the original was heard without
/// the echo.
const VOICE_LIFT: f32 = std::f32::consts::SQRT_2;
/// Range of a placed voice-over, from the driver's start-up: full level to
/// 0.927 units, 0.25 at 1.083, silent at 1.25.
const STREAM_PLACE: Category = Category {
    variants: Vec::new(),
    last: 0,
    near: 0xed30 as f64 / 65536.0,
    far: 0x11530 as f64 / 65536.0,
    slope_near: 0.75 / ((0x11530 - 0xed30) as f64 / 65536.0),
    slope_far: 0.25 / ((0x14000 - 0x11530) as f64 / 65536.0),
    doppler: false,
    pitch_random: 0.0,
};

/// Address of a voice that has played its last frame.
const END: usize = usize::MAX;

const ONE: i64 = 0x10000;
/// Pitch register value for 48 kHz.
const PITCH_UNIT: f64 = 0x1000 as f64;
/// Per-sample approach of the voice gains: about 20 ms.
const GAIN_SMOOTH: f32 = 0.001;
/// Doppler's speed-of-sound constant, in the driver's distance units.
const DOPPLER_K: f64 = 0x46e58 as f64 / ONE as f64;

/// Sound ids the driver treats specially.
const ID_ALIAS: (u16, u16) = (0xab, 0x39);
const IDS_WIDE: [u16; 3] = [0xa0, 0x105, 0xae];
const IDS_DOUBLED: [u16; 3] = [0x10, 0xf, 0xe];
const IDS_FADING: [u16; 2] = [0xef, 0xe0];
const ID_COLLISION_ALT: u16 = 0x1e;
const ID_COLLISION: u16 = 0xd;
const ID_COLLISION_OTHER: u16 = 0x2f;

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn i32_at(d: &[u8], o: usize) -> i32 {
    u32_at(d, o) as i32
}
fn fixed(d: &[u8], o: usize) -> f64 {
    i32_at(d, o) as f64 / ONE as f64
}
fn vec3(d: &[u8], o: usize) -> [f64; 3] {
    [fixed(d, o), fixed(d, o + 4), fixed(d, o + 8)]
}

/// One PS2 ADSR envelope, stepped once per output sample.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Attack,
    Decay,
    Sustain,
    Release,
    Off,
}

#[derive(Clone, Copy)]
struct Envelope {
    phase: Phase,
    level: i32,
    counter: u32,
    adsr1: u16,
    adsr2: u16,
}

impl Envelope {
    fn new(adsr1: u16, adsr2: u16) -> Envelope {
        Envelope { phase: Phase::Attack, level: 0, counter: 0, adsr1, adsr2 }
    }

    fn step(&mut self) {
        let (a1, a2) = (self.adsr1 as i32, self.adsr2 as i32);
        // (exponential, decreasing, shift, step)
        let (exp, down, shift, step) = match self.phase {
            Phase::Off => return,
            Phase::Attack => (a1 & 0x8000 != 0, false, (a1 >> 10) & 31, 7 - ((a1 >> 8) & 3)),
            Phase::Decay => (true, true, (a1 >> 4) & 15, -8),
            Phase::Sustain => {
                let down = a2 & 0x4000 != 0;
                let step = 7 - ((a2 >> 6) & 3);
                (a2 & 0x8000 != 0, down, (a2 >> 8) & 31, if down { !step } else { step })
            }
            Phase::Release => (a2 & 0x20 != 0, true, a2 & 31, -8),
        };
        let mut inc = 0x8000u32 >> (shift - 11).max(0);
        let mut add = step << (11 - shift).max(0);
        if exp {
            if !down && self.level > 0x6000 {
                inc >>= 2;
            }
            if down {
                add = ((add * self.level) >> 15) as i16 as i32;
            }
        }
        self.counter += inc.max(1);
        if self.counter >= 0x8000 {
            self.counter = 0;
            self.level = (self.level + add).clamp(0, 0x7fff);
        }
        match self.phase {
            Phase::Attack if self.level >= 0x7fff => self.phase = Phase::Decay,
            Phase::Decay if self.level <= ((a1 & 15) + 1) << 11 => self.phase = Phase::Sustain,
            Phase::Sustain | Phase::Release if self.level == 0 => self.phase = Phase::Off,
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Tone {
    adsr1: u16,
    adsr2: u16,
    pitch: u16,
    offset: u32,
}

struct Bank {
    body: Arc<Vec<u8>>,
    /// First tone of each program; the driver only ever plays tone 0.
    programs: Vec<Option<Tone>>,
}

#[derive(Default, Clone)]
struct Category {
    /// (bank index, program).
    variants: Vec<(usize, u8)>,
    last: usize,
    near: f64,
    far: f64,
    slope_near: f64,
    slope_far: f64,
    doppler: bool,
    pitch_random: f64,
}

struct Voice {
    id: u64,
    body: Arc<Vec<u8>>,
    addr: usize,
    loop_addr: usize,
    frame: [i16; 28],
    frame_pos: usize,
    p1: i32,
    p2: i32,
    /// No more frames: run out the filter, then stop.
    ending: usize,
    /// The tone's own step per output sample, and the current one.
    tone_pitch: f64,
    pitch: f64,
    acc: f64,
    hist: [f32; TAPS],
    head: usize,
    env: Envelope,
    gain: [f32; 2],
    target: [f32; 2],
    halted: bool,
    /// Sent to the reverb (the driver's default; menu, cockpit and voice-over
    /// voices are not).
    reverb: bool,
}

impl Voice {
    fn next_frame(&mut self) {
        let Some(f) = self.body.get(self.addr..self.addr.saturating_add(16)) else {
            self.ending = TAPS;
            return;
        };
        let shift = (f[0] & 15) as i32;
        let shift = if shift > 12 { 9 } else { shift };
        let (f0, f1) = FILTERS[((f[0] >> 4) as usize).min(4)];
        let flags = f[1];
        let mut i = 0;
        for byte in &f[2..16] {
            for nibble in [byte & 15, byte >> 4] {
                let raw = ((nibble as i32) << 28 >> 16) >> shift;
                let s = (raw + ((self.p1 * f0 + self.p2 * f1 + 32) >> 6)).clamp(-0x8000, 0x7fff);
                self.frame[i] = s as i16;
                (self.p2, self.p1) = (self.p1, s);
                i += 1;
            }
        }
        self.frame_pos = 0;
        if flags & 4 != 0 {
            self.loop_addr = self.addr;
        }
        self.addr = if flags & 1 == 0 {
            self.addr + 16
        } else if flags & 2 != 0 {
            self.loop_addr
        } else {
            END
        };
    }

    fn next_sample(&mut self) -> f32 {
        if self.ending > 0 {
            self.ending -= 1;
            return 0.0;
        }
        if self.frame_pos >= 28 {
            self.next_frame();
            if self.ending > 0 {
                return 0.0;
            }
        }
        let s = self.frame[self.frame_pos];
        self.frame_pos += 1;
        s as f32 / 32768.0
    }

    /// Adds this voice to `out`; false when it has finished.
    fn mix(&mut self, table: &[[f32; TAPS]], out: &mut [f32; 2], send: &mut [f32; 2]) -> bool {
        if self.env.phase == Phase::Off {
            return false;
        }
        if self.halted {
            return true;
        }
        self.acc += self.pitch;
        while self.acc >= 1.0 {
            self.acc -= 1.0;
            self.hist[self.head] = self.next_sample();
            self.head = (self.head + 1) % TAPS;
            if self.addr == END && self.frame_pos >= 28 && self.ending == 0 {
                self.ending = TAPS;
            }
        }
        self.env.step();
        let mut v = 0.0;
        let filter = &table[((self.acc * PHASES as f64) as usize).min(PHASES - 1)];
        for (t, c) in filter.iter().enumerate() {
            v += c * self.hist[(self.head + t) % TAPS];
        }
        v *= self.env.level as f32 / 32767.0;
        for ch in 0..2 {
            self.gain[ch] += (self.target[ch] - self.gain[ch]) * GAIN_SMOOTH;
            out[ch] += v * self.gain[ch];
            if self.reverb {
                send[ch] += v * self.gain[ch];
            }
        }
        self.ending != 1
    }

    fn loudness(&self) -> f32 {
        (self.target[0].abs() + self.target[1].abs()) * self.env.level as f32
    }
}

struct Ambient {
    cat: Option<usize>,
    pos: [f64; 3],
    volume: f64,
    pitch: f64,
    voice: Option<u64>,
    distance: f64,
}

#[derive(Clone, Copy, Default)]
struct Mic {
    pos: [f64; 3],
    /// Rows as the driver stores them, 3 x 3.
    m: [f64; 9],
}

/// Attenuation and pan of a position, as the driver works them out.
struct Placed {
    att: [f64; 2],
    pan: f64,
    distance: f64,
}

/// The voice-over stream: one mono sample sent in chunks.
#[derive(Default)]
struct Stream {
    /// Positional stream id and position (ops 0x17 and 0x18).
    place: Option<(u32, [f64; 3])>,
    id: u32,
    data: Vec<u8>,
    complete: bool,
    /// Blocks left to wait for the rest of a requested stream; 0 if none.
    wait: u32,
    voice: Option<u64>,
}

pub struct Sfx {
    banks: Vec<Bank>,
    defs: Vec<Vec<u8>>,
    pending: Option<(u32, Vec<u8>)>,
    categories: Vec<Category>,
    keys: HashMap<u8, usize>,
    lookup: [u8; SOUND_IDS],
    voices: Vec<Voice>,
    next_id: u64,
    ambient: Vec<Option<Ambient>>,
    ambient_on: bool,
    menu_loop: Option<(u64, u8)>,
    menu_one_shot: Option<u64>,
    big: [[Option<u64>; 2]; BIG_SLOTS],
    /// sfx, music, movie, announcer, engine; index 1 based as the game's.
    volumes: [u8; 6],
    mono: bool,
    players: u8,
    mics: [Mic; 2],
    bikes: bikes::Bikes,
    stream: Stream,
    reverb: Reverb,
    reverb_set: usize,
    reverb_preset: Option<u8>,
    reverb_on: bool,
    replies: Vec<[u8; ENTRY]>,
    rand: u32,
    table: Vec<[f32; TAPS]>,
}

const NO_CATEGORY: u8 = 0xff;

impl Sfx {
    pub fn new() -> Sfx {
        Sfx {
            banks: Vec::new(),
            defs: Vec::new(),
            pending: None,
            categories: Vec::new(),
            keys: HashMap::new(),
            lookup: [NO_CATEGORY; SOUND_IDS],
            voices: Vec::with_capacity(MAX_VOICES),
            next_id: 1,
            ambient: (0..AMBIENT_SLOTS).map(|_| None).collect(),
            ambient_on: true,
            menu_loop: None,
            menu_one_shot: None,
            big: [[None; 2]; BIG_SLOTS],
            volumes: [0; 6],
            mono: false,
            players: 1,
            mics: [Mic::default(); 2],
            stream: Stream::default(),
            reverb: Reverb::new(),
            reverb_set: 0,
            reverb_preset: None,
            reverb_on: true,
            bikes: bikes::Bikes::default(),
            replies: Vec::new(),
            rand: 0x1234_5678,
            table: sinc_table(),
        }
    }

    fn random(&mut self) -> u32 {
        self.rand ^= self.rand << 13;
        self.rand ^= self.rand >> 17;
        self.rand ^= self.rand << 5;
        self.rand
    }

    /// Random number in 0..n.
    fn below(&mut self, n: u32) -> u32 {
        ((self.random() as u64 * n as u64) >> 32) as u32
    }

    /// A header or body transfer. `source` is its EE address: the words at
    /// +0 are EE pointers, relative to it.
    pub fn transfer(&mut self, source: u32, mut data: Vec<u8>) {
        if data.len() < 0x40 {
            return;
        }
        let start = u32_at(&data, 0).wrapping_sub(source) as usize;
        let id = u32_at(&data, 0x20);
        let tag: [u8; 4] = data[0x1c..0x20].try_into().unwrap();
        if start >= data.len() {
            return;
        }
        match &tag {
            TAG_SHD => {
                if u16_at(&data, start) == COMPILED_VERSION {
                    self.defs.push(data.split_off(start));
                }
            }
            TAG_VKH => self.pending = Some((id, data.split_off(start))),
            TAG_VKB => {
                if let Some((pending, header)) = self.pending.take() {
                    if pending == id {
                        self.open_bank(id, header, data.split_off(start));
                    }
                }
            }
            _ => {}
        }
    }

    fn open_bank(&mut self, id: u32, header: Vec<u8>, body: Vec<u8>) {
        let count = header[4] as usize;
        let mut programs = Vec::with_capacity(count);
        for i in 0..count {
            let table = 0x10 + i * 4;
            let offset = if table + 4 <= header.len() { u32_at(&header, table) as usize } else { 0 };
            let tone = offset + 4;
            programs.push((offset != 0 && tone + TONE_SIZE <= header.len() && header[offset] > 0).then(|| Tone {
                adsr1: u16_at(&header, tone + 0xa),
                adsr2: u16_at(&header, tone + 0xc),
                pitch: u16_at(&header, tone + 0xe),
                offset: u32_at(&header, tone + 0x10),
            }));
        }
        let index = self.banks.len();
        self.banks.push(Bank { body: Arc::new(body), programs });
        self.add_definitions(id, index);
    }

    fn add_definitions(&mut self, id: u32, bank: usize) {
        for d in std::mem::take(&mut self.defs) {
            let entries = u16_at(&d, 2) as usize;
            for e in 0..entries {
                let entry = 4 + e * 8;
                if entry + 8 > d.len() || u16_at(&d, entry) as u32 != id {
                    continue;
                }
                let count = u16_at(&d, entry + 2) as usize;
                let first = u32_at(&d, entry + 4) as usize;
                for r in d[first.min(d.len())..].chunks_exact(DEF_SIZE).take(count) {
                    self.add_definition(r, bank);
                }
            }
            self.defs.push(d);
        }
    }

    fn add_definition(&mut self, r: &[u8], bank: usize) {
        let key = r[0];
        let program = r[2];
        let cat = match self.keys.get(&key) {
            Some(&c) => c,
            None => {
                let c = self.categories.len();
                self.keys.insert(key, c);
                for i in 0..MAX_VARIANTS {
                    let id = u16_at(r, 8 + i * 2) as usize;
                    if id > 0 && id < SOUND_IDS {
                        self.lookup[id] = c as u8;
                    }
                }
                self.categories.push(Category {
                    near: fixed(r, 0x18),
                    far: fixed(r, 0x1c),
                    slope_near: fixed(r, 0x24),
                    slope_far: fixed(r, 0x28),
                    doppler: u32_at(r, 0x2c) & 1 != 0,
                    pitch_random: fixed(r, 0x30),
                    ..Category::default()
                });
                c
            }
        };
        let c = &mut self.categories[cat];
        if r[5] > 0 && c.variants.len() < MAX_VARIANTS {
            c.variants.push((bank, program));
        }
    }

    fn category(&self, id: u16) -> Option<usize> {
        match self.lookup.get(id as usize) {
            Some(&c) if c != NO_CATEGORY && id > 0 => Some(c as usize),
            _ => None,
        }
    }

    fn pick(&mut self, cat: usize) -> Option<(usize, u8)> {
        let n = self.categories[cat].variants.len();
        if n == 0 {
            return None;
        }
        let mut i = 0;
        if n > 1 {
            // Never the same variation twice running.
            loop {
                i = self.below(n as u32) as usize;
                if i != self.categories[cat].last {
                    break;
                }
            }
        }
        self.categories[cat].last = i;
        Some(self.categories[cat].variants[i])
    }

    /// `pitch` is relative to the tone's own, `gain` as voice registers
    /// (0x4000 = full scale); `offset` in bytes into the sample.
    fn start(&mut self, cat: usize, gain: [i32; 2], pitch: f64, offset: i32) -> Option<u64> {
        let (bank, program) = self.pick(cat)?;
        let b = &self.banks[bank];
        let tone = (*b.programs.get(program as usize)?)?;
        let body = b.body.clone();
        if self.voices.len() >= MAX_VOICES {
            let quietest = self.voices.iter().enumerate().min_by(|a, b| a.1.loudness().total_cmp(&b.1.loudness()))?.0;
            self.voices.swap_remove(quietest);
        }
        let id = self.next_id;
        self.next_id += 1;
        let base = tone.offset as usize;
        let target = self.scaled(gain);
        self.voices.push(Voice {
            id,
            body,
            addr: base + (offset.max(0) as usize & !15),
            loop_addr: base,
            frame: [0; 28],
            frame_pos: 28,
            p1: 0,
            p2: 0,
            ending: 0,
            tone_pitch: tone.pitch as f64 / PITCH_UNIT,
            pitch: tone.pitch as f64 * pitch / PITCH_UNIT,
            acc: 0.0,
            hist: [0.0; TAPS],
            head: 0,
            env: Envelope::new(tone.adsr1, tone.adsr2),
            gain: target,
            target,
            halted: false,
            reverb: true,
        });
        Some(id)
    }

    /// Output gains for voice register values. Other sounds dip while a
    /// voice-over plays.
    fn scaled(&self, gain: [i32; 2]) -> [f32; 2] {
        let duck = if self.ducking() { DUCK } else { 1.0 };
        gain.map(|g| (g as f32 / 0x4000 as f32) * MASTER * duck)
    }

    /// One chunk of a voice-over.
    pub fn stream_chunk(&mut self, data: &[u8]) {
        if data.len() < STREAM_HEADER {
            return;
        }
        let total = u16_at(data, STREAM_TOTAL);
        let index = u16_at(data, STREAM_INDEX);
        if index == 1 {
            self.stream.data.clear();
        }
        let size = (u32_at(data, 4) as usize).clamp(STREAM_HEADER, data.len());
        self.stream.data.extend_from_slice(&data[STREAM_HEADER..size]);
        self.stream.data.resize(self.stream.data.len().next_multiple_of(STREAM_HEADER), 0);
        self.stream.complete = index >= total;
        self.replies.push(reply(REPLY_STREAM_CHUNK, 0, 0));
    }

    /// Starts a requested voice-over once all of it has arrived, or after a
    /// wait if the rest never comes.
    fn stream_cycle(&mut self) {
        if let Some(id) = self.stream.voice {
            match self.stream_gain() {
                _ if self.voice(id).is_none() => {
                    self.stream.voice = None;
                    self.replies.push(reply(REPLY_STREAM_DONE, self.stream.id, 1));
                }
                Some(gain) => {
                    let target = self.scaled_voice(gain);
                    if let Some(v) = self.voice(id) {
                        v.target = target;
                    }
                }
                None => self.stream_stop(1),
            }
        }
        if self.stream.wait == 0 {
            return;
        }
        self.stream.wait -= 1;
        if !self.stream.complete && self.stream.wait > 0 {
            return;
        }
        self.stream.wait = 0;
        let Some(gain) = self.stream_gain() else {
            self.stream_stop(1);
            return;
        };
        let target = self.scaled_voice(gain);
        let id = self.next_id;
        self.next_id += 1;
        if self.voices.len() >= MAX_VOICES {
            self.voices.swap_remove(0);
        }
        self.voices.push(Voice {
            id,
            body: Arc::new(std::mem::take(&mut self.stream.data)),
            addr: 0,
            loop_addr: 0,
            frame: [0; 28],
            frame_pos: 28,
            p1: 0,
            p2: 0,
            ending: 0,
            tone_pitch: STREAM_PITCH / PITCH_UNIT,
            pitch: STREAM_PITCH / PITCH_UNIT,
            acc: 0.0,
            hist: [0.0; TAPS],
            head: 0,
            env: Envelope::new(STREAM_ADSR.0, STREAM_ADSR.1),
            gain: target,
            target,
            halted: false,
            reverb: false,
        });
        self.stream.voice = Some(id);
    }

    /// The voice-over's level: centred, or placed in the world when the game
    /// named it as the positional stream (op 0x17). None when out of range.
    fn stream_gain(&self) -> Option<[i32; 2]> {
        let v = self.volumes[4] as i32 * 128;
        match self.stream.place {
            Some((id, pos)) if id == self.stream.id => {
                let placed = self.place(&STREAM_PLACE, pos, 1.0);
                if placed.att[0] + placed.att[1] <= 0.0 {
                    return None;
                }
                // The driver doubles a placed voice, then clamps.
                Some(self.gains_with(&placed, v as f64 * 2.0).map(|g| g.clamp(-0x3fff, 0x3fff)))
            }
            _ => Some([v, v]),
        }
    }

    /// Voice registers to output gains for the voice-over, which is not
    /// itself turned down, and is lifted by `VOICE_LIFT`.
    fn scaled_voice(&self, gain: [i32; 2]) -> [f32; 2] {
        gain.map(|g| g as f32 / 0x4000 as f32 * MASTER * VOICE_LIFT)
    }

    /// Whether other sounds and music are turned down for a voice-over.
    pub fn ducking(&self) -> bool {
        self.stream.voice.is_some() && self.volumes[4] > 0
    }

    /// Stops the voice-over; the EE is always told.
    fn stream_stop(&mut self, result: u32) {
        self.stream.wait = 0;
        if let Some(id) = self.stream.voice.take() {
            self.release(id);
        }
        self.replies.push(reply(REPLY_STREAM_DONE, self.stream.id, result));
    }

    /// Replies for the EE's next status block.
    pub fn take_replies(&mut self) -> Vec<[u8; ENTRY]> {
        std::mem::take(&mut self.replies)
    }

    /// Keeps a voice out of the reverb.
    fn dry(&mut self, id: Option<u64>) {
        if let Some(v) = id.and_then(|id| self.voice(id)) {
            v.reverb = false;
        }
    }

    /// Applies the reverb room the game
    /// picked, or turns the reverb off.
    fn apply_reverb(&mut self) {
        let entry = self.reverb_preset.and_then(|p| REVERB_SETS.get(self.reverb_set * 4 + p as usize));
        match entry {
            Some(&(mode, depth, delay, feedback)) if self.reverb_on => {
                // The driver writes depth * 0x3fff >> 16 to the effect volume.
                let level = ((depth as i64 * 0x3fff) >> 16) as f32 / 0x4000 as f32;
                self.reverb.set(mode, level, delay, feedback);
            }
            _ => self.reverb.set(reverb::MODE_OFF, 0.0, 0, 0),
        }
    }

    fn switch_reverb(&mut self, on: bool) {
        self.reverb_on = on;
        self.apply_reverb();
    }

    fn voice(&mut self, id: u64) -> Option<&mut Voice> {
        self.voices.iter_mut().find(|v| v.id == id)
    }

    /// Updates a voice; false if the voice has finished.
    fn update(&mut self, id: u64, gain: [i32; 2], pitch: f64) -> bool {
        let target = self.scaled(gain);
        match self.voice(id) {
            Some(v) => {
                v.target = target;
                v.pitch = v.tone_pitch * pitch;
                true
            }
            None => false,
        }
    }

    fn release(&mut self, id: u64) {
        if let Some(v) = self.voice(id) {
            if v.env.phase != Phase::Off {
                v.env.phase = Phase::Release;
                v.env.counter = 0;
            }
            v.halted = false;
        }
    }

    fn place(&self, cat: &Category, pos: [f64; 3], volume: f64) -> Placed {
        let mut att = [0.0; 2];
        let mut distance = 0.0;
        let mut pan = 0.0;
        for i in (0..(self.players as usize).clamp(1, 2)).rev() {
            let m = &self.mics[i];
            let v = [pos[0] - m.pos[0], pos[1] - m.pos[1], pos[2] - m.pos[2]];
            let d = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            att[i] = if d > cat.far {
                (0.25 - (d - cat.far) * cat.slope_far).max(0.0)
            } else if d > cat.near {
                ((1.0 - (d - cat.near) * cat.slope_near) * volume).max(0.0)
            } else {
                1.0
            };
            if i == 0 {
                distance = d;
                let x = v[0] * m.m[0] + v[1] * m.m[3] + v[2] * m.m[6];
                if d > 2.0 / ONE as f64 {
                    pan = (-x / d).clamp(-1.0, 1.0);
                }
            }
        }
        Placed { att, pan, distance }
    }

    /// Voice registers for a placed sound. The driver's surround mode also
    /// inverts sounds behind the listener; that only suits a Pro Logic
    /// decoder, so it is left out.
    fn gains(&self, p: &Placed) -> [i32; 2] {
        self.gains_with(p, self.volumes[1] as f64 * 128.0)
    }

    /// As `gains`, for a level `v` in voice register units.
    fn gains_with(&self, p: &Placed, v: f64) -> [i32; 2] {
        let (mut l, mut r);
        if self.players == 2 {
            (l, r) = (v * p.att[0], v * p.att[1]);
        } else {
            l = v * p.att[0] * (1.0 - p.pan);
            r = v * p.att[0] * (1.0 + p.pan);
        }
        if self.mono {
            let c = (l + r) / 4.0;
            (l, r) = (c, c);
        } else {
            (l, r) = (l / 2.0, r / 2.0);
        }
        [l as i32, r as i32]
    }

    fn big_stop(&mut self, slot: i8) {
        if slot == -1 {
            for s in 0..BIG_SLOTS as i8 {
                self.big_stop(s);
            }
        } else if (0..BIG_SLOTS as i8).contains(&slot) {
            for id in self.big[slot as usize] {
                if let Some(id) = id {
                    self.release(id);
                }
            }
            self.big[slot as usize] = [None; 2];
        }
    }

    /// Big sound: one sample on two voices, one per side, the
    /// second slightly offset for width.
    fn big_start(&mut self, slot: i8, id: u16, gain: [i32; 2], ratio: f64, pitch: f64, offset: i32) {
        let Some(cat) = self.category(id) else { return };
        if !(0..BIG_SLOTS as i8).contains(&slot) {
            return;
        }
        self.big_stop(slot);
        let a = self.start(cat, [gain[0], 0], pitch, offset.max(0));
        let b = self.start(cat, [0, gain[1]], pitch * ratio, (-offset).max(0));
        self.big[slot as usize] = [a, b];
    }

    fn one_shot(&mut self, id: u16, pos: [f64; 3], volume: f64, pitch: f64, offset: i32) {
        let id = if id == ID_ALIAS.0 { ID_ALIAS.1 } else { id };
        let Some(cat) = self.category(id) else { return };
        let placed = self.place(&self.categories[cat], pos, volume);
        if placed.att[0] + placed.att[1] <= 0.0 {
            return;
        }
        let [mut l, mut r] = self.gains(&placed);
        if IDS_WIDE.contains(&id) {
            let g = if l > 0 { l.max(r) } else { l.min(r) };
            self.big_start(BIG_SLOT_ONE_SHOT, id, [g, g], 0xff7c as f64 / ONE as f64, pitch * 7.0 / 8.0, BIG_OFFSET);
            return;
        }
        if IDS_FADING.contains(&id) {
            let p = placed.att[0].max(placed.att[1]);
            self.big_start(BIG_SLOT_ONE_SHOT, id, [l, r], 1.0, p, BIG_OFFSET);
            return;
        }
        if IDS_DOUBLED.contains(&id) {
            l = (l * 2).clamp(-0x3fff, 0x3fff);
            r = (r * 2).clamp(-0x3fff, 0x3fff);
        }
        self.start(cat, [l, r], pitch, offset);
    }

    fn collision(&mut self, mut id: u16, pos: [f64; 3], strength: i32) {
        let pitch = 1.0 - self.below(0x6000) as f64 / ONE as f64;
        let volume = strength << 10;
        let mut offset = -volume;
        if id == ID_COLLISION_ALT {
            id = ID_COLLISION;
        }
        if id == ID_COLLISION {
            if self.below(100) < 50 {
                id = ID_COLLISION_OTHER;
                offset += 0x2d94;
            } else {
                offset += 0x225e;
            }
        }
        self.one_shot(id, pos, volume as f64 / ONE as f64, pitch, offset.max(0));
    }

    fn menu_sound(&mut self, id: u16, which: i8) {
        let Some(cat) = self.category(id) else { return };
        if which < 0 {
            let g = self.volumes[1] as i32 * 128;
            self.menu_one_shot = self.start(cat, [g, g], 1.0, 0);
            self.dry(self.menu_one_shot);
        } else if (1..=5).contains(&which) {
            if let Some((v, _)) = self.menu_loop.take() {
                self.release(v);
            }
            let g = self.volumes[which as usize] as i32 * 128;
            self.menu_loop = self.start(cat, [g, g], 1.0, 0).map(|v| (v, which as u8));
            self.dry(self.menu_loop.map(|m| m.0));
        }
    }

    fn menu_stop(&mut self) {
        if let Some((v, _)) = self.menu_loop.take() {
            self.release(v);
        }
    }

    fn cockpit(&mut self, id: u16, volume: i32, pitch: i32) {
        let Some(cat) = self.category(id) else { return };
        let volume = if volume == -1 { ONE } else { (volume as i64) << 2 };
        let g = ((self.volumes[1] as i64 * 128 * volume) >> 16) as i32;
        let v = self.start(cat, [g, g], pitch as f64 / ONE as f64, 0);
        self.dry(v);
    }

    fn ambient_command(&mut self, e: &[u8]) {
        let slot = i16::from_le_bytes([e[4], e[5]]);
        if !(0..AMBIENT_SLOTS as i16).contains(&slot) {
            return;
        }
        let slot = slot as usize;
        let pos = vec3(e, 8);
        match e[1] as i8 {
            0 => {
                let id = u16_at(e, 2);
                let cat = self.category(id);
                let mut pitch = fixed(e, 0x18);
                if let Some(c) = cat {
                    let r = self.categories[c].pitch_random;
                    if r != 0.0 {
                        pitch += (self.random() as f64 / u32::MAX as f64) * 2.0 * r - r;
                    }
                }
                let distance = self.place(&Category::default(), pos, 1.0).distance;
                if let Some(a) = self.ambient[slot].take() {
                    if let Some(v) = a.voice {
                        self.release(v);
                    }
                }
                self.ambient[slot] = Some(Ambient { cat, pos, volume: fixed(e, 0x14), pitch, voice: None, distance });
            }
            1 => {
                if let Some(a) = &mut self.ambient[slot] {
                    a.pos = pos;
                    if a.voice.is_some() {
                        a.volume = fixed(e, 0x14);
                        a.pitch = fixed(e, 0x18);
                    }
                }
            }
            2 => {
                if let Some(Some(a)) = self.ambient.get_mut(slot).map(Option::take) {
                    if let Some(v) = a.voice {
                        self.release(v);
                    }
                }
            }
            _ => {}
        }
    }

    fn ambient_stop(&mut self, delete: bool) {
        for slot in 0..AMBIENT_SLOTS {
            let Some(a) = &mut self.ambient[slot] else { continue };
            let voice = a.voice.take();
            if delete {
                self.ambient[slot] = None;
            }
            if let Some(v) = voice {
                self.release(v);
            }
        }
    }

    /// Ambient update: start, stop and move the ambient voices.
    fn ambient_cycle(&mut self) {
        if !self.ambient_on {
            return;
        }
        for slot in 0..AMBIENT_SLOTS {
            let Some(a) = &self.ambient[slot] else { continue };
            let Some(cat) = a.cat else { continue };
            let (pos, volume, base_pitch, voice, previous) = (a.pos, a.volume, a.pitch, a.voice, a.distance);
            let c = &self.categories[cat];
            let doppler = c.doppler;
            let placed = self.place(c, pos, volume);
            let audible = placed.att[0] + placed.att[1] > 0.0;
            let gain = self.gains(&placed);
            let mut pitch = base_pitch;
            if doppler {
                let closing = ((previous - placed.distance) * 32.0).min(1.0);
                pitch *= DOPPLER_K / (DOPPLER_K - closing);
            }
            let voice = match (audible, voice) {
                (false, Some(v)) => {
                    self.release(v);
                    None
                }
                (false, None) => None,
                (true, Some(v)) => {
                    let target = self.scaled(gain);
                    match self.voice(v) {
                        Some(voice) => {
                            voice.target = target;
                            voice.pitch = voice.tone_pitch * pitch;
                            Some(v)
                        }
                        None => None,
                    }
                }
                (true, None) => {
                    let offset = (self.below(0x100) << 4) as i32;
                    self.start(cat, gain, pitch, offset)
                }
            };
            if let Some(a) = &mut self.ambient[slot] {
                a.voice = voice;
                a.distance = placed.distance;
            }
        }
    }

    fn halt(&mut self, except: Option<u64>) {
        for v in &mut self.voices {
            if Some(v.id) != except {
                v.halted = true;
            }
        }
    }

    fn unload(&mut self) {
        self.voices.clear();
        self.banks.clear();
        self.defs.clear();
        self.categories.clear();
        self.keys.clear();
        self.lookup = [NO_CATEGORY; SOUND_IDS];
        self.ambient.iter_mut().for_each(|a| *a = None);
        self.menu_loop = None;
        self.menu_one_shot = None;
        self.big = [[None; 2]; BIG_SLOTS];
    }

    fn set_volume(&mut self, which: u8, value: u8) {
        if let Some(v) = self.volumes.get_mut(which as usize) {
            *v = value.min(0x7f);
        }
    }

    /// The command block of one game frame.
    pub fn block(&mut self, b: &[u8]) {
        self.mono = b[5] == 0;
        self.set_volume(1, b[7]);
        self.set_volume(2, b[9]);
        self.set_volume(3, b[0xb]);
        self.set_volume(4, b[0xc]);
        self.set_volume(5, b[0xe]);
        for (i, r) in b[MICS..].chunks_exact(ENTRY).take(2).enumerate() {
            if r[0x3d] != 0 {
                let mut m = [0.0; 9];
                for (j, v) in m.iter_mut().enumerate() {
                    *v = fixed(r, 0x18 + j * 4);
                }
                self.mics[i] = Mic { pos: vec3(r, 0), m };
            }
        }
        // Room id 0-3 from the microphone; split screen uses the lower.
        let mut room = b[MICS + 0x3c];
        if self.players == 2 && b[MICS + ENTRY + 0x3d] != 0 {
            room = room.min(b[MICS + ENTRY + 0x3c]);
        }
        if b[MICS + 0x3d] != 0 && room < 4 && Some(room) != self.reverb_preset {
            self.reverb_preset = Some(room);
            self.apply_reverb();
        }
        self.bikes_update(b);
        if let Some((v, which)) = self.menu_loop {
            let g = (self.volumes[which as usize] as f32 * 128.0 / 0x4000 as f32) * MASTER;
            if let Some(voice) = self.voice(v) {
                voice.target = [g; 2];
            }
        }
        let n = (u32_at(b, 0) as usize).min(MAX_COMMANDS);
        for e in b[ENTRY..].chunks_exact(ENTRY).take(n) {
            match e[0] {
                OP_SAMPLE => self.one_shot(u16_at(e, 2), vec3(e, 8), fixed(e, 0x14), fixed(e, 0x18), 0),
                OP_COCKPIT => self.cockpit(u16_at(e, 2), i32_at(e, 4), i32_at(e, 8)),
                OP_COLLISION => self.collision(u16_at(e, 2), vec3(e, 4), i32_at(e, 0x10)),
                OP_AMBIENT => self.ambient_command(e),
                OP_AMBIENT_MUTE => {
                    self.ambient_on = e[1] != 0;
                    if !self.ambient_on {
                        self.ambient_stop(false);
                    }
                }
                OP_MENU if e[1] != 0 => self.menu_sound(u16_at(e, 2), e[4] as i8),
                OP_MENU => self.menu_stop(),
                OP_BIG_START => {
                    let gain = [i16::from_le_bytes([e[8], e[9]]) as i32, i16::from_le_bytes([e[0xc], e[0xd]]) as i32];
                    self.big_start(e[1] as i8, u16_at(e, 4), gain, fixed(e, 0x10), fixed(e, 0x14), i32_at(e, 0x18));
                }
                OP_BIG_STOP => self.big_stop(e[1] as i8),
                OP_SET_VOLUME => self.set_volume(e[1], e[2]),
                OP_PLAYERS | OP_PLAYERS_2 => self.players = e[1],
                OP_INIT_MODULE => {
                    self.players = e[3];
                    // Reverb set from the setup entry; set 9 (menus) has no reverb.
                    self.reverb_set = (e[1] as usize).min(REVERB_SETS.len() / 4 - 1);
                    self.reverb_preset = (e[1] == 9).then_some(0);
                    self.apply_reverb();
                    self.bikes_init(e);
                }
                OP_KILL_BIKES => self.bikes_kill(),
                OP_CLOSE_MODULE => {
                    self.stream_stop(0);
                    self.unload();
                }
                OP_STREAM_DONE => self.stream_stop(0),
                OP_STREAM_INIT => {
                    if self.stream.voice.is_some() {
                        self.stream_stop(1);
                    }
                    self.stream.id = u32_at(e, 4);
                }
                OP_STREAM_PLAY => self.stream.wait = STREAM_WAIT,
                OP_STREAM_PLACE => {
                    let id = u32_at(e, 4);
                    self.stream.place = (id != u32::MAX).then(|| (id, vec3(e, 0xc)));
                }
                OP_STREAM_MOVE => {
                    if let Some((id, pos)) = &mut self.stream.place {
                        if *id == u32_at(e, 4) {
                            *pos = vec3(e, 0xc);
                        }
                    }
                }
                OP_RESUME_AUDIO => {
                    self.voices.iter_mut().for_each(|v| v.halted = false);
                    self.switch_reverb(true);
                }
                OP_STOP_AUDIO => {
                    self.switch_reverb(false);
                    self.stream_stop(0);
                    self.bikes_stop();
                    self.ambient_stop(true);
                    self.halt(self.menu_one_shot);
                }
                OP_STOP_AUDIO_2 => {
                    self.switch_reverb(false);
                    self.stream_stop(0);
                    self.halt(self.menu_one_shot);
                }
                OP_SIDE_STOP => {
                    self.switch_reverb(false);
                    self.big_stop(-1);
                    self.bikes_kill();
                    self.ambient_on = false;
                    self.ambient_stop(false);
                }
                OP_STOP_ALL_NORMAL => {
                    self.switch_reverb(false);
                    self.bikes_stop();
                    self.ambient_stop(false);
                    self.halt(None);
                }
                _ => {}
            }
        }
        self.bikes_cycle();
        self.ambient_cycle();
        self.stream_cycle();
    }

    /// Adds one output sample to `out`.
    pub fn mix(&mut self, out: &mut [f32; 2]) {
        let table = std::mem::take(&mut self.table);
        let mut send = [0.0; 2];
        self.voices.retain_mut(|v| v.mix(&table, out, &mut send));
        self.table = table;
        if self.reverb.active() {
            let wet = self.reverb.process(send);
            out[0] += wet[0];
            out[1] += wet[1];
        }
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        (self.banks.len(), self.categories.len(), self.voices.len())
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bank with one looping tone, wired to sound id `id`.
    fn loaded(id: u16, loops: bool) -> Sfx {
        let mut shd = vec![0u8; 0x40];
        shd[..4].copy_from_slice(&0x40u32.to_le_bytes());
        shd[0x1c..0x20].copy_from_slice(TAG_SHD);
        shd[0x20] = 5;
        let mut d = vec![0u8; 12 + DEF_SIZE];
        d[0] = 7;
        d[2] = 1;
        d[4] = 5;
        d[6] = 1;
        d[8] = 12;
        let r = &mut d[12..];
        r[0] = 9;
        r[4] = 1;
        r[5] = 1;
        r[8..10].copy_from_slice(&id.to_le_bytes());
        shd.extend_from_slice(&d);

        let mut vkh = vec![0u8; 0x40];
        vkh[..4].copy_from_slice(&0x40u32.to_le_bytes());
        vkh[0x1c..0x20].copy_from_slice(TAG_VKH);
        vkh[0x20] = 5;
        let mut h = vec![0u8; 0x30];
        h[4] = 1;
        h[0x10] = 0x18;
        h[0x18] = 1;
        h[0x1c + 0xa..0x1c + 0xc].copy_from_slice(&0x000fu16.to_le_bytes()); // fast attack
        h[0x1c + 0xc..0x1c + 0xe].copy_from_slice(&0x1fc0u16.to_le_bytes());
        h[0x1c + 0xe..0x1c + 0x10].copy_from_slice(&0x1000u16.to_le_bytes());
        vkh.extend_from_slice(&h);

        let mut vkb = vec![0u8; 0x40];
        vkb[..4].copy_from_slice(&0x40u32.to_le_bytes());
        vkb[0x1c..0x20].copy_from_slice(TAG_VKB);
        vkb[0x20] = 5;
        for f in 0..4 {
            let mut frame = [0x11u8; 16];
            frame[0] = 2;
            frame[1] = if f == 3 { if loops { 3 } else { 1 } } else { 0 };
            vkb.extend_from_slice(&frame);
        }
        let mut s = Sfx::new();
        s.transfer(0, shd);
        s.transfer(0, vkh);
        s.transfer(0, vkb);
        s.volumes[1] = 0x7f;
        s
    }

    fn run(s: &mut Sfx, n: usize) -> f32 {
        let mut peak = 0f32;
        for _ in 0..n {
            let mut o = [0.0; 2];
            s.mix(&mut o);
            peak = peak.max(o[0].abs());
        }
        peak
    }

    #[test]
    fn bank_definitions_reach_a_sound_id() {
        let s = loaded(0x33, true);
        assert_eq!(s.counts().0, 1);
        assert!(s.category(0x33).is_some());
        assert!(s.category(0x34).is_none());
    }

    #[test]
    fn one_shot_finishes_and_loop_continues() {
        let mut s = loaded(0x33, false);
        s.menu_sound(0x33, -1);
        assert!(run(&mut s, 400) > 0.0);
        run(&mut s, 1000);
        assert_eq!(s.counts().2, 0);

        let mut s = loaded(0x33, true);
        s.menu_sound(0x33, 1);
        run(&mut s, 5000);
        assert_eq!(s.counts().2, 1);
        s.menu_stop();
        run(&mut s, 400_000);
        assert_eq!(s.counts().2, 0);
    }

    #[test]
    fn halted_voices_keep_their_place() {
        let mut s = loaded(0x33, true);
        s.menu_sound(0x33, 1);
        run(&mut s, 100);
        s.halt(None);
        assert_eq!(run(&mut s, 100), 0.0);
        assert_eq!(s.counts().2, 1);
    }

    #[test]
    fn distance_and_pan_follow_the_driver() {
        let mut s = loaded(0x33, true);
        s.mics[0].m = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let c = Category { near: 1.0, far: 3.0, slope_near: 0.25, slope_far: 0.1, ..Category::default() };
        assert_eq!(s.place(&c, [0.5, 0.0, 0.0], 0.5).att[0], 1.0);
        assert_eq!(s.place(&c, [2.0, 0.0, 0.0], 0.5).att[0], 0.375);
        assert!((s.place(&c, [5.0, 0.0, 0.0], 1.0).att[0] - 0.05).abs() < 1e-9);
        assert_eq!(s.place(&c, [2.0, 0.0, 0.0], 1.0).pan, -1.0);
    }

    #[test]
    fn voice_over_waits_for_its_last_chunk_and_reports_the_end() {
        let chunk = |index: u16| {
            let mut d = vec![0u8; 0x40];
            d[4..8].copy_from_slice(&0x60u32.to_le_bytes());
            d[STREAM_TOTAL] = 2;
            d[STREAM_INDEX..STREAM_INDEX + 2].copy_from_slice(&index.to_le_bytes());
            let mut frame = [0x11u8; 16];
            frame[0] = 2;
            d.extend_from_slice(&frame);
            let mut last = frame;
            last[1] = if index == 2 { 1 } else { 0 };
            d.extend_from_slice(&last);
            d.extend_from_slice(&[0; 0x40]);
            d
        };
        let mut s = Sfx::new();
        s.volumes[4] = 0x7f;
        s.stream.id = 0x8b;
        s.stream_chunk(&chunk(1));
        s.stream.wait = STREAM_WAIT;
        s.stream_cycle();
        assert!(s.stream.voice.is_none());
        s.stream_chunk(&chunk(2));
        s.stream_cycle();
        assert!(s.stream.voice.is_some());
        assert!(run(&mut s, 200) > 0.0);
        s.replies.clear();
        run(&mut s, 2000);
        s.stream_cycle();
        assert!(s.stream.voice.is_none());
        assert_eq!(s.replies.len(), 1);
        assert_eq!(s.replies[0][0], REPLY_STREAM_DONE);
        assert_eq!(s.replies[0][4], 0x8b);
        assert_eq!(s.replies[0][8], 1);
    }

    #[test]
    fn envelope_stops_after_release() {
        let mut e = Envelope::new(0x000f, 0x1fc0);
        for _ in 0..10_000 {
            e.step();
        }
        assert_eq!(e.phase, Phase::Sustain);
        e.phase = Phase::Release;
        for _ in 0..1_000 {
            e.step();
        }
        assert_eq!(e.phase, Phase::Off);
        // The race engines' envelope (release rate 10, linear) fades in
        // well under a second.
        let mut e = Envelope::new(0x80ff, 0x5fca);
        e.level = 0x7fff;
        e.phase = Phase::Release;
        let mut n = 0;
        while e.phase != Phase::Off {
            e.step();
            n += 1;
        }
        assert!(n < 48_000, "{n}");
    }
}
