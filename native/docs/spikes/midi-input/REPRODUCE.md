# Reproduce the MIDI input slice

## Run the owned-port probe

On macOS arm64, from the repository root:

```sh
	export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
	export CARGO_BUILD_JOBS=1
	python3 native/docs/spikes/midi-input/check_probe.py --run
```

Expect a pass line and new evidence under
`native/target/midi-probe/latest-evidence.json`. The probe uses its own target
directory. Do not run another Cargo command in this checkout at the same time.
Each receive has a two-second deadline. Silent observation intervals last
100 ms. Missing callbacks or missing cleanup fail the probe.

To check the committed evidence without MIDI access:

```sh
	python3 native/docs/spikes/midi-input/check_probe.py
```

## Run the pure fixtures

Before integrator export registration, run:

```sh
	python3 native/tests/check_midi.py --bootstrap
```

This compiles the pure modules by path in the **actual native test manifest**.
The test uses the exported project and performance types.
It is bootstrap evidence, not verification of an exported MIDI library.

After the integrator applies the exact request in [README.md](README.md), run:

```sh
	python3 native/tests/check_midi.py
```

The default command fails explicitly while export or test-import wiring is
missing. Do not count the bootstrap command as a substitute.

Run the repository gates with an existing, read-only real bench:

```sh
	export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
	export CARGO_BUILD_JOBS=1
	export CARGO_TARGET_DIR="$PWD/native/target/midi-lane"
	export BENCH="/absolute/path/to/GuitarPedal/Validation/bench/bench"
	export BROWNNOTE_BENCH="$BENCH"
	export BROWNNOTE_REQUIRE_BENCH=1
	export BROWNNOTE_BIN="$PWD/build/brownnote"
	make all
	make native-test
	make test
```

The native gate includes GPU tests. Neither gate proves live MIDI input.

## Test IAC in the second PR

This first PR has no production port selector or sender for an existing port.
Do not use the probe to open a user session or change global MIDI settings.

1. Ask the maintainer to create and enable a dedicated IAC bus in **Audio MIDI
   Setup**, if they want to run this procedure. This run found no IAC bus.
2. After the second PR provides production input, select only that dedicated
   bus in Brownnote. Use an isolated temporary project and temporary preference
   root, not the maintainer's existing mappings.
3. Use a separate, explicit test sender connected only to that bus. Send human
   channel 2, note 40 at velocities 1 and 127. Send the same note on channel 1.
   Expect two distinct channel-2 starts and no mapped channel-1 start.
4. Send two overlapping channel-2 note-ons, then two note-offs, including a
   velocity-zero note-on. Verify two event IDs and FIFO completed durations.
5. Test a held instrument with CC64 down, off, and up. Verify deferred release.
   Repeat with CC123 and CC120. Verify source/channel scope and no stuck note.
   Repeat on a one-shot pad and verify physical release does not choke it.
6. Save, quit, reopen, and reconnect through a newly created input connection.
   Verify mappings recall from the project while device selection stays in
   machine preferences.
7. Exercise queue saturation with a burst larger than capacity. Verify explicit
   loss, bounded held-note reconciliation, unchanged mappings, and later input.
8. Remove only the dedicated test bus with maintainer approval. Verify visible
   loss, cleanup, fresh connection epoch on reconnect, and continued input.

Record those results as software-only IAC evidence. Keep the named-controller
gate open until a physical controller is available and tested by name.
