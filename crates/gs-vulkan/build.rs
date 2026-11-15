//! Compile the GLSL shaders to SPIR-V with naga (pure Rust, no external glslc needed).

use std::{env, fs, path::PathBuf};

use naga::back::spv;
use naga::front::glsl::{Frontend, Options as GlslOptions};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::ShaderStage;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut frontend = Frontend::default();

    // Note: KEEP WriterFlags::ADJUST_COORDINATE_SPACE disabled so Vulkan clip-space
    // coordinates match glslc output (gs.vert outputs window-space Y directly).
    let spv_options = spv::Options {
        lang_version: (1, 3),
        flags: spv::WriterFlags::LABEL_VARYINGS | spv::WriterFlags::CLAMP_FRAG_DEPTH,
        ..spv::Options::default()
    };

    // gs.frag twice: with the second (dual-source) blend output, and
    // without it for the pipelines that don't blend with it.
    let targets = [
        ("gs.vert", "gs.vert", ShaderStage::Vertex, &[][..]),
        ("gs.frag", "gs.frag", ShaderStage::Fragment, &[][..]),
        ("gs_dual.frag", "gs.frag", ShaderStage::Fragment, &[("DUAL_SOURCE", "1")][..]),
    ];

    for (name, src, stage, defs) in targets {
        let src_path = format!("shaders/{src}");
        println!("cargo:rerun-if-changed={src_path}");
        let source = fs::read_to_string(&src_path).expect("read GLSL shader");

        let mut glsl_opts = GlslOptions::from(stage);
        for &(k, v) in defs {
            glsl_opts.defines.insert(k.to_string(), v.to_string());
        }

        let module = frontend
            .parse(&glsl_opts, &source)
            .unwrap_or_else(|e| panic!("failed to parse {src_path}: {e}"));

        let info = Validator::new(ValidationFlags::all(), Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("failed to validate {src_path}: {e:?}"));

        let words = spv::write_vec(&module, &info, &spv_options, None)
            .unwrap_or_else(|e| panic!("failed to emit SPIR-V for {src_path}: {e}"));

        let mut bytes = Vec::with_capacity(words.len() * 4);
        for w in words {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        fs::write(out.join(format!("{name}.spv")), bytes).expect("write SPIR-V output");
    }
}
