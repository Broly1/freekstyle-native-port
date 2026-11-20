//! Mapping configuration between `freekstyle.toml` names and SDL3 / PS2 inputs.

use freek_config::{button_bit, Binding};
use sdl3::gamepad::{Axis, Button};
use sdl3::joystick::HatState;

/// Cardinal direction on a raw joystick hat switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HatDir {
    Up,
    Down,
    Left,
    Right,
}

impl HatDir {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            HatDir::Up => "Up",
            HatDir::Down => "Down",
            HatDir::Left => "Left",
            HatDir::Right => "Right",
        }
    }

    pub(crate) fn matches(self, state: HatState) -> bool {
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

    pub(crate) fn first_from_state(state: HatState) -> Option<Self> {
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
    pub(crate) entries: Vec<(PadInput, u16)>,
    pub(crate) raw_bound_bits: u16,
    pub(crate) raw_bound_buttons: u64,
    pub(crate) raw_bound_axes: u64,
}

impl PadMap {
    /// Compile configured bindings into fast controller-lookup entries.
    pub fn compile(bindings: &[Binding]) -> Self {
        let mut entries = Vec::new();
        let mut raw_bound_bits = 0u16;
        let mut raw_bound_buttons = 0u64;
        let mut raw_bound_axes = 0u64;
        for binding in bindings {
            let Some(bit) = button_bit(&binding.button) else {
                continue;
            };
            for name in &binding.pad {
                match parse_pad_input(name) {
                    Some(input) => {
                        match input {
                            PadInput::JoyButton(idx) => {
                                raw_bound_bits |= bit;
                                if idx < 64 {
                                    raw_bound_buttons |= 1u64 << idx;
                                }
                            }
                            PadInput::JoyHat(_, _) => {
                                raw_bound_bits |= bit;
                            }
                            PadInput::JoyAxis(idx, _) => {
                                raw_bound_bits |= bit;
                                if idx < 64 {
                                    raw_bound_axes |= 1u64 << idx;
                                }
                            }
                            PadInput::GamepadButton(_) | PadInput::GamepadTrigger(_) => {}
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
            raw_bound_bits,
            raw_bound_buttons,
            raw_bound_axes,
        }
    }

    pub(crate) fn uses_raw_hat(&self, hat_idx: u8, dir: HatDir) -> bool {
        self.entries
            .iter()
            .any(|&(input, _)| input == PadInput::JoyHat(hat_idx, dir))
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

#[cfg(test)]
mod tests {
    use ee_runtime::pad::button;

    use super::*;

    #[test]
    fn parses_gamepad_aliases_and_triggers() {
        assert_eq!(parse_pad_input("South"), Some(PadInput::GamepadButton(Button::South)));
        assert_eq!(parse_pad_input("LeftTrigger"), Some(PadInput::GamepadButton(Button::LeftShoulder)));
        assert_eq!(parse_pad_input("LeftShoulder"), Some(PadInput::GamepadButton(Button::LeftShoulder)));
        assert_eq!(parse_pad_input("Select"), Some(PadInput::GamepadButton(Button::Back)));
        assert_eq!(parse_pad_input("Back"), Some(PadInput::GamepadButton(Button::Back)));
        assert_eq!(parse_pad_input("LeftTrigger2"), Some(PadInput::GamepadTrigger(Axis::TriggerLeft)));
        assert_eq!(parse_pad_input("RightTrigger2"), Some(PadInput::GamepadTrigger(Axis::TriggerRight)));
    }

    #[test]
    fn parses_raw_joystick_bindings() {
        assert_eq!(parse_pad_input("JoyButton0"), Some(PadInput::JoyButton(0)));
        assert_eq!(parse_pad_input("JoyButton15"), Some(PadInput::JoyButton(15)));
        assert_eq!(parse_pad_input("JoyHat0Up"), Some(PadInput::JoyHat(0, HatDir::Up)));
        assert_eq!(parse_pad_input("JoyHat2Right"), Some(PadInput::JoyHat(2, HatDir::Right)));
        assert_eq!(parse_pad_input("JoyAxis4+"), Some(PadInput::JoyAxis(4, true)));
        assert_eq!(parse_pad_input("JoyAxis3-"), Some(PadInput::JoyAxis(3, false)));
        assert_eq!(parse_pad_input("JoyAxis3"), None);
        assert_eq!(parse_pad_input("JoyHat0Diag"), None);
        assert_eq!(parse_pad_input("NotAButton"), None);
    }

    #[test]
    fn compile_tracks_partial_raw_bindings() {
        let bindings = vec![
            Binding {
                button: "cross".into(),
                keys: vec![],
                pad: vec!["South".into(), "JoyButton0".into()],
            },
            Binding {
                button: "r2".into(),
                keys: vec![],
                pad: vec!["RightTrigger2".into(), "JoyAxis5+".into()],
            },
            Binding {
                button: "triangle".into(),
                keys: vec![],
                pad: vec!["JoyHat0Up".into()],
            },
        ];
        let map = PadMap::compile(&bindings);
        assert_eq!(map.raw_bound_bits, button::CROSS | button::R2 | button::TRIANGLE);
        assert_eq!(map.raw_bound_buttons, 1u64 << 0);
        assert_eq!(map.raw_bound_axes, 1u64 << 5);
        assert!(map.uses_raw_hat(0, HatDir::Up));
        assert!(!map.uses_raw_hat(0, HatDir::Down));
    }
}
