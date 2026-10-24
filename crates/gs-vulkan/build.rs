//! Compile the GLSL shaders to SPIR-V with glslc (from shaderc).

use std::{env, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    for name in ["gs.vert", "gs.frag"] {
        let src = format!("shaders/{name}");
        println!("cargo:rerun-if-changed={src}");
        let status = Command::new("glslc")
            .args(["-O", "--target-env=vulkan1.3", &src, "-o"])
            .arg(out.join(format!("{name}.spv")))
            .status()
            .expect("glslc not found: install shaderc");
        assert!(status.success(), "failed to compile {src}");
    }
}
