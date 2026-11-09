# Output device and playback timestamp seam

First slice of #101, following D8 and D16. It does not deliver a device picker,
persistent preferences, or the complete recovery UI.

## API

- `output_devices()` returns `DeviceInfo` snapshots with CPAL IDs, display-name
  hints, defaults, and supported channel/rate/format/buffer ranges. IDs, not
  names or enumeration positions, identify devices. CPAL only promises stable
  IDs where the backend can provide them.
- `Output::open_selected(slots, &selection)` re-queries the selected device
  before opening it. `device_id: None` explicitly follows the system default.
  An absent/stale explicit ID fails; it never opens a namesake or the default.
- Unspecified rate and channels retain that device's defaults. The format
  remains the device's default format (F32, I16, U16, or I32). Validation
  requires one supported range to contain the complete tuple. An invalid
  default is also an error. A fixed buffer requires known supported bounds.
  The callback length remains authoritative, not the requested fixed size.
- `Output::open` remains the system-default wrapper used by native startup
  and showcase. It deliberately does not read machine preferences.
- Every open makes a fresh mixer at the validated rate. Replacing or dropping
  the old output stops its voices. `retry` reports that voices stopped, retries
  the remembered selection, and preserves it in a failed silent mixer.
  `silent_selected` preserves the selection after loss and advertises silence
  and stopped voices. These are off-thread operations.
- `opened()` is `None` for every silent output, even one advancing virtual
  time. `timestamps()` is also `None`; a virtual mixer never claims physical
  output observations.

The native retry/loss path preserves the selection already held by `Output`.
Startup still requests the default. Showcase retains its existing default-only
startup/recovery behavior; no selection UI exists there. Its caller remains
source-compatible. Selection controls, explicit fallback actions, preferences,
and the full Running/Silent/Retrying UI state machine belong to the second PR.
Do not serialize these settings into a portable project.

## Timestamp contract for #104

Each physical stream gets a process-local `StreamId`, including same-device,
same-rate reopens. Its frame offset starts at zero. Before command drain and
render, the callback publishes the first-frame offset, actual frame count,
sample rate, and unmodified CPAL callback/predicted-playback instants.

`PlaybackClock` is constructed off-thread. `publish` uses only scalar work,
a preallocated SPSC queue, and an atomic overflow counter. The bounded
64-observation queue drops new observations when full, never audio. Frame
offsets still advance. `take_latest()` drains at most 64 observations; after
overflow its returned observation may be old. Check `dropped()` and poll again
for a fresh observation. This is a timestamp handoff, not an event scheduler.

Compare CPAL instants only within one stream identity. They are not a mapping
to UI wall time or stored take time. Predicted playback minus callback time is
not round-trip latency.

## Callback measurement seam

`CallbackProbe` and `CallbackMeasurements` are the sibling of the timestamp
queue, with the same shape and guarantees: constructed off-thread, moved into
the callback, a bounded 64-observation SPSC queue, and an atomic overflow
counter. `Callback::set_probe` attaches one before the stream starts; the
normal app open never attaches one, so the app's callback path is unchanged.
None of this reaches the application handle.

When a probe is attached the callback measures, outside every lock:

- `duration`: two `Instant` reads around the callback body (timestamp publish,
  command drain, sample render, live mix);
- `frames`: the buffer length CPAL delivered;
- `lead`: `playback - callback` from the callback's own `OutputStreamTimestamp`,
  or unavailable when the prediction does not follow the callback instant.

`CallbackSummary::absorb` drains the queue on the consumer thread and computes
nearest-rank p50/p95, keeping at most 4096 values per distribution, so the
audio thread stores nothing and a long run stays bounded. It rejects replayed,
reordered and foreign observations instead of merging them, and counts
unavailable predictions rather than reporting a zero lead. `--check-audio`
prints that summary as a stable block; round-trip latency stays explicitly
unmeasured because output-only timestamps cannot derive it. Those printed
numbers describe one machine's run; nothing here is a latency guarantee, and no
device test depends on a probe.

## Reproduce

Use an isolated target directory and one Cargo build job:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR="$PWD/native/target/lane-c"
cargo test --manifest-path native/Cargo.toml --test output_selection --test output_timing
cargo test --manifest-path native/Cargo.toml --lib output
make
# Point this at an existing GuitarPedal bench, treated as read-only.
export BROWNNOTE_BENCH="/path/to/GuitarPedal/Validation/bench/bench"
export BENCH="$BROWNNOTE_BENCH"
export BROWNNOTE_BIN="$PWD/build/brownnote"
export BROWNNOTE_REQUIRE_BENCH=1
make native-test
```

`output_selection` checks duplicate names, missing/stale IDs, explicit default
fallback, defaults, invalid whole configurations, both sample rates, unknown
buffer bounds, and honest silent state. `output_timing` checks stream identity,
frame reset, observed buffer sizes, overflow, and zero allocations/frees in
timestamp publication. It also checks that synthesised callback durations and
CPAL leads summarize as min/p50/p95, that zero and single observations report
placeholders instead of numbers, that an unavailable prediction is counted
rather than read as a zero lead, and that a probed real callback neither
allocates nor frees while its full queue counts drops. These are deterministic
software tests, not physical disconnect/reconnect evidence.

On a Mac with a physical default output:

```sh
cargo run --manifest-path native/Cargo.toml --example output_probe
cargo run --release --manifest-path native/Cargo.toml -- --check-audio
```

The example opens the current default by its explicit CPAL ID at 44.1 and
48 kHz, uses a silent sample to check voice lifecycle, and verifies real
callback timestamps and fresh stream identities. It fails if either rate is
unsupported. It prints neither device IDs nor names. The binary probe plays
pad 0, checks frames and signal, and prints the callback-duration,
observed-frame and presentation-lead summary described above. It measures no
round-trip latency and no listening quality.

## Remaining manual gates

1. After the second PR wires selection and preferences, choose built-in
   speakers and a second headphone, Bluetooth, or AirPlay output. Verify
   keyboard navigation and clear running/silent/retrying status.
2. While voices play, change the device and rate. Verify old voices stop,
   the UI reports that, and new hits use the new rate/device.
3. Remove or disable the selected second output. Verify silent fallback,
   remembered-ID retry failure, explicit system-default fallback, reconnect
   and successful remembered-ID retry, including a restart with a stale ID.
4. Have a maintainer listen for correct output and unwanted artifacts.
   Record the named-device procedure without publishing IDs, serials or media.
5. Measure the real round-trip latency. The probe reports callback cost,
   delivered buffer lengths and CPAL's presentation estimate separately, and
   says so; none of them is a round-trip measurement, and only a physical
   input/output loop can supply one.

Built-in Mac audio alone cannot prove the second-device loss/reconnect gate.
No audio-interface, other-platform, listening, or latency support is claimed.
