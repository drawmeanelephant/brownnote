//! Finding and running optional pedal tooling through the runtime resolver.
use super::catalog::{Catalog, ParseError};
use std::{
    fmt,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

pub use crate::paths::DEFAULT_BIN;

#[derive(Debug)]
pub enum CatalogError {
    NotFound(PathBuf),
    Path(crate::paths::PathError),
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
                "Pedals need brownnote: run `make` or set --brownnote PATH / BROWNNOTE_BIN (looked for {}). Base pads still play.",
                path.display()
            ),
            Self::Path(error) => write!(f, "Pedals unavailable; base pads still play. {error}"),
            Self::NoBench(detail) => write!(
                f,
                "Pedals need the GuitarPedal bench: set BROWNNOTE_BENCH or run `make -C vendor/GuitarPedal/Validation bench`. {detail}"
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
    let mut command = crate::paths::Resolver::current()
        .and_then(|paths| paths.pedal_command(Some(path), None))
        .map_err(|error| {
            if error.resource == crate::paths::Resource::Brownnote
                && error.kind == crate::paths::PathErrorKind::Missing
            {
                CatalogError::NotFound(error.path.clone().unwrap_or_else(|| path.into()))
            } else {
                CatalogError::Path(error)
            }
        })?;
    let mut child = command
        .arg("--list")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CatalogError::Failed {
            code: -1,
            stderr: format!("{}: {error}", command.get_program().to_string_lossy()),
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
