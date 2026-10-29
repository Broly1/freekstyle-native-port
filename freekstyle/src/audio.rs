//! Sound output: the emulated IOP's SPU2 samples to the host audio device,
//! with a playing movie's sound mixed in.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use ps2_iop::{AudioRing, MOVIE_AUDIO};

/// Samples kept queued ahead of the device: about 32 ms.
pub const LATENCY: usize = 1536;
/// Device buffer per callback: 10 ms. The default (often 100 ms periods
/// on ALSA) makes sound trail the picture noticeably; shorter ones (5 ms
/// asked, 1.3 ms granted through PipeWire's ALSA plugin) crackled and
/// ended in POLLERR.
const PERIOD: u32 = 480;

/// Play `ring` on the default output device from an audio thread. Without
/// a usable device (or muted) samples are drained at the output rate
/// instead, so the IOP still runs in real time. A stream that reports an
/// error or stops asking for samples is closed, the game goes on silent,
/// and the device is reopened every few seconds until it plays again.
pub fn start(ring: Arc<AudioRing>, mute: bool) {
    let _ = std::thread::Builder::new().name("audio".into()).spawn(move || supervise(ring, mute));
}

/// Callbacks stopping for this long count as a dead stream.
const STALL: Duration = Duration::from_millis(500);
/// How long to stay silent before trying the device again.
const RETRY: Duration = Duration::from_secs(2);

/// What the stream's callbacks report to the audio thread.
struct Health {
    start: Instant,
    /// Milliseconds since `start` at the last data callback.
    last_callback: AtomicU64,
    failed: AtomicBool,
}

impl Health {
    fn alive(&self) -> bool {
        let quiet = self.start.elapsed().saturating_sub(Duration::from_millis(self.last_callback.load(Relaxed)));
        !self.failed.load(Relaxed) && quiet < STALL
    }
}

fn supervise(ring: Arc<AudioRing>, mute: bool) {
    if mute {
        eprintln!("[audio] muted; running silent");
    }
    let mut silent = Silent::new();
    let mut retry_at = Instant::now();
    let mut reported = false;
    loop {
        if !mute && Instant::now() >= retry_at {
            let health = Arc::new(Health { start: Instant::now(), last_callback: AtomicU64::new(0), failed: AtomicBool::new(false) });
            match open(ring.clone(), health.clone()) {
                Ok(stream) => {
                    reported = false;
                    while health.alive() {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    drop(stream);
                    eprintln!("[audio] output stopped; running silent and retrying");
                }
                Err(e) if !reported => {
                    eprintln!("[audio] no output: {e}; running silent and retrying");
                    reported = true;
                }
                Err(_) => {}
            }
            retry_at = Instant::now() + RETRY;
            silent = Silent::new();
        }
        silent.step(&ring);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn open(ring: Arc<AudioRing>, health: Arc<Health>) -> Result<cpal::Stream, String> {
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
    // FREEK_HEARTBEAT: report missing samples (heard as crackle) each second.
    let report = std::env::var_os("FREEK_HEARTBEAT").is_some();
    let (mut missing, mut since) = (0usize, Instant::now());
    let (mut last_call, mut worst_gap) = (Instant::now(), Duration::ZERO);
    let health_err = health.clone();
    let stream = device
        .build_output_stream(
            &config,
            move |out: &mut [f32], _| {
                health.last_callback.store(health.start.elapsed().as_millis() as u64, Relaxed);
                if std::env::var_os("FREEK_AUDIO_LOG").is_some() {
                    eprintln!("[audio] callback {} frames, queued {}", out.len() / 2, ring.len());
                }
                let mut q = ring.buf.lock().unwrap();
                let mut movie = MOVIE_AUDIO.buf.lock().unwrap();
                let mut played = 0;
                for frame in out.chunks_exact_mut(2) {
                    // On underrun, hold the last sample rather than click.
                    if let Some([l, r]) = q.pop_front() {
                        last = [l as f32 / 32768.0, r as f32 / 32768.0];
                    } else {
                        missing += 1;
                    }
                    frame.copy_from_slice(&last);
                    if let Some([l, r]) = movie.pop_front() {
                        frame[0] = (frame[0] + l as f32 / 32768.0).clamp(-1.0, 1.0);
                        frame[1] = (frame[1] + r as f32 / 32768.0).clamp(-1.0, 1.0);
                        played += 1;
                    }
                }
                MOVIE_AUDIO.played.fetch_add(played, Relaxed);
                if report {
                    worst_gap = worst_gap.max(last_call.elapsed());
                    last_call = Instant::now();
                    if since.elapsed() >= Duration::from_secs(1) {
                        // A long gap between callbacks means the device
                        // itself ran dry (heard as a click), not our queue.
                        eprintln!("[audio] last second: {missing} samples missing, longest callback gap {:.1} ms, {} frames per callback", worst_gap.as_secs_f64() * 1e3, out.len() / 2);
                        (missing, since, worst_gap) = (0, Instant::now(), Duration::ZERO);
                    }
                }
            },
            {
                // One report per stream: a broken device can return the
                // same error on every poll.
                let health = health_err;
                move |e| {
                    if !health.failed.swap(true, Relaxed) {
                        eprintln!("[audio] {e}");
                    }
                }
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    eprintln!("[audio] {} at 48 kHz", device.name().unwrap_or_default());
    Ok(stream)
}

/// Silent output: takes samples at the output rate, so the IOP and movies
/// keep real time.
struct Silent {
    start: Instant,
    taken: u64,
}

impl Silent {
    fn new() -> Silent {
        Silent { start: Instant::now(), taken: 0 }
    }

    fn step(&mut self, ring: &AudioRing) {
        let due = self.start.elapsed().as_micros() as u64 * ps2_iop::spu2::RATE as u64 / 1_000_000;
        let mut q = ring.buf.lock().unwrap();
        let n = (due - self.taken).min(q.len() as u64);
        q.drain(..n as usize);
        self.taken += n;
        // A movie's sound keeps its clock running at the same rate.
        let mut movie = MOVIE_AUDIO.buf.lock().unwrap();
        let m = n.min(movie.len() as u64);
        movie.drain(..m as usize);
        MOVIE_AUDIO.played.fetch_add(m, Relaxed);
    }
}
