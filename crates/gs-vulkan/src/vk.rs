//! Vulkan setup: instance, device, swapchain, memory, one-off helpers.

use std::ffi::CStr;
use std::mem::ManuallyDrop;

use ash::vk;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc};
use gpu_allocator::MemoryLocation;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub type Result<T> = std::result::Result<T, String>;

fn err<E: std::fmt::Debug>(what: &'static str) -> impl Fn(E) -> String {
    move |e| format!("{what}: {e:?}")
}

pub struct Image {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub alloc: Option<Allocation>,
    pub extent: vk::Extent2D,
}

pub struct Buffer {
    pub buffer: vk::Buffer,
    pub alloc: Option<Allocation>,
    pub size: u64,
}

impl Buffer {
    pub fn mapped(&mut self) -> &mut [u8] {
        self.alloc.as_mut().unwrap().mapped_slice_mut().expect("buffer not host visible")
    }
}

pub struct Swapchain {
    pub loader: ash::khr::swapchain::Device,
    pub swapchain: vk::SwapchainKHR,
    pub images: Vec<vk::Image>,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
}

pub struct Vk {
    /// Keeps the Vulkan library loaded for the lifetime of the device.
    _entry: ash::Entry,
    pub instance: ash::Instance,
    pub surface_loader: ash::khr::surface::Instance,
    pub surface: vk::SurfaceKHR,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub allocator: ManuallyDrop<Allocator>,
    pub swapchain: Swapchain,
    pub device_name: String,
}

impl Vk {
    pub fn new(window: &(impl HasDisplayHandle + HasWindowHandle), size: (u32, u32)) -> Result<Vk> {
        let display = window.display_handle().map_err(err("display handle"))?.as_raw();
        let win = window.window_handle().map_err(err("window handle"))?.as_raw();
        // SAFETY: loading the system Vulkan library.
        let entry = unsafe { ash::Entry::load() }.map_err(err("load Vulkan"))?;
        let app = vk::ApplicationInfo::default()
            .application_name(c"Freekstyle")
            .engine_name(c"freekstyle-gs")
            .api_version(vk::API_VERSION_1_3);
        let exts = ash_window::enumerate_required_extensions(display).map_err(err("surface extensions"))?;
        let instance = unsafe {
            entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(exts), None)
        }
        .map_err(err("create instance"))?;
        let surface = unsafe { ash_window::create_surface(&entry, &instance, display, win, None) }.map_err(err("create surface"))?;
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);

        // Prefer a discrete GPU with a queue that can draw and present.
        let mut best = None;
        for pd in unsafe { instance.enumerate_physical_devices() }.map_err(err("enumerate devices"))? {
            let props = unsafe { instance.get_physical_device_properties(pd) };
            if props.api_version < vk::API_VERSION_1_3 {
                continue;
            }
            let families = unsafe { instance.get_physical_device_queue_family_properties(pd) };
            let family = families.iter().enumerate().position(|(i, f)| {
                f.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                    && unsafe { surface_loader.get_physical_device_surface_support(pd, i as u32, surface) }.unwrap_or(false)
            });
            if let Some(f) = family {
                let score = if props.device_type == vk::PhysicalDeviceType::DISCRETE_GPU { 2 } else { 1 };
                if best.as_ref().map(|(s, _, _, _)| score > *s).unwrap_or(true) {
                    let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }.to_string_lossy().into_owned();
                    best = Some((score, pd, f as u32, name));
                }
            }
        }
        let (_, physical, queue_family, device_name) = best.ok_or("no Vulkan 1.3 GPU that can present to this window")?;

        let mut v13 = vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true).synchronization2(true);
        let prio = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family).queue_priorities(&prio)];
        let dev_exts = [ash::khr::swapchain::NAME.as_ptr()];
        let device = unsafe {
            instance.create_device(
                physical,
                // Dual-source blending: the blend factor (alpha / 0x80) and
                // the stored alpha byte differ.
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&dev_exts)
                    .enabled_features(&vk::PhysicalDeviceFeatures::default().dual_src_blend(true))
                    .push_next(&mut v13),
                None,
            )
        }
        .map_err(err("create device"))?;
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: physical,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })
        .map_err(err("create allocator"))?;

        let loader = ash::khr::swapchain::Device::new(&instance, &device);
        let swapchain = Swapchain { loader, swapchain: vk::SwapchainKHR::null(), images: Vec::new(), extent: vk::Extent2D::default(), format: vk::Format::UNDEFINED };
        let mut vk = Vk {
            _entry: entry,
            instance,
            surface_loader,
            surface,
            physical,
            device,
            queue,
            queue_family,
            allocator: ManuallyDrop::new(allocator),
            swapchain,
            device_name,
        };
        vk.recreate_swapchain(size)?;
        Ok(vk)
    }

    pub fn recreate_swapchain(&mut self, size: (u32, u32)) -> Result<()> {
        unsafe {
            let _ = self.device.device_wait_idle();
            let caps = self.surface_loader.get_physical_device_surface_capabilities(self.physical, self.surface).map_err(err("surface caps"))?;
            let formats = self.surface_loader.get_physical_device_surface_formats(self.physical, self.surface).map_err(err("surface formats"))?;
            let modes = self.surface_loader.get_physical_device_surface_present_modes(self.physical, self.surface).map_err(err("present modes"))?;
            // UNORM keeps the PS2's already gamma-encoded output untouched.
            let format = formats
                .iter()
                .find(|f| f.format == vk::Format::B8G8R8A8_UNORM || f.format == vk::Format::R8G8B8A8_UNORM)
                .copied()
                .unwrap_or(formats[0]);
            let mode = if modes.contains(&vk::PresentModeKHR::MAILBOX) { vk::PresentModeKHR::MAILBOX } else { vk::PresentModeKHR::FIFO };
            let extent = if caps.current_extent.width != u32::MAX {
                caps.current_extent
            } else {
                vk::Extent2D {
                    width: size.0.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                    height: size.1.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
                }
            };
            let count = (caps.min_image_count + 1).min(if caps.max_image_count == 0 { u32::MAX } else { caps.max_image_count });
            let old = self.swapchain.swapchain;
            let info = vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(count)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(mode)
                .clipped(true)
                .old_swapchain(old);
            let sc = self.swapchain.loader.create_swapchain(&info, None).map_err(err("create swapchain"))?;
            if old != vk::SwapchainKHR::null() {
                self.swapchain.loader.destroy_swapchain(old, None);
            }
            self.swapchain.swapchain = sc;
            self.swapchain.images = self.swapchain.loader.get_swapchain_images(sc).map_err(err("swapchain images"))?;
            self.swapchain.extent = extent;
            self.swapchain.format = format.format;
        }
        Ok(())
    }

    pub fn create_image(&mut self, w: u32, h: u32, format: vk::Format, usage: vk::ImageUsageFlags, name: &str) -> Result<Image> {
        let depth = format == vk::Format::D32_SFLOAT;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: w.max(1), height: h.max(1), depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        unsafe {
            let image = self.device.create_image(&info, None).map_err(err("create image"))?;
            let req = self.device.get_image_memory_requirements(image);
            let alloc = self
                .allocator
                .allocate(&AllocationCreateDesc { name, requirements: req, location: MemoryLocation::GpuOnly, linear: false, allocation_scheme: AllocationScheme::GpuAllocatorManaged })
                .map_err(err("allocate image"))?;
            self.device.bind_image_memory(image, alloc.memory(), alloc.offset()).map_err(err("bind image"))?;
            let aspect = if depth { vk::ImageAspectFlags::DEPTH } else { vk::ImageAspectFlags::COLOR };
            let view = self
                .device
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format)
                        .subresource_range(range(aspect)),
                    None,
                )
                .map_err(err("create view"))?;
            Ok(Image { image, view, alloc: Some(alloc), extent: vk::Extent2D { width: w.max(1), height: h.max(1) } })
        }
    }

    pub fn create_buffer(&mut self, size: u64, usage: vk::BufferUsageFlags, name: &str) -> Result<Buffer> {
        unsafe {
            let buffer = self.device.create_buffer(&vk::BufferCreateInfo::default().size(size).usage(usage), None).map_err(err("create buffer"))?;
            let req = self.device.get_buffer_memory_requirements(buffer);
            let alloc = self
                .allocator
                .allocate(&AllocationCreateDesc {
                    name,
                    requirements: req,
                    // Readback buffers need cached memory: reading the
                    // write-combined upload kind is ~100x slower.
                    location: if usage == vk::BufferUsageFlags::TRANSFER_DST { MemoryLocation::GpuToCpu } else { MemoryLocation::CpuToGpu },
                    linear: true,
                    allocation_scheme: AllocationScheme::GpuAllocatorManaged,
                })
                .map_err(err("allocate buffer"))?;
            self.device.bind_buffer_memory(buffer, alloc.memory(), alloc.offset()).map_err(err("bind buffer"))?;
            Ok(Buffer { buffer, alloc: Some(alloc), size })
        }
    }

    pub fn destroy_image(&mut self, mut i: Image) {
        unsafe {
            self.device.destroy_image_view(i.view, None);
            self.device.destroy_image(i.image, None);
        }
        if let Some(a) = i.alloc.take() {
            let _ = self.allocator.free(a);
        }
    }

    pub fn destroy_buffer(&mut self, mut b: Buffer) {
        unsafe { self.device.destroy_buffer(b.buffer, None) };
        if let Some(a) = b.alloc.take() {
            let _ = self.allocator.free(a);
        }
    }
}

pub fn range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange { aspect_mask: aspect, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 }
}

pub fn layers(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers { aspect_mask: aspect, mip_level: 0, base_array_layer: 0, layer_count: 1 }
}

/// Full execution and memory barrier. The renderer's passes are simple
/// enough that finer-grained barriers are not worth their complexity yet.
pub fn barrier(dev: &ash::Device, cmd: vk::CommandBuffer) {
    let mb = [vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)];
    unsafe { dev.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().memory_barriers(&mb)) };
}

/// Layout transition with a full barrier.
pub fn transition(dev: &ash::Device, cmd: vk::CommandBuffer, image: vk::Image, aspect: vk::ImageAspectFlags, from: vk::ImageLayout, to: vk::ImageLayout) {
    let ib = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
        .old_layout(from)
        .new_layout(to)
        .image(image)
        .subresource_range(range(aspect))];
    unsafe { dev.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&ib)) };
}

impl Drop for Vk {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.swapchain.loader.destroy_swapchain(self.swapchain.swapchain, None);
            ManuallyDrop::drop(&mut self.allocator);
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}
