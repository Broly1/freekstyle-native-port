//! Freekstyle launcher: edits freekstyle.toml (display options and key /
//! controller bindings) and starts the game next to it.
//!
//! The port directory (the one holding `game/`) is the working directory
//! for both; when started from elsewhere the launcher finds it from its own
//! location (target/release -> port). On start it also installs desktop
//! entries and icons under ~/.local/share, which is how Wayland desktops
//! show the right icon for the game and launcher windows.

mod i18n;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use freek_config::{Config, PS2_BUTTONS};
use gilrs::{EventType, Gilrs};
use i18n::{fill, Lang, Tr};

/// The game's icon (the devil face) at each hicolor size KDE and GNOME
/// look up.
const ICONS: [(u32, &[u8]); 9] = [
    (16, include_bytes!("../../freekstyle/assets/icon/16.png")),
    (22, include_bytes!("../../freekstyle/assets/icon/22.png")),
    (24, include_bytes!("../../freekstyle/assets/icon/24.png")),
    (32, include_bytes!("../../freekstyle/assets/icon/32.png")),
    (48, include_bytes!("../../freekstyle/assets/icon/48.png")),
    (64, include_bytes!("../../freekstyle/assets/icon/64.png")),
    (128, include_bytes!("../../freekstyle/assets/icon/128.png")),
    (256, include_bytes!("../../freekstyle/assets/icon/256.png")),
    (512, include_bytes!("../../freekstyle/assets/icon/512.png")),
];
/// The launcher's icon (the Freekstyle wordmark), same sizes.
const LAUNCHER_ICONS: [(u32, &[u8]); 9] = [
    (16, include_bytes!("../../freekstyle/assets/launcher/16.png")),
    (22, include_bytes!("../../freekstyle/assets/launcher/22.png")),
    (24, include_bytes!("../../freekstyle/assets/launcher/24.png")),
    (32, include_bytes!("../../freekstyle/assets/launcher/32.png")),
    (48, include_bytes!("../../freekstyle/assets/launcher/48.png")),
    (64, include_bytes!("../../freekstyle/assets/launcher/64.png")),
    (128, include_bytes!("../../freekstyle/assets/launcher/128.png")),
    (256, include_bytes!("../../freekstyle/assets/launcher/256.png")),
    (512, include_bytes!("../../freekstyle/assets/launcher/512.png")),
];
const WORDMARK_PNG: &[u8] = include_bytes!("../../freekstyle/assets/launcher/wordmark.png");

// Colours from the Freekstyle logo: rim red, deep maroon, off-white.
const ACCENT: Color32 = Color32::from_rgb(0xf0, 0x44, 0x36);
const MAROON: Color32 = Color32::from_rgb(0x6e, 0x04, 0x05);
const INK: Color32 = Color32::from_rgb(0xf8, 0xf3, 0xf4);
const HEADER_BG: Color32 = Color32::from_rgb(0x2a, 0x0c, 0x0e);
const PANEL_BG: Color32 = Color32::from_rgb(0x17, 0x10, 0x11);
const GROUP_BG: Color32 = Color32::from_rgb(0x24, 0x18, 0x19);
const MUTED: Color32 = Color32::from_rgb(0xb0, 0x9a, 0x9b);

/// Keys the game keeps for itself: quit, fullscreen, frame capture.
const RESERVED_KEYS: [&str; 3] = ["Escape", "F11", "F12"];

fn main() -> eframe::Result {
    let root = find_root();
    if let Err(e) = std::env::set_current_dir(&root) {
        eprintln!("{}: {e}", root.display());
    }
    if let Err(e) = install_desktop_entries(&root) {
        eprintln!("[launcher] desktop entries: {e}");
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Freekstyle Launcher")
            .with_app_id("freekstyle-launcher")
            .with_inner_size([760.0, 640.0])
            .with_min_inner_size([560.0, 420.0])
            .with_icon(eframe::icon_data::from_png_bytes(LAUNCHER_ICONS[7].1).unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native("Freekstyle Launcher", options, Box::new(|cc| Ok(Box::new(Launcher::new(cc)))))
}

/// The working directory if it looks like the port directory, else the
/// nearest ancestor of the executable that does.
fn find_root() -> PathBuf {
    let is_root = |p: &Path| p.join("game").is_dir() || p.join(freek_config::FILE).is_file();
    let cwd = std::env::current_dir().unwrap_or_default();
    if is_root(&cwd) {
        return cwd;
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.ancestors().find(|p| is_root(p)).map(Path::to_path_buf).unwrap_or(cwd)
}

/// The game binary, built next to the launcher.
fn game_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.with_file_name(format!("freekstyle{}", std::env::consts::EXE_SUFFIX))
}

/// Application-menu entries and icons for the game and the launcher. The
/// window app IDs ("freekstyle", "freekstyle-launcher") match the entry
/// names, which is how Wayland compositors pick the icon.
fn install_desktop_entries(root: &Path) -> std::io::Result<()> {
    let Some(home) = std::env::var_os("HOME") else { return Ok(()) };
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| Path::new(&home).join(".local/share"));
    let launcher = std::env::current_exe()?;
    let game = game_binary();
    // Plasma and KWin cache icons by name for the whole session, so each
    // picture gets its own name ("freekstyle-1a2b3c4d"): a new icon shows
    // up at once instead of after the next login.
    let icon_name = |app: &str, icons: &[(u32, &[u8])]| {
        let hash = icons.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, (_, png)| {
            png.iter().fold(h, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
        });
        format!("{app}-{:08x}", hash as u32)
    };
    let (game_icon, launcher_icon) = (icon_name("freekstyle", &ICONS), icon_name("freekstyle-launcher", &LAUNCHER_ICONS));
    let entry = |name: &str, comment: fn(&Tr) -> &str, exec: &Path, icon: &str, app_id: &str| {
        let mut comments = format!("Comment={}\n", comment(&i18n::EN));
        for (locale, lang) in [("pt_BR", Lang::PtBr), ("es", Lang::Es), ("fr", Lang::Fr)] {
            comments += &format!("Comment[{locale}]={}\n", comment(lang.tr()));
        }
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\n{comments}Exec=\"{}\"\nPath={}\nIcon={icon}\nStartupWMClass={app_id}\nCategories=Game;\nTerminal=false\n",
            exec.display(),
            root.display()
        )
    };
    let mut files: Vec<(PathBuf, Vec<u8>)> = vec![
        (
            data.join("applications/freekstyle.desktop"),
            entry("Freekstyle", |t| t.game_comment, &game, &game_icon, "freekstyle").into(),
        ),
        (
            data.join("applications/freekstyle-launcher.desktop"),
            entry("Freekstyle Launcher", |t| t.launcher_comment, &launcher, &launcher_icon, "freekstyle-launcher").into(),
        ),
    ];
    for ((n, game), (_, launcher)) in ICONS.into_iter().zip(LAUNCHER_ICONS) {
        files.push((data.join(format!("icons/hicolor/{n}x{n}/apps/{game_icon}.png")), game.into()));
        files.push((data.join(format!("icons/hicolor/{n}x{n}/apps/{launcher_icon}.png")), launcher.into()));
    }
    // Drop icons from earlier versions (unhashed names, old pictures, SVGs).
    let current = [format!("{game_icon}.png"), format!("{launcher_icon}.png")];
    if let Ok(dirs) = std::fs::read_dir(data.join("icons/hicolor")) {
        for dir in dirs.flatten() {
            let Ok(icons) = std::fs::read_dir(dir.path().join("apps")) else { continue };
            for icon in icons.flatten() {
                let name = icon.file_name().to_string_lossy().into_owned();
                if name.starts_with("freekstyle") && !current.contains(&name) {
                    let _ = std::fs::remove_file(icon.path());
                }
            }
        }
    }
    let mut changed = false;
    for (path, bytes) in files {
        // Rewrite only on change, so desktops don't rescan every start.
        if std::fs::read(&path).ok().as_deref() != Some(&bytes[..]) {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, bytes)?;
            changed = true;
        }
    }
    // KDE: refresh the application database now rather than on its next scan.
    if changed {
        let _ = Command::new("kbuildsycoca6").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Play,
    Controls,
}

/// A binding slot waiting for input: (index into bindings, keyboard?).
type Capture = (usize, bool);

struct Launcher {
    config: Config,
    /// Last state written to disk; saves happen when `config` differs.
    saved: Config,
    tab: Tab,
    capture: Option<Capture>,
    gilrs: Option<Gilrs>,
    game: Option<Child>,
    status: String,
    logo: Option<egui::TextureHandle>,
}

impl Launcher {
    fn new(cc: &eframe::CreationContext) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx.style_mut_of(egui::Theme::Dark, |style| {
            let v = &mut style.visuals;
            v.panel_fill = PANEL_BG;
            v.window_fill = PANEL_BG;
            v.extreme_bg_color = Color32::from_rgb(0x0e, 0x09, 0x0a);
            v.faint_bg_color = Color32::from_rgb(0x1f, 0x15, 0x16);
            v.override_text_color = Some(INK);
            v.selection.bg_fill = MAROON;
            v.selection.stroke.color = ACCENT;
            v.hyperlink_color = ACCENT;
            v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x3a, 0x22, 0x24);
            v.widgets.inactive.bg_fill = Color32::from_rgb(0x3a, 0x22, 0x24);
            v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x7a, 0x3c, 0x3e));
            v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x55, 0x1c, 0x1e);
            v.widgets.hovered.bg_fill = Color32::from_rgb(0x55, 0x1c, 0x1e);
            v.widgets.hovered.bg_stroke.color = ACCENT;
            v.widgets.active.weak_bg_fill = MAROON;
            v.widgets.active.bg_fill = MAROON;
            style.spacing.item_spacing = egui::vec2(10.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
        });

        let (config, status) = match Config::load(freek_config::FILE.as_ref()) {
            Ok(c) => (c, String::new()),
            Err(e) => (Config::default(), fill(Lang::system().tr().using_defaults, &[&e])),
        };
        let logo = decode_png(WORDMARK_PNG).map(|img| cc.egui_ctx.load_texture("logo", img, egui::TextureOptions::LINEAR));
        Launcher {
            saved: config.clone(),
            config,
            tab: Tab::Play,
            capture: None,
            gilrs: Gilrs::new().ok(),
            game: None,
            status,
            logo,
        }
    }

    /// Text in the chosen language.
    fn tr(&self) -> &'static Tr {
        Lang::from_setting(&self.config.language).tr()
    }

    fn save(&mut self) {
        if self.config != self.saved {
            match self.config.save(freek_config::FILE.as_ref()) {
                Ok(()) => self.saved = self.config.clone(),
                Err(e) => self.status = fill(self.tr().could_not_save, &[&e]),
            }
        }
    }

    fn play(&mut self) {
        self.save();
        let bin = game_binary();
        match Command::new(&bin).spawn() {
            Ok(child) => {
                self.game = Some(child);
                self.status = self.tr().game_running.into();
            }
            Err(e) => self.status = format!("{}: {e} ({})", bin.display(), self.tr().build_hint),
        }
    }

    /// Bind `name` to the slot being captured, taking it off any other
    /// button so one input never presses two things.
    fn bind(&mut self, (index, keyboard): Capture, name: String) {
        let t = self.tr();
        for (i, b) in self.config.bindings.iter_mut().enumerate() {
            let list = if keyboard { &mut b.keys } else { &mut b.pad };
            if i != index && list.contains(&name) {
                list.retain(|n| *n != name);
                self.status = fill(t.moved_from, &[&input_label(t, &name, keyboard), button_label(t, &b.button)]);
            }
        }
        let list = if keyboard { &mut self.config.bindings[index].keys } else { &mut self.config.bindings[index].pad };
        if !list.contains(&name) {
            list.push(name);
        }
        self.capture = None;
    }

    fn poll_gamepad(&mut self) -> (Option<String>, Option<String>) {
        let Some(g) = &mut self.gilrs else { return (None, None) };
        let mut pressed = None;
        while let Some(ev) = g.next_event() {
            if let EventType::ButtonPressed(b, _) = ev.event {
                pressed = Some(format!("{b:?}"));
            }
        }
        let name = g.gamepads().find(|(_, p)| p.is_connected()).map(|(_, p)| p.name().to_string());
        (name, pressed)
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let t = self.tr();
        ui.horizontal(|ui| {
            // The wordmark is the title; text only if it failed to decode.
            ui.vertical(|ui| {
                match &self.logo {
                    Some(logo) => {
                        ui.add(egui::Image::new(logo).max_height(76.0));
                    }
                    None => {
                        ui.label(RichText::new("Freekstyle").size(30.0).strong().color(INK));
                    }
                }
                ui.label(RichText::new(t.subtitle).color(MUTED));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let running = self.game.is_some();
                let play = egui::Button::new(RichText::new(if running { t.running } else { t.play }).size(22.0).strong().color(INK))
                    .fill(if running { MAROON } else { ACCENT })
                    .min_size(egui::vec2(150.0, 48.0))
                    .corner_radius(10.0);
                if ui.add_enabled(!running, play).clicked() {
                    self.play();
                }
            });
        });
    }

    fn play_tab(&mut self, ui: &mut egui::Ui) {
        let t = self.tr();
        let c = &mut self.config;
        section(ui, t.display, |ui| {
            egui::Grid::new("display").num_columns(2).min_col_width(150.0).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label(t.resolution);
                egui::ComboBox::from_id_salt("res").selected_text(res_label(&c.res)).show_ui(ui, |ui| {
                    for r in ["1080", "1440", "4k"] {
                        ui.selectable_value(&mut c.res, r.to_string(), res_label(r));
                    }
                });
                ui.end_row();
                ui.label(t.internal_resolution);
                let text = if c.scale == 0 { t.match_window.to_string() } else { format!("{}× PS2", c.scale) };
                egui::ComboBox::from_id_salt("scale").selected_text(text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut c.scale, 0, t.match_window);
                    for s in 1..=8 {
                        ui.selectable_value(&mut c.scale, s, format!("{s}× PS2"));
                    }
                });
                ui.end_row();
                ui.label(t.fullscreen);
                ui.checkbox(&mut c.fullscreen, t.fullscreen_hint);
                ui.end_row();
                ui.label(t.widescreen);
                ui.checkbox(&mut c.widescreen, t.widescreen_hint);
                ui.end_row();
                ui.label(t.stretch);
                ui.checkbox(&mut c.stretch, t.stretch_hint);
                ui.end_row();
                ui.label(t.side_bars);
                ui.checkbox(&mut c.blur_bars, t.side_bars_hint);
                ui.end_row();
                ui.label(t.sharp_textures);
                ui.checkbox(&mut c.sharp_textures, t.sharp_textures_hint);
                ui.end_row();
            });
        });
        section(ui, t.game, |ui| {
            egui::Grid::new("game").num_columns(2).min_col_width(150.0).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label(t.sound);
                ui.checkbox(&mut c.mute, t.mute);
                ui.end_row();
                ui.label(t.button_icons);
                ui.checkbox(&mut c.ps2_icons, t.ps2_icons_hint);
                ui.end_row();
                ui.label(t.disc_image);
                ui.add(egui::TextEdit::singleline(&mut c.disc).desired_width(360.0));
                ui.end_row();
                ui.label(t.executable);
                ui.add(egui::TextEdit::singleline(&mut c.elf).desired_width(360.0));
                ui.end_row();
            });
        });
    }

    fn controls_tab(&mut self, ui: &mut egui::Ui, pad_name: Option<&str>) {
        let t = self.tr();
        ui.horizontal(|ui| {
            let (dot, text) = match pad_name {
                Some(n) => (Color32::from_rgb(0x4c, 0xd9, 0x64), fill(t.controller_named, &[n])),
                None => (Color32::from_rgb(0xe0, 0x4f, 0x4f), t.no_controller.into()),
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, dot);
            ui.label(text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t.reset_defaults).clicked() {
                    self.config.bindings = freek_config::default_bindings();
                    self.capture = None;
                }
            });
        });
        ui.label(
            RichText::new(t.controls_help).color(MUTED),
        );
        ui.add_space(4.0);
        let mut remove = None;
        let mut start = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("controls").num_columns(3).striped(true).spacing([20.0, 8.0]).min_col_width(140.0).show(ui, |ui| {
                ui.label(RichText::new(t.ps2_button).strong());
                ui.label(RichText::new(t.keyboard).strong());
                ui.label(RichText::new(t.controller).strong());
                ui.end_row();
                for (i, b) in self.config.bindings.iter().enumerate() {
                    ui.label(button_label(t, &b.button));
                    for keyboard in [true, false] {
                        let list = if keyboard { &b.keys } else { &b.pad };
                        ui.horizontal_wrapped(|ui| {
                            for name in list {
                                let chip = egui::Button::new(RichText::new(input_label(t, name, keyboard)).color(INK))
                                    .fill(MAROON)
                                    .corner_radius(6.0);
                                if ui.add(chip).on_hover_text(t.click_to_remove).clicked() {
                                    remove = Some((i, keyboard, name.clone()));
                                }
                            }
                            if self.capture == Some((i, keyboard)) {
                                let text = if keyboard { t.press_key } else { t.press_button };
                                if ui.add(egui::Button::new(RichText::new(text).color(INK)).fill(ACCENT)).clicked() {
                                    start = Some(None);
                                }
                            } else if ui.small_button(" + ").on_hover_text(t.add_binding).clicked() {
                                start = Some(Some((i, keyboard)));
                            }
                        });
                    }
                    ui.end_row();
                }
            });
        });
        if let Some((i, keyboard, name)) = remove {
            let b = &mut self.config.bindings[i];
            let list = if keyboard { &mut b.keys } else { &mut b.pad };
            list.retain(|n| *n != name);
        }
        if let Some(s) = start {
            self.capture = s;
        }
    }

    /// Feed a key press to the capture slot, if one is waiting.
    fn capture_key(&mut self, ctx: &egui::Context) {
        let Some(slot) = self.capture else { return };
        let mut got = None;
        ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Key { key, physical_key, pressed: true, repeat: false, .. } = ev {
                    got = Some(winit_key_name(physical_key.unwrap_or(*key)));
                }
            }
            // Modifiers are not keys in egui; the game accepts either side.
            if got.is_none() {
                let m = i.modifiers;
                got = if m.shift { Some(Some("Shift".into())) } else if m.ctrl { Some(Some("Control".into())) } else if m.alt { Some(Some("Alt".into())) } else { None };
            }
        });
        match got {
            Some(Some(name)) if name == "Escape" => self.capture = None,
            Some(Some(name)) if RESERVED_KEYS.contains(&name.as_str()) => self.status = fill(self.tr().reserved_key, &[&name]),
            Some(Some(name)) if slot.1 => self.bind(slot, name),
            Some(None) => self.status = self.tr().cant_bind.into(),
            _ => {}
        }
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let t = self.tr();
        if let Some(child) = &mut self.game {
            if let Ok(Some(status)) = child.try_wait() {
                self.status = if status.success() { t.game_closed.into() } else { fill(t.game_exited, &[&status.to_string()]) };
                self.game = None;
            }
        }
        let (pad_name, pad_pressed) = self.poll_gamepad();
        match (self.capture, pad_pressed) {
            (Some(slot), Some(name)) if !slot.1 => self.bind(slot, name),
            _ => self.capture_key(ctx),
        }

        egui::Panel::top("header").frame(egui::Frame::new().fill(HEADER_BG).inner_margin(16.0)).show(ui, |ui| {
            self.header(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Play, RichText::new(t.settings_tab).size(16.0));
                ui.selectable_value(&mut self.tab, Tab::Controls, RichText::new(t.controls_tab).size(16.0));
                // Top right, so it can be found without reading the current language.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let lang = &mut self.config.language;
                    let current = Lang::ALL.into_iter().find(|l| l.code() == lang);
                    egui::ComboBox::from_id_salt("language").selected_text(current.map_or(t.system_language, Lang::name)).show_ui(ui, |ui| {
                        ui.selectable_value(lang, String::new(), t.system_language);
                        for l in Lang::ALL {
                            ui.selectable_value(lang, l.code().to_string(), l.name());
                        }
                    });
                    ui.label(RichText::new(t.language).color(MUTED));
                });
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.status).color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(fill(t.saved_to, &[freek_config::FILE])).color(MUTED.gamma_multiply(0.7)));
                });
            });
        });
        egui::CentralPanel::default_margins().show(ui, |ui| match self.tab {
            Tab::Play => {
                egui::ScrollArea::vertical().show(ui, |ui| self.play_tab(ui));
            }
            Tab::Controls => self.controls_tab(ui, pad_name.as_deref()),
        });
        self.save();
        // Keep polling the controller and the game process.
        ctx.request_repaint_after(Duration::from_millis(if self.capture.is_some() { 16 } else { 100 }));
    }
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).size(18.0).strong().color(ACCENT));
    egui::Frame::group(ui.style()).fill(GROUP_BG).inner_margin(14.0).corner_radius(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        body(ui);
    });
}

fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut reader = png::Decoder::new(bytes).read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).ok()?;
    Some(egui::ColorImage::from_rgba_unmultiplied([info.width as usize, info.height as usize], &rgba[..info.buffer_size()]))
}

fn res_label(res: &str) -> &'static str {
    match res {
        "1440" => "2560 × 1440",
        "4k" | "2160" => "3840 × 2160 (4K)",
        _ => "1920 × 1080",
    }
}

fn button_label(t: &'static Tr, name: &str) -> &'static str {
    PS2_BUTTONS.iter().position(|b| b.0 == name).map_or("?", |i| t.buttons[i])
}

/// The winit `KeyCode` name the game matches for an egui key; None for keys
/// winit has no physical code for.
fn winit_key_name(key: egui::Key) -> Option<String> {
    let name = format!("{key:?}");
    if name.len() == 1 {
        return Some(format!("Key{name}"));
    }
    if let Some(d) = name.strip_prefix("Num") {
        return Some(format!("Digit{d}"));
    }
    Some(
        match name.as_str() {
            "OpenBracket" => "BracketLeft",
            "CloseBracket" => "BracketRight",
            "Backtick" => "Backquote",
            "Equals" => "Equal",
            "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Escape" | "Tab" | "Backspace" | "Enter" | "Space" | "Insert"
            | "Delete" | "Home" | "End" | "PageUp" | "PageDown" | "Comma" | "Period" | "Minus" | "Semicolon" | "Quote" | "Slash"
            | "Backslash" => return Some(name),
            _ if name.starts_with('F') && name[1..].parse::<u32>().is_ok() => return Some(name),
            _ => return None,
        }
        .to_string(),
    )
}

/// Short display names: "KeyZ" -> "Z", "South" -> "A".
fn input_label(t: &Tr, name: &str, keyboard: bool) -> String {
    if keyboard {
        if let Some(k) = name.strip_prefix("Key").or_else(|| name.strip_prefix("Digit")) {
            return k.to_string();
        }
        return match name {
            "ArrowUp" => "↑",
            "ArrowDown" => "↓",
            "ArrowLeft" => "←",
            "ArrowRight" => "→",
            "BracketLeft" => "[",
            "BracketRight" => "]",
            "Backquote" => "`",
            "Equal" => "=",
            "Minus" => "-",
            "Comma" => ",",
            "Period" => ".",
            "Slash" => "/",
            "Backslash" => "\\",
            "Semicolon" => ";",
            "Quote" => "'",
            other => other,
        }
        .to_string();
    }
    match name {
        "South" => "A",
        "East" => "B",
        "West" => "X",
        "North" => "Y",
        "LeftTrigger" => "LB",
        "RightTrigger" => "RB",
        "LeftTrigger2" => "LT",
        "RightTrigger2" => "RT",
        "Select" => "Back",
        "Mode" => "Guide",
        "LeftThumb" => t.ls_click,
        "RightThumb" => t.rs_click,
        "DPadUp" => return format!("{} ↑", t.dpad),
        "DPadDown" => return format!("{} ↓", t.dpad),
        "DPadLeft" => return format!("{} ←", t.dpad),
        "DPadRight" => return format!("{} →", t.dpad),
        other => other,
    }
    .to_string()
}
