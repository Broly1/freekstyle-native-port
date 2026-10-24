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

| Crate        | Status  | Purpose |
|--------------|---------|---------|
| `r5900`      | done    | Instruction decoder: MIPS III, EE extensions, MMI, COP0, COP1 FPU, VU0 macro mode |
| `ee-analyze` | done    | Loads the ELF, finds every function, resolves jump tables, reports coverage |
| `ee-recomp`  | working | Emits one Rust function per guest function; HLE overrides via `freekstyle/hle.txt` |
| `ee-runtime` | partial | CPU state, memory map, MMI/FPU/VU0 helpers, HLE kernel, vblank + interrupt delivery, SPR DMA, SIF RPC layer |
| `freekstyle` | boots   | The game binary: generated code + runtime, with a hang watchdog |
| IOP services | partial | CD/DVD: init, disk type/ready, file search and sector reads from the disc image. Todo: module loader, pad, memory card, EA audio |
| GS / Vulkan  | todo    | GIF/VIF1/VU1 paths and a Vulkan renderer with resolution scaling |

## Building and running

```sh
# 1. Copy SLUS_203.67 from your own disc into game/.
# 2. Generate Rust from it, then build and run.
cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt
cargo run --release -p freekstyle -- game/SLUS_203.67 [disc image]
```

The disc image (raw `.bin` or `.iso`) defaults to `game/Freekstyle.bin`.

Generation takes under a second. The release build of about 460k generated
lines takes about a minute on 12 cores.

Debugging switches (environment variables):

| Variable             | Effect |
|----------------------|--------|
| `FREEK_SYSCALL_LOG`  | Log every syscall and SIF RPC |
| `FREEK_IO_LOG`       | Log every hardware register access |
| `FREEK_HEARTBEAT`    | Every 2 s, print the current guest function and call rate |
| `FREEK_QUIET`        | Hide routine kernel messages |
| `FREEK_RPC_DUMP`     | Hex-dump every IOP request |

When guest code calls an address the analyzer missed, the runtime says so.
Add the address to `freekstyle/seeds.txt` and regenerate.

## Current state

The recompiled game boots natively and reaches its frame loop.

1. Startup, kernel and library init, GS reset, then the first GIF packet.
2. IOP services bind. The CD service finds and reads files from the disc
   image: `ENGLISH.DAT`, palettes, font glyphs, then
   `\SXDATA\OPENING\SPLASH1.LSC`.
3. Threads run under an EE-accurate priority scheduler. The CD callback
   thread wakes when a read completes, and `sceCdSync` returns.
4. The splash image is sent to the IPU (MPEG decoder), and the render loop
   runs once per vblank at 60 Hz.

Nothing is visible yet: the GIF, VIF1 and IPU DMA paths complete without
effect.

Next steps, in order:
1. GIF/VIF1 packet processing and a Vulkan GS renderer, so frames appear.
2. IPU (MPEG-2 intra/inter decode) for the splash screen and movies.
3. VU0/VU1 microprogram execution (99 `vcallms` sites, plus VU1 rendering).
4. Pad input (libpad expects padman 4.0), memory card saves, EA audio.

## Analysis results (SLUS_203.67, sha256 debcb4b0…6d80a)

- 3,964 functions. 96.1% of the text section is reached. No instruction in
  call-reachable code is undecodable; 3 heuristic starts land on data and
  become runtime traps that never execute.
- 59 jump tables resolved. The one remaining indirect `jr` (fn 0x2206c0)
  dispatches through a gp-relative table of 2 known functions. There are
  171 `jalr` call sites.
- Built with Sony SDK libraries (libgraph 2.4.0, libpad, libmc, libcdvd). It
  makes about 120 distinct kernel syscalls.
- 198 distinct mnemonics are used: about 3,000 VU0 macro instructions, 99
  `vcallms` VU0 microprogram calls, and heavy 128-bit MMI use. The compiler's
  128-bit move idiom is `paddub rd, rs, $zero`.
