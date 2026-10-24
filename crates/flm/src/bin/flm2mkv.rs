//! Convert Freekstyle's .FLM movies to MKV (VP9 video, Opus audio) for
//! Linux players and for in-game playback.
//!
//! Usage: flm2mkv <movie.FLM | directory> <out-dir>
//! Needs ffmpeg with libvpx-vp9 and libopus on PATH.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::{env, fs};

fn convert(src: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let data = fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let movie = flm::demux(&data).map_err(|e| format!("{}: {e}", src.display()))?;
    let stem = src.file_stem().unwrap().to_string_lossy().to_ascii_lowercase();
    let tmp = env::temp_dir().join(format!("flm2mkv-{}-{stem}", std::process::id()));
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let (m2v, wav) = (tmp.join("video.m2v"), tmp.join("audio.wav"));
    fs::write(&m2v, &movie.video).map_err(|e| e.to_string())?;
    fs::write(&wav, flm::wav(&movie.audio, flm::AUDIO_RATE)).map_err(|e| e.to_string())?;
    let out = out_dir.join(format!("{stem}.mkv"));
    let (num, den) = flm::VIDEO_FPS;
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-r", &format!("{num}/{den}"), "-i"])
        .arg(&m2v)
        .arg("-i")
        .arg(&wav)
        // The PS2 shows 512x448 at 4:3; keep the pixels, tag the aspect.
        .args(["-c:v", "libvpx-vp9", "-crf", "20", "-b:v", "0", "-row-mt", "1", "-aspect", "4:3"])
        .args(["-c:a", "libopus", "-b:a", "128k", "-shortest"])
        .arg(&out)
        .status()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let _ = fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err(format!("ffmpeg failed for {}", src.display()));
    }
    let secs = movie.audio.len() as f64 / 2.0 / flm::AUDIO_RATE as f64;
    eprintln!("{} -> {} ({secs:.1} s)", src.display(), out.display());
    Ok(out)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: flm2mkv <movie.FLM | directory> <out-dir>");
        exit(2);
    }
    let (src, out) = (Path::new(&args[1]), Path::new(&args[2]));
    if let Err(e) = fs::create_dir_all(out) {
        eprintln!("{}: {e}", out.display());
        exit(1);
    }
    let inputs: Vec<PathBuf> = if src.is_dir() {
        let mut v: Vec<PathBuf> = fs::read_dir(src)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("flm")))
            .collect();
        v.sort();
        v
    } else {
        vec![src.to_path_buf()]
    };
    let mut failed = false;
    for p in inputs {
        if let Err(e) = convert(&p, out) {
            eprintln!("{e}");
            failed = true;
        }
    }
    exit(i32::from(failed));
}
