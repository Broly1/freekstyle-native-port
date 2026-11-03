//! FREEK_GS_RECORD=file: save everything the EE sends the renderer.
//! FREEK_GS_REPLAY=file: feed a saved stream to the renderer instead of
//! running the game. A replay draws the same frames on every run (the game
//! itself doesn't: its timing varies), so screenshots (FREEK_SHOT) from two
//! builds can be compared pixel for pixel. Movies aren't recorded. A
//! debugging aid; recordings grow by up to ~100 MB per second of racing.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::mpsc::{sync_channel, Receiver};

const GIF: u8 = 0;
const PRIV: u8 = 1;
const VSYNC: u8 = 2;
// Both directions use a bounded channel: a replay would otherwise read the
// whole file into memory, and a recording slower than the game would grow
// without limit (it holds the game back instead).

/// Pass messages through, writing them to `path` on the way.
pub fn record(rx: Receiver<gs::Msg>, path: String) -> Receiver<gs::Msg> {
    let (tx, out) = sync_channel(4096);
    std::thread::spawn(move || {
        let mut f = BufWriter::with_capacity(1 << 20, File::create(&path).unwrap_or_else(|e| panic!("{path}: {e}")));
        eprintln!("[gsrec] recording to {path}");
        while let Ok(msg) = rx.recv() {
            let _ = match &msg {
                gs::Msg::Gif(d) => f.write_all(&[GIF]).and(f.write_all(&(d.len() as u32).to_le_bytes())).and(f.write_all(unsafe { std::slice::from_raw_parts(d.as_ptr() as *const u8, d.len() * 16) })),
                gs::Msg::Priv(r, v) => f.write_all(&[PRIV]).and(f.write_all(&r.to_le_bytes())).and(f.write_all(&v.to_le_bytes())),
                gs::Msg::Vsync(n) => f.write_all(&[VSYNC]).and(f.write_all(&n.to_le_bytes())),
                gs::Msg::Movie(_) => Ok(()),
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
        let _ = f.flush();
    });
    out
}

/// Messages read back from `path`.
pub fn replay(path: String) -> Receiver<gs::Msg> {
    let (tx, out) = sync_channel(4096);
    std::thread::spawn(move || {
        let mut f = BufReader::with_capacity(1 << 20, File::open(&path).unwrap_or_else(|e| panic!("{path}: {e}")));
        eprintln!("[gsrec] replaying {path}");
        let mut tag = [0u8; 1];
        while f.read_exact(&mut tag).is_ok() {
            let msg = match tag[0] {
                GIF => {
                    let mut n = [0u8; 4];
                    if f.read_exact(&mut n).is_err() {
                        break;
                    }
                    let mut d = vec![0u128; u32::from_le_bytes(n) as usize];
                    if f.read_exact(unsafe { std::slice::from_raw_parts_mut(d.as_mut_ptr() as *mut u8, d.len() * 16) }).is_err() {
                        break;
                    }
                    gs::Msg::Gif(d)
                }
                PRIV => {
                    let (mut r, mut v) = ([0u8; 4], [0u8; 8]);
                    if f.read_exact(&mut r).and(f.read_exact(&mut v)).is_err() {
                        break;
                    }
                    gs::Msg::Priv(u32::from_le_bytes(r), u64::from_le_bytes(v))
                }
                _ => {
                    let mut n = [0u8; 8];
                    if f.read_exact(&mut n).is_err() {
                        break;
                    }
                    gs::Msg::Vsync(u64::from_le_bytes(n))
                }
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
        eprintln!("[gsrec] end of {path}");
        // Keep the renderer's channel open so the window stays up.
        std::thread::park();
        drop(tx);
    });
    out
}
