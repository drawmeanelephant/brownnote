//! Offline plugin helper: hosts one plugin instance per process for the
//! native app's offline renderer. The wire protocol is specified in
//! `native/src/pedals/offline/PROTOCOL.md`.
//!
//! Usage: `brownnote-plugin-host clap`
use std::{
    fs::File,
    io::{self, Write},
    os::fd::AsFd,
    process::ExitCode,
};

mod clap;
mod clap_helper;
mod protocol;

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

fn main() -> ExitCode {
    let backend = std::env::args().nth(1);
    if backend.as_deref() != Some("clap") {
        eprintln!("usage: brownnote-plugin-host clap");
        return ExitCode::from(2);
    }
    let output = match protocol_output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("brownnote-plugin-host: {error}");
            return ExitCode::from(2);
        }
    };
    ExitCode::from(clap_helper::serve(io::stdin().lock(), output))
}
