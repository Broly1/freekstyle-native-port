//! Freekstyle native: boots the recompiled EE executable.
//!
//! Generate the code first:
//!   cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt
//! then:
//!   cargo run --release -p freekstyle -- [options] [game/SLUS_203.67 [disc image]]
//!
//! Options:
//!   --res 1080|1440|4k   window size and matching internal resolution (default 1080)
//!   --scale N            internal resolution multiplier (overrides --res)
//!   --fullscreen         borderless fullscreen
//!   --stretch            fill the window instead of keeping 4:3
//!   --headless           no window; print GS statistics
//!
//! The disc image (raw .bin or .iso) defaults to game/Freekstyle.bin.

#[allow(clippy::all)]
mod gen;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{env, fs, process, thread};

use ee_runtime::Cpu;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

struct Options {
    elf: String,
    disc: String,
    window: (u32, u32),
    scale: u32,
    fullscreen: bool,
    stretch: bool,
    headless: bool,
}

fn options() -> Options {
    let mut o = Options {
        elf: "game/SLUS_203.67".into(),
        disc: "game/Freekstyle.bin".into(),
        window: (1920, 1080),
        scale: 3,
        fullscreen: false,
        stretch: false,
        headless: false,
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
            "--headless" => o.headless = true,
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

    let (gs_tx, gs_rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ee-main".into())
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let mut c = Cpu::new(&gen::FUNCS);
            c.iop.disc = Some(disc);
            c.mem.io.gs = Some(gs_tx);
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
    let mut app = App { opts, rx: gs_rx, window: None, renderer: None, last_title: Instant::now(), last_frame: 0 };
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
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (w, h) = self.opts.window;
        let mut attrs = Window::default_attributes().with_title("Freekstyle").with_inner_size(PhysicalSize::new(w, h));
        if self.opts.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = el.create_window(attrs).unwrap_or_else(|e| {
            eprintln!("create window: {e}");
            process::exit(1);
        });
        let size = window.inner_size();
        let cfg = gs_vulkan::Config { scale: self.opts.scale, stretch: self.opts.stretch };
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
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => match logical_key {
                Key::Named(NamedKey::Escape) => el.exit(),
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
            eprintln!("[heartbeat] in fn {f:#010x}, {} calls in the last 2s", n - last);
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
    while let Ok(msg) = rx.recv() {
        match msg {
            gs::Msg::Gif(d) => g.write_gif(&d, &mut count),
            gs::Msg::Priv(r, v) => g.write_priv(r, v),
            gs::Msg::Vsync => {
                frames += 1;
                if frames % 60 == 0 {
                    let d = &g.display;
                    eprintln!(
                        "[gs] {}s: points/lines/tris/sprites {:?} textured {} uploads {} packets {} | pmode {:#x} dispfb1 {:#x} dispfb2 {:#x} display2 {:#x}",
                        frames / 60, count.shapes, count.textured, count.writes, g.stats.packets, d.pmode, d.dispfb[0], d.dispfb[1], d.display[1]
                    );
                    count = Count::default();
                }
            }
        }
    }
}
