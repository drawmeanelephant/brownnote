pub mod brownnote;
pub mod catalog;
pub mod offline;
pub mod render;

// Shell-script bench fakes and signal handling are POSIX-only (issue #257).
// The allow is explicit because `cfg(all(test, unix))` does not register
// as test code with clippy the way a bare `cfg(test)` does.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "tests.rs"]
mod tests;
