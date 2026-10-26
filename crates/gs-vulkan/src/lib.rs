//! Vulkan renderer for the PS2 Graphics Synthesizer.
//!
//! [`Renderer`] owns the GS state machine (`gs::Gs`) and a hardware
//! backend that draws into render targets at an integer multiple of the
//! PS2 resolution (3x for 1080p, 5x for 4K) and shows the displayed frame
//! buffer in the window.

mod blend;
mod hw;
mod icons;
mod pictures;
mod png;
mod vk;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub use hw::{Config, Stats};

pub struct Renderer {
    pub gs: gs::Gs,
    hw: hw::Hw,
    /// Time spent in GS processing and in presenting, for FREEK_GS_LOG.
    timing: (std::time::Duration, std::time::Duration, u32),
    /// Vblanks that brought new draws (game frames), and the draw count
    /// at the last vblank, for FREEK_GS_LOG.
    game_frames: u32,
    last_draws: u64,
    last_flips: u64,
    since: std::time::Instant,
    last_flip: Option<std::time::Instant>,
}

impl Renderer {
    pub fn new(window: &(impl HasDisplayHandle + HasWindowHandle), size: (u32, u32), cfg: Config) -> Result<Renderer, String> {
        let vk = vk::Vk::new(window, size)?;
        eprintln!(
            "[gs] Vulkan on {} | {}x{} window | {}x internal resolution",
            vk.device_name, vk.swapchain.extent.width, vk.swapchain.extent.height, cfg.scale
        );
        Ok(Renderer { gs: gs::Gs::new(), hw: hw::Hw::new(vk, cfg)?, timing: Default::default(), game_frames: 0, last_draws: 0, last_flips: 0, since: std::time::Instant::now(), last_flip: None })
    }

    /// Process one message from the EE. Returns true after presenting.
    pub fn handle(&mut self, msg: gs::Msg) -> bool {
        let t0 = std::time::Instant::now();
        match msg {
            gs::Msg::Gif(data) => {
                self.gs.write_gif(&data, &mut self.hw);
                self.timing.0 += t0.elapsed();
            }
            gs::Msg::Priv(reg, v) => {
                if std::env::var_os("FREEK_GS_LOG").is_some() {
                    eprintln!("[gs] priv {reg:#x} = {v:#x}");
                }
                if reg == 0x90 && std::env::var_os("FREEK_FRAME_GAPS").is_some() {
                    // Game frames are buffer flips (DISPFB2); report hitches.
                    let now = std::time::Instant::now();
                    if let Some(prev) = self.last_flip {
                        let ms = now.duration_since(prev).as_millis();
                        if ms > 100 {
                            eprintln!("[hitch] {ms} ms between game frames at vblank {}", self.hw.vblank);
                        }
                    }
                    self.last_flip = Some(now);
                }
                self.gs.write_priv(reg, v)
            }
            gs::Msg::Vsync(n) => {
                if std::env::var_os("FREEK_FRAME_GAPS").is_some() && n > self.hw.vblank + 3 && self.hw.vblank > 0 {
                    eprintln!("[hitch] renderer {} vblanks behind at vblank {n}", n - self.hw.vblank);
                }
                self.hw.vblank = n;
                if self.hw.stats.draws != self.last_draws {
                    self.game_frames += 1;
                    self.last_draws = self.hw.stats.draws;
                }
                let tp = std::time::Instant::now();
                self.hw.present(&self.gs);
                if std::env::var_os("FREEK_FRAME_GAPS").is_some() && tp.elapsed().as_millis() > 50 {
                    eprintln!("[hitch] present took {} ms at vblank {n}", tp.elapsed().as_millis());
                }
                self.timing.1 += t0.elapsed();
                self.timing.2 += 1;
                if self.timing.2 == 120 && std::env::var_os("FREEK_GS_LOG").is_some() {
                    let (g, p, n) = self.timing;
                    let fps = n as f64 / self.since.elapsed().as_secs_f64();
                    let secs = self.since.elapsed().as_secs_f64();
                    let (game, flips) = (self.game_frames as f64 / secs, (self.hw.stats.flips - self.last_flips) as f64 / secs);
                    self.last_flips = self.hw.stats.flips;
                    eprintln!("[gs] {fps:.1} fps, drawn {game:.1}/s, shown {flips:.1}/s; per frame: GS {:.2} ms, present {:.2} ms; vblank {}", g.as_secs_f64() * 1e3 / n as f64, p.as_secs_f64() * 1e3 / n as f64, self.hw.vblank);
                    self.timing = Default::default();
                    self.game_frames = 0;
                    self.since = std::time::Instant::now();
                }
                return true;
            }
        }
        false
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.hw.resize((w, h));
    }

    pub fn stats(&self) -> Stats {
        self.hw.stats
    }

    pub fn trace_shot(&mut self, delay: u64) {
        self.hw.trace_shot(delay);
    }

    pub fn trace_in(&mut self, delay: u64) {
        self.hw.trace_in(delay);
    }

    pub fn toggle_recording(&mut self) {
        self.hw.toggle_recording();
    }

    pub fn frame(&self) -> u64 {
        self.hw.frame_no
    }
}
