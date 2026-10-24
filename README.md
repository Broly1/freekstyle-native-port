# Freekstyle native port

A native Linux port of *Freekstyle* (PS2, SLUS-20367), written in Rust with a
Vulkan renderer. The goal is 60 fps at high resolution.

The game's EE (Emotion Engine) code is **statically recompiled**: a tool
translates every MIPS R5900 function into Rust ahead of time. A Rust runtime
then replaces the PS2 hardware and OS around that code. Hot or awkward
functions can later be rewritten by hand in idiomatic Rust, one at a time.

No game data is included. You need your own copy of the disc; extracted files
go in `game/`, which git ignores.

## Crates

| Crate        | Status | Purpose |
|--------------|--------|---------|
| `r5900`      | done   | Instruction decoder: MIPS III, EE extensions, MMI, COP0, COP1 FPU, VU0 macro mode |
| `ee-analyze` | done   | Loads the ELF, finds every function, resolves jump tables, reports coverage |
| `ee-recomp`  | next   | Emits Rust from the analyzed functions |
| `ee-runtime` | todo   | CPU state, 32 MB RAM, syscalls, DMA, timers, threads, file I/O from extracted disc files |
| `gs-vulkan`  | todo   | Graphics Synthesizer (GS) / GIF / VIF1 / VU1 implemented on Vulkan, with resolution scaling |
| `iop-hle`    | todo   | High-level replacements for the I/O processor (IOP) modules (pad, memory card, CD, EA `AUDIO.IRX`) |

## Usage

```sh
cargo run --release -p ee-analyze -- game/SLUS_203.67 out
```

## Analysis results (SLUS_203.67, sha256 debcb4b0…6d80a)

- 3,873 functions. 95.5% of the text section (0x100000–0x2279c8) is reached,
  with zero undecodable instructions.
- 59 jump tables resolved. The one remaining indirect `jr` (fn 0x2206c0)
  dispatches through a gp-relative table of 2 known functions. There are
  171 `jalr` call sites.
- Built with Sony SDK libraries (libgraph 2.4.0, libpad, libmc, libcdvd). It
  makes about 120 distinct kernel syscalls.
- 198 distinct mnemonics are used: about 3,000 VU0 macro instructions, 99
  `vcallms` VU0 microprogram calls, and heavy 128-bit MMI use. The compiler's
  128-bit move idiom is `paddub rd, rs, $zero`.
