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
| IOP services | todo    | Native CD/file I/O, module loader, pad, memory card, EA audio (`ee_runtime::iop`) |
| GS / Vulkan  | todo    | GIF/VIF1/VU1 paths and a Vulkan renderer with resolution scaling |

## Building and running

```sh
# 1. Copy SLUS_203.67 from your own disc into game/.
# 2. Generate Rust from it, then build and run.
cargo run --release -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt
cargo run --release -p freekstyle -- game/SLUS_203.67
```

Generation takes under a second. The release build of about 460k generated
lines takes about a minute on 12 cores.

Debugging switches (environment variables):

| Variable             | Effect |
|----------------------|--------|
| `FREEK_SYSCALL_LOG`  | Log every syscall and SIF RPC |
| `FREEK_IO_LOG`       | Log every hardware register access |
| `FREEK_HEARTBEAT`    | Every 2 s, print the current guest function and call rate |
| `FREEK_QUIET`        | Hide routine kernel messages |

When guest code calls an address the analyzer missed, the runtime says so.
Add the address to `freekstyle/seeds.txt` and regenerate.

## Current state

The recompiled game boots natively. It runs the startup code, kernel and
library init, resets the GS, waits for vblank, sends its first GIF packet,
binds IOP services (module loader, CD/DVD, pad, memory card), starts VU0, and
enters its frame loop. It then waits on CD file searches, because the CD
service is still a stub. Nothing is drawn yet.

Next steps, in order:
1. CD/DVD and file I/O served from the disc image (search, read, stream).
2. Thread scheduling (the game creates threads and blocks on semaphores).
3. GIF/VIF1 packet processing and a Vulkan GS renderer.
4. VU0/VU1 microprogram execution (99 `vcallms` sites, plus VU1 rendering).
5. Pad input (SDL/evdev), memory card saves, EA audio.

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
