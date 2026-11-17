//! Real-ESRGAN (realesrgan-ncnn-vulkan), the AI upscaler: where it is,
//! whether it has a real GPU to run on, and running it on a folder.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Line art: menus, fonts, the HUD and the pictures.
pub const ANIME: &str = "realesrgan-x4plus-anime";
/// Photographs: the 3D surfaces.
pub const PHOTO: &str = "realesrgan-x4plus";

const EXE: &str = if cfg!(windows) { "realesrgan-ncnn-vulkan.exe" } else { "realesrgan-ncnn-vulkan" };

/// The upscaler beside this program, where the installers put it
/// (`realesrgan/` next to `freekstyle-hd`).
pub fn find() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.join("realesrgan");
    let exe = dir.join(EXE);
    exe.is_file().then_some(exe)
}

/// The command for `exe`, without a console window on Windows.
fn command(exe: &Path) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Software Vulkan drivers: Real-ESRGAN runs on them, but takes minutes per
/// picture, so they count as no GPU.
const SOFTWARE: [&str; 4] = ["llvmpipe", "lavapipe", "swiftshader", "microsoft basic render"];

/// Upscale one tiny picture to see that a real GPU runs the model. The
/// error says why not.
pub fn check(exe: &Path, tmp: &Path) -> Result<(), String> {
    let src = tmp.join("check.png");
    let dst = tmp.join("check-out.png");
    image::RgbImage::from_pixel(16, 16, image::Rgb([128, 128, 128])).save(&src).map_err(|e| e.to_string())?;
    let out = command(exe)
        .args(["-i".as_ref(), src.as_os_str(), "-o".as_ref(), dst.as_os_str(), "-n".as_ref(), ANIME.as_ref(), "-s".as_ref(), "4".as_ref()])
        .arg("-m")
        .arg(models(exe))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let log = String::from_utf8_lossy(&out.stderr).to_lowercase();
    // It lists every Vulkan device ("[0 name]  queueC=..."), not just the
    // one it uses, and ncnn picks a real GPU over a software one. Distros
    // install llvmpipe beside the real driver (Ubuntu, Debian and Fedora's
    // mesa-vulkan-drivers), so a software one in the list says nothing:
    // only a list with nothing else in it means no GPU.
    let devices = vulkan_devices(&log);
    let software = |d: &String| SOFTWARE.iter().find(|s| d.contains(*s));
    if !devices.is_empty() && devices.iter().all(|d| software(d).is_some()) {
        return Err(format!("only the {} software renderer", software(&devices[0]).unwrap()));
    }
    if !out.status.success() || !dst.is_file() {
        let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output").trim().to_string();
        return Err(format!("the upscaler failed: {last}"));
    }
    Ok(())
}

/// The Vulkan device names in the upscaler's log, one per device, from
/// its "[N name]  ..." lines.
fn vulkan_devices(log: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in log.lines() {
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some((id, rest)) = rest.split_once(' ') else { continue };
        let Some((name, _)) = rest.split_once(']') else { continue };
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

fn models(exe: &Path) -> PathBuf {
    exe.parent().unwrap_or(Path::new(".")).join("models")
}

/// Set by `stop`: no upscaler starts after it.
static STOPPED: AtomicBool = AtomicBool::new(false);
/// The upscaler running now, for `stop` to end.
static RUNNING: Mutex<Option<Child>> = Mutex::new(None);

/// End the build: the upscaler running now is killed and `run` fails from
/// then on, so the build unwinds and cleans up after itself. Called from
/// another thread.
pub fn stop() {
    STOPPED.store(true, Ordering::SeqCst);
    if let Some(child) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = child.kill();
    }
}

/// Upscale every PNG in `src` 4x into `dst` (same names) with `model`.
pub fn run(exe: &Path, model: &str, src: &Path, dst: &Path) -> Result<(), String> {
    const STOP_MSG: &str = "stopped";
    if STOPPED.load(Ordering::SeqCst) {
        return Err(STOP_MSG.into());
    }
    let mut child = command(exe)
        .args(["-i".as_ref(), src.as_os_str(), "-o".as_ref(), dst.as_os_str(), "-n".as_ref(), model.as_ref(), "-s".as_ref(), "4".as_ref(), "-f".as_ref(), "png".as_ref()])
        .arg("-m")
        .arg(models(exe))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    // Read beside it, or a full pipe would stall it.
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut log = Vec::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_end(&mut log);
        }
        log
    });
    let running = || RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    *running() = Some(child);
    // A stop between the check above and here found nothing to kill.
    if STOPPED.load(Ordering::SeqCst) {
        stop();
    }
    // Polled, not waited on, so `stop` can take the lock and kill it.
    let status = loop {
        let state = running().as_mut().map(|c| c.try_wait());
        match state {
            Some(Ok(Some(status))) => break Ok(status),
            Some(Ok(None)) => std::thread::sleep(Duration::from_millis(50)),
            Some(Err(e)) => break Err(format!("{}: {e}", exe.display())),
            None => unreachable!("only run takes the child out"),
        }
    };
    if let Some(mut c) = running().take() {
        let _ = c.wait();
    }
    let log = reader.join().unwrap_or_default();
    let status = status?;
    if STOPPED.load(Ordering::SeqCst) {
        Err(STOP_MSG.into())
    } else if status.success() {
        Ok(())
    } else {
        let log = String::from_utf8_lossy(&log);
        Err(format!("{model} failed: {}", log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::vulkan_devices;

    #[test]
    fn devices_from_log() {
        let log = "[0 intel(r) arc(tm) b580 graphics (bmg g21)]  queuec=1[1]  queueg=0[1]\n\
                   [0 intel(r) arc(tm) b580 graphics (bmg g21)]  fp16-p/s/a=1/1/1\n\
                   [1 llvmpipe (llvm 21.1.8, 256 bits)]  queuec=0[1]\n\
                   25.00%\n";
        assert_eq!(vulkan_devices(log), ["intel(r) arc(tm) b580 graphics (bmg g21)", "llvmpipe (llvm 21.1.8, 256 bits)"]);
        assert!(vulkan_devices("[ x ]\n0.00%\n").is_empty());
    }
}
