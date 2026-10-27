//! Copy a file off the game disc image (raw .bin or .iso), for setting up
//! from your own dump: disc-extract [disc image] [file] [out dir]
//! (defaults: game/Freekstyle.bin SLUS_203.67 game); `--list` as the file
//! prints every file on the disc.
use std::path::Path;

use ee_runtime::disc::Disc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let disc_path = args.get(1).map_or("game/Freekstyle.bin", String::as_str);
    let name = args.get(2).map_or("SLUS_203.67", String::as_str);
    let out = Path::new(args.get(3).map_or("game", String::as_str));
    let mut disc = Disc::open(Path::new(disc_path)).unwrap_or_else(|e| {
        eprintln!("{disc_path}: {e}");
        std::process::exit(1);
    });
    if name == "--list" {
        for (path, e) in disc.walk() {
            println!("{path} {}", e.size);
        }
        return;
    }
    let Some(e) = disc.find(name) else {
        eprintln!("{disc_path}: no {name} on this disc (the port needs the NTSC-U release, SLUS-20367)");
        std::process::exit(1);
    };
    let data = disc.read_file(&e).expect("read file");
    std::fs::create_dir_all(out).expect("create output directory");
    let file = out.join(name.rsplit(['\\', '/']).next().unwrap().split(';').next().unwrap());
    std::fs::write(&file, &data).expect("write file");
    println!("{name} -> {} ({} bytes)", file.display(), data.len());
}
