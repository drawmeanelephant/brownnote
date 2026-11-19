//! Live CLAP instrument helper: one instrument instance per process for the
//! native app's live worker. The wire protocol is specified in
//! `native/src/plugins/LIVE_PROTOCOL.md`.
//!
//! This is a separate binary only until `main.rs` can register a `clap-live`
//! backend next to `clap`; it compiles the same FFI core (`clap.rs`) rather
//! than a copy of it.
//!
//! Usage: `brownnote-clap-live`
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::{AsHandle, AsRawHandle};
use std::{
    fs::File,
    io::{self, Write},
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

#[cfg(unix)]
unsafe extern "C" {
    fn dup2(old: i32, new: i32) -> i32;
}

#[cfg(windows)]
unsafe extern "system" {
    fn SetStdHandle(which: u32, handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
unsafe extern "C" {
    fn _dup2(old: i32, new: i32) -> i32;
}

/// Plugins may print to stdout. Keep a private copy of stdout for the
/// protocol and point file descriptor 1 at stderr, so stray output cannot
/// corrupt a frame.
#[cfg(unix)]
fn protocol_output() -> io::Result<File> {
    let protocol = io::stdout().as_fd().try_clone_to_owned()?;
    io::stdout().flush()?;
    // SAFETY: dup2 on the process's own standard descriptors.
    if unsafe { dup2(2, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(protocol))
}

/// Plugins may print to stdout. Keep a private copy of stdout for the
/// protocol and point the process's standard output at stderr, so stray
/// output cannot corrupt a frame. Both layers need redirecting: `_dup2`
/// retargets the C runtime's descriptor table, which caches the original
/// stdout handle at startup, for plugin `printf`; `SetStdHandle` retargets
/// the Win32 handle that Rust's `stdout()` resolves per call.
#[cfg(windows)]
fn protocol_output() -> io::Result<File> {
    const STD_OUTPUT_HANDLE: u32 = -11_i32 as u32;
    let protocol = io::stdout().as_handle().try_clone_to_owned()?;
    io::stdout().flush()?;
    // SAFETY: defined operations on the process's own standard streams.
    if unsafe { _dup2(2, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: SetStdHandle on the process's own standard handles; the
    // stderr handle is valid for the process's lifetime.
    if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, io::stderr().as_handle().as_raw_handle()) } == 0 {
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
