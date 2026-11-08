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

> This is a proposed roadmap, not a list of available features or promised
> release dates. All milestones below are planned. Integration choices need
> working demonstrations before we claim compatibility.
>
> Last reviewed: 8 November 2026.

## What exists today

The C command-line tool processes WAV or raw audio through the real
[GuitarPedal](https://github.com/torvalds/GuitarPedal) bench.
The native Rust and Rune app has playable sample pads, gain, pan, pitch,
and per-pad pedal chains rendered in the background. Native settings and
chains currently reset when you quit.

The [[guides/studio|browser studio]] has more of the music workflow:
sequencing, mixing, layered loop recording, Web MIDI pad input, microphone
recording, saved patches, and mix export. That does not make those features
available in the native app. Its loop capture and export also discard some
performance detail.

Native is the main product direction. The browser studio remains useful
and provides a reference for the workflows we bring into native. The goal
is one native music workflow, not two independent full studios. The
existing command-line pipeline does not need a rewrite.

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
of integrations. Release checks grow with every milestone.

### First, test the integration assumptions {#feasibility}

Use small fixtures to prove a SoundCraft session with separate audio hits
and a MIDI part, a CLAP instrument's note and state lifecycle, and one
LADSPA render. SoundCraft's own app must open the fixture.

These checks can accompany project work. Their results inform the export
and hosting designs before we commit to a large implementation.

### M1. My native kit survives quitting {#m1-kit}

Load a custom sample, edit a pedal chain, save the kit, and reopen it
outside the original checkout. Establish the project format and asset
handling for G1, with the first failure checks from G8.

### M2. My native loop keeps what I played {#m2-loop}

Add faithful capture, loop playback, overdub, and take controls from G2.
Bring in controller input from G3 through the same event path. Save and
reopen overlapping hits and note durations without flattening them into
the sequencer grid.

### M3. My loop becomes an editable SoundCraft session {#m3-soundcraft}

Add one existing sampled instrument so the take has real notes to edit.
Export an actual native take, not only a fixture. Verify clip positions,
note data, and media references in the tested SoundCraft version. Complete
the edit, save, and reopen demonstration from G6.

### M4. My kit includes other instruments and plugins {#m4-instruments}

Expand G4 and G5 using the feasibility results. Deliver LADSPA pad
rendering, CLAP effects, and live CLAP instruments in separately testable
steps. Every step keeps project recall and export working.

### M5. My external instruments join the session {#m5-external}

Extend G3 with MIDI output, synchronization, and audio-return recording.
Measure timing through the real devices. Preserve both the performance
data and the captured audio.

G7 can proceed as an independent experiment once the controller event
path exists. G8 applies throughout, rather than waiting for a final
packaging milestone.

## Decisions still open

- Which existing sampled-instrument backend gives us a useful first
  instrument without creating another engine to maintain?
- Which SoundCraft commands preserve the required editing behavior, and
  which processing needs per-hit audio or stem fallbacks?
- How should live plugins run so that a failed plugin does not lose a
  project or leave the instrument unusable?

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
