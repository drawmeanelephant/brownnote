# Named-controller MIDI input runbook (#105)

**Status: NOT EXECUTED. Every row defaults to `NOT RUN`. No named controller
has run this procedure. #105 acceptance criterion 5 stays open, #105 stays
`needs-hardware`, and nothing here may be quoted as a `PASS` until a maintainer
runs it on the device named in the record below.**

This is the written procedure for the hardware half of #105 acceptance
criterion 5: "Verify the actual native input path with one named controller and
deterministic event fixtures." The deterministic half is delivered and runs in
CI. This document is the missing half, and it replaces the old ending of the
[IAC procedure](spikes/midi-input/REPRODUCE.md), which said only to keep the
gate open until a physical controller could be tested.

D2 applies in full: a virtual port, the IAC Driver, fixture packets, mocked
queues and screenshots never close this gate. A `PASS` requires the device
named in the run record.

## What this closes and what it does not

- Closes only when a maintainer executes every applicable step on one named
  controller and records the observed text and counters. The closing claim is
  "criterion 5's named-controller half ran on device X at commit Y".
- Does not replace the [IAC run](spikes/midi-input/REPRODUCE.md). That run
  remains software-only evidence and stays separate.
- Does not cover MIDI output, clock synchronization or external audio returns
  (#115, #116) or the parked research controller (#117).
- Does not change code, manifests or tests. If a step names something the
  delivered software cannot observe, the step says so and is `BLOCKED`, not
  invented.

## Evidence rules

Acceptable evidence is text and data the delivered software actually produces:

- The exact MIDI status line and the exact transport status line, copied
  verbatim.
- Counter values from those lines (`received`, `malformed`, `unmapped`,
  `triggered`, `held N active`, every `LOSS` field).
- The selected input port name and backend ID, exactly as the panel lists it.
- The saved `project.json`: `midi_mappings`, and each take note's `sequence`,
  `channel`, `pitch`, `velocity`, `on_nanos` and `end_nanos`.
- The machine preference file `midi-input.json` (port ID and name hint only).
- The Brownnote commit and the artifact SHA-256.

Screenshots are not evidence. A listening impression is not evidence for this
gate; it may be recorded separately as a listening note. Do not write an
expected value into a result row: copy what the app showed. Record `FAIL` and
`BLOCKED` rows as they happened; an overall green summary over missing rows is
invalid.

The deterministic fixtures in the last section prove the software side only.
They are the CI counterpart of this runbook and never a substitute for it.

## Run record

Fill this in during the run. Until then every field stays as written.

| Field | Value |
|---|---|
| Date, operator, run ID | NOT RECORDED |
| Controller make and model | NOT RECORDED |
| Controller firmware or version (or "not exposed by the device") | NOT RECORDED |
| Connection type: USB / 5-pin DIN via interface / Bluetooth-MIDI | NOT RECORDED |
| macOS product and build (`sw_vers -productVersion`, `-buildVersion`) | NOT RECORDED |
| Architecture (`uname -m`) | NOT RECORDED |
| Brownnote commit (`git rev-parse HEAD`) | NOT RECORDED |
| Brownnote artifact path and SHA-256 (`shasum -a 256`) | NOT RECORDED |
| Selected input port name and backend ID | NOT RECORDED |
| Project root (absolute, isolated for this run) | NOT RECORDED |
| Machine preference root (`BROWNNOTE_PREFERENCES` or the default) | NOT RECORDED |
| Instrument used for held notes: SF2 path and SHA-256 (if any) | NOT RECORDED |
| Raw status/JSON captures kept outside the project | NOT RECORDED |
| Overall result | NOT RUN |

Use PASS / FAIL / BLOCKED / NOT RUN per step. The overall result is only
`PASS` when every applicable step is `PASS`; otherwise it is `FAIL` or
`BLOCKED`.

## #105 criterion to step map

| #105 acceptance criterion | Steps | Result |
|---|---|---|
| 1. Discover/select a controller and learn pad or instrument mappings with channel handling and full velocity | 1–3 | NOT RUN |
| 2. Note-on/off, sustain and panic with one-shot versus held-instrument semantics | 3–7 | NOT RUN |
| 3. Persist mappings in projects and record note durations through the shared event path | 8–9 | NOT RUN |
| 4. Disconnect/reconnect, repeated notes and queue saturation without stuck notes or silent mapping changes | 4, 10–11 | NOT RUN |
| 5. Verify the actual native input path with one named controller and deterministic event fixtures | 1–13 | NOT RUN |

Step 11's named-controller execution and parts of steps 2 and 4 are expected to
be `BLOCKED` on some devices. A blocked sub-step keeps the gate open for that
criterion and must say why.

## Preparation

0.1. Register the controller with macOS (USB or Bluetooth-MIDI pairing) and
confirm it appears in **Audio MIDI Setup**. Do not change any global MIDI
configuration; only the new device may appear.

0.2. Build or locate the artifact and record its identity:

```sh
cargo build --release --manifest-path native/Cargo.toml
git rev-parse HEAD
shasum -a 256 native/target/release/brownnote-native
sw_vers -productVersion
sw_vers -buildVersion
uname -m
```

0.3. Create isolated run directories, then launch with the machine preference
root redirected so this run cannot disturb the maintainer's real preferences:

```sh
mkdir -p /abs/run-195/prefs /abs/run-195/project
BROWNNOTE_PREFERENCES=/abs/run-195/prefs \
  native/target/release/brownnote-native
```

Do not run the app against a personal project or the default preference root.

0.4. Open the **MIDI INPUT** header button and confirm the panel caption says
`MIDI INPUT · DEVICE CHOICE IS MACHINE-LOCAL (D8)`. Press `REFRESH INPUTS`.

## Procedure

Each step lists what to do, the observable you must copy down, and its default
result. Do not change a result without observed text or saved JSON behind it.

### Step 1: Enumerate and select the controller (criterion 1)

Press `REFRESH INPUTS`. The port list shows one radio button per enumerated
port as `<name> · <backend id>`, plus `No input (disconnect)`. Select the
controller's port, press `CONNECT`, and record the exact text.

Expected observable: the header reads `MIDI · <port name>`, and the status line
begins `CONNECTED · <port name> · source <n>` followed by
`received N · malformed N · unmapped N · triggered N · held N active`.

Record: port name, backend ID, full status line, and the contents of
`<BROWNNOTE_PREFERENCES>/midi-input.json` (port ID plus name hint only; D8).

Result: NOT RUN

### Step 2: Confirm device choice is a machine preference (D8, criterion 1)

Quit the app, relaunch it with the same `BROWNNOTE_PREFERENCES`, reopen the MIDI
panel, and press `REFRESH INPUTS`. Then inspect a saved project later in step 9
for the absence of any port field.

Expected observable: the selection is recalled from `midi-input.json`, the app
reconnects or reports the saved selection, and the project document contains no
device selection. The selected port is never project data.

Record: relaunch status line and the `midi-input.json` field names.

Result: NOT RUN

### Step 3: Learn a one-shot pad and a held instrument with channel filtering and velocities 1 and 127 (criteria 1–2)

Load the sampled instrument if held notes are to sound: open the instrument
panel, type an absolute `.sf2` path, press `LOAD SF2`. Without one, held notes
are still captured with durations and the transport reports
`N HELD NOT SOUNDED (NO INSTRUMENT LOADED)`.

Set the controller to send on a chosen human channel. Turn learn on with the
`LEARN OFF`/`LEARN ON` toggle (while on it reads `LEARN ON`), play a pad once,
and confirm `LEARN · captured: ch N · note P`. Select a pad with an audible
sample, then press `MAP TO PAD NN`.

With learn still on, play a note for the instrument and press
`WHOLE CHANNEL → INSTRUMENT`. Then press the toggle to turn learn off before
playing mapped notes: while learn is on, a note becomes a candidate instead of a
trigger.

Now play the learned pad note:

- at velocity 1 and at velocity 127;
- once on the mapped channel and once with the same note number on a channel
  that has no mapping.

Expected observable: `triggered` increments once per hit on the mapped
channel; the same note on the unmapped channel increments `unmapped` and plays
nothing. A note release never triggers a one-shot. A learning pass never
captures a release. Velocity is not printed on the MIDI status line, so verify
it from the saved project in step 8: the two one-shot hits must carry
`velocity` 1 and 127, distinct.

If the controller cannot emit velocity 1, or cannot send on a second channel,
record that sub-step `BLOCKED` with the reason (for example "pads are fixed
velocity 100"). Do not mark it `PASS` from the fixture result.

Record: learn candidate text, mapping list rows, counter values before and
after, the two velocities from saved JSON, and the exact blocked reason if any.

Result: NOT RUN

### Step 4: Overlapping same-pitch notes end FIFO (criteria 2, 4; D14)

Map one held instrument note (the channel mapping from step 3). Play two
overlapping note-ons of the same pitch, hold the first longer than the second,
then release them. Repeat with two notes of the same pitch started together.
Then check the saved note list from step 8.

Expected observable: `held 2 active` while both sound. `triggered` does not
count held starts. Two distinct `sequence` values exist in the saved take, and
the first note-off ends the oldest identity: durations show the first-started
note ending first, with the second continuing to its own release. No release
is invented for a note that is still held.

Record: status line before and after each release, and both saved note rows.

Result: NOT RUN

### Step 5: CC64 sustain on held versus one-shot notes (criterion 2)

Press the learn toggle to turn learn on, move the sustain pedal, confirm a
`LEARN · captured: ch N · cc 64` candidate (the controller may emit another CC;
record the real number), press `SUSTAIN`, then press the toggle (reading
`LEARN ON`) to turn learn off so the pedal does not become a new candidate on
each press.

Hold a held-instrument note, press the pedal down (values 64–127), release the
key while the pedal is down, then release the pedal (values 0–63). Repeat on a
mapped one-shot pad.

Expected observable: with the pedal down the held note keeps sounding and
`held 1 active` stays; pedal-up releases it at the pedal-up time, and the saved
duration ends at pedal-up, not key-up. Sustain does not choke or extend a
one-shot pad hit: the pad triggers once on note-on and the pedal does not make
it sound again.

Record: learned CC number, status before key-up / after key-up / after pedal-up,
and the saved durations.

Result: NOT RUN

### Step 6: CC123 all-notes-off (criterion 2)

Learn the controller's all-notes-off control if it has one (map the actual CC
it emits, using `ALL NOTES OFF`), then turn learn off, and send it while two
held notes sound. Repeat with sustain down.

Expected observable: all held notes on that source and channel are released;
`held 0 active`. With sustain down, the release is deferred until pedal-up. A
one-shot pad is unaffected. Notes outside that source/channel scope stay held if
another mapped channel exists; otherwise record that scope check as `BLOCKED`.

Record: learned CC number, status before and after, durations, and whether the
pedal deferred the release.

Result: NOT RUN

### Step 7: CC120 panic on held versus one-shot notes (criterion 2)

Learn the controller's panic control if it has one (using `PANIC`), turn learn
off, and send it while held notes sound with sustain down. Repeat during an
open recording pass.

Expected observable: every held note in that source/channel is released
immediately, sustain is cleared, `held 0 active`, and an open pass closes on
panic instead of staying `CLOSING`. A one-shot pad hit is unaffected. Global
audio panic, if used as a fallback, must report any other voices it stops.

If the controller has no panic control, map another CC it can send and record
which one; if it has no usable control at all, record the sub-step `BLOCKED`.

Record: learned CC number, status before and after, transport status if a pass
was open.

Result: NOT RUN

### Step 8: Record a take and read the durations (criterion 3)

Set `LOOP 1`, `OVERDUB` or `REPLACE` (record which), `COUNT-IN OFF`. Press
`● REC` and play a mapped one-shot hit and a held note with a deliberate,
counted hold time. Release the key before the loop ends and press `■ REC` to
close the pass.

Expected observable: while recording, `REC T1 · N NOTES` counts completed
notes. If the cycle ends with a key down, the status shows
`CLOSING T1 · N HELD` and the pass waits; it never invents an end. After the
pass closes, the status includes `SEL T1: N NOTES · HELD <n>ms/...` with the
durations derived from the real note lifecycle.

Stop all transport activity, then save the project to the isolated root with
`SAVE / SAVE AS` and record the saved take rows:

```sh
python3 -c 'import json,sys; d=json.load(open(sys.argv[1]));
print(json.dumps(d.get("takes"), indent=2))' /abs/run-195/project/project.json
```

Inspect `sequence`, `channel` (zero-based in JSON; the UI shows one-based),
`pitch`, `velocity`, `on_nanos` and `end_nanos`. The one-shot hit is a
zero-length pad note; the held note has a real duration near the counted hold.

Result: NOT RUN

### Step 9: Save, quit, reopen and confirm mapping recall (criteria 1, 3)

Quit the process completely, relaunch with the same `BROWNNOTE_PREFERENCES`,
open the MIDI panel, then open the saved project with `OPEN (REPLACE KIT)`.

Expected observable: the mapping list is identical to step 3 (same channels,
notes, CC actions, pad and instrument targets), and the saved take still shows
the same note count and durations. The device selection is unchanged because it
is machine-local: it comes from `midi-input.json`, not from the project.
Opening a project does not silently change the selected port or the mappings'
meanings.

Then test the missing saved port: quit, unplug (or power off) the controller,
relaunch with the same preferences, and open the MIDI panel.

Expected observable, after refresh: the status is
`DISCONNECTED · Selected input "<name>" is not available; refresh the list`,
the mapping list is unchanged, and the project opens normally. Record this
failure path separately; it does not by itself close the device run.

Record: mapping list before and after, take summary before and after, the
missing-port status text, and the preference/project field difference.

Result: NOT RUN

### Step 10: Unplug and reconnect with loss reporting (criterion 4)

With a held note sounding, unplug the controller. The selected API has no
disconnect callback, so silence alone is not detection: the app can still show
`CONNECTED` until it enumerates. Press `REFRESH INPUTS`.

Expected observable: `Input disconnected: <n> held note(s) released, loss
reported; mappings unchanged`; `held 0 active`; the port disappears from the
list; the mapping list is unchanged; any open pass stops waiting on the
released note. Reconnect the controller, press `REFRESH INPUTS` and then
`CONNECT` if it does not reconnect by the saved selection; a saved-selection
reconnect reports `Reconnected to <name> by saved selection`.

After reconnecting, play the same mapped note. The `source` number is a fresh
connection epoch, not the old one. The mapping list is unchanged, and no note
from the old connection is retriggered.

Record: status before unplug, the exact notice and loss fields after refresh,
`held` count, port list, source number before and after, and the list of
mappings (must be identical).

Result: NOT RUN

### Step 11: Queue saturation (criterion 4)

This step is `BLOCKED` for the named controller unless the controller can emit
more messages inside one UI poll than the 1,024-slot raw input handoff and the
256-slot command queue accept. A human playable controller cannot normally do
that: a burst large enough to saturate the queue needs a scripted sender, and
this runbook may not inject packets into the controller's own input port.

If the device has an arpeggiator or sequencer that can be set to an
exceptionally dense burst, try it and record the result. Otherwise record:

`BLOCKED: no scripted sender into the named controller's input port; the
deterministic saturation fixture and the IAC software-only path cover this, and
neither closes the gate.`

Expected observable if a burst is possible: `LOSS ... commands <n>` (and
possibly `raw`/`oversized`) appears; `triggered` is lower than the number of
hits sent; no note is left stuck (`held` reconciles); mappings are unchanged;
after a short pause a further hit still triggers and increments `received`.
The deterministic counterpart is
`ui::midi::tests::queue_saturation_is_counted_and_later_input_continues` and
`midi::input::tests::burst_beyond_queue_capacity_counts_loss_and_later_input_continues`.

Record: whether the burst was possible, the exact `LOSS` fields, counters
before/after, held count, and the later-hit result.

Result: NOT RUN (expected to be recorded `BLOCKED` on a controller with no
scripted sender)

### Step 12: Deterministic fixtures as the CI counterpart (criterion 5)

Run the software checks from the repository root and record each result. These
are the deterministic half of criterion 5:

```sh
python3 native/tests/check_midi.py
cargo test --manifest-path native/Cargo.toml --test midi
cargo test --manifest-path native/Cargo.toml --test project_midi
make native-test
```

The fixtures cover message parsing, every channel/pitch/velocity, unsupported
message classes, channel-2-only mapping, 1/127 velocity, learn, mapping-key
uniqueness, release identity after mapping edits, FIFO overlaps, one-shot versus
held, sustain/all-notes-off/panic, queue saturation, disconnect/reconnect, and
project mapping recall. `--scene transport-midi` renders the real controller and
dispatch path with a virtual input, but it is still software evidence.

Expected observable: all commands pass. They do not prove the named controller,
do not close criterion 5, and do not change #105's labels. A docs-only change
must leave these results identical.

Record: command, commit, pass/fail, and the test count or summary line for each.

Result: NOT RUN

### Step 13: Named-controller native input overall (criterion 5)

After steps 1–12, confirm with the named device throughout:

- No crash, hang, silent mapping edit or stuck note after any of the failure
  paths above.
- The mapping list is byte-for-byte the same after unplug/reconnect, a saturated
  burst where attempted, and a project reopen.
- The saved project's mappings and take notes match what was observed live.

Expected observable: every recorded status line, counter, saved row and
mapping row is mutually consistent. Any inconsistency is `FAIL` with the
observed text, even when the rest of the run passed.

Record: final status lines, final saved project path, and the mapping list.

Result: NOT RUN

## Failure paths and expected observations

| Failure path | How to reach it | Expected observable | Result |
|---|---|---|---|
| Unsupported message classes | move the pitch/mod control, play aftertouch, send SysEx, clock or system reset from the device if it can | `received` rises and `malformed` rises; `triggered` does not; no event or held identity is invented; the app keeps running | NOT RUN |
| Controller emits only unsupported classes | a device that sends no note-on or CC at all | no mapping can be learned; record `BLOCKED` for the device | NOT RUN |
| Unplug mid-take | unplug while a pass is open with a note held | pass shows `CLOSING Tn · N HELD` until refresh; refresh reports the released count and the note ends at reconcile time, not at an invented earlier time | NOT RUN |
| Vanished port, no disconnect callback | unplug without refreshing | status can still read `CONNECTED`; silence is not detection. `REFRESH INPUTS` reconciles and reports `Input disconnected: ...; mappings unchanged` | NOT RUN |
| Saturated command queue | scripted burst only; not normally reachable from a playable controller | `LOSS` fields appear, `triggered` falls short of the burst, held notes reconcile, mappings unchanged, later input continues; record `BLOCKED` on a named controller without a sender | NOT RUN |
| Missing saved port on reopen | quit, remove the device, relaunch with the same preferences | `DISCONNECTED · Selected input "<name>" is not available; refresh the list`; mappings and project open unchanged | NOT RUN |
| Duplicate port names | two ports share the saved name and the saved ID is gone | `Selected input "<name>" matches <n> ports; choose one from the list`; no port is silently aliased | NOT RUN |
| MIDI backend unavailable | launch a build or session without the backend | status `MIDI BACKEND UNAVAILABLE`; the rest of the app still works | NOT RUN |

A failure path that cannot be triggered by the recorded device is `BLOCKED`
with the reason, not `PASS`.

## Failure outcome: no compatible controller

If the maintainer has no compatible controller, cannot connect it, or cannot
produce the messages a step needs, record the run as `BLOCKED (no device)`,
adding the specific missing capability when the device exists (for example
"no sustain pedal", "fixed velocity", "single channel").

A `BLOCKED (no device)` run cannot be used as a closure claim for #105. The
issue stays open with `needs-hardware`, and this runbook stays `NOT EXECUTED`
for the device it names. Never convert a blocked step into a software pass.

## Deterministic counterpart and its limits

The committed fixtures are the CI-side counterpart of this runbook:

- `native/tests/midi.rs` and `native/tests/project_midi.rs`, run through
  `python3 native/tests/check_midi.py` and the native test targets.
- The input/UI tests in `native/src/midi/input.rs` and `native/src/ui/midi.rs`.
- `--scene transport-midi` in the real binary.

They prove the delivered software paths repeat deterministically. They do not
substitute for the hardware run, and no fixture, virtual port or IAC bus can
close criterion 5 (D2).

## Not in this runbook

- MIDI output, clock sync, external audio returns (#115, #116).
- The parked unusual-controller research (#117).
- Changing mappings, project format, backend or any source file. This document
  is the whole deliverable.
