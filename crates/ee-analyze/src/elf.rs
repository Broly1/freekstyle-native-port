//! Minimal loader for the 32-bit little-endian MIPS ELF the PS2 boots.

pub struct Segment {
    pub vaddr: u32,
    pub data: Vec<u8>,
    /// Bytes backed by the file; the rest of `data` is zero-filled bss.
    pub file_len: usize,
    pub executable: bool,
}

pub struct Elf {
    pub entry: u32,
    pub segments: Vec<Segment>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl Elf {
    pub fn parse(bytes: &[u8]) -> Result<Elf, String> {
        if bytes.len() < 52 || &bytes[..4] != b"\x7fELF" {
            return Err("not an ELF file".into());
        }
        if bytes[4] != 1 || bytes[5] != 1 {
            return Err("expected ELF32 little-endian".into());
        }
        if u16_at(bytes, 0x12) != 8 {
            return Err("expected EM_MIPS".into());
        }
        let entry = u32_at(bytes, 0x18);
        let phoff = u32_at(bytes, 0x1c) as usize;
        let phentsize = u16_at(bytes, 0x2a) as usize;
        let phnum = u16_at(bytes, 0x2c) as usize;

        let mut segments = Vec::new();
        for n in 0..phnum {
            let ph = phoff + n * phentsize;
            if u32_at(bytes, ph) != 1 {
                continue; // not PT_LOAD
            }
            let offset = u32_at(bytes, ph + 4) as usize;
            let vaddr = u32_at(bytes, ph + 8);
            let filesz = u32_at(bytes, ph + 16) as usize;
            let memsz = u32_at(bytes, ph + 20) as usize;
            let flags = u32_at(bytes, ph + 24);
            if memsz == 0 {
                continue;
            }
            let file = bytes
                .get(offset..offset + filesz)
                .ok_or("segment extends past end of file")?;
            let mut data = file.to_vec();
            data.resize(memsz, 0);
            segments.push(Segment { vaddr, data, file_len: filesz, executable: flags & 1 != 0 });
        }
        Ok(Elf { entry, segments })
    }

    pub fn read_u32(&self, addr: u32) -> Option<u32> {
        self.segments.iter().find_map(|s| {
            let off = addr.checked_sub(s.vaddr)? as usize;
            s.data.get(off..off + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        })
    }
}
