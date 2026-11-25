//! Offline startup validation for a real runtime library.
//!
//! `holon daemon validate-startup` answers whether the current runtime library
//! is acceptable to this binary without starting a daemon. The default mode is
//! strictly read-only: it never migrates, initializes storage domains, upserts
//! workspace/identity rows, normalizes wakes or queues, or appends audit
//! events. `--migrate` performs the existing [`RuntimeDb::migrate`] operation on
//! an explicitly selected copy after proving the daemon is stopped.
//!
//! Read-only inspection opens the database with `SQLITE_OPEN_READ_ONLY` plus
//! `immutable=1` so SQLite never creates or rewrites `-shm`/`-wal` sidecars.
//! A stopped runtime can legitimately leave pending `-wal` frames, and SQLite
//! can only read them by mapping (and rewriting) `-shm`. In that case the
//! validation copies the database and its sidecars into a temporary directory
//! and inspects the copy, so the source data directory stays byte- and
//! mtime-identical.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::runtime_db::migrations::{max_known_migration_version, MIGRATIONS};
use crate::runtime_db::{RuntimeDb, RuntimeDbLock};

/// Version written as `schema_version` inside the validation payload.
pub const VALIDATION_SCHEMA_VERSION: u32 = 1;

const MAX_OPERATOR_MESSAGE_CHARS: usize = 400;
const MAX_AGENT_SUMMARIES: usize = 16;
const MAX_DOMAIN_NAMES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupValidationMode {
    ReadOnly,
    Migrate,
}

impl StartupValidationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Migrate => "migrate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseName {
    Paths,
    DaemonGate,
    DbPresence,
    SchemaVerify,
    Migrate,
    StorageDomains,
    AgentRegistry,
    Quarantine,
    Eligibility,
    Summary,
}

pub const PHASES: [PhaseName; 10] = [
    PhaseName::Paths,
    PhaseName::DaemonGate,
    PhaseName::DbPresence,
    PhaseName::SchemaVerify,
    PhaseName::Migrate,
    PhaseName::StorageDomains,
    PhaseName::AgentRegistry,
    PhaseName::Quarantine,
    PhaseName::Eligibility,
    PhaseName::Summary,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseReport {
    pub phase: PhaseName,
    pub status: PhaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    ConfigInvalid,
    PathsUnresolved,
    DbMissing,
    DbNotReadable,
    SchemaNewerThanBinary,
    MigrationNameMismatch,
    MigrationRequired,
    MigrationFailed,
    BackfillResourceExhausted,
    StorageDomainMissing,
    DaemonLeaseHeld,
    DaemonRunning,
}

impl FailureCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfigInvalid => "config_invalid",
            Self::PathsUnresolved => "paths_unresolved",
            Self::DbMissing => "db_missing",
            Self::DbNotReadable => "db_not_readable",
            Self::SchemaNewerThanBinary => "schema_newer_than_binary",
            Self::MigrationNameMismatch => "migration_name_mismatch",
            Self::MigrationRequired => "migration_required",
            Self::MigrationFailed => "migration_failed",
            Self::BackfillResourceExhausted => "backfill_resource_exhausted",
            Self::StorageDomainMissing => "storage_domain_missing",
            Self::DaemonLeaseHeld => "daemon_lease_held",
            Self::DaemonRunning => "daemon_running",
        }
    }

    pub fn is_blocked(self) -> bool {
        matches!(self, Self::DaemonLeaseHeld | Self::DaemonRunning)
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::DbNotReadable
                | Self::DaemonLeaseHeld
                | Self::DaemonRunning
                | Self::MigrationRequired
                | Self::BackfillResourceExhausted
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceCertainty {
    Unchanged,
    PartiallyChanged,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    NoActiveDefaultAgent,
    QuarantineUnavailable,
    StorageDomainsInitializedOnStart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationFailure {
    pub code: FailureCode,
    pub phase: PhaseName,
    pub operator_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence_certainty: Option<PersistenceCertainty>,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationWarning {
    pub code: WarningCode,
    pub phase: PhaseName,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationPaths {
    pub home_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_db: PathBuf,
    pub maintenance_lock: PathBuf,
    pub control_socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbSummary {
    pub mode: StartupValidationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<i64>,
    pub max_known: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub agent_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsSummary {
    pub total: usize,
    pub default_resolution: String,
    pub summaries: Vec<AgentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineSummary {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligibilityAgentSummary {
    pub agent_id: String,
    pub current_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligibilitySummary {
    pub snapshot: String,
    pub captures: u64,
    pub current_records: u64,
    pub eligibility_records: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<EligibilityAgentSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Ok,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupValidationReport {
    pub schema_version: u32,
    pub status: ReportStatus,
    pub mode: StartupValidationMode,
    pub paths: ValidationPaths,
    pub phases: Vec<PhaseReport>,
    pub db: DbSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentsSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine: Option<QuarantineSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility: Option<EligibilitySummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<ValidationFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ValidationWarning>,
    pub elapsed_ms: u64,
}

/// Validate the runtime library selected by `config` without starting a daemon.
pub fn validate_startup(
    config: &AppConfig,
    mode: StartupValidationMode,
) -> StartupValidationReport {
    validate_startup_with_migrator(config, mode, &|path| {
        let db = RuntimeDb::open_and_migrate(path, config.runtime_db_lock_path())
            .context("runtime database migration failed")?;
        drop(db);
        Ok(())
    })
}

/// Same as [`validate_startup`] with an injectable migration step, so the
/// failure contract can be exercised hermetically.
fn validate_startup_with_migrator(
    config: &AppConfig,
    mode: StartupValidationMode,
    migrate: &dyn Fn(&Path) -> Result<()>,
) -> StartupValidationReport {
    let started = Instant::now();
    let paths = ValidationPaths {
        home_dir: config.home_dir.clone(),
        data_dir: config.data_dir.clone(),
        runtime_db: config.runtime_db_path(),
        maintenance_lock: config.runtime_db_maintenance_lock_path(),
        control_socket: config.socket_path.clone(),
    };
    let mut phases: Vec<PhaseReport> = PHASES
        .iter()
        .map(|phase| PhaseReport {
            phase: *phase,
            status: PhaseStatus::Skipped,
            elapsed_ms: None,
        })
        .collect();
    let mut state = InspectState::new(mode);
    let mut warnings = Vec::new();

    let failure = execute(
        config,
        mode,
        migrate,
        &paths,
        &mut phases,
        &mut state,
        &mut warnings,
    )
    .err();

    let status = match &failure {
        None => ReportStatus::Ok,
        Some(failure) if failure.code.is_blocked() => ReportStatus::Blocked,
        Some(_) => ReportStatus::Failed,
    };
    let mut failures = Vec::new();
    if let Some(failure) = failure {
        if failure.code == FailureCode::BackfillResourceExhausted {
            let mut general = failure.clone();
            general.code = FailureCode::MigrationFailed;
            general.operator_message = bounded_message(
                "runtime database migration failed; inspect the migration metadata and daemon logs before retrying",
            );
            general.retryable = false;
            failures.push(general);
        }
        failures.push(failure);
    }
    let summary_index = phase_index(PhaseName::Summary);
    phases[summary_index] = PhaseReport {
        phase: PhaseName::Summary,
        status: match status {
            ReportStatus::Ok => PhaseStatus::Ok,
            ReportStatus::Failed | ReportStatus::Blocked => PhaseStatus::Failed,
        },
        elapsed_ms: None,
    };

    StartupValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION,
        status,
        mode,
        paths,
        phases,
        db: state.db,
        agents: state.agents,
        quarantine: state.quarantine,
        eligibility: state.eligibility,
        failures,
        warnings,
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
    }
}

struct InspectState {
    db: DbSummary,
    agents: Option<AgentsSummary>,
    quarantine: Option<QuarantineSummary>,
    eligibility: Option<EligibilitySummary>,
}

impl InspectState {
    fn new(mode: StartupValidationMode) -> Self {
        Self {
            db: DbSummary {
                mode,
                schema_version: None,
                max_known: max_known_migration_version(),
            },
            agents: None,
            quarantine: None,
            eligibility: None,
        }
    }
}

fn execute(
    config: &AppConfig,
    mode: StartupValidationMode,
    migrate: &dyn Fn(&Path) -> Result<()>,
    paths: &ValidationPaths,
    phases: &mut [PhaseReport],
    state: &mut InspectState,
    warnings: &mut Vec<ValidationWarning>,
) -> std::result::Result<(), ValidationFailure> {
    run_phase(phases, PhaseName::Paths, || check_paths(config, paths))?;

    let mut _lease: Option<RuntimeDbLock> = None;
    if mode == StartupValidationMode::Migrate {
        run_phase(phases, PhaseName::DaemonGate, || {
            let lease = RuntimeDbLock::try_lock(paths.maintenance_lock.clone()).map_err(|_| {
                failure(
                    FailureCode::DaemonLeaseHeld,
                    PhaseName::DaemonGate,
                    "the runtime maintenance lease is held; stop the daemon or wait for the other maintenance process to finish",
                )
            })?;
            if control_socket_live(&paths.control_socket) {
                return Err(failure(
                    FailureCode::DaemonRunning,
                    PhaseName::DaemonGate,
                    "a live control socket was found; migration mode requires the daemon to be stopped",
                ));
            }
            _lease = Some(lease);
            Ok(())
        })?;
    }

    let connection = run_phase(phases, PhaseName::DbPresence, || {
        open_for_inspection(&paths.runtime_db, mode)
    })?;
    let migration_target = run_phase(phases, PhaseName::SchemaVerify, || {
        verify_schema(&connection, mode, &mut state.db)
    })?;

    let connection = if mode == StartupValidationMode::Migrate {
        // `--migrate` always runs the real migration entrypoint, even when the
        // schema ledger is already current: `RuntimeDb::migrate` also performs
        // the on-open backfills and verification that decide whether
        // `holon serve` can start, so skipping it would report a library as
        // healthy that the daemon cannot open.
        let target = migration_target.unwrap_or_else(|| current_schema_target(&state.db));
        let before = state.db.schema_version;
        drop(connection);
        run_phase(phases, PhaseName::Migrate, || {
            migrate(&paths.runtime_db).map_err(|error| {
                migration_failure(&error, &paths.runtime_db, before, &target, mode)
            })?;
            let connection = open_for_inspection(&paths.runtime_db, mode).map_err(|_| {
                failure(
                    FailureCode::MigrationFailed,
                    PhaseName::Migrate,
                    "runtime database migration finished but the database could not be reopened for verification",
                )
                .with_migration(&target)
            })?;
            verify_migrated_schema(&connection, &target, &mut state.db)?;
            Ok(connection)
        })?
    } else {
        // A read-only run fails schema drift with `migration_required`, so no
        // migration target can be returned here.
        debug_assert!(migration_target.is_none());
        connection
    };

    let unregistered_domains = run_phase(phases, PhaseName::StorageDomains, || {
        check_storage_domains(&connection)
    })?;
    if !unregistered_domains.is_empty() {
        // Startup upserts a missing domain row before it validates the
        // registry, so an absent row is not a startup failure. Only an
        // existing but non-canonical row blocks the daemon.
        warnings.push(ValidationWarning {
            code: WarningCode::StorageDomainsInitializedOnStart,
            phase: PhaseName::StorageDomains,
            message: bounded_message(&format!(
                "the runtime storage domain registry is uninitialized ({}); startup inserts these rows before validating them",
                unregistered_domains.join(", ")
            )),
        });
    }
    run_phase(phases, PhaseName::AgentRegistry, || {
        inspect_agents(&connection, config, &mut state.agents, warnings)
    })?;
    run_phase(phases, PhaseName::Quarantine, || {
        inspect_quarantine(config, &mut state.quarantine, warnings)
    })?;
    run_phase(phases, PhaseName::Eligibility, || {
        inspect_eligibility(&connection, mode, &mut state.eligibility)
    })?;
    Ok(())
}

fn run_phase<T>(
    phases: &mut [PhaseReport],
    phase: PhaseName,
    run: impl FnOnce() -> std::result::Result<T, ValidationFailure>,
) -> std::result::Result<T, ValidationFailure> {
    let started = Instant::now();
    let index = phase_index(phase);
    match run() {
        Ok(value) => {
            phases[index] = PhaseReport {
                phase,
                status: PhaseStatus::Ok,
                elapsed_ms: Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
            };
            Ok(value)
        }
        Err(failure) => {
            phases[index] = PhaseReport {
                phase,
                status: PhaseStatus::Failed,
                elapsed_ms: Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
            };
            Err(failure)
        }
    }
}

fn phase_index(phase: PhaseName) -> usize {
    PHASES
        .iter()
        .position(|candidate| *candidate == phase)
        .expect("phase list is complete")
}

fn failure(code: FailureCode, phase: PhaseName, message: impl Into<String>) -> ValidationFailure {
    ValidationFailure {
        code,
        phase,
        operator_message: bounded_message(&message.into()),
        migration_version: None,
        migration_name: None,
        agent_id: None,
        persistence_certainty: None,
        retryable: code.retryable(),
    }
}

impl ValidationFailure {
    fn with_migration(mut self, target: &MigrationTarget) -> Self {
        self.migration_version = Some(target.version);
        self.migration_name = Some(target.name.clone());
        self
    }

    fn with_persistence(mut self, certainty: PersistenceCertainty) -> Self {
        self.persistence_certainty = Some(certainty);
        self
    }
}

fn bounded_message(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_OPERATOR_MESSAGE_CHARS).collect()
}

fn check_paths(
    _config: &AppConfig,
    paths: &ValidationPaths,
) -> std::result::Result<(), ValidationFailure> {
    if !paths.home_dir.is_dir() {
        return Err(failure(
            FailureCode::ConfigInvalid,
            PhaseName::Paths,
            "the resolved Holon home is missing or is not a directory",
        ));
    }
    for (label, path) in [
        ("data directory", &paths.data_dir),
        ("runtime database parent", &paths.runtime_db),
        ("control socket parent", &paths.control_socket),
    ] {
        let Some(parent) = path.parent() else {
            return Err(failure(
                FailureCode::PathsUnresolved,
                PhaseName::Paths,
                format!("the {label} path has no resolvable parent directory"),
            ));
        };
        if parent.exists() && !parent.is_dir() {
            return Err(failure(
                FailureCode::PathsUnresolved,
                PhaseName::Paths,
                format!("the {label} path conflicts with a non-directory entry"),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn control_socket_live(path: &Path) -> bool {
    use std::os::unix::net::UnixStream;
    path.exists() && UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn control_socket_live(path: &Path) -> bool {
    path.exists()
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// A read-only inspection handle. The temporary copy of a WAL-bearing
/// database lives exactly as long as the connection.
struct InspectionDb {
    connection: Connection,
    _copy: Option<tempfile::TempDir>,
}

impl std::ops::Deref for InspectionDb {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.connection
    }
}

fn open_for_inspection(
    path: &Path,
    mode: StartupValidationMode,
) -> std::result::Result<InspectionDb, ValidationFailure> {
    if !path.is_file() {
        return Err(failure(
            FailureCode::DbMissing,
            PhaseName::DbPresence,
            "the runtime database does not exist; this command never creates one",
        ));
    }
    let wal = sidecar_path(path, "-wal");
    let pending_wal = std::fs::metadata(&wal).is_ok_and(|metadata| metadata.len() > 0);
    if mode == StartupValidationMode::ReadOnly && pending_wal {
        // A stopped runtime legitimately keeps pending WAL frames. SQLite can
        // only read them by mapping (and therefore rewriting) `-shm`, so
        // inspecting the original path would violate the zero-write contract.
        // Read a byte copy in the system temp directory instead.
        return copy_for_read_only_inspection(path, &wal);
    }
    let url = sqlite_file_url(path, mode).map_err(|_| {
        failure(
            FailureCode::PathsUnresolved,
            PhaseName::DbPresence,
            "the runtime database path cannot be represented as a local SQLite URI",
        )
    })?;
    let connection = Connection::open_with_flags(
        url,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| {
        failure(
            FailureCode::DbNotReadable,
            PhaseName::DbPresence,
            "the runtime database cannot be opened read-only; verify file ownership and permissions",
        )
    })?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|_| {
            failure(
                FailureCode::DbNotReadable,
                PhaseName::DbPresence,
                "the runtime database refused a read-only query-only session",
            )
        })?;
    Ok(InspectionDb {
        connection,
        _copy: None,
    })
}

fn copy_for_read_only_inspection(
    path: &Path,
    wal: &Path,
) -> std::result::Result<InspectionDb, ValidationFailure> {
    let unreadable = || {
        failure(
            FailureCode::DbNotReadable,
            PhaseName::DbPresence,
            "the runtime database could not be copied for a zero-write read-only inspection",
        )
    };
    let dir = tempfile::tempdir().map_err(|_| unreadable())?;
    let copy = dir.path().join("runtime.sqlite");
    std::fs::copy(path, &copy).map_err(|_| unreadable())?;
    std::fs::copy(wal, sidecar_path(&copy, "-wal")).map_err(|_| unreadable())?;
    let hot = sidecar_path(path, "-shm");
    if hot.is_file() {
        std::fs::copy(&hot, sidecar_path(&copy, "-shm")).map_err(|_| unreadable())?;
    }
    let url = sqlite_file_url(&copy, StartupValidationMode::Migrate).map_err(|_| unreadable())?;
    let connection = Connection::open_with_flags(
        url,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| unreadable())?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|_| unreadable())?;
    Ok(InspectionDb {
        connection,
        _copy: Some(dir),
    })
}

/// Read-only mode uses `immutable=1` so SQLite never creates sidecars. Migrate
/// mode reads the accurate WAL view because that mode is allowed to write.
fn sqlite_file_url(path: &Path, mode: StartupValidationMode) -> Result<String> {
    let mut url = url::Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("runtime db path is not an absolute local path"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("mode", "ro");
        if mode == StartupValidationMode::ReadOnly {
            query.append_pair("immutable", "1");
        }
    }
    Ok(url.into())
}

struct MigrationTarget {
    version: i64,
    name: String,
}

fn read_applied_migrations(connection: &Connection) -> Result<Vec<(i64, String)>> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [],
            |row| row.get(0),
        )
        .context("reading the runtime schema ledger")?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut statement =
        connection.prepare("SELECT version, name FROM schema_migrations ORDER BY version")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the runtime schema ledger")
}

fn legacy_episode_memory_name(version: i64, name: &str) -> bool {
    matches!(version, 81 | 83 | 89 | 91) && name == "episode_memory_results"
}

fn verify_schema(
    connection: &Connection,
    mode: StartupValidationMode,
    db: &mut DbSummary,
) -> std::result::Result<Option<MigrationTarget>, ValidationFailure> {
    let applied = read_applied_migrations(connection).map_err(|_| {
        failure(
            FailureCode::DbNotReadable,
            PhaseName::SchemaVerify,
            "the runtime schema ledger could not be read",
        )
    })?;
    let current = applied.last().map(|(version, _)| *version).unwrap_or(0);
    db.schema_version = Some(current);

    for (version, name) in &applied {
        if *version > db.max_known {
            return Err(failure(
                FailureCode::SchemaNewerThanBinary,
                PhaseName::SchemaVerify,
                format!(
                    "the runtime database schema version {version} is newer than this binary supports ({})",
                    db.max_known
                ),
            )
            .with_persistence(PersistenceCertainty::Unchanged));
        }
        match MIGRATIONS
            .iter()
            .find(|migration| migration.version == *version)
        {
            Some(migration) if migration.name == name => {}
            Some(_) if legacy_episode_memory_name(*version, name) => {}
            Some(migration) => {
                return Err(failure(
                    FailureCode::MigrationNameMismatch,
                    PhaseName::SchemaVerify,
                    format!(
                        "the runtime database migration {version} has an unfamiliar name; refusing to validate a foreign or hand-edited ledger"
                    ),
                )
                .with_migration(&MigrationTarget {
                    version: migration.version,
                    name: migration.name.to_string(),
                })
                .with_persistence(PersistenceCertainty::Unchanged));
            }
            None => {
                return Err(failure(
                    FailureCode::MigrationNameMismatch,
                    PhaseName::SchemaVerify,
                    format!(
                        "the runtime database reports unknown migration version {version}; refusing to validate a foreign or hand-edited ledger"
                    ),
                )
                .with_persistence(PersistenceCertainty::Unchanged));
            }
        }
    }

    if current < db.max_known {
        let target = current_schema_target(db);
        if mode == StartupValidationMode::ReadOnly {
            return Err(failure(
                FailureCode::MigrationRequired,
                PhaseName::SchemaVerify,
                format!(
                    "the runtime database schema is at version {current} but this binary expects {}; re-run with --migrate on an explicit copy",
                    db.max_known
                ),
            )
            .with_migration(&target)
            .with_persistence(PersistenceCertainty::Unchanged));
        }
        return Ok(Some(target));
    }
    Ok(None)
}

/// The migration target representing the schema this binary expects.
fn current_schema_target(db: &DbSummary) -> MigrationTarget {
    MigrationTarget {
        version: db.max_known,
        name: MIGRATIONS
            .iter()
            .find(|migration| migration.version == db.max_known)
            .map(|migration| migration.name.to_string())
            .unwrap_or_default(),
    }
}

fn verify_migrated_schema(
    connection: &Connection,
    target: &MigrationTarget,
    db: &mut DbSummary,
) -> std::result::Result<(), ValidationFailure> {
    let applied = read_applied_migrations(connection).map_err(|_| {
        failure(
            FailureCode::MigrationFailed,
            PhaseName::Migrate,
            "the migrated runtime schema ledger could not be read back",
        )
        .with_migration(target)
    })?;
    let current = applied.last().map(|(version, _)| *version).unwrap_or(0);
    db.schema_version = Some(current);
    if current != db.max_known {
        return Err(failure(
            FailureCode::MigrationFailed,
            PhaseName::Migrate,
            format!(
                "the runtime database schema is at version {current} after migration but this binary expects {}",
                db.max_known
            ),
        )
        .with_migration(target)
        .with_persistence(PersistenceCertainty::PartiallyChanged));
    }
    Ok(())
}

fn migration_failure(
    error: &anyhow::Error,
    path: &Path,
    before: Option<i64>,
    target: &MigrationTarget,
    mode: StartupValidationMode,
) -> ValidationFailure {
    let after = read_schema_version_best_effort(path, mode);
    let certainty = match (before, after) {
        (_, None) => PersistenceCertainty::Unknown,
        (Some(before), Some(after)) if after != before => PersistenceCertainty::PartiallyChanged,
        _ => PersistenceCertainty::Unchanged,
    };
    let mut primary = failure(
        FailureCode::MigrationFailed,
        PhaseName::Migrate,
        "runtime database migration failed; inspect the migration metadata and daemon logs before retrying",
    )
    .with_migration(target)
    .with_persistence(certainty);
    if is_backfill_resource_exhaustion(error) {
        primary.code = FailureCode::BackfillResourceExhausted;
        primary.operator_message = bounded_message(
            "runtime database migration backfill exhausted a bounded resource; the schema may be incomplete, so inspect migration metadata before retrying",
        );
        primary.retryable = true;
    }
    primary
}

/// Recognize the bounded-resource backfill failures (#4429 class) by their
/// stable internal markers only; the raw chain never reaches operator output.
fn is_backfill_resource_exhaustion(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string();
        text.contains("ResourceExhausted") || text.contains("resource exhausted")
    })
}

fn read_schema_version_best_effort(path: &Path, mode: StartupValidationMode) -> Option<i64> {
    let connection = open_for_inspection(path, mode).ok()?;
    let applied = read_applied_migrations(&connection).ok()?;
    Some(applied.last().map(|(version, _)| *version).unwrap_or(0))
}

/// Verifies the storage domain registry. Returns the domains that startup will
/// insert on its own; an incomplete existing row is a failure.
fn check_storage_domains(
    connection: &Connection,
) -> std::result::Result<Vec<String>, ValidationFailure> {
    const REQUIRED_TABLES: [&str; 5] = [
        "schema_migrations",
        "runtime_metadata",
        "agent_identities",
        "storage_domains",
        "input_evidence_current",
    ];
    let mut missing = Vec::new();
    for table in REQUIRED_TABLES {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(|_| {
                failure(
                    FailureCode::DbNotReadable,
                    PhaseName::StorageDomains,
                    "the runtime database layout could not be inspected",
                )
            })?;
        if !exists {
            missing.push(format!("table {table}"));
        }
    }
    if !missing.is_empty() {
        return Err(storage_domain_failure(&missing));
    }

    let mut unregistered = Vec::new();
    let mut incomplete = Vec::new();
    for expected in RuntimeDb::expected_storage_domains() {
        if unregistered.len() + incomplete.len() >= MAX_DOMAIN_NAMES {
            break;
        }
        match holon_storage::storage_domain::read_storage_domain_connection(
            connection,
            expected.domain,
        ) {
            Ok(Some(snapshot))
                if snapshot.import_status == "complete"
                    && snapshot.canonical_source == expected.canonical_source => {}
            Ok(Some(snapshot)) => incomplete.push(format!(
                "domain {} is {} with canonical_source={}",
                snapshot.domain, snapshot.import_status, snapshot.canonical_source
            )),
            Ok(None) => unregistered.push(expected.domain.to_string()),
            Err(_) => {
                return Err(failure(
                    FailureCode::DbNotReadable,
                    PhaseName::StorageDomains,
                    "the runtime storage domain registry could not be read",
                ))
            }
        }
    }
    if !incomplete.is_empty() {
        return Err(storage_domain_failure(&incomplete));
    }
    Ok(unregistered)
}

fn storage_domain_failure(problems: &[String]) -> ValidationFailure {
    let shown = problems
        .iter()
        .take(MAX_DOMAIN_NAMES)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    failure(
        FailureCode::StorageDomainMissing,
        PhaseName::StorageDomains,
        format!("the runtime database has incomplete storage domain rows: {shown}"),
    )
}

fn inspect_agents(
    connection: &Connection,
    config: &AppConfig,
    agents: &mut Option<AgentsSummary>,
    warnings: &mut Vec<ValidationWarning>,
) -> std::result::Result<(), ValidationFailure> {
    let identities =
        holon_storage::agent_registry::agent_identities_on(connection).map_err(|_| {
            failure(
                FailureCode::DbNotReadable,
                PhaseName::AgentRegistry,
                "the runtime agent registry could not be read",
            )
        })?;
    let default_agent_id = &config.default_agent_id;
    let default_resolution = match identities
        .iter()
        .find(|identity| &identity.agent_id == default_agent_id)
    {
        Some(identity) if identity.status == holon_domain::AgentRegistryStatus::Active => {
            "resolved"
        }
        Some(_) => "inactive",
        None => {
            warnings.push(ValidationWarning {
                code: WarningCode::NoActiveDefaultAgent,
                phase: PhaseName::AgentRegistry,
                message: bounded_message(
                    "the configured default agent has no durable registry identity in this library",
                ),
            });
            "missing"
        }
    };
    let mut summaries = identities
        .iter()
        .map(|identity| AgentSummary {
            agent_id: identity.agent_id.clone(),
            status: match identity.status {
                holon_domain::AgentRegistryStatus::Active => "active",
                holon_domain::AgentRegistryStatus::Deleting => "deleting",
                holon_domain::AgentRegistryStatus::Deleted => "deleted",
            }
            .to_string(),
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    summaries.truncate(MAX_AGENT_SUMMARIES);
    *agents = Some(AgentsSummary {
        total: identities.len(),
        default_resolution: default_resolution.to_string(),
        summaries,
    });
    Ok(())
}

fn inspect_quarantine(
    config: &AppConfig,
    quarantine: &mut Option<QuarantineSummary>,
    warnings: &mut Vec<ValidationWarning>,
) -> std::result::Result<(), ValidationFailure> {
    let report = crate::daemon::read_quarantine_report(config);
    let summary = match report {
        crate::daemon::QuarantineReport::Available { .. } => QuarantineSummary {
            status: "available".to_string(),
            reason: None,
        },
        crate::daemon::QuarantineReport::Unavailable { reason } => {
            warnings.push(ValidationWarning {
                code: WarningCode::QuarantineUnavailable,
                phase: PhaseName::Quarantine,
                message: bounded_message(
                    "no readable startup quarantine report is available; this is not a startup failure by itself",
                ),
            });
            QuarantineSummary {
                status: "unavailable".to_string(),
                reason: Some(
                    match reason {
                        crate::daemon::QuarantineReportUnavailable::Missing => "missing",
                        crate::daemon::QuarantineReportUnavailable::ReadFailed => "read_failed",
                        crate::daemon::QuarantineReportUnavailable::NotPrivateRegularFile => {
                            "not_private_regular_file"
                        }
                        crate::daemon::QuarantineReportUnavailable::Oversized => "oversized",
                        crate::daemon::QuarantineReportUnavailable::Invalid => "invalid",
                    }
                    .to_string(),
                ),
            }
        }
    };
    *quarantine = Some(summary);
    Ok(())
}

fn count_rows(connection: &Connection, table: &str) -> Result<u64> {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|count| count.max(0) as u64)
        .with_context(|| format!("counting rows in {table}"))
}

fn inspect_eligibility(
    connection: &Connection,
    mode: StartupValidationMode,
    eligibility: &mut Option<EligibilitySummary>,
) -> std::result::Result<(), ValidationFailure> {
    let read = |table: &str| {
        count_rows(connection, table).map_err(|_| {
            failure(
                FailureCode::DbNotReadable,
                PhaseName::Eligibility,
                "the persisted input-evidence snapshot metadata could not be read",
            )
        })
    };
    let captures = read("input_evidence_captures")?;
    let current_records = read("input_evidence_current")?;
    let eligibility_records = read("input_evidence_eligibility")?;
    let snapshot = if captures == 0 {
        if current_records == 0 {
            "snapshot_missing"
        } else {
            // Records without capture metadata mean the persisted snapshot and
            // its capture bookkeeping disagree; report it without recomputing
            // eligibility.
            "stale"
        }
    } else {
        "snapshot_present"
    };
    let agents = if mode == StartupValidationMode::Migrate && current_records > 0 {
        let mut statement = connection
            .prepare(
                "SELECT agent_id, COUNT(*) FROM input_evidence_current GROUP BY agent_id ORDER BY agent_id LIMIT ?1",
            )
            .map_err(|_| {
                failure(
                    FailureCode::DbNotReadable,
                    PhaseName::Eligibility,
                    "the persisted input-evidence per-agent attribution could not be read",
                )
            })?;
        let rows = statement
            .query_map([MAX_AGENT_SUMMARIES as i64], |row| {
                Ok(EligibilityAgentSummary {
                    agent_id: row.get(0)?,
                    current_records: row.get::<_, i64>(1)?.max(0) as u64,
                })
            })
            .map_err(|_| {
                failure(
                    FailureCode::DbNotReadable,
                    PhaseName::Eligibility,
                    "the persisted input-evidence per-agent attribution could not be read",
                )
            })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| {
            failure(
                FailureCode::DbNotReadable,
                PhaseName::Eligibility,
                "the persisted input-evidence per-agent attribution could not be read",
            )
        })?
    } else {
        Vec::new()
    };
    *eligibility = Some(EligibilitySummary {
        snapshot: snapshot.to_string(),
        captures,
        current_records,
        eligibility_records,
        agents,
    });
    Ok(())
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use super::*;

    fn snapshot_dir(dir: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    metadata.len(),
                    metadata.modified().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    #[test]
    fn read_only_open_never_creates_sidecars_or_touches_mtimes() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home.path().join("state/runtime.sqlite");
        let lock_path = home.path().join("state/runtime.lock");
        drop(RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap());

        let before = snapshot_dir(home.path());
        let connection = open_for_inspection(&db_path, StartupValidationMode::ReadOnly).unwrap();
        let _: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        let after = snapshot_dir(home.path());
        assert_eq!(
            before, after,
            "read-only inspection must not write the data directory"
        );
    }

    #[test]
    fn pending_wal_is_inspected_through_a_zero_write_copy() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home.path().join("state/runtime.sqlite");
        let lock_path = home.path().join("state/runtime.lock");
        drop(RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap());

        // Hold a live writer so the WAL keeps uncheckpointed frames: closing
        // every connection can truncate `-wal` to zero bytes even though the
        // file itself survives, which would silently skip the copy path.
        let writer = Connection::open(&db_path).unwrap();
        writer
            .execute(
                "INSERT OR REPLACE INTO runtime_metadata (key, value, created_at, updated_at)
                 VALUES ('validate_startup_wal_fixture', 'pending', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
        let wal = sidecar_path(&db_path, "-wal");
        assert!(
            std::fs::metadata(&wal).is_ok_and(|metadata| metadata.len() > 0),
            "the fixture must leave pending WAL frames"
        );

        let state_dir = home.path().join("state");
        let before = snapshot_dir(&state_dir);
        let expected = max_known_migration_version();
        let inspection = open_for_inspection(&db_path, StartupValidationMode::ReadOnly).unwrap();
        assert!(
            inspection._copy.is_some(),
            "pending WAL frames must be read through the zero-write copy"
        );
        let version: i64 = inspection
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(inspection);
        // Snapshot while the writer still holds the WAL; closing the last
        // connection checkpoints and removes the sidecars.
        let after = snapshot_dir(&state_dir);
        assert_eq!(version, expected);
        assert_eq!(
            before, after,
            "WAL inspection must not write the source data directory"
        );
        drop(writer);
    }

    #[test]
    fn resource_exhaustion_fixture_reports_backfill_failure_without_schema_advance() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home.path().join("state/runtime.sqlite");
        let lock_path = home.path().join("state/runtime.lock");
        drop(RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap());
        let before =
            read_schema_version_best_effort(&db_path, StartupValidationMode::Migrate).unwrap();
        let target = MigrationTarget {
            version: before,
            name: "fixture_migration".to_string(),
        };
        // The post-#4433 shape: a bounded backfill resource limit aborts the
        // upgrade. The raw chain must never reach the operator message.
        let error = anyhow::anyhow!(
            "runtime database migration failed: input evidence resource exhausted: reference depth"
        );
        let failure = migration_failure(
            &error,
            &db_path,
            Some(before),
            &target,
            StartupValidationMode::Migrate,
        );
        assert_eq!(failure.code, FailureCode::BackfillResourceExhausted);
        assert_eq!(failure.phase, PhaseName::Migrate);
        assert_eq!(failure.migration_version, Some(before));
        assert_eq!(failure.migration_name.as_deref(), Some("fixture_migration"));
        assert_eq!(
            failure.persistence_certainty,
            Some(PersistenceCertainty::Unchanged)
        );
        assert!(failure.retryable);
        assert!(failure.operator_message.chars().count() <= MAX_OPERATOR_MESSAGE_CHARS);
        assert!(!failure.operator_message.contains("reference depth"));
        assert!(!failure.operator_message.contains("ResourceExhausted"));
        assert_eq!(
            read_schema_version_best_effort(&db_path, StartupValidationMode::Migrate),
            Some(before),
            "a failed backfill must not advance the persisted schema version"
        );
    }

    #[test]
    fn operator_messages_are_bounded_and_collapsed() {
        let message = "a".repeat(MAX_OPERATOR_MESSAGE_CHARS * 2);
        let bounded = bounded_message(&format!("  {message}\n\n  tail  "));
        assert_eq!(bounded.chars().count(), MAX_OPERATOR_MESSAGE_CHARS);
        assert_eq!(bounded_message("  two   words  "), "two words");
    }

    #[test]
    fn legacy_episode_memory_names_are_not_reported_as_mismatch() {
        assert!(legacy_episode_memory_name(81, "episode_memory_results"));
        assert!(legacy_episode_memory_name(91, "episode_memory_results"));
        assert!(!legacy_episode_memory_name(92, "episode_memory_results"));
        assert!(!legacy_episode_memory_name(81, "another_name"));
    }
}
