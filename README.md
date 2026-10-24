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
| IOP services | partial | CD/DVD from the disc image; padman (controller frames each vblank); saves as files instead of memory cards. Todo: EA audio |
| `gs`         | working | GS core: registers, GIF PACKED/REGLIST/IMAGE, 4 MB VRAM with all swizzled formats (verified against hardware tables), transfers, CLUT, primitive assembly |
| `gs-vulkan`  | working | Vulkan renderer: render targets at N x resolution (3x 1080p, 5x 4K), GS blend modes, texture cache, display output, screenshots |
| VU0/VU1      | working | VIF unpack/MPG/DIRECT, VU micro-mode interpreter with pipeline timing (Q/P latency, flag delay, hazard stalls); recorded programs are recompiled to Rust from `freekstyle/vu_dumps` |
| `flm`        | working | EA `.FLM` movie demuxer and PS2 ADPCM decoder; `flm2mkv` converts movies to VP9 + Opus `.mkv` |
| IPU          | todo    | MPEG decoding for compressed backgrounds |

## Building and running

```sh
# 1. Copy SLUS_203.67 from your own disc into game/.
# 2. Generate Rust from it, then build and run.
cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt
cargo run --release -p freekstyle -- [--res 1080|1440|4k] [--scale N] [--fullscreen] [--stretch]
```

`--res` picks the window size and a matching internal resolution (3x, 4x
or 5x the PS2's 512x448; vertical scale is doubled again in interlaced
field mode). `--scale` overrides the multiplier. F11 toggles fullscreen
and Esc quits.

## PC conveniences

- **Controllers**: any gamepad (SDL-style mapping through gilrs). Buttons
  map by position, so Xbox A/B/X/Y act as Cross/Circle/Square/Triangle.
  Keyboard: arrows = D-pad, Z/Space = A, X = B, A = X, S = Y, Q/E = LB/RB,
  1/3 = LT/RT, Enter = Start, Backspace = Back.
- **Xbox button prompts**: the game's PlayStation button icons are
  recognised by content and redrawn as high-resolution Xbox A/B/X/Y
  buttons (`--ps2-icons` keeps the originals).
- **Saves without memory cards**: the game's libmc calls are served from
  `$XDG_DATA_HOME/freekstyle/saves/` (default `~/.local/share/...`), always
  available. The 70 memory card messages in `ENGLISH.DAT` are reworded
  ("Saving.", "No saved game found.", ...) when the file is read.

The disc image (raw `.bin` or `.iso`) defaults to `game/Freekstyle.bin`.

## Movies

The intro and logo movies are EA `.FLM` files (MPEG-2 video at 512x448,
29.97 fps, with PS2 ADPCM stereo audio). `flm2mkv` converts them to
Linux-friendly Matroska (VP9 video, Opus audio) with ffmpeg:

```sh
cargo run --release -p flm --bin flm2mkv -- <extracted OPENING dir> game/movies
```

In-game playback of the `.mkv` files is not wired up yet, so the game still
skips its movies.

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
| `FREEK_SHOT_EVERY=K` | With `FREEK_SHOT`, save every K frames from N on |
| `FREEK_PAD_SCRIPT`   | Scripted taps, e.g. `300:start,600:a+down` (Xbox names) |
| `FREEK_DUMP_TEXTURES=dir` | Write every decoded texture as PNG |

When guest code calls an address the analyzer missed, the runtime says so.
Add the address to `freekstyle/seeds.txt` and regenerate.

## Current state

The game reaches its main menu natively and is controllable: profile name
entry, main menu (Circuit, Single Race, Freestyle, Freeride, Record Book,
Options), at up to 5x resolution with Xbox prompts. The rider select
screen renders the 3D rider models (VU1) and the animated flames.

Temporarily skipped:
- Intro movies (`.FLM`) and IPU-compressed backgrounds. The IPU (MPEG
  decoder) is not emulated, so movie open/play/close are no-ops and some
  backgrounds (e.g. the title screen) show undecoded data.
- Sound: EA's IOP audio driver is acknowledged silently each frame.

Next steps, in order:
1. Get into races, and speed up VU1 (busy 3D scenes run at about 20 fps).
2. The IPU decoder for backgrounds, and `.mkv` movie playback in game.
3. EA audio (`AUDIO.IRX` protocol) through the host audio device.
4. 60 fps and widescreen patches.

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
