# Provenance of the files in `recordings/`

This is the factual record and rights decision for the seven files in this
directory: `fart_dry.wav`, `fart_fuzz.wav`, `fart_delay.wav`,
`fart_chorus.wav`, `fart_chain.wav`, `demo_brownnote.wav` and
`demo_brownnote.m4a`. It records the maintainer's permission and packaging
decision; it is not a legal opinion. Production facts that are not recorded
still say **unknown** below; the rights are no longer unknown.

The maintainer, `drawmeanelephant`, generated and committed all seven files
in one commit and grants permission to use, copy, modify, merge, publish,
distribute, sublicense and sell them under the repository's root MIT
`LICENSE` (Copyright (c) 2026 drawmeanelephant), with its copyright and
permission notice included.

The six files processed through the GPL v2 GuitarPedal bench, or derived
from bench-processed audio, are `fart_fuzz.wav`, `fart_delay.wav`,
`fart_chorus.wav`, `fart_chain.wav`, `demo_brownnote.wav` and
`demo_brownnote.m4a`. The grant covers `drawmeanelephant`'s own contribution
to them; no legal opinion is claimed about whether the GPL v2 GuitarPedal
bench imposes any obligation on that subprocess output.

They are not part of the macOS test artifact, which ships
arithmetic-generated test tones instead. The packager
(`native/scripts/package_macos.py`) refuses to bundle any audio file whose
SHA-256 matches a file in this directory, and refuses to copy a file from
this directory unless this declaration names it.

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
file below. Every file was generated and committed by the maintainer,
`drawmeanelephant`, who grants permission over it under the root MIT
`LICENSE`. The obligations column records the one condition that grant
carries; the six bench-processed files add an open question this record does
not answer.

| File | Generator / revision | Command | Input | Effects and pots | Rights holder | Permission | Obligations |
|---|---|---|---|---|---|---|---|
| `fart_dry.wav` | kujamba, revision unknown | unknown (documented template only) | phrase `kujamba karibu`, seed 42 | none | `drawmeanelephant` | MIT (root `LICENSE`) | Include MIT copyright and permission notice |
| `fart_fuzz.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Rat Sketch` (alias `fuzz`); pots unknown | `drawmeanelephant` | MIT (root `LICENSE`) | Include MIT notice; no legal opinion on GPL v2 GuitarPedal subprocess output |
| `fart_delay.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Tape Echo` (alias `delay`); pots unknown | `drawmeanelephant` | MIT (root `LICENSE`) | Include MIT notice; no legal opinion on GPL v2 GuitarPedal subprocess output |
| `fart_chorus.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Flanger` (alias `chorus`); pots unknown | `drawmeanelephant` | MIT (root `LICENSE`) | Include MIT notice; no legal opinion on GPL v2 GuitarPedal subprocess output |
| `fart_chain.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | `Rat Sketch` → `Tape Echo` → `Flanger`; pots unknown | `drawmeanelephant` | MIT (root `LICENSE`) | Include MIT notice; no legal opinion on GPL v2 GuitarPedal subprocess output |
| `demo_brownnote.wav` | kujamba, revision unknown | unknown | `fart_dry.wav` signal | dry, fuzz, delay, chorus and chain segments in sequence, 10.7 s | `drawmeanelephant` | MIT (root `LICENSE`) | Include MIT notice; no legal opinion on GPL v2 GuitarPedal subprocess output |
| `demo_brownnote.m4a` | kujamba, revision unknown | unknown | `demo_brownnote.wav` | same sequence, AAC-compressed | `drawmeanelephant` | MIT (root `LICENSE`) | Include MIT notice; no legal opinion on GPL v2 GuitarPedal subprocess output |

## Rights

- **Rights holder: `drawmeanelephant`.** The maintainer generated and
  committed all seven files in one commit and states the rights position
  here in his own name. The git author field names the same person; this
  section is the rights statement that field alone is not.
- **Permission: root MIT `LICENSE`.** The maintainer grants permission to
  use, copy, modify, merge, publish, distribute, sublicense and sell all
  seven files under the repository's MIT `LICENSE` (Copyright (c) 2026
  drawmeanelephant). Copies and substantial portions must include that
  copyright and permission notice, which the root `LICENSE` text provides.
- **Obligations: MIT notice, and nothing decided about the bench.** The six
  bench-processed files (`fart_fuzz.wav`, `fart_delay.wav`,
  `fart_chorus.wav`, `fart_chain.wav`, `demo_brownnote.wav`,
  `demo_brownnote.m4a`) passed through the GPL v2 GuitarPedal
  `Validation/bench` subprocess, or derive from audio that did. No legal
  opinion is claimed on whether that subprocess use imposes any GPL v2
  obligation on them. The distribution inventory already excludes the bench
  binary and GuitarPedal source, and this record does not decide the
  question in either direction.

## Maintainer decision

The maintainer, `drawmeanelephant`, records the decision that closes the
previous open questions:

1. **Rights.** He grants MIT (`LICENSE`) permission over all seven files as
   his own contribution, with the notice obligation stated under Rights. No
   other permission, waiver or asset license is asserted.
2. **Packaging.** `recordings/` stays **excluded** from packaged
   `brownnote-native` artifacts, including the unsigned macOS test archive
   and any draft release built from it. Those artifacts ship only the five
   generated arithmetic test tones. This exclusion is a packaging decision,
   not a restriction on the MIT permission above.
3. **Enforcement.** `native/scripts/package_macos.py` keeps the decision
   mechanical: `tone_kit()` generates the five test tones and
   `refuse_recordings()` rejects any bundled audio file whose SHA-256
   matches a file here, while `bundle_recordings()` refuses a `recordings/`
   file this declaration does not name. Packaging fails before an archive is
   written rather than shipping an undeclared sample.

A future recording kit that ships any of these files needs its own reviewed
distribution decision. The MIT grant recorded here permits redistribution;
it does not change what the current packaged artifact contains.
