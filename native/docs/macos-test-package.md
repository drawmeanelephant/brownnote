# Relocatable macOS arm64 test package

This is a **base-pad-only unsigned test artifact with generated tones**.
It is not the recording kit, a full saveable instrument release, a signed
`.app`, or proof of audible output. GuitarPedal and the C CLI are not bundled.
The existing FUZZ/DELAY/CHORUS/CHAIN pad names still appear, but the five
sources in this artifact are test tones, not pedal renders.

## Build and verify

Requirements: macOS on Apple silicon, Rust 1.99, Python 3.11 or newer (with
the current `tarfile` data filter), Xcode command-line tools, and access to
the locked Cargo sources. Install the toolchain's `rust-docs` component for
its standard-library copyright/license record. No bench is needed to build this base artifact.
Read `DISTRIBUTION.md` (or `distribution-inventory.md` in the repository)
before adding assets or binaries.

From the repository root:

```sh
native/scripts/package-macos.sh
native/scripts/smoke-package.sh build/packages/brownnote-native-macos-arm64-<version>-<commit>-unsigned-test.tar.gz
```

During a resource-constrained swarm, run **each whole command through the
integrator-provided shared Cargo/lock wrapper**, from this checkout. Do not
create an independent target or delete another lane's artifacts.

The packager builds the actual `brownnote-native` release executable with
`--locked`, using Cargo's reported target directory. It preserves that file's
executable mode and uses the existing executable-relative flat resource
layout. It refuses to replace an existing artifact. An optional output
directory argument permits separate runs. There is no installer or launcher
that injects `--samples`.

It also refuses a build it cannot identify: a dirty working tree, an expected
commit (`--expected-commit`) other than `HEAD`, and a release tag (`--tag`)
that is not `v<version>` for the version in `native/Cargo.toml`. The archive
name carries that version beside the 12-character commit, so tag, filename and
the executable's own report agree.

### Tagged release build

A maintainer pushes a `v<version>` tag. The `release` workflow runs on it and
calls `test_package.py`, then `package-macos.sh` with the tag's commit and
name, then the required `smoke-package.sh`. Finally it creates a **draft**
GitHub Release for that tag with exactly four assets: the archive, its
`.sha256`, its `.smoke.json` and the `build-record.json` extracted from the
archive. It does not publish the release, and no script or workflow creates or
pushes the tag.

The job fails and publishes nothing when the working tree is dirty, when the
tag commit is not `HEAD`, when the tag is not `v<version>` for the version in
`native/Cargo.toml`, or when the smoke report's status is not `passed`. A
re-run for a tag that already has a release fails instead of overwriting,
deleting or replacing its assets. Once a release is created, publishing it,
signing it or attaching notes remains a maintainer decision.

The packaged executable reports its own identity offline, before any kit,
device or window work:

```sh
"/absolute/path/Brownnote Native Test/brownnote-native" --version
brownnote-native 0.1.0
commit <full 40-character source commit>
```

`dirty` appears on the commit line only when the build had uncommitted changes,
which packaging refuses. The printed commit is the `source_commit` in the
archive's `build-record.json`, and smoke requires the two to agree.

Outputs:

- `.tar.gz`: executable, generated kit, notices, required source and records.
- `.tar.gz.sha256`: SHA-256 of that exact archive.
- `.tar.gz.smoke.json`: written by smoke, passed or failed, with the matching
  archive digest, source/binary identity, checked paths and explicit omissions.

CI's `native-package` workflow builds on an arm64 macOS runner and invokes
the same required smoke. It uploads the artifact/checksum/report even after
a smoke failure when those files exist. An upload is **not** a passing smoke:
read the workflow result and `.smoke.json`. A runner without a Metal adapter
fails rather than treating a skipped GPU check as passed.

## Install and run manually

Verify the checksum alongside the downloaded archive, then extract:

```sh
shasum -a 256 -c brownnote-native-macos-arm64-<version>-<commit>-unsigned-test.tar.gz.sha256
tar -xzf brownnote-native-macos-arm64-<version>-<commit>-unsigned-test.tar.gz
cd /tmp
"/absolute/path/Brownnote Native Test/brownnote-native" --version
```

Move/copy the **whole directory**, including `resources`, not the executable
alone. Spaces in the install path are supported by the smoke.
The artifact has no Developer ID signature or notarization. Apple silicon
linkers can add an ad-hoc signature; that is not a trusted distribution
signature. Download quarantine/Gatekeeper behavior is untested. Do not disable
system security controls to make a test pass.

`--list-pedals` must exit 3 and explain that `tools/brownnote` is missing.
Base pads do not require that tool. A user may separately install compatible
tools via the documented `BROWNNOTE_BIN`/`BROWNNOTE_BENCH` overrides, but that
configuration is not part of this artifact's tested support.

## Resource and writable locations

Samples resolve to `resources/recordings/` beside the actual executable,
not the current directory. The shipped build contains no `tools/`.
The existing runtime overrides remain authoritative; the smoke clears all
`BROWNNOTE_*` resource overrides and runs without `--samples`.

Writable defaults are under the user's home, never inside the installation:

| Purpose | macOS default |
|---|---|
| Pedal cache | `~/Library/Caches/Brownnote/pedals` |
| Recovery location | `~/Library/Application Support/Brownnote/recovery` |
| Output preferences | `~/Library/Application Support/Brownnote/preferences` |

These locations are not a second project format. Projects belong to the
existing directory/JSON/media store. A writable recovery directory alone
does not prove saving, autosave, recovery or an open-project UI.

## What smoke actually proves

Smoke checks the checksum, extracts only regular files/directories, and
runs the **packaged release executable** from an unrelated directory with
an invalid decoy cwd kit. It triggers pads using all five sources, checks
active voices and non-silent mixer meter data through Rune's GPU-rendered
tree, and checks the real missing-pedal CLI error.

It copies that installation twice into space-containing paths, removes each
previous **owned temporary** installation, and repeats those checks. It then
deletes only its temporary bundled samples and requires asset errors and
disabled voices, proving no hidden checkout/cwd fallback. It never renames,
deletes or hides real checkout recordings, targets, builds or vendor trees.
This is logical relocation evidence, not a clean-machine sandbox.

Before that deletion, smoke drives the packaged app's own import and
**PROJECT** controls through Rune scripts (typed paths and clicks, no CLI
shortcut), with project and installation paths that contain spaces:

- Import a generated quarter-second WAV onto pad 01, save a project, and
  require the document and `media/` to own those exact bytes. Delete the
  original WAV, move the project, move the installation, relaunch, open the
  moved project, and require pad 01 to play the project-owned sample.
- Save into a read-only copy. The app must report the saved document
  unchanged, every project file must stay byte-identical, and the same
  generation must reopen once the copy is writable again.
- Open a copy with the custom media file deleted. The whole open is rejected
  and the current kit keeps playing.
- Open a copy whose `project.json` is malformed. The open is rejected, LIST
  RECOVERY offers the committed generation, choosing it opens that generation,
  and the corrupt original is left as it was.

These runs use the silent virtual mixer. They prove the packaged project
workflow, not audible playback, power-loss durability or autosave.

Three existing deterministic output scenes exercise settings/error UI after
relocation. They are virtual fixtures, not audio-device selection or recovery
evidence. The isolated HOME's documented cache/recovery/preference locations
receive filesystem write/read/remove probes. The current app does not consume
the recovery location and does not create a pedal cache without rendering;
these probes do not claim application persistence. The existing exported
library `paths` tests exercise production writable-path resolution separately.

## Support and remaining gates

| Configuration / gate | Evidence |
|---|---|
| macOS arm64, built-in audio only | D1/D2 target scope; no wider hardware support claim |
| Release compile, dynamic dependencies, checksums | Required by package script; inspect exact build record |
| Tagged build and draft release | `release` workflow on a maintainer `v<version>` tag; archive version, full commit and `--version` agree; four draft assets, nothing published |
| Real executable relocation, default five-source kit | Required GPU/headless smoke at three locations; silent mixer only |
| GPU UI scenes | Required by smoke and `make native-test`; not audio evidence |
| Engine/library/import/store/device tests | Separate `make native-test`; not relocated full workflow |
| C pipeline / real upstream bench | Separate `make test` and required `pedals_live`; tools are not packaged |
| Audible built-in output / listening | **Open**, maintainer must run `--check-audio`, play pads and listen after relocation |
| Physical output loss/retry/fallback | **Open**, deterministic scenes do not close it |
| Custom import → save → quit → move → reopen | Required packaged UI smoke after relocation; silent mixer only, listening **Open** |
| Failed save / recovery / missing project media | Required packaged UI smoke (read-only save, missing media, malformed document); power loss and autosave not covered |
| Clean-machine downloaded installation / Gatekeeper | **Open**, no clean-machine or signing evidence |
| Recording asset rights / final distribution review | **Open**, only generated test tones ship; [`recordings/PROVENANCE.md`](../../recordings/PROVENANCE.md) records provenance, while rights holder, permission and obligations stay undecided. The packager refuses repository recordings |
| Intel macOS, Linux, Windows, external audio hardware | Not supported by this artifact |

Packaging landed under #103 and that issue is closed. The **Open** rows
above stay with the maintainer, including clean-machine downloaded
installation and Gatekeeper. A tagged build and its draft release do not
close them: the release is a place to carry the identified artifact, not a
support claim. No signing/notarization route or roadmap
decision changes here. Before treating the packaging work as
release-ready, record the exact artifact hash and head for the
clean-machine, device and listening checks, and play the reopened project
audibly. Manual listening must confirm that pads start/stop, panic and
device changes behave correctly; an agent cannot infer this from meter
values.
