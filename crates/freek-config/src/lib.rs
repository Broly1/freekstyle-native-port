//! `freekstyle.toml`: the settings the launcher edits and the game reads at
//! startup. Keys are stored by winit `KeyCode` name ("KeyZ", "ArrowUp",
//! "Digit1"), gamepad buttons by gilrs `Button` name ("South", "DPadUp",
//! "LeftTrigger2"), so neither side needs the other's input library.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// The settings file's name.
pub const FILE: &str = "freekstyle.toml";

/// Where the launcher writes and the game looks: `freekstyle.toml` in the
/// working directory (the port or install directory, which also holds
/// `game/`) when it is there or can be created; else, in an install shared
/// by all users (/opt/freekstyle), each user's
/// `$XDG_CONFIG_HOME/freekstyle/freekstyle.toml`.
pub fn path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let here = PathBuf::from(FILE);
        if here.is_file() || writable(Path::new(".")) {
            return here;
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))
            .unwrap_or_default();
        config.join("freekstyle").join(FILE)
    })
}

/// Whether files can be created in `dir` (tried, not guessed from modes).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".freekstyle-probe-{}", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub elf: String,
    pub disc: String,
    /// "1080", "1440" or "4k": window size and matching internal resolution.
    pub res: String,
    /// Internal resolution multiplier; 0 follows `res`.
    pub scale: u32,
    pub fullscreen: bool,
    pub stretch: bool,
    pub widescreen: bool,
    pub blur_bars: bool,
    pub ps2_icons: bool,
    /// Upscale game textures (menu text and artwork) with xBRZ.
    pub sharp_textures: bool,
    pub mute: bool,
    /// Launcher language: "en", "pt-BR", "es", "fr"; empty follows the system.
    pub language: String,
    /// The player the keyboard controls, 1 or 2. Gamepads take player 1
    /// and 2 in the order they connect.
    pub keyboard_player: u32,
    pub bindings: Vec<Binding>,
}

/// Everything that presses one PS2 button.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// PS2 button name, one of [`PS2_BUTTONS`].
    pub button: String,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub pad: Vec<String>,
}

/// PS2 buttons in display order, with their bit in the pad data
/// (`ee_runtime::pad::button`) and the Xbox-layout label players know.
pub const PS2_BUTTONS: [(&str, u16, &str); 16] = [
    ("cross", 1 << 14, "A (Cross)"),
    ("circle", 1 << 13, "B (Circle)"),
    ("square", 1 << 15, "X (Square)"),
    ("triangle", 1 << 12, "Y (Triangle)"),
    ("l1", 1 << 10, "LB (L1)"),
    ("r1", 1 << 11, "RB (R1)"),
    ("l2", 1 << 8, "LT (L2)"),
    ("r2", 1 << 9, "RT (R2)"),
    ("l3", 1 << 1, "Left stick click (L3)"),
    ("r3", 1 << 2, "Right stick click (R3)"),
    ("start", 1 << 3, "Start"),
    ("select", 1 << 0, "Back (Select)"),
    ("up", 1 << 4, "D-pad up"),
    ("down", 1 << 6, "D-pad down"),
    ("left", 1 << 7, "D-pad left"),
    ("right", 1 << 5, "D-pad right"),
];

/// The PS2 button bit for a button name.
pub fn button_bit(name: &str) -> Option<u16> {
    PS2_BUTTONS.iter().find(|b| b.0 == name).map(|b| b.1)
}

pub fn default_bindings() -> Vec<Binding> {
    let table: [(&str, &[&str], &[&str]); 16] = [
        // Right hand, two rows: U I O P = X Y RB LB, J K L ; = A B RT LT.
        // Enter also selects, as menus confirm with Cross.
        ("cross", &["KeyJ", "Space", "Enter"], &["South"]),
        ("circle", &["KeyK"], &["East"]),
        ("square", &["KeyU"], &["West"]),
        ("triangle", &["KeyI"], &["North"]),
        ("l1", &["KeyP"], &["LeftTrigger"]),
        ("r1", &["KeyO"], &["RightTrigger"]),
        ("l2", &["Semicolon"], &["LeftTrigger2"]),
        ("r2", &["KeyL"], &["RightTrigger2"]),
        ("l3", &[], &["LeftThumb"]),
        ("r3", &[], &["RightThumb"]),
        ("start", &["KeyT"], &["Start"]),
        ("select", &["Backspace"], &["Select"]),
        // Left hand: WASD, arrows too. The game also turns these into the
        // left stick.
        ("up", &["KeyW", "ArrowUp"], &["DPadUp"]),
        ("down", &["KeyS", "ArrowDown"], &["DPadDown"]),
        ("left", &["KeyA", "ArrowLeft"], &["DPadLeft"]),
        ("right", &["KeyD", "ArrowRight"], &["DPadRight"]),
    ];
    let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
    table.iter().map(|(b, k, p)| Binding { button: b.to_string(), keys: strings(k), pad: strings(p) }).collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            elf: "game/SLUS_203.67".into(),
            disc: "game/Freekstyle.bin".into(),
            res: "1080".into(),
            scale: 0,
            fullscreen: false,
            stretch: false,
            widescreen: false,
            blur_bars: false,
            ps2_icons: false,
            sharp_textures: true,
            mute: false,
            language: String::new(),
            keyboard_player: 1,
            bindings: default_bindings(),
        }
    }
}

impl Config {
    /// Read `path`; a missing file gives the defaults. Buttons the file
    /// leaves out keep their default bindings.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut c: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        c.bindings.retain(|b| button_bit(&b.button).is_some());
        for d in default_bindings() {
            if !c.bindings.iter().any(|b| b.button == d.button) {
                c.bindings.push(d);
            }
        }
        c.bindings.sort_by_key(|b| PS2_BUTTONS.iter().position(|p| p.0 == b.button));
        Ok(c)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Window size and internal scale for `res` and `scale`.
    pub fn display(&self) -> Option<((u32, u32), u32)> {
        // Internal scale covers the window height: 448 lines x scale.
        let (window, scale) = match self.res.as_str() {
            "1080" => ((1920, 1080), 3),
            "1440" => ((2560, 1440), 4),
            "4k" | "2160" => ((3840, 2160), 5),
            _ => return None,
        };
        Some((window, if self.scale == 0 { scale } else { self.scale.clamp(1, 8) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_fill_missing() {
        let mut c = Config::default();
        c.widescreen = true;
        c.bindings.retain(|b| b.button != "cross");
        let text = toml::to_string_pretty(&c).unwrap();
        let dir = std::env::temp_dir().join(format!("freek-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        std::fs::write(&path, text).unwrap();
        let back = Config::load(&path).unwrap();
        assert!(back.widescreen);
        assert_eq!(back.bindings, default_bindings());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
