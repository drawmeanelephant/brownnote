# Application project transactions

Refs #100. This slice supplies `project::session` and `ui/projects.rs`, under
D3–D5, D7 and D8. It does **not** close #100. D6 autosave, manual
save/reopen/relocation listening, and recovery/media collection policy remain
open.

## Application controls

Activate **PROJECT** in the compact pad header with Tab and Space. Enter an
absolute project directory, then activate **SAVE / SAVE AS**, **OPEN (REPLACE
KIT)**, or **LIST RECOVERY**. Paths are literal, including spaces and trailing
whitespace; they are not shell commands or tilde-expanded paths.

Opening stops old voices only after the whole replacement is ready. Listing
recovery does not apply it: choose a specific evidenced generation. Save
recovered content to a new directory. **CANCEL OPEN** or Escape cancels an open
or recovery request; an admitted save cannot be cancelled. Escape returns to
the pads when no save is still running. **BACK TO PADS** also leaves the panel.

`State` owns the bounded project worker. Its poll runs before output, device,
media, and pedal polling. The shipped view gives project controls priority
over device/import panels, and project keys cannot trigger pads or change a
hidden import panel. The panel scrolls and all controls are keyboard reachable.

The pedal module includes the narrow `projects/pedal_state.rs` helper. It
checks the private landed chain before snapshotting and initializes its
private landed/requested metadata on open. A generic `Ready` status alone
does not prove that retained sound belongs to the currently edited chain.

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

The validator compiles and tests the actual application binary and its
`ui::projects` module. It no longer creates temporary mounting glue, copies
`State`, or adds a path-bootstrap integration target.

The tests use the actual `State` service methods, worker, Rune button/key
callers, registry and virtual mixer. Keyboard save/open/recovery flows use the
shipped `ui::view`, `ui::on_key`, and `ui::poll` at 820×600. They cover custom
float32 WAV, PCM16 stereo,
all pad settings and chains, retained audio beyond 30 seconds, physical
directory relocation, deleting original paths and the opened project before
save-as, missing/corrupt media, hash-valid bad retained WAV, failed saves,
candidate output/partial queue refusal, runtime capacity failure, pending work,
overlapping requests, immutable save snapshots, cancellation, stale completion,
and explicit recovery/save-as.

The script forces the local crate fresh and checks every expected checkout-local
project-control test name, both in the focused run and full `make native-test`.
A zero exit from an unrelated stale test executable is not enough. Registered
`project-path` and `project-error` scenes also run through the actual binary's
GPU renderer, checking the minimum window and accessible controls.

These deterministic virtual-output tests do not establish listening,
physical-output, OS power-loss, or packaged pedal evidence.
Store fault-injection coverage remains in `project_store`; no new power-loss
durability claim is made here.
