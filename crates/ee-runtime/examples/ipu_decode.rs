//! Decode an "ipum" picture (.LSC) the way Freekstyle drives the IPU and
//! write it as PPM: ipu_decode <file.lsc> <ctrl hex> <out.ppm>
use ee_runtime::ipu::{Ipu, IPU_CMD, IPU_CTRL};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let file = std::fs::read(&a[1]).unwrap();
    let ctrl = u32::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
    let (w, h) = (u16::from_le_bytes([file[8], file[9]]) as usize, u16::from_le_bytes([file[10], file[11]]) as usize);
    let mut ipu = Ipu::default();
    ipu.write(IPU_CTRL, 0x4000_0000);
    ipu.write(IPU_CMD, 0);
    ipu.feed(&file[16..]);
    ipu.write(IPU_CTRL, ctrl as u128);
    ipu.write(IPU_CMD, 0x4000_0008);
    ipu.write(IPU_CMD, 0x1001_0000);
    let out = ipu.take_output(u32::MAX);
    eprintln!("{} macroblocks", out.len() / 64);
    let mut img = vec![0u8; w * h * 3];
    for (mb, q) in out.chunks(64).enumerate() {
        let (mx, my) = ((mb % (w / 16)) * 16, (mb / (w / 16)) * 16);
        for (i, px) in q.iter().flat_map(|q| (0..4).map(move |n| (*q >> (32 * n)) as u32)).enumerate() {
            let (x, y) = (mx + i % 16, my + i / 16);
            if y < h {
                img[(y * w + x) * 3..][..3].copy_from_slice(&px.to_le_bytes()[..3]);
            }
        }
    }
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    ppm.extend_from_slice(&img);
    std::fs::write(&a[3], ppm).unwrap();
}
