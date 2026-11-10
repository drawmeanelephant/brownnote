//! Bake the source identity into the executable so `--version` works offline.
//!
//! The packager passes the commit it records in `build-record.json` explicitly, so the
//! packaged binary and its build record cannot disagree. A plain Cargo build asks git.

use std::{env, path::Path, process::Command};

fn main() {
    for variable in ["BROWNNOTE_SOURCE_COMMIT", "BROWNNOTE_SOURCE_DIRTY"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    // A commit or a working-tree edit moves these files; the identity changes with them.
    for name in ["HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            let path = Path::new(&manifest_dir()).join(path);
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let commit = setting("BROWNNOTE_SOURCE_COMMIT")
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let dirty = match setting("BROWNNOTE_SOURCE_DIRTY") {
        Some(value) => value == "1",
        None => git(&["status", "--porcelain", "--untracked-files=normal"])
            .is_some_and(|status| !status.is_empty()),
    };
    println!("cargo:rustc-env=BROWNNOTE_SOURCE_COMMIT={commit}");
    println!("cargo:rustc-env=BROWNNOTE_SOURCE_DIRTY={}", u8::from(dirty));
}

/// A non-empty build environment value, trimmed; the packager's identity wins over git.
fn setting(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn manifest_dir() -> String {
    env::var("CARGO_MANIFEST_DIR").unwrap_or_default()
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
