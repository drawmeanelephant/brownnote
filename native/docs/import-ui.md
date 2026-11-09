# Typed-path sample import controls

Refs #99. This is the second import slice, not an issue close. It reuses
`State::import_sample`, its worker, `SessionMedia`, and the runtime registry.
Project save/reopen/relocation UI integration remains #100. Hearing the result
requires a maintainer listening check.

## Interaction

Select a pad with its performance key, then activate **IMPORT WAV** with Tab
and Space, or Command-I. The dialog names the target pad and its current
sample. **PREVIOUS PAD** and **NEXT PAD** select any of the sixteen pads,
including a pad whose sample is missing, without triggering it.

Type or paste a literal filesystem path into **Local WAV file path**. Press
Return in the field, or Tab to **IMPORT TO PAD** and press Space. Spaces,
quotes, and leading/trailing whitespace are preserved as filename characters,
not interpreted as shell syntax. Do not add shell escaping or expect tilde
expansion. No native file dialog or drag-and-drop support is promised.

While importing, the path and submit control are disabled; cancel and pad
selection remain reachable. Repeated Return cannot queue duplicate work.
Changing the target cancels the previous ticket before another completion can
land. Changing the application's selected pad externally also cancels the old
target before the media poll. Cancel/close or Escape keeps the draft path and
the old playable assignment. It does not stop playing voices.

Status distinguishes worker processing, prepared media waiting for engine
admission, success, cancellation, invalid input, busy worker, and unavailable
worker. A failed or cancelled import leaves the existing sound and unrelated
pads intact. Publication remains off the UI/audio callbacks. Unsaved session
media is not a durable project save.

## Application integration

`ui/media.rs` includes `ui/import.rs` as `media::controls`. `Media` owns the
controls, so no new root `State` field, dependency, or library export is needed.

The actual application mounts **IMPORT WAV** in the editor header and replaces
the grid/editor with the dialog while it is open. `ui::on_key` routes import
keys before performance keys. `ui::poll` checks external selection changes
before polling media completion, without short-circuiting either poll.
Existing media polling supplies worker wakeups. Closing the dialog returns
focus to the import button.

`ui::SCENES` registers `import-path`, `import-busy`, `import-error`, and
`import-success`. Scenes are presentation fixtures, not successful worker or
save evidence.

## Validation boundary

`ui/import/tests.rs` uses the real `ui::view`, `ui::on_key`, and `ui::poll`
entry points at the 820×600 minimum size. It uses the actual exported native
library, encoded WAV fixtures, existing import worker, output queues and mixer.
Keyboard events reach Rune's text editor, Tab traversal, button activation and
modal dismissal. Controlled worker gates exercise overlapping operations
without doing I/O on the UI thread. Scene checks use the registered application
scenes rather than a separate fragment host.

Required automated checks are the focused import UI tests, `make native-test`
(including GPU and required real-bench checks), and `make test` on the integrated
artifact. Normal performance keys must still play after closing the dialog.

The maintainer must still import a user WAV and listen to an already-playing
voice through replacement, then listen to the next hit. Check that errors and
cancellation leave the previous sound audible. Virtual mixer tests cannot
establish that listening evidence.

Do not close #99 until its real save/reopen/relocation gate is satisfied.
