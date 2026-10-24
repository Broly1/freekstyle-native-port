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
| `gs`         | working | GS core: registers, GIF PACKED/REGLIST/IMAGE, 4 MB VRAM with all swizzled formats (verified against hardware tables), transfers, CLUT, primitive assembly |
| `gs-vulkan`  | working | Vulkan renderer: render targets at N x resolution (3x 1080p, 5x 4K), GS blend modes, texture cache, display output, screenshots |
| VU0/VU1, IPU | todo    | Microprogram execution (3D) and MPEG decoding (splash/movies) |

## Building and running

```sh
# 1. Copy SLUS_203.67 from your own disc into game/.
# 2. Generate Rust from it, then build and run.
cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt
cargo run --release -p freekstyle -- [--res 1080|1440|4k] [--scale N] [--fullscreen] [--stretch]
```

`--res` picks the window size and a matching internal resolution (3x, 4x
or 5x the PS2's 512x448). `--scale` overrides the multiplier. F11 toggles
fullscreen and Esc quits.

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
| `FREEK_GS_LOG`       | Renderer state every 2 s, privileged register writes |
| `FREEK_SHOT=f.png:N` | Save the displayed frame N at internal resolution |

When guest code calls an address the analyzer missed, the runtime says so.
Add the address to `freekstyle/seeds.txt` and regenerate.

## Current state

The game runs natively and draws through Vulkan. The first screen it shows
is its own memory card check ("NO MEMORY CARD (8MB) ... IN MEMORY CARD
SLOT 1."), rendered at up to 5x resolution. This is correct behaviour for
now, because the memory card service is still a stub and there is no
controller input to dismiss the screen.

The path a frame takes: recompiled game code → VIF1/GIF DMA (tag chains,
VIF commands) → GS packets over a channel → the `gs` core on the render
thread → batched Vulkan draws into upscaled targets → displayed buffer
blitted to the window at 4:3.

Next steps, in order:
1. Controller input (libpad/padman over SIF) and memory card saves
   (libmc/mcserv).
2. VU1 microprogram execution, which the game uses for 3D (MSCAL is
   currently skipped).
3. VU0 microprograms (99 `vcallms` sites) and the IPU (MPEG splash and
   movies).
4. EA audio (`AUDIO.IRX` over SIF), then 60 fps and widescreen patches.

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
