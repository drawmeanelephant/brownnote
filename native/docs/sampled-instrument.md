# Sampled instrument (D18, #108)

The app mounts one SF2 preset, played by RustySynth 1.3.8 (MIT, pinned
`=1.3.8`). This is the smallest complete path for held MIDI notes. It is not
a general SF2/SFZ player, and it adds no CLAP or other synthesis.

## What works

- **Load.** The INSTRUMENT panel takes an absolute `.sf2` path. A worker
  thread reads at most 64 MiB, prescans the RIFF chunks, and parses the file
  inside `catch_unwind`. The first preset (or the saved bank/program) must
  exist; RustySynth's silent fallback to a default preset is refused. The
  exact file bytes become project media.
- **Live notes.** MIDI notes mapped to a held instrument
  (`WHOLE CHANNEL → INSTRUMENT` in the MIDI panel) start and end by their own
  event IDs (D14). RustySynth releases every voice on one channel and pitch,
  so the player gives each overlapping same-pitch note its own channel (15
  melodic channels). A release then ends only that note, oldest first. A
  sixteenth same-pitch overlap, or more than 128 held notes, steals the
  oldest. Voices are capped at 64.
- **Callback.** The player is built off the callback at the output's rate and
  sent as a command. It renders 32-frame blocks, so a live onset can trail by
  under 32 frames. Mount, notes, releases, panic, remount and unmount neither
  allocate nor free on the callback (`tests/rt_alloc.rs`). A replaced player
  leaves by the return queue. Each new output stream mounts a fresh player.
- **Audition.** AUDITION C4 plays one 0.6 s middle C through the mounted
  player.
- **Takes.** Recorded held notes keep their real lengths. Replay schedules
  each note's start and end through the frame scheduler.
- **Save.** The project document gets an `instrument` entry: identity, name,
  source asset, bank/program, gain, backend and render rate. No voices,
  channels or device rates are stored. Saving renders each distinct
  (pitch, velocity, gate) of the current effective notes once, offline, at
  48 kHz with reverb and chorus off. Each render is kept as project media (D4)
  and reused on later saves.
- **Reopen.** The saved source is prepared again. If this build cannot play it
  (for example, the saved preset is missing), the project still opens. The
  status reads `SILENT` with the reason, notes and renders are kept, nothing
  pretends to sound, and transport reports held notes as not sounded.
- **Export (#109).** Each instrument note with a retained render exports as
  that audio, with the MIDI note. Notes without one fall back to the existing
  "editable MIDI only" loss record. The SF2 itself is not exported.

## Evidence and its limits

- `tests/instrument.rs`: preparation limits and malformed files, same-pitch
  FIFO, replayed IDs, velocity, panic, deterministic renders, and a player
  callback with steals that allocates nothing.
- `ui/instrument/tests.rs`: the panel, the MIDI fixture input, the transport,
  project save/open and the export adapter, through the app's own entry
  points.
- The only SF2 used is the generated MIT mechanics fixture from
  `docs/spikes/sampled-instrument/fixtures/mechanics.sf2`: one triangle-wave
  preset. It proves mechanics, not sound quality.
- Missing: a real reference SF2, listening checks, latency on hardware, and
  a named physical MIDI controller. None of these were run. The #105 hardware
  gate is still open.
