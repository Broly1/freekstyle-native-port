//! Hardware GS backend: renders GS primitives with Vulkan into upscaled
//! render targets and presents the displayed frame buffer.
//!
//! Render targets are keyed by their GS base pointer and are `scale` times
//! the GS resolution. Colour targets stay in GENERAL layout (attachment,
//! copy source/destination and sampling all work there); depth stays in
//! DEPTH_ATTACHMENT_OPTIMAL. Draws are batched while state is unchanged.

use std::collections::HashMap;
use std::io::Cursor;

use ash::vk;
use gs::mem::{Psm, Vram};
use gs::{Gs, MemRect, Shape, Vertex};

use crate::blend::{self, Blend};
use crate::vk::{barrier, layers, range, transition, Buffer, Image, Result, Vk};

const FRAMES: usize = 2;
const VERTEX_BYTES: u64 = 64 << 20;
const STAGING_BYTES: u64 = 64 << 20;
const MAX_SETS: u32 = 8192;
const COLOR: vk::Format = vk::Format::R8G8B8A8_UNORM;
const DEPTH: vk::Format = vk::Format::D32_SFLOAT;
/// Unused textures are dropped after this many frames.
const TEXTURE_TTL: u64 = 600;
/// Texture cache budget; least recently used textures go first beyond it.
const TEXTURE_BUDGET: u64 = 1 << 30;
/// Blurred side bars: the picture is halved down this chain (each blit
/// averages 2x2 texels), then the smallest is stretched over the window.
const BLUR_CHAIN: [(u32, u32); 6] = [(1024, 768), (512, 384), (256, 192), (128, 96), (64, 48), (32, 24)];

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GpuVertex {
    x: f32,
    y: f32,
    z: f32,
    color: u32,
    s: f32,
    t: f32,
    q: f32,
    fog: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct Push {
    rt_size: [f32; 2],
    tex_scale: [f32; 2],
    fogcol: [f32; 4],
    flags: u32,
    aref: f32,
    fix: f32,
    _pad: f32,
}

// Push flag bits (mirrored in gs.frag).
const F_TME: u32 = 1;
const F_TCC: u32 = 8;
const F_FGE: u32 = 16;
const F_ATE: u32 = 32;
/// Texture is a render-target snapshot: its alpha is stored as a/128.
const F_RTTEX: u32 = 1 << 13;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct PipeKey {
    blend: Blend,
    write_mask: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct SamplerKey {
    linear: bool,
    repeat_u: bool,
    repeat_v: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct TexKey {
    tex0: u64,
    texa: u64,
    clut: u64,
    gen: u64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum TexRef {
    None,
    Cache(TexKey),
    Target(u32),
}

#[derive(Clone, Copy, PartialEq)]
struct BatchKey {
    target: u32,
    pipe: PipeKey,
    tex: TexRef,
    sampler: SamplerKey,
    push: Push,
    scissor: vk::Rect2D,
    depth: (bool, vk::CompareOp),
}

struct Target {
    bp: u32,
    bw: u32,
    psm: Psm,
    /// Size in GS pixels.
    w: u32,
    h: u32,
    /// Vertical scale; doubled in interlaced field mode so the 224-line
    /// field buffers render at full output resolution.
    sy: u32,
    color: Image,
    depth: Image,
    snapshot: Option<Image>,
    snapshot_valid: bool,
    /// First use must clear (contents are undefined).
    fresh: bool,
}

/// Target map key: frame buffer address and width.
fn target_key(bp: u32, bw: u32) -> u32 {
    bp | bw << 16
}

struct Recording {
    path: String,
    start: u64,
    end: u64,
    size: Option<(u32, u32)>,
    encoder: Option<std::process::Child>,
}

struct Texture {
    image: Image,
    last_used: u64,
    bytes: u64,
    /// A title or loading picture replaced by its 16:9 version.
    wide_picture: bool,
}

/// How a 2D overlay draw is placed in widescreen (see `Hw::overlay`).
#[derive(Clone, Copy)]
enum Overlay {
    None,
    /// 3/4 as wide around this x: keeps its shape (HUD, menus).
    Squeeze(f32),
    /// Moved like the wide picture under it around this x.
    Picture(f32),
}

enum Garbage {
    Image(Image),
    Buffer(Buffer),
}

struct Frame {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    acquired: vk::Semaphore,
    vbuf: Buffer,
    vbuf_used: u64,
    staging: Buffer,
    staging_used: u64,
    pool: vk::DescriptorPool,
    garbage: Vec<Garbage>,
}

pub struct Config {
    /// Internal resolution: target pixels per GS pixel, vertically and
    /// horizontally (higher in widescreen, where the picture is stretched).
    pub scale: u32,
    pub scale_x: u32,
    /// Show the picture at 16:9 (the game renders a squeezed wide view)
    /// and use the wide versions of high-resolution pictures.
    pub widescreen: bool,
    /// Stretch the picture to the window instead of keeping 4:3.
    pub stretch: bool,
    /// Fill the sides of a wide window with a blurred copy of the picture
    /// instead of black.
    pub blur_bars: bool,
    /// Replace PlayStation button icons with Xbox ones.
    pub xbox_icons: bool,
    /// Converted pictures (`game/images`); replacements in its `hd`.
    pub pictures: Option<std::path::PathBuf>,
}

pub struct Hw {
    pub vk: Vk,
    pub cfg: Config,
    cmd_pool: vk::CommandPool,
    frames: Vec<Frame>,
    slot: usize,
    pub frame_no: u64,
    /// Vblank number of the frame being presented (the game's clock; the
    /// same one FREEK_PAD_SCRIPT uses).
    pub vblank: u64,
    rendered: Vec<vk::Semaphore>,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    vert: vk::ShaderModule,
    frag: vk::ShaderModule,
    pipelines: HashMap<PipeKey, vk::Pipeline>,
    samplers: HashMap<SamplerKey, vk::Sampler>,
    targets: HashMap<u32, Target>,
    textures: HashMap<TexKey, Texture>,
    pictures: Option<crate::pictures::Pictures>,
    /// A wide picture fills the screen (set by drawing one, cleared by 3D
    /// or another background), so overlays follow its stretch curve.
    on_wide_picture: bool,
    /// Images for the blurred side bars, made on first use.
    blur: Vec<Image>,
    white: Image,
    rendering: Option<u32>,
    batch: Option<BatchKey>,
    verts: Vec<GpuVertex>,
    texture_bytes: u64,
    pub stats: Stats,
    /// Target shown at the last vblank, to count flips.
    shown: Option<u32>,
    /// FREEK_SHOT=<file.png>:<frame>: save the displayed picture.
    shot: Option<(u64, String)>,
    /// FREEK_RECORD=<file.mkv>:<start vblank>:<seconds>: video capture.
    record: Option<Recording>,
    /// FREEK_SHOT_EVERY=<n> with FREEK_SHOT=<prefix>:<first>: a series.
    shot_every: Option<u64>,
    /// FREEK_GS_TRACE=<vblank>[:<presents>]: log every draw from that
    /// vblank on, for that many presented frames (default 1).
    /// FREEK_GS_TRACE=tex:<tbp hex>[:<presents>] starts at the first draw
    /// sampling that texture instead (game timing varies between runs).
    trace_frame: Option<u64>,
    trace_left: u32,
    trace_tex: Option<u32>,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub draws: u64,
    /// Times the displayed frame buffer changed (game frames shown).
    pub flips: u64,
    pub batches: u64,
    pub texture_uploads: u64,
    pub target_uploads: u64,
}

impl Hw {
    pub fn new(mut vk: Vk, cfg: Config) -> Result<Hw> {
        let dev = vk.device.clone();
        let e = |what: &'static str| move |err: vk::Result| format!("{what}: {err:?}");
        unsafe {
            let cmd_pool = dev
                .create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(vk.queue_family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None)
                .map_err(e("command pool"))?;
            let cmds = dev
                .allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(cmd_pool).command_buffer_count(FRAMES as u32))
                .map_err(e("command buffers"))?;
            let mut frames = Vec::new();
            for (n, &cmd) in cmds.iter().enumerate() {
                let sizes = [vk::DescriptorPoolSize { ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, descriptor_count: MAX_SETS }];
                frames.push(Frame {
                    cmd,
                    fence: dev.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None).map_err(e("fence"))?,
                    acquired: dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(e("semaphore"))?,
                    vbuf: vk.create_buffer(VERTEX_BYTES, vk::BufferUsageFlags::VERTEX_BUFFER, &format!("vertices {n}"))?,
                    vbuf_used: 0,
                    staging: vk.create_buffer(STAGING_BYTES, vk::BufferUsageFlags::TRANSFER_SRC, &format!("staging {n}"))?,
                    staging_used: 0,
                    pool: dev.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(MAX_SETS).pool_sizes(&sizes), None).map_err(e("descriptor pool"))?,
                    garbage: Vec::new(),
                });
            }
            let bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
            let set_layout = dev.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None).map_err(e("set layout"))?;
            let ranges = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT, offset: 0, size: std::mem::size_of::<Push>() as u32 }];
            let sets = [set_layout];
            let layout = dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sets).push_constant_ranges(&ranges), None).map_err(e("pipeline layout"))?;
            let module = |bytes: &[u8]| -> Result<vk::ShaderModule> {
                let code = ash::util::read_spv(&mut Cursor::new(bytes)).map_err(|e| e.to_string())?;
                dev.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(|e| format!("shader: {e:?}"))
            };
            let vert = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs.vert.spv")))?;
            let frag = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs.frag.spv")))?;
            let white = vk.create_image(1, 1, COLOR, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST, "white")?;
            let rendered = (0..vk.swapchain.images.len()).map(|_| dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).unwrap()).collect();
            let mut hw = Hw {
                vk,
                cmd_pool,
                frames,
                slot: 0,
                frame_no: 0,
                vblank: 0,
                rendered,
                set_layout,
                layout,
                vert,
                frag,
                pipelines: HashMap::new(),
                samplers: HashMap::new(),
                targets: HashMap::new(),
                textures: HashMap::new(),
                pictures: cfg.pictures.as_deref().map(crate::pictures::Pictures::load),
                on_wide_picture: false,
                cfg,
                blur: Vec::new(),
                white,
                rendering: None,
                batch: None,
                verts: Vec::new(),
                texture_bytes: 0,
                stats: Stats::default(),
                shown: None,
                shot_every: std::env::var("FREEK_SHOT_EVERY").ok().and_then(|v| v.parse().ok()),
                record: std::env::var("FREEK_RECORD").ok().and_then(|v| {
                    let mut it = v.rsplitn(3, ':');
                    let secs: u64 = it.next()?.parse().ok()?;
                    let start: u64 = it.next()?.parse().ok()?;
                    let path = it.next()?.to_string();
                    Some(Recording { path, start, end: start + secs * 60, size: None, encoder: None })
                }),
                trace_frame: std::env::var("FREEK_GS_TRACE").ok().and_then(|v| v.split(':').next()?.parse().ok()),
                trace_left: std::env::var("FREEK_GS_TRACE").ok().and_then(|v| v.rsplit(':').next()?.parse().ok().filter(|_| v.matches(':').count() == 1 + v.starts_with("tex:") as usize)).unwrap_or(1),
                trace_tex: std::env::var("FREEK_GS_TRACE").ok().and_then(|v| u32::from_str_radix(v.strip_prefix("tex:")?.split(':').next()?.trim_start_matches("0x"), 16).ok()),
                shot: std::env::var("FREEK_SHOT").ok().and_then(|v| {
                    let (path, frame) = v.rsplit_once(':')?;
                    Some((frame.parse().ok()?, path.to_string()))
                }),
            };
            hw.begin_frame();
            let cmd = hw.cmd();
            transition(&hw.vk.device, cmd, hw.white.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL);
            let clear = vk::ClearColorValue { float32: [1.0; 4] };
            hw.vk.device.cmd_clear_color_image(cmd, hw.white.image, vk::ImageLayout::GENERAL, &clear, &[range(vk::ImageAspectFlags::COLOR)]);
            barrier(&hw.vk.device, cmd);
            Ok(hw)
        }
    }

    fn cmd(&self) -> vk::CommandBuffer {
        self.frames[self.slot].cmd
    }

    fn begin_frame(&mut self) {
        let dev = self.vk.device.clone();
        let f = &mut self.frames[self.slot];
        unsafe {
            let _ = dev.wait_for_fences(&[f.fence], true, u64::MAX);
            let _ = dev.reset_fences(&[f.fence]);
            let _ = dev.reset_descriptor_pool(f.pool, vk::DescriptorPoolResetFlags::empty());
            let _ = dev.reset_command_buffer(f.cmd, vk::CommandBufferResetFlags::empty());
            let _ = dev.begin_command_buffer(f.cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
        }
        f.vbuf_used = 0;
        f.staging_used = 0;
        for g in std::mem::take(&mut f.garbage) {
            match g {
                Garbage::Image(i) => self.vk.destroy_image(i),
                Garbage::Buffer(b) => self.vk.destroy_buffer(b),
            }
        }
    }

    // ---- staging ----

    /// Copy bytes into this frame's staging memory. Returns (buffer, offset).
    fn stage(&mut self, bytes: &[u8]) -> Result<(vk::Buffer, u64)> {
        let f = &mut self.frames[self.slot];
        let off = (f.staging_used + 15) & !15;
        if off + bytes.len() as u64 <= f.staging.size {
            f.staging.mapped()[off as usize..off as usize + bytes.len()].copy_from_slice(bytes);
            f.staging_used = off + bytes.len() as u64;
            return Ok((f.staging.buffer, off));
        }
        // Too big for the ring: a one-off buffer freed with the frame.
        let mut b = self.vk.create_buffer(bytes.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, "staging overflow")?;
        b.mapped()[..bytes.len()].copy_from_slice(bytes);
        let handle = b.buffer;
        self.frames[self.slot].garbage.push(Garbage::Buffer(b));
        Ok((handle, 0))
    }

    /// Upload RGBA8 pixels into a new w x h image in GENERAL layout.
    fn upload_image(&mut self, w: u32, h: u32, pixels: &[u32], usage: vk::ImageUsageFlags, name: &str) -> Result<Image> {
        let bytes = unsafe { std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4) };
        let (buf, off) = self.stage(bytes)?;
        let img = self.vk.create_image(w, h, COLOR, usage | vk::ImageUsageFlags::TRANSFER_DST, name)?;
        let dev = &self.vk.device;
        let cmd = self.frames[self.slot].cmd;
        transition(dev, cmd, img.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL);
        let region = vk::BufferImageCopy::default()
            .buffer_offset(off)
            .image_subresource(layers(vk::ImageAspectFlags::COLOR))
            .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
        unsafe { dev.cmd_copy_buffer_to_image(cmd, buf, img.image, vk::ImageLayout::GENERAL, &[region]) };
        barrier(dev, cmd);
        Ok(img)
    }

    // ---- render targets ----

    fn end_rendering(&mut self) {
        self.flush();
        if self.rendering.take().is_some() {
            unsafe { self.vk.device.cmd_end_rendering(self.cmd()) };
            barrier(&self.vk.device, self.cmd());
        }
    }

    fn begin_rendering(&mut self, bp: u32) {
        if self.rendering == Some(bp) {
            return;
        }
        self.end_rendering();
        let s = self.cfg.scale_x;
        let t = self.targets.get_mut(&bp).unwrap();
        let load = if t.fresh { vk::AttachmentLoadOp::CLEAR } else { vk::AttachmentLoadOp::LOAD };
        t.fresh = false;
        let color = [vk::RenderingAttachmentInfo::default()
            .image_view(t.color.view)
            .image_layout(vk::ImageLayout::GENERAL)
            .load_op(load)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 0.0] } })];
        let depth = vk::RenderingAttachmentInfo::default()
            .image_view(t.depth.view)
            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
            .load_op(load)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 0.0, stencil: 0 } });
        let area = vk::Rect2D { offset: vk::Offset2D::default(), extent: vk::Extent2D { width: t.w * s, height: t.h * t.sy } };
        let info = vk::RenderingInfo::default().render_area(area).layer_count(1).color_attachments(&color).depth_attachment(&depth);
        unsafe { self.vk.device.cmd_begin_rendering(self.cmd(), &info) };
        self.rendering = Some(bp);
    }

    /// Get or create the target at `bp` with buffer width `bw`, at least
    /// `min_h` GS lines tall, with vertical scale `sy`. Returns its key.
    ///
    /// Targets are per (address, width): games reuse a frame buffer's
    /// memory with another width for effects (Freekstyle draws alpha masks
    /// at 256 and 128 pixels wide into its 512-wide buffer every frame),
    /// and rebuilding the buffer at that width would lose the picture.
    fn target(&mut self, bp: u32, bw: u32, psm: Psm, min_h: u32, sy: u32) -> Result<u32> {
        let key = target_key(bp, bw);
        let need_h = min_h.max(512).next_multiple_of(64).min(2048);
        if let Some(t) = self.targets.get(&key) {
            if t.h >= min_h.min(2048) && t.sy == sy {
                return Ok(key);
            }
        }
        self.end_rendering();
        let s = self.cfg.scale_x;
        let w = (bw.max(1) * 64).min(2048);
        let usage = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED;
        let color = self.vk.create_image(w * s, need_h * sy, COLOR, usage, "target")?;
        let depth = self.vk.create_image(w * s, need_h * sy, DEPTH, vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT, "depth")?;
        let dev = self.vk.device.clone();
        let cmd = self.cmd();
        transition(&dev, cmd, color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL);
        transition(&dev, cmd, depth.image, vk::ImageAspectFlags::DEPTH, vk::ImageLayout::UNDEFINED, vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL);
        let mut t = Target { bp, bw, psm, w, h: need_h, sy, color, depth, snapshot: None, snapshot_valid: false, fresh: true };
        // Growing: keep the old picture.
        if let Some(old) = self.targets.remove(&key) {
            if std::env::var_os("FREEK_TARGET_LOG").is_some() {
                eprintln!("[target] recreate bp={bp:#x}: bw {} -> {bw}, h {} -> {need_h}, sy {} -> {sy} at vblank {}", old.bw, old.h, old.sy, self.vblank);
            }
            let clear = vk::ClearColorValue { float32: [0.0; 4] };
            unsafe {
                dev.cmd_clear_color_image(cmd, t.color.image, vk::ImageLayout::GENERAL, &clear, &[range(vk::ImageAspectFlags::COLOR)]);
                barrier(&dev, cmd);
                // Blit: the old target may have a different vertical scale.
                let blit = vk::ImageBlit {
                    src_subresource: layers(vk::ImageAspectFlags::COLOR),
                    src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: (old.w.min(w) * s) as i32, y: (old.h.min(need_h) * old.sy) as i32, z: 1 }],
                    dst_subresource: layers(vk::ImageAspectFlags::COLOR),
                    dst_offsets: [vk::Offset3D::default(), vk::Offset3D { x: (old.w.min(w) * s) as i32, y: (old.h.min(need_h) * sy) as i32, z: 1 }],
                };
                dev.cmd_blit_image(cmd, old.color.image, vk::ImageLayout::GENERAL, t.color.image, vk::ImageLayout::GENERAL, &[blit], vk::Filter::LINEAR);
            }
            barrier(&dev, cmd);
            // Colour is valid now; only depth needs clearing on first use,
            // which LOAD_OP_CLEAR would also do to colour. Clear depth here.
            t.fresh = false;
            self.clear_depth_now(&t);
            let f = &mut self.frames[self.slot];
            f.garbage.push(Garbage::Image(old.color));
            f.garbage.push(Garbage::Image(old.depth));
            if let Some(s) = old.snapshot {
                f.garbage.push(Garbage::Image(s));
            }
        }
        self.targets.insert(key, t);
        Ok(key)
    }

    fn clear_depth_now(&self, t: &Target) {
        let s = self.cfg.scale_x;
        let depth = vk::RenderingAttachmentInfo::default()
            .image_view(t.depth.view)
            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 0.0, stencil: 0 } });
        let area = vk::Rect2D { offset: vk::Offset2D::default(), extent: vk::Extent2D { width: t.w * s, height: t.h * t.sy } };
        unsafe {
            self.vk.device.cmd_begin_rendering(self.cmd(), &vk::RenderingInfo::default().render_area(area).layer_count(1).depth_attachment(&depth));
            self.vk.device.cmd_end_rendering(self.cmd());
        }
        barrier(&self.vk.device, self.cmd());
    }

    /// Copy GS memory (already updated) into a target's picture.
    fn upload_to_target(&mut self, vram: &Vram, texa: gs::regs::Texa, key: u32, x: u32, y: u32, w: u32, h: u32) -> Result<()> {
        let (psm, bw, tw, th, bp) = {
            let t = &self.targets[&key];
            (t.psm, t.bw, t.w, t.h, t.bp)
        };
        let (w, h) = (w.min(tw.saturating_sub(x)), h.min(th.saturating_sub(y)));
        if w == 0 || h == 0 {
            return Ok(());
        }
        let mut px = gs::tex::decode_rect(vram, psm, bp, bw, x, y, w, h, texa);
        for p in &mut px {
            // Targets store alpha as a/128.
            let a = (*p >> 24).min(128) * 255 / 128;
            *p = (*p & 0xff_ffff) | a << 24;
        }
        self.end_rendering();
        let tmp = self.upload_image(w, h, &px, vk::ImageUsageFlags::TRANSFER_SRC, "target upload")?;
        let s = self.cfg.scale_x as i32;
        let t = self.targets.get_mut(&key).unwrap();
        let sy = t.sy as i32;
        if t.fresh {
            // The upload must not be wiped by a later first-use clear.
            let clear = vk::ClearColorValue { float32: [0.0; 4] };
            unsafe { self.vk.device.cmd_clear_color_image(self.frames[self.slot].cmd, t.color.image, vk::ImageLayout::GENERAL, &clear, &[range(vk::ImageAspectFlags::COLOR)]) };
            barrier(&self.vk.device, self.frames[self.slot].cmd);
        }
        let blit = vk::ImageBlit {
            src_subresource: layers(vk::ImageAspectFlags::COLOR),
            src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: w as i32, y: h as i32, z: 1 }],
            dst_subresource: layers(vk::ImageAspectFlags::COLOR),
            dst_offsets: [
                vk::Offset3D { x: x as i32 * s, y: y as i32 * sy, z: 0 },
                vk::Offset3D { x: (x + w) as i32 * s, y: (y + h) as i32 * sy, z: 1 },
            ],
        };
        let cmd = self.frames[self.slot].cmd;
        unsafe { self.vk.device.cmd_blit_image(cmd, tmp.image, vk::ImageLayout::GENERAL, t.color.image, vk::ImageLayout::GENERAL, &[blit], vk::Filter::NEAREST) };
        barrier(&self.vk.device, cmd);
        t.snapshot_valid = false;
        let was_fresh = std::mem::replace(&mut t.fresh, false);
        if was_fresh {
            let t = &self.targets[&key];
            self.clear_depth_now(t);
        }
        self.frames[self.slot].garbage.push(Garbage::Image(tmp));
        self.stats.target_uploads += 1;
        Ok(())
    }

    // ---- textures ----

    fn texture(&mut self, gs: &Gs) -> Result<(TexRef, [f32; 2], bool)> {
        let ctx = gs.context();
        let t = ctx.tex0;
        let (tw, th) = (1u32 << t.tw(), 1u32 << t.th());
        // A texture that starts at a render target samples its picture.
        if !t.psm().is_indexed() {
            // Prefer the target drawn with the texture's own width.
            let found = self.targets.get_key_value(&target_key(t.tbp0(), t.tbw())).or_else(|| self.targets.iter().find(|(_, rt)| rt.bp == t.tbp0()));
            if let Some((&k, rt)) = found {
                let scale = [tw as f32 / rt.w as f32, th as f32 / rt.h as f32];
                return Ok((TexRef::Target(k), scale, true));
            }
        }
        let indexed = t.psm().is_indexed();
        let clut = if indexed {
            let n = if matches!(t.psm(), gs::mem::Psm::T8 | gs::mem::Psm::T8h) { 256 } else { 16 };
            let off = if n == 16 { t.csa() as usize * 16 % 256 } else { 0 };
            (0..n).fold(0xcbf2_9ce4_8422_2325u64, |h, i| (h ^ gs.clut[(off + i) % 256] as u64).wrapping_mul(0x100_0000_01b3)) ^ (gs.clut_psm == Psm::Ct32) as u64
        } else {
            0
        };
        let uses_texa = !matches!(t.psm(), Psm::Ct32 | Psm::Z32) || (indexed && gs.clut_psm != Psm::Ct32);
        let pages = Vram::pages_of(t.psm(), t.tbp0(), t.tbw(), tw, th);
        let key = TexKey {
            // Only fields that change the decoded texels.
            tex0: t.0 & ((1 << 34) - 1) | ((t.csa() as u64) << 40) * indexed as u64,
            texa: if uses_texa { gs.texa.0 } else { 0 },
            clut,
            gen: gs.vram.generation(pages),
        };
        let frame = self.frame_no;
        if let Some(tex) = self.textures.get_mut(&key) {
            tex.last_used = frame;
            return Ok((TexRef::Cache(key), [1.0, 1.0], false));
        }
        let (w, h, px) = gs::tex::decode(&gs.vram, t, gs.texa, &gs.clut, gs.clut_psm);
        if let Some(dir) = std::env::var_os("FREEK_DUMP_TEXTURES") {
            dump_texture(std::path::Path::new(&dir), &key, t, w, h, &px);
        }
        // Pictures in high resolution; PlayStation button prompts become
        // Xbox buttons.
        let picture = match &self.pictures {
            Some(p) if !t.psm().is_indexed() => p.replace(w, h, &px, (self.cfg.scale_x, self.cfg.scale), self.cfg.widescreen),
            _ => None,
        };
        let (w, h, px, wide_picture) = match picture {
            Some(r) => r,
            None if self.cfg.xbox_icons => match crate::icons::replace(w, h, &px) {
                Some((w, h, px)) => (w, h, px, false),
                None => (w, h, px, false),
            },
            None => (w, h, px, false),
        };
        self.end_rendering();
        let image = self.upload_image(w, h, &px, vk::ImageUsageFlags::SAMPLED, "texture")?;
        self.textures.insert(key, Texture { image, last_used: frame, bytes: w as u64 * h as u64 * 4, wide_picture });
        self.texture_bytes += w as u64 * h as u64 * 4;
        if self.texture_bytes > TEXTURE_BUDGET {
            self.evict_lru();
        }
        self.stats.texture_uploads += 1;
        Ok((TexRef::Cache(key), [1.0, 1.0], false))
    }

    /// View to sample for a texture reference, snapshotting targets.
    fn texture_view(&mut self, r: TexRef) -> Result<vk::ImageView> {
        match r {
            TexRef::None => Ok(self.white.view),
            TexRef::Cache(k) => Ok(self.textures[&k].image.view),
            TexRef::Target(bp) => {
                if !self.targets[&bp].snapshot_valid {
                    if self.targets[&bp].snapshot.is_none() {
                        let (w, h) = (self.targets[&bp].color.extent.width, self.targets[&bp].color.extent.height);
                        let img = self.vk.create_image(w, h, COLOR, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST, "snapshot")?;
                        transition(&self.vk.device, self.cmd(), img.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL);
                        self.targets.get_mut(&bp).unwrap().snapshot = Some(img);
                    }
                    // Copying needs to happen outside rendering; the caller
                    // flushed the batch already.
                    let was = self.rendering;
                    self.end_rendering();
                    let t = &self.targets[&bp];
                    let snap = t.snapshot.as_ref().unwrap();
                    let copy = vk::ImageCopy::default()
                        .src_subresource(layers(vk::ImageAspectFlags::COLOR))
                        .dst_subresource(layers(vk::ImageAspectFlags::COLOR))
                        .extent(vk::Extent3D { width: t.color.extent.width, height: t.color.extent.height, depth: 1 });
                    unsafe { self.vk.device.cmd_copy_image(self.cmd(), t.color.image, vk::ImageLayout::GENERAL, snap.image, vk::ImageLayout::GENERAL, &[copy]) };
                    barrier(&self.vk.device, self.cmd());
                    self.targets.get_mut(&bp).unwrap().snapshot_valid = true;
                    if let Some(b) = was {
                        self.begin_rendering(b);
                    }
                }
                Ok(self.targets[&bp].snapshot.as_ref().unwrap().view)
            }
        }
    }

    fn sampler(&mut self, k: SamplerKey) -> vk::Sampler {
        let dev = &self.vk.device;
        *self.samplers.entry(k).or_insert_with(|| {
            let filter = if k.linear { vk::Filter::LINEAR } else { vk::Filter::NEAREST };
            let mode = |r: bool| if r { vk::SamplerAddressMode::REPEAT } else { vk::SamplerAddressMode::CLAMP_TO_EDGE };
            let info = vk::SamplerCreateInfo::default()
                .mag_filter(filter)
                .min_filter(filter)
                .address_mode_u(mode(k.repeat_u))
                .address_mode_v(mode(k.repeat_v))
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .max_lod(0.0);
            unsafe { dev.create_sampler(&info, None).expect("sampler") }
        })
    }

    fn pipeline(&mut self, k: PipeKey) -> vk::Pipeline {
        if let Some(&p) = self.pipelines.get(&k) {
            return p;
        }
        let entry = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(self.vert).name(entry),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(self.frag).name(entry),
        ];
        let binding = [vk::VertexInputBindingDescription { binding: 0, stride: std::mem::size_of::<GpuVertex>() as u32, input_rate: vk::VertexInputRate::VERTEX }];
        let attr = |location, format, offset| vk::VertexInputAttributeDescription { location, binding: 0, format, offset };
        let attrs = [
            attr(0, vk::Format::R32G32_SFLOAT, 0),
            attr(1, vk::Format::R32_SFLOAT, 8),
            attr(2, vk::Format::R8G8B8A8_UNORM, 12),
            attr(3, vk::Format::R32G32B32_SFLOAT, 16),
            attr(4, vk::Format::R32_SFLOAT, 28),
        ];
        let vi = vk::PipelineVertexInputStateCreateInfo::default().vertex_binding_descriptions(&binding).vertex_attribute_descriptions(&attrs);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default().polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE).line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let ds = vk::PipelineDepthStencilStateCreateInfo::default();
        let b = k.blend;
        let att = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(b.enable)
            .color_blend_op(b.op)
            .src_color_blend_factor(b.src)
            .dst_color_blend_factor(b.dst)
            // Destination alpha is replaced by source alpha.
            .alpha_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .color_write_mask(vk::ColorComponentFlags::from_raw(k.write_mask))];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&att);
        let dynamic = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::DEPTH_TEST_ENABLE,
            vk::DynamicState::DEPTH_WRITE_ENABLE,
            vk::DynamicState::DEPTH_COMPARE_OP,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dy = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let formats = [COLOR];
        let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats).depth_attachment_format(DEPTH);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .depth_stencil_state(&ds)
            .color_blend_state(&cb)
            .dynamic_state(&dy)
            .layout(self.layout)
            .push_next(&mut rendering);
        let p = unsafe { self.vk.device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }.expect("pipeline")[0];
        self.pipelines.insert(k, p);
        p
    }

    // ---- batching ----

    fn flush(&mut self) {
        let Some(k) = self.batch else { return };
        if self.verts.is_empty() {
            return;
        }
        let verts = std::mem::take(&mut self.verts);
        let bytes = unsafe { std::slice::from_raw_parts(verts.as_ptr() as *const u8, verts.len() * std::mem::size_of::<GpuVertex>()) };
        let f = &mut self.frames[self.slot];
        if f.vbuf_used + bytes.len() as u64 > f.vbuf.size {
            eprintln!("[gs] vertex buffer full; dropping {} vertices", verts.len());
            self.verts = verts;
            self.verts.clear();
            return;
        }
        let off = f.vbuf_used;
        f.vbuf.mapped()[off as usize..off as usize + bytes.len()].copy_from_slice(bytes);
        f.vbuf_used += bytes.len() as u64;
        let vbuf = f.vbuf.buffer;
        let pool = f.pool;

        let view = match self.texture_view(k.tex) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[gs] texture: {e}");
                self.white.view
            }
        };
        self.begin_rendering(k.target);
        let pipeline = self.pipeline(k.pipe);
        let sampler = self.sampler(k.sampler);
        let dev = &self.vk.device;
        let cmd = self.frames[self.slot].cmd;
        unsafe {
            let layouts = [self.set_layout];
            let Ok(sets) = dev.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&layouts)) else {
                eprintln!("[gs] out of descriptor sets this frame");
                return;
            };
            let img = [vk::DescriptorImageInfo { sampler, image_view: view, image_layout: vk::ImageLayout::GENERAL }];
            let write = vk::WriteDescriptorSet::default().dst_set(sets[0]).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&img);
            dev.update_descriptor_sets(&[write], &[]);
            let t = &self.targets[&k.target];
            let s = self.cfg.scale_x as f32;
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
            dev.cmd_set_viewport(cmd, 0, &[vk::Viewport { x: 0.0, y: 0.0, width: t.w as f32 * s, height: (t.h * t.sy) as f32, min_depth: 0.0, max_depth: 1.0 }]);
            dev.cmd_set_scissor(cmd, 0, &[k.scissor]);
            dev.cmd_set_depth_test_enable(cmd, k.depth.0 || k.depth.1 != vk::CompareOp::ALWAYS);
            dev.cmd_set_depth_write_enable(cmd, k.depth.0);
            dev.cmd_set_depth_compare_op(cmd, k.depth.1);
            dev.cmd_set_blend_constants(cmd, &[k.push.fix; 4]);
            dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, self.layout, 0, &sets, &[]);
            let push = std::slice::from_raw_parts(&k.push as *const Push as *const u8, std::mem::size_of::<Push>());
            dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT, 0, push);
            dev.cmd_bind_vertex_buffers(cmd, 0, &[vbuf], &[off]);
            dev.cmd_draw(cmd, verts.len() as u32, 1, 0, 0);
        }
        self.stats.batches += 1;
        if let Some(t) = self.targets.get_mut(&k.target) {
            t.snapshot_valid = false;
        }
        self.verts = verts;
        self.verts.clear();
    }

    fn draw_inner(&mut self, gs: &Gs, shape: Shape, v: &[Vertex]) -> Result<()> {
        let attrs = gs.attrs();
        let ctx = *gs.context();
        if self.trace_tex.is_some_and(|tbp| attrs.textured() && ctx.tex0.tbp0() == tbp) {
            self.trace_tex = None;
            self.trace_frame = Some(self.vblank);
        }
        if self.trace_frame.is_some_and(|f| self.vblank >= f) {
            let (f, z, t) = (ctx.frame, ctx.zbuf, ctx.test);
            eprintln!(
                "[draw] {shape:?} v0=({:.1},{:.1},z={:#x}) v1=({:.1},{:.1}) rgba={:?} tme={} psm={:?} tbp={:#x} fb={:#x}/{:?} mask={:#x} zb={:#x}/{:?} zmsk={} zte={} ztst={} ate={} atst={} aref={} afail={} date={} abe={} alpha={:?} sc={:?} prim={:#x}",
                v[0].x, v[0].y, v[0].z, v.get(1).map_or(0.0, |x| x.x), v.get(1).map_or(0.0, |x| x.y), v[0].rgba,
                attrs.textured(), ctx.tex0.psm(), ctx.tex0.tbp0(), f.bp(), f.psm(), f.mask(), z.bp(), z.psm(), z.masked(),
                t.zte(), t.ztst(), t.alpha_test(), t.atst(), t.aref(), t.afail(), t.date(), attrs.blend(), ctx.alpha.abcd(), ctx.scissor.rect(), attrs.0
            );
        }
        let (fb, zb, test) = (ctx.frame, ctx.zbuf, ctx.test);
        let (sx0, sy0, sx1, sy1) = ctx.scissor.rect();
        if sx1 < sx0 || sy1 < sy0 {
            return Ok(());
        }
        let color_off = fb.mask() == u32::MAX;
        if color_off && zb.masked() {
            return Ok(());
        }
        let ysc = self.vertical_scale(gs);
        let bp = self.target(fb.bp(), fb.bw(), fb.psm(), sy1 + 1, ysc)?;

        // Texture first: it may need transfers outside rendering.
        let (tex, tex_scale, rt_tex) = if attrs.textured() { self.texture(gs)? } else { (TexRef::None, [1.0, 1.0], false) };

        let blend = if attrs.blend() { blend::map(ctx.alpha.abcd()) } else { Blend::OFF };
        let fm = fb.mask();
        let mut write_mask = 0;
        for (n, bit) in [vk::ColorComponentFlags::R, vk::ColorComponentFlags::G, vk::ColorComponentFlags::B, vk::ColorComponentFlags::A].iter().enumerate() {
            if (fm >> (8 * n)) & 0xff != 0xff {
                write_mask |= bit.as_raw();
            }
        }
        if fb.psm() == Psm::Ct24 {
            write_mask &= !vk::ColorComponentFlags::A.as_raw();
        }
        let tex0 = ctx.tex0;
        let mut flags = 0;
        if attrs.textured() {
            flags |= F_TME | tex0.tfx() << 1;
            if tex0.tcc() {
                flags |= F_TCC;
            }
            if rt_tex {
                flags |= F_RTTEX;
            }
        }
        if attrs.fog() {
            flags |= F_FGE;
        }
        if test.alpha_test() {
            flags |= F_ATE | test.atst() << 6 | test.afail() << 9;
        }
        flags |= blend.shader_scale << 11;
        let fogcol = gs.fogcol;
        let t = &self.targets[&bp];
        let push = Push {
            rt_size: [t.w as f32, t.h as f32],
            tex_scale,
            fogcol: [(fogcol & 0xff) as f32 / 255.0, ((fogcol >> 8) & 0xff) as f32 / 255.0, ((fogcol >> 16) & 0xff) as f32 / 255.0, 0.0],
            flags,
            aref: test.aref() as f32,
            fix: ctx.alpha.fix() as f32 / 128.0,
            _pad: 0.0,
        };
        let s = self.cfg.scale_x;
        let scissor = vk::Rect2D {
            offset: vk::Offset2D { x: (sx0 * s) as i32, y: (sy0 * t.sy) as i32 },
            extent: vk::Extent2D { width: ((sx1.min(t.w - 1) + 1).saturating_sub(sx0)) * s, height: ((sy1.min(t.h - 1) + 1).saturating_sub(sy0)) * t.sy },
        };
        let ztst = match test.ztst() {
            0 => vk::CompareOp::NEVER,
            1 => vk::CompareOp::ALWAYS,
            2 => vk::CompareOp::GREATER_OR_EQUAL,
            _ => vk::CompareOp::GREATER,
        };
        let ztst = if test.zte() { ztst } else { vk::CompareOp::ALWAYS };
        let sampler = SamplerKey {
            linear: ctx.tex1.mag_linear() || ctx.tex1.min_linear(),
            repeat_u: ctx.clamp.wms() == 0,
            repeat_v: ctx.clamp.wmt() == 0,
        };
        let key = BatchKey {
            target: bp,
            pipe: PipeKey { blend, write_mask },
            tex,
            sampler,
            push,
            scissor,
            depth: (!zb.masked(), ztst),
        };
        if self.batch != Some(key) {
            self.flush();
            self.batch = Some(key);
        }

        // Vertices.
        let zscale = match zb.psm() {
            Psm::Z32 => 1.0 / 4_294_967_296.0,
            Psm::Z24 => 1.0 / 16_777_216.0,
            _ => 1.0 / 65_536.0,
        };
        let (tw, th) = ((1u32 << tex0.tw()) as f32, (1u32 << tex0.th()) as f32);
        let fst = attrs.fst();
        let overlay = self.overlay(&ctx, v, tex);
        let conv = |v: &Vertex| GpuVertex {
            x: match overlay {
                Overlay::None => v.x,
                Overlay::Squeeze(c) => c + (v.x - c) * 0.75,
                Overlay::Picture(c) => c + c * crate::pictures::wide_position((v.x - c) / c),
            },
            y: v.y,
            z: (v.z as f64 * zscale).min(1.0) as f32,
            color: u32::from_le_bytes(v.rgba),
            s: if fst { v.u / tw } else { v.s },
            t: if fst { v.v / th } else { v.t },
            q: if fst { 1.0 } else if v.q == 0.0 { f32::MIN_POSITIVE } else { v.q },
            fog: v.fog as f32 / 255.0,
        };
        match shape {
            Shape::Triangle => self.verts.extend(v.iter().map(conv)),
            Shape::Sprite => {
                let (a, b) = (conv(&v[0]), conv(&v[1]));
                let corner = |x: &GpuVertex, y: &GpuVertex| GpuVertex { x: x.x, s: x.s, y: y.y, t: y.t, ..b };
                let (p0, p1, p2, p3) = (corner(&a, &a), corner(&b, &a), corner(&a, &b), corner(&b, &b));
                self.verts.extend([p0, p1, p2, p1, p3, p2]);
            }
            Shape::Line => {
                let (a, b) = (conv(&v[0]), conv(&v[1]));
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = (dx * dx + dy * dy).sqrt().max(1e-6);
                let (nx, ny) = (-dy / len * 0.5, dx / len * 0.5);
                let off = |p: GpuVertex, k: f32| GpuVertex { x: p.x + nx * k, y: p.y + ny * k, ..p };
                self.verts.extend([off(a, 1.0), off(b, 1.0), off(a, -1.0), off(b, 1.0), off(b, -1.0), off(a, -1.0)]);
            }
            Shape::Point => {
                let p = conv(&v[0]);
                let at = |dx: f32, dy: f32| GpuVertex { x: p.x + dx, y: p.y + dy, ..p };
                self.verts.extend([at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0), at(1.0, 0.0), at(1.0, 1.0), at(0.0, 1.0)]);
            }
        }
        self.stats.draws += 1;
        Ok(())
    }

    /// In widescreen the frame is shown stretched by 4/3, so 2D overlays
    /// (HUD, menu text, button prompts) are drawn 3/4 as wide around the
    /// frame's centre to keep their shape. The game draws its overlays at
    /// the far depth limit with the depth test on; 3D geometry never sits
    /// exactly there, and full-screen passes (backgrounds, fades, effects)
    /// either use another depth or span the whole width, which is left
    /// alone.
    ///
    /// Over a wide picture, overlays (some drawn with the depth test off,
    /// like the loading bar) instead follow the picture's stretch curve,
    /// so they stay on the spot in the picture they were made for.
    fn overlay(&mut self, ctx: &gs::regs::Context, v: &[Vertex], tex: TexRef) -> Overlay {
        if !self.cfg.widescreen {
            return Overlay::None;
        }
        let far = match ctx.zbuf.psm() {
            Psm::Z32 => u32::MAX,
            Psm::Z24 => 0xff_ffff,
            _ => 0xffff,
        };
        let width = (ctx.frame.bw() * 64) as f32;
        let (x0, x1) = v.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v.x), b.max(v.x)));
        if x0 <= 1.0 && x1 >= width - 1.0 {
            // A background: a wide picture, or something else textured.
            match tex {
                TexRef::Cache(k) if self.textures[&k].wide_picture => self.on_wide_picture = true,
                TexRef::None => {}
                _ => self.on_wide_picture = false,
            }
            return Overlay::None;
        }
        if v.iter().any(|v| v.z != far) {
            self.on_wide_picture = false;
            return Overlay::None;
        }
        if self.on_wide_picture {
            Overlay::Picture(width / 2.0)
        } else if ctx.test.zte() && ctx.test.ztst() >= 2 {
            Overlay::Squeeze(width / 2.0)
        } else {
            Overlay::None
        }
    }

    /// Vertical render scale: doubled for interlaced field-mode output,
    /// where each 224-line buffer is shown at 448 lines.
    fn vertical_scale(&self, gs: &Gs) -> u32 {
        let field_mode = gs.display.smode2 & 3 == 3;
        self.cfg.scale * if field_mode { 2 } else { 1 }
    }

    // ---- display ----

    /// Present the frame buffer the display circuits point at.
    pub fn present(&mut self, gs: &Gs) {
        self.end_rendering();
        // A traced frame ends here.
        if self.trace_frame.is_some_and(|f| self.vblank >= f) {
            eprintln!("[draw] --- present at vblank {} ---", self.vblank);
            self.trace_left = self.trace_left.saturating_sub(1);
            if self.trace_left == 0 {
                self.trace_frame = None;
            }
        }
        let d = gs.display;
        let circuit = if d.enabled(1) { Some(1) } else if d.enabled(0) { Some(0) } else { None };
        let mut src = None;
        if let Some(c) = circuit {
            let fb = d.dispfb[c];
            let disp = d.display[c];
            let bp = (fb & 0x1ff) as u32 * 32;
            let bw = ((fb >> 9) & 0x3f) as u32;
            let psm = Psm::from_bits((fb >> 15) & 0x1f);
            let (dbx, dby) = (((fb >> 32) & 0x7ff) as u32, ((fb >> 43) & 0x7ff) as u32);
            let magh = ((disp >> 23) & 0xf) as u32 + 1;
            let magv = ((disp >> 27) & 3) as u32 + 1;
            let w = (((disp >> 32) & 0xfff) as u32 + 1) / magh;
            let mut h = (((disp >> 44) & 0x7ff) as u32 + 1) / magv;
            // Interlaced field mode (SMODE2 INT and FFMD): the buffer holds
            // one field, shown at double height.
            if d.smode2 & 3 == 3 {
                h /= 2;
            }
            if w > 0 && h > 0 && bw > 0 {
                // Never drawn into: build the target from GS memory.
                let fresh = !self.targets.contains_key(&target_key(bp, bw));
                if let Ok(key) = self.target(bp, bw, psm, dby + h, self.vertical_scale(gs)) {
                    if fresh {
                        let _ = self.upload_to_target(&gs.vram, gs.texa, key, 0, 0, bw * 64, dby + h);
                    }
                    if self.shown != Some(key) {
                        self.shown = Some(key);
                        self.stats.flips += 1;
                    }
                    src = Some((key, dbx, dby, w, h));
                }
            }
        }
        if self.frame_no % 120 == 0 && std::env::var_os("FREEK_GS_LOG").is_some() {
            eprintln!("[gs] frame {} display {:?} pmode {:#x} dispfb {:#x?} display {:#x?} targets {:?} textures {} {:?}", self.frame_no, src, d.pmode, d.dispfb, d.display, self.targets.keys().collect::<Vec<_>>(), self.textures.len(), self.stats);
        }
        if std::env::var("FREEK_DISPLAY_LOG").ok().and_then(|v| v.parse::<u64>().ok()).is_some_and(|v| (v..v + 12).contains(&self.vblank)) {
            let t = src.and_then(|s| self.targets.get(&s.0)).map(|t| (t.bw, t.psm, t.w, t.h));
            eprintln!("[display] vblank {} src {:?} target {:?} dispfb {:#x?} display {:#x?}", self.vblank, src, t, d.dispfb, d.display);
        }
        let recording = match (&self.record, src) {
            (Some(r), Some(src)) if (r.start..r.end).contains(&self.vblank) => self.record_readback(src).ok(),
            _ => None,
        };
        let readback = match (&self.shot, src) {
            (Some((frame, _)), Some(src)) if self.vblank >= *frame => match self.record_readback(src) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!("[gs] screenshot at vblank {}: {e}", self.vblank);
                    None
                }
            },
            _ => None,
        };
        self.submit_and_present(src);
        if let Some((buf, w, h)) = readback {
            self.save_shot(buf, w, h);
        }
        if let Some((buf, w, h)) = recording {
            self.record_frame(buf, w, h);
        }
        self.evict_textures();
    }

    /// Copy the displayed region at internal resolution into a buffer.
    fn record_readback(&mut self, (bp, x, y, w, h): (u32, u32, u32, u32, u32)) -> Result<(Buffer, u32, u32)> {
        let s = self.cfg.scale_x;
        let t = &self.targets[&bp];
        let sy = t.sy;
        let (w, h) = ((w * s).min(t.color.extent.width - x * s), (h * sy).min(t.color.extent.height - y * sy));
        let image = t.color.image;
        let buf = self.vk.create_buffer(w as u64 * h as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST, "screenshot")?;
        let region = vk::BufferImageCopy::default()
            .image_subresource(layers(vk::ImageAspectFlags::COLOR))
            .image_offset(vk::Offset3D { x: (x * s) as i32, y: (y * sy) as i32, z: 0 })
            .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
        unsafe { self.vk.device.cmd_copy_image_to_buffer(self.cmd(), image, vk::ImageLayout::GENERAL, buf.buffer, &[region]) };
        barrier(&self.vk.device, self.cmd());
        Ok((buf, w, h))
    }

    /// Send one frame to the FREEK_RECORD encoder. Every frame is scaled
    /// into the first frame's size, so display mode changes don't break
    /// the stream.
    fn record_frame(&mut self, mut buf: Buffer, w: u32, h: u32) {
        unsafe {
            let _ = self.vk.device.device_wait_idle();
        }
        let src = buf.mapped()[..(w * h * 4) as usize].to_vec();
        self.vk.destroy_buffer(buf);
        let Some(r) = self.record.as_mut() else { return };
        let (ow, oh) = *r.size.get_or_insert((w, h));
        let mut frame = vec![0u8; (ow * oh * 4) as usize];
        for y in 0..oh {
            let sy = (y * h / oh) as usize;
            for x in 0..ow {
                let sx = (x * w / ow) as usize;
                let o = ((y * ow + x) * 4) as usize;
                let i = (sy * w as usize + sx) * 4;
                frame[o..o + 3].copy_from_slice(&src[i..i + 3]);
                frame[o + 3] = 255;
            }
        }
        if r.encoder.is_none() {
            let child = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{ow}x{oh}"), "-r", "60", "-i", "-"])
                .args(["-vf", "scale=1024:-2", "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p", &r.path])
                .stdin(std::process::Stdio::piped())
                .spawn();
            match child {
                Ok(c) => r.encoder = Some(c),
                Err(e) => {
                    eprintln!("[gs] recording: cannot start ffmpeg: {e}");
                    self.record = None;
                    return;
                }
            }
        }
        use std::io::Write;
        let enc = r.encoder.as_mut().unwrap();
        let ok = enc.stdin.as_mut().is_some_and(|i| i.write_all(&frame).is_ok());
        if !ok || self.vblank + 1 >= r.end {
            let mut r = self.record.take().unwrap();
            drop(r.encoder.as_mut().and_then(|e| e.stdin.take()));
            let _ = r.encoder.as_mut().map(|e| e.wait());
            eprintln!("[gs] recording saved to {}", r.path);
        }
    }

    fn save_shot(&mut self, mut buf: Buffer, w: u32, h: u32) {
        unsafe {
            let _ = self.vk.device.device_wait_idle();
        }
        let (_, base) = self.shot.take().unwrap();
        let path = match self.shot_every {
            Some(n) => {
                self.shot = Some((self.vblank - self.vblank % n + n, base.clone()));
                format!("{}_{:06}.png", base.trim_end_matches(".png"), self.vblank)
            }
            None => base,
        };
        let png = crate::png::encode(w, h, &buf.mapped()[..(w * h * 4) as usize]);
        match std::fs::write(&path, png) {
            Ok(()) => eprintln!("[gs] saved {w}x{h} screenshot to {path}"),
            Err(e) => eprintln!("[gs] screenshot {path}: {e}"),
        }
        self.vk.destroy_buffer(buf);
    }

    fn submit_and_present(&mut self, src: Option<(u32, u32, u32, u32, u32)>) {
        if self.cfg.blur_bars && !self.cfg.stretch && self.blur.is_empty() {
            for (w, h) in BLUR_CHAIN {
                match self.vk.create_image(w, h, COLOR, vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST, "blur") {
                    Ok(img) => {
                        transition(&self.vk.device, self.cmd(), img.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL);
                        self.blur.push(img);
                    }
                    Err(e) => {
                        eprintln!("[gs] blurred bars: {e}");
                        self.cfg.blur_bars = false;
                        break;
                    }
                }
            }
        }
        let dev = self.vk.device.clone();
        let cmd = self.cmd();
        let f = &self.frames[self.slot];
        let acquired = unsafe { self.vk.swapchain.loader.acquire_next_image(self.vk.swapchain.swapchain, u64::MAX, f.acquired, vk::Fence::null()) };
        let image_index = match acquired {
            Ok((i, _)) => Some(i),
            Err(_) => None,
        };
        if let Some(i) = image_index {
            let sw = self.vk.swapchain.images[i as usize];
            let ext = self.vk.swapchain.extent;
            transition(&dev, cmd, sw, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            unsafe {
                let black = vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] };
                dev.cmd_clear_color_image(cmd, sw, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &black, &[range(vk::ImageAspectFlags::COLOR)]);
            }
            barrier(&dev, cmd);
            if let Some((bp, x, y, w, h)) = src {
                let s = self.cfg.scale_x as i32;
                let t = &self.targets[&bp];
                // Fit 4:3 (16:9 in widescreen) into the window unless stretching.
                let (ww, wh) = (ext.width as i32, ext.height as i32);
                let (ax, ay) = if self.cfg.widescreen { (16, 9) } else { (4, 3) };
                let (dw, dh) = if self.cfg.stretch {
                    (ww, wh)
                } else if ww * ay > wh * ax {
                    (wh * ax / ay, wh)
                } else {
                    (ww, ww * ay / ax)
                };
                let (dx, dy) = ((ww - dw) / 2, (wh - dh) / 2);
                let ys = t.sy as i32;
                let sx1 = ((x + w) as i32 * s).min(t.color.extent.width as i32);
                let sy1 = ((y + h) as i32 * ys).min(t.color.extent.height as i32);
                let src_rect = [vk::Offset3D { x: x as i32 * s, y: y as i32 * ys, z: 0 }, vk::Offset3D { x: sx1, y: sy1, z: 1 }];
                if self.cfg.blur_bars && dw < ww {
                    // Blur by halving, then stretch the smallest over the window.
                    let mut from = (t.color.image, src_rect);
                    for img in &self.blur {
                        let to = [vk::Offset3D::default(), vk::Offset3D { x: img.extent.width as i32, y: img.extent.height as i32, z: 1 }];
                        blit(&dev, cmd, from, (img.image, vk::ImageLayout::GENERAL, to));
                        from = (img.image, to);
                    }
                    blit(&dev, cmd, from, (sw, vk::ImageLayout::TRANSFER_DST_OPTIMAL, [vk::Offset3D::default(), vk::Offset3D { x: ww, y: wh, z: 1 }]));
                }
                let to = [vk::Offset3D { x: dx, y: dy, z: 0 }, vk::Offset3D { x: dx + dw, y: dy + dh, z: 1 }];
                blit(&dev, cmd, (t.color.image, src_rect), (sw, vk::ImageLayout::TRANSFER_DST_OPTIMAL, to));
            }
            transition(&dev, cmd, sw, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR);
        }
        unsafe {
            let _ = dev.end_command_buffer(cmd);
            let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let waits = [vk::SemaphoreSubmitInfo::default().semaphore(self.frames[self.slot].acquired).stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let signals = image_index.map(|i| [vk::SemaphoreSubmitInfo::default().semaphore(self.rendered[i as usize]).stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)]);
            let mut submit = vk::SubmitInfo2::default().command_buffer_infos(&cmds);
            if let Some(sig) = signals.as_ref() {
                submit = submit.wait_semaphore_infos(&waits).signal_semaphore_infos(sig);
            }
            let _ = dev.queue_submit2(self.vk.queue, &[submit], self.frames[self.slot].fence);
            if let Some(i) = image_index {
                let sems = [self.rendered[i as usize]];
                let scs = [self.vk.swapchain.swapchain];
                let idx = [i];
                let r = self.vk.swapchain.loader.queue_present(self.vk.queue, &vk::PresentInfoKHR::default().wait_semaphores(&sems).swapchains(&scs).image_indices(&idx));
                if matches!(r, Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Ok(true)) {
                    let e = self.vk.swapchain.extent;
                    self.resize((e.width, e.height));
                }
            } else {
                let e = self.vk.swapchain.extent;
                self.resize((e.width, e.height));
            }
        }
        self.slot = (self.slot + 1) % FRAMES;
        self.frame_no += 1;
        self.begin_frame();
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        if let Err(e) = self.vk.recreate_swapchain(size) {
            eprintln!("[gs] swapchain: {e}");
            return;
        }
        let dev = self.vk.device.clone();
        for s in self.rendered.drain(..) {
            unsafe { dev.destroy_semaphore(s, None) };
        }
        self.rendered = (0..self.vk.swapchain.images.len()).map(|_| unsafe { dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).unwrap() }).collect();
    }

    fn evict_textures(&mut self) {
        if self.frame_no % 60 != 0 {
            return;
        }
        let now = self.frame_no;
        let old: Vec<TexKey> = self.textures.iter().filter(|(_, t)| now - t.last_used > TEXTURE_TTL).map(|(k, _)| *k).collect();
        for k in old {
            let t = self.textures.remove(&k).unwrap();
            self.texture_bytes -= t.bytes;
            self.frames[self.slot].garbage.push(Garbage::Image(t.image));
        }
    }

    /// Drop least recently used textures (not used this frame) until the
    /// cache is back under half its budget.
    fn evict_lru(&mut self) {
        let mut by_age: Vec<(u64, TexKey)> = self.textures.iter().filter(|(_, t)| t.last_used + 2 < self.frame_no).map(|(k, t)| (t.last_used, *k)).collect();
        by_age.sort_by_key(|e| e.0);
        for (_, k) in by_age {
            if self.texture_bytes <= TEXTURE_BUDGET / 2 {
                break;
            }
            let t = self.textures.remove(&k).unwrap();
            self.texture_bytes -= t.bytes;
            self.frames[self.slot].garbage.push(Garbage::Image(t.image));
        }
    }
}

/// Linear blit between colour images; the source is in GENERAL layout.
fn blit(dev: &ash::Device, cmd: vk::CommandBuffer, (src, from): (vk::Image, [vk::Offset3D; 2]), (dst, layout, to): (vk::Image, vk::ImageLayout, [vk::Offset3D; 2])) {
    let region = vk::ImageBlit { src_subresource: layers(vk::ImageAspectFlags::COLOR), src_offsets: from, dst_subresource: layers(vk::ImageAspectFlags::COLOR), dst_offsets: to };
    unsafe { dev.cmd_blit_image(cmd, src, vk::ImageLayout::GENERAL, dst, layout, &[region], vk::Filter::LINEAR) };
    barrier(dev, cmd);
}

/// Write a decoded texture as PNG (alpha scaled to 0-255) for inspection.
fn dump_texture(dir: &std::path::Path, key: &TexKey, t: gs::regs::Tex0, w: u32, h: u32, px: &[u32]) {
    let _ = std::fs::create_dir_all(dir);
    let id = key.tex0 ^ key.clut.rotate_left(17) ^ key.texa.rotate_left(33);
    let name = format!("tex_{id:016x}_{:?}_{w}x{h}_tbp{:x}.png", t.psm(), t.tbp0());
    // Composite over magenta so transparent areas are visible.
    let rgba: Vec<u8> = px
        .iter()
        .flat_map(|p| {
            let a = ((p >> 24).min(128) * 255 / 128) as f32 / 255.0;
            let c = |v: u32, bg: f32| ((v & 0xff) as f32 * a + bg * (1.0 - a)) as u8;
            [c(*p, 255.0), c(p >> 8, 0.0), c(p >> 16, 255.0), 255]
        })
        .collect();
    let _ = std::fs::write(dir.join(name), crate::png::encode(w, h, &rgba));
}

impl Drop for Hw {
    fn drop(&mut self) {
        unsafe {
            let _ = self.vk.device.device_wait_idle();
            self.vk.device.destroy_command_pool(self.cmd_pool, None);
        }
    }
}

impl gs::Backend for Hw {
    fn draw(&mut self, gs: &Gs, shape: Shape, v: &[Vertex]) {
        if let Err(e) = self.draw_inner(gs, shape, v) {
            eprintln!("[gs] draw: {e}");
        }
    }

    fn mem_written(&mut self, gs: &Gs, r: MemRect) {
        if self.trace_frame.is_some_and(|f| self.vblank >= f) {
            eprintln!("[draw] transfer bp={:#x} bw={} psm={:?} rect=({},{} {}x{})", r.bp, r.bw, r.psm, r.x, r.y, r.w, r.h);
        }
        if r.psm.is_indexed() || r.bw == 0 {
            return;
        }
        // Targets whose memory the write lands in (same buffer width and
        // a row-aligned base offset).
        let hits: Vec<(u32, u32)> = self
            .targets
            .iter()
            .filter(|(_, t)| t.bw == r.bw && t.psm.bpp() == r.psm.bpp() && r.bp >= t.bp)
            .filter_map(|(&k, t)| {
                let row_blocks = t.bw * 32;
                let off = r.bp - t.bp;
                (off % row_blocks == 0).then(|| (k, off / row_blocks * 32)).filter(|&(_, dy)| dy + r.y < t.h)
            })
            .collect();
        for (bp, dy) in hits {
            if let Err(e) = self.upload_to_target(&gs.vram, gs.texa, bp, r.x, r.y + dy, r.w, r.h) {
                eprintln!("[gs] upload: {e}");
            }
        }
    }
}
