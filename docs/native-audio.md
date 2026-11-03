# Native audio: what the game asks of its sound driver

Notes for replacing the emulated IOP and SPU2 with a Rust audio engine.
Everything here was read from `AUDIO.IRX` (which keeps its symbol table) or
logged from a run that went from the menus into a race. Items marked
"unknown" have not been worked out.

## Traffic from the EE

All sound traffic is SIF command 0 from the EE. Its packet names a transfer:
EE source at +0x10, size at +0x14, IOP destination at +0x20, and a type byte
at +0x1c/+0x1d (meaning of the values: unknown). The data arrives as the
command's extra payload.

| Size | IOP destination | Contents |
|---|---|---|
| 0x22c0 (8896) | 0x80000 | The command block, about 20 a second over the whole run |
| 0x5fc0 (24512) | 0x100000 and up | One streamed audio chunk (`VAGB` or `VAGM`) |
| varies, up to about 500 KB | 0x80000 | Sound bank body (ADPCM samples) |
| varies, up to about 3 KB | 0x10xxxx-0x17xxxx | `Cshd` and `Cvkh` bank headers |

A bank header is followed by its body, so the engine receives samples already
decompressed and does not need the disc's compression.

## Command block

| Offset | Size | Meaning |
|---|---|---|
| +0x00 | u32 | Number of commands |
| +0x05 | u8 | Audio mode (0 mono, otherwise stereo) |
| +0x07 | u8 | Sfx volume |
| +0x09 | u8 | Music volume |
| +0x0b | u8 | Movie volume |
| +0x0c | u8 | Announcer volume |
| +0x0e | u8 | Engine volume |
| +0x10 | u32 | Stored by the driver; use unknown |
| +0x24 | i8 | Non-zero: bike updates below are valid |
| +0x40 | 0x40 each | Command array, 128 entries |
| +0x2040 | 0x40 each | 8 bike records, first byte = active |
| +0x2240 | 0x40 each | Microphones (listeners), byte +0x3d = active |

## Commands (first byte of each entry)

Slots not listed do nothing. Argument offsets are within the 0x40-byte entry.
"Seen" means the logged run used it.

| Op | What it does | Arguments | Seen |
|---|---|---|---|
| 0x01 | Start music | +1 u8 | yes |
| 0x02 | Voice-over finished | | yes |
| 0x03 | Play a placed sound | +2 u16, +8 vec, +0x14 u32, +0x18 u32 | yes |
| 0x04-0x07 | Resume, stop, stop variants | 1, 0, -1, -2 | 0x04-0x06 |
| 0x0b | Stop music | 0 | yes |
| 0x0c | Set one volume | +1 which (1 sfx, 2 music, 3 movie, 4 announcer, 5 engine), +2 u8 | yes |
| 0x0d | Music stem volumes | +1, +2, +3 u8 | yes |
| 0x0e | Start movie audio | | no |
| 0x11 | Stop movie audio | 0 | yes |
| 0x12 | Stop all bikes | | yes |
| 0x13 | Set up a race or menu | the entry | yes |
| 0x14 | Close the race or menu | | yes |
| 0x16 | Cockpit sound with volume | +1 i8, +2 u16, +4 u32, +8 u32 | no |
| 0x17 | Start a placed voice-over | +4, +8 u32, +0xc coords | yes |
| 0x18 | Move a placed voice-over | +4, +8 u32, +0xc coords | no |
| 0x19 | Pick the voice-over's player | +4, +8 u32 | yes |
| 0x1a | Prepare a voice-over | +4 u32 | yes |
| 0x1b | Play the prepared voice-over | | yes |
| 0x1c | Stop all normal sounds | | no |
| 0x1d | Collision sound | +2 u16, +4 vec, +0x10 u32 | no |
| 0x1f | Ambient sound add, update or remove | the entry | yes, most common |
| 0x20 | Start a two-sided big sound | +1 i8, +4 u16, +8 i16, +0xc i16, +0x10-0x18 u32 | no |
| 0x21 | Stop a big sound | +1 i8 | no |
| 0x22 | Set number of players | +1 u8 | no |
| 0x23 | Low-level play (unused) | | no |
| 0x24 | Low-level shutdown | | yes |
| 0x25 | Menu sound or loop | +1 u8 (0 stops the loop), +2 i16, +4 i8 | yes |
| 0x28 | Stores two words (+4, +8), keeping the old pair | | no |
| 0x2a | Set number of players | +1 u8 | no |
| 0x2b | Mute ambient sounds | +1 u8 | yes |

"Seen" comes from one scripted run, so "no" does not mean unused. A later run
with full blocks also saw 0x16 and 0x18.

## Files on the disc

Chunk tags are stored byte-reversed. Files are built from `CTRL`, `SHOC`
(with `SHDR` or `SDAT` inside) and `FILL` chunks.

| File | Contents |
|---|---|
| `SXDATA/AV/ALL.AV` (342 MB) | Music and ambient streams: a compressed name table, then `SWVR` files of `VAGB` chunks. Tracks seen include `alb01.stream` and `alb02.stream` |
| `SXDATA/*/*.TRK`, `FREEKDAT.PS2` | Level data with `Cshd`, `Cvkh` and `Cvkb` sound banks |
| `SXDATA/*/*.FLM` | Movies with `VAGM` audio |

`Rdat` chunks are compressed with a format not yet decoded: `88 nn` copies
`nn` literal bytes, the rest is unknown. `SDAT` chunks are raw.

## Sound banks

`Cvkh` and `Cshd` start with a 0x20-byte wrapper (tag at +0x1c), then a
further 0x20 bytes. Offsets below are from +0x40.

`Cvkh`:

| Offset | Meaning |
|---|---|
| +0x04 | u8: number of programs |
| +0x0c | u32: body size in bytes |
| +0x10 | u32 array: offset of each program |

Program: u8 tone count, 3 more bytes (unknown), then 0x14-byte tone records.
Tone record: +0x0a ADSR1, +0x0c ADSR2, +0x0e pitch register value, +0x10
sample offset into the body (the driver adds the sound RAM address when it loads the header).
The other fields are unknown. In the six banks logged, all 265 tones had
16-byte-aligned offsets inside the body, and the ADSR values match those seen
at key on. The driver only plays tone 0 of a program.

Word +0 of each wrapper is an EE pointer; subtract the transfer's source
address for the offset of the real data. Word +0x20 is the bank id that ties
a `Cvkh` to its `Cvkb` and to a `Cshd` entry.

`Cshd` data: u16 version (7), u16 entry count, then entries of u16 bank id,
u16 definition count, u32 offset of the definitions.

Sound definition, 0x34 bytes:

| Offset | Meaning |
|---|---|
| +0x00 | Category key; definitions with the same key are variations of one sound |
| +0x02 | u8: program number |
| +0x04 | u8: voice priority (unused natively) |
| +0x05 | u8: 0 means this definition is not added |
| +0x08 | 8 u16: game sound ids that map to the category |
| +0x18 | Near distance |
| +0x1c | Far distance |
| +0x24 | Slope inside far |
| +0x28 | Slope beyond far |
| +0x2c | Bit 0: doppler |
| +0x30 | Random pitch range, applied to ambient sounds when added |

Only the first definition of a category sets its fields. A category holds up
to 8 variations; a play picks one at random, never the same one twice running.

## Race data

Numbers are 16.16 fixed point unless noted. Meanings marked (guess) come from
how a value behaved in one race, not from the code.

### Bike record (0x40 bytes)

| Offset | Type | Meaning |
|---|---|---|
| +0x00 | u8 | Active |
| +0x02 | i8 | Gas: non-zero while on the throttle (8 in the logged race) |
| +0x03 | i8 | Skid input, read by the skid code |
| +0x04 | vec3 | Position |
| +0x10 | i16 | Speed (0-178 logged); the engine clamps it to 200 |
| +0x12 | u16 | Stored; use unknown |
| +0x14 | i32 | 0 to 1.0, scales pitch in engine states 6 and 8 (revs, guess) |
| +0x18 | u8 | Non-zero silences the engine |
| +0x1e | u8 | Skid amount, read by the skid code |
| +0x1f | i8 | Terrain id; -1 means airborne |
| +0x20 | i8 | First-time init for this bike |
| +0x21 | i8 | Sets flag 0x40 when off the gas |
| +0x22 | u8 | Non-zero stops the engine voice so it restarts; 2 or more uses sample 4 for idle |
| +0x24 | i32 | Read by the terrain code for one-shot terrain sounds |
| +0x2a | u8 | Skid input, read by the skid code |
| +0x2b | u8 | Stored; use unknown |
| +0x38 | u32 | Stored; use unknown |
| +0x3c | u8 | Bike sound set. A change stops its sounds; set 2 uses engine samples +6 |
| +0x3d | i8 | Stored; use unknown |
| +0x3e | i8 | Non-zero adds 15% engine pitch (boost, guess) |

The throttle code turns +0x02 and +0x3e into flags, each with a "just changed"
partner: gas 0x08 and 0x10, boost 0x80 and 0x100.

### Engine

Sample state, stored at bike +0x05. Ground and air use different states:

| Gas | Ground | Airborne | Just took off | Just landed |
|---|---|---|---|---|
| Steady off | 2 | 3 | | |
| Steady on | 10, then 8 after a delay | 3 | 4 | 6 (also on any terrain change) |
| Just pressed | 10 | 4 | | |
| Just released | 9 | 5 | | |

A changed state stops the voice and starts a new sample from
the table above. Abrupt changes here are what a crossfade could smooth.

| State | Sample |
|---|---|
| 0 | 5 |
| 1, 2 | 0, or 4 when +0x22 is 2 or more |
| 3 | 0 |
| 4, 10 | 4 |
| 5, 9 | 1 |
| 6 | 2 |
| 7, 8 | 3 (95%) or 2 |
| 11 | 3 (25%) or 2 |

Bike sound set 2 adds 6 to the sample number.

Pitch per frame:

1. Start from doppler x 1.5.
2. Boost: add 15%.
3. Airborne: add 0.2. Otherwise, on or just off the gas: add the terrain's
   table value x 0.01. The table is at 0xd5d0 in `AUDIO.IRX`, one i32 per
   terrain, and starts 0, 8, 0, 0, 10, 15, 8, 8.
4. Add min(speed, 200) x 8 / 65536.
5. Clamp to 0.75-2.0.
6. States 6 and 8: multiply by 0.9375 + (+0x14) / 4.

### Ambient (op 0x1f)

| Offset | Type | Meaning |
|---|---|---|
| +0x01 | i8 | 0 add, 1 update, 2 delete |
| +0x02 | i16 | Sound id (add only) |
| +0x04 | i16 | Slot, 0-127 |
| +0x08 | vec3 | Position |
| +0x14 | fixed | Volume (guess: 1.0 almost always) |
| +0x18 | fixed | Pitch (guess: 1.0 almost always); add applies the category's random offset |

Update only changes volume and pitch once a voice is playing. In the race the
game sent about 43,000 updates for 69 adds.

### Microphone (0x40 bytes)

+0x00 vec3 position, +0x18 3x3 orientation matrix, +0x3c reverb preset id
(presets 0-3), +0x3d active.

### 3D

- Volume: full inside the category's near distance (+0x28), falling linearly
  to the far distance (+0x2c), then falling from 0.25 to zero, using slopes at
  +0x34 and +0x38.
- Pan: the sound's position is rotated into the listener's space, then pan is
  -x / distance. Left = volume x (1 - pan), right = volume x (1 + pan), each
  halved. Mono mode uses (left + right) / 4 on both.
- A sound inside the near distance ignores its volume argument.
- Surround: with audio mode 2, a sound more than 0x1000 behind the listener
  has one channel inverted (matrix surround).
- With a second microphone, each channel uses its own (split screen).
- Doppler: from the distance change between frames.

## Handshake with the EE

The EE's handler for SIF command 0 is at 0x193260. Packets are 64 bytes: +0x10
source, +0x14 size, +0x1c kind, +0x1d type, +0x1e slot, +0x20 destination.

Transfer types, EE to IOP: 0 and 2 bank headers, 1 bank body, 3 music chunk,
4 other stream chunk, 5 command block. Kinds: 2 is a transfer, 3 acknowledges one.

**Startup.** After `AUDIO.IRX` starts, the IOP sets SIF registers 0x1e to
0x80000 and 0x1f to 0x100000. The EE waits for both, then sets 0x1e to its
status buffer (0x1fa2dc0 in the log) and 0x1f to 0x2800.

**Acknowledgements are required.** The EE has 8 send slots. Every kind 2
transfer needs a reply: kind 3, with the same slot at +0x1e. Without it the
slot stays busy, and for type 5 the EE spins forever at 0x192f38 before it
sends the next block.

**One status per command block.** After sending a block, the next call to
0x192d20 spins at 0x192d40 until a kind 2, type 5 packet arrives. The EE
ignores a new one while the last is unhandled, so send one per block.

## Status block (IOP to EE, 704 bytes)

| Offset | Meaning |
|---|---|
| +0x00 | u32: number of commands, up to 10 |
| +0x04 | u8: saved by the EE at gp+3040, the first byte of the settings it copies to the next block's +0x04 |
| +0x40 | Commands, 0x40 bytes each |

| Op | EE effect | Meaning |
|---|---|---|
| 0x02 | Calls a registered callback: +4 id, +8 argument; 1 also calls 0x194940 | Event (guess: stream or sound finished) |
| 0x08 | Lowers counter gp+4136 | One music chunk used; the EE may send another (limit 12) |
| 0x09 | Clears gp+4136 and gp+4140 | Music queue reset |
| 0x0a | Lowers counter gp+4148 | One chunk used on a second stream |
| 0x0f | Lowers counter gp+4160 | One chunk used on a third stream (limit 3) |
| 0x10 | Clears gp+4160 and gp+4164 | Third stream reset |
| 0x26 | Stores +1, +2, +3 at gp+3102, 3101, 3100 | Unknown |
| 0x27 | Clears gp+3099 | Unknown |
| 0x29 | Stores +4 at gp+2808 | Unknown (logged value 0x19) |

The EE's own 0x1a9a40 returns 12 minus gp+4136, which suggests a credit scheme
for music. Ops 0x08, 0x02, 0x0a, 0x09 and 0x29 appeared in a logged run, and
most statuses had no commands.

## Stub test

With `LIBSD.IRX` and `AUDIO.IRX` not loaded, a stub replied with the two
startup registers, an acknowledgement for each transfer, one status per block,
and op 0x08 per stream chunk. The game booted and went through the menus into
a race with no sound and no hang. It did not send op 0x02. Race end, movies,
voice-overs and saving have not been tried this way.

## Music

Transfer type 3 carries music, one chunk per stem. Chunk layout:

| Offset | Meaning |
|---|---|
| +0x0c | u32: stems left in this set. Stem number = stems - 1 - this |
| +0x10 | `VAGM` tag (stored reversed) |
| +0x14 | u16: chunk count or stem count (not used) |
| +0x16 | u16: chunk number, returned in op 0x29 on stop |
| +0x40 | 0x2fc0 bytes of left channel ADPCM |
| +0x3000 | 0x2fc0 bytes of right channel ADPCM |

Each chunk lasts about 0.97 s. ADPCM history carries across chunks. Op 0x01's
argument is the stem count: 1 in menus, 3 in races. The race logged played
only stem 2 of 3 (op 0x0d sets its level to 255 and the others to 0).

A stem's voice volume is `level * music volume >> 1`, where 0x4000 is full
scale, then the master volume of 0x3000 (0.75). This gives the 0x28d7 and
0x18e7 values the driver wrote. Op 0x0d sets the target and the step, applied
once per command block.

The driver asks for 22,050 Hz, but the SPU2 pitch register rounds that to
22,043 Hz, so emulated music plays 0.03% slow.

`crates/ps2-iop/src/native.rs` implements this. It is the default;
FREEK_AUDIO=emulated runs the original driver on the emulated IOP and SPU2.

## Sound effects

`crates/ps2-iop/src/sfx.rs` plays one-shots, collisions, cockpit sounds, menu
sounds, big sounds and ambient sounds. Voice gain is a voice register value
over 0x4000, then the master volume of 0.75. In a scripted run the menus
measured 2162 and 918 RMS against 2161 and 930 for the emulated SPU2.

Pause: ops 0x05-0x07 freeze voices and op 0x04 resumes
them, so they must not be stopped. The last menu one-shot keeps playing.

Special ids for one-shot sounds: 0xab plays as 0x39; 0xa0, 0xae and 0x105
and 0xe0 and 0xef are big sounds, one sample on two voices, one per side, with
the second offset by 0x40 bytes; 0x0e, 0x0f and 0x10 are doubled. Ids 0xe0 and
0xef use the attenuation as pitch, so they drop in pitch with distance.

Left out on purpose: the surround mode's phase inversion of sounds behind the
listener, which only suits a Pro Logic decoder.

## Reverb

The driver picks a row of its reverb table (0xd648) by op 0x13 +1, and a
room 0-3 from the microphone record +0x3c (split screen: the lower of the two
microphones). Each room is a libsd effect mode, a depth, and an echo delay and
feedback (0-127):

| Set | Room 0 | Room 1 | Room 2 | Room 3 |
|---|---|---|---|---|
| 0 | echo 0.34, 70, 13 | echo 0.76, 70, 13 | hall 0.76 | studio C 0.58 |
| 1 | echo 0.34, 70, 13 | echo 0.66, 70, 13 | hall 0.58 | hall 0.76 |
| 2 | echo 0.34, 45, 8 | echo 0.75, 45, 8 | hall 0.58 | hall 0.58 |
| 3 | echo 0.47, 30, 13 | echo 1.0, 30, 13 | hall 0.58 | studio C 0.89 |
| 4 | echo 0.47, 41, 18 | echo 0.66, 70, 13 | echo 0.84, 25, 15 | hall 0.64 |
| 5 | echo 0.34, 70, 13 | echo 0.66, 70, 13 | hall 0.58 | hall 0.58 |
| 6 | hall 0.53 | echo 0.66, 70, 13 | hall 0.58 | hall 0.58 |
| 7 | echo 0.34, 70, 13 | echo 0.66, 70, 13 | studio A 0.89 | hall 0.58 |
| 8 | echo 0.34, 33, 13 | echo 0.66, 53, 13 | hall 0.58 | hall 0.58 |
| 9 | off | off | off | off |

Set 9 is used in the menus. The driver writes depth x 0x3fff >> 16 to the
effect volume, with the right side negated; `native` does not copy the
negation, as its reverb is already wide. Voices go to the reverb unless they
were allocated with flag 1: menu sounds, cockpit sounds and voice-overs.
Ops 0x05-0x07 turn it off; resume (op 0x04) turns it back on.

`crates/ps2-iop/src/reverb.rs` is a new reverb, not the SPU2 circuit: an
8-line feedback delay network for the studio and hall rooms (decay 0.7, 1.4
and 2.2 s) and a filtered feedback echo for the echo rooms (delay up to
0.75 s at 127). The decay times and the echo's time scale are chosen by ear
and by the libsd mode names; the original SPU2 presets were not measured.

## Bikes

`crates/ps2-iop/src/sfx/bikes.rs` follows the driver's bike code. Op 0x13 sets up the race:
+2 bike count, +3 players, +4 non-zero stops a local engine playing at double
level, then 4 bytes per bike from +5: sound set, rider (-1 for nobody on this
console), unused, flags (0x40 also counts as local). The driver updates bikes
from the records before the block's commands and runs them after, once per
block (the driver's update runs after each command block).

Nothing plays until sound ids 1-13, 0xe3, 0x143, 0xf5, 0x10 and 0xe4 all have
categories. Ids 1-12 are the engine samples, 0x143 the boost sound.

Each bike's level: 1.0 within 0.9375 units, falling by 0.114 per unit to 3.125,
then from 0.857 by 0.5 per unit. Bikes nobody on this console rides get half.

| Sound | Level | Pitch |
|---|---|---|
| Engine | engine volume x 128, doubled and clamped for a local bike | see Engine above |
| Boost (0x143), on boost press; release stops it | sfx x 128 x 0.675 | doppler x 0.75 |
| Rolling, from the terrain's sound | speed x sfx / 2, x 0.48 unless id 0x84 | doppler |
| Bump, each time +0x24 grows past 0x300-0xa00 (by speed) | 0.5-0.875 random, x 0.48 | 1 - random / 2 |
| Skid, on the rolling sound's category | smoothed skid / 256 x sfx x 128 | 1 + 0.1 x skid / 256, +1/16 when +0x03 |

Skid amount: +0x2a x 16, plus +0x1e x 8 if over 16, 0x80 when +0x03 is set,
up to 0x100, smoothed as 0.3 new + 0.7 old. It starts above 4. Rolling and
bump sound ids per terrain are in the driver's data at 0xd8f4.

The engine state 10 moves to 8 after 30 or 60 blocks of held throttle (the
driver alternates by cycle count).

## Envelopes

ADSR rates: attack and sustain use 7 bits (shift = rate >> 2, step = rate &
3); decay is 4 bits and release 5 bits, and their value is the shift itself.
Getting this wrong made released sounds hang on for minutes.

## Voice-overs

Transfer type 4 carries a mono sample in chunks: +0x04 u32 size including the
0x40-byte header, +0x14 u16 chunk count, +0x16 u16 chunk number from 1, then
ADPCM. Each chunk is padded to 0x40 bytes and answered with status op 0x0a.
Chunks can arrive before and after op 0x1a (+4 stream id) and op 0x1b (play),
so `sfx` waits for the last chunk before it starts the voice.

The voice plays at pitch 0x759 (22,050 Hz) at `announcer volume * 128`, and
other sounds and the music drop to 7/8 meanwhile (music registers went from
0x18e7 to 0x15cb in the trace). A voice named by op 0x17 (+4 stream id, +0xc
position; op 0x18 moves it) is placed in the world at double level, clamped
to 0x3fff: full to 0.927 units, 0.25 at 1.083, silent at 1.25. Out of range
stops it. `native` lifts voice-overs 3 dB: the emulated trace sometimes
keyed one line on two voices half a second apart (145.26 s and 145.73 s, one
play command), which made lines louder than a single voice. When it stops, status op 0x02 goes back
with +4 the stream id and +8 the result: 1 if it ended or was stopped, 0 if
the EE's op 0x02 ended it.

The driver also has a safety timer, which is not built.

## Still to work out

- What the EE's callbacks do with status op 0x02.
- Whether the refill gaps near race start come from the EE or the driver.
- The skid, clutch and terrain code in detail.
- Fields marked (guess), and tone and sound definition records.

## Movie audio

The movie soundtracks are PS-ADPCM at 22050 Hz, mastered hot. `prod1` has
301 samples the decoder clamps, and after conversion to Opus its decoded
audio went over full scale 1933 times; the game decodes movies to 16-bit
(`movie.rs`), so each over was hard-clipped into a crackle.
`tools/movie_declick.py` rebuilds clamped samples and clicks from the
surrounding audio and leaves headroom before encoding. Some clicks remain:
on loud hits the original encoder fell back to unpredicted 4-bit frames
(2.0 s and 21.7 s are the clearest), and those are on the disc.

`flm2mkv` turns any soundtrack peaking above 23000 down to that level, so
every converted movie keeps that headroom.
