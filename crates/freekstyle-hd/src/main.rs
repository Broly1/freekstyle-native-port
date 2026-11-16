//! freekstyle-hd: makes the HD extras with Real-ESRGAN.
//!
//!     freekstyle-hd check              is there a GPU the upscaler can use?
//!     freekstyle-hd pictures <images>  HD and 16:9 versions of the pictures
//!     freekstyle-hd pack [--dump D] [--out O] [--skip PACK]...
//!                                      upscale new recorded textures
//!
//! `--esrgan <realesrgan-ncnn-vulkan>` picks the upscaler; by default it is
//! the one the installers put in `realesrgan/` beside this program. The
//! game runs `pack` by itself after it quits, on the textures it recorded
//! (`freek_config::texture_dump`) into the user's pack
//! (`freek_config::user_texture_pack`); it stops as soon as the game runs
//! again and carries on next time. Without a GPU (only a software Vulkan
//! driver) nothing is made: on one the model takes minutes per picture.

mod esrgan;
mod pack;
mod pictures;
mod plane;
mod redraw;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

/// The game refreshes its lock this often (freekstyle/src/main.rs); an
/// older one is left over from a game that crashed.
const LOCK_FRESH: Duration = Duration::from_secs(30);

fn fresh(lock: &Path, within: Duration) -> bool {
    std::fs::metadata(lock)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).map_or(true, |age| age < within))
}

fn playing() -> bool {
    fresh(&freek_config::playing_lock(), LOCK_FRESH)
}

/// A scratch folder, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch, String> {
        let dir = std::env::temp_dir().join(format!("freekstyle-hd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Only one build at a time: the game may quit twice in a row.
struct BuildLock(PathBuf);

impl BuildLock {
    fn take() -> Option<BuildLock> {
        let path = freek_config::data_dir().join("hd-building.lock");
        let _ = std::fs::create_dir_all(path.parent()?);
        // One left by a build that was killed doesn't count for long.
        if path.exists() && !fresh(&path, Duration::from_secs(6 * 3600)) {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::OpenOptions::new().write(true).create_new(true).open(&path).ok()?;
        Some(BuildLock(path))
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: freekstyle-hd [--esrgan PATH] check | pictures <images dir> | pack [--dump DIR] [--out DIR] [--skip DIR]...");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| -> Vec<String> {
        let mut out = Vec::new();
        while let Some(i) = args.iter().position(|a| a == flag) {
            if i + 1 < args.len() {
                out.push(args.remove(i + 1));
            }
            args.remove(i);
        }
        out
    };
    let esrgan_arg = take("--esrgan").pop().map(PathBuf::from);
    let dump = take("--dump").pop().map(PathBuf::from).unwrap_or_else(freek_config::texture_dump);
    let out = take("--out").pop().map(PathBuf::from).unwrap_or_else(freek_config::user_texture_pack);
    let skip: Vec<PathBuf> = take("--skip").into_iter().map(PathBuf::from).collect();
    let Some(cmd) = args.first().cloned() else { return usage() };

    let Some(exe) = esrgan_arg.or_else(esrgan::find) else {
        eprintln!("freekstyle-hd: Real-ESRGAN not found (realesrgan/ beside this program); nothing to do");
        return ExitCode::from(3);
    };
    let tmp = match Scratch::new() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("freekstyle-hd: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = esrgan::check(&exe, &tmp.0) {
        eprintln!("freekstyle-hd: {e}; nothing to do");
        return ExitCode::from(3);
    }
    let result = match cmd.as_str() {
        "check" => {
            println!("ok");
            Ok(())
        }
        "pictures" => {
            let Some(images) = args.get(1) else { return usage() };
            pictures::build(&exe, Path::new(images), &tmp.0).map(|(hd, wide)| println!("{hd} HD pictures, {wide} 16:9 pictures"))
        }
        "pack" => {
            let Some(_lock) = BuildLock::take() else {
                println!("another build is running");
                return ExitCode::SUCCESS;
            };
            if !dump.is_dir() {
                println!("no recorded textures yet");
                return ExitCode::SUCCESS;
            }
            pack::build(&exe, &dump, &out, &skip, &tmp.0, &playing).and_then(|n| {
                let r = if playing() { 0 } else { redraw::build(&exe, &dump, &out, &skip, &tmp.0)? };
                println!("{n} textures upscaled, {r} redrawn{}", if playing() { " (stopped: the game is running)" } else { "" });
                Ok(())
            })
        }
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("freekstyle-hd: {e}");
            ExitCode::FAILURE
        }
    }
}
