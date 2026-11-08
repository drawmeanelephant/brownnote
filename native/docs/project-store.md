# Project store, first slice (#100)

Refs #100. The save transaction and read-only inspection are implemented.
#100 remains open: recovery selection, migration policy, open/apply integration,
save/open UI, and autosave are later slices.

## Layout and commit point

```text
project/
  project.json                      # sole committed document
  media/<sha256>.wav                 # shared immutable authoritative media
  .brownnote-store/
    lock                            # OS exclusive writer lock
    versions/<document-sha256>.json  # complete immutable document snapshots
    staging/
      document.tmp
      version.tmp
      media-<sha256>.tmp
```

The root and `media/` layout is the merged #98 contract. Pass this root,
not `versions/`, to `project::media::resolve_verified`. Document snapshots
share the root's immutable media. They are not independent project roots.

**The only commit point is the atomic same-filesystem rename of
`staging/document.tmp` over root `project.json`.** This is a full document,
not a pointer or new schema. Before that operation:

1. Validate the candidate document and source identities without mutating them.
2. Create and sync the directory hierarchy, including the root's parent.
   Acquire the exclusive writer lock before touching managed staging.
3. Inspect the existing document and its entire media closure. Sync its media
   and preserve its exact original document bytes in a synced snapshot.
4. Read each candidate asset into owned memory, validate its declared length
   and SHA-256, and write, flush, and `File::sync_all` its staging file.
   Stage every required source and retained render before publishing any.
5. Publish new media with atomic, no-replace hard links. Reused media must
   match the declared hash and length and is synced again. Sync `media/`.
6. Write, flush, sync, and publish the candidate's complete document snapshot.
   Sync `versions/`, staging, the private directory, and the project root.
7. Commit the document rename. Sync both the source staging directory and
   destination project directory before returning a successful receipt.

This is not JSON-only atomicity: both old and new document media closures
exist before the switch, and neither media nor snapshots are overwritten or
garbage-collected. A failed precommit save cannot change the committed
document, its snapshot, or any of its media bytes. After a publish attempt,
inspection sees complete old or complete new, not mixed media.

All complete snapshots are retained in this first slice, including candidates
that reached snapshot publication but never committed. Snapshot presence
therefore does **not** prove a document was ever the user's saved version.
No automatic recovery choice is made. Keeping all snapshots is conservative;
bounded retention and media collection need a later policy that protects both
saved and separate recovery roots. D6's later recovery writer must call this
same transaction API with its separate recovery root.

## Consumer API

The real manifest exports `brownnote_native::project::store`; these tests use
that exported artifact, not path-included bootstrap modules. No integrator
exports, dependencies, State fields, polls, or views are requested.

```rust
use brownnote_native::project::store::{MediaSource, Store};

// The root is absolute. Its parent already exists.
let mut store = Store::new(root);
let receipt = store.save(&document, &sources)?;
let current = store.inspect_committed()?; // Option<Inspection>
```

`MediaSource { id: AssetId, path: PathBuf }` supplies an encoded source file for
a document asset. The store copies and validates it, never records the path.
Omit a source to reuse already published `root/media/<id>.wav`. Duplicate or
undeclared source IDs are errors. All declared assets, even ones not currently
assigned to a pad, must be present and verified. `SampleAssignment::audible`
and the real retained-render semantics are unchanged.

`Inspection` contains a validated `Document`, exact `bytes`, and its
`GenerationId`. `inspect_committed` is read-only; it returns `None` only when
`project.json` is absent. `inspect_snapshot(id)` validates a named complete
snapshot and its media, but does not select or apply recovery. Corrupt current
documents are never silently overwritten, even if a snapshot is valid.

`SaveReceipt { generation, previous }` is returned only after all required sync
operations succeed. On failure, branch on `SaveFailure::commit`:

| State | Meaning |
|---|---|
| `Unchanged` | No commit was attempted. The saved document is untouched. |
| `Unknown(candidate)` | Publish returned an error and may have taken effect. Inspect current state; do not report rollback or durable success. |
| `PublishedNotDurable(candidate)` | Publish succeeded, but a postcommit directory sync failed. The new complete document is visible; durability is unproven. |

Retry after any failure, including after an uncertain commit. The lock is
released on all exits. Retries verify existing immutable content, remove only
recognized owned staging entries, and converge on the same document identity.
Unknown staging entries are preserved; cleanup never recurses and never
deletes media, versions, or another recovery root.

### #99 immutable importer

The proposed `import::MediaPublisher::publish(&AssetRef, &[u8])` can append
verified, synced immutable media without taking the document lock. The store
uses no-replace hard links too. If another publisher wins a same-ID race,
the store verifies and syncs the existing file rather than overwriting it.
After import, call `save(document, &[])` for media already inside this root.
An injector can implement `store::FileSystem`; no production import types
are copied here and no import module is edited.

Unassigned or cancelled import media is not collected by this slice. Store
staging is private to the store, not the importer's staging namespace.
Both writers must obey immutability. They do not support hostile concurrent
filesystem mutation, symlink substitution races, or externally replacing a
lock file. Managed directories and files reject existing symlinks.

## Validation and limits

Run on the real manifest with a worktree-local target and one Cargo job:

```sh
CARGO_BUILD_JOBS=1 cargo test --manifest-path native/Cargo.toml \
  --test project_store -- --nocapture
CARGO_BUILD_JOBS=1 cargo test --manifest-path native/Cargo.toml --test project
make native-test
```

The filesystem fault harness wraps actual filesystem operations, not a second
storage implementation. It enumerates every operation of fresh saves,
replacement saves with shared media and abandoned staging, and replacement
saves where a concurrent immutable publisher wins. It fails immediately
before and after each operation and halfway through each write. Each fault
case checks exact old-or-new state and retained A bytes, then fails another
retry on that same project before a final successful retry. Read-only
inspection has a separate before/after fault matrix.

An independent operation audit tracks flushed and synced files and directory
changes. It requires every staged file and directory to be synced before
commit, and both rename directories to be synced before success. Removing a
required sync therefore fails the audit rather than shortening the matrix.

Other checks cover vanished/corrupt sources, corrupt current with an intact
snapshot, newer/older version rejection without overwrite, exclusive writers,
symlink rejection, safe cleanup, and actual directory relocation after deleting
owned originals. The relocated retained WAV is decoded by the real decoder
and triggered through the virtual mixer without invoking bench or cache.
This proves retained data and deterministic signal, not listening or hardware.

The local filesystem boundary is macOS 27.3 APFS with Rust `File::sync_all`
for files and opened directories, same-volume hard links and rename. Ordinary
fault injection does **not** prove OS power-loss durability. No power cut,
filesystem crash, drive-cache flush (`F_FULLFSYNC`), or physical-device test is
claimed. Other filesystems/platforms are untested; unsupported directory sync
or hard links must produce an error, not a successful save.
