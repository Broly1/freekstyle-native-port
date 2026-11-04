//! FREEK_GPU_TIMES=1: split each frame's GPU time by kind of work.
//!
//! The renderer writes a timestamp wherever one kind of GPU work ends and
//! another begins (drawing into a target, copying a target, uploads, the
//! final blit); each span between two timestamps is booked to the kind
//! that started it. Every pass break is followed by a full barrier, so the
//! spans don't overlap. Totals are printed every 120 frames. A debugging
//! aid: it costs a little GPU time itself.

use ash::vk;
use std::collections::HashMap;

/// Timestamps per frame at most; later marks in a frame are dropped.
const CAP: u32 = 4096;

/// What a span of GPU time was spent on: a kind and, for target work,
/// the target's size in pixels.
pub type Label = (&'static str, u32, u32);

pub struct GpuTimer {
    pools: Vec<vk::QueryPool>,
    /// Per frame slot: the label of the span each timestamp starts.
    labels: Vec<Vec<Label>>,
    period_ns: f64,
    mask: u64,
    /// Nanoseconds and spans per label since the last report.
    totals: HashMap<Label, (f64, u64)>,
    frames: u64,
}

impl GpuTimer {
    /// A timer when FREEK_GPU_TIMES is set and the queue has timestamps.
    pub fn new(instance: &ash::Instance, physical: vk::PhysicalDevice, dev: &ash::Device, family: u32, slots: usize) -> Option<GpuTimer> {
        std::env::var_os("FREEK_GPU_TIMES")?;
        let bits = unsafe { instance.get_physical_device_queue_family_properties(physical) }.get(family as usize)?.timestamp_valid_bits;
        if bits == 0 {
            eprintln!("[gpu] no timestamps on this queue");
            return None;
        }
        let period_ns = unsafe { instance.get_physical_device_properties(physical) }.limits.timestamp_period as f64;
        let info = vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(CAP);
        let pools = (0..slots).map(|_| unsafe { dev.create_query_pool(&info, None) }).collect::<Result<Vec<_>, _>>().ok()?;
        eprintln!("[gpu] timing GPU work ({bits}-bit timestamps, {period_ns} ns per tick)");
        Some(GpuTimer { pools, labels: vec![Vec::new(); slots], period_ns, mask: if bits >= 64 { u64::MAX } else { (1 << bits) - 1 }, totals: HashMap::new(), frames: 0 })
    }

    /// Start a frame slot's command buffer (its previous use has finished):
    /// collect that frame's times and reset its queries.
    pub fn begin(&mut self, dev: &ash::Device, cmd: vk::CommandBuffer, slot: usize) {
        let labels = std::mem::take(&mut self.labels[slot]);
        if labels.len() >= 2 {
            let mut ts = vec![0u64; labels.len()];
            let ok = unsafe { dev.get_query_pool_results(self.pools[slot], 0, &mut ts, vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT) }.is_ok();
            if ok {
                for (i, l) in labels.iter().enumerate().take(labels.len() - 1) {
                    let ticks = ts[i + 1].wrapping_sub(ts[i]) & self.mask;
                    let e = self.totals.entry(*l).or_default();
                    e.0 += ticks as f64 * self.period_ns;
                    e.1 += 1;
                }
                self.frames += 1;
                if self.frames == 120 {
                    self.report();
                }
            }
        }
        self.labels[slot] = labels;
        self.labels[slot].clear();
        unsafe { dev.cmd_reset_query_pool(cmd, self.pools[slot], 0, CAP) };
    }

    /// Book the GPU time from here to the next mark to `label`.
    pub fn mark(&mut self, dev: &ash::Device, cmd: vk::CommandBuffer, slot: usize, label: Label) {
        let n = self.labels[slot].len() as u32;
        if n < CAP {
            unsafe { dev.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, self.pools[slot], n) };
            self.labels[slot].push(label);
        }
    }

    fn report(&mut self) {
        let f = self.frames as f64;
        let mut rows: Vec<_> = self.totals.drain().collect();
        rows.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0));
        let total: f64 = rows.iter().map(|r| r.1 .0).sum();
        let mut line = format!("[gpu] {:.2} ms per frame:", total / f / 1e6);
        for ((kind, w, h), (ns, n)) in rows {
            if ns / f < 20_000.0 {
                continue;
            }
            let size = if w > 0 { format!(" {w}x{h}") } else { String::new() };
            line += &format!(" {kind}{size} {:.2} ms ({:.1}x)", ns / f / 1e6, n as f64 / f);
        }
        eprintln!("{line}");
        self.frames = 0;
    }

    pub fn destroy(&mut self, dev: &ash::Device) {
        for p in self.pools.drain(..) {
            unsafe { dev.destroy_query_pool(p, None) };
        }
    }
}
