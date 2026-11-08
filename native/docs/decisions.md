# Native roadmap decisions

These are the product calls the roadmap issues left open. Each one is a
default: implementation lanes build against it rather than deciding it
again. To change one, edit this file in its own PR and note the change in
the planning index ([#94](https://github.com/drawmeanelephant/brownnote/issues/94)).

An entry marked **Revisit** names the evidence that should reopen it.

## Platform and hardware

- **D1. Platform.** M1 to M3 target macOS on Apple silicon only. Linux and
  Windows remain compile-only until someone can test audio on them.
- **D2. Available hardware.** No MIDI controller, audio interface, or
  external synth is available. The only device is built-in Mac audio.
  - Software paths use substitutes: the macOS IAC Driver for virtual MIDI
    input, and switching between built-in outputs (speakers, headphones,
    AirPlay or Bluetooth) for device-loss tests.
  - A close gate that names real hardware stays open and carries the
    `needs-hardware` label. A virtual substitute never closes it.
  - M5 (#115, #116) and the unusual-hardware research (#117) are parked
    until gear exists.

## Projects (#98, #99, #100)

- **D3. Container.** A project is a directory: one versioned JSON document
  plus a `media/` folder. Importing a sample always copies it into the
  project. Projects never reference files outside their own directory.
  Media files are named by content hash, so two pads that share a sound
  share one file.
- **D4. Retained audio.** Any rendered sound the project needs to play is
  project media, not cache. That includes pedal, plugin, and instrument
  renders that might not regenerate identically. The render cache stays
  evictable and is never the only copy.
- **D5. When a save is done.** A save reports success only after the new
  document and every media file have been written and synced
  (`File::sync_all`, plus the containing directory), and the switch to the
  new version has been committed. The previous complete version is kept
  until the next save succeeds. A failed save never touches it.
- **D6. Autosave.** Autosave writes a separate recovery copy every 60
  seconds while there are unsaved changes, using the same storage API as a
  normal save. It never overwrites the user's saved version. On open, a
  recovery copy newer than the saved version is offered, not applied
  silently.
- **D7. First import scope.** Import supports WAV only, within the limits
  the native decoder already has: PCM16 or float32, mono or stereo,
  8–192 kHz, up to 30 seconds. File selection uses a typed path or
  drag-and-drop, whichever the pinned Rune build supports. A native open
  dialog is not promised.
- **D8. Machine versus project settings.** These belong to the machine:
  audio output device, MIDI device selection, cache location, and window
  layout. These belong to the project: pad samples and settings, pedal
  chains, MIDI mappings, tempo, takes, edits, and instrument
  configuration. MIDI mappings are keyed by channel and note or controller
  number. A device name is stored only as a hint for reconnecting.

## Performance time (#104, #106, #107)

- **D9. Stored time.** An event's original time is an integer offset from
  the start of its take, in a unit that does not depend on sample rate or
  buffer size. #104 picks the unit (nanoseconds is fine). Original times
  are never rewritten. Quantized and edited positions are derived from
  them.
- **D10. Tempo.** A take records at the project tempo, and the tempo is
  locked while any take is armed or recording. Changing the tempo later
  keeps each event's musical position, so loops stay in bars, as in a
  hardware looper. Tempo automation and tempo changes during a take are
  out of scope.
- **D11. Loops.** Loops are 1, 2, 4, or 8 bars of 4/4. The default tempo
  is 116 BPM, the same as the browser studio. The loop length is chosen
  before the first take. Shortening it hides events outside the loop but
  never deletes them.
- **D12. Recording.** A new pass overdubs by default. Replace is an
  explicit choice for each pass. The count-in is off by default and can be
  set to one bar. The metronome is off by default.
- **D13. Quantization.** Quantization is off by default and set per take,
  with a 1/16 grid at full strength. Turning it off restores every
  original time exactly.
- **D14. Same-pitch overlaps.** Every note-on gets its own event ID. A
  note-off ends the oldest sounding note of that pitch on that channel
  (first in, first out).
- **D15. Undo.** Undo history keeps the last 100 steps and does not
  survive quitting. Only the current edited state is saved. One continuous
  drag counts as one undo step.

## Audio engine (#101, #102)

- **D16. Device changes.** Changing the output device or sample rate stops
  the voices that are playing and reports that it did. Device loss
  switches to the silent mixer and offers a retry with the remembered
  device or a fall back to the system default.
- **D17. Overload meter.** The meter shows the peak measured before the
  final clamp. A clip indicator stays lit for 1.5 seconds, matching
  `CLIP_LATCH` in the showcase. A limiter is not part of M1.

## Instruments, plugins, and handoff (#95–#97, #108–#114)

- **D18. First instrument.** Prefer a sampled-instrument backend written
  in Rust and licensed permissively (an SF2 or SFZ player) over a CLAP
  sampler, so that M3 does not depend on the CLAP host research (#96).
  The reference instrument may be supplied by the user rather than
  bundled. **Revisit** if #108's backend spike finds no usable candidate.
- **D19. SoundCraft.** #95 runs early as a go/no-go check, because it
  tests the riskiest assumption in M3. Expect to pin a newer SoundCraft
  release again before #110 starts. If #95 returns no-go, M3's target
  becomes the DAW-neutral bundle from #109, checked in one other open
  DAW. **Revisit** at the #95 result.
- **D20. Plugin research timing.** CLAP (#96) and LADSPA (#97) research
  waits until M1 lands, unless D18 is reversed in favor of a CLAP sampler.
  LADSPA stays a goal and is not deferred silently.

## How lanes plug into the app

- A feature's UI goes in its own `native/src/ui/<feature>.rs`, following
  the pattern in `ui/pedals`: `poll(&mut State) -> bool`, view fragments
  that take `&State`, and test scenes.
- Device recovery lives in `ui/audio.rs`, and layout lives in
  `ui/view.rs`. Shared widgets (`ink`, `caption`, `accent`, `button`) stay
  in `ui.rs`.
- Feature lanes do not edit `ui.rs`, `ui/view.rs`, `main.rs`, `lib.rs`,
  `Cargo.toml`, or `Cargo.lock`. The integrator agent adds the new
  `State` field, the call from `poll`, the view slot, the module
  declaration, or the dependency, one PR at a time.
