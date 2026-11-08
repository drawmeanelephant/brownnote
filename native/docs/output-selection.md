# Native output selection

The main pad app exposes this second #101 slice through **AUDIO OUTPUT**.
#101 remains open for the physical recovery, listening, and latency checks.

## Behavior

Audio output is a machine preference, not project data (D8). Device names
are labels; CPAL's host-qualified IDs identify outputs. A missing saved ID
never selects another output with the same name.

Open **AUDIO OUTPUT** to choose an output. Tab reaches the device group,
refresh, configuration fields, apply, retry and system-default fallback.
Arrow keys move within the device group. Space activates a focused button.
The panel replaces the pads and editor while open. **BACK TO PADS** restores them.
pad keyboard shortcuts still work.

The panel shows device defaults, channel counts, sample rates, formats, and
buffer ranges from CPAL. Blank configuration fields use device defaults. The format
follows the device default, matching the seam's supported configuration
policy. **APPLY OUTPUT** validates the whole tuple before stopping audio.
Unsupported or malformed edits leave the current output and voices alone.
A fixed buffer is only a request, not an observed callback size or a latency
promise.

An accepted change or retry stops the current voices, drops the old stream,
and opens a fresh mixer (D16). **RETRY REMEMBERED** uses the last attempted
selection, ignoring unsaved edits. **USE SYSTEM DEFAULT** explicitly resets
both identity and configuration to system defaults. Failed opens and device
loss stay silent with a reason. A factory returning a silent output is never
reported as running.

The versioned `audio-output.json` stores only the output ID and configuration.
The runtime resolver selects
`$HOME/Library/Application Support/Brownnote/preferences` by default.
`BROWNNOTE_PREFERENCES` overrides that directory. Resolution does not create
or write the directory, so readable preferences do not require write access.
Writes use a synced temporary file and rename. Missing preferences use
the default selection. Corrupt or
unsupported preferences leave startup silent until an explicit choice. Save
errors remain separate from stream errors, and the in-memory choice can
still be retried. No project API or global device settings are touched.

Headless and `--no-audio` sessions do not read machine preferences or open
hardware, even after explicit panel actions. If the preference directory
cannot be resolved, startup stays silent with an error. An explicit default
choice can still open audio, but the preference error remains visible.

## Check the mounted controls

From the repository root, run the actual feature and application targets:

```sh
	CARGO_BUILD_JOBS=1 cargo test --manifest-path native/Cargo.toml --bin brownnote-native ui::devices::
	CARGO_BUILD_JOBS=1 cargo test --manifest-path native/Cargo.toml --test headless actual_binary_mounts_output_settings_and_recovery_scenes -- --ignored
	make native-test
	make test
```

The headless command needs a GPU adapter. For required pedal coverage, build
the C CLI, set `BROWNNOTE_BIN` to that executable, set `BENCH` and
`BROWNNOTE_BENCH` to the real bench, and set `BROWNNOTE_REQUIRE_BENCH=1`.
The old source-copy bootstrap checker has been removed.

The feature tests use injected discovery, stream factories, and owned
temporary preference roots. They do not read real preferences or open devices.
The actual binary also renders the selection, missing-output, and invalid
configuration scenes at 820 by 600 without layout warnings.

The injected success case deliberately returns a virtual output and asserts
that it remains SILENT. Configuration tests check the actual seam's
validation and requested reopen parameters at 44.1/48 kHz, not physical
playback at those rates. Device-loss tests drive the recovery handler with
active mixer voices; they are not physical unplug evidence.

## Maintainer checks still required

Only built-in MacBook Pro Speakers were physically probed in the first
slice, at 44.1/48 kHz and with release `--check-audio`. That does not verify
this UI, loss/reconnect, listening, or latency. Agents cannot hear audio.

To leave normal preferences untouched, set `BROWNNOTE_PREFERENCES` to an
owned temporary directory before launching the app.

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
4. Quit and reopen. Confirm the same output ID and configuration are attempted. Temporarily
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
measurements remain unresolved. `--check-audio` probes the system default,
not the UI's saved selection. The probe reports callback
duration, observed callback frames and CPAL's output-presentation estimate
separately. None is round-trip latency; this UI makes no latency claim.
