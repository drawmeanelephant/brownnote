# Unsigned test artifact distribution inventory

This inventory covers the flat macOS arm64 **base-pad-only test artifact**,
not a release or a claim that all repository assets can ship. Packaging
landed under #103; the clean-machine and Gatekeeper review stays open.
Review this inventory again when the lockfile, Rune revision, kit, or bundled
tools change. The package generator rejects unreviewed license identifiers
and missing notices. This is an engineering inventory, not legal advice.

## Contents and obligations

| Component | Evidence and distribution choice |
|---|---|
| Brownnote native binary | Repository MIT `LICENSE`; copy it with the artifact. Build only `brownnote-native`, not the showcase or C CLI. |
| Rust dependencies | `Cargo.lock` and target-filtered `cargo metadata --locked` identify the conservative macOS graph, including build/dev crates. Preserve actual source notices recursively, including Apache NOTICE and Unicode/BSD/Zlib notices. `dependency-inventory.json` records every version, declared license, origin, and included notice. It is not an exact binary link map. |
| Rune | Git revision `cbaf637dfe5fcf3ec8b3f1c3ad27246b69742fbf`, MIT, copyright 2026 Anomaly. Its workspace crates inherit the upstream root notice, which is included. |
| Inter fonts embedded by Rune | Native `text/default_fonts.rs` embeds unmodified Inter Regular, Medium and SemiBold. Font name tables say copyright 2016 The Inter Project Authors, SIL OFL 1.1. Include that attribution and full OFL; preserve the font license rather than relabeling fonts MIT. IBM Plex Mono is also conservatively credited, although the pinned default native font list does not embed it. No system fonts are copied. |
| Symphonia and opuscule | MPL-2.0 components are pulled in by Rune, even though Brownnote's kit decoder uses hound. Include their full unmodified registry source in `corresponding-source.tar.gz` and their license notices. Recipients may obtain and modify those sources under MPL-2.0; Brownnote does not restrict those rights. opuscule's Xiph notice is also preserved. |
| Missing upstream crate notices | Some crates omit standalone notices from their registry archive. `package-notices.json` preserves the notices from each crate's `.cargo_vcs_info.json` commit, with repository/revision provenance. Font entries record the upstream OFL content blob SHA. |
| dispatch 0.2.0 | Its published manifest declares MIT, author Steven Sheldon, repository `SSheldon/rust-dispatch`. Neither the published crate nor its pinned repository root contains a standalone license notice. Preserve its complete original source, metadata and attribution in the source archive; do not manufacture a copyright notice. The Brownnote MIT license provides the same permission/disclaimer text, but is not represented as an upstream dispatch notice. A maintainer distribution review remains a release gate. |
| Apple frameworks / Rust standard library | `otool -L` must show only `/System/Library/` and `/usr/lib/` dependencies. No Homebrew dylibs, Apple frameworks, SDK, or system fonts are copied. Include the building toolchain's `COPYRIGHT-library.html` and license texts for statically linked standard-library code. The conservative text collection is not a claim that compiler-only GPL code is linked. |
| Sample recordings | **Excluded** from this artifact. [`recordings/PROVENANCE.md`](../../recordings/PROVENANCE.md) names all seven files and records the generator, command, inputs, effects and verification for each. The rights holder, the permission the maintainer relies on and any obligations remain **unknown** there: the root MIT license covers source code, not these assets, and the repository states no asset grant. The packager refuses a bundled audio file whose SHA-256 matches a recording, and refuses to copy a `recordings/` file unless the declaration names it. A maintainer rights decision is still required before any recording kit is distributed. |
| Test tones | The packaging script generates five one-second PCM16 mono tones using arithmetic only. They carry no recording/generator/plugin input. Filenames follow the existing kit contract; the FUZZ/DELAY/CHORUS/CHAIN labels do **not** mean those tones are processed effects or the original kit. The generator is covered by Brownnote's MIT license. The packager refuses the build if any bundled audio file is byte-identical to a file under `recordings/`. |
| GuitarPedal | **Excluded.** The inspected upstream `LICENSE` is GPL v2. Bundling its bench would require a pinned source/build inventory and complete corresponding-source distribution review. No inference is made that subprocess use changes either project's license. |
| Plugins, SoundCraft, instruments, fart-app, kujamba/zcn | **Excluded**, both binaries and media. No plugin, instrument bank, backend runtime or SoundCraft redistribution permission is assumed. |

## Audit the generated artifact

The archive includes `Cargo.lock`, `dependency-inventory.json`, `licenses/`,
`corresponding-source.tar.gz`, `DISTRIBUTION.md`, and `build-record.json`.
The build record identifies the crate version, full source SHA, dirty state,
binary and lockfile SHA-256, Rust version, build OS, dylibs, signing
limitation and kit limitation. Do not describe a dirty build as validated at
its recorded commit.

Supplemental notices are checked in so a package build does not need GitHub
credentials or make network license guesses. Dependency updates that add a
new notice-less crate must supply the correct upstream notices before
packaging. License expressions are preserved, not rewritten into a blanket
MIT claim. Source/notice inclusion is conservative and does not imply every
crate in metadata was linked.

Before a public release, a maintainer must review the generated inventory,
the dispatch notice limitation, toolchain redistribution obligations, and
any new binaries/assets. No tag, GitHub Release, package registry upload,
Developer ID signature or notarization is created by these scripts. The
`release` workflow creates a **draft** GitHub Release from a tag the
maintainer pushes, and still signs, notarizes and publishes nothing.
