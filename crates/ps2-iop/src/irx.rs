//! IRX module loading: relocation and linking.
//!
//! An IRX is a relocatable ELF (type 0xff80) with one PT_LOAD segment at
//! address 0 and an `.iopmod` header giving the entry point and gp.
//! Imports are stub tables in the text: a header (magic 0x41e00000, 0,
//! version, 8-byte library name) followed by `jr ra; addiu zero, zero, n`
//! pairs, where n is the function's ordinal in that library. Exports use
//! magic 0x41c00000 and a zero-terminated list of function addresses.

const ET_IRX: u16 = 0xff80;
const PT_LOAD: u32 = 1;
const PT_IOPMOD: u32 = 0x7000_0080;
const SHT_REL: u32 = 9;

const IMPORT_MAGIC: u32 = 0x41e0_0000;
const EXPORT_MAGIC: u32 = 0x41c0_0000;
const JR_RA: u32 = 0x03e0_0008;

pub struct Module {
    pub name: String,
    pub base: u32,
    /// Loaded size including bss.
    pub size: u32,
    pub entry: u32,
    pub gp: u32,
    pub image: Vec<u8>,
}

/// An import stub to resolve: the address of its `jr ra`, library, ordinal.
pub struct Import {
    pub stub: u32,
    pub library: String,
    pub ordinal: u32,
}

/// An exported library: name and absolute function addresses by ordinal.
pub struct Export {
    pub library: String,
    pub funcs: Vec<u32>,
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn put32(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// Relocate an IRX to `base`. The result is the memory image (with bss).
pub fn load(file: &[u8], base: u32) -> Result<Module, String> {
    if file.len() < 52 || &file[..4] != b"\x7fELF" || u16_at(file, 16) != ET_IRX {
        return Err("not an IRX module".into());
    }
    let (phoff, phnum) = (u32_at(file, 28) as usize, u16_at(file, 44) as usize);
    let (shoff, shnum) = (u32_at(file, 32) as usize, u16_at(file, 48) as usize);
    let mut image = Vec::new();
    let mut iopmod = None;
    for i in 0..phnum {
        let ph = phoff + i * 32;
        let (ty, off, filesz, memsz) = (u32_at(file, ph), u32_at(file, ph + 4) as usize, u32_at(file, ph + 16) as usize, u32_at(file, ph + 20) as usize);
        match ty {
            PT_LOAD => {
                image = vec![0u8; memsz];
                image[..filesz].copy_from_slice(&file[off..off + filesz]);
            }
            PT_IOPMOD => iopmod = Some(off),
            _ => {}
        }
    }
    let m = iopmod.ok_or("no .iopmod header")?;
    let entry = u32_at(file, m + 4);
    let gp = u32_at(file, m + 8);
    let name: String = file[m + 26..].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();

    // Relocations. HI16 entries are held until their LO16 partner, which
    // carries the low half of the addend.
    for i in 0..shnum {
        let sh = shoff + i * 40;
        if u32_at(file, sh + 4) != SHT_REL {
            continue;
        }
        let (off, size) = (u32_at(file, sh + 16) as usize, u32_at(file, sh + 20) as usize);
        let mut pending_hi: Vec<usize> = Vec::new();
        for e in (off..off + size).step_by(8) {
            let at = u32_at(file, e) as usize;
            let ty = u32_at(file, e + 4) & 0xff;
            if at + 4 > image.len() {
                return Err(format!("relocation at {at:#x} outside the image"));
            }
            let w = u32_at(&image, at);
            match ty {
                0 => {}
                2 => put32(&mut image, at, w.wrapping_add(base)),
                4 => {
                    let target = ((w & 0x03ff_ffff) << 2).wrapping_add(base);
                    put32(&mut image, at, (w & 0xfc00_0000) | ((target >> 2) & 0x03ff_ffff));
                }
                5 => pending_hi.push(at),
                6 => {
                    let lo = w as u16 as i16 as i32;
                    for hi_at in pending_hi.drain(..) {
                        let hw = u32_at(&image, hi_at);
                        let ahl = ((hw & 0xffff) << 16).wrapping_add(lo as u32);
                        let v = ahl.wrapping_add(base);
                        put32(&mut image, hi_at, (hw & 0xffff_0000) | (v.wrapping_add(0x8000) >> 16));
                    }
                    put32(&mut image, at, (w & 0xffff_0000) | ((lo as u32).wrapping_add(base) & 0xffff));
                }
                t => return Err(format!("unsupported relocation type {t}")),
            }
        }
    }
    Ok(Module { name, base, size: image.len() as u32, entry: entry.wrapping_add(base), gp: gp.wrapping_add(base), image })
}

fn lib_name(image: &[u8], o: usize) -> String {
    image[o..o + 8].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect()
}

/// Import stubs and export tables in a loaded module.
pub fn tables(m: &Module) -> (Vec<Import>, Vec<Export>) {
    let d = &m.image;
    let (mut imports, mut exports) = (Vec::new(), Vec::new());
    let mut o = 0;
    while o + 20 <= d.len() {
        let w = u32_at(d, o);
        if (w == IMPORT_MAGIC || w == EXPORT_MAGIC) && u32_at(d, o + 4) == 0 {
            let library = lib_name(d, o + 12);
            let mut j = o + 20;
            if w == IMPORT_MAGIC {
                while j + 8 <= d.len() && u32_at(d, j) == JR_RA && u32_at(d, j + 4) >> 16 == 0x2400 {
                    imports.push(Import { stub: m.base + j as u32, library: library.clone(), ordinal: u32_at(d, j + 4) & 0xffff });
                    j += 8;
                }
            } else {
                let mut funcs = Vec::new();
                while j + 4 <= d.len() && u32_at(d, j) != 0 {
                    funcs.push(u32_at(d, j));
                    j += 4;
                }
                exports.push(Export { library, funcs });
            }
            o = j;
            continue;
        }
        o += 4;
    }
    (imports, exports)
}

/// MIPS encodings used to patch import stubs.
pub fn syscall(code: u32) -> u32 {
    0x0000_000c | (code & 0xfffff) << 6
}

pub fn jump(target: u32) -> u32 {
    0x0800_0000 | ((target >> 2) & 0x03ff_ffff)
}

pub const NOP: u32 = 0;
