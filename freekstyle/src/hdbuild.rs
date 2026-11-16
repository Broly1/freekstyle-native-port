//! The texture pack built while playing: the game records the textures it
//! shows (`gs_vulkan::Config::texture_dump`) and, once it quits, starts
//! `freekstyle-hd pack` in the background to upscale the new ones into the
//! user's pack, loaded at the next start. While the game runs it keeps a
//! lock file fresh, and the build stops as soon as it sees one: the GPU is
//! the game's then.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// How often the lock is refreshed; freekstyle-hd treats one older than
/// 30 seconds as left over from a crash.
const REFRESH: Duration = Duration::from_secs(10);

pub struct PlayingLock {
    last: Instant,
}

impl PlayingLock {
    pub fn take() -> PlayingLock {
        let path = freek_config::playing_lock();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, std::process::id().to_string());
        PlayingLock { last: Instant::now() }
    }

    pub fn refresh(&mut self) {
        if self.last.elapsed() >= REFRESH {
            self.last = Instant::now();
            let _ = std::fs::File::options().write(true).open(freek_config::playing_lock()).and_then(|f| f.set_modified(SystemTime::now()));
        }
    }
}

impl Drop for PlayingLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(freek_config::playing_lock());
    }
}

/// freekstyle-hd and Real-ESRGAN beside this program: the installers
/// leave Real-ESRGAN out on a machine without a GPU, and then there is no
/// point recording textures.
pub fn available() -> bool {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return false };
    let (tool, esrgan) = if cfg!(windows) { ("freekstyle-hd.exe", "realesrgan-ncnn-vulkan.exe") } else { ("freekstyle-hd", "realesrgan-ncnn-vulkan") };
    dir.join(tool).is_file() && dir.join("realesrgan").join(esrgan).is_file()
}

/// Start `freekstyle-hd pack`, beside this program, to outlive the game at
/// low priority; its output goes to `hd.log` in the user's data folder.
/// Textures in `game_pack` are left out: that pack already has them.
pub fn start_build(game_pack: &Path) {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return };
    let tool = dir.join(if cfg!(windows) { "freekstyle-hd.exe" } else { "freekstyle-hd" });
    if !tool.is_file() {
        return;
    }
    let log = freek_config::data_dir().join("hd.log");
    let out = std::fs::File::create(&log).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
    let err = std::fs::File::options().append(true).open(&log).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
    let mut cmd = Command::new(&tool);
    cmd.arg("pack").arg("--skip").arg(game_pack).stdin(Stdio::null()).stdout(out).stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own session, so closing the terminal the game ran from doesn't
        // end it, and a low priority.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::nice(10);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        // Detached: no console of its own, and it outlives the game.
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | BELOW_NORMAL_PRIORITY_CLASS);
    }
    match cmd.spawn() {
        Ok(_) => eprintln!("[textures] upscaling new textures in the background ({})", log.display()),
        Err(e) => eprintln!("[textures] {}: {e}", tool.display()),
    }
}
