# Freekstyle native port

A native Linux port of *Freekstyle* (PS2, SLUS-20367), written in Rust with a
Vulkan renderer. The goal is 60 fps at high resolution.

The game's EE (Emotion Engine) code is **statically recompiled**: a tool
translates every MIPS R5900 function into Rust ahead of time. A Rust runtime
then replaces the PS2 hardware and OS around that code. Hot or awkward
functions can later be rewritten by hand in idiomatic Rust, one at a time.

No game data is included. You need your own copy of the disc; extracted files
go in `game/`, which git ignores.

> Unofficial fan project, not affiliated with or endorsed by Electronic Arts,
> Sony Interactive Entertainment or Microsoft. *Freekstyle* is a trademark of
> Electronic Arts; PlayStation and Xbox are trademarks of their owners. This
> repository contains no game code or assets, and no prebuilt game binaries
> are distributed: the game is built on your machine from a disc you own.
> Please don't open issues with game files or links to them.

## Crates

| Crate        | Status  | Purpose |
|--------------|---------|---------|
| `r5900`      | done    | Instruction decoder: MIPS III, EE extensions, MMI, COP0, COP1 FPU, VU0 macro mode |
| `ee-analyze` | done    | Loads the ELF, finds every function, resolves jump tables, reports coverage |
| `ee-recomp`  | working | Emits one Rust function per guest function; HLE overrides via `freekstyle/hle.txt` |
| `ee-runtime` | partial | CPU state, memory map, MMI/FPU/VU0 helpers, HLE kernel, vblank + interrupt delivery, SPR DMA, SIF RPC layer |
| `freekstyle` | boots   | The game binary: generated code + runtime, with a hang watchdog |
| IOP services | partial | CD/DVD from the disc image; padman (controller frames each vblank, two ports); saves as files instead of memory cards |
| `ps2-iop`    | working | Sound: the game's own IOP driver (EA's AUDIO.IRX on Sony's LIBSD.IRX) runs on an R3000 interpreter over a native IOP kernel (threads, interrupts, SIF commands), with an SPU2 (ADPCM voices, ADSR, DMA) feeding the host audio device at 48 kHz |
| `gs`         | working | GS core: registers, GIF PACKED/REGLIST/IMAGE, 4 MB VRAM with all swizzled formats (verified against hardware tables), transfers, CLUT, primitive assembly |
| `gs-vulkan`  | working | Vulkan renderer: render targets at N x resolution (3x 1080p, 5x 4K), GS blend modes, texture cache, display output, screenshots |
| VU0/VU1      | working | VIF unpack/MPG/DIRECT, VU micro-mode interpreter with pipeline timing (Q/P latency, flag delay, hazard stalls); recorded programs are compiled to native SSE code from `freekstyle/vu_dumps` (see below) |
| `flm`        | working | EA `.FLM` movie demuxer and PS2 ADPCM decoder; `flm2mkv` converts movies to VP9 + Opus `.mkv` |
| pictures     | working | No IPU emulation: `lsc2png` converts the 21 MPEG-2 still pictures (.LSC: title, menu, loading screens) to PNG once, and the game's picture decoder is replaced by a native function that loads them. The converter's decoder matches FFmpeg to 45-60 dB |

## Setting up from your own disc

The port needs the North American release (SLUS-20367); nothing from the
game is in this repository. You need Python 3 (preinstalled on most
distributions), Rust 1.95 or newer (distributions often ship an older one;
`install.py` then installs current Rust with rustup), a Vulkan driver and,
for the optional movies, ffmpeg with MPEG-2 (and libplacebo for their CRT
look; most distributions' ffmpeg has both, Fedora's needs RPM Fusion, which
`install.py` offers to enable).

**Quick way**: dump your disc (step 1 below) into `game/`, then run

```sh
./install.py
```

It shows what is present and what is missing, installs the missing pieces
(Rust through rustup; build libraries, a Vulkan driver for your GPU and
ffmpeg through pacman, apt, dnf or zypper; asking first) and does steps 2
to 5 for you, in English, Portuguese, Spanish or French depending on the
system language. Everything it runs is logged to `install.log`. At the end
it offers to install the game to the system, like a game from a store:
`/opt/freekstyle`, with Freekstyle Launcher in the app menu's Games section
(KDE, GNOME, Cinnamon, XFCE and other freedesktop menus) and
`freekstyle-launcher` on the command line. The source folder is then no
longer needed to play; settings go to `~/.config/freekstyle`, saves to
`~/.local/share/freekstyle/saves`. `./uninstall.py` (also copied to
`/opt/freekstyle/uninstall.py`) removes the game and what `install.py`
added: it lists each part and asks, keeping your saves and settings,
packages and Rust unless you say otherwise, and removes a package only if
nothing else installed needs it (`--dry-run` shows what it would do). Or keep it in the folder and start
`./target/release/freekstyle-launcher`. Steps already done are skipped, so
running it again after a `git pull` only redoes what changed. For an
unattended run: `./install.py --yes --system` (or `--here`).

The same by hand:

1. **Dump your disc** to `game/Freekstyle.bin`. Either format works:
   a raw image (`.bin`, 2352-byte sectors, as made by ImgBurn or
   `cdrdao read-cd --read-raw`) or a plain ISO, e.g.
   `dd if=/dev/sr0 of=game/Freekstyle.bin bs=2048`.
2. **Copy the game's program off it**:
   ```sh
   cargo run --release -p ee-runtime --bin disc-extract   # -> game/SLUS_203.67
   ```
   Its sha256 should be `debcb4b0731741a389a0c890296609eb99d111952e97a250ea4c3cfc04a6d80a`.
3. **Generate the Rust code** from it and build:
   ```sh
   cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt freekstyle/vu_dumps
   cargo build --release -p freekstyle -p freekstyle-launcher
   ```
4. **Convert the title and menu pictures** (optional, see *Pictures*):
   ```sh
   cargo run --release -p ee-runtime --bin lsc2png
   ```
5. **Convert the movies** (optional, see *Movies*): the intro and logos,
   in HD with a CRT TV look. Without them the game skips its movies.
   ```sh
   for m in OPENING/BIGLOGO OPENING/EAGAMES OPENING/EAS_BIG OPENING/INTRO ABOUTEA/PROD1; do
     cargo run --release -p ee-runtime --bin disc-extract -- game/Freekstyle.bin "\\SXDATA\\${m%/*}\\${m#*/}.FLM" game/movies/flm
   done
   cargo run --release -p flm --bin flm2mkv -- --crt game/movies/flm game/movies/hd
   ```
6. **Play**: `./target/release/freekstyle-launcher`, or the game directly:
   ```sh
   ./target/release/freekstyle [--res 1080|1440|4k] [--scale N] [--fullscreen] [--widescreen] [--stretch] [--blur-bars] [--mute]
   ```

Run everything from the repository root; the game reads `game/` and
`freekstyle.toml` from there.

`--res` picks the window size and a matching internal resolution (3x, 4x
or 5x the PS2's 512x448; vertical scale is doubled again in interlaced
field mode). `--scale` overrides the multiplier. F11 toggles fullscreen,
F12 starts and stops recording video into `captures/` and
Esc quits.

## PC conveniences

- **Launcher**: `cargo run --release -p freekstyle-launcher` sets display
  options and remaps keys and gamepad buttons, saved to `freekstyle.toml`
  (which the game reads at startup; command-line options override it), then
  starts the game. It also installs app-menu entries and icons under
  `~/.local/share` so Wayland desktops show the right icons.
- **Controllers**: any gamepad (SDL-style mapping through gilrs). Buttons
  map by position, so Xbox A/B/X/Y act as Cross/Circle/Square/Triangle.
  Keyboard and gamepad work together. Default keys: WASD or arrows = D-pad
  and left stick, J/Space/Enter = A (menus confirm with A), K = B, U = X,
  I = Y, P/O = LB/RB, ;/L = LT/RT, T = Start (pause), Backspace = Back.
- **Two players**: the game's split-screen modes (Single Race → Two
  Players) work. Gamepads become player 1 and player 2 in the order they
  connect; one that drops off keeps its seat for the next pad that
  connects. The keyboard plays as player 1, or as player 2 with
  `keyboard_player = 2` (the launcher's Controls tab), so one person can
  use a pad and the other the keyboard.
- **Sharp textures**: textures drawn as 2D sprites (menus, text, HUD) are
  upscaled with xBRZ to the internal resolution on a worker thread
  (`--no-sharp-textures` turns it off). 3D surfaces keep their originals.
  xBRZ comes from `xbrz-rs`, which is GPL-3.0.
- **Xbox button prompts**: the game's PlayStation button icons are
  recognised by content and redrawn as high-resolution Xbox A/B/X/Y and
  LB/RB/LT/RT buttons and a solid D-pad. D-pads in the loading tips are
  drawn over, and the loading screen's controller diagram gets an
  Xbox-style pad drawn by `tools/make-xbox-pad.py` (`--ps2-icons` keeps
  the originals).
- **Saves without memory cards**: the game's libmc calls are served from
  `$XDG_DATA_HOME/freekstyle/saves/` (default `~/.local/share/...`), always
  available. The 70 memory card messages in `ENGLISH.DAT` are reworded
  ("Saving.", "No saved game found.", ...) when the file is read.

The disc image (raw `.bin` or `.iso`) defaults to `game/Freekstyle.bin`.

## Pictures

The title, menu and loading-screen backgrounds are MPEG-2 stills that the
PS2 decodes with its IPU. Convert them to PNG once:

```sh
cargo run --release -p ee-runtime --bin lsc2png -- game/Freekstyle.bin game/images
```

The game then loads `game/images/*.png` (found next to the disc image).
They can be edited; a picture of another size is scaled to the original's.
Without them the game still works, decoding the originals in software.

**High-resolution pictures**: put a replacement with the same name in
`game/images/hd/` (for example an AI upscale). It may have any size and is
shown over the same 4:3 area, so make it 4:3 (2880x2160 for 4K); the
originals are 512x448 shown at 4:3, so stretch an upscale's width by 8/7.
The renderer recognises the picture by its texels, resamples the
replacement (Lanczos) to the internal resolution and draws it instead.

On a wide window the 4:3 picture is centred with black bars;
`--blur-bars` fills them with a blurred copy of the picture.

## Widescreen

`--widescreen` gives a real 16:9 view rather than a stretched one:

- **3D**: the race camera's horizontal projection scale (set at 0x145210
  from 0x1443a4) becomes 0.75 instead of 1.0, the same change as PCSX2's
  widescreen patch. The game renders a third more view squeezed into its
  4:3 frame, which is shown stretched to 16:9. The frame is rendered with
  a 4/3 higher horizontal internal resolution so the stretch costs no
  sharpness.
- **HUD and menu text**: drawn 3/4 as wide around the screen centre, so
  they keep their shape inside the middle 4:3 area. The renderer
  recognises them as draws at the far depth limit with the depth test on.
- **Pictures**: `game/images/hd/wide/<NAME>.png` (16:9, e.g. 3840x2160)
  is used if present, else the 4:3 one is stretched. Make them with
  `tools/widen-pictures.py` (ffmpeg + numpy): it stretches the edges more
  than the centre, and the game's overlays on a picture (loading bar,
  "PRESS START") are moved by the same curve so they stay in place.

## Movies

The intro and logo movies are EA `.FLM` files (MPEG-2 video at 512x448,
29.97 fps, with PS2 ADPCM stereo audio). `flm2mkv` converts them to
Linux-friendly Matroska (VP9 video, Opus audio) with ffmpeg. Copy them off
the disc first, then convert:

```sh
for m in OPENING/BIGLOGO OPENING/EAGAMES OPENING/EAS_BIG OPENING/INTRO ABOUTEA/PROD1; do
  cargo run --release -p ee-runtime --bin disc-extract -- game/Freekstyle.bin "\\SXDATA\\${m%/*}\\${m#*/}.FLM" game/movies/flm
done
cargo run --release -p flm --bin flm2mkv -- game/movies/flm game/movies
```

For an HD version that looks like a CRT TV (flat screen, scanlines, RGB
phosphor stripes, a little glow) add `--crt`. It renders 1440x1080 at
59.94 fps on the GPU through ffmpeg's libplacebo filter (`--height N` picks
another size), and looks best fullscreen on a 1080p or 4K screen:

```sh
cargo run --release -p flm --bin flm2mkv -- --crt game/movies/flm game/movies/hd
```

The shader is `crates/flm/src/crt.glsl`; it also works live in mpv
(`mpv --glsl-shaders=crates/flm/src/crt.glsl game/movies/intro.mkv`).

The game plays them where the PS2 did (the intro and logos at boot),
preferring `game/movies/hd/` and falling back to `game/movies/`; a movie
with no conversion is skipped. ffmpeg decodes them while they play, so it
must be installed to see them. Start or Cross skips a movie.

## VU microprograms

The game uploads its VU0/VU1 microcode at run time with VIF MPG packets
stored in the ELF, and calls into it at addresses only seen while playing.
`FREEK_VU_DUMP=freekstyle/vu_dumps` records each program image the
interpreter runs (`.bin`, not committed), its entry points (`.entries`) and
the JR/JALR targets compiled code did not know (`.jumps`). The entry and
jump lists are in the repository; regenerating rebuilds each image from the
ELF's MPG chains (matched by hash) and compiles them (`ee-recomp`'s `vuc`):

- The VU's pipeline timing (hazard stalls, Q latency, flags visible 4
  cycles late, branches seeing a VI register's old value) depends only on
  the path taken, so it is simulated at build time. Each block is
  compiled once per pipeline state it is entered in; the code that remains
  is plain arithmetic.
- VF registers live in SSE registers (`ee_runtime::vux`, bit-identical to
  the interpreter's scalar helpers).
- MAC flags are computed only where a later FMAND/FMEQ/FMOR can see them.
- BAL/JR return addresses are tracked as constants; computed jumps use the
  recorded targets and otherwise continue in the interpreter.

`FREEK_VU_CHECK=1` reruns every compiled program on the interpreter from
the same state and reports any difference. Entries that were never
recorded run on the interpreter (`FREEK_VU_MISS=1` lists them).

Generation takes under a second. The release build of about 460k generated
lines takes about a minute on 12 cores.

Debugging switches (environment variables):

| Variable             | Effect |
|----------------------|--------|
| `FREEK_SYSCALL_LOG`  | Log every syscall and SIF RPC |
| `FREEK_IO_LOG`       | Log every hardware register access |
| `FREEK_HEARTBEAT`    | Every 2 s, print the current guest function and call rate, VU time and idle (polling) time |
| `FREEK_QUIET`        | Hide routine kernel messages |
| `FREEK_RPC_DUMP`     | Hex-dump every IOP request |
| `FREEK_GS_LOG`       | Renderer state every 2 s, privileged register writes |
| `FREEK_SHOT=f.png:N` | Save the displayed frame N at internal resolution |
| `FREEK_SHOT_EVERY=K` | With `FREEK_SHOT`, save every K frames from N on |
| `FREEK_PAD_SCRIPT`   | Scripted taps, e.g. `300:start,600:a+down,900-1500:a,1000:p2+a` (Xbox names; a range holds; `p2` is player 2's pad, which plugs it in) |
| `FREEK_DUMP_TEXTURES=dir` | Write every decoded texture as PNG |
| `IOP_TRACE=1` / `=2` | Log SIF traffic and SPU2 state each second / also every IOP kernel call |
| `IOP_WAV=out.wav`    | Record the sound output |
| `FREEK_PROFILE=N`    | After N seconds, sample the EE thread for 10 s into `profile.txt` (addresses for `addr2line -f -C -e target/release/freekstyle`) |
| `FREEK_AUDIO_LOG`    | Log each audio device callback and the queue depth |
| `FREEK_VU_INTERP`    | Interpret all VU microcode (to tell recompiler bugs from VU semantics bugs) |
| `FREEK_VU_CHECK`     | Check every compiled VU program against the interpreter |
| `FREEK_VU_DUMP=dir`  | Record VU programs, entry points and jump targets for compiling |
| `FREEK_VU_MISS`      | List VU entry points that have no compiled code |
| `FREEK_VU_TOP`       | Periodically list the most-run VU microprogram entry points |
| `FREEK_RECORD=out.mkv:V:S` | Record S seconds of video from vblank V (needs ffmpeg) |
| `FREEK_FRAME_GAPS`   | Report hitches: long gaps between game frames, slow presents |

When guest code calls an address the analyzer missed, the runtime says so.
Add the address to `freekstyle/seeds.txt` and regenerate.

## Current state

The game reaches its main menu natively and is controllable: profile name
entry, main menu (Circuit, Single Race, Freestyle, Freeride, Record Book,
Options), at up to 5x resolution with Xbox prompts. The rider select
screen renders the 3D rider models (VU1) and the animated flames, and
races run at 60 fps. (They used to run at 30: the game has no cap, but a
frame that misses one vblank waits for the next, and emulating VU1 and the
VIF used to cost about 20 ms a frame. `--headless` prints the frame rate.)

Temporarily skipped:
- Intro movies (`.FLM`): movie open/play/close are no-ops.
- SPU2 reverb, noise and pitch modulation, so sound is drier than the
  original.

Next steps, in order:
1. Movies: the `.mkv` conversions played in game (or the IPU's movie
   commands).
2. SPU2 reverb.

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

## License

The code in this repository is GPL-3.0-or-later (see `LICENSE`). It covers
only the port's own code, not the game: the code `ee-recomp` generates from
your executable, and everything extracted from your disc, stay yours to keep
and must not be redistributed.

Most of the code was written with Claude (Anthropic's AI model).
