//! Unified controller input and force-feedback built on SDL3.
//!
//! Used by both `freekstyle` and `launcher` so controller detection,
//! `gamecontrollerdb.txt` mappings, deadzone scaling, raw-joystick fallback,
//! and rumble work identically in both binaries.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ee_runtime::pad::{button, Input};
use freek_config::Binding;
use sdl3::event::Event;
use sdl3::gamepad::{Axis, Button, Gamepad};
use sdl3::iostream::IOStream;
use sdl3::joystick::{HatState, Joystick, JoystickId, JoystickType};
use sdl3::{EventPump, GamepadSubsystem, JoystickSubsystem, Sdl};

const RUMBLE_PERIOD: Duration = Duration::from_millis(50);
const RUMBLE_REFRESH: Duration = Duration::from_millis(500);
const RUMBLE_DURATION_MS: u32 = 2_000;
const TRIGGER_PRESS_THRESHOLD: i16 = 8_192;
const JOY_AXIS_DIGITAL_THRESHOLD: i16 = 16_384;

/// PS2 DualShock 2 button names (matching `freek_config::PS2_BUTTONS`) and bits.
pub const PAD_BUTTONS: &[(&str, u16)] = &[
    ("cross", button::CROSS),
    ("circle", button::CIRCLE),
    ("square", button::SQUARE),
    ("triangle", button::TRIANGLE),
    ("l1", button::L1),
    ("r1", button::R1),
    ("l2", button::L2),
    ("r2", button::R2),
    ("l3", button::L3),
    ("r3", button::R3),
    ("start", button::START),
    ("select", button::SELECT),
    ("up", button::UP),
    ("down", button::DOWN),
    ("left", button::LEFT),
    ("right", button::RIGHT),
];

/// Fallback button layout when an unmapped raw joystick has no `Joy*` bindings.
const RAW_FALLBACK_BUTTONS: &[u16] = &[
    button::CROSS,
    button::CIRCLE,
    button::SQUARE,
    button::TRIANGLE,
    button::L1,
    button::R1,
    button::L2,
    button::R2,
    button::SELECT,
    button::START,
    button::L3,
    button::R3,
];

/// Cardinal direction on a raw joystick hat switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HatDir {
    Up,
    Down,
    Left,
    Right,
}

impl HatDir {
    fn as_str(self) -> &'static str {
        match self {
            HatDir::Up => "Up",
            HatDir::Down => "Down",
            HatDir::Left => "Left",
            HatDir::Right => "Right",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "Up" => Some(HatDir::Up),
            "Down" => Some(HatDir::Down),
            "Left" => Some(HatDir::Left),
            "Right" => Some(HatDir::Right),
            _ => None,
        }
    }

    fn matches(self, state: HatState) -> bool {
        match self {
            HatDir::Up => matches!(state, HatState::Up | HatState::LeftUp | HatState::RightUp),
            HatDir::Down => {
                matches!(state, HatState::Down | HatState::LeftDown | HatState::RightDown)
            }
            HatDir::Left => matches!(state, HatState::Left | HatState::LeftUp | HatState::LeftDown),
            HatDir::Right => {
                matches!(state, HatState::Right | HatState::RightUp | HatState::RightDown)
            }
        }
    }

    fn first_from_state(state: HatState) -> Option<Self> {
        match state {
            HatState::Up | HatState::LeftUp | HatState::RightUp => Some(HatDir::Up),
            HatState::Down | HatState::LeftDown | HatState::RightDown => Some(HatDir::Down),
            HatState::Left => Some(HatDir::Left),
            HatState::Right => Some(HatDir::Right),
            HatState::Centered => None,
        }
    }
}

/// A parsed controller binding target supporting SDL gamepads and raw joysticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadInput {
    GamepadButton(Button),
    GamepadTrigger(Axis),
    JoyButton(u8),
    JoyHat(u8, HatDir),
    JoyAxis(u8, bool),
}

/// Precompiled mapping from physical controller inputs to PS2 button bitmasks.
#[derive(Clone, Debug, Default)]
pub struct PadMap {
    entries: Vec<(PadInput, u16)>,
    has_raw_joy_bindings: bool,
}

impl PadMap {
    /// Compile configured bindings into fast controller-lookup entries.
    pub fn compile(bindings: &[Binding]) -> Self {
        let mut entries = Vec::new();
        let mut has_raw_joy_bindings = false;
        for binding in bindings {
            let Some(&(_, bit)) = PAD_BUTTONS.iter().find(|&&(name, _)| name == binding.button)
            else {
                continue;
            };
            for name in &binding.pad {
                match parse_pad_input(name) {
                    Some(input) => {
                        if matches!(
                            input,
                            PadInput::JoyButton(_) | PadInput::JoyHat(_, _) | PadInput::JoyAxis(_, _)
                        ) {
                            has_raw_joy_bindings = true;
                        }
                        entries.push((input, bit));
                    }
                    None if !name.is_empty() => {
                        eprintln!("[input] unknown gamepad button {name:?}");
                    }
                    None => {}
                }
            }
        }
        Self {
            entries,
            has_raw_joy_bindings,
        }
    }
}

/// Canonical string name stored in `freekstyle.toml` for an SDL gamepad button.
///
/// Preserves existing gilrs names (`"South"`, `"LeftTrigger"`, `"Select"`, etc.)
/// so existing config files remain valid without migration.
pub fn canonical_button_name(button: Button) -> &'static str {
    match button {
        Button::South => "South",
        Button::East => "East",
        Button::West => "West",
        Button::North => "North",
        Button::LeftShoulder => "LeftTrigger",
        Button::RightShoulder => "RightTrigger",
        Button::LeftStick => "LeftThumb",
        Button::RightStick => "RightThumb",
        Button::Start => "Start",
        Button::Back => "Select",
        Button::Guide => "Mode",
        Button::DPadUp => "DPadUp",
        Button::DPadDown => "DPadDown",
        Button::DPadLeft => "DPadLeft",
        Button::DPadRight => "DPadRight",
        Button::Touchpad => "Touchpad",
        Button::Misc1 => "Misc1",
        Button::Misc2 => "Misc2",
        Button::Misc3 => "Misc3",
        Button::Misc4 => "Misc4",
        Button::Misc5 => "Misc5",
        Button::Misc6 => "Misc6",
        Button::LeftPaddle1 => "LeftPaddle1",
        Button::LeftPaddle2 => "LeftPaddle2",
        Button::RightPaddle1 => "RightPaddle1",
        Button::RightPaddle2 => "RightPaddle2",
    }
}

/// Parse a binding string from `freekstyle.toml` into a `PadInput`.
pub fn parse_pad_input(name: &str) -> Option<PadInput> {
    let input = match name {
        "South" => PadInput::GamepadButton(Button::South),
        "East" => PadInput::GamepadButton(Button::East),
        "West" => PadInput::GamepadButton(Button::West),
        "North" => PadInput::GamepadButton(Button::North),
        "LeftTrigger" | "LeftShoulder" => PadInput::GamepadButton(Button::LeftShoulder),
        "RightTrigger" | "RightShoulder" => PadInput::GamepadButton(Button::RightShoulder),
        "LeftTrigger2" => PadInput::GamepadTrigger(Axis::TriggerLeft),
        "RightTrigger2" => PadInput::GamepadTrigger(Axis::TriggerRight),
        "LeftThumb" | "LeftStick" => PadInput::GamepadButton(Button::LeftStick),
        "RightThumb" | "RightStick" => PadInput::GamepadButton(Button::RightStick),
        "Start" => PadInput::GamepadButton(Button::Start),
        "Select" | "Back" => PadInput::GamepadButton(Button::Back),
        "Mode" | "Guide" => PadInput::GamepadButton(Button::Guide),
        "DPadUp" => PadInput::GamepadButton(Button::DPadUp),
        "DPadDown" => PadInput::GamepadButton(Button::DPadDown),
        "DPadLeft" => PadInput::GamepadButton(Button::DPadLeft),
        "DPadRight" => PadInput::GamepadButton(Button::DPadRight),
        "Touchpad" => PadInput::GamepadButton(Button::Touchpad),
        "Misc1" => PadInput::GamepadButton(Button::Misc1),
        "Misc2" => PadInput::GamepadButton(Button::Misc2),
        "Misc3" => PadInput::GamepadButton(Button::Misc3),
        "Misc4" => PadInput::GamepadButton(Button::Misc4),
        "Misc5" => PadInput::GamepadButton(Button::Misc5),
        "Misc6" => PadInput::GamepadButton(Button::Misc6),
        "LeftPaddle1" => PadInput::GamepadButton(Button::LeftPaddle1),
        "LeftPaddle2" => PadInput::GamepadButton(Button::LeftPaddle2),
        "RightPaddle1" => PadInput::GamepadButton(Button::RightPaddle1),
        "RightPaddle2" => PadInput::GamepadButton(Button::RightPaddle2),
        _ => return parse_raw_joy_input(name),
    };
    Some(input)
}

fn parse_raw_joy_input(name: &str) -> Option<PadInput> {
    if let Some(rest) = name.strip_prefix("JoyButton") {
        let idx: u8 = rest.parse().ok()?;
        return Some(PadInput::JoyButton(idx));
    }
    if let Some(rest) = name.strip_prefix("JoyHat") {
        for dir in [HatDir::Up, HatDir::Down, HatDir::Left, HatDir::Right] {
            if let Some(idx_str) = rest.strip_suffix(dir.as_str()) {
                let idx: u8 = idx_str.parse().ok()?;
                return Some(PadInput::JoyHat(idx, dir));
            }
        }
        return None;
    }
    if let Some(rest) = name.strip_prefix("JoyAxis") {
        if let Some(idx_str) = rest.strip_suffix('+') {
            let idx: u8 = idx_str.parse().ok()?;
            return Some(PadInput::JoyAxis(idx, true));
        }
        if let Some(idx_str) = rest.strip_suffix('-') {
            let idx: u8 = idx_str.parse().ok()?;
            return Some(PadInput::JoyAxis(idx, false));
        }
    }
    None
}

enum DeviceKind {
    Gamepad(Gamepad),
    RawJoystick { joy: Joystick, rest_axes: Vec<i16> },
}

struct DeviceState {
    kind: DeviceKind,
    left_trigger_down: bool,
    right_trigger_down: bool,
}

impl DeviceState {
    fn name(&self) -> String {
        match &self.kind {
            DeviceKind::Gamepad(gp) => gp.name().unwrap_or_else(|| "Gamepad".to_string()),
            DeviceKind::RawJoystick { joy, .. } => {
                let n = joy.name();
                if n.is_empty() {
                    format!("Controller ({})", joy.guid())
                } else {
                    n
                }
            }
        }
    }

    fn connected(&self) -> bool {
        match &self.kind {
            DeviceKind::Gamepad(gp) => gp.connected(),
            DeviceKind::RawJoystick { joy, .. } => joy.connected(),
        }
    }

    fn is_gamepad(&self) -> bool {
        matches!(self.kind, DeviceKind::Gamepad(_))
    }

    fn set_rumble(&mut self, low_freq: u16, high_freq: u16, duration_ms: u32) {
        match &mut self.kind {
            DeviceKind::Gamepad(gp) => {
                let _ = gp.set_rumble(low_freq, high_freq, duration_ms);
            }
            DeviceKind::RawJoystick { joy, .. } => {
                let _ = joy.set_rumble(low_freq, high_freq, duration_ms);
            }
        }
    }
}

/// Manages SDL3 gamepads, raw joystick fallbacks, hotplugging, and rumble.
///
/// Must be constructed and polled on the main thread alongside the window loop.
pub struct ControllerManager {
    // Field drop order matters: devices and SDL subsystems drop before `Sdl`.
    devices: HashMap<JoystickId, DeviceState>,
    players: [Option<JoystickId>; 2],
    rumble: [(u16, u16); 2],
    rumble_at: [Instant; 2],
    gamepad: GamepadSubsystem,
    joystick: JoystickSubsystem,
    events: EventPump,
    _sdl: Sdl,
}

impl ControllerManager {
    /// Initialize SDL3 controller subsystems and load built-in + external DBs.
    pub fn new() -> Result<Self, String> {
        // Required on Windows when another library (winit/eframe) owns the
        // window and SDL_INIT_VIDEO is not used.
        sdl3::hint::set("SDL_JOYSTICK_THREAD", "1");
        sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
        sdl3::hint::set("SDL_JOYSTICK_HIDAPI", "1");
        sdl3::hint::set("SDL_JOYSTICK_HIDAPI_COMBINE_JOY_CONS", "1");

        let sdl = sdl3::init().map_err(|e| format!("SDL3 init failed: {e}"))?;
        let gamepad = sdl.gamepad().map_err(|e| format!("SDL3 gamepad failed: {e}"))?;
        let joystick = sdl.joystick().map_err(|e| format!("SDL3 joystick failed: {e}"))?;
        let events = sdl.event_pump().map_err(|e| format!("SDL3 event pump failed: {e}"))?;

        if let Ok(rw) = IOStream::from_bytes(freek_config::GAMEPAD_MAPPINGS.as_bytes()) {
            match gamepad.load_mappings_from_rw(rw) {
                Ok(n) if n > 0 => eprintln!("[input] loaded {n} bundled controller mappings"),
                Ok(_) => {}
                Err(e) => eprintln!("[input] bundled controller mappings error: {e}"),
            }
        }

        for path in freek_config::external_gamepad_db_paths() {
            match gamepad.load_mappings(&path) {
                Ok(n) => {
                    eprintln!("[input] loaded {n} mappings from {}", path.display());
                }
                Err(e) => {
                    eprintln!("[input] failed to load {}: {e}", path.display());
                }
            }
        }

        let now = Instant::now();
        let mut mgr = Self {
            devices: HashMap::new(),
            players: [None, None],
            rumble: [(0, 0), (0, 0)],
            rumble_at: [now; 2],
            gamepad,
            joystick,
            events,
            _sdl: sdl,
        };

        if let Ok(ids) = mgr.joystick.joysticks() {
            for id in ids {
                mgr.add_device(id);
            }
        }

        Ok(mgr)
    }

    /// Process pending SDL controller/joystick events and update connected seats.
    ///
    /// Returns the canonical input name of the most recent newly pressed
    /// button, trigger, hat direction, or digital axis pull (for launcher
    /// rebinding).
    pub fn poll_events(&mut self) -> Option<String> {
        let pending: Vec<Event> = self.events.poll_iter().collect();
        let mut captured = None;

        for event in pending {
            match event {
                Event::GamepadAdded { which, .. } | Event::GamepadRemapped { which, .. } => {
                    self.add_device(which);
                }
                Event::GamepadRemoved { which, .. } | Event::JoyDeviceRemoved { which, .. } => {
                    self.remove_device(which);
                }
                Event::JoyDeviceAdded { which, .. } => {
                    self.add_device(which);
                }
                Event::GamepadButtonDown { button, .. } => {
                    captured = Some(canonical_button_name(button).to_string());
                }
                Event::GamepadAxisMotion {
                    which, axis, value, ..
                } => {
                    if let Some(dev) = self.devices.get_mut(&which) {
                        let is_down = value >= TRIGGER_PRESS_THRESHOLD;
                        match axis {
                            Axis::TriggerLeft => {
                                if is_down && !dev.left_trigger_down {
                                    captured = Some("LeftTrigger2".to_string());
                                }
                                dev.left_trigger_down = is_down;
                            }
                            Axis::TriggerRight => {
                                if is_down && !dev.right_trigger_down {
                                    captured = Some("RightTrigger2".to_string());
                                }
                                dev.right_trigger_down = is_down;
                            }
                            _ => {}
                        }
                    }
                }
                Event::JoyButtonDown {
                    which, button_idx, ..
                } => {
                    if self.devices.get(&which).is_some_and(|d| !d.is_gamepad()) {
                        captured = Some(format!("JoyButton{button_idx}"));
                    }
                }
                Event::JoyHatMotion {
                    which,
                    hat_idx,
                    state,
                    ..
                } => {
                    if self.devices.get(&which).is_some_and(|d| !d.is_gamepad()) {
                        if let Some(dir) = HatDir::first_from_state(state) {
                            captured = Some(format!("JoyHat{hat_idx}{}", dir.as_str()));
                        }
                    }
                }
                Event::JoyAxisMotion {
                    which,
                    axis_idx,
                    value,
                    ..
                } => {
                    if let Some(dev) = self.devices.get(&which) {
                        if let DeviceKind::RawJoystick { rest_axes, .. } = &dev.kind {
                            let rest = rest_axes.get(axis_idx as usize).copied().unwrap_or(0);
                            let delta = value as i32 - rest as i32;
                            if delta.abs() > JOY_AXIS_DIGITAL_THRESHOLD as i32 {
                                let sign = if delta > 0 { '+' } else { '-' };
                                captured = Some(format!("JoyAxis{axis_idx}{sign}"));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        captured
    }

    /// Name of the player 1 (or first connected) controller, for UI status.
    pub fn primary_controller_name(&self) -> Option<String> {
        self.players[0]
            .or(self.players[1])
            .and_then(|id| self.devices.get(&id))
            .filter(|d| d.connected())
            .map(DeviceState::name)
    }

    /// Read both PS2 controller ports.
    ///
    /// Returns `None` for an unoccupied port. When `!focused`, connected ports
    /// return neutral state so the emulated game does not treat Alt-Tab as a
    /// physical unplug.
    pub fn read_players(&self, pad_map: &PadMap, deadzone: f32, focused: bool) -> [Option<Input>; 2] {
        let mut out = [None, None];
        for (slot, seat) in self.players.iter().enumerate() {
            let Some(id) = seat else { continue };
            let Some(dev) = self.devices.get(id) else {
                continue;
            };
            if !dev.connected() {
                continue;
            }
            if !focused {
                out[slot] = Some(Input::default());
                continue;
            }
            out[slot] = Some(read_device_input(dev, pad_map, deadzone));
        }
        out
    }

    /// Apply PS2 motor speeds `[(small, large); 2]` to seated controllers.
    pub fn set_rumble(&mut self, motors: [(u8, u8); 2], focused: bool) {
        let now = Instant::now();
        for (slot, (small, large)) in motors.into_iter().enumerate() {
            let target = if focused {
                rumble_strength((small, large))
            } else {
                (0, 0)
            };
            let prev = self.rumble[slot];
            let needs_refresh =
                target != (0, 0) && now.duration_since(self.rumble_at[slot]) >= RUMBLE_REFRESH;

            if target == prev && !needs_refresh {
                continue;
            }
            if target != (0, 0)
                && prev != (0, 0)
                && !needs_refresh
                && now.duration_since(self.rumble_at[slot]) < RUMBLE_PERIOD
            {
                continue;
            }

            self.rumble[slot] = target;
            self.rumble_at[slot] = now;

            if let Some(id) = self.players[slot] {
                if let Some(dev) = self.devices.get_mut(&id) {
                    // SDL3 takes (low_frequency = heavy motor, high_frequency = light motor).
                    let duration = if target == (0, 0) { 0 } else { RUMBLE_DURATION_MS };
                    dev.set_rumble(target.1, target.0, duration);
                }
            }
        }
    }

    fn add_device(&mut self, id: JoystickId) {
        if self.gamepad.is_gamepad(id) {
            match self.gamepad.open(id) {
                Ok(gp) => {
                    let name = gp.name().unwrap_or_else(|| "Gamepad".to_string());
                    let already_seated = self.devices.contains_key(&id);
                    self.devices.insert(
                        id,
                        DeviceState {
                            kind: DeviceKind::Gamepad(gp),
                            left_trigger_down: false,
                            right_trigger_down: false,
                        },
                    );
                    if already_seated {
                        eprintln!("[input] controller remapped as SDL gamepad: {name}");
                    } else {
                        self.seat(id, &name, "gamepad");
                    }
                }
                Err(e) => eprintln!("[input] failed to open gamepad {id}: {e}"),
            }
            return;
        }

        if self.devices.contains_key(&id) {
            return;
        }

        match self.joystick.open(id) {
            Ok(joy) => {
                if !is_plausible_raw_joystick(&joy, id) {
                    return;
                }
                let rest_axes: Vec<i16> = (0..joy.num_axes())
                    .map(|idx| joy.axis(idx).unwrap_or(0))
                    .collect();
                let guid = joy.guid().to_string();
                let name = if joy.name().is_empty() {
                    format!("Controller ({guid})")
                } else {
                    joy.name()
                };
                self.devices.insert(
                    id,
                    DeviceState {
                        kind: DeviceKind::RawJoystick { joy, rest_axes },
                        left_trigger_down: false,
                        right_trigger_down: false,
                    },
                );
                eprintln!("[input] unmapped controller GUID: {guid} (using raw joystick fallback)");
                self.seat(id, &name, "joystick");
            }
            Err(e) => eprintln!("[input] failed to open joystick {id}: {e}"),
        }
    }

    fn remove_device(&mut self, id: JoystickId) {
        let Some(dev) = self.devices.remove(&id) else {
            return;
        };
        let name = dev.name();
        if let Some(slot) = self.players.iter().position(|s| *s == Some(id)) {
            self.players[slot] = None;
            self.rumble[slot] = (0, 0);
            eprintln!("[input] player {} controller disconnected: {name}", slot + 1);
        }
    }

    fn seat(&mut self, id: JoystickId, name: &str, kind: &str) {
        if self.players.contains(&Some(id)) {
            return;
        }
        let Some(slot) = self.players.iter().position(Option::is_none) else {
            return;
        };
        self.players[slot] = Some(id);
        self.rumble[slot] = (0, 0);
        eprintln!("[input] player {} controller ({kind}): {name}", slot + 1);
    }
}

fn is_plausible_raw_joystick(joy: &Joystick, id: JoystickId) -> bool {
    // Filter non-gamepad HID devices (e.g. keyboards, sensors, mice) that
    // expose a stray RawInput/DirectInput node without controller controls.
    let kind = JoystickType::from_ll(unsafe {
        sdl3::sys::joystick::SDL_GetJoystickTypeForID(id.into())
    });
    match kind {
        JoystickType::Gamepad
        | JoystickType::ArcadeStick
        | JoystickType::ArcadePad
        | JoystickType::DancePad
        | JoystickType::FlightStick
        | JoystickType::Wheel => true,
        _ => joy.num_buttons() >= 4 || joy.num_hats() > 0,
    }
}

fn read_device_input(dev: &DeviceState, pad_map: &PadMap, deadzone: f32) -> Input {
    match &dev.kind {
        DeviceKind::Gamepad(gp) => read_gamepad_input(gp, pad_map, deadzone),
        DeviceKind::RawJoystick { joy, rest_axes } => {
            read_raw_joystick_input(joy, rest_axes, pad_map, deadzone)
        }
    }
}

fn read_gamepad_input(gp: &Gamepad, pad_map: &PadMap, deadzone: f32) -> Input {
    let mut buttons = 0u16;
    let mut l2 = 0u8;
    let mut r2 = 0u8;

    for &(input, bit) in &pad_map.entries {
        let (pressed, pressure) = match input {
            PadInput::GamepadButton(b) => {
                let p = gp.button(b);
                (p, if p { 0xff } else { 0 })
            }
            PadInput::GamepadTrigger(axis) => {
                let raw = gp.axis(axis);
                (raw >= TRIGGER_PRESS_THRESHOLD, trigger_to_u8(raw))
            }
            PadInput::JoyButton(_) | PadInput::JoyHat(_, _) | PadInput::JoyAxis(_, _) => (false, 0),
        };
        if pressed {
            buttons |= bit;
        }
        if bit & button::L2 != 0 {
            l2 = l2.max(pressure);
        }
        if bit & button::R2 != 0 {
            r2 = r2.max(pressure);
        }
    }

    let (lx, ly) = radial_deadzone(gp.axis(Axis::LeftX), gp.axis(Axis::LeftY), deadzone);
    let (rx, ry) = radial_deadzone(gp.axis(Axis::RightX), gp.axis(Axis::RightY), deadzone);

    Input {
        buttons,
        sticks: [rx, ry, lx, ly],
        l2,
        r2,
    }
}

fn read_raw_joystick_input(
    joy: &Joystick,
    rest_axes: &[i16],
    pad_map: &PadMap,
    deadzone: f32,
) -> Input {
    let mut buttons = 0u16;

    if pad_map.has_raw_joy_bindings {
        for &(input, bit) in &pad_map.entries {
            let pressed = match input {
                PadInput::JoyButton(idx) => joy.button(idx as u32).unwrap_or(false),
                PadInput::JoyHat(idx, dir) => joy
                    .hat(idx as u32)
                    .map(|state| dir.matches(state))
                    .unwrap_or(false),
                PadInput::JoyAxis(idx, positive) => {
                    let val = joy.axis(idx as u32).unwrap_or(0);
                    let rest = rest_axes.get(idx as usize).copied().unwrap_or(0);
                    joy_axis_pressed(val, rest, positive)
                }
                PadInput::GamepadButton(_) | PadInput::GamepadTrigger(_) => false,
            };
            if pressed {
                buttons |= bit;
            }
        }
    } else {
        for (idx, &bit) in RAW_FALLBACK_BUTTONS.iter().enumerate() {
            if idx as u32 >= joy.num_buttons() {
                break;
            }
            if joy.button(idx as u32).unwrap_or(false) {
                buttons |= bit;
            }
        }
        if joy.num_hats() > 0 {
            if let Ok(state) = joy.hat(0) {
                if HatDir::Up.matches(state) {
                    buttons |= button::UP;
                }
                if HatDir::Down.matches(state) {
                    buttons |= button::DOWN;
                }
                if HatDir::Left.matches(state) {
                    buttons |= button::LEFT;
                }
                if HatDir::Right.matches(state) {
                    buttons |= button::RIGHT;
                }
            }
        }
    }

    let axis_or_zero = |idx: u32| -> i16 {
        if idx < joy.num_axes() {
            joy.axis(idx).unwrap_or(0)
        } else {
            0
        }
    };
    let (lx, ly) = radial_deadzone(axis_or_zero(0), axis_or_zero(1), deadzone);
    let (rx, ry) = if joy.num_axes() >= 4 {
        radial_deadzone(axis_or_zero(2), axis_or_zero(3), deadzone)
    } else {
        (0x80, 0x80)
    };

    Input {
        buttons,
        sticks: [rx, ry, lx, ly],
        l2: if buttons & button::L2 != 0 { 0xff } else { 0 },
        r2: if buttons & button::R2 != 0 { 0xff } else { 0 },
    }
}

fn joy_axis_pressed(value: i16, rest: i16, positive: bool) -> bool {
    let delta = value as i32 - rest as i32;
    let thresh = JOY_AXIS_DIGITAL_THRESHOLD as i32;
    if positive {
        delta > thresh
    } else {
        delta < -thresh
    }
}

fn radial_deadzone(x: i16, y: i16, deadzone: f32) -> (u8, u8) {
    let fx = (x as f32 / 32767.0).clamp(-1.0, 1.0);
    let fy = (y as f32 / 32767.0).clamp(-1.0, 1.0);
    let mag = (fx * fx + fy * fy).sqrt();
    let dz = deadzone.clamp(0.0, 0.5);
    if mag <= dz || mag == 0.0 {
        return (0x80, 0x80);
    }
    let scale = ((mag - dz) / (1.0 - dz)).min(1.0) / mag;
    (norm_to_u8(fx * scale), norm_to_u8(fy * scale))
}

fn norm_to_u8(v: f32) -> u8 {
    ((v * 127.5) + 127.5).round().clamp(0.0, 255.0) as u8
}

fn trigger_to_u8(v: i16) -> u8 {
    ((v.max(0) as u32 * 255) / 32767).min(255) as u8
}

/// Convert PS2 `(small, large)` motor speeds into `(weak, strong)` u16 strengths.
///
/// The light motor spins above zero as a binary pulse; the heavy motor curves
/// up quadratically above the PS2 quiet threshold so mild bumps stay quiet.
fn rumble_strength((small, large): (u8, u8)) -> (u16, u16) {
    const QUIET: f32 = 110.0;
    let weak = if small != 0 { 8_000 } else { 0 };
    let strong = if (large as f32) <= QUIET {
        0
    } else {
        let x = (large as f32 - QUIET) / (255.0 - QUIET);
        (x * x * 50_000.0) as u16
    };
    (weak, strong)
}
