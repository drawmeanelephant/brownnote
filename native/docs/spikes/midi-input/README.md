# MIDI input boundary, first slice (#105)

`midir =0.11.1`, default features, is selected for the next macOS arm64
input slice after the owned-port probe passed. The standalone manifest and
lockfile pin this choice. `midir` is MIT licensed.

The 8 November 2026 run used macOS 27.3 and rustc 1.99.1,
`aarch64-apple-darwin`. [Saved evidence](probe-evidence.json) contains four
real CoreMIDI callbacks, not mocks. The probe found **zero existing input
ports**, including zero names containing `IAC`. It opened no existing ports
and changed no MIDI, audio, or device preferences.

The probe creates one temporary CoreMIDI source with a process-unique name.
It enumerates and connects to that source, receives known bytes, closes and
reopens the input, removes the source, then recreates it with the same name.
The old port disappears, its name lookup fails, and the new port has another
ID. The probe removes its endpoints before returning. These results support
backend selection, not production input reliability or controller support.

## Timestamp evidence and limits

The four callback timestamps in the saved run are increasing across both
input reconnect and source recreation. The initial value is not zero-based
at connection. Converted timestamp deltas agree with monotonic host receive
deltas in the [scale check](check_probe.py). That check rejects unit mistakes,
not timing jitter. It is not a latency benchmark.

The registry-pinned `midir` source, `src/backend/coremidi/mod.rs`, explains the
observed scale: `handle_input` converts CoreAudio host ticks to nanoseconds
and divides by 1,000. Zero packet timestamps use the current host clock.
The generic API promises microseconds with an unspecified connection-stable
epoch. This macOS run supports a continuing host-clock epoch, but proves
nothing about reboot, sleep, hardware timestamps, or other backends.

The probe's default-feature sender uses zero packet timestamps. Its callbacks
therefore test receipt-time fallback, not preservation of device-origin times.
The second PR needs a checked microsecond-to-nanosecond conversion and a fresh
connection epoch calibrated against #104's real `ClockAnchor`. Backend values
are not `OriginalTime`, CPAL frames, or Unix timestamps.

There is no disconnect callback in the selected API. The probe observes
source removal through enumeration, not silence. Silence cannot distinguish
a removed controller from an idle one. Reconnect needs explicit enumeration,
connection ownership, a new connection epoch, and registry-owned `SourceId`
policy. A name or enumerated index is not a stable performance source ID.

The output API exposes immediate `send`, not a caller-supplied deadline.
The same backend supplied only the probe's own traffic. Future #115 output,
clock sync, external-device timing, and the optional
`coremidi_send_timestamped` feature remain untested. They are not selected
production capabilities by implication.

## Pure parser API

`midi::parser::parse_message(&[u8]) -> Result<Message, ParseError>` accepts
**one complete, status-prefixed MIDI 1.0 message per call**:

- Note on, preserving `Velocity` 1–127.
- Note off, retaining its seven-bit release velocity.
- Control change, retaining its seven-bit controller and value.

Channels are #104's zero-based `Channel`: human channel 2 is wire value 1.
Pitch and velocity are the real #104 types, not parallel DTOs.
Velocity-zero note on becomes note off before constructing `Velocity`.

The parser is stateless. Running status, concatenated messages, interleaved
real-time bytes, and partial chunks are not raw-stream inputs. Length errors,
high data bits, missing status, and unsupported messages return errors without
events or retained state. SysEx, pressure, pitch bend, clock, and system reset
are unsupported. The selected CoreMIDI backend frames callback messages.
An adapter must count malformed or unsupported callbacks, not reinterpret
their bytes or claim that truncated input sounded.

## Mapping API and lifecycle boundary

`midi::mapping` contains semantic DTOs, with no independent storage format:

- `Mapping::Note` keys the real `Channel` and `Pitch` to either
  `NoteTarget::OneShot(PadId)` or `NoteTarget::Held(InstrumentId)`.
- `Mapping::Controller` keys channel and controller number to `ControlAction`.
- `Mappings::new` rejects duplicate keys. A note and controller with the same
  number are separate keys. Different channels are separate keys.
- `MappingKey::learn` returns a candidate from on or CC, never release.
  This is candidate extraction, not a running learn UI.
- `Mappings::resolve(SourceId, Message)` returns an optional `Intent`.
  No ID, timestamp, source, or selected device is invented by the mapper.

Starts preserve the exact velocity and source/channel/pitch key. The producer
must assign the registry's real `EventId` and take-relative `OriginalTime`.
Held starts enter #104's real `NoteLifecycle`; one-shot starts do not become
held instrument voices. A release has a `NoteKey`, never a pad target.
It is emitted independently of current mappings so a mapping edit cannot
redirect or suppress the release of an already-held identity.

CC64 mapping yields sustain down at values 64–127 and up at 0–63.
CC123 and CC120 mappings yield distinct all-notes-off and panic intents.
These mappings are explicit project entries, not hidden global listeners.
Every control intent carries `SourceChannel`, including panic.

This PR tests those intents and uses the actual `NoteLifecycle` to test
overlap FIFO, cross-take IDs, original ends, and source isolation.
It **does not implement** a held-note controller. Sustain deferral and its
release, all-notes-off release, panic dispatch, and source-disconnect release
are integration gates, not proven by the intent fixtures.

The next producer must retain onset identity and target through mapping edits.
Sustain defers physical releases but preserves FIFO pairing. Pedal-up releases
the deferred identities. All-notes-off acts as releases and honors sustain.
Panic immediately releases held notes and clears sustain in its source/channel
scope. A one-shot ignores physical note release. Global audio panic as a
recovery fallback must report any other voices it stops.

## Ownership and exact integration request

The first-PR request to the integrator is:

1. Add `pub mod midi;` to `native/src/lib.rs`.
2. In `native/tests/midi.rs`, remove the bootstrap comment, `#[path]`, and
   `mod midi;`. Replace the existing native import with
   `use brownnote_native::{midi, performance, project};`.
3. Run `python3 native/tests/check_midi.py` and both repository gates.

No root manifest dependency, State field, poll call, UI slot, engine change,
or device preference is needed for these pure modules. The standalone probe's
lockfile is separate. The **second PR**, not this PR, requests
`midir = "=0.11.1"` in the root native manifest and its lockfile update.

#100 owns adding these semantic mappings to the project document, including
its explicit version decision and persistence validation. There is no serde
wire format or separate MIDI settings file here. D8 keeps selected port and
reconnect hints in machine preferences, not in mapping keys.

There is no queue or scheduler in this PR. Future queue-full and disconnect
paths must report loss separately from capture and dispatch, reconcile held
notes and recording state, and continue later input without changing mappings.
Those paths, IAC, project recall, durations through the recorder, instruments,
and the named-controller check remain open. **#105 remains open.**

When a physical controller exists, run the
[named-controller input runbook](../../midi-named-controller-runbook.md). It
starts `NOT EXECUTED` and only a run on the device named in its record can
close the hardware gate. A virtual port or fixture never can (D2).

See [reproduction steps](REPRODUCE.md) and [validation status](VALIDATION.md).
