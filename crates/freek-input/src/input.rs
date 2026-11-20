//! Translates SDL3 gamepad and raw-joystick states into PS2 pad inputs.

use ee_runtime::pad::{button, Input};
use sdl3::gamepad::{Axis, Gamepad};
use sdl3::joystick::Joystick;

use crate::mapping::{HatDir, PadInput, PadMap};

pub(crate) const TRIGGER_PRESS_THRESHOLD: i16 = 8_192;
pub(crate) const JOY_AXIS_DIGITAL_THRESHOLD: i16 = 16_384;

/// Fallback button layout when an unmapped raw joystick lacks explicit `Joy*` bindings.
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

pub(crate) fn read_gamepad_input(gp: &Gamepad, pad_map: &PadMap, deadzone: f32) -> Input {
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

pub(crate) fn read_raw_joystick_input(
    joy: &Joystick,
    rest_axes: &[i16],
    pad_map: &PadMap,
    deadzone: f32,
) -> Input {
    let mut buttons = 0u16;
    let mut l2 = 0u8;
    let mut r2 = 0u8;

    for &(input, bit) in &pad_map.entries {
        let (pressed, pressure) = match input {
            PadInput::JoyButton(idx) => {
                let p = joy.button(idx as u32).unwrap_or(false);
                (p, if p { 0xff } else { 0 })
            }
            PadInput::JoyHat(idx, dir) => {
                let p = joy
                    .hat(idx as u32)
                    .map(|state| dir.matches(state))
                    .unwrap_or(false);
                (p, if p { 0xff } else { 0 })
            }
            PadInput::JoyAxis(idx, positive) => {
                let val = joy.axis(idx as u32).unwrap_or(0);
                let rest = rest_axes.get(idx as usize).copied().unwrap_or(0);
                (
                    joy_axis_pressed(val, rest, positive),
                    joy_axis_pressure(val, rest, positive),
                )
            }
            PadInput::GamepadButton(_) | PadInput::GamepadTrigger(_) => (false, 0),
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

    // Apply fallback bindings only for PS2 buttons and physical controls that are
    // not explicitly bound via `Joy*`, so partial raw configs don't disable the
    // rest of the controller or cause double-bindings.
    for (idx, &bit) in RAW_FALLBACK_BUTTONS.iter().enumerate() {
        if idx as u32 >= joy.num_buttons() {
            break;
        }
        let button_taken = idx < 64 && (pad_map.raw_bound_buttons & (1u64 << idx)) != 0;
        if (pad_map.raw_bound_bits & bit) != 0 || button_taken {
            continue;
        }
        if joy.button(idx as u32).unwrap_or(false) {
            buttons |= bit;
            if bit & button::L2 != 0 {
                l2 = 0xff;
            }
            if bit & button::R2 != 0 {
                r2 = 0xff;
            }
        }
    }
    if joy.num_hats() > 0 {
        if let Ok(state) = joy.hat(0) {
            for (dir, bit) in [
                (HatDir::Up, button::UP),
                (HatDir::Down, button::DOWN),
                (HatDir::Left, button::LEFT),
                (HatDir::Right, button::RIGHT),
            ] {
                if (pad_map.raw_bound_bits & bit) == 0
                    && !pad_map.uses_raw_hat(0, dir)
                    && dir.matches(state)
                {
                    buttons |= bit;
                }
            }
        }
    }

    let num_axes = joy.num_axes();
    let stick_axis = |idx: u32| -> i16 {
        if !is_stick_axis_usable(idx, num_axes, pad_map.raw_bound_axes, rest_axes) {
            return 0;
        }
        joy.axis(idx).unwrap_or(0)
    };
    let (lx, ly) = radial_deadzone(stick_axis(0), stick_axis(1), deadzone);
    let (rx, ry) = if num_axes >= 4 {
        radial_deadzone(stick_axis(2), stick_axis(3), deadzone)
    } else {
        (0x80, 0x80)
    };

    Input {
        buttons,
        sticks: [rx, ry, lx, ly],
        l2,
        r2,
    }
}

fn is_stick_axis_usable(
    idx: u32,
    num_axes: u32,
    raw_bound_axes: u64,
    rest_axes: &[i16],
) -> bool {
    if idx >= num_axes {
        return false;
    }
    if idx < 64 && (raw_bound_axes & (1u64 << idx)) != 0 {
        return false;
    }
    let rest = rest_axes.get(idx as usize).copied().unwrap_or(0);
    // Axes resting outside the center zone (e.g. triggers at -32768) must not drive sticks.
    // Widen to i32 before `abs()` so `i16::MIN` (-32768) does not overflow.
    i32::from(rest).abs() <= i32::from(JOY_AXIS_DIGITAL_THRESHOLD)
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

fn joy_axis_pressure(value: i16, rest: i16, positive: bool) -> u8 {
    let delta = if positive {
        value as i32 - rest as i32
    } else {
        rest as i32 - value as i32
    };
    if delta <= 0 {
        return 0;
    }
    let span = if positive {
        i16::MAX as i32 - rest as i32
    } else {
        rest as i32 - i16::MIN as i32
    }
    .max(1);
    ((delta as u32 * 255) / span as u32).min(255) as u8
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
pub(crate) fn rumble_strength((small, large): (u8, u8)) -> (u16, u16) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_deadzone_centers_small_deflections_and_preserves_extremes() {
        assert_eq!(radial_deadzone(0, 0, 0.15), (0x80, 0x80));
        assert_eq!(radial_deadzone(4000, -3000, 0.2), (0x80, 0x80));
        assert_eq!(radial_deadzone(i16::MAX, 0, 0.15), (0xff, 0x80));
        assert_eq!(radial_deadzone(i16::MIN, 0, 0.15), (0x00, 0x80));
        assert_eq!(radial_deadzone(0, i16::MIN, 0.15), (0x80, 0x00));
    }

    #[test]
    fn joy_axis_handles_centered_and_trigger_resting_origins() {
        // Trigger resting at i16::MIN pushed in + direction to i16::MAX.
        assert!(!joy_axis_pressed(i16::MIN, i16::MIN, true));
        assert_eq!(joy_axis_pressure(i16::MIN, i16::MIN, true), 0);
        assert!(!joy_axis_pressed(-20_000, i16::MIN, true));
        assert_eq!(joy_axis_pressure(0, i16::MIN, true), 127);
        assert!(joy_axis_pressed(0, i16::MIN, true));
        assert_eq!(joy_axis_pressure(i16::MAX, i16::MIN, true), 255);

        // Centered axis pulled in - direction.
        assert!(!joy_axis_pressed(-10_000, 0, false));
        assert!(joy_axis_pressed(-20_000, 0, false));
        assert_eq!(joy_axis_pressure(i16::MIN, 0, false), 255);

        // Rest at i16::MAX avoids division by zero.
        assert_eq!(joy_axis_pressure(i16::MAX, i16::MAX, true), 0);
    }

    #[test]
    fn stick_axis_excludes_bound_and_offcenter_rest_axes() {
        let rest = [0, 100, i16::MIN, 0];
        assert!(is_stick_axis_usable(0, 4, 0, &rest));
        assert!(is_stick_axis_usable(1, 4, 0, &rest));
        assert!(!is_stick_axis_usable(2, 4, 0, &rest));
        assert!(is_stick_axis_usable(3, 4, 0, &rest));
        assert!(!is_stick_axis_usable(1, 4, 1u64 << 1, &rest));
        assert!(!is_stick_axis_usable(4, 4, 0, &rest));
    }

    #[test]
    fn trigger_and_rumble_curves_stay_in_range() {
        assert_eq!(trigger_to_u8(-100), 0);
        assert_eq!(trigger_to_u8(i16::MAX), 255);
        assert_eq!(rumble_strength((0, 110)), (0, 0));
        assert_eq!(rumble_strength((1, 110)), (8_000, 0));
        assert_eq!(rumble_strength((0, 255)), (0, 50_000));
    }
}
