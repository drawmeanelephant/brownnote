# Native output selection

This feature is the second slice of #101, based on the device seam in PR #124.
It needs integrator wiring before the normal app exposes it. #101 remains open.

## Behavior

Audio output is a machine preference, not project data (D8). Device names
are labels; CPAL's host-qualified IDs identify outputs. A missing saved ID
never selects another output with the same name.

Open **AUDIO OUTPUT** to choose an output. Tab reaches the device group,
refresh, configuration fields, apply, retry and system-default fallback.
Arrow keys move within the device group. Space activates a focused button.
The panel replaces the pads/editor while open. **BACK TO PADS** restores them;
pad keyboard shortcuts still work.

The panel shows device defaults and CPAL's supported channel/rate/format and
buffer ranges. Blank configuration fields use device defaults. The format
follows the device default, matching the seam's supported configuration
policy. **APPLY OUTPUT** validates the whole tuple before stopping audio.
Unsupported or malformed edits leave the current output and voices alone.
A fixed buffer is only a request, not an observed callback size or a latency
promise.

An accepted apply/retry stops the current voices, drops the old stream,
and opens a fresh mixer (D16). **RETRY REMEMBERED** uses the last attempted
selection, ignoring unsaved edits. **USE SYSTEM DEFAULT** explicitly resets
both identity and configuration to system defaults. Failed opens and device
loss stay silent with a reason. A factory returning a silent output is never
reported as running.

The versioned `audio-output.json` stores only the output ID and configuration.
The caller injects its machine-preference root. Writes use a synced temporary
file and rename. Missing preferences mean the default selection; corrupt or
unsupported preferences leave startup silent until an explicit choice. Save
errors remain separate from stream errors, and the in-memory choice can
still be retried. No project API or global device settings are touched.

## Integrator wiring

The feature lane does not modify shared files. Apply these changes there:

1. Add `pub(crate) mod devices;` to `ui.rs`. Add
   `pub devices: devices::Devices` to `State`.
2. In `State::with_cache`, construct devices before output:
   - For headless or `--no-audio`, use `devices::Devices::disabled()`.
   - For interactive audio, use `devices::Devices::load(machine_root)`.
     Supply the agreed machine-preference directory; do not use a project,
     sample directory or pedal cache. Root injection deliberately avoids
     depending on the draft #103 paths API.
   - Replace the existing open/default branch with
     `devices.initial_output(slots.clone(), disabled_reason)`, preserving
     the current headless/no-audio status strings. Store `devices` in `Self`.
     This avoids opening the default device before reading saved selection.
3. After `recover_stream_error()` and before `output.advance(...)`, call
   `let devices_changed = devices::poll(s);`. Include it in `changed`.
   While `s.devices.busy()`, call
   `tx.wake_after(Duration::from_millis(1))` so the second opening phase runs
   even when no voices or meters are active. This is a wake request, not a
   latency guarantee.
4. Import `devices` in `ui/view.rs`. Add `devices::header(s)` in an accessible
   view slot. Replace the `.child(grid).child(editor(s))` pair with the
   feature panel while `s.devices.visible`, otherwise keep both unchanged.
   Keep the existing retry handler for current callers. Add
   `.disabled(s.devices.busy()).focusable(!s.devices.busy())` to that button
   so it cannot replace a stream during a queued feature change.
5. Register `output-selection`, `output-missing`, and `output-invalid`
   scenes through `devices::scene(s, devices::SceneKind::{Selection,Missing,Invalid})`.
6. Reuse normal dependencies `serde = { version = "1", features = ["derive"] }`
   and `serde_json = "1"`, now landed in project PR #120. No dependency edit,
   new serializer or SHA dependency is needed here.

No change is requested in `lib.rs`, the mixer/DSP, or showcase. Showcase
continues using the seam's remembered-selection recovery independently.

## Automated checks

`python3 native/scripts/check-devices-feature.py focused` stages the requested
module/State/poll/view wiring in a clearly owned `native/target/` copy. It
runs real feature handlers and Rune Runtime tests with injectable discovery,
stream factories and owned temporary preference roots. It does not modify
shared source files, read real preferences, or open audio devices.

`python3 native/scripts/check-devices-feature.py full` adds bootstrap clippy
and the full native test suite, including ignored GPU tests. Set
`BROWNNOTE_BIN`, `BROWNNOTE_BENCH`, and `BROWNNOTE_REQUIRE_BENCH=1` to require
the real CLI/bench paths. The copied source and manifest are bootstrap
evidence, not a green result for the integrator's eventual tree. Run
`make native-test` and `make test` on that tree after wiring.

The injected success case deliberately returns a virtual output and asserts
that it remains SILENT. Configuration tests check the actual seam's
validation and requested reopen parameters at 44.1/48 kHz, not physical
playback at those rates. Device-loss tests drive the recovery handler with
active mixer voices; they are not physical unplug evidence.

## Maintainer checks still required

Only built-in MacBook Pro Speakers were physically probed in the first
slice, at 44.1/48 kHz and with release `--check-audio`. That does not verify
this UI, loss/reconnect, listening, or latency. Agents cannot hear audio.

After integration:

1. Lower the Mac's output volume to a comfortable level. Close other audio
   work if changing outputs could disrupt it. Launch the native app normally.
   Do not change macOS's global output or sample-rate settings for this test.
2. Use Tab, arrows and Space to select the built-in speakers by ID. Apply
   the device defaults. Confirm RUNNING and the actual opened configuration.
   Play a pad and have the maintainer listen. Stop with Esc.
3. If the displayed ranges support both rates, apply 44100 and then 48000 Hz
   in the app. Trigger a pad just before each apply. Confirm the voice-stop
   notice, fresh voice count, correct opened rate, and audible new hits.
   Enter an unsupported rate/buffer tuple; confirm the error without stopping
   the existing output.
4. Quit and reopen. Confirm the same ID/configuration is attempted. Temporarily
   use an owned test preference root to exercise a stale ID or corrupt JSON;
   do not corrupt the maintainer's normal preferences.
5. Optionally connect the maintainer's headphone, Bluetooth or AirPlay output.
   Refresh and explicitly select it in Brownnote. Lower volume before playing.
   While a pad is sounding, safely disconnect that output. Confirm SILENT,
   the loss reason, stopped voices and the remembered ID. Retry while absent,
   then reconnect, refresh and retry. Confirm recovery to that same ID.
   Merely changing the system default is not proof of device disappearance.
6. With the optional output absent, choose **USE SYSTEM DEFAULT** explicitly.
   Confirm the new choice is saved and attempts the current system default
   with default configuration. Verify Tab focus and status updates through
   both failures and recovery. Check VoiceOver announcements manually.

The optional second-output run, maintainer listening, and first-use latency
measurements remain unresolved. The seam's `--check-audio` reports callback
duration, observed callback frames and CPAL's output-presentation estimate
separately. None is round-trip latency; this UI makes no latency claim.
