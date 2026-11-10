# Provenance of the files in `recordings/`

This is a factual record for the seven files in this directory, not a license
grant and not legal advice. It states what the repository records, and says
**unknown** where it does not. The rights holder, the permission to
distribute and any obligations are **not** inferred here; the maintainer
records that decision.

No distribution permission is declared for these files. They are not part
of the macOS test artifact, which ships arithmetic-generated test tones
instead. The packager (`native/scripts/package_macos.py`) refuses to bundle
any audio file whose SHA-256 matches a file in this directory, and refuses
to copy a file from this directory unless this declaration names it.

## Identity

All seven files were added in one commit,
`e63941c57fbf183627d6dea0e79ea729bf8b94be` (11 October 2026), whose message
records how they were produced. Their formats and SHA-256 digests:

| File | Format | SHA-256 |
|---|---|---|
| `fart_dry.wav` | 44.1 kHz mono PCM16, 1.800 s | `45dae31a82c2f6eaec19afbdb043797f16b4e5c43aca831af2dffd7a544d0173` |
| `fart_fuzz.wav` | 44.1 kHz mono PCM16, 1.800 s | `a1f6a6355b7e34a38c16936e1d2b05a3f022f0ecd41c08c67287dd86b4fe2b15` |
| `fart_delay.wav` | 44.1 kHz mono PCM16, 1.800 s | `504d772e1f48199114e8df3e23284fa7a9fa9a2bd074649581042581cf03aba1` |
| `fart_chorus.wav` | 44.1 kHz mono PCM16, 1.800 s | `df5cdf2e083a2feec8362679ebeb0e19ea7f8782359523789d5f2fb3bb58c06e` |
| `fart_chain.wav` | 44.1 kHz mono PCM16, 1.800 s | `29af3eea5e7c5cd02f23bc0ed4afd43b066b311482a66b1fb68c056e9b1c6f35` |
| `demo_brownnote.wav` | 44.1 kHz mono PCM16, 10.749887 s | `8506538ab903843720e2b5887de943731a7c094b84690adaadfffd76c240732e` |
| `demo_brownnote.m4a` | 44.1 kHz mono AAC in M4A, 10.749887 s, ~116.5 kbit/s | `eab65e4751ffa93fb68c05a727c81624637bcf10d6413e3ba3d7359b0de1f45a` |

## Common pipeline facts

- **Generator:** `kujamba`, the renderer from the
  [fart-app](https://github.com/drawmeanelephant/fart-app) project.
  **Version or commit: unknown.** No fart-app, kujamba or zcn revision is
  pinned in this repository. `scripts/presets.sh` looks for a local
  `../fart-app` build, so the revision used was whatever checkout was
  present; that checkout is not recorded.
- **Requested phrase:** `kujamba karibu`, with seed 42 (commit message).
- **Exact command line: unknown.** The documented pipeline command is
  `kujamba render --phrase "kujamba karibu" --out fart.wav`
  (`README.md`, `docs/site/content/guides/pipeline.md`), but the actual
  invocation, its output paths and any `--voice` argument were not recorded.
- **Input recording:** the kujamba render of that phrase. The intermediate
  dry file was not kept as a separate artifact; `fart_dry.wav` is that
  signal, and the four processed WAVs are derivations of it (README: "the
  same fart through fuzz ... delay ... chorus ... the three-effect chain").
- **Pedal host:** this repository's C `brownnote` CLI, at commit
  `e63941c` or a working tree near it. **The exact working-tree state is
  unknown**; the recordings were not regenerated or checked after later
  changes.
- **Effects engine:** the GuitarPedal `Validation/bench` binary, built from
  an unmodified shallow `torvalds/GuitarPedal` clone. **GuitarPedal commit
  and bench binary: unknown.** No vendored copy is in the repository.
- **Pot and mix settings: unknown.** No `--pot EFFECT:POT=VALUE` or
  `--mix EFFECT=VALUE` arguments used for these renders are recorded.
- **Verification:** `zcn check-wav` (revision unknown) accepted all six WAVs
  at 44.1 kHz mono s16. Reported RMS / peak from PR #1:

  | File | RMS | Peak |
  |---|---|---|
  | `fart_dry.wav` | 0.127375 | 0.671570 |
  | `fart_fuzz.wav` | 0.044605 | 0.352264 |
  | `fart_delay.wav` | 0.144425 | 0.689697 |
  | `fart_chorus.wav` | 0.091816 | 0.560913 |
  | `fart_chain.wav` | 0.040267 | 0.326691 |
  | `demo_brownnote.wav` | 0.090695 | 0.689697 |

- **Renderer revision:** these files predate the anti-aliasing change to the
  resampler (#32). Re-rendering with the current code would produce
  different bytes; these files are not reproducible from the repository as
  it stands.
- **M4A encoder and settings: unknown.** Only the resulting AAC stream is
  recorded (`demo_brownnote.m4a`, derived from `demo_brownnote.wav`).

## Per-file record

The generator, command, input and verification facts above apply to every
file below. The rights holder, permission and obligations columns are
unknown for every file; the repository states no asset grant and no
individual file carries a notice.

| File | Generator / revision | Command | Input | Effects and pots | Rights holder | Permission | Obligations |
|---|---|---|---|---|---|---|---|
| `fart_dry.wav` | kujamba, revision unknown | unknown (documented template only) | phrase `kujamba karibu`, seed 42 | none | unknown | unknown | unknown |
| `fart_fuzz.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Rat Sketch` (alias `fuzz`); pots unknown | unknown | unknown | unknown |
| `fart_delay.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Tape Echo` (alias `delay`); pots unknown | unknown | unknown | unknown |
| `fart_chorus.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Flanger` (alias `chorus`); pots unknown | unknown | unknown | unknown |
| `fart_chain.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Rat Sketch` → `Tape Echo` → `Flanger`; pots unknown | unknown | unknown | unknown |
| `demo_brownnote.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | dry, fuzz, delay, chorus and chain segments in sequence, 10.7 s | unknown | unknown | unknown |
| `demo_brownnote.m4a` | kujamba, revision unknown | unknown | `demo_brownnote.wav` | same sequence, AAC-compressed | unknown | unknown | unknown |

## Rights

- **Rights holder: unknown.** The repository's root MIT `LICENSE` covers
  source code. Nothing in the repository states who holds rights in these
  recordings, and there is no license or notice file inside `recordings/`.
  The commit author (`drawmeanelephant`) is git attribution metadata, not a
  rights statement.
- **Permission relied on: none declared.** No grant, waiver, release or
  asset license exists in the repository. The README description of how the
  files were rendered is context, not permission.
- **Obligations: undecided.** Whether the GPL v2 GuitarPedal bench imposes
  any obligation on rendered output is not evaluated here and is not a legal
  opinion. The distribution inventory already excludes the bench binary and
  GuitarPedal source.

## Maintainer decision (open)

The repository does not answer this, so it is left explicit rather than
guessed:

1. Who holds the rights in these seven files, and under what terms may they
   be redistributed?
2. May a future recording kit ship any of them, and with what attribution or
   other obligations?

Until the maintainer records an answer, `recordings/` stays excluded from
distributed artifacts. The packaging boundary check enforces the exclusion
without deciding the rights question.
