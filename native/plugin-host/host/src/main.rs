//! Offline plugin helper: hosts one plugin instance per process for the
//! native app's offline renderer. The wire protocol is specified in
//! `native/src/pedals/offline/PROTOCOL.md`.
//!
//! Usage: `brownnote-plugin-host clap` or
//! `brownnote-plugin-host ladspa [--allow UniqueID:Label]...`
use std::{
    fs::File,
    io::{self, Write},
    os::fd::AsFd,
    process::ExitCode,
};

mod clap;
mod clap_helper;
mod ladspa;
mod ladspa_helper;
mod protocol;

const USAGE: &str = "usage: brownnote-plugin-host clap\n       brownnote-plugin-host ladspa [--allow UniqueID:Label]...";

unsafe extern "C" {
    fn dup2(old: i32, new: i32) -> i32;
}

/// Plugins may print to stdout. Keep a private copy of stdout for the
/// protocol and point file descriptor 1 at stderr, so stray output cannot
/// corrupt a frame.
fn protocol_output() -> io::Result<File> {
    let protocol = io::stdout().as_fd().try_clone_to_owned()?;
    io::stdout().flush()?;
    // SAFETY: dup2 on the process's own standard descriptors.
    if unsafe { dup2(2, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(protocol))
}

enum Backend {
    Clap,
    /// The LADSPA plugins this process may load.
    Ladspa(Vec<String>),
}

fn parse(args: &[String]) -> Option<Backend> {
    match args {
        [backend] if backend == "clap" => Some(Backend::Clap),
        [backend, rest @ ..] if backend == "ladspa" => {
            let mut allowed: Vec<String> = ladspa_helper::ALLOWED
                .iter()
                .map(|id| (*id).to_owned())
                .collect();
            let mut rest = rest.iter();
            while let Some(flag) = rest.next() {
                if flag != "--allow" {
                    return None;
                }
                allowed.push(rest.next()?.clone());
            }
            Some(Backend::Ladspa(allowed))
        }
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(backend) = parse(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let output = match protocol_output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("brownnote-plugin-host: {error}");
            return ExitCode::from(2);
        }
    };
    ExitCode::from(match backend {
        Backend::Clap => clap_helper::serve(io::stdin().lock(), output),
        Backend::Ladspa(allowed) => ladspa_helper::serve(io::stdin().lock(), output, &allowed),
    })
}
