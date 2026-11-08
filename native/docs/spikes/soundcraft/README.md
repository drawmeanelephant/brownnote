# SoundCraft editable-session research

## Decision at 8 November 2026

**Bounded go for an editable handoff, not sound-equivalent sampler playback.**
The actual v0.18.0 macOS GUI imports separate overlapping audio hits and editable
MIDI notes. Edited clips and notes survive app quit, reopen, and relocation after
the original sources and the original session directory are removed.

This is research for [#95](https://github.com/drawmeanelephant/brownnote/issues/95),
not an exporter or a production adapter. It relies on D19 and does not change
Brownnote's project format. No integrator wiring or dependencies are needed.
The issue remains referenced rather than automatically closed.

The viable path uses supported commands, with separate audio tracks for
overlapping hits. MIDI keeps editability. Retained per-hit audio keeps sounds
that the target cannot reproduce. A stem alone does not satisfy this result.
Track reuse or playlist packing beyond this small fixture is unproven.

## Pin and evidence

The GitHub latest-release endpoint returned **v0.18.0**, published
6 November 2026, at full commit
`5ff012ab88490cdfc635ee304c1019885e1b880d`. The older issue pin was checked, not
assumed current. The tag ref resolves directly to that commit.

- [pin.json](pin.json) contains the downloaded DMG, CLI ZIP, checksum-file,
  and extracted executable SHA-256 values.
- [evidence/release.json](evidence/release.json) records macOS 27.3, arm64,
  universal binary architectures, and the actual app and CLI version output.
- [evidence/commands.json](evidence/commands.json) is the complete released
  `soundcraft-cli commands --json` output. Its enabled flags describe an empty
  session, not the app's current session.
- [evidence/app.json](evidence/app.json) contains ordered CLI requests, replies,
  exit codes, independent reader checks, and logs from the GUI instances.
- [REPRODUCE.md](REPRODUCE.md) contains the repeat and pre-#110 recheck procedure.
- [VALIDATION.md](VALIDATION.md) records repository checks and research limits.

The process is `SoundCraft.app/Contents/MacOS/SoundCraft --no-audio --control 0`,
with `SOUNDCRAFT_NO_PREFS=1` and a fresh `SOUNDCRAFT_CONFIG_DIR`.
`ui.key` waits for a GUI input pass. `ui.inspect` then reports a nonempty
timeline layout, no audio device, and stopped transport. This is not CLI
`run`, MCP, an offscreen renderer, a screenshot, or a listening test.

Only synthetic files enter the app. Downloads, profiles, and generated
sessions stay in `.work/`. Every app instance started by the probe exits.
Published transcripts replace local paths and ephemeral ports with tokens.
No executables, private session JSON, user media, or device identifiers are
redistributed. Checksums match both the release API and `SHA256SUMS.txt`;
the release's detached signing-key signature was not verified.

## Go/no-go matrix

| Requirement | Observed result | Bound or fallback |
|---|---|---|
| Independent input check | PASS. `wave` reads PCM16 WAV headers, lengths, and impulse positions. `mido` reads format 1, 960 PPQ, 120 BPM, five notes, channels, velocities, and FIFO durations. | Readers do not import the generator's codec. Mutation tests reject shifted transients, wrong velocity, and wrong duration even after updating hashes. |
| Separate overlapping hit clips | PASS. Two complete 48,000-frame impulses occupy separate tracks at 0 and 6,000 samples before edits. | One track per overlapping hit is proven. No large-track-count claim. |
| Same-track overlapping import | NO. Import at 6,000 samples does not retain the full first 48,000-frame clip. | Separate tracks, or a later proven nonoverlapping track allocation. Do not rely on destructive same-track import. |
| Audio move and delete | PASS. `edit.move_clips` moves the first hit to 12,000. `edit.select` plus `edit.clear` removes a disposable clip at 96,000. | Other complete overlapping clips remain present after reopen. |
| Editable MIDI import | PASS. `file.import_midi` at 48,000 samples imports two instrument tracks with all five expected notes. | One source MIDI part is split into two content tracks to isolate the overlap case. No stable Brownnote event identity is implied by SMF. |
| Note edits | PASS. `midi.note_edit` changes the middle distinct note to pitch 65, tick 600, length 360, velocity 88. | `midi.notes` matches before and after both reopens. Other note channels, durations, and velocities remain unchanged. |
| Same-channel, same-pitch overlap | PASS for import and session persistence. Notes at ticks 0 and 240 retain lengths 960 and 1,200. | NO for target SMF round-trip fidelity. Its export trims the first duration to 240. This is not a DAW-to-Brownnote import promise. |
| Tempo and time conversion | PASS. Import replaces the scratch session's 116 BPM with the fixture's 120 BPM. MIDI clip starts remain at sample 48,000. | At this pin and fixture tempo, 960 ticks = 24,000 samples. No tempo automation or non-960-PPQ fixture claim. |
| 44.1-to-48 kHz import | PASS. One second becomes 48,000 frames for audio and sampler loading. | An independent libsndfile reader checks saved 48 kHz media, lengths, and 441 positive tone crossings. This does not establish resampler transparency. |
| Sampler mapping | PASS for one sample per instrument track, root/key range, and key tracking. An out-of-range note bounces to silence. | No multisample kit or cross-track pad mapping. Separate tracks per sound, or retain rendered hit audio. |
| One-shot versus gate | PASS for the synthetic tone. With a 60-tick note, one-shot still has signal at 0.2–0.4 seconds. Gate is exactly silent there. | `play_mode=1` ignores note release. This is an offline bounce from the GUI, not a claim about listening or physical output. |
| Playback rate and pitch | PASS for an octave-up sampler tone. The 0.1–0.4 second window has 265 positive crossings versus 133 at root. | Target uses cubic interpolation and an envelope. Brownnote uses linear interpolation. Bake per-hit pitch/rate when exact sound matters. The baked-octave fixture demonstrates import, not a production renderer. |
| Choke | NO equivalent sampler parameter. `choke_group` is rejected. Source lists all 19 parameters without a choke group. | Bake the required truncation/fade into each retained hit. The 50 ms baked-choke fixture proves short-asset import only, not Brownnote's general 5 ms choke algorithm. |
| Gated same-pitch sampler playback | NO claim of faithful stacking. Pinned source releases earlier held voices on retrigger and releases held voices by pitch. | Preserve note data, but use separate instrument tracks or baked hits for faithful overlapping sound. One-shot is not a substitute for every gated instrument. |
| Save, quit, reopen | PASS. `session.save_as`, actual process exit, fresh GUI process, and `session.open` return `missing == []`. | Tracks, clip positions, MIDI notes, tempo, and loaded media match. |
| Relocate and remove original sources | PASS. The whole session directory is copied. Both original directories are deleted before a fresh app opens the copy. | Every source is loaded from a relative `Audio Files/` path. Moving `.scraft` alone was not tested or promised. |
| Missing-media negative control | PASS. After removing the relocated synthetic impulse WAV, a fresh app reports missing media and unhealthy audio. | `session.audio_health` checks loaded availability and bounds, not disk integrity, signal, routing, or plugin sound. |
| Real-device output and listening | NOT RUN. All app runs use `--no-audio`. | Maintainer checks remain. No assertion that an agent heard audio. |

## Retained-audio boundaries

Brownnote's per-hit pitch/rate, gain and velocity law, pan law, pedal/plugin
tails, choke fades, and the chosen instrument's sound require retained audio
where target settings cannot reproduce them exactly. Target sampler parameters
can remain editable as an approximation, never as the sole copy of that sound.
Master processing that couples simultaneous hits may need an additional
reference render, without replacing the required per-hit clips and notes.

Only the octave and short-choke synthetic assets are exercised here.
There is no proof of pedal/plugin parity, external instrument playback,
general instruments, or exact native-to-target audio equivalence.
#109 owns the future DAW-neutral bundle and retained-media semantics.
#110 owns any target adapter. Neither is implemented here.

## Authoritative sources at the pin

All source links use the same full commit as the downloaded release:

- [CLI flags and command schema](https://github.com/storytold/soundcraft/blob/5ff012ab88490cdfc635ee304c1019885e1b880d/docs/cli.md)
- [GUI control protocol and isolated profiles](https://github.com/storytold/soundcraft/blob/5ff012ab88490cdfc635ee304c1019885e1b880d/docs/control-protocol.md)
- [File imports, saves, and exports](https://github.com/storytold/soundcraft/blob/5ff012ab88490cdfc635ee304c1019885e1b880d/crates/engine/src/cmd/file.rs)
- [MIDI track import and sample/tick positions](https://github.com/storytold/soundcraft/blob/5ff012ab88490cdfc635ee304c1019885e1b880d/crates/engine/src/io/midi.rs)
- [FIFO SMF reader and overlap-trimming SMF writer](https://github.com/storytold/soundcraft/blob/5ff012ab88490cdfc635ee304c1019885e1b880d/crates/midi/src/smf.rs)
- [Sampler source loading and save ownership](https://github.com/storytold/soundcraft/blob/5ff012ab88490cdfc635ee304c1019885e1b880d/crates/engine/src/cmd/sampler.rs)
- [Sampler parameters, cubic interpolation, gate, and one-shot behavior](https://github.com/storytold/soundcraft/blob/5ff012ab88490cdfc635ee304c1019885e1b880d/crates/dsp/src/plugins/sampler.rs)

SoundCraft declares `MIT OR Apache-2.0`. The captured command catalog includes
its MIT notice in [evidence/SOUNDCRAFT-LICENSE-MIT](evidence/SOUNDCRAFT-LICENSE-MIT).
Spike scripts use Brownnote's MIT license. The synthetic WAV and MIDI fixtures
are dedicated to the public domain under CC0-1.0, with no third-party recordings.
