//! Convert the game's .LSC pictures (title, menu and loading screens) to
//! PNG, so the port loads them instead of decoding MPEG with an emulated
//! IPU: lsc2png [disc image] [out dir]   (defaults: game/Freekstyle.bin game/images)
use std::fmt::Write as _;
use std::path::Path;

use ee_runtime::{disc::Disc, images, ipu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let disc_path = args.get(1).map_or("game/Freekstyle.bin", String::as_str);
    let out = Path::new(args.get(2).map_or("game/images", String::as_str));
    let mut disc = Disc::open(Path::new(disc_path)).unwrap_or_else(|e| {
        eprintln!("{disc_path}: {e}");
        std::process::exit(1);
    });
    std::fs::create_dir_all(out).expect("create output directory");
    let mut index = String::new();
    let mut n = 0;
    for (path, e) in disc.walk() {
        if !path.ends_with(".LSC") {
            continue;
        }
        let data = disc.read_file(&e).expect("read file");
        let name = path.rsplit('\\').next().unwrap().trim_end_matches(".LSC");
        let Some((w, h, px)) = ipu::decode_picture(&data) else {
            eprintln!("{path}: not an ipum picture");
            continue;
        };
        let file = format!("{name}.png");
        images::save_png(&out.join(&file), w, h, &px).expect("write PNG");
        let _ = writeln!(index, "{:016x} {file}", images::content_hash(&data));
        println!("{path} -> {} ({w}x{h})", out.join(&file).display());
        n += 1;
    }
    std::fs::write(out.join("index.txt"), index).expect("write index");
    println!("{n} pictures");
}
