---
title: Roadmap
parent: index
status: published
summary: A native-first future for brownnote, with portable projects, faithful loops, real instruments, open plugins, and editable SoundCraft sessions.
---

<p class="eyebrow">Native instrument / public roadmap</p>

# The brownnote roadmap {#direction .hero-heading}

brownnote started as plumbing between a fart synth and a guitar pedal.
The next goal is a native instrument for playing pads, shaping sounds,
and recording loops that you can keep working on in a digital audio
workstation, or DAW.

The personality stays. The work ahead is to make the instrument dependable,
expand what you can play, and preserve the music when you leave brownnote.

> This is a proposed roadmap, not a promise of release dates. The
> milestones below are plans; **What exists today** lists only work that
> has landed and cites the issue that delivered it. Integration choices
> need working demonstrations before we claim compatibility.
>
> Last reviewed: 10 November 2026.

## What exists today

The C command-line tool processes WAV or raw audio through the real
[GuitarPedal](https://github.com/torvalds/GuitarPedal) bench.

The native Rust and Rune app runs on macOS on Apple silicon with built-in
audio. It can:

- Import WAV samples into the sixteen pads and shape each pad with gain,
  pan, pitch, and an ordered GuitarPedal pedal chain (#80–#85, #99).
- Keep that kit in a portable project directory. Samples, pad settings,
  chains, takes, edits, MIDI mappings, and instrument configuration are
  project data, and SAVE and OPEN move the whole set between directories
  and machines (#98, #100).
- Validate the document, its settings, and every media hash before OPEN
  replaces the active kit. A failed save leaves the previous generation
  intact and usable, and a corrupt original is not overwritten (#100).
- Record takes against a 1, 2, 4, or 8 bar loop, replay and overdub them,
  and mute or remove them (#106).
- Quantize a take to sixteenth notes, nudge or delete a hit, change its
  velocity, and undo. Turning quantization off restores the original
  timing (#107).
- Learn MIDI notes and continuous controllers, assign them to pads and
  controls, and keep the mappings with the project (#145, #148).
- Play one sampled SF2 instrument from MIDI with held notes, record its
  notes, and keep a frozen render of each distinct note as project media
  (#108).
- Host one live CLAP instrument with saved parameters and opaque state,
  recall it when the project opens, and export the notes it recorded
  (#112, #171).
- Render pad sounds offline through CLAP effects and through the selected
  LADSPA effect as pad pedal stages, retaining the wet audio (#113, #114).
- Export a DAW-neutral bundle of clips, MIDI notes, retained audio, and a
  manifest (#109), then turn that bundle into an editable session in a
  pinned SoundCraft build with the `brownnote-soundcraft` tool (#110,
  #111).

Project data survives a quit only through an explicit SAVE and a later
OPEN. Unsaved changes are not recovered, and autosave is not implemented
(#100).

These paths are proven by automated tests and a packaged smoke run that
uses a silent mixer. Audible output, physical device loss, and a named
MIDI controller are still unproven and appear under **Still open**.

The [[guides/studio|browser studio]] has sequencing, mixing, layered loop
recording, Web MIDI pad input, microphone recording, saved patches, and
mix export. It remains useful and is a reference for the workflows the
native app brings across, but it is not a second studio to grow in
parallel. Native is the main product direction, the goal is one native
music workflow, and the existing command-line pipeline does not need a
rewrite.

## Still open

- Audio device selection and recovery from device loss (#101).
- Mix overload reporting and click-resistant voice stealing (#102).
- A named physical MIDI controller. Learn and input have run through a
  virtual MIDI port only (#105).
- MIDI clock output to external instruments (#115) and recording the
  audio they return (#116).
- One unusual controller and the Open Instrument assessment (#117),
  parked until that hardware is available.

## The goals

### G1. Your jam survives {#g1-projects}

A project keeps its samples, pad assignments, pedal settings, and recorded
takes together. It has a versioned format and portable asset references.
Browser storage, temporary render caches, and paths into a developer's
checkout are not the project.

**Done when:** save a jam with a custom sample and several takes. Quit,
then reopen it without a browser or server. Copy the project to another
machine and hear the same performance. Missing assets produce a useful
message rather than an empty or silently changed project.

### G2. Keep what you played {#g2-performance}

The native looper preserves original hit timing, separate overlapping
events, velocity, and instrument note duration. Quantization is reversible.
Takes support overdub, undo, mute, and removal. Small edits let you nudge
or delete an event and change its velocity.

**Done when:** record a flam and overlapping hits on the same pad, then a
held instrument note. Save and reopen the take. Turning quantization off
restores the original timing, and no hit disappears during export.

This is loop and take editing, not a general arrangement timeline or
waveform editor.

### G3. Play with your gear {#g3-midi}

The first stage supports MIDI controllers, saved learn mappings, velocity,
and note-off and sustain where the instrument needs them. Device reconnect
and panic are part of normal use.

A later stage adds MIDI output to external instruments, clock
synchronization, and recording their returning audio. Those features need
their own timing and audio-input checks.

**Done when:** connect a controller, learn a mapping, record a part, and
reopen it with the mapping intact. Unplugging the controller does not leave
stuck notes. For the later stage, drive an external synth and keep both
its editable notes and its recorded audio.

### G4. Real instruments alongside the pads {#g4-instruments}

Use an existing sampled-instrument backend and host a CLAP instrument.
Musical parts need live note response, releases, and saved instrument
settings. Rendering a fixed sound in advance is useful for pads, but does
not satisfy live instrument play.

**Done when:** load a sampled instrument and a CLAP instrument, play each
from a keyboard, record a part, save, and reopen with its sound restored.
The export keeps the notes and includes rendered audio as a fallback.

We are not starting a new synthesis engine or promising every sample
format. The first backend needs a small compatibility test and a review
of its distribution and sample licenses.

### G5. Open plugin pedals {#g5-plugins}

[LADSPA](https://www.ladspa.org/) starts with offline effects for a pad.
Controls come from the plugin's audio and control-port descriptions.
You adjust the effect, audition the rendered result, and keep playing.
LADSPA is not the MIDI instrument path.

[CLAP](https://github.com/free-audio/clap) adds effects and the live
instruments in G4, with saved plugin state. Hosting other plugins is the
goal. Shipping brownnote itself as a plugin is a separate, deferred idea.

**Done when:** use a real LADSPA effect, then a CLAP effect, save their
settings, and reopen the project. A missing or failed plugin does not
erase the project. Keep a usable rendered sound where available, and
clearly identify any dry fallback.

The first effect milestone does not require live processing or plugin
editor windows. Live instrument hosting has separate audio-thread,
device-buffer, state, and failure-recovery requirements.

### G6. Finish the track in SoundCraft {#g6-handoff}

[SoundCraft](https://github.com/storytold/soundcraft) is the first DAW target.
It is open source and has audio and MIDI imports, session commands, and
inspection tools. It is also pre-alpha. We need to pin and test a version,
not assume that a file we generate will work.

The initial handoff aims for these editing capabilities:

- Pad performances become individual audio clips with their timing and
  gain. A pitched or choked hit may need a rendered per-hit file. Original
  samples remain available.
- Instrument performances keep editable notes with velocity and duration.
  Instrument mapping and state transfer only where the target supports
  them. Unsupported instruments still keep their notes and rendered audio.
- Aligned dry and wet stems, plus a reference mix, preserve the sound when
  the destination cannot reproduce processing. Stems alone do not satisfy
  event-level editing.
- An export manifest identifies the parts, source assets, and anything
  baked into audio or changed during conversion.

**Done when:** export a recorded native project, open it in SoundCraft,
move one pad hit, delete another, and edit an instrument note. Play the
result, save it, and reopen it in SoundCraft without missing media.

Plugin state saved inside brownnote is not a promise of cross-DAW plugin
portability. MIDI timing conversion and audio resampling also need explicit
checks. A working brownnote-side parser is not proof of a working handoff.

### G7. A place for unusual hardware {#g7-hardware}

Explore one useful nonstandard input, such as an OSC or serial device,
through the same performance-event path used by controllers.

The [Open Instrument Project](https://openinstrument.dev/) concerns test
and measurement equipment, not a musical instrument library. Its draft
CAN-FD, hardware-trigger, and USB-C standards are a research lead, not an
announced brownnote integration.

**Discovery is complete when:** one unusual device records a normal take,
and a short assessment explains whether Open Instrument has a useful
musical application. No hardware compatibility claim comes before a
working device test.

This work stays outside the main release sequence.

### G8. A release you can trust {#g8-releases}

Native needs an installable build with no development-folder assumptions
or manual bench clone. Releases need checked dependency and asset
licenses, an audio device selector, useful errors, and project compatibility
tests.

**Done when:** install a tagged build on a clean machine and open an older
fixture project. Recover from a missing device, missing media, and a
missing plugin. A failed save leaves the previous project usable and
offers a recovery path.

Start with the platform we can test. Publish support and latency claims
only after testing the actual release build and audio path.

## Milestones in order

The sequence favors a complete musician workflow before a large catalog
of integrations. M1 to M4 have landed; M5 and the hardware research are
still ahead. Release checks grow with every milestone.

### First, test the integration assumptions {#feasibility}

Small fixtures proved a SoundCraft session with separate audio hits and a
MIDI part, a CLAP instrument's note and state lifecycle, and one LADSPA
render, and SoundCraft's own app opened the fixture (#95, #96, #97).

### M1. My native kit survives quitting {#m1-kit}

**Landed (#98, #99, #100, #103).** A custom sample can be imported, edited
into a pedal chain, saved, and reopened outside the original checkout. The
project format and asset handling for G1 exist, with the first failure
checks from G8.

The M1 items still open are audio device selection and device loss (#101)
and overload reporting with click-resistant voice stealing (#102).

### M2. My native loop keeps what I played {#m2-loop}

**Landed (#104, #106, #107).** Faithful capture, loop playback, overdub,
and take controls from G2 use the controller event path from G3. Saved
overlapping hits and note durations reopen without being flattened into
the sequencer grid.

Controller input keeps one hardware gate (#105), listed under
**Still open**.

### M3. My loop becomes an editable SoundCraft session {#m3-soundcraft}

**Landed (#108, #109, #110, #111).** One existing sampled instrument gives
the take real notes to edit. An actual native take, not only a fixture, is
exported and verified for clip positions, note data, and media references
in the pinned SoundCraft version, completing the edit, save, and reopen
demonstration from G6.

### M4. My kit includes other instruments and plugins {#m4-instruments}

**Landed (#112, #113, #114, #171).** LADSPA pad rendering, CLAP effects,
and a live CLAP instrument arrived in separately testable steps, and every
step keeps project recall and export working.

### M5. My external instruments join the session {#m5-external}

**Open (#115, #116).** This extends G3 with MIDI output, synchronization,
and audio-return recording. Timing is measured through real devices, and
both the performance data and the captured audio are preserved.

G7 (#117) stays parked until unusual hardware is available. G8 applies
throughout, rather than waiting for a final packaging milestone.

## Decisions we made

- The first instrument is one SF2 preset played by RustySynth 1.3.8, with
  the source file kept as project media (#108).
- The SoundCraft adapter uses that build's public clip and note commands at
  960 PPQ and imports each event asset onto its own track. Audio that
  differs from the full asset is reported, not silently trimmed (#110).
- A live CLAP instrument is recalled by identity and opaque state through a
  bounded helper, and its takes stay playable when the plugin is gone
  (#112, #171).

Full plugin editor hosting, brownnote as a plugin, AAF, OMF, DAWproject,
other DAW adapters, mobile, and cloud collaboration are outside the
initial roadmap.

## Help shape the instrument

The most useful feedback is a real workflow. Name the controller or
instrument you use, the loop you want to make, and what must stay editable
when you open it in your DAW.

Plugin recommendations should include the format, platform, and license.
Hardware experiments need a concrete device and a musical use. SoundCraft
export feedback should distinguish note editing, individual audio clips,
and mix stems.

[Share feedback in the brownnote issues](https://github.com/drawmeanelephant/brownnote/issues),
or read [[getting-started|the current setup guide]] to try the existing
pipeline. This roadmap records the direction. Working demonstrations
determine what becomes a supported feature.
