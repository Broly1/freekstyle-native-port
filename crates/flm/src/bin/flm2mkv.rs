//! Convert Freekstyle's .FLM movies to MKV (VP9 video, Opus audio) for
//! Linux players and for in-game playback.
//!
//! Usage: flm2mkv [--height N] [--crt] <movie.FLM | directory> <out-dir>
//! Needs ffmpeg with libvpx-vp9 and libopus on PATH (and libplacebo with a
//! Vulkan GPU for --crt).
//!
//! Interlaced movies (the intro is 60-field video; the logos are
//! progressive) are deinterlaced to 59.94 frames a second, one per field,
//! so motion stays as smooth as on a TV. `--height` upscales to that many
//! lines at 4:3 with square pixels, e.g. 1080 -> 1440x1080. `--crt`
//! draws that picture as a flat CRT TV would (scanlines, phosphor stripes,
//! a little glow; see crt.glsl) instead of a plain resize, at 1080 lines
//! unless --height says otherwise.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::{env, fs};

/// Field order if most frames of the MPEG-2 stream show combing. The
/// stream headers say progressive either way, so this looks at the
/// pictures (ffmpeg's idet).
fn field_order(m2v: &Path, rate: &str) -> Result<Option<&'static str>, String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-r", rate, "-i"])
        .arg(m2v)
        .args(["-vf", "idet", "-an", "-f", "null", "-"])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let log = String::from_utf8_lossy(&out.stderr);
    let line = log.lines().filter(|l| l.contains("Multi frame detection")).last().unwrap_or("");
    let count = |key: &str| -> u32 {
        line.split(key).nth(1).and_then(|s| s.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0)
    };
    let (tff, bff, prog) = (count("TFF:"), count("BFF:"), count("Progressive:"));
    // Still shots look progressive in interlaced video too, so a quarter
    // of combed frames already means the whole movie is interlaced.
    Ok(if (tff + bff) * 4 < tff + bff + prog {
        None
    } else if tff >= bff {
        Some("tff")
    } else {
        Some("bff")
    })
}

/// The CRT shader, sized for the output.
const CRT_GLSL: &str = include_str!("../crt.glsl");

fn convert(src: &Path, out_dir: &Path, height: Option<u32>, crt: bool) -> Result<PathBuf, String> {
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
    let rate = format!("{num}/{den}");
    let fields = field_order(&m2v, &rate)?;

    let mut filters = Vec::new();
    if let Some(order) = fields {
        filters.push(format!("setfield={order},bwdif=mode=send_field:parity={order}:deint=all"));
    }
    let mut color = vec!["-colorspace", "smpte170m", "-color_primaries", "smpte170m", "-color_trc", "smpte170m"];
    if let Some(h) = height {
        // HD players assume BT.709, so convert the SD colours as well.
        let w = (h * 4 / 3 + 1) & !1;
        if crt {
            let shader = tmp.join("crt.glsl");
            let glsl = CRT_GLSL.replace("//!WIDTH 1440", &format!("//!WIDTH {w}")).replace("//!HEIGHT 1080", &format!("//!HEIGHT {h}"));
            fs::write(&shader, glsl).map_err(|e| e.to_string())?;
            // Passing -init_hw_device makes libplacebo fail to start; it
            // finds the Vulkan GPU by itself.
            filters.push(format!(
                "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m,\
                 libplacebo=w={w}:h={h}:upscaler=nearest:custom_shader_path='{}':colorspace=bt709:color_primaries=bt709:color_trc=bt709,\
                 setsar=1,format=yuv420p",
                shader.display()
            ));
        } else {
            filters.push(format!("scale={w}:{h}:flags=lanczos+accurate_rnd+full_chroma_int:in_color_matrix=bt601:out_color_matrix=bt709,setsar=1"));
        }
        color = vec!["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-r", &rate, "-i"]).arg(&m2v).arg("-i").arg(&wav);
    if !filters.is_empty() {
        cmd.args(["-vf", &filters.join(",")]);
    }
    if height.is_none() {
        // The PS2 shows 512x448 at 4:3; keep the pixels, tag the aspect.
        cmd.args(["-aspect", "4:3"]);
    }
    let status = cmd
        .args(["-c:v", "libvpx-vp9", "-crf", if height.is_some() { "18" } else { "20" }, "-b:v", "0"])
        .args(["-deadline", "good", "-cpu-used", "2", "-row-mt", "1", "-tile-columns", "2", "-pix_fmt", "yuv420p"])
        .args(&color)
        .args(["-c:a", "libopus", "-b:a", "160k", "-shortest"])
        .arg(&out)
        .status()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let _ = fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err(format!("ffmpeg failed for {}", src.display()));
    }
    let secs = movie.audio.len() as f64 / 2.0 / flm::AUDIO_RATE as f64;
    let kind = if fields.is_some() { "interlaced, deinterlaced to 59.94 fps" } else { "progressive" };
    let kind = if crt { format!("{kind}, CRT look") } else { kind.to_string() };
    eprintln!("{} -> {} ({secs:.1} s, {kind})", src.display(), out.display());
    Ok(out)
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut height = None;
    if let Some(i) = args.iter().position(|a| a == "--height") {
        height = args.get(i + 1).and_then(|h| h.parse::<u32>().ok()).filter(|&h| h >= 2);
        if height.is_none() {
            eprintln!("--height needs a number of lines, e.g. 1080");
            exit(2);
        }
        args.drain(i..i + 2);
    }
    let crt = args.iter().any(|a| a == "--crt");
    args.retain(|a| a != "--crt");
    if crt && height.is_none() {
        height = Some(1080);
    }
    if args.len() != 2 {
        eprintln!("usage: flm2mkv [--height N] [--crt] <movie.FLM | directory> <out-dir>");
        exit(2);
    }
    let (src, out) = (Path::new(&args[0]), Path::new(&args[1]));
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
        if let Err(e) = convert(&p, out, height, crt) {
            eprintln!("{e}");
            failed = true;
        }
    }
    exit(i32::from(failed));
}
