//! Freekstyle native: boots the recompiled EE executable.
//!
//! Generate the code first:
//!   cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt freekstyle/vu_dumps
//! then:
//!   cargo run --release -p freekstyle -- [options] [game/SLUS_203.67 [disc image]]
//!
//! Settings come from freekstyle.toml in the working directory, or
//! ~/.config/freekstyle when that isn't writable (see freek_config::path;
//! written by freekstyle-launcher, defaults if missing); options given here
//! override it.
//!
//! Options:
//!   --res 1080|1440|4k   window size and matching internal resolution (default 1080)
//!   --scale N            internal resolution multiplier (overrides --res)
//!   --fullscreen         borderless fullscreen
//!   --stretch            fill the window instead of keeping 4:3
//!   --widescreen         16:9: the game renders a wider view, shown
//!                        stretched to 16:9, with wide pictures
//!   --blur-bars          fill the sides of a wide window with a blurred
//!                        copy of the picture instead of black
//!   --headless           no window; print GS statistics
//!   --ps2-icons          keep the original PlayStation button icons
//!   --no-sharp-textures  original texture resolution (no xBRZ upscaling)
//!   --no-hd-textures     ignore the texture packs (game/textures/hd and
//!                        the one built while playing)
//!   --no-build-hd-textures  don't record textures to build the pack from
//!   --mute               no sound (the sound driver still runs)
//!
//! Controls: any gamepad (Xbox layout: A/B/X/Y = Cross/Circle/Square/
//! Triangle by position), or the keyboard: WASD/arrows = D-pad and left
//! stick, J/Space/Enter = A, K = B, U = X, I = Y, P/O = LB/RB, ;/L =
//! LT/RT, T = Start, Backspace = Back; both remappable in freekstyle.toml. F11 toggles
//! fullscreen, F12 captures a frame, Esc quits.
//!
//! The disc image (raw .bin or .iso) defaults to game/Freekstyle.bin.

// On Windows, a release build is a windowed program: no console window
// opens beside it (debug builds keep theirs, for the log).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod gsrec;
mod hdbuild;
mod profile;
#[allow(clippy::all)]
mod gen;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs, process, thread};

use ee_runtime::pad::{button, Input};
use ee_runtime::Cpu;
use freek_input::{ControllerManager, PadMap};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

struct Options {
    elf: String,
    disc: String,
    window: (u32, u32),
    scale: u32,
    fullscreen: bool,
    stretch: bool,
    blur_bars: bool,
    widescreen: bool,
    headless: bool,
    ps2_icons: bool,
    sharp_textures: bool,
    hd_textures: bool,
    build_hd_textures: bool,
    mute: bool,
    emulated_audio: bool,
    keyboard_player: u32,
    stick_deadzone: f32,
    bindings: Vec<freek_config::Binding>,
}

fn options() -> Options {
    let c = freek_config::Config::load(freek_config::path()).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(2);
    });
    let (window, scale) = c.display().unwrap_or_else(|| {
        eprintln!("{}: res must be 1080, 1440 or 4k, got {:?}", freek_config::path().display(), c.res);
        process::exit(2);
    });
    let stick_deadzone = c.deadzone();
    let mut o = Options {
        elf: c.elf,
        disc: c.disc,
        window,
        scale,
        fullscreen: c.fullscreen,
        stretch: c.stretch,
        blur_bars: c.blur_bars,
        widescreen: c.widescreen,
        headless: false,
        ps2_icons: c.ps2_icons,
        sharp_textures: c.sharp_textures,
        hd_textures: c.hd_textures,
        build_hd_textures: c.build_hd_textures,
        mute: c.mute,
        emulated_audio: c.emulated_audio,
        keyboard_player: c.keyboard_player,
        stick_deadzone,
        bindings: c.bindings,
    };
    let mut positional = Vec::new();
    let mut scale: Option<u32> = None;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--res" => match args.next().as_deref() {
                // Internal scale covers the window height: 448 lines x scale.
                Some("1080") => (o.window, o.scale) = ((1920, 1080), 3),
                Some("1440") => (o.window, o.scale) = ((2560, 1440), 4),
                Some("4k") | Some("2160") => (o.window, o.scale) = ((3840, 2160), 5),
                other => {
                    eprintln!("--res expects 1080, 1440 or 4k, got {other:?}");
                    process::exit(2);
                }
            },
            "--scale" => scale = args.next().and_then(|v| v.parse().ok()),
            "--fullscreen" => o.fullscreen = true,
            "--stretch" => o.stretch = true,
            "--blur-bars" => o.blur_bars = true,
            "--widescreen" => o.widescreen = true,
            "--headless" => o.headless = true,
            "--ps2-icons" => o.ps2_icons = true,
            "--no-sharp-textures" => o.sharp_textures = false,
            "--no-hd-textures" => o.hd_textures = false,
            "--no-build-hd-textures" => o.build_hd_textures = false,
            "--mute" => o.mute = true,
            "--emulated-audio" => o.emulated_audio = true,
            _ if a.starts_with("--") => {
                eprintln!("unknown option {a}");
                process::exit(2);
            }
            _ => positional.push(a),
        }
    }
    if let Some(s) = scale {
        o.scale = s.clamp(1, 8);
    }
    let mut positional = positional.into_iter();
    if let Some(p) = positional.next() {
        o.elf = p;
    }
    if let Some(p) = positional.next() {
        o.disc = p;
    }
    o
}

/// Recompiled code uses the host stack for guest calls; give it room.
const STACK_SIZE: usize = 512 << 20;

/// Request 1 ms system timer resolution on Windows so short sleeps in the EE
/// idle loop and IOP audio ring do not stretch to the 15.6 ms default tick.
#[cfg(target_os = "windows")]
struct WinTimerPeriod {
    active: bool,
}

#[cfg(target_os = "windows")]
impl WinTimerPeriod {
    fn enable() -> Self {
        #[link(name = "winmm")]
        extern "system" {
            fn timeBeginPeriod(period: u32) -> u32;
        }
        let active = unsafe { timeBeginPeriod(1) == 0 };
        Self { active }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WinTimerPeriod {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        #[link(name = "winmm")]
        extern "system" {
            fn timeEndPeriod(period: u32) -> u32;
        }
        unsafe {
            timeEndPeriod(1);
        }
    }
}

fn main() {
    freek_config::crash::install("Freekstyle");
    let opts = options();
    ee_runtime::hle::WIDESCREEN.store(opts.widescreen, std::sync::atomic::Ordering::Relaxed);
    let path = opts.elf.clone();
    let disc = ee_runtime::disc::Disc::open(opts.disc.as_ref()).unwrap_or_else(|e| {
        eprintln!("disc image: {e}");
        process::exit(1);
    });
    let bytes = fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        process::exit(1);
    });
    if ee_runtime::fnv1a(&bytes) != gen::ELF_HASH {
        eprintln!("{path} is not the executable the code was generated from; re-run ee-recomp");
        process::exit(1);
    }
    #[cfg(target_os = "windows")]
    let _win_timer = WinTimerPeriod::enable();

    // The IOP (sound driver and SPU2) runs on its own thread, paced by the
    // audio device.
    let link = std::sync::Arc::new(ps2_iop::Link::default());
    let ring = std::sync::Arc::new(ps2_iop::AudioRing::default());
    ps2_iop::native::set_emulated(opts.emulated_audio);
    ps2_iop::spawn(link.clone(), ring.clone(), audio::LATENCY);
    audio::start(ring, opts.mute);

    // Converted pictures live next to the disc image (see lsc2png).
    let images_dir = std::path::Path::new(&opts.disc).parent().unwrap_or(std::path::Path::new(".")).join("images");
    let (gs_tx, gs_rx) = std::sync::mpsc::sync_channel(4096);
    let replay = env::var("FREEK_GS_REPLAY").ok();
    let frame_gate = (!opts.headless && replay.is_none()).then(|| Arc::new(ee_runtime::io::FrameGate::new(MAX_FRAMES_IN_FLIGHT)));
    if replay.is_none() {
    let ee_gate = frame_gate.clone();
    thread::Builder::new()
        .name("ee-main".into())
        .stack_size(STACK_SIZE)
        .spawn(move || {
            profile::start_if_requested("FREEK_PROFILE", "profile.txt");
            let mut c = Cpu::new(&gen::FUNCS);
            c.iop.disc = Some(disc);
            c.iop.link = Some(link);
            c.iop.images = std::path::Path::new(&images_dir).to_path_buf();
            c.iop.patch_disc_files();
            c.mem.io.gs = Some(gs_tx);
            c.mem.io.frame_gate = ee_gate;
            // FREEK_VU_INTERP: interpret all VU microcode (debugging aid).
            if env::var_os("FREEK_VU_INTERP").is_none() {
                c.mem.set_vu_programs(ee_runtime::vumicro::Programs(&gen::vu_programs::VU_PROGRAMS));
            }
            let entry = ee_runtime::load_elf(&mut c, &bytes).expect("load ELF");
            if !ee_runtime::quiet() {
                eprintln!("[boot] {} functions, entry {entry:#x}", gen::FUNCS.len());
            }
            ee_runtime::kernel::run_thread_body(&mut c, entry);
            eprintln!("[boot] main thread returned");
            // Other guest threads may still hold references to `c`.
            process::exit(0);
        })
        .expect("spawn EE thread");
    thread::spawn(watchdog);
    }
    let gs_rx = match (replay, env::var("FREEK_GS_RECORD").ok()) {
        (Some(path), _) => gsrec::replay(path),
        (None, Some(path)) => gsrec::record(gs_rx, path),
        _ => gs_rx,
    };

    if opts.headless {
        headless_gs(gs_rx);
        return;
    }
    let event_loop = EventLoop::new().unwrap_or_else(|e| {
        eprintln!("cannot open a window: {e}");
        process::exit(1);
    });
    event_loop.set_control_flow(ControlFlow::Poll);
    let controllers = ControllerManager::new().map_err(|e| eprintln!("[input] gamepads unavailable: {e}")).ok();
    let key_map = key_map(&opts.bindings);
    let pad_map = PadMap::compile(&opts.bindings);
    let keyboard_player = (opts.keyboard_player.clamp(1, 2) - 1) as usize;
    let mut app = App {
        opts,
        rx: gs_rx,
        pending: VecDeque::new(),
        pending_frames: 0,
        rx_closed: false,
        window: None,
        renderer: None,
        last_title: Instant::now(),
        last_frame: 0,
        controllers,
        keyboard_player,
        focused: true,
        keys: Vec::new(),
        last_buttons: 0,
        traces: 0,
        recording: false,
        trace_button: env::var("FREEK_TRACE_BUTTON").ok().and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok()),
        key_map,
        pad_map,
        frame_gate,
    };
    // A replay draws a recording, not the game: nothing new to record.
    app.recording = app.opts.build_hd_textures && env::var_os("FREEK_GS_REPLAY").is_none() && hdbuild::available();
    profile::start_if_requested("FREEK_PROFILE_GS", "profile_gs.txt");
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop: {e}");
    }
    if let Some(c) = &mut app.controllers {
        c.set_rumble([(0, 0); 2], false);
    }
    drop(app);
    #[cfg(target_os = "windows")]
    drop(_win_timer);
    process::exit(0);
}

const MAX_PENDING_MSGS: usize = 4096;
const MAX_PENDING_FRAMES: usize = 3;
/// Only a guard against a runaway renderer. A vsynced renderer normally runs
/// 3-5 frames behind (swapchain queue); smaller limits made the EE thread wait
/// ~25 ms every ~6 s on Windows + Arc at 60 Hz, a visible skip.
const MAX_FRAMES_IN_FLIGHT: usize = 30;

fn is_frame_msg(m: &gs::Msg) -> bool {
    matches!(m, gs::Msg::Vsync(_) | gs::Msg::Movie(Some(_)))
}

fn push_pending(pending: &mut VecDeque<gs::Msg>, pending_frames: &mut usize, msg: gs::Msg) {
    if is_frame_msg(&msg) {
        *pending_frames += 1;
    }
    pending.push_back(msg);
}

fn pop_pending(pending: &mut VecDeque<gs::Msg>, pending_frames: &mut usize) -> Option<gs::Msg> {
    let msg = pending.pop_front()?;
    if is_frame_msg(&msg) {
        *pending_frames = pending_frames.saturating_sub(1);
    }
    Some(msg)
}

fn drain_rx(rx: &Receiver<gs::Msg>, pending: &mut VecDeque<gs::Msg>, pending_frames: &mut usize) -> bool {
    while pending.len() < MAX_PENDING_MSGS && *pending_frames < MAX_PENDING_FRAMES {
        match rx.try_recv() {
            Ok(msg) => push_pending(pending, pending_frames, msg),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
    true
}

struct App {
    opts: Options,
    rx: Receiver<gs::Msg>,
    pending: VecDeque<gs::Msg>,
    pending_frames: usize,
    rx_closed: bool,
    window: Option<Window>,
    renderer: Option<gs_vulkan::Renderer>,
    last_title: Instant,
    last_frame: u64,
    controllers: Option<ControllerManager>,
    /// The player (0 or 1) the keyboard controls.
    keyboard_player: usize,
    focused: bool,
    /// Mapped keys held down; several keys can share a button.
    keys: Vec<KeyCode>,
    last_buttons: u16,
    traces: u32,
    /// Recording textures for the launcher to upscale (`hdbuild`).
    recording: bool,
    trace_button: Option<u16>,
    /// Bound keys (winit KeyCode names) and gamepad buttons, with the PS2
    /// buttons each one presses.
    key_map: HashMap<String, u16>,
    pad_map: PadMap,
    frame_gate: Option<Arc<ee_runtime::io::FrameGate>>,
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(gate) = &self.frame_gate {
            gate.close();
        }
    }
}

/// A key's name in freekstyle.toml: its winit KeyCode name, except that
/// modifiers bind by either side ("Shift", not "ShiftLeft").
fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "Shift".into(),
        KeyCode::ControlLeft | KeyCode::ControlRight => "Control".into(),
        KeyCode::AltLeft | KeyCode::AltRight => "Alt".into(),
        _ => format!("{code:?}"),
    }
}

/// Resolve the configured keyboard bindings into a winit KeyCode lookup table.
fn key_map(bindings: &[freek_config::Binding]) -> HashMap<String, u16> {
    let mut keys = HashMap::new();
    for b in bindings {
        let Some(bit) = freek_config::button_bit(&b.button) else { continue };
        for k in &b.keys {
            *keys.entry(k.clone()).or_insert(0) |= bit;
        }
    }
    keys
}

impl App {
    /// Run each player's pad motors at the speeds the game asked for. An
    /// effect's strength is fixed, so a new speed needs a new effect.
    fn update_rumble(&mut self) {
        if let Some(c) = &mut self.controllers {
            c.set_rumble(ee_runtime::pad::motors(), self.focused);
        }
    }

    /// Combine gamepad and keyboard state into the two PS2 controllers.
    fn update_input(&mut self) {
        let keys = self.keys.iter().filter_map(|&k| self.key_map.get(&key_name(k))).fold(0, |a, b| a | b);
        let mut players: [Option<Input>; 2] = match &mut self.controllers {
            Some(c) => {
                c.poll_events();
                c.read_players(&self.pad_map, self.opts.stick_deadzone, self.focused)
            }
            None => [None; 2],
        };
        // Player 1 is always plugged in; the keyboard plugs in its player.
        players[0].get_or_insert_with(Input::default);
        let input = players[self.keyboard_player].get_or_insert_with(Input::default);
        input.buttons |= keys;
        if keys & button::L2 != 0 {
            input.l2 = 0xff;
        }
        if keys & button::R2 != 0 {
            input.r2 = 0xff;
        }
        // D-pad keys (WASD, arrows) also push the left stick, so the keyboard
        // steers whether the game reads the D-pad or the stick. Per axis the
        // larger deflection wins: a resting or drifting gamepad stick never
        // cancels a key.
        let dir = |neg: u16, pos: u16| match (keys & neg != 0, keys & pos != 0) {
            (true, false) => 0x00,
            (false, true) => 0xff,
            _ => 0x80,
        };
        let merge = |pad: u8, key: u8| if (key as i32 - 128).abs() > (pad as i32 - 128).abs() { key } else { pad };
        let (left_x, left_y) = (dir(button::LEFT, button::RIGHT), dir(button::UP, button::DOWN));
        input.sticks[2] = merge(input.sticks[2], left_x);
        input.sticks[3] = merge(input.sticks[3], left_y);
        self.update_rumble();
        // FREEK_TRACE_BUTTON=<mask>: half a second after that pad button is
        // pressed, trace a frame and save its picture (effects too quick to
        // catch by hand), up to ten times.
        let buttons = players[0].map_or(0, |i| i.buttons);
        if let Some(mask) = self.trace_button {
            if buttons & mask != 0 && self.last_buttons & mask == 0 && self.traces < 10 {
                if let Some(r) = &mut self.renderer {
                    self.traces += 1;
                    r.trace_shot(30);
                }
            }
        }
        self.last_buttons = buttons;
        ee_runtime::pad::set_input(players);
    }
}

fn window_icon() -> Option<winit::window::Icon> {
    let mut reader = png::Decoder::new(&include_bytes!("../assets/icon/256.png")[..]).read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).ok()?;
    rgba.truncate(info.buffer_size());
    winit::window::Icon::from_rgba(rgba, info.width, info.height).ok()
}

impl ApplicationHandler for App {
    /// Free the renderer, then the window, while the event loop still holds
    /// the Wayland connection: run_app consumes the loop, so destroying the
    /// swapchain after it returned used a closed connection (SIGSEGV in
    /// libwayland-client on Linux).
    fn exiting(&mut self, _el: &ActiveEventLoop) {
        if let Some(gate) = &self.frame_gate {
            gate.close();
        }
        self.renderer = None;
        self.window = None;
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (w, h) = self.opts.window;
        // The app ID matches freekstyle.desktop, where Wayland desktops find
        // the icon; X11 takes the pixels directly.
        let mut attrs = Window::default_attributes()
            .with_title("Freekstyle")
            .with_inner_size(PhysicalSize::new(w, h))
            .with_window_icon(window_icon());
        #[cfg(target_os = "linux")]
        {
            attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, "freekstyle", "freekstyle");
            attrs = winit::platform::x11::WindowAttributesExtX11::with_name(attrs, "freekstyle", "freekstyle");
        }
        if self.opts.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = el.create_window(attrs).unwrap_or_else(|e| {
            eprintln!("create window: {e}");
            process::exit(1);
        });
        let size = window.inner_size();
        let game_dir = std::path::Path::new(&self.opts.disc).parent().unwrap_or(std::path::Path::new("."));
        let pictures = game_dir.join("images");
        let texture_packs = if self.opts.hd_textures { vec![freek_config::user_texture_pack(), game_dir.join("textures/hd")] } else { Vec::new() };
        let texture_dump = self.recording.then(freek_config::texture_dump);
        // Widescreen stretches the frame by 4/3: render that much wider.
        let scale_x = if self.opts.widescreen { (self.opts.scale * 4).div_ceil(3) } else { self.opts.scale };
        let cfg = gs_vulkan::Config {
            scale: self.opts.scale,
            scale_x,
            widescreen: self.opts.widescreen,
            stretch: self.opts.stretch, blur_bars: self.opts.blur_bars, xbox_icons: !self.opts.ps2_icons,
            sharp_textures: self.opts.sharp_textures,
            pictures: Some(pictures),
            texture_packs,
            texture_dump,
            pipeline_cache: Some(freek_config::data_dir().join("vk_pipeline_cache.bin")),
        };
        let renderer = gs_vulkan::Renderer::new(&window, (size.width, size.height), cfg).unwrap_or_else(|e| {
            eprintln!("Vulkan: {e}");
            process::exit(1);
        });
        self.window = Some(window);
        self.renderer = Some(renderer);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(gate) = &self.frame_gate {
                    gate.close();
                }
                el.exit();
            }
            WindowEvent::Resized(s) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(s.width, s.height);
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat: false, .. }, .. }
                if self.key_map.contains_key(&key_name(code)) =>
            {
                self.keys.retain(|&k| k != code);
                if state == ElementState::Pressed {
                    self.keys.push(code);
                }
            }
            // Releases while unfocused never arrive: drop everything held.
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused {
                    self.keys.clear();
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => match logical_key {
                Key::Named(NamedKey::Escape) => {
                    if let Some(gate) = &self.frame_gate {
                        gate.close();
                    }
                    el.exit();
                }
                Key::Named(NamedKey::F12) => {
                    if let Some(r) = &mut self.renderer {
                        r.toggle_recording();
                    }
                }
                Key::Named(NamedKey::F11) => {
                    if let Some(w) = &self.window {
                        let fs = if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) };
                        w.set_fullscreen(fs);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.update_input();
        if self.renderer.is_none() {
            return;
        }
        // Drain every turn so the pending queue always holds the newest frames.
        // Draining only after a finished batch made 60 Hz vsync play visibly
        // skip on Windows (bisected to this loop).
        if !self.rx_closed && !drain_rx(&self.rx, &mut self.pending, &mut self.pending_frames) {
            self.rx_closed = true;
        }
        if !self.rx_closed && self.pending_frames == 0 {
            // Nothing ready to present yet: wait up to 20 ms, bounded so the
            // window stays responsive when the game is busy loading.
            let deadline = Instant::now() + Duration::from_millis(20);
            while self.pending.len() < MAX_PENDING_MSGS {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(msg) => {
                        let done = is_frame_msg(&msg);
                        push_pending(&mut self.pending, &mut self.pending_frames, msg);
                        if done {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        self.rx_closed = true;
                        break;
                    }
                }
            }
            if !self.rx_closed && !drain_rx(&self.rx, &mut self.pending, &mut self.pending_frames) {
                self.rx_closed = true;
            }
        }
        // Bound catch-up per event-loop turn so Windows always gets control
        // back to pump window messages and never marks the window Not Responding.
        let budget = Instant::now() + Duration::from_millis(25);
        let mut consumed_vsyncs = 0usize;
        if let Some(r) = &mut self.renderer {
            while let Some(msg) = pop_pending(&mut self.pending, &mut self.pending_frames) {
                let is_vsync = matches!(&msg, gs::Msg::Vsync(_));
                let frame_msg = is_frame_msg(&msg);
                let within_budget = Instant::now() < budget;
                if frame_msg && within_budget && !self.rx_closed && !drain_rx(&self.rx, &mut self.pending, &mut self.pending_frames) {
                    self.rx_closed = true;
                }
                let has_newer = frame_msg && within_budget && self.pending_frames > 0;
                let presented = r.handle_frame(msg, !has_newer);
                if is_vsync {
                    consumed_vsyncs += 1;
                }
                if presented {
                    break;
                }
                if Instant::now() >= budget {
                    break;
                }
            }
        }
        if consumed_vsyncs > 0 {
            if let Some(gate) = &self.frame_gate {
                gate.release(consumed_vsyncs);
            }
        }
        if self.rx_closed && self.pending.is_empty() {
            if let Some(gate) = &self.frame_gate {
                gate.close();
            }
            el.exit();
            return;
        }
        if self.last_title.elapsed() >= Duration::from_secs(1) {
            let (frames, st) = self.renderer.as_ref().map(|r| (r.frame(), r.stats())).unwrap_or((0, Default::default()));
            let diff = frames - self.last_frame;
            if let Some(w) = &self.window {
                w.set_title(&format!("Freekstyle | {diff} fps | {}x | draws {} batches {}", self.opts.scale, st.draws, st.batches));
            }
            self.last_frame = frames;
            self.last_title = Instant::now();
        }
    }
}

/// Report when guest code stops entering new functions, which usually means
/// it is spinning on hardware or a kernel object we don't emulate yet.
fn watchdog() {
    use ee_runtime::cpu::{ENTER_COUNT, LAST_ENTERED};
    let mut last = 0;
    let mut reported = 0;
    let mut last_poll = 0;
    let heartbeat = env::var_os("FREEK_HEARTBEAT").is_some();
    loop {
        thread::sleep(Duration::from_secs(2));
        let n = ENTER_COUNT.load(Relaxed);
        let f = LAST_ENTERED.load(Relaxed);
        if n == last && f != reported {
            eprintln!("[watchdog] no function calls for 2s; stuck inside or after fn {f:#010x} ({n} calls so far)");
            reported = f;
        }
        if heartbeat {
            let (runs, cycles, ns) = std::mem::take(&mut *ee_runtime::vumicro::STATS.lock().unwrap());
            let poll = ee_runtime::cpu::POLL_US.load(Relaxed);
            eprintln!(
                "[heartbeat] in fn {f:#010x}, {} calls in the last 2s; VU: {runs} programs ({} VU1, {} XGKICKs), {cycles} cycles, {:.0} ms; polling {:.0} ms; waiting for VU1 {:.0} ms",
                n - last,
                ee_runtime::dma::VU1_RUNS.swap(0, Relaxed),
                ee_runtime::dma::PATH1_PACKETS.swap(0, Relaxed),
                ns as f64 / 1e6,
                (poll - last_poll) as f64 / 1e3,
                ee_runtime::vu1::WAIT_US.swap(0, Relaxed) as f64 / 1e3
            );
            last_poll = poll;
        }
        last = n;
    }
}

/// Run the GS without a renderer and report what it receives each second.
fn headless_gs(rx: Receiver<gs::Msg>) {
    #[derive(Default)]
    struct Count {
        shapes: [u64; 4],
        writes: u64,
        textured: u64,
    }
    impl gs::Backend for Count {
        fn draw(&mut self, g: &gs::Gs, shape: gs::Shape, _v: &[gs::Vertex]) {
            self.shapes[shape as usize] += 1;
            self.textured += u64::from(g.attrs().textured());
        }
        fn mem_written(&mut self, _g: &gs::Gs, _r: gs::MemRect) {
            self.writes += 1;
        }
    }
    let mut g = gs::Gs::new();
    let mut count = Count::default();
    let mut frames = 0u64;
    // Displayed-buffer changes: the game's frame rate.
    let (mut flips, mut shown) = (0u64, 0u64);
    while let Ok(msg) = rx.recv() {
        match msg {
            gs::Msg::Gif(d) => g.write_gif(&d, &mut count),
            gs::Msg::Priv(r, v) => g.write_priv(r, v),
            gs::Msg::Movie(_) => {}
            gs::Msg::Vsync(_) => {
                frames += 1;
                if g.display.dispfb[1] != shown {
                    shown = g.display.dispfb[1];
                    flips += 1;
                }
                if frames % 60 == 0 {
                    let d = &g.display;
                    eprintln!(
                        "[gs] {}s: {flips} fps, points/lines/tris/sprites {:?} textured {} uploads {} packets {} | pmode {:#x} dispfb1 {:#x} dispfb2 {:#x} display2 {:#x}",
                        frames / 60, count.shapes, count.textured, count.writes, g.stats.packets, d.pmode, d.dispfb[0], d.dispfb[1], d.display[1]
                    );
                    count = Count::default();
                    flips = 0;
                }
            }
        }
    }
}
