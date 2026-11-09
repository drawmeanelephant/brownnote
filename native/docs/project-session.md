# Application project transactions

Refs #100. This slice supplies `project::session` and `ui/projects.rs`, under
D3–D5, D7 and D8. It does **not** close #100. D6 autosave, permanent application
mounting, manual save/reopen/relocation listening, and recovery/media collection
policy remain open.

## Exact integrator wiring

The lane does not edit `ui.rs`, `ui/view.rs`, `main.rs`, `lib.rs`, either Cargo
file, `ui/media.rs`, scheduler code, or the takes model. There are no new
dependencies. `project/mod.rs` exports the new `session` child module. The only
decoder change is making the existing `kit::decode_wav_max` public.

Apply these small root changes:

1. In `ui.rs`, declare `pub(crate) mod projects;`. Add
   `pub projects: projects::Projects` to `State`. Initialize it with
   `projects::Projects::default()` in `State::with_cache`. No constructor
   filesystem access occurs. It starts one bounded project worker.
2. In `ui::poll`, call `let projects_changed = projects::poll(s);` before
   output/device/media/pedal polling. Include it in `changed`. While
   `s.projects.busy()`, call `tx.wake_after(Duration::from_millis(10))`.
   Starting a request invokes the existing `State::wake`; no new setup hook
   or waker field is needed.
3. At the start of `ui::on_key`, before pad shortcuts and modifier checks:
   `if projects::on_key(s, event, tx) { return true; }`.
   Project-panel keys must never also trigger pads. This consumes the global
   pad shortcut handler, not Rune's focused text-input/button handling.
4. In `ui/view.rs`, add `projects::header(s)` to the header. When
   `s.projects.visible`, render `projects::panel(s)` instead of pads/editor.
   Choose the project panel before the device/import panels, or close those
   panels when opening it. Keep device preferences and import UI unchanged.
   The project panel has its own scroll area and keyboard-accessible path,
   save, open, recovery-list, generation-choice, and cancel controls.
5. At the end of `ui/pedals/mod.rs`, add
   `include!("../projects/pedal_state.rs");`. The owned helper lives in this
   feature but compiles inside the pedal module. It checks the private landed
   chain before snapshotting and initializes its private landed/requested
   metadata on open, without exposing those fields or regenerating audio.
   This one-line addition is necessary: a generic `Ready` status alone does not
   prove that a retained sound belongs to the currently edited chain.
6. Compile and run the tests embedded in `ui/projects/tests.rs`, then repeat
   the actual shipped application's full `make native-test` and `make test`.

There are no `main.rs`, `lib.rs`, Cargo, engine, or device-service changes.
The separate output is created through `Projects::open_output`; production
uses `Output::open_selected` with the active output's machine selection.
An already silent/disabled output stays silent/disabled. Failure does not
silently substitute another device or stop the old output. On platforms that
cannot open a second stream alongside the active one, open fails safely;
there is no destructive stop-first fallback.

## Save and save-as

The same explicit absolute directory field selects the save destination and
open root. Save captures all sixteen assignments, gain, pan, pitch, chains,
master, choke, and every assigned source/retained asset in `Document`. Asset
identity comes from `AssetRegistry`, not decoded floats or a filename.
Unfinished imports/renders, failed chains and changed-but-unrendered chains
block save with an actionable error. A new chain cannot persist the previous
chain's audible render.

The UI captures only small settings/assignment metadata and an owned
`Arc<SessionMedia>`. Publication, filesystem access, hashing, sync and the store
transaction run on one worker. Media is immutable and append-only. Later edits
cannot alter a captured document or its required bytes. Publication can include
additional unreferenced session objects; it never changes the saved document.

The worker creates only the chosen project directory (its parent must exist),
rejects a corrupt current document before publishing extra media, adopts
`SessionMedia` through `ProjectMedia`, then calls `Store::save(document, &[])`.
There is no second document writer. Errors retain the store's
`Unchanged`/`Unknown`/`PublishedNotDurable` distinction. No successful-save or
rollback claim is made for uncertain publication.

Save does not cancel pad work, stop voices or change runtime assignments.
The completion names the saved generation and explicitly says that edits made
during the save were not included. A save already admitted cannot be cancelled.
This slice does not claim dirty tracking, automatic save, or quit protection.

## Open and explicit recovery

`Store::open(OpenChoice)` verifies the complete closure and returns owned bytes.
The project worker decodes exactly those bytes using the existing WAV decoder,
creates all waveforms, a separate complete slot registry, bindings, and a fresh
`SessionMedia` owner. It never reopens source/media paths after preparation and
never uses cache, bench or source fallback for failed retained audio.

Source imports keep D7's 30-second limit. Retained audio is bounded by its
actual encoded byte length, not the importer duration: a render can include
tails, and a retained render reused as a source can acquire another tail.
The same PCM16/float32, mono/stereo, rate and finite-frame checks still apply.
Every declared asset must decode and fit the runtime slot table, including
unassigned assets. Capacity/decode failure rejects the entire project.

On completion the controls recheck that active settings, assignments and
pending work have not changed. A stale completion reports an error rather than
overwriting intervening edits. Then they construct a separate output and admit
master/choke settings to its real command queue. Only after every fallible step
succeeds do they cancel old imports/renders, replace registry/media/settings/
pedal state, install the candidate output and clear old meters/flashes.
Success deliberately stops old voices and discards their old scheduler/return
queues. A failed open leaves them and outstanding pad tickets untouched.
Output creation follows the existing platform seam on the control thread;
filesystem work and audio decode never run there.

The document stores global choke and source assignments, not runtime group
numbers or display filenames. Reopened groups use stable source-asset identity;
machine slot numbers are freshly allocated. A duplicated encoded startup file
with distinct legacy per-filename choke groups cannot retain that distinction
in the existing v1 format. This slice does not change the document schema.

Recovery listing uses `Store::recovery_options` on the same worker. It displays
current validation, rejected evidence, and explicit evidenced choices.
Unconfirmed snapshots are counted but never offered as saved generations.
Hash order is not chronological order; no choice is recommended or applied
automatically. Clicking a choice revalidates it through
`OpenChoice::Committed`. Cancel drops its sole completion receiver, so a late
answer cannot mutate state or reach a newer request. A bounded-queue refusal
leaves current state alone and can be retried explicitly.

Recovered content must be saved to a new, nonexistent directory; the worker
rejects existing destinations, including aliases of the original root. The
original root is not silently overwritten. Corrupt current documents are
independently rejected by the store even without a preceding recovery open.

## Reproducible verification

Run `python3 native/tests/check_project_session.py` **inside the swarm's whole
check lock, shared Cargo target and two-job wrapper**. Set
`BROWNNOTE_REQUIRE_BENCH=1`, `BROWNNOTE_BIN`, `BROWNNOTE_BENCH` and `BENCH` to
the existing real C CLI/bench.

Because the lane cannot change the integrator-owned root, the script generates
temporary test-only mounting glue from the actual checkout's `ui.rs`. It adds
exactly the proposed field, constructor, module, poll, panel and key wiring,
then imports the existing media, pedals, audio and view modules by path. It
does not create a second State model or decoder, and does not alter root file
contents. It refuses to overwrite existing probe files and removes only its
own files afterwards. A changed mounting anchor is an error, not a guess.

The probe tests use the actual `State` service methods, worker, Rune button/key
callers, registry and virtual mixer. They cover custom float32 WAV, PCM16 stereo,
all pad settings and chains, retained audio beyond 30 seconds, physical
directory relocation, deleting original paths and the opened project before
save-as, missing/corrupt media, hash-valid bad retained WAV, failed saves,
candidate output/partial queue refusal, runtime capacity failure, pending work,
overlapping requests, immutable save snapshots, cancellation, stale completion,
and explicit recovery/save-as.

The script forces the local crate fresh and checks every expected checkout-local
project-control test name, then runs `make native-test` after removing the
temporary glue. A zero exit from an unrelated stale test executable is not
enough. This proves the bounded artifact with the proposed mount, **not that
the shipped application already mounts it**. The integrator must apply the
wiring and run the real entry point before claiming application coverage.

These deterministic virtual-output tests do not establish listening,
physical-output, GPU visual, OS power-loss, or packaged pedal evidence.
Store fault-injection coverage remains in `project_store`; no new power-loss
durability claim is made here.
