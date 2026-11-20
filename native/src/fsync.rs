//! Durability helpers whose implementation differs by platform.
use std::{io, path::Path};

/// Flush a directory's metadata to stable storage.
///
/// On Unix the directory is opened and fsync'd directly. Windows requires
/// `FILE_FLAG_BACKUP_SEMANTICS` to obtain a directory handle at all, and
/// `FlushFileBuffers` then needs the handle opened for write access.
#[cfg(unix)]
pub fn sync_dir(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// Windows variant of [`sync_dir`].
#[cfg(windows)]
pub fn sync_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .sync_all()
}

/// Best-effort fallback for targets with no portable directory fsync.
#[cfg(not(any(unix, windows)))]
pub fn sync_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Flush a regular file to stable storage.
///
/// Unix allows fsync on a read-only handle; `FlushFileBuffers` on Windows
/// needs a handle opened with write access.
#[cfg(windows)]
pub fn sync_file(path: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .sync_all()
}

/// Unix/other variant of [`sync_file`].
#[cfg(not(windows))]
pub fn sync_file(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}
