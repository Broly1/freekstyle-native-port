//! Controller input: a native padman. The game's libpad gives padman a
//! 256-byte buffer per port (scePadPortOpen); padman keeps two 128-byte
//! frames there and libpad reads whichever has the newer counter. We write
//! those frames on every vblank from the host input state.
//!
//! Frame layout as read by this game's libpad:
//!   +0   32 bytes of pad data (scePadRead)
//!   +48  actuator descriptors      +80  mode table (u16 types x 4)
//!   +88  frame counter             +96  data length
//!   +100 mode config size          +101 current mode ID
//!   +103 data ready                +104 number of modes
//!   +105 current mode index        +106 number of actuators
//!   +112 state (6 = stable)        +113 request state (0 = complete)
//!   +114 extended info available

use std::sync::Mutex;

/// PS2 button bits in pad data bytes 2-3 (stored active-low there).
pub mod button {
    pub const SELECT: u16 = 1 << 0;
    pub const L3: u16 = 1 << 1;
    pub const R3: u16 = 1 << 2;
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const L2: u16 = 1 << 8;
    pub const R2: u16 = 1 << 9;
    pub const L1: u16 = 1 << 10;
    pub const R1: u16 = 1 << 11;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CIRCLE: u16 = 1 << 13;
    pub const CROSS: u16 = 1 << 14;
    pub const SQUARE: u16 = 1 << 15;
}

/// Host controller state, in PS2 terms.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    /// Pressed buttons (`button::*`), active-high.
    pub buttons: u16,
    /// Right X, right Y, left X, left Y; 0x80 is centred, 0 is up/left.
    pub sticks: [u8; 4],
    /// Analog pressure for L2/R2 (from triggers); others are 0 or 255.
    pub l2: u8,
    pub r2: u8,
}

impl Default for Input {
    fn default() -> Self {
        Input { buttons: 0, sticks: [0x80; 4], l2: 0, r2: 0 }
    }
}

/// Written by the window thread, read by the game on each vblank.
pub static INPUT: Mutex<Input> = Mutex::new(Input { buttons: 0, sticks: [0x80; 4], l2: 0, r2: 0 });

pub fn set_input(i: Input) {
    *INPUT.lock().unwrap() = i;
}

const MODE_DIGITAL: u8 = 0x41;
const MODE_ANALOG: u8 = 0x73;
const STATE_STABLE: u8 = 6;
const STATE_DISCONNECTED: u8 = 0;

#[derive(Clone, Copy)]
struct Port {
    port: u32,
    slot: u32,
    buffer: u32,
}

#[derive(Default)]
pub struct Pad {
    ports: Vec<Port>,
    counter: u32,
    analog: bool,
    /// Buttons at the last FREEK_PAD_LOG report.
    logged: u16,
    /// FREEK_PAD_SCRIPT taps: (first frame, last frame, buttons), for
    /// automated testing.
    script: Option<Vec<(u32, u32, u16)>>,
}

/// Parse "600:a,900:start+up,1200-1800:a" into (first, last frame,
/// buttons): a tap holds for 6 frames, a range for its whole length. Button
/// names use the Xbox layout (a b x y lb rb lt rt back start up down left
/// right).
fn parse_script(s: &str) -> Vec<(u32, u32, u16)> {
    use button::*;
    s.split(',')
        .filter_map(|tap| {
            let (frame, names) = tap.split_once(':')?;
            let mut b = 0;
            for n in names.split('+') {
                b |= match n.trim() {
                    "a" => CROSS,
                    "b" => CIRCLE,
                    "x" => SQUARE,
                    "y" => TRIANGLE,
                    "lb" => L1,
                    "rb" => R1,
                    "lt" => L2,
                    "rt" => R2,
                    "back" => SELECT,
                    "start" => START,
                    "up" => UP,
                    "down" => DOWN,
                    "left" => LEFT,
                    "right" => RIGHT,
                    _ => 0,
                };
            }
            let (first, last) = match frame.split_once('-') {
                Some((a, z)) => (a.trim().parse().ok()?, z.trim().parse().ok()?),
                None => {
                    let f: u32 = frame.trim().parse().ok()?;
                    (f, f + 5)
                }
            };
            Some((first, last, b))
        })
        .collect()
}

impl Pad {
    /// padman RPC (server 0x8000_0100/0101). The request is a 128-byte
    /// block with the command in word 0; replies reuse it, with results at
    /// +12 (open/version/close) and +20 (mode and actuator setters).
    pub fn rpc(&mut self, request: &[u8], rsize: u32) -> Vec<u8> {
        let word = |o: usize| request.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
        let mut reply = request.to_vec();
        reply.resize(rsize.max(24) as usize, 0);
        let mut put = |o: usize, v: u32| reply[o..o + 4].copy_from_slice(&v.to_le_bytes());
        match word(0) {
            0x01 => {
                let p = Port { port: word(4), slot: word(8), buffer: word(16) };
                self.ports.retain(|q| (q.port, q.slot) != (p.port, p.slot));
                self.ports.push(p);
                put(12, 1);
            }
            0x06 => {
                // SetMainMode(mode, lock): 1 = analog.
                self.analog = word(12) == 1;
                put(20, 1);
            }
            0x12 => put(12, 0x0400), // padman 4.0
            0x0e => {
                let (port, slot) = (word(4), word(8));
                self.ports.retain(|q| (q.port, q.slot) != (port, slot));
                put(12, 1);
            }
            _ => {
                put(12, 1);
                put(20, 1);
            }
        }
        reply.truncate(rsize as usize);
        reply
    }

    /// Fill one frame for every open port with the current input.
    /// `vblank` numbers FREEK_PAD_LOG's button reports, the same count
    /// that F12 recordings start from.
    pub fn vblank(&mut self, vblank: u64, write: &mut dyn FnMut(u32, &[u8])) {
        if self.ports.is_empty() {
            return;
        }
        self.counter = self.counter.wrapping_add(1);
        let mut input = *INPUT.lock().unwrap();
        let script = self.script.get_or_insert_with(|| std::env::var("FREEK_PAD_SCRIPT").map(|s| parse_script(&s)).unwrap_or_default());
        for &(first, last, b) in script.iter() {
            if (first..=last).contains(&self.counter) {
                input.buttons |= b;
            }
        }
        if input.buttons != self.logged && std::env::var_os("FREEK_PAD_LOG").is_some() {
            eprintln!("[pad] vblank {vblank}: buttons {:#06x} (pressed {:#06x})", input.buttons, input.buttons & !self.logged);
            self.logged = input.buttons;
        }
        for p in &self.ports {
            let connected = p.port == 0 && p.slot == 0;
            let mode = if self.analog { MODE_ANALOG } else { MODE_DIGITAL };
            let mut f = [0u8; 128];
            if connected {
                let b = !input.buttons;
                f[1] = mode;
                f[2] = b as u8;
                f[3] = (b >> 8) as u8;
                f[4..8].copy_from_slice(&input.sticks);
                // Pressure: right, left, up, down, triangle, circle, cross,
                // square, L1, R1, L2, R2.
                use button::*;
                let pressed = |m: u16| if input.buttons & m != 0 { 255 } else { 0 };
                let order = [RIGHT, LEFT, UP, DOWN, TRIANGLE, CIRCLE, CROSS, SQUARE, L1, R1];
                for (n, m) in order.iter().enumerate() {
                    f[8 + n] = pressed(*m);
                }
                f[18] = input.l2.max(pressed(L2));
                f[19] = input.r2.max(pressed(R2));
            }
            // Two actuators (small and large motor), DualShock 2 style.
            f[48..56].copy_from_slice(&[0, 1, 0, 1, 1, 1, 0x14, 0]);
            // Mode table holds controller types (4 digital, 7 DualShock),
            // which scePadInfoMode returns and games compare against.
            f[80..84].copy_from_slice(&[4, 0, 7, 0]);
            f[88..92].copy_from_slice(&self.counter.to_le_bytes());
            f[96..100].copy_from_slice(&32u32.to_le_bytes());
            f[100] = 2;
            f[101] = mode;
            f[102] = 3;
            f[103] = 1;
            f[104] = 2;
            f[105] = u8::from(self.analog);
            f[106] = 2;
            f[107] = 1;
            f[112] = if connected { STATE_STABLE } else { STATE_DISCONNECTED };
            f[113] = 0;
            f[114] = 1;
            // Overwrite the older of the two frames.
            if self.counter < 3 && std::env::var_os("FREEK_PAD_LOG").is_some() {
                eprintln!("[pad] frame {} port {} slot {} -> {:#x}", self.counter, p.port, p.slot, p.buffer + (self.counter & 1) * 128);
            }
            write(p.buffer + (self.counter & 1) * 128, &f);
        }
    }
}
