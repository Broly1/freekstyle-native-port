//! freekstyle-hd: makes the HD extras with Real-ESRGAN.
//!
//!     freekstyle-hd check              is there a GPU the upscaler can use?
//!     freekstyle-hd pictures <images>  HD and 16:9 versions of the pictures
//!     freekstyle-hd pack [--dump D] [--out O] [--skip PACK]...
//!                                      upscale new recorded textures
//!
//! `pack` prints `todo N` first (0: nothing to do, and it ends at once,
//! without starting the upscaler), then `progress DONE N` as it goes.
//!
//! `--stop-on-eof` ends the build, upscaler and all, when stdin closes: the
//! launcher's Skip.
//!
//! `--esrgan <realesrgan-ncnn-vulkan>` picks the upscaler; by default it is
//! the one the installers put in `realesrgan/` beside this program. The
//! launcher runs `pack` when Play is pressed, before it starts the game, on
//! the textures the game recorded (`freek_config::texture_dump`) into the
//! user's pack (`freek_config::user_texture_pack`): the model takes the
//! whole GPU while it runs, and the desktop stutters if it runs beside
//! anything else. Without a GPU (only a software Vulkan driver) nothing is
//! made: on one the model takes minutes per picture.

mod boost;
mod esrgan;
mod pack;
mod pictures;
mod plane;
mod redraw;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

fn fresh(lock: &Path, within: Duration) -> bool {
    std::fs::metadata(lock)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).map_or(true, |age| age < within))
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

/// Only one build at a time: two launchers may be open. The build touches
/// it as it goes; one left alone that long was left by a build that was
/// killed.
const LOCK_STALE: Duration = Duration::from_secs(600);

struct BuildLock(PathBuf);

impl BuildLock {
    fn take() -> Option<BuildLock> {
        let path = freek_config::data_dir().join("hd-building.lock");
        let _ = std::fs::create_dir_all(path.parent()?);
        if path.exists() && !fresh(&path, LOCK_STALE) {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::OpenOptions::new().write(true).create_new(true).open(&path).ok()?;
        Some(BuildLock(path))
    }

    fn touch(&self) {
        let _ = std::fs::File::options().write(true).open(&self.0).and_then(|f| f.set_modified(SystemTime::now()));
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: freekstyle-hd [--esrgan PATH] [--stop-on-eof] check | pictures <images dir> | pack [--dump DIR] [--out DIR] [--skip DIR]...");
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
    if let Some(i) = args.iter().position(|a| a == "--stop-on-eof") {
        args.remove(i);
        // Killing this program alone would leave the upscaler running (and
        // this program's scratch folder behind): closing stdin stops it
        // instead. Nothing is ever sent; any end of the input counts.
        std::thread::spawn(|| {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            esrgan::stop();
        });
    }
    let Some(cmd) = args.first().cloned() else { return usage() };

    // Counting first, so that with nothing new the launcher waits for
    // nothing (the upscaler takes a moment to start).
    let mut pending = None;
    if cmd == "pack" {
        let found = (pack::todo(&dump, &out, &skip), redraw::todo(&dump, &out, &skip));
        let (up, red) = match found {
            (Ok(u), Ok(r)) => (u, r),
            _ => (Vec::new(), Vec::new()), // no recording yet
        };
        println!("todo {}", up.len() + red.len());
        if up.is_empty() && red.is_empty() {
            return ExitCode::SUCCESS;
        }
        pending = Some((up, red));
    }

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
            let Some(lock) = BuildLock::take() else {
                println!("another build is running");
                return ExitCode::SUCCESS;
            };
            let (up, red) = pending.unwrap_or_default();
            let total = up.len() + red.len();
            let report = |done: usize| {
                lock.touch();
                println!("progress {done} {total}");
                let _ = std::io::stdout().flush();
            };
            report(0);
            pack::build(&exe, &up, &out, &tmp.0, &mut |n| report(n)).and_then(|n| {
                let r = redraw::build(&exe, &red, &out, &tmp.0, &mut |k| report(up.len() + k))?;
                println!("{n} textures upscaled, {r} redrawn");
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
