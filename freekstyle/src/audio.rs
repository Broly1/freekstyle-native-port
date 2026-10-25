//! Sound output: the emulated IOP's SPU2 samples to the host audio device.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ps2_iop::AudioRing;

/// Samples kept queued ahead of the device: about 16 ms.
pub const LATENCY: usize = 768;
/// Device buffer per callback: 5 ms. The default (often 100 ms periods
/// on ALSA) makes sound trail the picture noticeably.
const PERIOD: u32 = 240;

/// Start playback from `ring`. Returns the stream, which must be kept
/// alive. Without a usable device, samples are drained at the output
/// rate instead, so the IOP still runs in real time.
pub fn start(ring: Arc<AudioRing>, mute: bool) -> Option<cpal::Stream> {
    let opened = if mute { Err("muted".to_string()) } else { open(ring.clone()) };
    match opened {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("[audio] no output: {e}; running silent");
            std::thread::spawn(move || drain(ring));
            None
        }
    }
}

fn open(ring: Arc<AudioRing>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let rate = cpal::SampleRate(ps2_iop::spu2::RATE);
    let supported = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|c| c.channels() == 2 && c.sample_format() == cpal::SampleFormat::F32)
        .find(|c| c.min_sample_rate() <= rate && rate <= c.max_sample_rate())
        .ok_or("no 48 kHz stereo float output")?;
    let period = match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => PERIOD.clamp(*min, *max),
        cpal::SupportedBufferSize::Unknown => PERIOD,
    };
    let mut config: cpal::StreamConfig = supported.with_sample_rate(rate).into();
    config.buffer_size = cpal::BufferSize::Fixed(period);
    let mut last = [0f32; 2];
    let stream = device
        .build_output_stream(
            &config,
            move |out: &mut [f32], _| {
                if std::env::var_os("FREEK_AUDIO_LOG").is_some() {
                    eprintln!("[audio] callback {} frames, queued {}", out.len() / 2, ring.len());
                }
                let mut q = ring.buf.lock().unwrap();
                for frame in out.chunks_exact_mut(2) {
                    // On underrun, hold the last sample rather than click.
                    if let Some([l, r]) = q.pop_front() {
                        last = [l as f32 / 32768.0, r as f32 / 32768.0];
                    }
                    frame.copy_from_slice(&last);
                }
            },
            |e| eprintln!("[audio] {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    eprintln!("[audio] {} at 48 kHz", device.name().unwrap_or_default());
    Ok(stream)
}

fn drain(ring: Arc<AudioRing>) {
    let start = Instant::now();
    let mut taken = 0u64;
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let due = start.elapsed().as_micros() as u64 * ps2_iop::spu2::RATE as u64 / 1_000_000;
        let mut q = ring.buf.lock().unwrap();
        let n = (due - taken).min(q.len() as u64);
        q.drain(..n as usize);
        taken += n;
    }
}
