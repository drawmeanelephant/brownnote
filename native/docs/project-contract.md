# Project contract, first slice (#98)

This is a provisional v1 kit contract. Do not freeze the format until the
companion #104 pure performance contract lands. It does not save projects or
allocate mixer slots.

## Document and media

A project directory has `project.json` and `media/<sha256>.wav`. The hash covers
the exact encoded bytes, before decoding or resampling. No stored absolute,
user-selected, or cache path is accepted. Pad array indices 0–15 are stable
`PadId`s, unrelated to engine slots and choke groups. Multiple pads reference
one asset. The document stores settings and existing typed ordered pedal
chains, not UI state. Unknown fields and unsupported versions fail explicitly.

All required render outputs are retained media. A nonempty pedal chain without
its retained output cannot be saved as a complete document. Loading must use
`SampleAssignment::audible`, verify the declared byte count and SHA-256, and
report an error if it is unavailable. Never silently regenerate or substitute
the source. Catalog-specific pot limits are checked when editing/rendering,
not by this catalog-independent persistence layer.

`resolve_verified` reads only. It rejects canonical paths outside the project,
missing files, non-files, byte-count mismatches and hash mismatches. Diagnostics
contain IDs, not machine paths. Verification is not a concurrent-writer lock:
#100 owns atomic commit, open-handle lifetime, syncing and recovery.
`DisposableCache` has no document representation.

## Timing agreement with #104

The current document persists a kit only. Future take serialization belongs
here, using #104's semantic IDs and immutable integer nanosecond originals
relative to each take, with its recording tempo. Playback derives musical
positions at the current tempo; device frames and MIDI PPQ are boundary
conversions. Quantize-off must reproduce originals exactly. No take, tempo,
instrument backend, or placeholder arbitrary JSON schema is invented in v1.
Adding those document fields requires an explicit format-version decision.

Machine audio/MIDI device selection, window layout and cache location are
excluded (D8). Instrument configuration will be added with its own feature
contract, not serialized from runtime state.

## MIDI mappings (#105, D8)

Project format v1 now accepts an optional `midi_mappings` field containing the
MIDI layer's versioned `midi::input::MappingsDocument`. No second mapping format
or storage file is introduced. Missing or `null` means no mappings;
`Document::default()` omits the field when encoded. Existing v1 fixtures load
unchanged, and `Document::mappings()` resolves them to an empty mapping bank.
The project version remains 1 because this field is optional. Older app builds
that do not know the field still reject it rather than silently losing it.

Mappings use channel (zero-based 0–15) plus note or CC number, never a device
identity. The DTO preserves one-shot pad targets, held instrument IDs and CC
actions. `Document::validate`, `encode` and `decode` use the MIDI layer's public
`to_mappings()` validation. Invalid values, duplicate keys and unsupported
mapping versions return `DocumentError::MidiMappings(MappingDocumentError)`.
Unsupported project versions retain the existing typed version errors, checked
before interpreting the project schema.

The store commits the complete document, so current saves, historical recovery
snapshots and relocation all retain mappings without special media handling.
The session worker saves the captured DTO and prepares validated `Mappings`
in `session::Prepared::mappings` for both current and explicit recovery opens.

### Integrator wiring

The project APIs are ready; mounting them in the MIDI/project UI is outside this
lane's ownership. The current `ui/midi.rs` handle keeps its input private and
needs capture/apply accessors. No MIDI library API is missing.

1. Capture the controller's `Mappings` with
   `Snapshot::capture(...)?.with_midi_mappings(controller.mappings())` for every
   save/snapshot, including dirty checks and recovery saves.
2. On a successful project replacement, apply `Prepared::mappings` through
   `Controller::set_mappings` at the same commit boundary as the other project
   state. Stop/reconcile the old controller's held notes first; do not change
   mappings on a failed or cancelled open. Opening a legacy project must clear
   the previous project's mappings, not carry them across.
3. Leave the machine's selected port and reconnect-name hint unchanged.

`cargo test --manifest-path native/Cargo.toml --test project_midi` exercises
the exported document, store and asynchronous session APIs, including old v1
bytes, typed failures, channel-sensitive recall, recovery and relocation.
This is deterministic software evidence, not named-controller evidence.
The #105 `needs-hardware` close gate stays open.

## Library integration and validation

`native/src/lib.rs` exports `project`. Its regular dependencies are:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "=0.10.9"
```

`serde_json` is a regular dependency, and the lockfile records the hash
dependency. No UI fields, polls, view slots or engine edits are needed.

`python3 native/tests/check_contracts.py project` runs the real manifest's
project target and honors `CARGO_TARGET_DIR`. The integration test imports
`brownnote_native::project`, so it validates the exported library rather than
a path-included copy. Run:

```sh
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$PWD/native/target/lane-a" \
  cargo test --manifest-path native/Cargo.toml --test project
make native-test
```

Fixtures cover every pad, boundary settings, ordered pots/mix, shared assets,
relocation after deleting the original, retained bytes without bench/cache,
version failures and read-only integrity errors. They do not claim audible
playback, import validation, or crash-safe storage.
