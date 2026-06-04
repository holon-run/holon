use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction};

#[derive(Debug, Clone)]
pub struct RuntimeDb {
    path: PathBuf,
    lock_path: PathBuf,
}

#[derive(Debug)]
pub struct RuntimeDbLock {
    file: File,
    path: PathBuf,
}

#[derive(Debug)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "runtime_db_foundation",
    sql: r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS storage_domains (
  domain TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL,
  import_status TEXT NOT NULL,
  canonical_source TEXT NOT NULL,
  source_checkpoint_json TEXT,
  imported_at TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agents (
  agent_id TEXT PRIMARY KEY,
  status TEXT,
  visibility TEXT,
  ownership TEXT,
  profile_preset TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT
);

CREATE TABLE IF NOT EXISTS audit_events (
  audit_event_id TEXT PRIMARY KEY,
  event_seq INTEGER,
  agent_id TEXT,
  kind TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_storage_domains_import_status
  ON storage_domains(import_status);

CREATE INDEX IF NOT EXISTS idx_agents_status
  ON agents(status);

CREATE INDEX IF NOT EXISTS idx_audit_events_agent_created
  ON audit_events(agent_id, created_at);

CREATE INDEX IF NOT EXISTS idx_audit_events_event_seq
  ON audit_events(event_seq);
"#,
}];

impl RuntimeDb {
    pub fn open_and_migrate(
        path: impl Into<PathBuf>,
        lock_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let db = Self {
            path: path.into(),
            lock_path: lock_path.into(),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub fn connection(&self) -> Result<Connection> {
        open_connection(&self.path)
    }

    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        match f(&transaction) {
            Ok(value) => {
                transaction.commit()?;
                Ok(value)
            }
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        }
    }

    pub fn current_schema_version(&self) -> Result<i64> {
        let connection = self.connection()?;
        current_schema_version(&connection)
    }

    pub fn migrate(&self) -> Result<()> {
        let _lock = RuntimeDbLock::lock(&self.lock_path)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
        }
        let mut connection = self.connection()?;
        ensure_migration_table(&connection)?;
        let current_version = current_schema_version(&connection)?;
        let max_known_version = max_known_migration_version();
        if current_version > max_known_version {
            bail!(
                "runtime db schema version {} is newer than this binary supports ({})",
                current_version,
                max_known_version
            );
        }
        for migration in MIGRATIONS {
            apply_migration(&mut connection, migration)?;
        }
        Ok(())
    }
}

impl RuntimeDbLock {
    pub fn lock(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(path.into(), LockMode::Blocking)
    }

    pub fn try_lock(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(path.into(), LockMode::NonBlocking)
    }

    fn open(path: PathBuf, mode: LockMode) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating runtime lock directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening runtime db lock {}", path.display()))?;
        flock(&file, mode).with_context(|| format!("locking runtime db {}", path.display()))?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RuntimeDbLock {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

#[derive(Debug, Clone, Copy)]
enum LockMode {
    Blocking,
    NonBlocking,
}

fn open_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
    }
    let connection =
        Connection::open(path).with_context(|| format!("opening runtime db {}", path.display()))?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
"#,
    )?;
    Ok(())
}

fn ensure_migration_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
"#,
    )?;
    Ok(())
}

fn apply_migration(connection: &mut Connection, migration: &Migration) -> Result<()> {
    let existing_name: Option<String> = connection
        .query_row(
            "SELECT name FROM schema_migrations WHERE version = ?1",
            [migration.version],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_name) = existing_name {
        if existing_name != migration.name {
            bail!(
                "runtime db migration {} name mismatch: expected {}, found {}",
                migration.version,
                migration.name,
                existing_name
            );
        }
        return Ok(());
    }

    let transaction = connection.transaction()?;
    transaction.execute_batch(migration.sql)?;
    transaction.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        (
            migration.version,
            migration.name,
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
    )?;
    transaction.commit()?;
    Ok(())
}

fn current_schema_version(connection: &Connection) -> Result<i64> {
    ensure_migration_table(connection)?;
    let version = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}

fn max_known_migration_version() -> i64 {
    MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

#[cfg(unix)]
fn flock(file: &File, mode: LockMode) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut operation = libc::LOCK_EX;
    if matches!(mode, LockMode::NonBlocking) {
        operation |= libc::LOCK_NB;
    }
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if matches!(mode, LockMode::NonBlocking) {
        if let Some(raw_error) = error.raw_os_error() {
            if raw_error == libc::EWOULDBLOCK || raw_error == libc::EAGAIN {
                return Err(anyhow!("runtime db lock is already held"));
            }
        }
    }
    Err(error.into())
}

#[cfg(unix)]
fn unlock(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(not(unix))]
fn flock(_file: &File, _mode: LockMode) -> Result<()> {
    bail!("runtime db file lock is only implemented on Unix platforms")
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use tempfile::TempDir;

    pub struct TempRuntimeDb {
        pub db: RuntimeDb,
        _temp_dir: TempDir,
    }

    impl TempRuntimeDb {
        pub fn new() -> Result<Self> {
            let temp_dir = tempfile::tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                temp_dir.path().join("state/runtime.sqlite"),
                temp_dir.path().join("state/runtime.lock"),
            )?;
            Ok(Self {
                db,
                _temp_dir: temp_dir,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn temp_paths() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp_dir = tempdir()?;
        let db_path = temp_dir.path().join("state/runtime.sqlite");
        let lock_path = temp_dir.path().join("state/runtime.lock");
        Ok((temp_dir, db_path, lock_path))
    }

    #[test]
    fn runtime_db_fresh_migration_creates_foundation_schema() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;

        let version = db.current_schema_version()?;
        assert_eq!(version, 1);
        for table in [
            "schema_migrations",
            "storage_domains",
            "agents",
            "audit_events",
        ] {
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "missing table {table}");
        }

        Ok(())
    }

    #[test]
    fn runtime_db_migration_is_idempotent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let connection = open_connection(&db_path)?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })?;
        assert_eq!(count, 1);
        assert_eq!(current_schema_version(&connection)?, 1);
        Ok(())
    }

    #[test]
    fn runtime_db_schema_version_comes_from_schema_migrations() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        connection.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            (7_i64, "future_test", Utc::now().to_rfc3339()),
        )?;

        assert_eq!(current_schema_version(&connection)?, 7);
        Ok(())
    }

    #[test]
    fn runtime_db_migration_name_mismatch_fails() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (1_i64, "wrong_name", Utc::now().to_rfc3339()),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error.to_string().contains("name mismatch"));
        Ok(())
    }

    #[test]
    fn runtime_db_migration_rejects_newer_schema_version() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (
                    max_known_migration_version() + 1,
                    "future_test",
                    Utc::now().to_rfc3339(),
                ),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error
            .to_string()
            .contains("newer than this binary supports"));
        Ok(())
    }

    #[test]
    fn runtime_db_foreign_keys_are_enabled_per_connection() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let enabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        assert_eq!(enabled, 1);
        Ok(())
    }

    #[test]
    fn runtime_db_transaction_helper_commits_and_rolls_back() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO storage_domains (
                    domain, schema_version, import_status, canonical_source, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                ("test", 1_i64, "pending", "jsonl", Utc::now().to_rfc3339()),
            )?;
            Ok(())
        })?;
        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);

        let error = db
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO storage_domains (
                        domain, schema_version, import_status, canonical_source, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    (
                        "rollback",
                        1_i64,
                        "pending",
                        "jsonl",
                        Utc::now().to_rfc3339(),
                    ),
                )?;
                Err::<(), anyhow::Error>(anyhow!("force rollback"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "force rollback");

        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn runtime_db_temp_helper_uses_isolated_state_dir() -> Result<()> {
        let temp_db = test_support::TempRuntimeDb::new()?;
        assert!(temp_db.db.path().ends_with("state/runtime.sqlite"));
        assert!(temp_db.db.lock_path().ends_with("state/runtime.lock"));
        assert_eq!(temp_db.db.current_schema_version()?, 1);
        Ok(())
    }

    #[test]
    fn runtime_db_lock_rejects_second_nonblocking_holder() -> Result<()> {
        if let Ok(lock_path) = std::env::var("HOLON_RUNTIME_DB_LOCK_CHILD_PATH") {
            RuntimeDbLock::try_lock(lock_path).expect_err("second process should not get lock");
            return Ok(());
        }

        let temp_dir = tempdir()?;
        let lock_path = temp_dir.path().join("state/runtime.lock");
        let first = RuntimeDbLock::lock(&lock_path)?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_db::tests::runtime_db_lock_rejects_second_nonblocking_holder")
            .arg("--nocapture")
            .env("HOLON_RUNTIME_DB_LOCK_CHILD_PATH", &lock_path)
            .output()?;
        assert!(
            output.status.success(),
            "child lock assertion failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        drop(first);

        let second = RuntimeDbLock::try_lock(&lock_path)?;
        assert_eq!(second.path(), lock_path.as_path());
        Ok(())
    }
}
