# brownnote

**[Read the field manual](https://drawmeanelephant.github.io/brownnote/)** ·
[Start here](docs/site/content/getting-started.md) ·
[Flags](docs/site/content/reference/cli.md) ·
[Docs workflow](docs/README.md)

Middleware between [fart-app](https://github.com/drawmeanelephant/fart-app)
and Linus Torvalds' [GuitarPedal](https://github.com/torvalds/GuitarPedal)
effects, feeding into [zcn](https://github.com/drawmeanelephant/zcn)/NINJAM:

```
fart-app | brownnote --effect fuzz | zcn
```

brownnote takes float samples in, runs them through the GuitarPedal test
bench — `Validation/bench`, the pedal's actual audio core compiled as a
host program, unmodified — and writes processed samples out. It
implements no DSP of its own: the bench binary *is* the pedal's signal
path, and brownnote is the plumbing between two programs that were never
introduced. The brown note must flow.

## Build

You need a C compiler, GNU make, and a clone of GuitarPedal (for the
bench). No other dependencies.

```sh
git clone https://github.com/drawmeanelephant/brownnote.git
cd brownnote
git clone https://github.com/torvalds/GuitarPedal.git vendor/GuitarPedal
make -C vendor/GuitarPedal/Validation bench   # builds vendor/.../bench/bench
make                                           # builds build/brownnote
```

`make -C vendor/GuitarPedal/Validation check-effects` proves the bench
itself (and takes a few minutes).

## Usage

```
brownnote [--effect NAME]... [--pot EFFECT:POT=VALUE]...
          [--mix EFFECT=VAL]... [--bench PATH] [--list]
```

| Flag | Meaning |
|---|---|
| `--effect NAME` | route an effect, in the order given (repeatable; chains) |
| `--pot E:POT=V` | set one pot, passed to the bench verbatim (repeatable) |
| `--mix E=V` | set an effect's mix 0..120 (repeatable) |
| `--bench PATH` | bench binary to run (see discovery below) |
| `--list` | list the effects and pots, straight from the bench |

`--effect` accepts aliases (below) and exact GuitarPedal effect names;
anything else goes to the bench's own matcher, which is case-insensitive,
unambiguous-substring friendly, and prints the candidate list when it
cannot resolve a name. The bench is discovered by: `--bench` flag, then
`BROWNNOTE_BENCH` in the environment, then
`vendor/GuitarPedal/Validation/bench/bench` relative to the working
directory or to the brownnote executable.

### Input modes

brownnote peeks at stdin and picks a mode:

- **raw** — float32 little-endian stereo frames at 48 kHz, the bench's
  own wire format, in one end and out the other. This is the native
  mode of the pedal's audio core. The bench reads and writes native
  `float`, so the little-endian layout holds on little-endian hosts
  only; on a big-endian host brownnote refuses raw mode (exit 2)
  rather than emit a stream no little-endian consumer can read. WAV
  mode converts explicitly on both sides of the bench and works on
  either byte order.
- **WAV** — a RIFF/WAVE file (PCM s16le, mono or stereo, any rate
  8–192 kHz). brownnote converts to float32 stereo 48 kHz for the
  bench (Catmull-Rom resampling up, bandlimited Kaiser-windowed
  sinc interpolation down — the way in for rates above 48 kHz, the
  way back for rates below it — so out-of-band energy is strongly
  attenuated rather than folded back; transport plumbing, not an
  effect),
  runs the effects, and writes a WAV back with the *input's*
  rate and channel count. Mono output takes the left channel of the processed
  stereo signal. Sample conversion is `v/32768` in and
  `lroundf(clamp(v,-1,1)*32767)` out — asymmetric on purpose, the
  common convention, bounded by one LSB either way. Results larger
  than the WAV format's 4 GiB limit fail with exit 2 rather than
  wrapping the header's 32-bit size fields.
  WAV mode streams in both directions: input is parsed, converted and
  resampled a block at a time, and the bench's output converts back as
  it is produced, so peak memory is bounded by a few hundred KiB of
  backlog rather than the file's size. Written to a pipe, the output
  header keeps the streaming-WAV convention `0xFFFFFFFF` for the RIFF
  and data sizes (readers that need exact sizes should write to a
  file); written to a regular file the sizes are backfilled at the end,
  so a seekable result is byte-for-byte the same.

The output mode always mirrors the input mode.

### Effect aliases

| Alias | Effect |
|---|---|
| `fuzz`, `distortion` | Rat Sketch |
| `overdrive` | Klonlike |
| `boost` | Boost |
| `wah` | Auto-Wah |
| `compressor` | Compressor |
| `delay`, `echo` | Tape Echo |
| `reverb` | Reverb |
| `chorus`, `flanger` | Flanger |
| `phaser` | Phaser |
| `tremolo` | Tremolo |
| `vibrato` | Vibrato |
| `pitch` | Pitch |
| `eq` | Parametric EQ |
| `cabinet` | Cabinet |
| `amp` | Frenchie Amp |
| `preamp` | Preamp |
| `tone` | Tone 1 |

Exact bench names always win over aliases. `--list` shows the bench's
truth: every routable effect and every pot with its range.

The table is generated from `aliases[]` in `src/brownnote.c` by
`scripts/gen_alias_table.sh` — the suite diffs the two, and checks
every target still appears in the bench's `--list`.

## The pipeline, concretely

fart-app renders WAV files and zcn consumes them, so with those two
binaries built ([fart-app](https://github.com/drawmeanelephant/fart-app),
[zcn](https://github.com/drawmeanelephant/zcn)):

```sh
kujamba render --phrase "kujamba karibu" --out fart.wav
brownnote --effect fuzz < fart.wav > fuzzed.wav
zcn check-wav fuzzed.wav --min-rms 0.01
```

A fart goes in; a fuzzed fart comes out; zcn checks the result. For a
live room, `kujamba` and `zcn` both speak NINJAM to a server, and
brownnote's raw float32 mode is the pipe between a synth that speaks it
and a client that feeds it.

## Recordings

`recordings/` holds the evidence: the dry fart, and the same fart
through fuzz (Rat Sketch), delay (Tape Echo), chorus (Flanger), and the
three-effect chain, plus a demonstration track with all five in
sequence. `demo_brownnote.wav` / `.m4a` — a fart goes in, a distorted,
delayed, chorused fart comes out. These were rendered before the
resampler's anti-aliasing filter (#32): re-rendering them today
would produce slightly different output.

## Presets & Studio Sequencer

`scripts/presets.sh` renders signature single-shot pedal chains (`doom`,
`cathedral`, `machinegun`, `swamp`, `klon-crunch`, `cyber-laser`, `dry`)
and full multi-track stereo genre arrangements (`funk`, `metal`, `doom`,
`techno`) to WAV or 320 kbps MP3:

```sh
scripts/presets.sh list
scripts/presets.sh fx cathedral --phrase "harufu mbaya angani" --play
scripts/presets.sh song funk --bpm 116 --bars 8 --out funk.mp3 --play
make presets   # renders all FX presets + genre songs into build/presets/
```

`scripts/studio.py` (`make studio`) serves an interactive browser studio
on `http://127.0.0.1:8765/` — four pages over the same six stems, linked
by a nav bar:

- `/` **SEQ+MIX** — 16-step sequencer, 6-channel stem mixer (per-channel
  volume, stereo pan, pitch, dry/wet A/B, mute/solo), custom
  phrase/pedal renderer, and MP3 exporter.
- `/mixer` **DESK** — console-desk skin: vertical channel strips with VU
  meters, gain/pan/pitch, mute/solo/dry, per-channel TAP and LOOP, plus
  a master bus with scope and panic button.
- `/pads` **MPC** — 16 finger-drum pads (keys `1-4` / `QWER` / `ASDF` /
  `ZXCV`); click height sets velocity and same-stem hits choke the tail.
- `/library` **CRATES** — every rendered MP3/WAV in `build/presets/` and
  `recordings/` as a card with a drawn waveform, playhead, and kind
  filters.

## Testing

```sh
make test
# or, explicitly:
BENCH=vendor/GuitarPedal/Validation/bench/bench tests/run_tests.sh
```

`make test` builds and runs the unit tests for the WAV parser, the
resampler and the sample conversions, then the suite below
(`make unit` runs just the unit tests; `tests/run_tests.sh` alone
is the suite, without them). `make fuzz` runs the WAV parser
fuzz harness over `tests/corpus/` plus a round of deterministic
mutations under ASan/UBSan; `make sanitize` rebuilds everything
sanitized and runs the whole battery. With clang/libFuzzer,
`make fuzz-libfuzzer` fuzzes the same target properly.
`tests/gen_corpus.py` regenerates the seed corpus.

The suite covers: build; raw-mode plumbing identity against `/bin/cat`
(byte-identical); fuzz through the real bench (output changes, sane
RMS); unknown-effect error propagation (the bench's candidate list, and
its exit code); chaining; SIGPIPE against a closed downstream reader;
WAV passthrough (header and level preserved); WAV fuzz; `--list`;
the streaming WAV header (placeholder sizes on a pipe, backfilled on a
file); a 30 MB WAV streamed through a pipe under a small address space;
alias-table drift (README vs `aliases[]`, and every target vs the
bench's `--list`);
and, when the Zig tools happen to be around, the full
kujamba → brownnote → zcn round trip. Real-bench and round-trip tests
skip cleanly (and the suite still passes) when their binaries are
absent, which is what CI relies on.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | brownnote usage or format error (bad flag, unsupported WAV) |
| other | the bench's exit status, propagated (unknown effect = 1) |
| 141 | downstream closed the pipe (128 + SIGPIPE) |

## License

MIT — see [LICENSE](LICENSE).
