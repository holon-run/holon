//! Filesystem helpers for runtime- and agent-private paths.
//!
//! Holon splits on-disk ownership into three classes: runtime-private and
//! agent-private paths both use `0700` directories and `0600` files, while
//! explicitly shared paths (project workspaces, published static assets) keep
//! the process umask and are never rewritten here.
//!
//! Call sites must use these helpers instead of relying on the process umask,
//! because a umask of `022` widens tool transcripts and diagnostics databases
//! to world-readable modes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Permission mode used for runtime- and agent-private directories.
pub const PRIVATE_DIR_MODE: u32 = 0o700;

/// Permission mode used for runtime- and agent-private files.
pub const PRIVATE_FILE_MODE: u32 = 0o600;

/// How [`open_private_file`] treats an existing file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateOpenMode {
    /// Truncate existing content before writing new content.
    Truncate,
    /// Append to existing content.
    Append,
}

/// Creates `path` and any missing parents as private directories.
///
/// Newly created directories get mode `0700`. When `path` already exists only
/// its own mode is corrected; parents are left untouched so that a private
/// directory created inside an explicitly shared workspace never rewrites the
/// shared workspace's permissions.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR_MODE)
            .create(path)?;
        set_private_dir_mode(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Writes `bytes` to `path` with mode `0600`.
///
/// The bytes are written to a private temporary sibling and renamed into
/// place, so a partially written file is never visible under `path` and an
/// existing wider-mode file is replaced by a private one.
///
/// The temporary file is not fsynced: callers such as tool-transcript capture
/// write on a hot path, and the previous implementation did not request
/// durability either.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path {} has no parent directory", path.display()),
        )
    })?;
    create_private_dir_all(parent)?;
    let temp_path = temp_sibling(path);
    let result = (|| {
        let mut file = open_new_private_file(&temp_path)?;
        file.write_all(bytes)?;
        drop(file);
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

/// Opens `path` for writing with mode `0600`, creating parents as needed.
///
/// Unlike [`write_private_file`] this keeps the handle open, which is required
/// by streaming writers such as command output capture and daemon logs. An
/// existing file created under a wider umask is corrected in place.
pub fn open_private_file(path: &Path, mode: PrivateOpenMode) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    match mode {
        PrivateOpenMode::Truncate => options.truncate(true),
        PrivateOpenMode::Append => options.append(true),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(PRIVATE_FILE_MODE);
    }
    let file = options.open(path)?;
    // `mode` only applies to newly created files; correct pre-existing ones.
    let _ = set_private_file_mode(path);
    Ok(file)
}

/// Corrects an existing directory to mode `0700`.
pub fn set_private_dir_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        set_mode(path, PRIVATE_DIR_MODE)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Corrects an existing file to mode `0600`.
pub fn set_private_file_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        set_mode(path, PRIVATE_FILE_MODE)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Narrows a SQLite database and its existing `-wal` / `-shm` sidecars to
/// `0600`, ignoring files that do not exist yet.
///
/// Sidecars created later are protected by the private parent directory, which
/// denies directory traversal to other users even if SQLite creates them with
/// the process umask.
pub fn restrict_sqlite_database(path: &Path) -> io::Result<()> {
    set_private_file_mode_if_exists(path)?;
    for suffix in ["-wal", "-shm"] {
        set_private_file_mode_if_exists(&sqlite_sidecar_path(path, suffix))?;
    }
    Ok(())
}

fn set_private_file_mode_if_exists(path: &Path) -> io::Result<()> {
    match set_private_file_mode(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn open_new_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(PRIVATE_FILE_MODE);
    }
    options.open(path)
}

fn temp_sibling(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("write");
    parent.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple()))
}
