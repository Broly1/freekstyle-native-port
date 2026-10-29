//! Full-screen movies (the intro and logos), played from `.mkv` files.
//!
//! The game plays EA `.FLM` movies through the IPU, which is not emulated.
//! Its movie calls are replaced instead (`hle::movie_open` / `movie_play`):
//! playing streams `game/movies/hd/<name>.mkv` (from `flm2mkv --crt`), or
//! else `game/movies/<name>.mkv`, through ffmpeg. Frames go to the
//! renderer as `gs::Msg::Movie`, the sound to `ps2_iop::MOVIE_AUDIO`, which
//! the audio device mixes in; the frames follow that sound's clock. Start
//! or Cross skips the movie, as on the PS2.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ps2_iop::MOVIE_AUDIO;

use crate::pad::{any_buttons, button};

/// Sound decoded ahead of the device: 0.2 s.
const AUDIO_AHEAD: usize = 9600;
const SKIP: u16 = button::START | button::CROSS;

/// Disc path of the movie opened and not yet played.
pub static OPENED: Mutex<Option<String>> = Mutex::new(None);

/// The `.mkv` for a disc path like `cdrom0:\SXDATA\OPENING\INTRO.FLM;1`.
pub fn find(dir: &Path, disc_path: &str) -> Option<PathBuf> {
    let name = disc_path.rsplit(['\\', '/', ':']).next()?;
    let stem = name.split(['.', ';']).next()?.to_ascii_lowercase();
    [dir.join("hd"), dir.to_path_buf()].into_iter().map(|d| d.join(format!("{stem}.mkv"))).find(|p| p.is_file())
}

/// Width, height and frame rate of the first video stream.
fn probe(path: &Path) -> Option<(u32, u32, f64)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.trim().split(',');
    let (w, h) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    let (n, d) = it.next()?.split_once('/')?;
    Some((w, h, n.parse::<f64>().ok()? / d.parse::<f64>().ok()?))
}

/// ffmpeg decoding `path` to raw data on its stdout. Its complaints are
/// dropped: skipping a movie closes the pipe it writes to.
fn decoder(path: &Path, output: &[&str]) -> Result<Child, String> {
    Command::new("ffmpeg")
        .args(["-nostdin", "-i"])
        .arg(path)
        .args(output)
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))
}

/// Play `path` to the end, or until skipped.
pub fn play(path: &Path, gs: &Sender<gs::Msg>) -> Result<(), String> {
    let (w, h, fps) = probe(path).ok_or_else(|| format!("{}: unreadable (is ffmpeg installed?)", path.display()))?;
    let mut video = decoder(path, &["-an", "-f", "rawvideo", "-pix_fmt", "rgba"])?;
    let mut audio = match decoder(path, &["-vn", "-f", "s16le", "-ac", "2", "-ar", "48000"]) {
        Ok(a) => a,
        Err(e) => {
            let _ = video.kill();
            let _ = video.wait();
            return Err(e);
        }
    };

    // Feed the sound to the device, a little ahead of it.
    MOVIE_AUDIO.buf.lock().unwrap().clear();
    let stop = Arc::new(AtomicBool::new(false));
    let audio_done = Arc::new(AtomicBool::new(false));
    let feeder = {
        let mut out = audio.stdout.take().unwrap();
        let (stop, done) = (stop.clone(), audio_done.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while !stop.load(Relaxed) {
                if MOVIE_AUDIO.buf.lock().unwrap().len() > AUDIO_AHEAD {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                // Whole stereo samples only; a partial one at the very end is dropped.
                if out.read_exact(&mut chunk).is_err() {
                    break;
                }
                let samples = chunk.chunks_exact(4).map(|s| [i16::from_le_bytes([s[0], s[1]]), i16::from_le_bytes([s[2], s[3]])]);
                MOVIE_AUDIO.buf.lock().unwrap().extend(samples);
            }
            done.store(true, Relaxed);
        })
    };

    // The movie's clock is the sound played so far; once the sound has
    // run out (or there is none), the wall clock carries on from there.
    let start = MOVIE_AUDIO.played.load(Relaxed);
    let mut after_sound: Option<(Instant, f64)> = None;
    let mut clock = || {
        let heard = (MOVIE_AUDIO.played.load(Relaxed) - start) as f64 / ps2_iop::spu2::RATE as f64;
        if after_sound.is_none() && audio_done.load(Relaxed) && MOVIE_AUDIO.buf.lock().unwrap().is_empty() {
            after_sound = Some((Instant::now(), heard));
        }
        after_sound.map_or(heard, |(t, s)| s + t.elapsed().as_secs_f64())
    };

    // A button held since before the movie doesn't skip it.
    let mut armed = any_buttons() & SKIP == 0;
    let mut stdout = video.stdout.take().unwrap();
    let mut shown = 0u64;
    'frames: for n in 0u64.. {
        let mut px = vec![0u32; (w * h) as usize];
        let bytes = unsafe { std::slice::from_raw_parts_mut(px.as_mut_ptr() as *mut u8, px.len() * 4) };
        if stdout.read_exact(bytes).is_err() {
            break;
        }
        let due = n as f64 / fps;
        let now = loop {
            let pressed = any_buttons() & SKIP != 0;
            if armed && pressed {
                break 'frames;
            }
            armed |= !pressed;
            let now = clock();
            if now >= due {
                break now;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        // More than a frame late: drop this one to catch up.
        if now > due + 1.0 / fps {
            continue;
        }
        if gs.send(gs::Msg::Movie(Some(Box::new(gs::MovieFrame { w, h, rgba: px })))).is_err() {
            break;
        }
        shown += 1;
    }

    stop.store(true, Relaxed);
    for child in [&mut video, &mut audio] {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = feeder.join();
    MOVIE_AUDIO.buf.lock().unwrap().clear();
    let _ = gs.send(gs::Msg::Movie(None));
    eprintln!("[movie] {} ({shown} frames shown)", path.display());
    Ok(())
}
