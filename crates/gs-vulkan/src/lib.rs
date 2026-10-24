//! Vulkan renderer for the PS2 Graphics Synthesizer.
//!
//! [`Renderer`] owns the GS state machine (`gs::Gs`) and a hardware
//! backend that draws into render targets at an integer multiple of the
//! PS2 resolution (3x for 1080p, 5x for 4K) and shows the displayed frame
//! buffer in the window.

mod blend;
mod hw;
mod icons;
mod png;
mod vk;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub use hw::{Config, Stats};

pub struct Renderer {
    pub gs: gs::Gs,
    hw: hw::Hw,
    /// Time spent in GS processing and in presenting, for FREEK_GS_LOG.
    timing: (std::time::Duration, std::time::Duration, u32),
    since: std::time::Instant,
}

impl Renderer {
    pub fn new(window: &(impl HasDisplayHandle + HasWindowHandle), size: (u32, u32), cfg: Config) -> Result<Renderer, String> {
        let vk = vk::Vk::new(window, size)?;
        eprintln!(
            "[gs] Vulkan on {} | {}x{} window | {}x internal resolution",
            vk.device_name, vk.swapchain.extent.width, vk.swapchain.extent.height, cfg.scale
        );
        Ok(Renderer { gs: gs::Gs::new(), hw: hw::Hw::new(vk, cfg)?, timing: Default::default(), since: std::time::Instant::now() })
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
                self.gs.write_priv(reg, v)
            }
            gs::Msg::Vsync => {
                self.hw.present(&self.gs);
                self.timing.1 += t0.elapsed();
                self.timing.2 += 1;
                if self.timing.2 == 120 && std::env::var_os("FREEK_GS_LOG").is_some() {
                    let (g, p, n) = self.timing;
                    let fps = n as f64 / self.since.elapsed().as_secs_f64();
                    eprintln!("[gs] {fps:.1} fps; per frame: GS {:.2} ms, present {:.2} ms", g.as_secs_f64() * 1e3 / n as f64, p.as_secs_f64() * 1e3 / n as f64);
                    self.timing = Default::default();
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

    pub fn frame(&self) -> u64 {
        self.hw.frame_no
    }
}
