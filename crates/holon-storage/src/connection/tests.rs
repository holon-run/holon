use super::*;
use crate::diagnostics::NO_DIAGNOSTICS;

fn open_connection(path: &Path) -> Result<Connection> {
    super::open_connection(path, &NO_DIAGNOSTICS)
}
#[cfg(target_os = "linux")]
use crate::write_queue::RuntimeDbWriter;
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::fd::IntoRawFd;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use tempfile::tempdir;

#[cfg(target_os = "linux")]
const OBSERVER_CHILD_DB_ENV: &str = "HOLON_RUNTIME_DB_OBSERVER_CHILD_DB";
#[cfg(target_os = "linux")]
const OBSERVER_CHILD_READY_ENV: &str = "HOLON_RUNTIME_DB_OBSERVER_CHILD_READY";
#[cfg(target_os = "linux")]
const OBSERVER_CHILD_RELEASE_ENV: &str = "HOLON_RUNTIME_DB_OBSERVER_CHILD_RELEASE";
#[cfg(target_os = "linux")]
const OBSERVER_TEST_NAME: &str =
    "connection::tests::external_read_write_observer_does_not_replace_runtime_db_sidecars";
#[cfg(target_os = "linux")]
const LOCK_PROBE_PATH_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_PATH";
#[cfg(target_os = "linux")]
const LOCK_PROBE_READY_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_READY";
#[cfg(target_os = "linux")]
const LOCK_PROBE_START_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_START";
#[cfg(target_os = "linux")]
const LOCK_PROBE_LEN_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_LEN";
#[cfg(target_os = "linux")]
const LOCK_PROBE_EXPECT_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_EXPECT";
#[cfg(target_os = "linux")]
const LOCK_PROBE_TEST_NAME: &str = "connection::tests::runtime_db_posix_lock_probe_child";

fn persistent_wal_setting(connection: &Connection) -> Result<i32> {
    const MAIN_SCHEMA: &[u8] = b"main\0";
    let mut current = -1_i32;
    // SAFETY: the connection remains alive for the call, the schema name is
    // nul-terminated, and SQLite writes the result before returning.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            MAIN_SCHEMA.as_ptr().cast(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut current as *mut i32).cast(),
        )
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(anyhow!(rusqlite::ffi::Error::new(result)));
    }
    Ok(current)
}

fn sidecar_path(db_path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut path: OsString = db_path.as_os_str().to_owned();
    path.push(suffix);
    path.into()
}

#[cfg(target_os = "linux")]
fn try_posix_write_lock(file: &File, start: i64, len: i64) -> std::io::Result<()> {
    // SAFETY: flock is initialized before the fcntl call and the file remains open.
    let mut lock = unsafe { std::mem::zeroed::<libc::flock>() };
    lock.l_type = libc::F_WRLCK as libc::c_short;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    lock.l_start = start as libc::off_t;
    lock.l_len = len as libc::off_t;
    // SAFETY: the descriptor and flock pointer remain valid for the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) } == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn wait_for_child_ready(
    mut child: std::process::Child,
    ready_path: &Path,
    label: &str,
) -> Result<std::process::Child> {
    let started_at = Instant::now();
    while !ready_path.exists() {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            anyhow::bail!(
                "{label} exited before ready: status={}; stdout={}; stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if started_at.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            anyhow::bail!(
                "timed out waiting for {label}: stdout={}; stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(child)
}

#[cfg(target_os = "linux")]
fn wait_for_child_exit(
    mut child: std::process::Child,
    label: &str,
) -> Result<std::process::Output> {
    let started_at = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child
                .wait_with_output()
                .with_context(|| format!("collecting {label} output"));
        }
        if started_at.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            anyhow::bail!(
                "timed out waiting for {label} to exit: stdout={}; stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn assert_lock_conflict_from_child(path: &Path, start: i64, len: i64) -> Result<()> {
    let ready_directory = tempdir()?;
    let ready_path = ready_directory.path().join("ready");
    let child = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg(LOCK_PROBE_TEST_NAME)
        .arg("--nocapture")
        .env(LOCK_PROBE_PATH_ENV, path)
        .env(LOCK_PROBE_READY_ENV, &ready_path)
        .env(LOCK_PROBE_START_ENV, start.to_string())
        .env(LOCK_PROBE_LEN_ENV, len.to_string())
        .env(LOCK_PROBE_EXPECT_ENV, "conflict")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child = wait_for_child_ready(child, &ready_path, "POSIX lock probe")?;
    let output = wait_for_child_exit(child, "POSIX lock probe")?;
    if !output.status.success() {
        anyhow::bail!(
            "POSIX lock probe failed: status={}; stdout={}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn runtime_db_posix_lock_probe_child() -> Result<()> {
    let Some(path) = std::env::var_os(LOCK_PROBE_PATH_ENV) else {
        return Ok(());
    };
    let ready_path = std::env::var_os(LOCK_PROBE_READY_ENV)
        .ok_or_else(|| anyhow!("POSIX lock probe ready path is missing"))?;
    let start = std::env::var(LOCK_PROBE_START_ENV)
        .context("POSIX lock probe start is missing")?
        .parse::<i64>()
        .context("parsing POSIX lock probe start")?;
    let len = std::env::var(LOCK_PROBE_LEN_ENV)
        .context("POSIX lock probe length is missing")?
        .parse::<i64>()
        .context("parsing POSIX lock probe length")?;
    let expected =
        std::env::var(LOCK_PROBE_EXPECT_ENV).context("POSIX lock probe expectation is missing")?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening POSIX lock probe file: {}", path.to_string_lossy()))?;
    File::create(ready_path)?;

    let result = try_posix_write_lock(&file, start, len);
    match expected.as_str() {
        "conflict" => match result {
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EACCES) | Some(libc::EAGAIN)
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error).context("POSIX lock probe failed unexpectedly"),
            Ok(()) => anyhow::bail!("POSIX lock probe unexpectedly acquired the lock"),
        },
        "acquire" => result.context("POSIX lock probe could not acquire the lock"),
        _ => anyhow::bail!("unknown POSIX lock probe expectation: {expected}"),
    }
}

#[test]
fn every_runtime_db_connection_enables_persistent_wal() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let first = open_connection(&db_path)?;
    let second = open_connection(&db_path)?;

    assert_eq!(persistent_wal_setting(&first)?, 1);
    assert_eq!(persistent_wal_setting(&second)?, 1);
    Ok(())
}

#[test]
fn closing_last_runtime_db_connection_preserves_wal_sidecars() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let connection = open_connection(&db_path)?;
    configure_persistent_database(&connection)?;
    connection.execute_batch(
        "CREATE TABLE values_seen(value INTEGER NOT NULL);
             INSERT INTO values_seen(value) VALUES (1);",
    )?;

    let wal_path = sidecar_path(&db_path, "-wal");
    let shm_path = sidecar_path(&db_path, "-shm");
    assert!(wal_path.is_file());
    assert!(shm_path.is_file());

    drop(connection);

    assert!(wal_path.is_file());
    assert!(shm_path.is_file());
    let reader = open_connection(&db_path)?;
    assert_eq!(
        reader.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
            .get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn deleted_open_sidecar_prevents_new_runtime_db_connection() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let wal_path = sidecar_path(&db_path, "-wal");
    let _deleted_open_file = File::create(&wal_path)?;
    fs::remove_file(&wal_path)?;
    File::create(&wal_path)?;

    let error = open_connection(&db_path).expect_err("deleted-open WAL must be rejected");
    let message = format!("{error:#}");
    assert!(message.contains("sidecar divergence detected"));
    assert!(message.contains("deleted-open sidecar"));
    assert!(message.contains("sidecar=-wal"));
    assert!(message.contains("open_inode="));
    assert!(message.contains("canonical_identity="));
    Ok(())
}

#[cfg(target_os = "linux")]
fn runtime_db_sidecar_fd_scans(db_path: &Path) -> Result<u64> {
    let canonical = db_path.canonicalize()?;
    Ok(verified_runtime_db_sidecars(&canonical)
        .map(|verified| verified.fd_scans)
        .unwrap_or(0))
}

#[cfg(target_os = "linux")]
#[test]
fn sidecar_fd_scans_are_skipped_while_identities_are_unchanged() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let writer = open_connection(&db_path)?;
    configure_persistent_database(&writer)?;
    writer.execute_batch(
        "CREATE TABLE values_seen(value INTEGER NOT NULL);
             INSERT INTO values_seen(value) VALUES (1);",
    )?;
    drop(writer);

    // Absorb the one scan owed to the (absent -> present) sidecar
    // transition so the cache reflects the live sidecar identities.
    let settled = open_connection(&db_path)?;
    drop(settled);
    let scans_before = runtime_db_sidecar_fd_scans(&db_path)?;

    let reader = open_connection(&db_path)?;
    let reader_two = open_connection(&db_path)?;
    assert_eq!(
        runtime_db_sidecar_fd_scans(&db_path)?,
        scans_before,
        "opens with unchanged sidecar identities must not rescan /proc/self/fd"
    );
    assert_eq!(
        reader.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
            .get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        reader_two.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
            .get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn replaced_sidecar_identity_retriggers_the_fd_scan() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let writer = open_connection(&db_path)?;
    configure_persistent_database(&writer)?;
    writer.execute_batch("CREATE TABLE values_seen(value INTEGER NOT NULL);")?;
    drop(writer);
    // Warm the verified-identity cache for the current sidecars.
    let settled = open_connection(&db_path)?;
    drop(settled);

    let wal_path = sidecar_path(&db_path, "-wal");
    let _held_open_sidecar = File::open(&wal_path)?;
    fs::remove_file(&wal_path)?;
    File::create(&wal_path)?;

    // The refusal itself proves the identity change re-ran the fd scan;
    // a wrongly trusted cache would have opened without detecting the
    // deleted-open sidecar.
    let error = open_connection(&db_path).expect_err("deleted-open WAL must be rejected");
    assert!(format!("{error:#}").contains("deleted-open sidecar"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn sidecar_inspection_preserves_posix_write_lock() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let wal_path = sidecar_path(&db_path, "-wal");
    let locked_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&wal_path)?;
    try_posix_write_lock(&locked_file, 0, 1)?;
    assert_lock_conflict_from_child(&wal_path, 0, 1)?;

    let fd_path = Path::new("/proc/self/fd").join(locked_file.as_raw_fd().to_string());
    let deleted_path = format!("{} (deleted)", wal_path.display());
    let metadata = locked_file.metadata()?;
    assert_eq!(
        inspect_runtime_db_sidecar_fd(&fd_path, &wal_path, &deleted_path)?,
        Some(RuntimeDbSidecarOpen {
            deleted: false,
            identity: (metadata.dev(), metadata.ino()),
        })
    );

    assert_lock_conflict_from_child(&wal_path, 0, 1)?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn sidecar_inspection_ignores_concurrent_fd_reuse() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let wal_path = sidecar_path(&db_path, "-wal");
    let main_path = db_path;
    let inspected_fd = File::create(&wal_path)?.into_raw_fd();
    let inspected_fd_path = Path::new("/proc/self/fd").join(inspected_fd.to_string());
    let deleted_path = format!("{} (deleted)", wal_path.display());
    let locked_main = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&main_path)?;
    assert_ne!(locked_main.as_raw_fd(), inspected_fd);
    try_posix_write_lock(&locked_main, 0, 1)?;
    assert_lock_conflict_from_child(&main_path, 0, 1)?;
    let locked_main_fd = locked_main.as_raw_fd();

    let observation = inspect_runtime_db_sidecar_fd_with_hook(
        &inspected_fd_path,
        &wal_path,
        &deleted_path,
        move || {
            thread::spawn(move || -> Result<()> {
                // SAFETY: both descriptors remain valid, and dup2 atomically replaces
                // the inspected sidecar descriptor with the locked main-db descriptor.
                if unsafe { libc::dup2(locked_main_fd, inspected_fd) } < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                Ok(())
            })
            .join()
            .map_err(|_| anyhow!("fd replacement thread panicked"))?
        },
    )?;

    assert_eq!(observation, None);
    assert_lock_conflict_from_child(&main_path, 0, 1)?;

    // SAFETY: dup2 transferred a duplicate of locked_main into raw ownership.
    unsafe {
        libc::close(inspected_fd);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn sidecar_inspection_classifies_metadata_handle_errors() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let wal_path = sidecar_path(&db_path, "-wal");
    let wal_file = File::create(&wal_path)?;
    let fd_path = Path::new("/proc/self/fd").join(wal_file.as_raw_fd().to_string());
    let deleted_path = format!("{} (deleted)", wal_path.display());

    let disappeared = inspect_runtime_db_sidecar_fd_with_hook_and_open(
        &fd_path,
        &wal_path,
        &deleted_path,
        || Ok(()),
        |_| Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
    )?;
    assert_eq!(disappeared, None);

    let error = inspect_runtime_db_sidecar_fd_with_hook_and_open(
        &fd_path,
        &wal_path,
        &deleted_path,
        || Ok(()),
        |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
    )
    .expect_err("systemic O_PATH failure must fail closed");
    assert!(format!("{error:#}").contains("opening runtime db fd metadata handle"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn sidecar_scan_error_does_not_update_verified_cache() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    File::create(&db_path)?;
    let wal_path = sidecar_path(&db_path, "-wal");
    let _wal_file = File::create(&wal_path)?;

    let error = ensure_runtime_db_sidecars_are_consistent_with(
        &db_path,
        &NO_DIAGNOSTICS,
        |fd_path, sidecar_path, deleted_path| {
            inspect_runtime_db_sidecar_fd_with_hook_and_open(
                fd_path,
                sidecar_path,
                deleted_path,
                || Ok(()),
                |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
            )
        },
    )
    .expect_err("sidecar scan must propagate systemic O_PATH failure");
    assert!(format!("{error:#}").contains("opening runtime db fd metadata handle"));
    assert!(
        verified_runtime_db_sidecars(&db_path.canonicalize()?).is_none(),
        "failed scans must not populate the verified identities cache"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn runtime_db_fd_metadata_handle_closes_on_drop() -> Result<()> {
    let directory = tempdir()?;
    let wal_path = directory.path().join("runtime.sqlite-wal");
    let wal_file = File::create(&wal_path)?;
    let fd_path = Path::new("/proc/self/fd").join(wal_file.as_raw_fd().to_string());
    let metadata_handle = open_runtime_db_fd_metadata_handle(&fd_path)?;
    let metadata_fd_path = Path::new("/proc/self/fd").join(metadata_handle.as_raw_fd().to_string());

    assert_eq!(fs::read_link(&metadata_fd_path)?, wal_path);
    assert!(metadata_handle.metadata()?.is_file());
    drop(metadata_handle);

    // Linux reuses fd numbers: a parallel test thread may open a new file that
    // lands on the dropped descriptor between drop and read_link. Only a link
    // still targeting the wal file proves the handle leaked; any other target
    // is benign fd-number reuse.
    match fs::read_link(&metadata_fd_path) {
        Ok(target) => assert_ne!(
            target, wal_path,
            "dropping the metadata handle must close its exact descriptor"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn sidecar_divergence_detection_resolves_runtime_db_symlink() -> Result<()> {
    use std::os::unix::fs::symlink;

    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let alias_path = directory.path().join("runtime-alias.sqlite");
    let connection = open_connection(&db_path)?;
    configure_persistent_database(&connection)?;
    connection.execute_batch("CREATE TABLE values_seen(value INTEGER NOT NULL);")?;
    drop(connection);
    symlink(&db_path, &alias_path)?;

    let wal_path = sidecar_path(&db_path, "-wal");
    let _deleted_open_file = File::open(&wal_path)?;
    fs::remove_file(&wal_path)?;
    File::create(&wal_path)?;

    let error = open_connection(&alias_path).expect_err("deleted-open WAL through alias must fail");
    assert!(format!("{error:#}").contains("deleted-open sidecar"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn external_read_write_observer_does_not_replace_runtime_db_sidecars() -> Result<()> {
    if let Some(db_path) = std::env::var_os(OBSERVER_CHILD_DB_ENV) {
        let connection = Connection::open(Path::new(&db_path))?;
        let max_value = connection.query_row("SELECT MAX(value) FROM values_seen", [], |row| {
            row.get::<_, i64>(0)
        })?;
        assert_eq!(max_value, 1);
        let ready_path = std::env::var_os(OBSERVER_CHILD_READY_ENV)
            .ok_or_else(|| anyhow!("observer child ready path is missing"))?;
        let release_path = std::env::var_os(OBSERVER_CHILD_RELEASE_ENV)
            .ok_or_else(|| anyhow!("observer child release path is missing"))?;
        File::create(ready_path)?;
        let started_at = Instant::now();
        while !Path::new(&release_path).exists() {
            if started_at.elapsed() > Duration::from_secs(10) {
                anyhow::bail!("timed out waiting to release observer child");
            }
            thread::sleep(Duration::from_millis(10));
        }
        drop(connection);

        for _ in 0..2 {
            let connection = Connection::open(Path::new(&db_path))?;
            let max_value =
                connection.query_row("SELECT MAX(value) FROM values_seen", [], |row| {
                    row.get::<_, i64>(0)
                })?;
            assert_eq!(max_value, 2);
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(Path::new(&db_path))?;
        let exclusive = libc::flock {
            l_type: libc::F_WRLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: SQLITE_SHARED_FIRST,
            l_len: SQLITE_SHARED_SIZE,
            l_pid: 0,
        };
        // SAFETY: `file` owns a valid descriptor and `exclusive` remains
        // valid for the duration of this non-blocking fcntl call.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &exclusive) };
        assert_eq!(result, -1, "external exclusive lock must be refused");
        assert!(matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EACCES | libc::EAGAIN)
        ));
        return Ok(());
    }

    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let ready_path = directory.path().join("observer-ready");
    let release_path = directory.path().join("observer-release");
    let connection = open_connection(&db_path)?;
    configure_persistent_database(&connection)?;
    connection.execute_batch(
        "CREATE TABLE values_seen(value INTEGER NOT NULL);
             INSERT INTO values_seen(value) VALUES (1);",
    )?;
    let writer = RuntimeDbWriter::open(db_path.clone(), connection)?;

    let wal_path = sidecar_path(&db_path, "-wal");
    let shm_path = sidecar_path(&db_path, "-shm");
    let wal_identity = fs::metadata(&wal_path)?.ino();
    let shm_identity = fs::metadata(&shm_path)?.ino();
    // SQLite's default Unix VFS uses process-associated POSIX locks. Closing
    // any descriptor for the same inode releases all of this process's locks,
    // including the writer connection's shared lock.
    drop(File::open(&db_path)?);
    let child = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg(OBSERVER_TEST_NAME)
        .arg("--nocapture")
        .env(OBSERVER_CHILD_DB_ENV, &db_path)
        .env(OBSERVER_CHILD_READY_ENV, &ready_path)
        .env(OBSERVER_CHILD_RELEASE_ENV, &release_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child = wait_for_child_ready(child, &ready_path, "runtime db observer")?;

    let canonical_db_path = db_path.canonicalize()?;
    for suffix in ["-wal", "-shm"] {
        ensure_runtime_db_sidecar_is_consistent_with(
            &canonical_db_path,
            suffix,
            &inspect_runtime_db_sidecar_fd,
        )?;
    }
    writer.append_wait_once(|transaction| {
        transaction.execute("INSERT INTO values_seen(value) VALUES (2)", [])?;
        Ok(())
    })?;

    File::create(&release_path)?;
    let output = wait_for_child_exit(child, "runtime db observer")?;
    assert!(
        output.status.success(),
        "observer child failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::metadata(&wal_path)?.ino(), wal_identity);
    assert_eq!(fs::metadata(&shm_path)?.ino(), shm_identity);
    writer.append_wait_once(|transaction| {
        transaction.execute("INSERT INTO values_seen(value) VALUES (3)", [])?;
        Ok(())
    })?;

    ensure_runtime_db_sidecars_are_consistent(&db_path, &NO_DIAGNOSTICS)?;
    drop(writer);

    let reader = open_connection(&db_path)?;
    assert_eq!(
        reader.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
            .get::<_, i64>(0))?,
        3
    );
    Ok(())
}

use tracing_subscriber::prelude::*;

#[derive(Default, Clone)]
struct WriteOutcomeRecorder {
    events: std::sync::Arc<std::sync::Mutex<Vec<RecordedOutcome>>>,
}

type RecordedOutcome = (tracing::Level, RecordedOutcomeFields);

#[derive(Clone, Default)]
struct RecordedOutcomeFields {
    message: String,
    reader_reprepare: bool,
    callback_phases_us: Option<String>,
}

impl tracing::field::Visit for RecordedOutcomeFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "callback_phases_us" => self.callback_phases_us = Some(format!("{value:?}")),
            _ => {}
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "reader_reprepare" {
            self.reader_reprepare = value;
        }
    }
}

impl<S> tracing_subscriber::layer::Layer<S> for WriteOutcomeRecorder
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != "holon_storage::connection" {
            return;
        }
        let mut fields = RecordedOutcomeFields::default();
        event.record(&mut fields);
        self.events
            .lock()
            .unwrap()
            .push((*event.metadata().level(), fields));
    }
}

fn captured_write_outcomes(failure: anyhow::Error) -> Vec<RecordedOutcome> {
    let recorder = WriteOutcomeRecorder::default();
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("write-outcome-probe.db");
    let result: Result<()> = tracing::subscriber::with_default(
        tracing_subscriber::registry().with(recorder.clone()),
        || {
            let connection = open_connection(&db_path)?;
            run_transaction_on_connection(
                &connection,
                &db_path,
                RuntimeDbWriteContext::sync("probe.write", "probe"),
                Duration::ZERO,
                Duration::ZERO,
                None,
                |_transaction| Err(failure),
            )
        },
    );
    assert!(result.is_err());
    let events = recorder.events.lock().unwrap().clone();
    events
}

fn warn_failures(events: &[RecordedOutcome]) -> usize {
    events
        .iter()
        .filter(|(level, fields)| {
            matches!(*level, tracing::Level::WARN)
                && fields.message.contains("write callback failed")
        })
        .count()
}

#[test]
fn reader_reprepare_marker_is_quiet_not_a_write_failure() {
    let events = captured_write_outcomes(anyhow::Error::new(ReaderReprepareRequested::new("task")));
    assert!(events.iter().any(|(level, fields)| {
        matches!(*level, tracing::Level::DEBUG)
            && fields.reader_reprepare
            && fields.message.contains("deferred to reader reprepare")
    }));
    assert_eq!(warn_failures(&events), 0);
}

#[test]
fn retryable_conflict_rollback_keeps_rolled_back_debug() {
    let events = captured_write_outcomes(anyhow::Error::new(
        RuntimeStateTransitionConflict::concurrent_mutation("task", "task:probe"),
    ));
    assert!(events.iter().any(|(level, fields)| {
        matches!(*level, tracing::Level::DEBUG) && fields.message.contains("write rolled back")
    }));
    assert_eq!(warn_failures(&events), 0);
}

#[test]
fn unmarked_rolled_back_error_keeps_failure_warn() {
    let events = captured_write_outcomes(anyhow!("probe business failure"));
    assert_eq!(warn_failures(&events), 1);
    assert!(!events.iter().any(|(_, fields)| {
        fields.reader_reprepare && fields.message.contains("deferred to reader reprepare")
    }));
}

#[test]
fn write_callback_phases_follow_transaction_outcomes() -> Result<()> {
    use crate::write_callback_phases::WriteCallbackPhase;

    let recorder = WriteOutcomeRecorder::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(recorder.clone()));
    let dir = tempdir()?;
    let path = dir.path().join("callback-phases.db");
    let connection = open_connection(&path)?;
    let context = RuntimeDbWriteContext::sync("probe.write", "probe");
    let phases = WriteCallbackPhases::default();
    let attempt = |wait, phases, f: fn(Option<&WriteCallbackPhases>) -> Result<()>| {
        run_transaction_on_connection(
            &connection,
            &path,
            context,
            wait,
            Duration::ZERO,
            phases,
            |_| f(phases),
        )
    };
    assert!(attempt(Duration::ZERO, Some(&phases), |phases| {
        let _phase = WriteCallbackPhase::QueueBusiness.start(phases);
        Err(anyhow!("probe callback rollback"))
    })
    .is_err());
    // Use a synthetic queue wait for the existing slow-write threshold, with
    // no sleep inside the callback. A new attempt clears the failed stage.
    attempt(Duration::from_secs(1), Some(&phases), |phases| {
        let _phase = WriteCallbackPhase::QueuePreflight.start(phases);
        Ok(())
    })?;
    attempt(Duration::from_secs(1), None, |_| Ok(()))?;
    attempt(Duration::ZERO, Some(&phases), |phases| {
        let _phase = WriteCallbackPhase::QueueFinalize.start(phases);
        Ok(())
    })?;

    let events = recorder.events.lock().unwrap();
    let attributed = events
        .iter()
        .filter_map(|(level, fields)| {
            fields
                .callback_phases_us
                .as_deref()
                .map(|phases| (*level, fields.message.as_str(), phases))
        })
        .collect::<Vec<_>>();
    let [(rolled_back, error, business), (committed, slow, preflight)] = attributed.as_slice()
    else {
        panic!("expected timings on one rollback and one slow commit: {attributed:?}");
    };
    assert_eq!(
        (*rolled_back, *committed),
        (tracing::Level::WARN, tracing::Level::WARN)
    );
    assert!(error.contains("write callback failed"));
    assert_eq!(*slow, "slow runtime db write");
    assert!(business
        .strip_prefix("queue_business:")
        .unwrap()
        .parse::<u128>()
        .is_ok());
    assert!(preflight
        .strip_prefix("queue_preflight:")
        .unwrap()
        .parse::<u128>()
        .is_ok());
    Ok(())
}

#[test]
fn sqlite_busy_classification_is_unchanged() {
    let busy = anyhow::Error::new(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: ErrorCode::DatabaseBusy,
            extended_code: 5,
        },
        None,
    ));
    assert!(is_retryable_db_error(&busy));
    assert!(!is_reader_reprepare_requested(&busy));
    assert!(!is_retryable_db_error(&anyhow!("probe business failure")));
}

#[test]
fn ownership_unresolved_marker_overrides_retryable_classification() {
    // The write callback reads the same typed marker as the deletion job state
    // machine, so a failed ownership invariant is never reported as retryable.
    let unresolved = holon_domain::deletion_failure::mark_ownership_unresolved(anyhow::anyhow!(
        "shared history requires explicit ownership resolution"
    ));
    assert!(!is_retryable_db_error(&unresolved));
}
