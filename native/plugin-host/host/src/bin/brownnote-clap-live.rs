//! Live CLAP instrument helper: one instrument instance per process for the
//! native app's live worker. The wire protocol is specified in
//! `native/src/plugins/LIVE_PROTOCOL.md`.
//!
//! This is a separate binary only until `main.rs` can register a `clap-live`
//! backend next to `clap`; it compiles the same FFI core (`clap.rs`) rather
//! than a copy of it.
//!
//! Usage: `brownnote-clap-live`
use std::{
    fs::File,
    io::{self, Write},
    os::fd::AsFd,
    process::ExitCode,
};

#[path = "../clap.rs"]
#[allow(dead_code)]
mod clap;
#[path = "../clap_live.rs"]
mod clap_live;
#[path = "../protocol.rs"]
#[allow(dead_code)]
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
    let output = match protocol_output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("brownnote-clap-live: {error}");
            return ExitCode::from(2);
        }
    };
    ExitCode::from(clap_live::serve(io::stdin().lock(), output))
}
