//! Atomic sibling-file publication for the guide cache.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};

use super::cache::{CacheError, Cancellation};

/// Failure-injection boundary for atomic cache writes.
pub trait AtomicWriteHooks {
    /// Runs before bytes are written to the temporary file.
    ///
    /// # Errors
    /// Returns an injected filesystem error.
    fn before_write(&self) -> io::Result<()> {
        Ok(())
    }

    /// Runs after the temporary file is synchronized.
    ///
    /// # Errors
    /// Returns an injected filesystem error.
    fn after_sync(&self) -> io::Result<()> {
        Ok(())
    }

    /// Runs immediately before the temporary file is renamed into place.
    ///
    /// # Errors
    /// Returns an injected filesystem error.
    fn before_rename(&self) -> io::Result<()> {
        Ok(())
    }

    /// Runs after publication before the parent directory is synchronized.
    ///
    /// # Errors
    /// Returns an injected filesystem error.
    fn after_rename(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Production atomic-write hooks with no injected failures.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultAtomicWriteHooks;

impl AtomicWriteHooks for DefaultAtomicWriteHooks {}

pub(crate) fn write_atomic<C: Cancellation, H: AtomicWriteHooks>(
    path: &Path,
    bytes: &[u8],
    cancellation: &C,
    hooks: &H,
) -> Result<(), CacheError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(CacheError::Io)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CacheError::Io(io::Error::other("cache path has no valid file name")))?;
    let mut temporary = None;
    for attempt in 0_u32..100 {
        let candidate = parent.join(format!(".{file_name}.tmp-{}-{attempt}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(CacheError::Io(error)),
        }
    }
    let Some((temporary_path, mut file)) = temporary else {
        return Err(CacheError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary cache path exhausted",
        )));
    };
    let result = (|| {
        hooks.before_write().map_err(CacheError::Io)?;
        file.write_all(bytes).map_err(CacheError::Io)?;
        file.sync_all().map_err(CacheError::Io)?;
        hooks.after_sync().map_err(CacheError::Io)?;
        if cancellation.is_cancelled() {
            return Err(CacheError::Cancelled);
        }
        hooks.before_rename().map_err(CacheError::Io)?;
        fs::rename(&temporary_path, path).map_err(CacheError::Io)?;
        hooks.after_rename().map_err(CacheError::Io)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(CacheError::Io)
    })();
    if result.is_err() {
        let _cleanup = fs::remove_file(&temporary_path);
    }
    result
}
