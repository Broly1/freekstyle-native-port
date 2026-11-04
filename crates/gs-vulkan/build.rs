//! Compile the GLSL shaders to SPIR-V with glslc (from shaderc).

use std::{env, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    // gs.frag twice: with the second (dual-source) blend output, and
    // without it for the pipelines that don't blend with it.
    for (name, src, defs) in [("gs.vert", "gs.vert", &[][..]), ("gs.frag", "gs.frag", &[][..]), ("gs_dual.frag", "gs.frag", &["-DDUAL_SOURCE"][..])] {
        let src = format!("shaders/{src}");
        println!("cargo:rerun-if-changed={src}");
        let status = Command::new("glslc")
            .args(["-O", "--target-env=vulkan1.3"])
            .args(defs)
            .args([&src, "-o"])
            .arg(out.join(format!("{name}.spv")))
            .status()
            .expect("glslc not found: install shaderc");
        assert!(status.success(), "failed to compile {src}");
    }
}
