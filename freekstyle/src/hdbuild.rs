//! The texture pack built while playing: the game records the textures it
//! shows (`gs_vulkan::Config::texture_dump`), and the launcher runs
//! `freekstyle-hd pack` on them when Play is next pressed, before it starts
//! the game, so the upscaler never shares the GPU with anything.

use std::path::Path;

/// freekstyle-hd and Real-ESRGAN beside this program: the installers
/// leave Real-ESRGAN out on a machine without a GPU, and then there is no
/// point recording textures.
pub fn available() -> bool {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return false };
    let (tool, esrgan) = if cfg!(windows) { ("freekstyle-hd.exe", "realesrgan-ncnn-vulkan.exe") } else { ("freekstyle-hd", "realesrgan-ncnn-vulkan") };
    dir.join(tool).is_file() && dir.join("realesrgan").join(esrgan).is_file()
}
