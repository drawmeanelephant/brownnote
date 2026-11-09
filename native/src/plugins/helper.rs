//! Machine-local location of the `brownnote-clap-live` helper process (D8).
//!
//! Like the output and MIDI selections, this is a machine preference, never
//! project data: it names a program on this computer. The caller supplies the
//! machine preferences directory; this module never discovers a home
//! directory and never touches a project document.
use crate::{
    paths::{PathError, Resolver, Resource},
    pedals::offline::HelperCommand,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const FILE: &str = "clap-live.json";
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    version: u32,
    /// Absolute helper path, or absent to follow the bounded machine layout.
    program: Option<String>,
}

/// Read the saved override. A missing file is an empty setting, not an error.
pub fn load(root: &Path) -> Result<Option<PathBuf>, String> {
    let file = match File::open(root.join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read machine live-instrument preferences".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read machine live-instrument preferences")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Machine live-instrument preferences are too large".into());
    }
    let saved: Preference = serde_json::from_slice(&bytes)
        .map_err(|_| "Corrupt machine live-instrument preferences; choose a helper explicitly")?;
    if saved.version != 1 {
        return Err("Unsupported machine live-instrument preference version".into());
    }
    match saved.program {
        Some(program) if program.is_empty() => {
            Err("Invalid saved live-instrument helper path".into())
        }
        Some(program) => Ok(Some(PathBuf::from(program))),
        None => Ok(None),
    }
}

/// Persist a helper override; `None` clears it and follows the machine layout.
pub fn save(root: &Path, program: Option<&Path>) -> Result<(), String> {
    if program.is_some_and(|path| path.as_os_str().is_empty()) {
        return Err("An empty live-instrument helper path is invalid".into());
    }
    let saved = Preference {
        version: 1,
        program: program.map(|path| path.to_string_lossy().into_owned()),
    };
    let bytes = serde_json::to_vec_pretty(&saved)
        .map_err(|_| "Cannot encode machine live-instrument preferences")?;
    // One app-side writer. A failed write leaves the last complete file intact.
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(root)?;
        let temporary = root.join("clap-live.json.tmp");
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(temporary, root.join(FILE))?;
        File::open(root)?.sync_all()
    };
    write().map_err(|_| "Live-instrument helper is active in memory but could not be saved".into())
}

/// Saved preference > `BROWNNOTE_CLAP_LIVE_HELPER` > bounded executable-relative
/// layout. A broken higher-priority path is an error, never another program.
pub fn resolve(paths: &Resolver, root: Option<&Path>) -> Result<HelperCommand, String> {
    let saved = match root {
        Some(root) => load(root)?,
        None => None,
    };
    let program = paths
        .resolve(Resource::ClapLive, saved.as_deref())
        .map_err(|error: PathError| error.to_string())?;
    Ok(HelperCommand {
        program,
        args: Vec::new(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{load, resolve, save};
    use crate::paths::{Environment, Resolver};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering::Relaxed},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "brownnote-clap-live-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn executable(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
        fn resolver(&self) -> Resolver {
            Resolver::new(
                self.0.join("layout/probe"),
                self.0.join("unrelated"),
                Environment {
                    home: Some(self.0.clone()),
                    ..Environment::default()
                },
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_saved_helper_is_a_machine_preference_and_must_be_executable() {
        let fixture = Fixture::new();
        let root = fixture.0.join("preferences");
        assert_eq!(load(&root).unwrap(), None, "missing file is unset");

        let helper = fixture.executable("brownnote-clap-live");
        save(&root, Some(&helper)).unwrap();
        assert_eq!(load(&root).unwrap(), Some(helper.clone()));
        assert_eq!(
            resolve(&fixture.resolver(), Some(&root)).unwrap().program,
            helper
        );

        // Clearing the preference follows the bounded layout, which is absent here.
        save(&root, None).unwrap();
        assert_eq!(load(&root).unwrap(), None);
        assert!(resolve(&fixture.resolver(), Some(&root)).is_err());
    }

    #[test]
    fn corrupt_files_and_missing_saved_helpers_are_reported() {
        let fixture = Fixture::new();
        let root = fixture.0.join("preferences");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("clap-live.json"), "{ not json").unwrap();
        assert!(resolve(&fixture.resolver(), Some(&root)).is_err());

        fs::write(
            root.join("clap-live.json"),
            r#"{"version":2,"program":null}"#,
        )
        .unwrap();
        assert!(resolve(&fixture.resolver(), Some(&root)).is_err());

        fs::write(root.join("clap-live.json"), r#"{"version":1,"program":""}"#).unwrap();
        assert!(resolve(&fixture.resolver(), Some(&root)).is_err());

        // A saved path that no longer exists is an error, not a fallback.
        fs::write(
            root.join("clap-live.json"),
            r#"{"version":1,"program":"/missing/brownnote-clap-live"}"#,
        )
        .unwrap();
        assert!(resolve(&fixture.resolver(), Some(&root)).is_err());
    }

    #[test]
    fn an_environment_helper_is_used_when_no_preference_is_saved() {
        let fixture = Fixture::new();
        let helper = fixture.executable("env-helper");
        let resolver = Resolver::new(
            fixture.0.join("layout/probe"),
            fixture.0.join("unrelated"),
            Environment {
                home: Some(fixture.0.clone()),
                clap_live: Some(helper.clone()),
                ..Environment::default()
            },
        );
        assert_eq!(resolve(&resolver, None).unwrap().program, helper);
    }
}
