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
excluded (D8). Project MIDI mappings and instruments will be added with their
own feature contracts, not serialized from runtime state.

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
