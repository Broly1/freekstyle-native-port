//! Hardware GS backend: renders GS primitives with Vulkan into upscaled
//! render targets and presents the displayed frame buffer.
//!
//! Render targets are keyed by their GS base pointer and are `scale` times
//! the GS resolution. Colour targets stay in COLOR_ATTACHMENT_OPTIMAL outside
//! transfer passes (enabling lossless color compression on modern GPUs); depth
//! stays in DEPTH_ATTACHMENT_OPTIMAL, and sampled textures stay in
//! SHADER_READ_ONLY_OPTIMAL. Draws are batched while state is unchanged.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;

use ash::vk;
use gs::mem::{Psm, Vram};
use gs::{Gs, MemRect, Shape, Vertex};

use crate::blend::{self, Blend};
use crate::vk::{attachment_barrier, layers, range, transfer_barrier, transfer_to_host_barrier, transition, transition_discard, transition_levels, Buffer, Image, Result, Vk};

const FRAMES: usize = 3;
const VERTEX_BYTES: u64 = 32 << 20;
const STAGING_BYTES: u64 = 32 << 20;
const MAX_SETS: u32 = 8192;
const COLOR: vk::Format = vk::Format::R8G8B8A8_UNORM;
const DEPTH: vk::Format = vk::Format::D32_SFLOAT;
/// Unused textures are dropped after this many frames.
const TEXTURE_TTL: u64 = 600;
/// Texture cache budget; least recently used textures go first beyond it.
const TEXTURE_BUDGET: u64 = 1 << 30;
/// Video memory for HD replacements, kept apart from the texture cache: they
/// outlive the many cache entries that show the same picture.
const HD_BUDGET: u64 = 3 << 30;
/// Where the race HUD's boost gear is drawn (x0, y0, x1, y1), and how much
/// to shrink it around its centre. It is a few textured triangles: the fill
/// behind it, the ring and the mascot, which must all move together.
const GEAR: [f32; 4] = [6.0, 144.5, 134.25, 209.0];
const GEAR_SCALE: f32 = 0.8;
/// How near the gear's centre its parts are centred.
const GEAR_HUB: f32 = 2.0;

/// Whether a triangle (its corners) is part of the boost gear. Being in the
/// gear's box isn't enough, as text crosses it too: every part is either a
/// quad centred on the gear, or a slice of the fill's fan round that centre.
/// The fan's moving corner can land outside the box, as far as its diagonal.
fn in_gear(p: [(f32, f32); 3]) -> bool {
    let [x0, y0, x1, y1] = GEAR;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let central = |(x, y): (f32, f32)| (x - cx).abs() <= GEAR_HUB && (y - cy).abs() <= GEAR_HUB;
    let within = |mx: f32, my: f32| p.iter().all(|&(x, y)| (x0 - mx..=x1 + mx).contains(&x) && (y0 - my..=y1 + my).contains(&y));
    if p.iter().any(|&c| central(c)) {
        return within(cx - x0, cy - y0);
    }
    // half of a quad spans all of it, so its middle is the quad's
    let [a, b, c] = p;
    let mid = |a: f32, b: f32, c: f32| (a.min(b).min(c) + a.max(b).max(c)) / 2.0;
    within(0.0, 0.0) && central((mid(a.0, b.0, c.0), mid(a.1, b.1, c.1)))
}
/// Total asynchronous texture data (pictures, xBRZ sprites, HD pack
/// replacements) uploaded to the GPU per frame; the rest waits its turn.
/// Matches `STAGING_BYTES` (32 MB) so bursts stay inside the per-frame
/// staging ring without slowing HD pack streaming.
const UPLOAD_BUDGET_PER_FRAME: u64 = 32 << 20;
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
    /// Alpha bits the frame mask protects (with F_AMERGE).
    amask: u32,
}

// Push flag bits (mirrored in gs.frag).
const F_TME: u32 = 1;
const F_TCC: u32 = 8;
const F_FGE: u32 = 16;
const F_ATE: u32 = 32;
/// Texture is a render-target snapshot.
const F_RTTEX: u32 = 1 << 13;
/// Destination alpha test: draw only where the target's alpha bit (alpha
/// >= 0x80) equals F_DATM.
const F_DATE: u32 = 1 << 14;
const F_DATM: u32 = 1 << 15;
/// FBA: written alpha gets bit 7 set.
const F_FBA: u32 = 1 << 16;
/// Alpha bits in Push::amask (or all of them, when an RGB_ONLY alpha test
/// fails) keep the target's value: Freekstyle writes single alpha bits.
const F_AMERGE: u32 = 1 << 17;
/// T8h texture on a render target: its alpha byte indexes the palette.
const F_PAL8H: u32 = 1 << 18;
/// Blend with destination alpha (C = 1), done in the shader; A, B and D
/// selectors in bits 19-20, 21-22 and 23-24.
const F_DBLEND: u32 = 1 << 25;
const F_FACTOR_A: u32 = 1 << 26;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ShaderVariant {
    Simple,
    NoDst,
    Full,
    DualNoDst,
    DualFull,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct PipeKey {
    blend: Blend,
    write_mask: u32,
    variant: ShaderVariant,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct SamplerKey {
    linear: bool,
    /// HD replacement: trilinear and anisotropic over its mip chain.
    mipmapped: bool,
    repeat_u: bool,
    repeat_v: bool,
}

/// FxHash-style hasher for the renderer's maps: their keys are a few
/// integers, and SipHash cost about 10% of the render thread.
#[derive(Default)]
struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..c.len()].copy_from_slice(c);
            self.write_u64(u64::from_le_bytes(b));
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_u32(&mut self, v: u32) {
        self.write_u64(v as u64);
    }
}

type FastMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<Fx>>;

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

/// The previous draw's batch, with what it was worked out from.
#[derive(Clone, Copy)]
struct LastDraw {
    state_gen: u64,
    vram_writes: u64,
    hw_gen: u64,
    sprite: bool,
    key: BatchKey,
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
    palette: Option<u64>,
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

#[derive(Default)]
struct Recording {
    path: String,
    start: u64,
    end: u64,
    size: Option<(u32, u32)>,
    /// Scaled copy of the displayed picture.
    image: Option<Image>,
    encoder: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    failed: bool,
}

struct Texture {
    image: Image,
    last_used: u64,
    bytes: u64,
    /// A title or loading picture replaced by its 16:9 version.
    wide_picture: bool,
    /// Content hash, when the texture pack replaces this texture.
    hd: Option<u64>,
}

/// A texture pack replacement on the GPU.
struct HdImage {
    image: Image,
    last_used: u64,
    bytes: u64,
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
    /// A recorded frame, read back once the frame's fence has passed.
    Recorded(Buffer),
}

/// Completed off-thread texture work waiting for per-frame GPU upload budget.
enum PendingUpload {
    Picture(TexKey, crate::pictures::ReadyPicture),
    Sharpened(Sharpened),
    Hd(crate::retexture::Loaded),
}

impl PendingUpload {
    fn bytes(&self) -> u64 {
        match self {
            PendingUpload::Picture(_, pic) => pic.w as u64 * pic.h as u64 * 4,
            PendingUpload::Sharpened(d) => d.w as u64 * d.h as u64 * 4,
            PendingUpload::Hd(l) => l.levels.iter().map(|m| m.px.len() as u64 * 4).sum(),
        }
    }
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
    sets_used: u32,
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
    /// Upscale game textures with xBRZ to the internal resolution, so menu
    /// text and artwork stay sharp instead of showing stretched texels.
    pub sharp_textures: bool,
    /// Converted pictures (`game/images`); replacements in its `hd`.
    pub pictures: Option<std::path::PathBuf>,
    /// Texture pack folders (see `retexture`); a later one wins.
    pub texture_packs: Vec<std::path::PathBuf>,
    /// Where to record the textures the game shows, for `freekstyle-hd`
    /// to upscale (FREEK_DUMP_TEXTURES overrides it).
    pub texture_dump: Option<std::path::PathBuf>,
    /// On-disk pipeline cache file to prevent JIT shader compilation stutter.
    pub pipeline_cache: Option<std::path::PathBuf>,
}

pub struct Hw {
    pub vk: Vk,
    pub cfg: Config,
    cmd_pool: vk::CommandPool,
    frames: Vec<Frame>,
    slot: usize,
    pub frame_no: u64,
    /// Monotonic count of submitted command buffers (always advances in `finish_frame`,
    /// even when skipped frames accumulate into the current command buffer).
    submit_no: u64,
    /// Last non-zero window size, used to recreate a valid swapchain if it starts 0x0
    /// or becomes out of date before a resize event updates `swapchain.extent`.
    window_size: (u32, u32),
    minimized: bool,
    /// Vblank number of the frame being presented (the game's clock; the
    /// same one FREEK_PAD_SCRIPT uses).
    pub vblank: u64,
    rendered: Vec<vk::Semaphore>,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    vert: vk::ShaderModule,
    frag_simple: vk::ShaderModule,
    frag_nodst: vk::ShaderModule,
    frag: vk::ShaderModule,
    frag_dual_nodst: vk::ShaderModule,
    /// The fragment shader with the dual-source blend output.
    frag_dual: vk::ShaderModule,
    pipeline_cache: vk::PipelineCache,
    pipeline_cache_dirty: bool,
    pipeline_cache_dirty_at: u64,
    pipelines: FastMap<PipeKey, vk::Pipeline>,
    samplers: FastMap<SamplerKey, vk::Sampler>,
    targets: FastMap<u32, Target>,
    textures: FastMap<TexKey, Texture>,
    /// xBRZ upscaling runs on a worker thread; textures show at their
    /// original size until the sharp version is swapped in between frames.
    sharpen: Option<Sharpener>,
    pack: Option<crate::retexture::Pack>,
    hd_images: FastMap<u64, HdImage>,
    hd_bytes: u64,
    /// Completed pictures, xBRZ textures, and HD replacements waiting for upload budget.
    upload_waiting: VecDeque<PendingUpload>,
    /// Number of `PendingUpload::Sharpened` entries currently queued in `upload_waiting`.
    sharpen_queued: usize,
    dumper: Option<crate::retexture::Dumper>,
    /// Hash of the CLUT entries a texture uses: (GS CLUT generation,
    /// offset, entries, hash), recomputed only after a CLUT load.
    clut_hash: (u64, usize, usize, u64),
    /// Palettes for T8h reads of render targets, with `submit_no` of last use.
    palettes: HashMap<u64, (Image, u64)>,
    palette: u64,
    pictures: Option<crate::pictures::Pictures>,
    picture_loader: Option<PictureLoader>,
    /// Images retired from shared maps; kept alive `FRAMES` submits so in-flight GPUs finish sampling.
    deferred_garbage: VecDeque<(u64, Garbage)>,
    /// A wide picture fills the screen (set by drawing one, cleared by 3D
    /// or another background), so overlays follow its stretch curve.
    on_wide_picture: bool,
    /// Images for the blurred side bars, made on first use.
    blur: Vec<Image>,
    white: Image,
    rendering: Option<u32>,
    batch: Option<BatchKey>,
    /// For reusing the previous draw's batch (see `draw_inner`).
    last_draw: Option<LastDraw>,
    /// Bumped by everything outside drawing that changes what a draw
    /// works out (presenting, uploads, movies, resizing).
    draw_gen: u64,
    /// FREEK_TARGET_LOG, read once.
    target_log: bool,
    /// FREEK_GS_LOG, read once.
    gs_log: bool,
    /// FREEK_DISPLAY_LOG, read once.
    display_log: Option<u64>,
    /// FREEK_DRAW_CHECK: work out every reused batch again and report
    /// differences (a debugging aid for `draw_inner`).
    draw_check: bool,
    /// FREEK_GPU_TIMES: GPU time per kind of work.
    gpu_timer: Option<crate::gputime::GpuTimer>,
    verts: Vec<GpuVertex>,
    texture_bytes: u64,
    pub stats: Stats,
    /// Target shown at the last vblank, to count flips.
    shown: Option<u32>,
    /// The playing movie's current frame, shown instead of the GS display.
    movie: Option<Image>,
    /// Draw count when a movie ended. Until the game draws again the screen
    /// stays black: the display still holds whatever it showed before the
    /// movie, which would flash between two skipped movies.
    after_movie: Option<u64>,
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
    /// Draws that reused the previous draw's batch.
    pub fast_draws: u64,
    /// Times the displayed frame buffer changed (game frames shown).
    pub flips: u64,
    pub batches: u64,
    pub texture_uploads: u64,
    pub target_uploads: u64,
    /// Copies of a render target taken so a draw can sample it, and their
    /// total size in pixels.
    pub snapshots: u64,
    pub snapshot_pixels: u64,
    /// Batches drawn with the dual-source shader (SIMD8 on Intel before
    /// Xe2), and the vertices in them and in all batches.
    pub dual_batches: u64,
    pub dual_vertices: u64,
    pub vertices: u64,
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
                let sizes = [
                    vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: MAX_SETS * 3 },
                    vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: MAX_SETS },
                ];
                frames.push(Frame {
                    cmd,
                    fence: dev.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None).map_err(e("fence"))?,
                    acquired: dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(e("semaphore"))?,
                    vbuf: vk.create_buffer(VERTEX_BYTES, vk::BufferUsageFlags::VERTEX_BUFFER, &format!("vertices {n}"))?,
                    vbuf_used: 0,
                    staging: vk.create_buffer(STAGING_BYTES, vk::BufferUsageFlags::TRANSFER_SRC, &format!("staging {n}"))?,
                    staging_used: 0,
                    pool: dev.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(MAX_SETS).pool_sizes(&sizes), None).map_err(e("descriptor pool"))?,
                    sets_used: 0,
                    garbage: Vec::new(),
                });
            }
            // 0: the texture; 1: a snapshot of the target being drawn, read
            // by the destination alpha test and blends; 2: a palette;
            // 3: the shared sampler.
            let binding = |n, ty| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(n)
                    .descriptor_type(ty)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            };
            let bindings = [
                binding(0, vk::DescriptorType::SAMPLED_IMAGE),
                binding(1, vk::DescriptorType::SAMPLED_IMAGE),
                binding(2, vk::DescriptorType::SAMPLED_IMAGE),
                binding(3, vk::DescriptorType::SAMPLER),
            ];
            let set_layout = dev.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None).map_err(e("set layout"))?;
            let ranges = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT, offset: 0, size: std::mem::size_of::<Push>() as u32 }];
            let sets = [set_layout];
            let layout = dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sets).push_constant_ranges(&ranges), None).map_err(e("pipeline layout"))?;
            let module = |bytes: &[u8]| -> Result<vk::ShaderModule> {
                let code = ash::util::read_spv(&mut Cursor::new(bytes)).map_err(|e| e.to_string())?;
                dev.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(|e| format!("shader: {e:?}"))
            };
            let vert = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs.vert.spv")))?;
            let frag_simple = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs_simple.frag.spv")))?;
            let frag_nodst = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs_nodst.frag.spv")))?;
            let frag = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs.frag.spv")))?;
            let frag_dual_nodst = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs_dual_nodst.frag.spv")))?;
            let frag_dual = module(include_bytes!(concat!(env!("OUT_DIR"), "/gs_dual.frag.spv")))?;
            let white = vk.create_image(1, 1, COLOR, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST, "white")?;
            let rendered = (0..vk.swapchain.images.len())
                .map(|_| dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(e("semaphore")))
                .collect::<Result<Vec<_>>>()?;
            let window_size = (vk.swapchain.extent.width, vk.swapchain.extent.height);
            let gpu_timer = crate::gputime::GpuTimer::new(&vk.instance, vk.physical, &dev, vk.queue_family, FRAMES);
            let pipeline_cache = create_pipeline_cache(&dev, cfg.pipeline_cache.as_deref());
            let pictures = cfg.pictures.as_deref().map(|d| crate::pictures::Pictures::load(d, cfg.xbox_icons));
            let picture_loader = pictures.as_ref().map(|_| PictureLoader::spawn());
            let mut hw = Hw {
                vk,
                cmd_pool,
                frames,
                slot: 0,
                frame_no: 0,
                submit_no: 0,
                window_size,
                minimized: window_size.0 == 0 || window_size.1 == 0,
                vblank: 0,
                rendered,
                set_layout,
                layout,
                vert,
                frag_simple,
                frag_nodst,
                frag,
                frag_dual_nodst,
                frag_dual,
                pipeline_cache,
                pipeline_cache_dirty: false,
                pipeline_cache_dirty_at: 0,
                pipelines: FastMap::default(),
                samplers: FastMap::default(),
                targets: FastMap::default(),
                textures: FastMap::default(),
                sharpen: cfg.sharp_textures.then(Sharpener::spawn),
                pack: crate::retexture::Pack::open(&cfg.texture_packs),
                hd_images: FastMap::default(),
                hd_bytes: 0,
                upload_waiting: VecDeque::new(),
                sharpen_queued: 0,
                dumper: std::env::var_os("FREEK_DUMP_TEXTURES").map(Into::into).or_else(|| cfg.texture_dump.clone()).map(crate::retexture::Dumper::new),
                clut_hash: (u64::MAX, 0, 0, 0),
                palettes: HashMap::new(),
                palette: 0,
                pictures,
                picture_loader,
                deferred_garbage: VecDeque::new(),
                on_wide_picture: false,
                cfg,
                blur: Vec::new(),
                white,
                rendering: None,
                batch: None,
                last_draw: None,
                draw_gen: 0,
                target_log: std::env::var_os("FREEK_TARGET_LOG").is_some(),
                gs_log: std::env::var_os("FREEK_GS_LOG").is_some(),
                display_log: std::env::var("FREEK_DISPLAY_LOG").ok().and_then(|v| v.parse::<u64>().ok()),
                draw_check: std::env::var_os("FREEK_DRAW_CHECK").is_some(),
                gpu_timer,
                verts: Vec::new(),
                texture_bytes: 0,
                stats: Stats::default(),
                shown: None,
                movie: None,
                after_movie: None,
                shot_every: std::env::var("FREEK_SHOT_EVERY").ok().and_then(|v| v.parse().ok()),
                record: std::env::var("FREEK_RECORD").ok().and_then(|v| {
                    let mut it = v.rsplitn(3, ':');
                    let secs: u64 = it.next()?.parse().ok()?;
                    let start: u64 = it.next()?.parse().ok()?;
                    let path = it.next()?.to_string();
                    Some(Recording { path, start, end: start + secs * 60, ..Default::default() })
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
            transition(&hw.vk.device, cmd, hw.white.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            let clear = vk::ClearColorValue { float32: [1.0; 4] };
            hw.vk.device.cmd_clear_color_image(cmd, hw.white.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear, &[range(vk::ImageAspectFlags::COLOR)]);
            transition(&hw.vk.device, cmd, hw.white.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            Ok(hw)
        }
    }

    fn cmd(&self) -> vk::CommandBuffer {
        self.frames[self.slot].cmd
    }

    /// FREEK_GPU_TIMES: book GPU time from here on to `label`.
    fn mark(&mut self, label: crate::gputime::Label) {
        if let Some(t) = &mut self.gpu_timer {
            t.mark(&self.vk.device, self.frames[self.slot].cmd, self.slot, label);
        }
    }

    /// FREEK_GPU_TIMES: back to drawing into the open target, or between.
    fn mark_resume(&mut self) {
        if self.gpu_timer.is_none() {
            return;
        }
        let label = match self.rendering {
            Some(bp) => self.draw_label(bp),
            None => ("between", 0, 0),
        };
        self.mark(label);
    }

    fn draw_label(&self, bp: u32) -> crate::gputime::Label {
        let t = &self.targets[&bp];
        ("draw", t.w * self.cfg.scale_x, t.h * t.sy)
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
        if let Some(t) = &mut self.gpu_timer {
            t.begin(&dev, f.cmd, self.slot);
            t.mark(&dev, f.cmd, self.slot, ("between", 0, 0));
        }
        f.vbuf_used = 0;
        f.staging_used = 0;
        f.sets_used = 0;
        for g in std::mem::take(&mut f.garbage) {
            match g {
                Garbage::Image(i) => self.vk.destroy_image(i),
                Garbage::Buffer(b) => self.vk.destroy_buffer(b),
                Garbage::Recorded(b) => self.record_ready(b),
            }
        }
        while self.deferred_garbage.front().is_some_and(|(retire_at, _)| self.submit_no >= *retire_at) {
            let (_, g) = self.deferred_garbage.pop_front().unwrap();
            match g {
                Garbage::Image(i) => self.vk.destroy_image(i),
                Garbage::Buffer(b) => self.vk.destroy_buffer(b),
                Garbage::Recorded(b) => self.record_ready(b),
            }
        }
    }

    fn retire_image(&mut self, img: Image) {
        self.deferred_garbage.push_back((self.submit_no + FRAMES as u64, Garbage::Image(img)));
    }

    fn save_pipeline_cache(&mut self) {
        if self.pipeline_cache == vk::PipelineCache::null() {
            return;
        }
        if let Some(path) = &self.cfg.pipeline_cache {
            if let Ok(data) = unsafe { self.vk.device.get_pipeline_cache_data(self.pipeline_cache) } {
                if !data.is_empty() {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let tmp = path.with_extension("bin.tmp");
                    if std::fs::write(&tmp, &data).is_ok() && std::fs::rename(&tmp, path).is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
        }
        self.pipeline_cache_dirty = false;
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

    /// Upload RGBA8 pixels into a new w x h image and transition it to `final_layout`.
    fn upload_image(&mut self, w: u32, h: u32, pixels: &[u32], usage: vk::ImageUsageFlags, final_layout: vk::ImageLayout, name: &str) -> Result<Image> {
        let bytes = unsafe { std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4) };
        let (buf, off) = self.stage(bytes)?;
        let img = self.vk.create_image(w, h, COLOR, usage | vk::ImageUsageFlags::TRANSFER_DST, name)?;
        self.mark(("upload", 0, 0));
        let dev = &self.vk.device;
        let cmd = self.frames[self.slot].cmd;
        transition(dev, cmd, img.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let region = vk::BufferImageCopy::default()
            .buffer_offset(off)
            .image_subresource(layers(vk::ImageAspectFlags::COLOR))
            .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
        unsafe { dev.cmd_copy_buffer_to_image(cmd, buf, img.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]) };
        transition(dev, cmd, img.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, final_layout);
        self.mark_resume();
        Ok(img)
    }

    /// Upload a mip chain into a new image in SHADER_READ_ONLY_OPTIMAL layout.
    fn upload_levels(&mut self, levels: &[crate::retexture::Level]) -> Result<Image> {
        let img = self.vk.create_image_levels(levels[0].w, levels[0].h, levels.len() as u32, COLOR, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST, "HD texture")?;
        self.mark(("upload", 0, 0));
        transition_levels(&self.vk.device, self.frames[self.slot].cmd, img.image, vk::ImageAspectFlags::COLOR, levels.len() as u32, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        for (i, l) in levels.iter().enumerate() {
            let bytes = unsafe { std::slice::from_raw_parts(l.px.as_ptr() as *const u8, l.px.len() * 4) };
            let (buf, off) = self.stage(bytes)?;
            let region = vk::BufferImageCopy::default()
                .buffer_offset(off)
                .image_subresource(vk::ImageSubresourceLayers { mip_level: i as u32, ..layers(vk::ImageAspectFlags::COLOR) })
                .image_extent(vk::Extent3D { width: l.w, height: l.h, depth: 1 });
            unsafe { self.vk.device.cmd_copy_buffer_to_image(self.frames[self.slot].cmd, buf, img.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]) };
        }
        transition_levels(&self.vk.device, self.frames[self.slot].cmd, img.image, vk::ImageAspectFlags::COLOR, levels.len() as u32, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        self.mark_resume();
        Ok(img)
    }

    // ---- render targets ----

    fn end_rendering(&mut self) {
        self.flush();
        if self.rendering.take().is_some() {
            unsafe { self.vk.device.cmd_end_rendering(self.cmd()) };
            attachment_barrier(&self.vk.device, self.cmd());
            self.mark(("between", 0, 0));
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
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
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
        if self.gpu_timer.is_some() {
            let l = self.draw_label(bp);
            self.mark(l);
        }
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
        // Field mode buffers are 224 lines tall (at 2x vertical scale); frame
        // mode is 448 lines. Avoid oversizing field buffers to 512 lines, which
        // would allocate and load/store twice as many pixels on every pass.
        let min_floor = if sy > self.cfg.scale { 224 } else { 448 };
        let need_h = min_h.max(min_floor).next_multiple_of(32).min(2048);
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
        let mut t = Target { bp, bw, psm, w, h: need_h, sy, color, depth, snapshot: None, snapshot_valid: false, fresh: true };
        transition(&dev, cmd, t.depth.image, vk::ImageAspectFlags::DEPTH, vk::ImageLayout::UNDEFINED, vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL);
        // Growing: keep the old picture.
        if let Some(old) = self.targets.remove(&key) {
            if self.target_log {
                eprintln!("[target] recreate bp={bp:#x}: bw {} -> {bw}, h {} -> {need_h}, sy {} -> {sy} at vblank {}", old.bw, old.h, old.sy, self.vblank);
            }
            transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            transition(&dev, cmd, old.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
            let clear = vk::ClearColorValue { float32: [0.0; 4] };
            unsafe {
                dev.cmd_clear_color_image(cmd, t.color.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear, &[range(vk::ImageAspectFlags::COLOR)]);
                transfer_barrier(&dev, cmd);
                // Blit: the old target may have a different vertical scale.
                let blit = vk::ImageBlit {
                    src_subresource: layers(vk::ImageAspectFlags::COLOR),
                    src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: (old.w.min(w) * s) as i32, y: (old.h.min(need_h) * old.sy) as i32, z: 1 }],
                    dst_subresource: layers(vk::ImageAspectFlags::COLOR),
                    dst_offsets: [vk::Offset3D::default(), vk::Offset3D { x: (old.w.min(w) * s) as i32, y: (old.h.min(need_h) * sy) as i32, z: 1 }],
                };
                dev.cmd_blit_image(cmd, old.color.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, t.color.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::LINEAR);
            }
            transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            // Colour is valid now; only depth needs clearing on first use,
            // which LOAD_OP_CLEAR would also do to colour. Clear depth here.
            t.fresh = false;
            self.clear_depth_now(&t);
            self.retire_image(old.color);
            self.retire_image(old.depth);
            if let Some(s) = old.snapshot {
                self.retire_image(s);
            }
        } else {
            transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
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
        attachment_barrier(&self.vk.device, self.cmd());
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
        let px = gs::tex::decode_rect(vram, psm, bp, bw, x, y, w, h, texa);
        self.end_rendering();
        let tmp = self.upload_image(w, h, &px, vk::ImageUsageFlags::TRANSFER_SRC, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, "target upload")?;
        self.mark(("upload into target", 0, 0));
        let s = self.cfg.scale_x as i32;
        let dev = self.vk.device.clone();
        let cmd = self.frames[self.slot].cmd;
        let t = self.targets.get_mut(&key).unwrap();
        let sy = t.sy as i32;
        transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        if t.fresh {
            // The upload must not be wiped by a later first-use clear.
            let clear = vk::ClearColorValue { float32: [0.0; 4] };
            unsafe { dev.cmd_clear_color_image(cmd, t.color.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear, &[range(vk::ImageAspectFlags::COLOR)]) };
            transfer_barrier(&dev, cmd);
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
        unsafe { dev.cmd_blit_image(cmd, tmp.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, t.color.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::NEAREST) };
        transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        t.snapshot_valid = false;
        if let Some(g) = &mut self.gpu_timer {
            g.mark(&self.vk.device, cmd, self.slot, ("between", 0, 0));
        }
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

    /// `sprite`: drawn as a 2D rectangle (menus, text, HUD), the only
    /// textures worth xBRZ; on 3D surfaces it turns photo-like detail into
    /// flat blobs.
    fn texture(&mut self, gs: &Gs, sprite: bool) -> Result<(TexRef, [f32; 2], bool)> {
        let ctx = gs.context();
        let t = ctx.tex0;
        let (tw, th) = (1u32 << t.tw(), 1u32 << t.th());
        // A texture that starts at a render target samples its picture.
        if !t.psm().is_indexed() {
            // Prefer the target drawn with the texture's own width.
            let found = self.targets.get_key_value(&target_key(t.tbp0(), t.tbw())).or_else(|| self.targets.iter().find(|(_, rt)| rt.bp == t.tbp0()));
            if let Some((&k, rt)) = found {
                if self.target_log {
                    // Each kind of render-to-texture effect once, when first
                    // seen: line these up with what happened on screen.
                    static SEEN: std::sync::Mutex<Vec<(u32, u32, u64)>> = std::sync::Mutex::new(Vec::new());
                    let f = ctx.frame;
                    let id = (k, f.bp(), t.0 & ((1 << 34) - 1));
                    let mut seen = SEEN.lock().unwrap();
                    if !seen.contains(&id) {
                        seen.push(id);
                        eprintln!("[sample] vblank {}: draw into {:#x}/{:?} samples target {k:#x} ({}x{}) as tbp {:#x} tbw {} {:?} {tw}x{th}", self.vblank, f.bp(), f.psm(), rt.w, rt.h, t.tbp0(), t.tbw(), t.psm());
                    }
                }
                let scale = [tw as f32 / rt.w as f32, th as f32 / rt.h as f32];
                return Ok((TexRef::Target(k), scale, true));
            }
        }
        let indexed = t.psm().is_indexed();
        let clut = if indexed {
            let n = if matches!(t.psm(), gs::mem::Psm::T8 | gs::mem::Psm::T8h) { 256 } else { 16 };
            let off = if n == 16 { t.csa() as usize * 16 % 256 } else { 0 };
            if self.clut_hash.0 != gs.clut_gen || self.clut_hash.1 != off || self.clut_hash.2 != n {
                let h = (0..n).fold(0xcbf2_9ce4_8422_2325u64, |h, i| (h ^ gs.clut[(off + i) % 256] as u64).wrapping_mul(0x100_0000_01b3));
                self.clut_hash = (gs.clut_gen, off, n, h);
            }
            self.clut_hash.3 ^ (gs.clut_psm == Psm::Ct32) as u64
        } else {
            0
        };
        // Freekstyle's shadows read the outline it drew into a target's
        // alpha (T8h): look the alpha byte up in the palette on the GPU.
        if t.psm() == Psm::T8h {
            if let Some(k) = self.targets.get_key_value(&target_key(t.tbp0(), t.tbw())).map(|(&k, _)| k) {
                let pk = clut ^ (gs.texa.0 * (gs.clut_psm != Psm::Ct32) as u64).rotate_left(7);
                if let Some((_, last_used)) = self.palettes.get_mut(&pk) {
                    *last_used = self.submit_no;
                } else {
                    let pal: Vec<u32> = gs.clut.iter().map(|&c| if gs.clut_psm == Psm::Ct32 { c } else { gs::tex::rgba16(c, gs.texa) }).collect();
                    if self.palettes.len() >= 64 {
                        let stale: Vec<u64> = self
                            .palettes
                            .iter()
                            .filter(|(_, (_, last_used))| self.submit_no.saturating_sub(*last_used) >= FRAMES as u64)
                            .map(|(&h, _)| h)
                            .collect();
                        for h in stale {
                            if let Some((img, _)) = self.palettes.remove(&h) {
                                self.retire_image(img);
                            }
                        }
                    }
                    if self.palettes.len() >= 64 {
                        let mut by_age: Vec<(u64, u64)> = self.palettes.iter().map(|(&h, (_, last_used))| (*last_used, h)).collect();
                        by_age.sort_unstable();
                        for (_, h) in by_age.into_iter().take(self.palettes.len() / 2) {
                            if let Some((img, _)) = self.palettes.remove(&h) {
                                self.retire_image(img);
                            }
                        }
                    }
                    self.end_rendering();
                    let img = self.upload_image(256, 1, &pal, vk::ImageUsageFlags::SAMPLED, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, "palette")?;
                    self.palettes.insert(pk, (img, self.submit_no));
                }
                self.palette = pk;
                let rt = &self.targets[&k];
                return Ok((TexRef::Target(k), [tw as f32 / rt.w as f32, th as f32 / rt.h as f32], true));
            }
        }
        let uses_texa = !matches!(t.psm(), Psm::Ct32 | Psm::Z32) || (indexed && gs.clut_psm != Psm::Ct32);
        let pages = Vram::pages_of(t.psm(), t.tbp0(), t.tbw(), tw, th);
        let key = TexKey {
            // Only fields that change the decoded texels.
            tex0: t.0 & ((1 << 34) - 1) | ((t.csa() as u64) << 40) * indexed as u64,
            texa: if uses_texa { gs.texa.0 } else { 0 },
            clut,
            gen: gs.vram.generation(pages),
        };
        let submit = self.submit_no;
        if let Some(tex) = self.textures.get_mut(&key) {
            tex.last_used = submit;
            if let Some(hash) = tex.hd {
                self.want_hd(hash, tw, th);
            }
            return Ok((TexRef::Cache(key), [1.0, 1.0], false));
        }
        let (w, h, px) = gs::tex::decode(&gs.vram, t, gs.texa, &gs.clut, gs.clut_psm);
        let hash = (self.pack.is_some() || self.dumper.is_some()).then(|| crate::retexture::content_hash(w, h, &px));
        if let (Some(d), Some(hash)) = (&mut self.dumper, hash) {
            d.dump(hash, t.psm(), sprite, w, h, &px);
        }
        // Pictures in high resolution (loaded off-thread unless capturing);
        // PlayStation button prompts become Xbox buttons.
        let picture_job = match &self.pictures {
            Some(p) if !t.psm().is_indexed() => p.identify(w, h, &px, (self.cfg.scale_x, self.cfg.scale), self.cfg.widescreen),
            _ => None,
        };
        let mut hd = None;
        let (w, h, px, wide_picture) = match picture_job {
            Some(job) if self.capturing() => match job.build() {
                Some(pic) => (pic.w, pic.h, pic.px, pic.wide_picture),
                None => (w, h, px, false),
            },
            Some(job) => {
                if let Some(loader) = &mut self.picture_loader {
                    loader.queue(key, job);
                }
                (w, h, px, false)
            }
            None => match self.cfg.xbox_icons.then(|| crate::icons::replace(w, h, &px)).flatten() {
                Some((w, h, px)) => (w, h, px, false),
                None => {
                    hd = hash.filter(|&h| self.pack.as_ref().is_some_and(|p| p.has(h)));
                    match hd {
                        Some(hash) => {
                            if let Some(p) = &mut self.pack {
                                p.note_peak(hash, &px);
                            }
                            self.want_hd(hash, w, h);
                        }
                        None => {
                            if let Some(s) = self.sharpen.as_mut().filter(|_| sprite) {
                                s.queue(key, self.cfg.scale, w, h, &px);
                            }
                        }
                    }
                    (w, h, px, false)
                }
            },
        };
        self.end_rendering();
        let image = self.upload_image(w, h, &px, vk::ImageUsageFlags::SAMPLED, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, "texture")?;
        self.textures.insert(key, Texture { image, last_used: submit, bytes: w as u64 * h as u64 * 4, wide_picture, hd });
        self.texture_bytes += w as u64 * h as u64 * 4;
        if self.texture_bytes > TEXTURE_BUDGET {
            self.evict_lru();
        }
        self.stats.texture_uploads += 1;
        Ok((TexRef::Cache(key), [1.0, 1.0], false))
    }

    /// Keep a replacement in memory, or have it loaded.
    fn want_hd(&mut self, hash: u64, w: u32, h: u32) {
        match self.hd_images.get_mut(&hash) {
            Some(i) => i.last_used = self.submit_no,
            None => {
                if let Some(p) = &mut self.pack {
                    p.request(hash, w, h);
                }
            }
        }
    }

    /// The replacement to draw with, once it has reached the GPU.
    fn hd_image(&self, t: &Texture) -> Option<&HdImage> {
        self.hd_images.get(&t.hd?)
    }

    /// View to sample for a texture reference, snapshotting targets.
    fn ensure_snapshot(&mut self, bp: u32) -> Result<()> {
        if self.targets[&bp].snapshot.is_none() {
            let (w, h) = (self.targets[&bp].color.extent.width, self.targets[&bp].color.extent.height);
            let img = self.vk.create_image(w, h, COLOR, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST, "snapshot")?;
            transition(&self.vk.device, self.cmd(), img.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::UNDEFINED, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            self.targets.get_mut(&bp).unwrap().snapshot = Some(img);
        }
        Ok(())
    }

    fn copy_target_to_snapshot(&mut self, bp: u32, off: vk::Offset3D, extent: vk::Extent3D, label: &'static str) {
        let ext = self.targets[&bp].color.extent;
        self.mark((label, ext.width, ext.height));
        let dev = self.vk.device.clone();
        let cmd = self.cmd();
        let t = &self.targets[&bp];
        let snap_img = t.snapshot.as_ref().unwrap().image;
        transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        transition(&dev, cmd, snap_img, vk::ImageAspectFlags::COLOR, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let copy = vk::ImageCopy::default()
            .src_subresource(layers(vk::ImageAspectFlags::COLOR))
            .dst_subresource(layers(vk::ImageAspectFlags::COLOR))
            .src_offset(off)
            .dst_offset(off)
            .extent(extent);
        unsafe { dev.cmd_copy_image(cmd, t.color.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, snap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[copy]) };
        transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        transition(&dev, cmd, snap_img, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        self.mark(("between", 0, 0));
        self.stats.snapshots += 1;
        self.stats.snapshot_pixels += extent.width as u64 * extent.height as u64;
    }

    /// Snapshot of the target with the area a batch covers brought up to
    /// date, for shaders that read the destination. Copying only that area
    /// keeps the many small masked draws cheap.
    fn snapshot_rect(&mut self, bp: u32, verts: &[GpuVertex], scissor: vk::Rect2D) -> Result<vk::ImageView> {
        self.ensure_snapshot(bp)?;
        let t = &self.targets[&bp];
        if !t.snapshot_valid {
            let (s, sy) = (self.cfg.scale_x as f32, t.sy as f32);
            let (x0, y0, x1, y1) = verts.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(a, b, c, d), v| (a.min(v.x), b.min(v.y), c.max(v.x), d.max(v.y)));
            let ext = t.color.extent;
            let x0 = ((x0 * s).floor() as i32 - 1).max(scissor.offset.x).max(0);
            let y0 = ((y0 * sy).floor() as i32 - 1).max(scissor.offset.y).max(0);
            let x1 = ((x1 * s).ceil() as i32 + 1).min(scissor.offset.x + scissor.extent.width as i32).min(ext.width as i32);
            let y1 = ((y1 * sy).ceil() as i32 + 1).min(scissor.offset.y + scissor.extent.height as i32).min(ext.height as i32);
            if x1 > x0 && y1 > y0 {
                self.end_rendering();
                let off = vk::Offset3D { x: x0, y: y0, z: 0 };
                let extent = vk::Extent3D { width: (x1 - x0) as u32, height: (y1 - y0) as u32, depth: 1 };
                self.copy_target_to_snapshot(bp, off, extent, "copy part of");
            }
        }
        Ok(self.targets[&bp].snapshot.as_ref().unwrap().view)
    }

    fn texture_view(&mut self, r: TexRef) -> Result<vk::ImageView> {
        match r {
            TexRef::None => Ok(self.white.view),
            TexRef::Cache(k) => {
                let t = &self.textures[&k];
                Ok(self.hd_image(t).map_or(t.image.view, |i| i.image.view))
            }
            TexRef::Target(bp) => {
                if !self.targets[&bp].snapshot_valid {
                    self.ensure_snapshot(bp)?;
                    // Copying needs to happen outside rendering; the caller
                    // flushed the batch already.
                    let was = self.rendering;
                    self.end_rendering();
                    let ext = self.targets[&bp].color.extent;
                    self.copy_target_to_snapshot(bp, vk::Offset3D::default(), vk::Extent3D { width: ext.width, height: ext.height, depth: 1 }, "copy all of");
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
            let aniso = k.mipmapped && self.vk.anisotropy;
            let info = vk::SamplerCreateInfo::default()
                .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                .anisotropy_enable(aniso)
                .max_anisotropy(if aniso { 16f32.min(self.vk.max_anisotropy) } else { 1.0 })
                .mag_filter(filter)
                .min_filter(filter)
                .address_mode_u(mode(k.repeat_u))
                .address_mode_v(mode(k.repeat_v))
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .max_lod(if k.mipmapped { vk::LOD_CLAMP_NONE } else { 0.0 });
            unsafe { dev.create_sampler(&info, None).expect("sampler") }
        })
    }

    fn pipeline(&mut self, k: PipeKey) -> vk::Pipeline {
        if let Some(&p) = self.pipelines.get(&k) {
            return p;
        }
        let entry = c"main";
        let frag = match k.variant {
            ShaderVariant::Simple => self.frag_simple,
            ShaderVariant::NoDst => self.frag_nodst,
            ShaderVariant::Full => self.frag,
            ShaderVariant::DualNoDst => self.frag_dual_nodst,
            ShaderVariant::DualFull => self.frag_dual,
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(self.vert).name(entry),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(frag).name(entry),
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
        let p = unsafe { self.vk.device.create_graphics_pipelines(self.pipeline_cache, &[info], None) }.expect("pipeline")[0];
        self.pipelines.insert(k, p);
        self.pipeline_cache_dirty = true;
        self.pipeline_cache_dirty_at = self.frame_no;
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
        // The destination alpha test and masked alpha writes read the
        // target as it was before this batch.
        let dst = if k.push.flags & (F_DATE | F_AMERGE | F_DBLEND) != 0 { self.snapshot_rect(k.target, &verts, k.scissor).unwrap_or(self.white.view) } else { self.white.view };
        let pal = k.palette.and_then(|p| self.palettes.get(&p)).map_or(self.white.view, |(i, _)| i.view);
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
            let img = [vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: view, image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL }];
            let dst_img = [vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: dst, image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL }];
            let pal_img = [vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: pal, image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL }];
            let smp = [vk::DescriptorImageInfo { sampler, image_view: vk::ImageView::null(), image_layout: vk::ImageLayout::UNDEFINED }];
            let writes = [
                vk::WriteDescriptorSet::default().dst_set(sets[0]).dst_binding(0).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&img),
                vk::WriteDescriptorSet::default().dst_set(sets[0]).dst_binding(1).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&dst_img),
                vk::WriteDescriptorSet::default().dst_set(sets[0]).dst_binding(2).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&pal_img),
                vk::WriteDescriptorSet::default().dst_set(sets[0]).dst_binding(3).descriptor_type(vk::DescriptorType::SAMPLER).image_info(&smp),
            ];
            dev.update_descriptor_sets(&writes, &[]);
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
        self.frames[self.slot].sets_used += 1;
        self.stats.batches += 1;
        self.stats.vertices += verts.len() as u64;
        if k.pipe.blend.dual_source() {
            self.stats.dual_batches += 1;
            self.stats.dual_vertices += verts.len() as u64;
        }
        if let Some(t) = self.targets.get_mut(&k.target) {
            t.snapshot_valid = false;
        }
        self.verts = verts;
        self.verts.clear();
    }

    fn draw_inner(&mut self, gs: &Gs, shape: Shape, v: &[Vertex]) -> Result<()> {
        // Primitives in a row with nothing changed in between (the
        // triangles of a strip, the letters of a line of text) share the
        // batch: skip working it out again.
        let sprite = shape == Shape::Sprite;
        // `batch` only changes below, where `last_draw` is set to the same
        // key (or cleared), so a `last_draw` is always the current batch.
        let same = self.last_draw.as_ref().is_some_and(|l| {
            debug_assert!(self.batch.as_ref() == Some(&l.key));
            l.state_gen == gs.state_gen && l.vram_writes == gs.vram.writes() && l.hw_gen == self.draw_gen && l.sprite == sprite
        }) && self.trace_frame.is_none()
            && self.trace_tex.is_none();
        let tex = if same {
            self.stats.fast_draws += 1;
            if self.draw_check {
                let key = self.last_draw.as_ref().unwrap().key;
                static BAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let full = self.batch_key(gs, shape, v)?;
                let bad = if full != Some(key) { BAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 } else { BAD.load(std::sync::atomic::Ordering::Relaxed) };
                if self.stats.fast_draws % 1_000_000 == 0 || (full != Some(key) && bad <= 5) {
                    eprintln!("[draw-check] {} reused batches, {bad} differed", self.stats.fast_draws);
                }
            }
            self.last_draw.as_ref().unwrap().key.tex
        } else {
            self.last_draw = None;
            let Some(key) = self.batch_key(gs, shape, v)? else { return Ok(()) };
            if self.batch.as_ref() != Some(&key) {
                self.flush();
                self.batch = Some(key);
            }
            self.last_draw = Some(LastDraw { state_gen: gs.state_gen, vram_writes: gs.vram.writes(), hw_gen: self.draw_gen, sprite, key });
            key.tex
        };
        self.add_vertices(gs, shape, v, tex);
        Ok(())
    }

    /// Everything a draw sets up before its vertices: target, texture,
    /// pipeline state. None when the draw can't change anything.
    fn batch_key(&mut self, gs: &Gs, shape: Shape, v: &[Vertex]) -> Result<Option<BatchKey>> {
        let attrs = gs.attrs();
        let ctx = *gs.context();
        if self.trace_tex.is_some_and(|tbp| attrs.textured() && ctx.tex0.tbp0() == tbp) {
            self.trace_tex = None;
            self.trace_frame = Some(self.vblank);
        }
        if self.trace_frame.is_some_and(|f| self.vblank >= f) {
            let (f, z, t) = (ctx.frame, ctx.zbuf, ctx.test);
            eprintln!(
                "[draw] {shape:?} v0=({:.1},{:.1},z={:#x}) v1=({:.1},{:.1}) rgba={:?}/{:?} tme={} psm={:?} tbp={:#x} fb={:#x}/{:?} mask={:#x} zb={:#x}/{:?} zmsk={} zte={} ztst={} ate={} atst={} aref={} afail={} date={} datm={} fba={} abe={} alpha={:?} sc={:?} prim={:#x}",
                v[0].x, v[0].y, v[0].z, v.get(1).map_or(0.0, |x| x.x), v.get(1).map_or(0.0, |x| x.y), v[0].rgba, v.last().map_or([0; 4], |x| x.rgba),
                attrs.textured(), ctx.tex0.psm(), ctx.tex0.tbp0(), f.bp(), f.psm(), f.mask(), z.bp(), z.psm(), z.masked(),
                t.zte(), t.ztst(), t.alpha_test(), t.atst(), t.aref(), t.afail(), t.date(), t.datm(), ctx.fba, attrs.blend(), ctx.alpha.abcd(), ctx.scissor.rect(), attrs.0
            );
        }
        let (fb, zb, test) = (ctx.frame, ctx.zbuf, ctx.test);
        let (sx0, sy0, sx1, sy1) = ctx.scissor.rect();
        if sx1 < sx0 || sy1 < sy0 {
            return Ok(None);
        }
        let color_off = fb.mask() == u32::MAX;
        if color_off && zb.masked() {
            return Ok(None);
        }
        let ysc = self.vertical_scale(gs);
        let bp = self.target(fb.bp(), fb.bw(), fb.psm(), sy1 + 1, ysc)?;

        // Texture first: it may need transfers outside rendering.
        let (tex, tex_scale, rt_tex) = if attrs.textured() { self.texture(gs, shape == Shape::Sprite)? } else { (TexRef::None, [1.0, 1.0], false) };

        let (ba, bb, bc, bd) = ctx.alpha.abcd();
        // Destination alpha as the factor goes through the shader: targets
        // keep the alpha byte, the factor is byte / 0x80.
        let dst_blend = attrs.blend() && bc == 1;
        let mut blend = if attrs.blend() && !dst_blend { blend::map(ctx.alpha.abcd()) } else { Blend::OFF };
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
        // Alpha isn't written: the source alpha factor can travel as the
        // output alpha, sparing the dual-source shader (SIMD8 on Intel
        // before Xe2).
        let factor_a = blend.dual_source() && write_mask & vk::ColorComponentFlags::A.as_raw() == 0;
        if factor_a {
            blend = blend.single_source();
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
        // 24-bit targets have no alpha to test.
        if test.date() && fb.psm() != Psm::Ct24 {
            flags |= F_DATE | if test.datm() { F_DATM } else { 0 };
        }
        let amask = (fm >> 24) & 0xff;
        if write_mask & vk::ColorComponentFlags::A.as_raw() != 0 {
            if ctx.fba {
                flags |= F_FBA;
            }
            if amask != 0 || (test.alpha_test() && test.afail() == 3) {
                flags |= F_AMERGE;
            }
        }
        if dst_blend {
            flags |= F_DBLEND | ba << 19 | bb << 21 | bd << 23;
        }
        if factor_a {
            flags |= F_FACTOR_A;
        }
        let palette = (rt_tex && tex0.psm() == Psm::T8h).then_some(self.palette);
        if palette.is_some() {
            flags |= F_PAL8H;
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
            amask,
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
        let mipmapped = matches!(tex, TexRef::Cache(k) if self.hd_image(&self.textures[&k]).is_some());
        let sampler = SamplerKey {
            linear: mipmapped || ctx.tex1.mag_linear() || ctx.tex1.min_linear(),
            mipmapped,
            repeat_u: ctx.clamp.wms() == 0,
            repeat_v: ctx.clamp.wmt() == 0,
        };
        let needs_dst = flags & (F_DATE | F_AMERGE | F_DBLEND | F_PAL8H) != 0;
        let variant = if blend.dual_source() {
            if needs_dst { ShaderVariant::DualFull } else { ShaderVariant::DualNoDst }
        } else if needs_dst {
            ShaderVariant::Full
        } else if flags & (F_ATE | F_FBA) == 0 && blend.shader_scale == 0 {
            ShaderVariant::Simple
        } else {
            ShaderVariant::NoDst
        };
        let key = BatchKey {
            target: bp,
            pipe: PipeKey { blend, write_mask, variant },
            tex,
            sampler,
            push,
            scissor,
            depth: (!zb.masked(), ztst),
            palette,
        };
        Ok(Some(key))
    }

    fn add_vertices(&mut self, gs: &Gs, shape: Shape, v: &[Vertex], tex: TexRef) {
        let attrs = gs.attrs();
        let ctx = *gs.context();
        let (zb, tex0) = (ctx.zbuf, ctx.tex0);
        let zscale = match zb.psm() {
            Psm::Z32 => 1.0 / 4_294_967_296.0,
            Psm::Z24 => 1.0 / 16_777_216.0,
            _ => 1.0 / 65_536.0,
        };
        let (tw, th) = ((1u32 << tex0.tw()) as f32, (1u32 << tex0.th()) as f32);
        let fst = attrs.fst();
        let overlay = self.overlay(&ctx, v, tex);
        let [gx0, gy0, gx1, gy1] = GEAR;
        let (gx, gy) = ((gx0 + gx1) / 2.0, (gy0 + gy1) / 2.0);
        let gear = shape == Shape::Triangle
            && attrs.textured()
            && matches!(overlay, Overlay::Squeeze(_))
            && in_gear([(v[0].x, v[0].y), (v[1].x, v[1].y), (v[2].x, v[2].y)]);
        let conv = |v: &Vertex| GpuVertex {
            x: {
                let x = if gear { gx + (v.x - gx) * GEAR_SCALE } else { v.x };
                match overlay {
                    Overlay::None => x,
                    Overlay::Squeeze(c) => c + (x - c) * 0.75,
                    Overlay::Picture(c) => c + c * crate::pictures::wide_position((x - c) / c),
                }
            },
            y: if gear { gy + (v.y - gy) * GEAR_SCALE } else { v.y },
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
        } else if (x0 <= 1.0 || x1 >= width - 1.0) && x1 - x0 >= width / 3.0 {
            // Menu frames are halves that meet in the middle and reach the
            // screen edges, over a panel that spans the width: squeezing
            // them would pull them in from the edges of that panel.
            Overlay::None
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

    /// Whether a screenshot or video capture is waiting on the next frame.
    pub fn capturing(&self) -> bool {
        self.record.is_some() || self.shot.is_some()
    }

    /// Present the frame buffer the display circuits point at, or submit only
    /// offscreen draws when `display` is false to catch up with the EE.
    pub fn present(&mut self, gs: &Gs, display: bool) {
        self.draw_gen += 1;
        self.end_rendering();
        // A traced frame ends here.
        if self.trace_frame.is_some_and(|f| self.vblank >= f) {
            eprintln!("[draw] --- present at vblank {} ---", self.vblank);
            self.trace_left = self.trace_left.saturating_sub(1);
            if self.trace_left == 0 {
                self.trace_frame = None;
            }
        }
        if !display && !self.capturing() {
            if self.after_movie.is_some_and(|d| d != self.stats.draws) {
                self.after_movie = None;
            }
            let f = &self.frames[self.slot];
            if f.vbuf_used > VERTEX_BYTES / 2
                || f.staging_used > STAGING_BYTES / 2
                || f.sets_used > MAX_SETS / 2
                || f.garbage.len() > 256
                || self.deferred_garbage.len() > 256
            {
                self.finish_frame(None, false);
            } else {
                self.frame_no += 1;
            }
            self.evict_textures();
            return;
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
        if self.frame_no % 120 == 0 && self.gs_log {
            eprintln!("[gs] frame {} display {:?} pmode {:#x} dispfb {:#x?} display {:#x?} targets {:?} textures {} {:?}", self.frame_no, src, d.pmode, d.dispfb, d.display, self.targets.keys().collect::<Vec<_>>(), self.textures.len(), self.stats);
        }
        if self.display_log.is_some_and(|v| (v..v + 12).contains(&self.vblank)) {
            let t = src.and_then(|s| self.targets.get(&s.0)).map(|t| (t.bw, t.psm, t.w, t.h));
            eprintln!("[display] vblank {} src {:?} target {:?} dispfb {:#x?} display {:#x?}", self.vblank, src, t, d.dispfb, d.display);
        }
        let stale = self.after_movie.is_some_and(|d| d == self.stats.draws);
        if !stale {
            if self.after_movie.take().is_some() && std::env::var_os("FREEK_MOVIE_LOG").is_some() {
                eprintln!("[movie] game drew again at vblank {}", self.vblank);
            }
        }
        let will_record = src.is_some() && self.record.as_ref().is_some_and(|r| self.vblank >= r.start && self.vblank < r.end);
        let will_shot = src.is_some() && self.shot.as_ref().is_some_and(|(frame, _)| self.vblank >= *frame);
        let need_read = src.is_some() && (!stale || will_record || will_shot);
        if let (Some((bp, ..)), true) = (src, need_read) {
            let img = self.targets[&bp].color.image;
            transition(&self.vk.device, self.cmd(), img, vk::ImageAspectFlags::COLOR, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        }
        if let (Some(r), Some(src)) = (&self.record, src) {
            if self.vblank >= r.end {
                self.stop_recording();
            } else if self.vblank >= r.start {
                if let Err(e) = self.record_capture(src) {
                    eprintln!("[gs] recording: {e}");
                    self.stop_recording();
                }
            }
        }
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
        let aspect = if self.cfg.widescreen { (16, 9) } else { (4, 3) };
        let shown = src.map(|(bp, x, y, w, h)| {
            let t = &self.targets[&bp];
            let (s, ys) = (self.cfg.scale_x as i32, t.sy as i32);
            let sx1 = ((x + w) as i32 * s).min(t.color.extent.width as i32);
            // The game's last full-screen draw stops half a line short, which
            // only shows at high resolution; a TV hid it in the overscan.
            let sy1 = ((y + h) as i32 * ys - ys / 2).min(t.color.extent.height as i32);
            (t.color.image, [vk::Offset3D { x: x as i32 * s, y: y as i32 * ys, z: 0 }, vk::Offset3D { x: sx1, y: sy1, z: 1 }], aspect)
        });
        if stale && need_read {
            if let Some((bp, ..)) = src {
                let img = self.targets[&bp].color.image;
                transition(&self.vk.device, self.cmd(), img, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            }
        }
        let restore = if !stale { src.map(|s| s.0) } else { None };
        self.submit_and_present(shown.filter(|_| !stale), restore);
        if let Some((buf, w, h)) = readback {
            self.save_shot(buf, w, h);
        }
        self.evict_textures();
        if self.pipeline_cache_dirty && self.frame_no >= self.pipeline_cache_dirty_at.saturating_add(120) {
            self.save_pipeline_cache();
        }
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
        unsafe { self.vk.device.cmd_copy_image_to_buffer(self.cmd(), image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buf.buffer, &[region]) };
        transfer_to_host_barrier(&self.vk.device, self.cmd());
        Ok((buf, w, h))
    }

    /// Queue one recorded frame: the GPU scales the displayed region into
    /// the recording's size (the first frame's, at most 1280 wide, so
    /// display mode changes don't break the stream) and copies it to a
    /// buffer read once this frame's fence has passed.
    fn record_capture(&mut self, (bp, x, y, w, h): (u32, u32, u32, u32, u32)) -> Result<()> {
        let s = self.cfg.scale_x;
        let t = &self.targets[&bp];
        let (sw, sh) = ((w * s).min(t.color.extent.width - x * s), (h * t.sy).min(t.color.extent.height - y * t.sy));
        let (sx, sy, image) = (x * s, y * t.sy, t.color.image);
        let Some(r) = self.record.as_mut() else { return Ok(()) };
        let (ow, oh) = *r.size.get_or_insert(if sw > 1280 { (1280, (sh * 1280 / sw) & !1) } else { (sw & !1, sh & !1) });
        if r.image.is_none() {
            let img = self.vk.create_image(ow, oh, COLOR, vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST, "recording")?;
            self.record.as_mut().unwrap().image = Some(img);
        }
        let small = self.record.as_ref().unwrap().image.as_ref().unwrap().image;
        let cmd = self.cmd();
        let o = |x: u32, y: u32| vk::Offset3D { x: x as i32, y: y as i32, z: 1 };
        transition_discard(&self.vk.device, cmd, small, vk::ImageAspectFlags::COLOR, vk::PipelineStageFlags2::TRANSFER, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        blit(&self.vk.device, cmd, (image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, [vk::Offset3D { z: 0, ..o(sx, sy) }, o(sx + sw, sy + sh)]), (small, vk::ImageLayout::TRANSFER_DST_OPTIMAL, [vk::Offset3D::default(), o(ow, oh)]));
        transition(&self.vk.device, cmd, small, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        let buf = self.vk.create_buffer(ow as u64 * oh as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST, "recording")?;
        let region = vk::BufferImageCopy::default().image_subresource(layers(vk::ImageAspectFlags::COLOR)).image_extent(vk::Extent3D { width: ow, height: oh, depth: 1 });
        unsafe { self.vk.device.cmd_copy_image_to_buffer(cmd, small, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buf.buffer, &[region]) };
        transfer_to_host_barrier(&self.vk.device, cmd);
        self.frames[self.slot].garbage.push(Garbage::Recorded(buf));
        Ok(())
    }

    /// Start the encoder for a recording of `ow`x`oh` RGBA frames: a thread
    /// feeding ffmpeg, so encoding never holds up the frame.
    fn start_encoder(path: &str, (ow, oh): (u32, u32)) -> Option<std::sync::mpsc::Sender<Vec<u8>>> {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb0", "-s", &format!("{ow}x{oh}"), "-r", "60", "-i", "-"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "20", "-pix_fmt", "yuv420p", path])
            .stdin(std::process::Stdio::piped());
        // No console window beside the (windowed) game on Windows.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        let child = cmd.spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[gs] recording: cannot start ffmpeg: {e}");
                return None;
            }
        };
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let path = path.to_string();
        std::thread::spawn(move || {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            while let Ok(frame) = rx.recv() {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
            drop(stdin);
            let _ = child.wait();
            eprintln!("[gs] recording saved to {path}");
        });
        Some(tx)
    }

    /// A recorded frame's pixels are ready (its fence has passed).
    fn record_ready(&mut self, mut buf: Buffer) {
        if let Some(r) = &mut self.record {
            if let Some(size) = r.size {
                if r.encoder.is_none() && !r.failed {
                    r.encoder = Self::start_encoder(&r.path, size);
                    r.failed = r.encoder.is_none();
                }
                if let Some(tx) = &r.encoder {
                    let _ = tx.send(buf.mapped()[..(size.0 * size.1 * 4) as usize].to_vec());
                }
            }
        }
        self.vk.destroy_buffer(buf);
    }

    /// End the recording; the encoder thread finishes the file.
    fn stop_recording(&mut self) {
        if let Some(r) = self.record.take() {
            if let Some(img) = r.image {
                self.retire_image(img);
            }
            eprintln!("[capture] recording stopped at vblank {}", self.vblank);
        }
    }

    /// Log every draw of four frames starting `delay` vblanks from now (for
    /// glitches seen while playing).
    pub fn trace_in(&mut self, delay: u64) {
        self.draw_gen += 1;
        eprintln!("[draw] --- trace requested at vblank {} for vblank {} ---", self.vblank, self.vblank + delay);
        self.trace_frame = Some(self.vblank + delay);
        self.trace_left = 4;
    }

    /// Log every draw of the frame `delay` vblanks from now and save that
    /// frame's picture to `captures/`, to match a glitch on screen with its
    /// draws.
    pub fn trace_shot(&mut self, delay: u64) {
        self.draw_gen += 1;
        let at = self.vblank + delay.max(1);
        let _ = std::fs::create_dir_all("captures");
        // Draws logged from `at` on are shown at the next present.
        eprintln!("[draw] --- trace and shot of vblank {} ---", at + 1);
        self.trace_frame = Some(at);
        self.trace_left = 2;
        self.shot = Some((at + 1, format!("captures/trace_{:06}.png", at + 1)));
    }

    /// F12: start recording to `captures/`, or stop. For glitches seen
    /// while playing; frame n of the video is vblank start + n (see the
    /// FREEK_PAD_LOG button log for the same numbering).
    pub fn toggle_recording(&mut self) {
        self.draw_gen += 1;
        if self.record.is_some() {
            self.stop_recording();
            return;
        }
        let _ = std::fs::create_dir_all("captures");
        let start = self.vblank + 1;
        let path = format!("captures/clip_{start:06}.mkv");
        eprintln!("[capture] recording from vblank {start} to {path}; F12 again to stop");
        self.record = Some(Recording { path, start, end: u64::MAX, ..Default::default() });
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
        let pixels = buf.mapped()[..(w * h * 4) as usize].to_vec();
        self.vk.destroy_buffer(buf);
        // Encoding and writing take a while; keep the frame going.
        std::thread::spawn(move || match std::fs::write(&path, crate::png::encode(w, h, &pixels)) {
            Ok(()) => eprintln!("[gs] saved {w}x{h} screenshot to {path}"),
            Err(e) => eprintln!("[gs] screenshot {path}: {e}"),
        });
    }

    /// Show a movie frame (4:3, whatever the game's display mode).
    pub fn present_movie(&mut self, f: &gs::MovieFrame) {
        self.draw_gen += 1;
        self.end_rendering();
        if self.movie.as_ref().is_none_or(|m| (m.extent.width, m.extent.height) != (f.w, f.h)) {
            match self.vk.create_image(f.w, f.h, COLOR, vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST, "movie") {
                Ok(img) => {
                    if let Some(old) = self.movie.replace(img) {
                        self.retire_image(old);
                    }
                }
                Err(e) => {
                    eprintln!("[gs] movie: {e}");
                    return;
                }
            }
        }
        let bytes = unsafe { std::slice::from_raw_parts(f.rgba.as_ptr() as *const u8, f.rgba.len() * 4) };
        let Ok((buf, off)) = self.stage(bytes) else { return };
        let image = self.movie.as_ref().unwrap().image;
        transition_discard(&self.vk.device, self.cmd(), image, vk::ImageAspectFlags::COLOR, vk::PipelineStageFlags2::TRANSFER, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let region = vk::BufferImageCopy::default()
            .buffer_offset(off)
            .image_subresource(layers(vk::ImageAspectFlags::COLOR))
            .image_extent(vk::Extent3D { width: f.w, height: f.h, depth: 1 });
        unsafe { self.vk.device.cmd_copy_buffer_to_image(self.cmd(), buf, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]) };
        transition(&self.vk.device, self.cmd(), image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        let rect = [vk::Offset3D::default(), vk::Offset3D { x: f.w as i32, y: f.h as i32, z: 1 }];
        self.submit_and_present(Some((image, rect, (4, 3))), None);
    }

    /// The movie is over: back to the GS display.
    pub fn end_movie(&mut self) {
        self.draw_gen += 1;
        if let Some(img) = self.movie.take() {
            self.retire_image(img);
        }
        // Also for a movie skipped before its first frame: there was no image.
        self.after_movie = Some(self.stats.draws);
    }

    /// Show `src` (an image in TRANSFER_SRC_OPTIMAL, the rectangle to show,
    /// its aspect ratio) in the window, or just black, and optionally restore
    /// `restore_target` back to COLOR_ATTACHMENT_OPTIMAL before submission.
    fn submit_and_present(&mut self, src: Option<(vk::Image, [vk::Offset3D; 2], (i32, i32))>, restore_target: Option<u32>) {
        self.mark(("present", 0, 0));
        if self.cfg.blur_bars && !self.cfg.stretch && self.blur.is_empty() {
            for (w, h) in BLUR_CHAIN {
                match self.vk.create_image(w, h, COLOR, vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST, "blur") {
                    Ok(img) => self.blur.push(img),
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
        let has_swapchain = !self.minimized
            && self.vk.swapchain.swapchain != vk::SwapchainKHR::null()
            && self.rendered.len() == self.vk.swapchain.images.len();
        let image_index = if has_swapchain {
            match unsafe { self.vk.swapchain.loader.acquire_next_image(self.vk.swapchain.swapchain, u64::MAX, f.acquired, vk::Fence::null()) } {
                Ok((i, _)) => Some(i),
                Err(_) => None,
            }
        } else {
            None
        };
        if let Some(i) = image_index {
            let sw = self.vk.swapchain.images[i as usize];
            let ext = self.vk.swapchain.extent;
            transition_discard(&dev, cmd, sw, vk::ImageAspectFlags::COLOR, vk::PipelineStageFlags2::TRANSFER, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            unsafe {
                let black = vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] };
                dev.cmd_clear_color_image(cmd, sw, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &black, &[range(vk::ImageAspectFlags::COLOR)]);
            }
            transfer_barrier(&dev, cmd);
            if let Some((image, src_rect, (ax, ay))) = src {
                // Fit the aspect ratio into the window unless stretching.
                let (ww, wh) = (ext.width as i32, ext.height as i32);
                let (dw, dh) = if self.cfg.stretch {
                    (ww, wh)
                } else if ww * ay > wh * ax {
                    (wh * ax / ay, wh)
                } else {
                    (ww, ww * ay / ax)
                };
                let (dx, dy) = ((ww - dw) / 2, (wh - dh) / 2);
                if self.cfg.blur_bars && dw < ww {
                    // Blur by halving, then stretch the smallest over the window.
                    let mut from = (image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, src_rect);
                    for img in &self.blur {
                        let to = [vk::Offset3D::default(), vk::Offset3D { x: img.extent.width as i32, y: img.extent.height as i32, z: 1 }];
                        transition_discard(&dev, cmd, img.image, vk::ImageAspectFlags::COLOR, vk::PipelineStageFlags2::TRANSFER, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
                        blit(&dev, cmd, from, (img.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, to));
                        transition(&dev, cmd, img.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
                        from = (img.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, to);
                    }
                    blit(&dev, cmd, from, (sw, vk::ImageLayout::TRANSFER_DST_OPTIMAL, [vk::Offset3D::default(), vk::Offset3D { x: ww, y: wh, z: 1 }]));
                    transfer_barrier(&dev, cmd);
                }
                let to = [vk::Offset3D { x: dx, y: dy, z: 0 }, vk::Offset3D { x: dx + dw, y: dy + dh, z: 1 }];
                blit(&dev, cmd, (image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, src_rect), (sw, vk::ImageLayout::TRANSFER_DST_OPTIMAL, to));
            }
            transition(&dev, cmd, sw, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR);
        }
        if let Some(bp) = restore_target {
            if let Some(t) = self.targets.get(&bp) {
                transition(&dev, cmd, t.color.image, vk::ImageAspectFlags::COLOR, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            }
        }
        self.finish_frame(image_index, true);
    }

    /// End the active command buffer, submit GPU work, optionally present the
    /// swapchain image, and prepare the next frame slot.
    fn finish_frame(&mut self, image_index: Option<u32>, present: bool) {
        self.mark(("end", 0, 0));
        let dev = self.vk.device.clone();
        let cmd = self.cmd();
        unsafe {
            let _ = dev.end_command_buffer(cmd);
            let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let waits = [vk::SemaphoreSubmitInfo::default().semaphore(self.frames[self.slot].acquired).stage_mask(vk::PipelineStageFlags2::TRANSFER)];
            let signals = image_index.map(|i| [vk::SemaphoreSubmitInfo::default().semaphore(self.rendered[i as usize]).stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)]);
            let mut submit = vk::SubmitInfo2::default().command_buffer_infos(&cmds);
            if let Some(sig) = signals.as_ref() {
                submit = submit.wait_semaphore_infos(&waits).signal_semaphore_infos(sig);
            }
            let _ = dev.queue_submit2(self.vk.queue, &[submit], self.frames[self.slot].fence);
            if present {
                let mut need_resize = false;
                let probe_restore = self.minimized && self.frame_no % 60 == 0;
                if let Some(i) = image_index {
                    let sems = [self.rendered[i as usize]];
                    let scs = [self.vk.swapchain.swapchain];
                    let idx = [i];
                    let r = self.vk.swapchain.loader.queue_present(self.vk.queue, &vk::PresentInfoKHR::default().wait_semaphores(&sems).swapchains(&scs).image_indices(&idx));
                    if matches!(r, Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Ok(true)) {
                        need_resize = true;
                    }
                } else if !self.minimized || probe_restore {
                    need_resize = true;
                }
                if need_resize && self.window_size.0 > 0 && self.window_size.1 > 0 {
                    self.resize(self.window_size);
                }
            }
        }
        self.slot = (self.slot + 1) % FRAMES;
        self.frame_no += 1;
        self.submit_no += 1;
        self.begin_frame();
        self.apply_uploads();
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        if size.0 == 0 || size.1 == 0 {
            self.minimized = true;
            return;
        }
        self.window_size = size;
        self.draw_gen += 1;
        match self.vk.recreate_swapchain(size) {
            Ok(true) => self.minimized = false,
            Ok(false) => {
                self.minimized = true;
                return;
            }
            Err(e) => {
                eprintln!("[gs] swapchain: {e}");
                return;
            }
        }
        let dev = self.vk.device.clone();
        for s in self.rendered.drain(..) {
            unsafe { dev.destroy_semaphore(s, None) };
        }
        let mut rendered = Vec::with_capacity(self.vk.swapchain.images.len());
        for _ in 0..self.vk.swapchain.images.len() {
            match unsafe { dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) } {
                Ok(sem) => rendered.push(sem),
                Err(e) => {
                    eprintln!("[gs] semaphore: {e:?}");
                    for sem in rendered {
                        unsafe { dev.destroy_semaphore(sem, None) };
                    }
                    return;
                }
            }
        }
        self.rendered = rendered;
    }

    /// Replace an existing cached texture's GPU image with an asynchronously decoded/upscaled one.
    fn replace_texture_image(&mut self, key: TexKey, w: u32, h: u32, px: &[u32], bytes: u64, label: &str, wide_picture: Option<bool>) {
        let Ok(image) = self.upload_image(w, h, px, vk::ImageUsageFlags::SAMPLED, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, label) else {
            return;
        };
        let t = self.textures.get_mut(&key).unwrap();
        let old = std::mem::replace(&mut t.image, image);
        self.texture_bytes = self.texture_bytes.saturating_sub(t.bytes) + bytes;
        t.bytes = bytes;
        if let Some(wide) = wide_picture {
            t.wide_picture = wide;
            self.draw_gen += 1;
        }
        self.retire_image(old);
    }

    /// Swap in completed off-thread pictures, xBRZ-sharpened textures, and HD
    /// pack replacements at the start of a frame, sharing one per-frame upload
    /// budget so large bursts do not stall a frame; anything beyond the budget
    /// waits in `upload_waiting` for the next frame.
    fn apply_uploads(&mut self) {
        if let Some(pack) = &mut self.pack {
            while let Some(l) = pack.next_loaded() {
                self.upload_waiting.push_back(PendingUpload::Hd(l));
            }
        }
        if self.sharpen_queued < 16 {
            if let Some(s) = &mut self.sharpen {
                for d in s.finished().into_iter().rev() {
                    if self.textures.contains_key(&d.key) {
                        self.upload_waiting.push_front(PendingUpload::Sharpened(d));
                        self.sharpen_queued += 1;
                    }
                }
            }
        }
        if let Some(l) = &mut self.picture_loader {
            for d in l.finished().into_iter().rev() {
                if let Some(pic) = d.pic {
                    if self.textures.contains_key(&d.key) {
                        self.upload_waiting.push_front(PendingUpload::Picture(d.key, pic));
                    }
                }
            }
        }
        let mut budget = UPLOAD_BUDGET_PER_FRAME;
        while let Some(item) = self.upload_waiting.front() {
            let stale = match item {
                PendingUpload::Picture(key, _) => !self.textures.contains_key(key),
                PendingUpload::Sharpened(d) => !self.textures.contains_key(&d.key),
                PendingUpload::Hd(_) => false,
            };
            if stale {
                // Evicted or replaced meanwhile: discard without spending budget.
                if matches!(self.upload_waiting.pop_front(), Some(PendingUpload::Sharpened(_))) {
                    self.sharpen_queued = self.sharpen_queued.saturating_sub(1);
                }
                continue;
            }
            let bytes = item.bytes();
            if !take_upload_budget(&mut budget, bytes) {
                break;
            }
            match self.upload_waiting.pop_front().unwrap() {
                PendingUpload::Picture(key, pic) => {
                    self.replace_texture_image(key, pic.w, pic.h, &pic.px, bytes, "picture", Some(pic.wide_picture));
                }
                PendingUpload::Sharpened(d) => {
                    self.sharpen_queued = self.sharpen_queued.saturating_sub(1);
                    self.replace_texture_image(d.key, d.w, d.h, &d.px, bytes, "texture", None);
                }
                PendingUpload::Hd(l) => match self.upload_levels(&l.levels) {
                    Ok(image) => {
                        self.hd_bytes += bytes;
                        if let Some(old) = self.hd_images.insert(l.hash, HdImage { image, last_used: self.submit_no, bytes }) {
                            self.hd_bytes = self.hd_bytes.saturating_sub(old.bytes);
                            self.retire_image(old.image);
                        }
                    }
                    Err(e) => eprintln!("[textures] upload: {e}"),
                },
            }
        }
        if self.texture_bytes > TEXTURE_BUDGET {
            self.evict_lru();
        }
        if self.hd_bytes > HD_BUDGET {
            let mut by_age: Vec<(u64, u64)> = self
                .hd_images
                .iter()
                .filter(|(_, i)| self.submit_no.saturating_sub(i.last_used) >= FRAMES as u64)
                .map(|(&h, i)| (i.last_used, h))
                .collect();
            by_age.sort_unstable();
            for (_, hash) in by_age {
                if self.hd_bytes <= HD_BUDGET / 4 * 3 {
                    break;
                }
                let i = self.hd_images.remove(&hash).unwrap();
                self.hd_bytes = self.hd_bytes.saturating_sub(i.bytes);
                if let Some(p) = &mut self.pack {
                    p.forget(hash);
                }
                self.retire_image(i.image);
            }
        }
    }

    fn evict_textures(&mut self) {
        if self.frame_no % 60 != 0 {
            return;
        }
        let now = self.submit_no;
        let old: Vec<TexKey> = self.textures.iter().filter(|(_, t)| now.saturating_sub(t.last_used) > TEXTURE_TTL).map(|(k, _)| *k).collect();
        for k in old {
            let t = self.textures.remove(&k).unwrap();
            self.texture_bytes -= t.bytes;
            if let Some(pl) = &mut self.picture_loader {
                pl.forget(&k);
            }
            self.retire_image(t.image);
        }
    }

    /// Drop least recently used textures (not used by in-flight submissions)
    /// until the cache is back under half its budget.
    fn evict_lru(&mut self) {
        let mut by_age: Vec<(u64, TexKey)> = self
            .textures
            .iter()
            .filter(|(_, t)| self.submit_no.saturating_sub(t.last_used) >= FRAMES as u64)
            .map(|(k, t)| (t.last_used, *k))
            .collect();
        by_age.sort_by_key(|e| e.0);
        for (_, k) in by_age {
            if self.texture_bytes <= TEXTURE_BUDGET / 2 {
                break;
            }
            let t = self.textures.remove(&k).unwrap();
            self.texture_bytes -= t.bytes;
            if let Some(pl) = &mut self.picture_loader {
                pl.forget(&k);
            }
            self.retire_image(t.image);
        }
    }
}

/// Linear blit between colour images in the given transfer layouts.
fn blit(dev: &ash::Device, cmd: vk::CommandBuffer, (src, src_layout, from): (vk::Image, vk::ImageLayout, [vk::Offset3D; 2]), (dst, dst_layout, to): (vk::Image, vk::ImageLayout, [vk::Offset3D; 2])) {
    let region = vk::ImageBlit { src_subresource: layers(vk::ImageAspectFlags::COLOR), src_offsets: from, dst_subresource: layers(vk::ImageAspectFlags::COLOR), dst_offsets: to };
    unsafe { dev.cmd_blit_image(cmd, src, src_layout, dst, dst_layout, &[region], vk::Filter::LINEAR) };
}

struct Sharpened {
    key: TexKey,
    w: u32,
    h: u32,
    px: Vec<u32>,
}

/// The xBRZ worker thread and its queues.
struct Sharpener {
    jobs: std::sync::mpsc::Sender<(TexKey, u32, u32, u32, Vec<u32>)>,
    done: std::sync::mpsc::Receiver<Sharpened>,
    /// Jobs queued and not yet collected.
    pending: usize,
}

impl Sharpener {
    fn spawn() -> Sharpener {
        let (jobs, rx) = std::sync::mpsc::channel::<(TexKey, u32, u32, u32, Vec<u32>)>();
        let (tx, done) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("xbrz".into())
            .spawn(move || {
                for (key, scale, w, h, px) in rx {
                    let (w, h, px) = sharpen(scale, w, h, px);
                    if tx.send(Sharpened { key, w, h, px }).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn xbrz thread");
        Sharpener { jobs, done, pending: 0 }
    }

    /// Queue a texture worth upscaling. Textures rewritten every frame
    /// would flood the worker, so new work waits while it is busy.
    fn queue(&mut self, key: TexKey, scale: u32, w: u32, h: u32, px: &[u32]) {
        if scale.min(2048 / w.max(h)) < 2 || w * h > 256 * 256 || self.pending >= 16 {
            return;
        }
        if self.jobs.send((key, scale, w, h, px.to_vec())).is_ok() {
            self.pending += 1;
        }
    }

    fn finished(&mut self) -> Vec<Sharpened> {
        let done: Vec<Sharpened> = self.done.try_iter().collect();
        self.pending -= done.len();
        done
    }
}

/// xBRZ-upscale a texture by the internal resolution factor (at most 6, and
/// within 2048 per side). Sampling uses normalized coordinates, so the
/// bigger image drops in for the original unchanged.
fn sharpen(scale: u32, w: u32, h: u32, px: Vec<u32>) -> (u32, u32, Vec<u32>) {
    let f = scale.min(6).min(2048 / w.max(h));
    let rgba: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
    let out = xbrz::scale_rgba(&rgba, w as usize, h as usize, f as usize);
    let px = out.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    (w * f, h * f, px)
}

fn create_pipeline_cache(dev: &ash::Device, path: Option<&std::path::Path>) -> vk::PipelineCache {
    if let Some(p) = path {
        if let Ok(bytes) = std::fs::read(p) {
            if !bytes.is_empty() {
                let info = vk::PipelineCacheCreateInfo::default().initial_data(&bytes);
                match unsafe { dev.create_pipeline_cache(&info, None) } {
                    Ok(c) => return c,
                    Err(e) => eprintln!("[gs] pipeline cache rejected ({e:?}); recreating empty cache"),
                }
            }
        }
    }
    unsafe { dev.create_pipeline_cache(&vk::PipelineCacheCreateInfo::default(), None) }.unwrap_or(vk::PipelineCache::null())
}

struct PictureDone {
    key: TexKey,
    pic: Option<crate::pictures::ReadyPicture>,
}

struct PictureLoader {
    jobs: std::sync::mpsc::Sender<(TexKey, crate::pictures::PictureJob)>,
    done: std::sync::mpsc::Receiver<PictureDone>,
    in_flight: HashSet<TexKey>,
}

impl PictureLoader {
    fn spawn() -> PictureLoader {
        let (jobs, rx) = std::sync::mpsc::channel::<(TexKey, crate::pictures::PictureJob)>();
        let (tx, done) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pictures".into())
            .spawn(move || {
                for (key, job) in rx {
                    let pic = job.build();
                    if tx.send(PictureDone { key, pic }).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn pictures thread");
        PictureLoader { jobs, done, in_flight: HashSet::new() }
    }

    fn queue(&mut self, key: TexKey, job: crate::pictures::PictureJob) {
        if self.in_flight.insert(key) && self.jobs.send((key, job)).is_err() {
            self.in_flight.remove(&key);
        }
    }

    fn finished(&mut self) -> Vec<PictureDone> {
        let done: Vec<PictureDone> = self.done.try_iter().collect();
        for d in &done {
            self.in_flight.remove(&d.key);
        }
        done
    }

    fn forget(&mut self, key: &TexKey) {
        self.in_flight.remove(key);
    }
}

impl Drop for Hw {
    fn drop(&mut self) {
        fn destroy_garbage(vk: &mut Vk, g: Garbage) {
            match g {
                Garbage::Image(i) => vk.destroy_image(i),
                Garbage::Buffer(b) | Garbage::Recorded(b) => vk.destroy_buffer(b),
            }
        }
        let dev = self.vk.device.clone();
        unsafe {
            let _ = dev.device_wait_idle();
        }
        if let Some(t) = &mut self.gpu_timer {
            t.destroy(&dev);
        }
        if self.pipeline_cache != vk::PipelineCache::null() {
            let need_save = self.pipeline_cache_dirty || self.cfg.pipeline_cache.as_ref().is_some_and(|p| !p.exists());
            if need_save {
                self.save_pipeline_cache();
            }
            unsafe { dev.destroy_pipeline_cache(self.pipeline_cache, None) };
        }
        unsafe {
            for (_, p) in self.pipelines.drain() {
                dev.destroy_pipeline(p, None);
            }
            for (_, s) in self.samplers.drain() {
                dev.destroy_sampler(s, None);
            }
            while let Some((_, g)) = self.deferred_garbage.pop_front() {
                destroy_garbage(&mut self.vk, g);
            }
            for mut f in self.frames.drain(..) {
                for g in f.garbage.drain(..) {
                    destroy_garbage(&mut self.vk, g);
                }
                self.vk.destroy_buffer(f.vbuf);
                self.vk.destroy_buffer(f.staging);
                dev.destroy_descriptor_pool(f.pool, None);
                dev.destroy_semaphore(f.acquired, None);
                dev.destroy_fence(f.fence, None);
            }
            for sem in self.rendered.drain(..) {
                dev.destroy_semaphore(sem, None);
            }
            for (_, t) in self.targets.drain() {
                self.vk.destroy_image(t.color);
                self.vk.destroy_image(t.depth);
                if let Some(s) = t.snapshot {
                    self.vk.destroy_image(s);
                }
            }
            for (_, t) in self.textures.drain() {
                self.vk.destroy_image(t.image);
            }
            for (_, i) in self.hd_images.drain() {
                self.vk.destroy_image(i.image);
            }
            for (_, (img, _)) in self.palettes.drain() {
                self.vk.destroy_image(img);
            }
            for img in self.blur.drain(..) {
                self.vk.destroy_image(img);
            }
            if let Some(img) = self.movie.take() {
                self.vk.destroy_image(img);
            }
            if let Some(mut r) = self.record.take() {
                if let Some(img) = r.image.take() {
                    self.vk.destroy_image(img);
                }
            }
            dev.destroy_image_view(self.white.view, None);
            dev.destroy_image(self.white.image, None);
            if let Some(a) = self.white.alloc.take() {
                let _ = self.vk.allocator.free(a);
            }
            for shader in [self.vert, self.frag_simple, self.frag_nodst, self.frag, self.frag_dual_nodst, self.frag_dual] {
                dev.destroy_shader_module(shader, None);
            }
            dev.destroy_pipeline_layout(self.layout, None);
            dev.destroy_descriptor_set_layout(self.set_layout, None);
            dev.destroy_command_pool(self.cmd_pool, None);
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
        self.draw_gen += 1;
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

#[cfg(test)]
mod gear_tests {
    use super::in_gear;

    // Corners as logged from a race and from the start-up screens.
    #[test]
    fn the_gears_quads_shrink() {
        assert!(in_gear([(6.125, 144.6875), (134.125, 144.6875), (6.125, 208.6875)]));
        assert!(in_gear([(134.125, 144.6875), (6.125, 208.6875), (134.125, 208.6875)]));
    }

    #[test]
    fn every_slice_of_the_fill_shrinks() {
        let hub = (70.125, 176.6875);
        assert!(in_gear([hub, (70.125, 208.6875), (67.8125, 208.6875)]));
        assert!(in_gear([hub, (134.125, 208.6875), (134.125, 202.9375)]));
        // the moving one, past each corner of the box
        assert!(in_gear([hub, (70.125, 208.6875), (3.5625, 208.6875)]));
        assert!(in_gear([hub, (70.125, 208.6875), (141.8125, 208.6875)]));
        assert!(in_gear([hub, (6.125, 144.6875), (2.5, 144.6875)]));
        assert!(in_gear([hub, (134.125, 144.6875), (142.625, 144.6875)]));
    }

    #[test]
    fn text_over_the_gears_place_keeps_its_size() {
        for x in [34.5, 66.5, 98.5, 130.5] {
            assert!(!in_gear([(x, 174.0), (x + 32.0, 174.0), (x, 190.0)]));
            assert!(!in_gear([(x + 32.0, 174.0), (x, 190.0), (x + 32.0, 190.0)]));
        }
        assert!(!in_gear([(20.5, 197.5), (52.5, 197.5), (20.5, 213.5)]));
        assert!(!in_gear([(20.5, 150.0625), (220.125, 150.0625), (20.5, 199.9375)]));
    }
}

/// Consume `bytes` from the remaining per-frame upload `budget`.
/// Always permits the first item of a frame (when `*budget == UPLOAD_BUDGET_PER_FRAME`)
/// so a single oversized picture or mipmapped HD texture never starves.
fn take_upload_budget(budget: &mut u64, bytes: u64) -> bool {
    if bytes > *budget && *budget != UPLOAD_BUDGET_PER_FRAME {
        return false;
    }
    *budget = budget.saturating_sub(bytes);
    true
}

#[cfg(test)]
mod upload_budget_tests {
    use super::{take_upload_budget, PendingUpload, Sharpened, TexKey, UPLOAD_BUDGET_PER_FRAME};
    use crate::pictures::ReadyPicture;
    use crate::retexture::{Level, Loaded};

    #[test]
    fn small_uploads_share_frame_budget() {
        let mut budget = UPLOAD_BUDGET_PER_FRAME;
        let chunk = UPLOAD_BUDGET_PER_FRAME / 2;
        assert!(take_upload_budget(&mut budget, chunk));
        assert!(take_upload_budget(&mut budget, chunk));
        assert_eq!(budget, 0);
        assert!(!take_upload_budget(&mut budget, 1));
    }

    #[test]
    fn oversized_first_upload_does_not_starve_and_stops_rest() {
        let mut budget = UPLOAD_BUDGET_PER_FRAME;
        assert!(take_upload_budget(&mut budget, UPLOAD_BUDGET_PER_FRAME * 2));
        assert_eq!(budget, 0);
        assert!(!take_upload_budget(&mut budget, 4096));
    }

    #[test]
    fn oversized_upload_defers_after_partial_spend_then_runs_next_frame() {
        let mut budget = UPLOAD_BUDGET_PER_FRAME;
        assert!(take_upload_budget(&mut budget, 1 << 20));
        assert!(!take_upload_budget(&mut budget, UPLOAD_BUDGET_PER_FRAME));
        let mut next_frame = UPLOAD_BUDGET_PER_FRAME;
        assert!(take_upload_budget(&mut next_frame, UPLOAD_BUDGET_PER_FRAME * 2));
        assert_eq!(next_frame, 0);
    }

    #[test]
    fn pending_upload_bytes_accounts_for_all_variants() {
        let key = TexKey { tex0: 1, texa: 2, clut: 3, gen: 4 };
        let pic = PendingUpload::Picture(key, ReadyPicture { w: 10, h: 20, px: vec![0; 200], wide_picture: false });
        let sharp = PendingUpload::Sharpened(Sharpened { key, w: 8, h: 4, px: vec![0; 32] });
        let hd = PendingUpload::Hd(Loaded {
            hash: 99,
            levels: vec![Level { w: 4, h: 4, px: vec![0; 16] }, Level { w: 2, h: 2, px: vec![0; 4] }],
        });
        assert_eq!(pic.bytes(), 200 * 4);
        assert_eq!(sharp.bytes(), 32 * 4);
        assert_eq!(hd.bytes(), (16 + 4) * 4);
    }
}
