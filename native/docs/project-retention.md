# Project retention and media collection

Refs #197. This policy bounds superseded recovery history and makes
unreferenced import media reachable to the store. It does not change the
document format, media naming, the single document-rename commit point, or
commit-evidence semantics. A save never deletes anything automatically.

## Protected set

Collection never removes any of the following:

1. `project.json`, even when it is corrupt. A corrupt current document is
   preserved for investigation or repair; collection cannot rewrite it.
2. The current document's complete media closure: every `Document.assets`
   entry, including unassigned assets, retained pedal/plugin renders, the
   sampled instrument's source and per-note renders (`InstrumentSource`,
   `InstrumentRender`), and retained live-take audio (`LiveTakeRender`).
3. Every generation the recovery listing still offers in the project root.
   The listing and the plan use the same policy, so a generation is either
   retained by both or retired by both. A retained generation is always
   retained with its snapshot, its evidence marker, and its complete media
   closure.
4. Every generation the D6 autosave root still offers when collection runs on
   that root. Collection on the saved project root never reads or writes the
   D6 machine recovery directory. A recovery copy from #196 is never treated
   as an unreferenced cache of the saved project.
5. Unconfirmed snapshots and their declared media, by default. This covers
   first-slice snapshots with no evidence, abandoned candidates, and manual
   repair material.
6. Evidence-backed generations that do not validate today. Their snapshot and
   evidence stay for manual investigation; media their document declares is
   protected when the document can be parsed.
7. Every entry outside the managed names. Unknown files, unknown names inside
   `media/`, `versions/`, and `committed/`, directories, symlinks, staging
   entries, `autosave.json`, and anything outside the store layout are
   reported as unknown and never removed.

The D6 check keeps both the copy root's current generation and
`autosave.json`. The record is an unknown root entry to the store, so no
collection path lists it for removal.

## Bound and ordering rule

The bound is a byte budget, `RetentionPolicy::history_bytes`
(`DEFAULT_HISTORY_BYTES` is 64 MiB). It is charged only for superseded history
overhead:

- snapshot JSON bytes of retained non-current generations;
- media bytes first needed by those generations, excluding media the current
  document already requires;
- snapshot bytes and media of kept unconfirmed or rejected generations.

The current document's snapshot and media are the working set, not history.
They are never charged to the budget.

Selection is **stable generation-identity order** (the SHA-256 document
identity), not chronological order. A recoverable generation is retained when
its snapshot bytes plus its not-yet-protected media bytes fit the remaining
budget; otherwise it is retired. This is deterministic and repeatable, and it
needs no timestamp. Recovery listings are hash-ordered for the same reason.

No timestamp is added to commit evidence, and no field is added to
`project.json`. Evidence files stay empty immutable markers; the bound is
derived from identities and file sizes the store can already verify. The
policy deliberately does not claim to retain the newest generations. The
store has no recorded chronology, so it must not invent one.

`recovery_options()` uses the default policy. `recovery_options_with(policy)`
and `retention_plan(policy)` must be called with the same policy by a caller
that wants the listing and the collection to agree.

If the current document is absent, collection does not prune any recoverable
generation, so the project keeps at least one complete recovery copy. If the
current document is invalid, collection also refuses media removal and does
not prune history: without a valid current closure the store cannot prove
which bytes are unreferenced.

### Refusal behaviour

The store never deletes a protected generation to meet the bound.

- `RetentionPlan::over_budget` is true when retained history exceeds
  `history_bytes`, or when an unreadable retained document makes the bound
  uncertifiable.
- `RetentionPlan::media_refused` is true when a retained document's media
  closure cannot be parsed. In that case no media is removed at all.
- With `RetentionPolicy::require_bound = false` (the default), `collect`
  performs the safe removals it can and reports the overage. With
  `require_bound = true`, `collect` returns `StoreError::RetentionBoundUnmet`
  before deleting anything.

## Unconfirmed legacy snapshots

Unconfirmed snapshots are kept by default. `UnconfirmedSnapshots::Remove` is
the explicit, recorded decision that permits their removal:

- The plan lists exactly those snapshot paths and bytes.
- `collect` returns the removed snapshot count and byte total in its
  `CollectionReceipt`, which is the record of the decision.
- Their exclusively referenced media becomes collectable only after the
  snapshot is removed and no retained generation references it.
- The current generation is never unconfirmed for this purpose and is never
  removed.
- The same rule applies inside the D6 recovery root.

An interrupted collection can remove evidence before its snapshot, leaving
the snapshot unconfirmed. The default policy keeps it. A later explicit
`UnconfirmedSnapshots::Remove` sweep can reclaim it.

## Plan and apply

```rust
use brownnote_native::project::store::{RetentionPolicy, Store};

let mut store = Store::new(absolute_project_root);
let plan = store.retention_plan(&RetentionPolicy::default())?; // read-only
let receipt = store.collect(&RetentionPolicy::default())?;     // writer lock
```

`RetentionPlan` contains:

- `usage`: snapshot, evidence, and media counts and bytes, plus unknown entry
  count and bytes;
- `retained`: the current document and every generation the policy keeps;
- `evidence`, `snapshots`, `media`: the exact `Removal` paths and bytes;
- `unknown`: reported paths that are never removed;
- `history_bytes`, `protected_bytes`, `reclaimable_bytes`, `over_budget`, and
  `media_refused`.

Planning is read-only. Two plans on an unchanged directory are equal and leave
the directory byte-for-byte unchanged.

`collect` acquires the same `.brownnote-store/lock` a save uses, recomputes
the plan under that lock, and verifies every removal is a regular file with a
managed name and the expected size before deleting anything. The order is:

1. evidence markers (stale evidence, and evidence of retired snapshots);
2. retired snapshot files;
3. media files that no retained generation references.

Each containing directory is synced after its phase. Evidence is removed before
its snapshot, so evidence never points at a snapshot the store removed. Media
is removed last, so the last copy of protected media is never taken. A failure
can leave an unconfirmed snapshot or unreferenced media, but never a missing
protected file or unloadable project. A retry is safe, and a second run after a
successful collection removes nothing and reports zero bytes.

The project worker exposes both operations as
`session::Request::Retention { root, policy, apply }` and returns
`session::Completion::Retained { plan, receipt }`. Filesystem work and hashing
stay off the UI and audio threads. A user-facing sweep control is optional; the
store API is the only deleter.

## Import media

Unassigned or cancelled import media is published under `media/<sha256>.wav`
and is now reachable to collection. When no retained generation references
that identity, the plan lists the file and `collect` removes it. If any
retained generation references the same bytes, the file stays. Non-media names,
import staging names such as `.import-*`, and unknown entries are never
touched.

## Locking and read-only calls

`collect` is refused by the existing writer lock while a save holds it
(`StoreError::Io(WouldBlock)`), and a save cannot run while collection holds
it. The application's project worker also serializes open, recovery listing,
save, and collection, so they do not overlap. `open` and the recovery listing
remain read-only and do not create a lock file or change the directory.

## Verification

```sh
cargo test --manifest-path native/Cargo.toml --test project_store
cargo test --manifest-path native/Cargo.toml --test project
python3 native/tests/check_project_recovery.py
make native-test
```

The store fixtures cover:

- a read-only, repeatable plan with per-root usage and exact removals;
- unknown files in each managed directory and at the root;
- unreferenced import media removal, including a referenced import that stays;
- stale evidence-without-snapshot removal;
- a retired snapshot and its evidence removed together, with evidence never
  remaining for a missing snapshot;
- injected interruption at every removal boundary, preserving the current
  project, protected media, and evidence consistency;
- unconfirmed legacy snapshots kept by default, an explicit removal decision,
  and refusal of an unmet bound;
- D6 copy-root protection and its `autosave.json`;
- writer-lock refusal and convergence after a successful run;
- the project worker's plan and applied receipt.

This is ordinary filesystem fault injection on macOS/APFS. It does not prove
OS power-loss durability, other filesystems, cloud or cross-machine cleanup,
or listening. The maintainer-only check remains: after collection on the real
machine, reopen a pre-collection project directory and confirm its retained
audio still plays.
