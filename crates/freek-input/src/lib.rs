//! Unified controller input and force-feedback built on SDL3.
//!
//! Used by both `freekstyle` and `launcher` so controller detection,
//! `gamecontrollerdb.txt` mappings, deadzone scaling, raw-joystick fallback,
//! and rumble work identically in both binaries.

mod input;
mod mapping;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ee_runtime::pad::Input;
use ee_runtime::{pad_log, quiet};
use sdl3::event::Event;
use sdl3::gamepad::{Axis, Gamepad};
use sdl3::iostream::IOStream;
use sdl3::joystick::{Joystick, JoystickId, JoystickType};
use sdl3::{EventPump, GamepadSubsystem, JoystickSubsystem, Sdl};

use crate::input::{rumble_strength, JOY_AXIS_DIGITAL_THRESHOLD, TRIGGER_PRESS_THRESHOLD};
pub use crate::mapping::{canonical_button_name, parse_pad_input, HatDir, PadInput, PadMap};

const RUMBLE_PERIOD: Duration = Duration::from_millis(50);
const RUMBLE_REFRESH: Duration = Duration::from_millis(500);
const RUMBLE_DURATION_MS: u32 = 2_000;

enum DeviceKind {
    Gamepad(Gamepad),
    RawJoystick {
        joy: Joystick,
        rest_axes: Vec<i16>,
        axis_down: Vec<i8>,
    },
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
        // SDL would otherwise take SIGINT/SIGTERM and turn them into a quit
        // event nobody reads, so Ctrl+C and kill would no longer stop us.
        sdl3::hint::set("SDL_NO_SIGNAL_HANDLERS", "1");
        // Required on Windows when another library (winit/eframe) owns the
        // window and SDL_INIT_VIDEO is not used.
        #[cfg(target_os = "windows")]
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
                Ok(n) if n > 0 && !quiet() => eprintln!("[input] loaded {n} bundled controller mappings"),
                Ok(_) => {}
                Err(e) => eprintln!("[input] bundled controller mappings error: {e}"),
            }
        }

        for path in freek_config::external_gamepad_db_paths() {
            match gamepad.load_mappings(&path) {
                Ok(n) if !quiet() => {
                    eprintln!("[input] loaded {n} mappings from {}", path.display());
                }
                Ok(_) => {}
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

        // Populate initial controller state before sampling `rest_axes`.
        mgr.events.pump_events();
        if let Ok(ids) = mgr.joystick.joysticks() {
            for id in ids {
                mgr.add_device(id, false);
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
            // A controller takes a player seat on its first real input, not
            // when it appears: a wireless receiver with no pad paired shows
            // up as a gamepad too, and would otherwise sit on player 1.
            if let Some(id) = self.activity(&event) {
                self.seat(id);
            }
            match event {
                Event::GamepadAdded { which, .. } | Event::JoyDeviceAdded { which, .. } => {
                    self.add_device(which, false);
                }
                Event::GamepadRemapped { which, .. } => {
                    self.add_device(which, true);
                }
                Event::GamepadRemoved { which, .. } | Event::JoyDeviceRemoved { which, .. } => {
                    self.remove_device(which);
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
                                if is_down != dev.left_trigger_down && pad_log() {
                                    eprintln!("[input] sdl LeftTrigger2 raw={value} threshold={TRIGGER_PRESS_THRESHOLD} down={is_down}");
                                }
                                if is_down && !dev.left_trigger_down {
                                    captured = Some("LeftTrigger2".to_string());
                                }
                                dev.left_trigger_down = is_down;
                            }
                            Axis::TriggerRight => {
                                if is_down != dev.right_trigger_down && pad_log() {
                                    eprintln!("[input] sdl RightTrigger2 raw={value} threshold={TRIGGER_PRESS_THRESHOLD} down={is_down}");
                                }
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
                        if pad_log() {
                            eprintln!("[input] sdl raw JoyButton{button_idx} down");
                        }
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
                            if pad_log() {
                                eprintln!("[input] sdl raw JoyHat{hat_idx}{}", dir.as_str());
                            }
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
                    if let Some(dev) = self.devices.get_mut(&which) {
                        if let DeviceKind::RawJoystick {
                            rest_axes,
                            axis_down,
                            ..
                        } = &mut dev.kind
                        {
                            let idx = axis_idx as usize;
                            let rest = rest_axes.get(idx).copied().unwrap_or(0);
                            let delta = value as i32 - rest as i32;
                            let dir: i8 = if delta > i32::from(JOY_AXIS_DIGITAL_THRESHOLD) {
                                1
                            } else if delta < -i32::from(JOY_AXIS_DIGITAL_THRESHOLD) {
                                -1
                            } else {
                                0
                            };
                            if idx >= axis_down.len() {
                                axis_down.resize(idx + 1, 0);
                            }
                            if dir != 0 && dir != axis_down[idx] {
                                let sign = if dir > 0 { '+' } else { '-' };
                                if pad_log() {
                                    eprintln!("[input] sdl raw JoyAxis{axis_idx}{sign} value={value} rest={rest}");
                                }
                                captured = Some(format!("JoyAxis{axis_idx}{sign}"));
                            }
                            axis_down[idx] = dir;
                        }
                    }
                }
                _ => {}
            }
        }

        captured
    }

    /// Name of the player 1 (or first connected) controller, for UI status.
    /// Before any controller has been used, the first one found.
    pub fn primary_controller_name(&self) -> Option<String> {
        self.players[0]
            .or(self.players[1])
            .and_then(|id| self.devices.get(&id))
            .or_else(|| self.devices.values().next())
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

    fn add_device(&mut self, id: JoystickId, remapped: bool) {
        if self.gamepad.is_gamepad(id) {
            if !remapped && self.devices.get(&id).is_some_and(DeviceState::is_gamepad) {
                return;
            }
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
                        eprintln!("[input] controller found (gamepad): {name}; press a button to play");
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
                self.joystick.update();
                let rest_axes: Vec<i16> = (0..joy.num_axes())
                    .map(|idx| joy.axis(idx).unwrap_or(0))
                    .collect();
                let axis_down = vec![0i8; rest_axes.len()];
                let guid = joy.guid().to_string();
                let (axes, buttons, hats) = (joy.num_axes(), joy.num_buttons(), joy.num_hats());
                let name = if joy.name().is_empty() {
                    format!("Controller ({guid})")
                } else {
                    joy.name()
                };
                self.devices.insert(
                    id,
                    DeviceState {
                        kind: DeviceKind::RawJoystick {
                            joy,
                            rest_axes,
                            axis_down,
                        },
                        left_trigger_down: false,
                        right_trigger_down: false,
                    },
                );
                eprintln!("[input] unmapped controller GUID: {guid} (raw joystick fallback: {axes} axes, {buttons} buttons, {hats} hats)");
                eprintln!("[input] controller found (joystick): {name}; press a button to play");
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

    /// The controller an event shows someone using: a button, a trigger or
    /// stick pushed well past rest, or a hat moved. Raw joystick axes don't
    /// count, as their rest positions are guesses taken when they opened.
    fn activity(&self, event: &Event) -> Option<JoystickId> {
        let id = match *event {
            Event::GamepadButtonDown { which, .. } => which,
            Event::GamepadAxisMotion { which, axis, value, .. } => {
                let pushed = match axis {
                    Axis::TriggerLeft | Axis::TriggerRight => value >= TRIGGER_PRESS_THRESHOLD,
                    _ => value.unsigned_abs() >= JOY_AXIS_DIGITAL_THRESHOLD as u16,
                };
                if !pushed {
                    return None;
                }
                which
            }
            Event::JoyButtonDown { which, .. } => which,
            Event::JoyHatMotion { which, state, .. } if HatDir::first_from_state(state).is_some() => which,
            _ => return None,
        };
        self.devices.contains_key(&id).then_some(id)
    }

    /// Give a controller the first free player seat.
    fn seat(&mut self, id: JoystickId) {
        if self.players.contains(&Some(id)) {
            return;
        }
        let Some(dev) = self.devices.get(&id) else { return };
        let (name, kind) = (dev.name(), if dev.is_gamepad() { "gamepad" } else { "joystick" });
        let Some(slot) = self.players.iter().position(Option::is_none) else {
            eprintln!("[input] {name}: not playing, both players have a controller");
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
    // SAFETY: `id` is a live SDL3 joystick instance ID and the SDL joystick
    // subsystem is initialized on the calling thread.
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
        DeviceKind::Gamepad(gp) => input::read_gamepad_input(gp, pad_map, deadzone),
        DeviceKind::RawJoystick { joy, rest_axes, .. } => {
            input::read_raw_joystick_input(joy, rest_axes, pad_map, deadzone)
        }
    }
}
