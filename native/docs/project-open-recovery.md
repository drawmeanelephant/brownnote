# Validated project open and explicit recovery

Refs #100. This is the backend follow-up to the transactional store, under
D3, D4 and D5. D6 autosave, application save/open UI, runtime replacement,
and listening checks remain outstanding. This does not close #100.

## Public integration seam

The actual library already exports `project::store`. This slice adds
`project::recovery` through `project/mod.rs`; it needs no root export,
dependency, manifest, UI, or engine change.

Run these calls on the storage worker, not the UI or audio callback:

```rust
use brownnote_native::project::{
    recovery::OpenChoice,
    store::Store,
};

let mut store = Store::new(absolute_project_root);
let prepared = store.open(OpenChoice::Current)?;
// No active project has changed.
```

`PreparedProject` contains the validated `Inspection` (document, exact document
bytes, generation identity) and a map of every declared media identity to owned
`Arc<[u8]>` encoded bytes. It validates document version, strict schema, settings,
references, required retained renders, and every asset's byte count and SHA-256.
It includes unassigned declared assets, not just audible pad assignments.
There is no source/cache/bench fallback and no rewriting on success or failure.
The owned bytes survive removal of the source directory. Do not reopen the
original paths after preparation.

This is **storage-integrity validation**, not runtime admission or a new WAV
decoder. The next app slice must:

1. Decode the owned media on its worker using the existing decoder. Retained
   outputs may include pedal tails longer than the importer's 30-second source
   limit; expose the existing `kit::decode_wav_max` byte-reader entry point
   rather than reopening files or copying the decoder.
2. Build an entirely separate registry, slots, previews, and project settings.
   Use `SampleAssignment::audible()` and its exact retained bytes. Do not
   regenerate pedals or substitute the source when a retained decode fails.
3. Keep the active project, registry, pending operations, output, and unsaved
   session media untouched until the complete replacement is ready. Reject
   decoder, slot, and queue failures before swapping state. A test of this
   backend is not a test of that future application transaction.
4. Swap the prepared replacement only after success. Publish/adopt the owned
   encoded media through the existing importer/store ownership contract when
   saving. Never add a second document writer.

No immediate integrator wiring is required for this backend PR. The four steps
above are the exact follow-up integration requirements, not implemented UI.

## Offer recovery, then require a choice

When current open fails, call `store.recovery_options()`. Its result separates:

- `current`: the current generation, absence, or its typed validation/read
  failure. A corrupt current document does not become an empty project.
- `recoverable`: complete validated snapshots with commit evidence.
- `rejected`: evidenced generations with invalid evidence, missing or corrupt
  snapshots, unsupported versions, or missing/corrupt media.
- `unconfirmed`: snapshot identities without evidence, excluding a valid current
  document. These are not choices advertised as previously saved projects.

Errors reading the directory/history itself are returned as errors. Per-project
and per-generation failures remain in the report, not discarded. Lists use
stable hash order, **not chronological order**. No generation is recommended
or applied automatically. All history is retained in this slice.

After the user chooses a listed identity, call:

```rust
let selected = store.open(OpenChoice::Committed(chosen_generation))?;
```

Selection rechecks commit evidence and the complete document/media closure.
The UI must still handle a failure between listing and selection. Both calls
are read-only, do not create a lock file, and leave abandoned staging untouched.
The caller can cancel without changing either the active kit or disk.
To preserve a corrupt original, offer saving recovered content to a **new**
project root through the existing publisher and `Store::save`. Opening recovery
does not authorize overwriting the original; normal save continues to reject
an invalid current document.

## Commit evidence, not another project format

The document and `media/` format and the single document-rename commit point
are unchanged. The store adds internal transaction metadata:

```text
.brownnote-store/
  committed/<document-sha256>   # empty immutable evidence file
  staging/commit.tmp            # private staging, safely cleaned on save retry
```

A marker records a generation actually observed as current, never merely a
published candidate snapshot. The store records an observed, validated old
current generation before replacing it. After publishing the new current
document and syncing both rename directories, it records the new generation.
Markers use synced staging and a no-replace hard link, then sync both containing
directories. Existing marker bytes must be empty. Marker contents do not
contain a second serialized document, a second recovery writer, timestamps,
or a new user-project schema.

A save succeeds only after its evidence is synced too. An error recording
evidence after publication is `PublishedNotDurable`, even if the document's
own sync already succeeded. `Unknown` still means publish returned an error
and might have taken effect. A valid current document can always be opened
directly. Retrying a save witnesses it and can finish its evidence.

The first store slice (#130) retained complete snapshots of both committed
documents and abandoned candidates. **It is impossible to reconstruct which
of those old snapshots committed from their bytes or filenames.** This API
does not guess. Old stores still open their valid root document without writes;
a later save records that observed current generation before replacing it.
If an old store's current document is already corrupt and no evidence exists,
its snapshots remain unconfirmed. Keep them for manual investigation, but
never label one as a committed recovery choice.

Markers are transaction evidence under the store's existing private-root,
immutable-media assumptions. They are not tamper-proof signatures and do not
support hostile concurrent filesystem mutation or forged evidence files.

## Supported-version and migration policy

The first native project schema is **v1**, introduced in
`4948198edf9a33e5b9a171b2d9feee1306db2edf` (#98). Git history through base
`c57411ad18f82531f2beaf97f6a4054a402e08b3` contains no older native schema.
The earlier browser patch workflow is not a native project format and is not
an implicit migration source.

| Input | Policy |
| --- | --- |
| v1 | Decode, strictly validate, retain exact original bytes; no rewrite |
| Versions below 1 | `DocumentError::OlderVersion`; unsupported, no migration |
| Versions above 1 within `u32` | `DocumentError::NewerVersion` before interpreting fields |
| Missing, malformed, or out-of-range version | Invalid document/JSON; no guessing |
| Unknown v1 fields | Invalid document; never silently drop fields |

There is **no supported-old migration today**, because there is no real older
native schema. No v0 schema is invented to make a migration test pass. A future
version change must name the actual supported source schema and add immutable
source fixtures, explicit in-memory migration, and read-only failure tests
before expanding support.

`native/tests/fixtures/project-v1/project.json` is an empty v1 kit encoded from
the `Document`/`Pad` defaults in the original #98 contract. It is a compatibility
fixture for that real schema, not a claim of an archived user project. Media
and retained-audio fixtures are generated behaviorally by `project_store`.
Version 0/newer inputs in tests are rejection probes only, not invented schemas.

## Verification and limits

Run `python3 native/tests/check_project_recovery.py` under the swarm's shared
validation lock and target wrapper. It runs:

```sh
cargo test --manifest-path native/Cargo.toml --test project_store -- --nocapture
cargo test --manifest-path native/Cargo.toml --test project
make native-test
```

The store tests import the real `brownnote_native` library. The script forces
this package's library and owned inputs fresh without a second target or cache
deletion. It checks the actual passing fixture names against this checkout's
source after both store runs. A stale binary from another worktree cannot
produce a passing validation report merely by exiting successfully.

The existing filesystem injector now covers evidence publication as well as
the original save transaction. Every save operation is failed before and
after, every write halfway through, then another retry fails before a final
success. An independent flush/sync audit checks the commit boundary and final
directory state. Open and explicit recovery inject errors before/after every
read/check boundary. Recovery discovery injects errors before/after listing,
checking, and reading; errors must remain visible in its result.

Fixtures cover abandoned staging, complete never-committed candidates, corrupt
current with valid committed history, missing/corrupt retained media, malformed
evidence/snapshots, legacy ambiguity, unsupported versions, stable repeated
calls, and byte-for-byte read-only behavior. Relocation moves the actual
directory, deletes original custom asset paths, opens current and historical
content, decodes retained bytes using the real decoder, and triggers the virtual
mixer without any bench/cache interface in the open path.

This is ordinary I/O fault injection on macOS/APFS with `File::sync_all`,
directory sync, hard links, and same-filesystem rename. It does **not** prove
OS power-loss durability, drive-cache flush, or behavior on other filesystems.
No physical output or listening claim is made.

Outstanding #100 gates remain application save/open/recovery choice UI and its
atomic runtime replacement, D6's separate 60-second recovery root using this
same store, bounded retention/collection policy, and manual listening after
real application save/reopen/relocation.
