//! Permission coverage for the runtime's private filesystem helpers.
//!
//! These tests run under explicit process umasks on purpose: the defect this
//! policy fixes is umask dependence, so a suite that only ran on a host with
//! umask `077` would pass without proving anything.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

use holon_domain::private_fs::{
    create_private_dir_all, open_private_file, restrict_sqlite_database, write_private_file,
    PrivateOpenMode,
};

static UMASK_LOCK: Mutex<()> = Mutex::new(());

fn with_umask(mask: u32, test: impl FnOnce()) {
    let _guard = UMASK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = unsafe { libc::umask(mask as libc::mode_t) };
    test();
    unsafe {
        libc::umask(previous);
    }
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn private_paths_are_private_under_wide_umask() {
    with_umask(0o022, || {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("tool-artifacts");
        create_private_dir_all(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);

        let file = dir.join("tool-result.log");
        write_private_file(&file, b"transcript").unwrap();
        assert_eq!(mode_of(&file), 0o600);
        assert_eq!(fs::read(&file).unwrap(), b"transcript");

        let streamed = dir.join("task-output.log");
        drop(open_private_file(&streamed, PrivateOpenMode::Truncate).unwrap());
        assert_eq!(mode_of(&streamed), 0o600);

        let appended = dir.join("daemon.log");
        write_private_file(&appended, b"first").unwrap();
        let mut handle = open_private_file(&appended, PrivateOpenMode::Append).unwrap();
        handle.write_all(b"second").unwrap();
        drop(handle);
        assert_eq!(mode_of(&appended), 0o600);
        assert_eq!(fs::read(&appended).unwrap(), b"firstsecond");
    });
}

#[test]
fn private_paths_are_private_under_tight_umask() {
    with_umask(0o077, || {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("state");
        create_private_dir_all(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);

        let file = dir.join("runtime.sqlite");
        write_private_file(&file, b"db").unwrap();
        assert_eq!(mode_of(&file), 0o600);
    });
}

#[test]
fn existing_wide_directories_and_files_are_corrected() {
    with_umask(0o022, || {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("tool-artifacts");
        fs::create_dir_all(&dir).unwrap();
        set_mode(&dir, 0o755);

        create_private_dir_all(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);

        let file = dir.join("existing.log");
        fs::write(&file, b"old").unwrap();
        set_mode(&file, 0o644);

        write_private_file(&file, b"new").unwrap();
        assert_eq!(mode_of(&file), 0o600);
        assert_eq!(fs::read(&file).unwrap(), b"new");
        assert_eq!(
            fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
                .count(),
            0,
            "temporary siblings must be renamed or removed"
        );
    });
}

#[test]
fn shared_parent_directories_keep_their_mode() {
    with_umask(0o022, || {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("workspace");
        fs::create_dir_all(&shared).unwrap();
        set_mode(&shared, 0o755);

        let nested = shared.join("agent-home").join("notes");
        create_private_dir_all(&nested).unwrap();

        assert_eq!(
            mode_of(&shared),
            0o755,
            "the shared parent must stay untouched"
        );
        assert_eq!(mode_of(&nested), 0o700);

        let shared_file = shared.join("project.txt");
        fs::write(&shared_file, b"keep").unwrap();
        set_mode(&shared_file, 0o644);
        assert_eq!(mode_of(&shared_file), 0o644);
    });
}

#[test]
fn sqlite_sidecars_are_restricted_when_present() {
    with_umask(0o022, || {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("diagnostics.sqlite");
        fs::write(&db, b"db").unwrap();
        set_mode(&db, 0o644);
        let wal = root.path().join("diagnostics.sqlite-wal");
        fs::write(&wal, b"wal").unwrap();
        set_mode(&wal, 0o644);

        restrict_sqlite_database(&db).unwrap();

        assert_eq!(mode_of(&db), 0o600);
        assert_eq!(mode_of(&wal), 0o600);
        assert!(
            !root.path().join("diagnostics.sqlite-shm").exists(),
            "missing sidecars stay missing"
        );
    });
}
