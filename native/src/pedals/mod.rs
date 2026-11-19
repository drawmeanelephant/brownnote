pub mod brownnote;
pub mod catalog;
pub mod offline;
pub mod render;

// Shell-script bench fakes and signal handling are POSIX-only (issue #257).
#[cfg(all(test, unix))]
#[path = "tests.rs"]
mod tests;
