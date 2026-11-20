//! Runtime path policy. No resource is found through the process's working
//! directory unless the user explicitly supplies a relative override.
use std::{
    env, fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

// Existing callers pass PathBufs, so defaults have reserved, non-file tokens.
// An empty path is an invalid override, not another spelling of "default".
pub const DEFAULT_SAMPLES: &str = "brownnote:bundled-samples";
pub const DEFAULT_BIN: &str = "brownnote:bundled-cli";
pub const DEFAULT_CACHE: &str = "brownnote:user-pedal-cache";
pub const DEFAULT_CLAP_LIVE: &str = "brownnote:bundled-clap-live";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Samples,
    Brownnote,
    Bench,
    Cache,
    Recovery,
    Preferences,
    /// The `brownnote-clap-live` helper process (D8, machine-local).
    ClapLive,
}

impl Resource {
    fn hint(self) -> &'static str {
        match self {
            Self::Samples => "--samples DIR or BROWNNOTE_SAMPLES",
            Self::Brownnote => "--brownnote PATH or BROWNNOTE_BIN (run `make` in a checkout)",
            Self::Bench => {
                "BROWNNOTE_BENCH (or build with `make -C vendor/GuitarPedal/Validation bench`)"
            }
            Self::Cache => "--pedal-cache DIR or BROWNNOTE_CACHE",
            Self::Recovery => "BROWNNOTE_RECOVERY",
            Self::Preferences => "BROWNNOTE_PREFERENCES",
            Self::ClapLive => "BROWNNOTE_CLAP_LIVE_HELPER (or install the bundled helper)",
        }
    }

    fn token(self) -> Option<&'static str> {
        match self {
            Self::Samples => Some(DEFAULT_SAMPLES),
            Self::Brownnote => Some(DEFAULT_BIN),
            Self::Cache => Some(DEFAULT_CACHE),
            Self::ClapLive => Some(DEFAULT_CLAP_LIVE),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PathErrorKind {
    Empty,
    NotAbsolute,
    Missing,
    NotDirectory,
    NotFile,
    NotExecutable,
    NoHome,
    UnsupportedPlatform,
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
}

#[derive(Debug)]
pub struct PathError {
    pub resource: Resource,
    pub path: Option<PathBuf>,
    pub kind: PathErrorKind,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} path", self.resource)?;
        if let Some(path) = &self.path {
            write!(f, " {}", path.display())?;
        }
        match &self.kind {
            PathErrorKind::Empty => f.write_str(": empty override")?,
            PathErrorKind::NotAbsolute => f.write_str(": expected an absolute base path")?,
            PathErrorKind::Missing => f.write_str(": not found")?,
            PathErrorKind::NotDirectory => f.write_str(": expected a directory")?,
            PathErrorKind::NotFile => f.write_str(": expected a regular file")?,
            PathErrorKind::NotExecutable => f.write_str(": not executable")?,
            PathErrorKind::NoHome => f.write_str(": HOME is unavailable")?,
            PathErrorKind::UnsupportedPlatform => {
                f.write_str(": default user locations are only defined for macOS and Linux")?
            }
            PathErrorKind::Io { operation, kind } => write!(f, ": {operation}: {kind}")?,
        }
        write!(f, ". Set {}.", self.resource.hint())
    }
}
impl std::error::Error for PathError {}

/// Captured once per resolver; tests need not mutate process environment.
#[derive(Default)]
pub struct Environment {
    pub home: Option<PathBuf>,
    pub samples: Option<PathBuf>,
    pub brownnote: Option<PathBuf>,
    pub bench: Option<PathBuf>,
    pub cache: Option<PathBuf>,
    pub recovery: Option<PathBuf>,
    pub preferences: Option<PathBuf>,
    pub clap_live: Option<PathBuf>,
}

pub struct Resolver {
    executable: PathBuf,
    cwd: PathBuf,
    environment: Environment,
    /// Appended to every default tool name: `.exe` on Windows, empty elsewhere.
    exe_suffix: &'static str,
}

impl Resolver {
    pub fn new(executable: PathBuf, cwd: PathBuf, environment: Environment) -> Self {
        Self {
            executable,
            cwd,
            environment,
            exe_suffix: env::consts::EXE_SUFFIX,
        }
    }

    pub fn current() -> Result<Self, PathError> {
        let io = |operation, error: io::Error| PathError {
            resource: Resource::Samples,
            path: None,
            kind: PathErrorKind::Io {
                operation,
                kind: error.kind(),
            },
        };
        let get = |name| env::var_os(name).map(PathBuf::from);
        Ok(Self::new(
            env::current_exe().map_err(|e| io("locate executable", e))?,
            env::current_dir().map_err(|e| io("read working directory", e))?,
            Environment {
                home: get("HOME"),
                samples: get("BROWNNOTE_SAMPLES"),
                brownnote: get("BROWNNOTE_BIN"),
                bench: get("BROWNNOTE_BENCH"),
                cache: get("BROWNNOTE_CACHE"),
                recovery: get("BROWNNOTE_RECOVERY"),
                preferences: get("BROWNNOTE_PREFERENCES"),
                clap_live: get("BROWNNOTE_CLAP_LIVE_HELPER"),
            },
        ))
    }

    fn error(resource: Resource, path: &Path, kind: PathErrorKind) -> PathError {
        PathError {
            resource,
            path: Some(path.into()),
            kind,
        }
    }

    fn absolute(&self, resource: Resource, path: &Path) -> Result<PathBuf, PathError> {
        if path.as_os_str().is_empty() {
            return Err(Self::error(resource, path, PathErrorKind::Empty));
        }
        if path.is_absolute() {
            return Ok(path.into());
        }
        if !self.cwd.is_absolute() {
            return Err(Self::error(resource, &self.cwd, PathErrorKind::NotAbsolute));
        }
        Ok(self.cwd.join(path))
    }

    fn root(&self, resource: Resource) -> Result<&Path, PathError> {
        if !self.executable.is_absolute() {
            return Err(Self::error(
                resource,
                &self.executable,
                PathErrorKind::NotAbsolute,
            ));
        }
        self.executable
            .parent()
            .ok_or_else(|| Self::error(resource, &self.executable, PathErrorKind::NotDirectory))
    }

    /// Cargo's `native/target/{debug,release}` layouts are recognized: the
    /// older `deps/` location and the `build/<pkg>/<hash>/out/` location
    /// current toolchains use. There is no general ancestor search or
    /// build-time source fallback.
    fn checkout(&self, root: &Path) -> Option<PathBuf> {
        // Walk up past `deps`, or `out/<hash>/<pkg>/build`, to the profile dir.
        let mut profile = root;
        for _ in 0..6 {
            if matches!(profile.file_name()?.to_str()?, "debug" | "release") {
                break;
            }
            profile = profile.parent()?;
        }
        if !matches!(profile.file_name()?.to_str()?, "debug" | "release") {
            return None;
        }
        let target = profile.parent()?;
        let native = target.parent()?;
        if target.file_name()? != "target"
            || native.file_name()? != "native"
            || !native.join("Cargo.toml").is_file()
        {
            return None;
        }
        Some(native.parent()?.into())
    }

    fn selected(&self, resource: Resource, explicit: Option<&Path>) -> Result<PathBuf, PathError> {
        let explicit = explicit.filter(|path| {
            resource
                .token()
                .is_none_or(|token| *path != Path::new(token))
        });
        let environment = match resource {
            Resource::Samples => &self.environment.samples,
            Resource::Brownnote => &self.environment.brownnote,
            Resource::Bench => &self.environment.bench,
            Resource::Cache => &self.environment.cache,
            Resource::Recovery => &self.environment.recovery,
            Resource::Preferences => &self.environment.preferences,
            Resource::ClapLive => &self.environment.clap_live,
        };
        if let Some(path) = explicit.or(environment.as_deref()) {
            return self.absolute(resource, path);
        }
        if matches!(
            resource,
            Resource::Cache | Resource::Recovery | Resource::Preferences
        ) {
            let home = self.environment.home.as_deref().ok_or(PathError {
                resource,
                path: None,
                kind: PathErrorKind::NoHome,
            })?;
            if home.as_os_str().is_empty() || !home.is_absolute() {
                return Err(Self::error(resource, home, PathErrorKind::NotAbsolute));
            }
            if cfg!(target_os = "macos") {
                return Ok(home.join(match resource {
                    Resource::Cache => "Library/Caches/Brownnote/pedals",
                    Resource::Recovery => "Library/Application Support/Brownnote/recovery",
                    Resource::Preferences => "Library/Application Support/Brownnote/preferences",
                    _ => unreachable!(),
                }));
            }
            #[cfg(target_os = "linux")]
            {
                return Ok(home.join(match resource {
                    Resource::Cache => ".cache/Brownnote/pedals",
                    Resource::Recovery => ".local/share/Brownnote/recovery",
                    Resource::Preferences => ".config/Brownnote/preferences",
                    _ => unreachable!(),
                }));
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                return Err(Self::error(
                    resource,
                    home,
                    PathErrorKind::UnsupportedPlatform,
                ));
            }
        }
        let root = self.root(resource)?;
        // A resource tree beside a Cargo executable wins, even if incomplete.
        let mut bundled = false;
        for entry in ["resources", "tools"] {
            match fs::symlink_metadata(root.join(entry)) {
                Ok(_) => bundled = true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Self::error(
                        resource,
                        &root.join(entry),
                        PathErrorKind::Io {
                            operation: "inspect resource layout",
                            kind: error.kind(),
                        },
                    ));
                }
            }
        }
        let checkout = if bundled { None } else { self.checkout(root) };
        let tool = |path: &str| format!("{path}{}", self.exe_suffix);
        Ok(match (resource, checkout) {
            (Resource::Samples, Some(repo)) => repo.join("recordings"),
            (Resource::Brownnote, Some(repo)) => repo.join(tool("build/brownnote")),
            (Resource::Bench, Some(repo)) => {
                repo.join(tool("vendor/GuitarPedal/Validation/bench/bench"))
            }
            (Resource::ClapLive, Some(repo)) => {
                repo.join(tool("native/plugin-host/target/debug/brownnote-clap-live"))
            }
            (Resource::Samples, None) => root.join("resources/recordings"),
            (Resource::Brownnote, None) => root.join(tool("tools/brownnote")),
            (Resource::Bench, None) => root.join(tool("tools/bench")),
            (Resource::ClapLive, None) => root.join(tool("tools/brownnote-clap-live")),
            _ => unreachable!(),
        })
    }

    /// Explicit override > environment > bounded runtime layout. A selected
    /// missing/broken path is an error; it never falls through to another sound.
    pub fn resolve(
        &self,
        resource: Resource,
        explicit: Option<&Path>,
    ) -> Result<PathBuf, PathError> {
        let path = self.selected(resource, explicit)?;
        if resource == Resource::Preferences {
            return Ok(path);
        }
        if matches!(resource, Resource::Cache | Resource::Recovery) {
            writable(resource, &path)?;
        } else {
            let metadata = fs::metadata(&path).map_err(|error| {
                Self::error(
                    resource,
                    &path,
                    if error.kind() == io::ErrorKind::NotFound {
                        PathErrorKind::Missing
                    } else {
                        PathErrorKind::Io {
                            operation: "read metadata",
                            kind: error.kind(),
                        }
                    },
                )
            })?;
            if resource == Resource::Samples {
                if !metadata.is_dir() {
                    return Err(Self::error(resource, &path, PathErrorKind::NotDirectory));
                }
                fs::read_dir(&path).map_err(|error| {
                    Self::error(
                        resource,
                        &path,
                        PathErrorKind::Io {
                            operation: "read directory",
                            kind: error.kind(),
                        },
                    )
                })?;
            } else {
                if !metadata.is_file() {
                    return Err(Self::error(resource, &path, PathErrorKind::NotFile));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 == 0 {
                        return Err(Self::error(resource, &path, PathErrorKind::NotExecutable));
                    }
                }
            }
        }
        Ok(path)
    }

    /// Pin the bench argument so the C CLI cannot select a different cwd's
    /// vendor tree. Explicit custom tools may implement their own protocol;
    /// let them report a missing default bench, but validate explicit benches.
    pub fn pedal_command(
        &self,
        brownnote: Option<&Path>,
        bench: Option<&Path>,
    ) -> Result<Command, PathError> {
        let binary = self.resolve(Resource::Brownnote, brownnote)?;
        let default_binary = brownnote.is_none_or(|p| p == Path::new(DEFAULT_BIN));
        let bench = if default_binary || bench.is_some() || self.environment.bench.is_some() {
            self.resolve(Resource::Bench, bench)?
        } else {
            self.selected(Resource::Bench, None)?
        };
        let mut command = Command::new(binary);
        command.arg("--bench").arg(bench);
        Ok(command)
    }
}

static PROBES: AtomicU64 = AtomicU64::new(0);

fn writable(resource: Resource, path: &Path) -> Result<(), PathError> {
    let io = |operation, error: io::Error| {
        Resolver::error(
            resource,
            path,
            PathErrorKind::Io {
                operation,
                kind: error.kind(),
            },
        )
    };
    fs::create_dir_all(path).map_err(|e| io("create directory", e))?;
    let probe = path.join(format!(
        ".write-probe-{}-{}",
        std::process::id(),
        PROBES.fetch_add(1, Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|e| io("create write probe", e))?;
    let result = file.write_all(b"brownnote");
    drop(file);
    let removed = fs::remove_file(probe);
    result.map_err(|e| io("write probe", e))?;
    removed.map_err(|e| io("remove write probe", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "brownnote-paths-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Relaxed)
            )))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn resolver() -> Resolver {
        Resolver::new(
            PathBuf::from("probe"),
            PathBuf::from("probe"),
            Environment::default(),
        )
    }

    /// `native/target/<profile>/deps` — the layout older toolchains use.
    #[test]
    fn checkout_recognizes_the_deps_layout() {
        let fixture = Fixture::new();
        let deps = fixture.0.join("native/target/debug/deps");
        fs::create_dir_all(&deps).unwrap();
        fs::write(fixture.0.join("native/Cargo.toml"), "[package]").unwrap();
        assert_eq!(
            resolver().checkout(&deps).as_deref(),
            Some(fixture.0.as_path())
        );
    }

    /// `native/target/<profile>/build/<pkg>/<hash>/out` — the layout current
    /// toolchains use for binaries.
    #[test]
    fn checkout_recognizes_the_build_out_layout() {
        let fixture = Fixture::new();
        let out = fixture
            .0
            .join("native/target/release/build/brownnote-native/0123456789abcdef/out");
        fs::create_dir_all(&out).unwrap();
        fs::write(fixture.0.join("native/Cargo.toml"), "[package]").unwrap();
        assert_eq!(
            resolver().checkout(&out).as_deref(),
            Some(fixture.0.as_path())
        );
    }

    fn windows_resolver(executable: PathBuf) -> Resolver {
        Resolver {
            exe_suffix: ".exe",
            ..Resolver::new(executable, PathBuf::from("probe"), Environment::default())
        }
    }

    #[test]
    fn default_tools_carry_the_platform_executable_suffix_in_both_layouts() {
        let fixture = Fixture::new();
        let release = fixture.0.join("native/target/release");
        fs::create_dir_all(&release).unwrap();
        fs::write(fixture.0.join("native/Cargo.toml"), "[package]").unwrap();
        let checkout = windows_resolver(release.join("brownnote-native.exe"));
        for (resource, expected) in [
            (Resource::Brownnote, "build/brownnote.exe"),
            (
                Resource::Bench,
                "vendor/GuitarPedal/Validation/bench/bench.exe",
            ),
            (
                Resource::ClapLive,
                "native/plugin-host/target/debug/brownnote-clap-live.exe",
            ),
            (Resource::Samples, "recordings"),
        ] {
            assert_eq!(
                checkout.selected(resource, None).unwrap(),
                fixture.0.join(expected)
            );
        }

        let flat = fixture.0.join("flat");
        fs::create_dir_all(flat.join("tools")).unwrap();
        let bundled = windows_resolver(flat.join("brownnote-native.exe"));
        for (resource, expected) in [
            (Resource::Brownnote, "tools/brownnote.exe"),
            (Resource::Bench, "tools/bench.exe"),
            (Resource::ClapLive, "tools/brownnote-clap-live.exe"),
            (Resource::Samples, "resources/recordings"),
        ] {
            assert_eq!(
                bundled.selected(resource, None).unwrap(),
                flat.join(expected)
            );
        }
    }

    #[test]
    fn a_suffixed_cli_resolves_and_a_missing_one_names_the_suffixed_path() {
        let fixture = Fixture::new();
        let release = fixture.0.join("native/target/release");
        fs::create_dir_all(&release).unwrap();
        fs::create_dir_all(fixture.0.join("build")).unwrap();
        fs::write(fixture.0.join("native/Cargo.toml"), "[package]").unwrap();
        let resolver = windows_resolver(release.join("brownnote-native.exe"));

        let error = resolver.resolve(Resource::Brownnote, None).unwrap_err();
        assert_eq!(error.kind, PathErrorKind::Missing);
        assert_eq!(error.path, Some(fixture.0.join("build/brownnote.exe")));

        // An unsuffixed POSIX build beside it is not a Windows executable.
        fs::write(fixture.0.join("build/brownnote"), b"").unwrap();
        assert!(resolver.resolve(Resource::Brownnote, None).is_err());

        let cli = fixture.0.join("build/brownnote.exe");
        fs::write(&cli, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(resolver.resolve(Resource::Brownnote, None).unwrap(), cli);
    }

    #[test]
    fn checkout_rejects_non_target_layouts() {
        let fixture = Fixture::new();
        let other = fixture.0.join("elsewhere/release");
        fs::create_dir_all(&other).unwrap();
        assert!(resolver().checkout(&other).is_none());
    }
}
