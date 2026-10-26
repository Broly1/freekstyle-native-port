//! Freekstyle native: boots the recompiled EE executable.
//!
//! Generate the code first:
//!   cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt freekstyle/vu_dumps
//! then:
//!   cargo run --release -p freekstyle -- [options] [game/SLUS_203.67 [disc image]]
//!
//! Settings come from freekstyle.toml in the working directory (written by
//! freekstyle-launcher; defaults if missing); options given here override it.
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
//!   --mute               no sound (the sound driver still runs)
//!
//! Controls: any gamepad (Xbox layout: A/B/X/Y = Cross/Circle/Square/
//! Triangle by position), or the keyboard: WASD/arrows = D-pad and left
//! stick, J/Space = A, K = B, U = X, I = Y, Y/O = LB/RB, H/L = LT/RT,
//! Enter = Start, Backspace = Back; both remappable in freekstyle.toml. F11 toggles
//! fullscreen, F12 captures a frame, Esc quits.
//!
//! The disc image (raw .bin or .iso) defaults to game/Freekstyle.bin.

mod audio;
mod profile;
#[allow(clippy::all)]
mod gen;

use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{env, fs, process, thread};

use ee_runtime::pad::{button, Input};
use ee_runtime::Cpu;
use gilrs::{Axis, Button, EventType, Gilrs};
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
    mute: bool,
    bindings: Vec<freek_config::Binding>,
}

fn options() -> Options {
    let c = freek_config::Config::load(freek_config::FILE.as_ref()).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(2);
    });
    let (window, scale) = c.display().unwrap_or_else(|| {
        eprintln!("{}: res must be 1080, 1440 or 4k, got {:?}", freek_config::FILE, c.res);
        process::exit(2);
    });
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
        mute: c.mute,
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
            "--mute" => o.mute = true,
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

fn main() {
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

    // The IOP (sound driver and SPU2) runs on its own thread, paced by the
    // audio device.
    let link = std::sync::Arc::new(ps2_iop::Link::default());
    let ring = std::sync::Arc::new(ps2_iop::AudioRing::default());
    ps2_iop::spawn(link.clone(), ring.clone(), audio::LATENCY);
    let _stream = audio::start(ring, opts.mute);

    // Converted pictures live next to the disc image (see lsc2png).
    let images_dir = std::path::Path::new(&opts.disc).parent().unwrap_or(std::path::Path::new(".")).join("images");
    let (gs_tx, gs_rx) = std::sync::mpsc::channel();
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
            // FREEK_VU_INTERP: interpret all VU microcode (debugging aid).
            if env::var_os("FREEK_VU_INTERP").is_none() {
                c.mem.vu_programs = ee_runtime::vumicro::Programs(&gen::vu_programs::VU_PROGRAMS);
            }
            let entry = ee_runtime::load_elf(&mut c, &bytes).expect("load ELF");
            eprintln!("[boot] {} functions, entry {entry:#x}", gen::FUNCS.len());
            ee_runtime::kernel::run_thread_body(&mut c, entry);
            eprintln!("[boot] main thread returned");
            // Other guest threads may still hold references to `c`.
            process::exit(0);
        })
        .expect("spawn EE thread");
    thread::spawn(watchdog);

    if opts.headless {
        headless_gs(gs_rx);
        return;
    }
    let event_loop = EventLoop::new().unwrap_or_else(|e| {
        eprintln!("cannot open a window: {e}");
        process::exit(1);
    });
    event_loop.set_control_flow(ControlFlow::Poll);
    let gilrs = Gilrs::new().map_err(|e| eprintln!("[input] gamepads unavailable: {e}")).ok();
    if let Some(g) = &gilrs {
        for (_, pad) in g.gamepads() {
            eprintln!("[input] gamepad: {}", pad.name());
        }
    }
    let (key_map, pad_map) = input_maps(&opts.bindings);
    let mut app = App { opts, rx: gs_rx, window: None, renderer: None, last_title: Instant::now(), last_frame: 0, gilrs, keys: Vec::new(), key_map, pad_map };
    profile::start_if_requested("FREEK_PROFILE_GS", "profile_gs.txt");
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop: {e}");
    }
    process::exit(0);
}

struct App {
    opts: Options,
    rx: Receiver<gs::Msg>,
    window: Option<Window>,
    renderer: Option<gs_vulkan::Renderer>,
    last_title: Instant,
    last_frame: u64,
    gilrs: Option<Gilrs>,
    /// Mapped keys held down; several keys can share a button.
    keys: Vec<KeyCode>,
    /// Bound keys (winit KeyCode names) and gamepad buttons, with the PS2
    /// buttons each one presses.
    key_map: HashMap<String, u16>,
    pad_map: Vec<(Button, u16)>,
}

/// gilrs buttons by the names freekstyle.toml uses (their Debug names).
const PAD_BUTTONS: [Button; 19] = [
    Button::South,
    Button::East,
    Button::North,
    Button::West,
    Button::C,
    Button::Z,
    Button::LeftTrigger,
    Button::LeftTrigger2,
    Button::RightTrigger,
    Button::RightTrigger2,
    Button::Select,
    Button::Start,
    Button::Mode,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];

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

/// Resolve the bindings into lookup tables; unknown names are reported
/// and skipped.
fn input_maps(bindings: &[freek_config::Binding]) -> (HashMap<String, u16>, Vec<(Button, u16)>) {
    let mut keys = HashMap::new();
    let mut pad = Vec::new();
    for b in bindings {
        let Some(bit) = freek_config::button_bit(&b.button) else { continue };
        for k in &b.keys {
            *keys.entry(k.clone()).or_insert(0) |= bit;
        }
        for name in &b.pad {
            match PAD_BUTTONS.iter().find(|p| format!("{p:?}") == *name) {
                Some(&p) => pad.push((p, bit)),
                None => eprintln!("[input] {}: unknown gamepad button {name:?}", freek_config::FILE),
            }
        }
    }
    (keys, pad)
}

impl App {
    /// Combine gamepad and keyboard state into the PS2 controller.
    fn update_input(&mut self) {
        let keys = self.keys.iter().filter_map(|&k| self.key_map.get(&key_name(k))).fold(0, |a, b| a | b);
        let mut input = Input { buttons: keys, ..Input::default() };
        if let Some(g) = &mut self.gilrs {
            while let Some(ev) = g.next_event() {
                // Logged so a flaky controller shows up next to the game's output.
                match ev.event {
                    EventType::Connected => eprintln!("[input] gamepad connected: {}", g.gamepad(ev.id).name()),
                    EventType::Disconnected => eprintln!("[input] gamepad disconnected: {}", g.gamepad(ev.id).name()),
                    _ => {}
                }
            }
            if let Some((_, pad)) = g.gamepads().find(|(_, p)| p.is_connected()) {
                // Analog triggers press past a quarter; whatever is bound to
                // L2/R2 also gives their pressure.
                for &(b, m) in &self.pad_map {
                    let v = pad.button_data(b).map(|d| d.value()).unwrap_or(0.0);
                    let analog = matches!(b, Button::LeftTrigger2 | Button::RightTrigger2);
                    if if analog { v > 0.25 } else { pad.is_pressed(b) } {
                        input.buttons |= m;
                    }
                    let pressure = (v * 255.0) as u8;
                    if m & button::L2 != 0 {
                        input.l2 = input.l2.max(pressure);
                    }
                    if m & button::R2 != 0 {
                        input.r2 = input.r2.max(pressure);
                    }
                }
                // PS2 sticks: 0 = left/up, 0x80 = centre, 0xff = right/down.
                let axis = |a, invert: bool| {
                    let v = pad.value(a);
                    let v = if invert { -v } else { v };
                    (128.0 + v * 127.5).clamp(0.0, 255.0) as u8
                };
                input.sticks = [axis(Axis::RightStickX, false), axis(Axis::RightStickY, true), axis(Axis::LeftStickX, false), axis(Axis::LeftStickY, true)];
            }
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
        input.sticks[2] = merge(input.sticks[2], dir(button::LEFT, button::RIGHT));
        input.sticks[3] = merge(input.sticks[3], dir(button::UP, button::DOWN));
        ee_runtime::pad::set_input(input);
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
        attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, "freekstyle", "freekstyle");
        attrs = winit::platform::x11::WindowAttributesExtX11::with_name(attrs, "freekstyle", "freekstyle");
        if self.opts.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = el.create_window(attrs).unwrap_or_else(|e| {
            eprintln!("create window: {e}");
            process::exit(1);
        });
        let size = window.inner_size();
        let pictures = std::path::Path::new(&self.opts.disc).parent().unwrap_or(std::path::Path::new(".")).join("images");
        // Widescreen stretches the frame by 4/3: render that much wider.
        let scale_x = if self.opts.widescreen { (self.opts.scale * 4).div_ceil(3) } else { self.opts.scale };
        let cfg = gs_vulkan::Config {
            scale: self.opts.scale,
            scale_x,
            widescreen: self.opts.widescreen,
            stretch: self.opts.stretch, blur_bars: self.opts.blur_bars, xbox_icons: !self.opts.ps2_icons,
            sharp_textures: self.opts.sharp_textures,
            pictures: Some(pictures),
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
            WindowEvent::CloseRequested => el.exit(),
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
            WindowEvent::Focused(false) => self.keys.clear(),
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => match logical_key {
                Key::Named(NamedKey::Escape) => el.exit(),
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
        let Some(r) = &mut self.renderer else { return };
        // Run the GS until it presents a frame, bounded so the window stays
        // responsive when the game is busy loading.
        let deadline = Instant::now() + Duration::from_millis(20);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(msg) => {
                    // The event loop polls, so the next frame continues here.
                    if r.handle(msg) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    el.exit();
                    return;
                }
            }
        }
        if self.last_title.elapsed() >= Duration::from_secs(1) {
            let frames = r.frame() - self.last_frame;
            let st = r.stats();
            if let Some(w) = &self.window {
                w.set_title(&format!("Freekstyle | {frames} fps | {}x | draws {} batches {}", self.opts.scale, st.draws, st.batches));
            }
            self.last_frame = r.frame();
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
                "[heartbeat] in fn {f:#010x}, {} calls in the last 2s; VU: {runs} programs ({} VU1, {} XGKICKs), {cycles} cycles, {:.0} ms; polling {:.0} ms",
                n - last,
                ee_runtime::dma::VU1_RUNS.swap(0, Relaxed),
                ee_runtime::dma::PATH1_PACKETS.swap(0, Relaxed),
                ns as f64 / 1e6,
                (poll - last_poll) as f64 / 1e3
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
