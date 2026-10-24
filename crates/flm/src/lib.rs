//! EA `.FLM` movies as used by Freekstyle.
//!
//! A movie is a sequence of chunks, each tag stored byte-reversed:
//!
//! - `MGAV` ("VAGM", 0x6000 bytes): PS2 ADPCM audio. A 0x40-byte header,
//!   then 0x2fc0 bytes of the left channel and 0x2fc0 of the right.
//! - `2GPM` ("MPG2", size at +4): MPEG-2 video. A 0x40-byte header whose
//!   word at +0x14 is the payload length, then elementary stream data.
//! - `LLIF` ("FILL", size at +4): padding.
//!
//! Video is 512x448 at 29.97 fps; audio is 22050 Hz stereo.

pub const AUDIO_RATE: u32 = 22_050;
pub const VIDEO_FPS: (u32, u32) = (30_000, 1_001);

const AUDIO_BLOCK: usize = 0x6000;
const AUDIO_HEADER: usize = 0x40;
const AUDIO_CHANNEL: usize = 0x2fc0;
const VIDEO_HEADER: usize = 0x40;

/// The two elementary streams of a movie.
pub struct Movie {
    /// MPEG-2 video elementary stream.
    pub video: Vec<u8>,
    /// Interleaved stereo PCM (left, right).
    pub audio: Vec<i16>,
}

pub fn demux(data: &[u8]) -> Result<Movie, String> {
    let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
    let mut video = Vec::new();
    let (mut left, mut right) = (Vag::default(), Vag::default());
    let mut audio = Vec::new();
    let mut off = 0;
    while off + 8 <= data.len() {
        let tag = &data[off..off + 4];
        let size = u32_at(off + 4);
        match tag {
            b"MGAV" => {
                let block = data.get(off..off + AUDIO_BLOCK).ok_or("truncated audio block")?;
                let l = left.decode(&block[AUDIO_HEADER..AUDIO_HEADER + AUDIO_CHANNEL]);
                let r = right.decode(&block[AUDIO_HEADER + AUDIO_CHANNEL..AUDIO_HEADER + 2 * AUDIO_CHANNEL]);
                audio.extend(l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]));
                off += AUDIO_BLOCK;
            }
            b"2GPM" => {
                let n = u32_at(off + 0x14);
                let payload = data.get(off + VIDEO_HEADER..off + VIDEO_HEADER + n).ok_or("truncated video chunk")?;
                video.extend_from_slice(payload);
                off += size;
            }
            b"LLIF" => off += size,
            // Zero padding to the end of the file.
            [0, 0, 0, 0] => break,
            t => return Err(format!("unknown chunk {:?} at {off:#x}", String::from_utf8_lossy(t))),
        }
        if size == 0 && tag != b"MGAV" {
            return Err(format!("zero-sized chunk at {off:#x}"));
        }
    }
    Ok(Movie { video, audio })
}

/// PS2 ADPCM ("VAG") decoder state for one channel.
#[derive(Default)]
pub struct Vag {
    s1: i32,
    s2: i32,
}

const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

impl Vag {
    /// Decode 16-byte frames (28 samples each).
    pub fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        let mut out = Vec::with_capacity(data.len() / 16 * 28);
        for frame in data.chunks_exact(16) {
            let shift = (frame[0] & 15) as i32;
            let (f0, f1) = FILTERS[((frame[0] >> 4) as usize).min(4)];
            for byte in &frame[2..] {
                for nibble in [byte & 15, byte >> 4] {
                    let raw = ((nibble as i32) << 28 >> 28) << 12;
                    let s = (raw >> shift.min(12)) + (self.s1 * f0 + self.s2 * f1 + 32) / 64;
                    let s = s.clamp(i16::MIN as i32, i16::MAX as i32);
                    self.s2 = self.s1;
                    self.s1 = s;
                    out.push(s as i16);
                }
            }
        }
        out
    }
}

/// A 16-bit stereo WAV file.
pub fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let bytes = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + bytes as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + bytes).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&2u16.to_le_bytes()); // stereo
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&bytes.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vag_decodes_predictor_and_shift() {
        // Filter 0, shift 12: nibbles become raw samples (value << 12 >> 12).
        let mut frame = [0u8; 16];
        frame[0] = 0x0c;
        frame[2] = 0x71; // samples 1 then 7
        frame[3] = 0x0f; // -1 then 0
        let s = Vag::default().decode(&frame);
        assert_eq!(&s[..4], &[1, 7, -1, 0]);
        // Filter 1 continues from the previous sample: 60/64 of it.
        let mut v = Vag { s1: 640, s2: 0 };
        let mut f = [0u8; 16];
        f[0] = 0x1c;
        assert_eq!(v.decode(&f)[0], 600);
    }

    #[test]
    fn demuxes_interleaved_chunks() {
        let mut file = vec![0u8; AUDIO_BLOCK];
        file[..4].copy_from_slice(b"MGAV");
        file[4..8].copy_from_slice(&0x5fc0u32.to_le_bytes());
        let mut v = vec![0u8; 0x1000];
        v[..4].copy_from_slice(b"2GPM");
        v[4..8].copy_from_slice(&0x1000u32.to_le_bytes());
        v[0x14..0x18].copy_from_slice(&4u32.to_le_bytes());
        v[0x40..0x44].copy_from_slice(&[0, 0, 1, 0xb3]);
        file.extend(v);
        let m = demux(&file).unwrap();
        assert_eq!(m.video, [0, 0, 1, 0xb3]);
        assert_eq!(m.audio.len(), AUDIO_CHANNEL / 16 * 28 * 2);
    }
}
