//! Finding and running the `brownnote` binary. The bench is brownnote's
//! business: this module never passes `--bench`, so its own discovery
//! (`--bench`, `BROWNNOTE_BENCH`, `vendor/GuitarPedal/...`) still applies.
use super::catalog::{Catalog, ParseError};
use std::{
    fmt,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// The binary `make` builds, resolved at compile time.
pub const DEFAULT_BIN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../build/brownnote");

#[derive(Debug)]
pub enum CatalogError {
    NotFound(PathBuf),
    NoBench(String),
    Failed { code: i32, stderr: String },
    Timeout,
    Parse(ParseError),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(path) => write!(
                f,
                "Pedals need brownnote: run `make` (looked for {})",
                path.display()
            ),
            Self::NoBench(_) => write!(
                f,
                "Pedals need the GuitarPedal bench: run `make -C vendor/GuitarPedal/Validation bench`"
            ),
            Self::Failed { code, stderr } => {
                let first = stderr.lines().next().unwrap_or("no message");
                write!(f, "Pedals: brownnote failed (exit {code}): {first}")
            }
            Self::Timeout => f.write_str("Pedals: brownnote did not answer in time"),
            Self::Parse(error) => write!(f, "Pedals: {error}"),
        }
    }
}
impl std::error::Error for CatalogError {}

/// Run `path --list` and parse what it prints.
pub fn list(path: &Path, timeout: Duration) -> Result<Catalog, CatalogError> {
    let mut child = Command::new(path)
        .arg("--list")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                CatalogError::NotFound(path.to_path_buf())
            } else {
                CatalogError::Failed {
                    code: -1,
                    stderr: error.to_string(),
                }
            }
        })?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CatalogError::Timeout);
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CatalogError::Failed {
                    code: -1,
                    stderr: error.to_string(),
                });
            }
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let code = status.code().unwrap_or(-1);
    if code != 0 {
        if code == 2 && stderr.contains("no bench binary found") {
            return Err(CatalogError::NoBench(stderr.trim().to_string()));
        }
        return Err(CatalogError::Failed {
            code,
            stderr: stderr.trim().to_string(),
        });
    }
    Catalog::parse(&stdout).map_err(CatalogError::Parse)
}
