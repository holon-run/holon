//! Shared output contract for the `holon daemon` command family.
//!
//! `holon daemon --json <command>` writes exactly one envelope object to
//! stdout; `holon daemon <command>` renders bounded, secret-safe human text.
//! Envelope shape, error classification, redaction, and the exit-code mapping
//! live here so every daemon subcommand shares one contract. See
//! `docs/implementation-decisions/` for the recorded decision.

use std::{ffi::OsStr, fmt, io::Write};

use anyhow::Error;
use serde::Serialize;
use serde_json::{Map, Value};

/// Envelope version written as `schema_version`.
pub const SCHEMA_VERSION: u32 = 1;

/// Exit-code family shared by the JSON and human daemon renderers.
pub mod exit_codes {
    pub const OK: u8 = 0;
    pub const USAGE_OR_INTERNAL: u8 = 1;
    pub const DOMAIN_FAILURE: u8 = 20;
    pub const BLOCKED: u8 = 21;
    /// Reserved for `daemon validate-startup` reporting `migration_required`.
    pub const MIGRATION_REQUIRED: u8 = 22;
}

/// Upper bound for any operator-facing message rendered by this contract.
pub const MAX_MESSAGE_CHARS: usize = 400;

const REDACTED: &str = "<redacted>";
const MAX_LOG_LINE_CHARS: usize = 2_000;
const MAX_LOG_LINES: usize = 200;

const SENSITIVE_FLAGS: &[&str] = &[
    "--token",
    "--token-file",
    "--control-token",
    "--api-key",
    "--password",
    "--secret",
    "--client-secret",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonStatus {
    Ok,
    Failed,
    Blocked,
}

impl DaemonStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonErrorKind {
    Usage,
    Domain,
    DaemonUnavailable,
    Precondition,
    Internal,
}

/// Machine-readable command failure carried by the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonError {
    pub kind: DaemonErrorKind,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// Reserved for an explicit, typed, secret-safe allowlist. This contract
    /// does not publish any details today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
}

impl DaemonError {
    pub fn new(
        kind: DaemonErrorKind,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        let message = message.into();
        Self {
            kind,
            code: code.into(),
            message: sanitize_message(&message),
            retryable,
            details: None,
        }
    }
}

/// One daemon command result in the shared envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonEnvelope {
    pub schema_version: u32,
    pub command: String,
    pub status: DaemonStatus,
    pub data: Option<Value>,
    pub error: Option<DaemonError>,
}

impl DaemonEnvelope {
    pub fn ok(command: &str, data: Value) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_string(),
            status: DaemonStatus::Ok,
            data: Some(data),
            error: None,
        }
    }

    pub fn blocked(command: &str, error: DaemonError) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_string(),
            status: DaemonStatus::Blocked,
            data: None,
            error: Some(error),
        }
    }

    /// A blocked outcome that still carries the command payload.
    ///
    /// `daemon status` uses this so machine consumers can read the lifecycle
    /// view even when the CLI reports a blocked exit code.
    pub fn blocked_with_data(command: &str, data: Value, error: DaemonError) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_string(),
            status: DaemonStatus::Blocked,
            data: Some(data),
            error: Some(error),
        }
    }

    /// A command whose own report is the payload even when it did not pass.
    ///
    /// `daemon validate-startup` keeps its phases, database/agent/quarantine
    /// summaries, warnings, and failures in `data`; `error` carries the
    /// terminal machine code so the shared exit-code mapping still applies.
    pub fn report(
        command: &str,
        status: DaemonStatus,
        data: Value,
        error: Option<DaemonError>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_string(),
            status,
            data: Some(data),
            error,
        }
    }

    /// Usage failures happen before or outside the command body, so they carry
    /// their own kind instead of going through [`classify_error`].
    pub fn usage(command: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_string(),
            status: DaemonStatus::Failed,
            data: None,
            error: Some(DaemonError::new(
                DaemonErrorKind::Usage,
                code,
                message,
                false,
            )),
        }
    }

    pub fn from_anyhow(command: &str, error: &Error) -> Self {
        let classified = classify_error(error);
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_string(),
            status: classified.status,
            data: None,
            error: Some(classified.error),
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self.status {
            DaemonStatus::Ok => exit_codes::OK,
            DaemonStatus::Blocked => exit_codes::BLOCKED,
            DaemonStatus::Failed => match self.error.as_ref().map(|error| error.kind) {
                Some(DaemonErrorKind::Domain) => {
                    // `migration_required` is the one domain outcome that is not
                    // a validation failure: read-only mode could not proceed.
                    if self
                        .error
                        .as_ref()
                        .is_some_and(|error| error.code == "migration_required")
                    {
                        exit_codes::MIGRATION_REQUIRED
                    } else {
                        exit_codes::DOMAIN_FAILURE
                    }
                }
                Some(DaemonErrorKind::DaemonUnavailable) | Some(DaemonErrorKind::Precondition) => {
                    exit_codes::BLOCKED
                }
                _ => exit_codes::USAGE_OR_INTERNAL,
            },
        }
    }
}

/// Process exit status for a daemon command that already rendered its own
/// outcome. `main` must not print the generic `Error:` line for these.
#[derive(Debug)]
pub struct DaemonExitStatus(pub u8);

impl DaemonExitStatus {
    pub fn exit_code(&self) -> u8 {
        self.0
    }
}

impl fmt::Display for DaemonExitStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("daemon command already rendered its outcome")
    }
}

impl std::error::Error for DaemonExitStatus {}

/// Outcome of one classified daemon command failure.
#[derive(Debug, Clone)]
pub struct ClassifiedError {
    pub status: DaemonStatus,
    pub error: DaemonError,
}

struct Rule {
    prefix: &'static str,
    status: DaemonStatus,
    kind: DaemonErrorKind,
    code: &'static str,
    retryable: bool,
    message: &'static str,
}

const UNAVAILABLE_MESSAGE: &str =
    "the runtime is running but its control surface is unresponsive; inspect `holon daemon logs`";

/// Ordered stable-prefix table mapping daemon lifecycle failures onto stable
/// machine codes. Messages are curated so no interpolated detail, path, or
/// credential can leak. The contract test pins these prefixes against drift in
/// the daemon crate's error text.
const RULES: &[Rule] = &[
    Rule {
        prefix: "runtime is already running",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_running",
        retryable: false,
        message: "the runtime is already running; inspect it with `holon daemon status` or restart it explicitly",
    },
    Rule {
        prefix: "failed to acquire daemon lifecycle lock",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_lease_held",
        retryable: true,
        message: "another holon process is holding the daemon lifecycle lock; retry shortly",
    },
    Rule {
        prefix: "daemon lifecycle lock task failed",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_lease_held",
        retryable: true,
        message: "another holon process is holding the daemon lifecycle lock; retry shortly",
    },
    Rule {
        prefix: "runtime is not running",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_not_running",
        retryable: true,
        message: "the daemon is not running; start it with `holon daemon start`",
    },
    Rule {
        prefix: "control socket is occupied by a non-Holon process",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_socket_occupied",
        retryable: false,
        message: "the control socket is occupied by a non-Holon process; inspect the socket path before clearing it",
    },
    Rule {
        prefix: "cannot stop runtime safely",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_unresponsive",
        retryable: true,
        message: "the runtime is running but its control surface is unresponsive; inspect `holon daemon logs` before stopping it",
    },
    Rule {
        prefix: "cannot stop runtime: the control socket exists",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "the control socket cannot be probed; the runtime is incompatible or unresponsive, so stop it explicitly after confirming its identity",
    },
    Rule {
        prefix: "runtime is unresponsive during readiness probe",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_unresponsive",
        retryable: true,
        message: UNAVAILABLE_MESSAGE,
    },
    Rule {
        prefix: "runtime is unresponsive during daemon serve preflight",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_unresponsive",
        retryable: true,
        message: "the runtime is running but its control surface is unresponsive; inspect `holon daemon logs` before starting a new runtime",
    },
    Rule {
        prefix: "runtime process is alive, but the control surface is unavailable",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_unresponsive",
        retryable: true,
        message: "the runtime process is alive but its control surface is unavailable; inspect `holon daemon logs`",
    },
    Rule {
        prefix: "unix readiness probe timed out",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_unresponsive",
        retryable: true,
        message: "the readiness probe timed out; inspect `holon daemon status` and `holon daemon logs`",
    },
    Rule {
        prefix: "runtime belongs to a different home or control socket",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_foreign_runtime",
        retryable: false,
        message: "the running runtime belongs to a different Holon home or control socket; run the command from the owning Holon home",
    },
    Rule {
        prefix: "runtime identity does not match daemon metadata",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_identity_mismatch",
        retryable: false,
        message: "the running runtime identity does not match the recorded daemon metadata; inspect `holon daemon status`",
    },
    Rule {
        prefix: "refusing serve options from daemon metadata with a different identity",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_identity_mismatch",
        retryable: false,
        message: "the recorded serve options belong to a different runtime identity; reset them explicitly with `--reset-serve-options`",
    },
    Rule {
        prefix: "control protocol version",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "the running runtime uses an incompatible control protocol version; restart it with this holon binary",
    },
    Rule {
        prefix: "runtime is running but incompatible with the daemon lifecycle contract",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "the running runtime is incompatible with the daemon lifecycle contract; stop or restart it explicitly",
    },
    Rule {
        prefix: "cannot accept serve intent for an occupied, foreign, incompatible or unresponsive control surface",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "the control surface is occupied by an incompatible or unresponsive runtime; inspect `holon daemon status`",
    },
    Rule {
        prefix: "cannot replace incompatible runtime",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "an incompatible runtime cannot be replaced automatically; stop it explicitly and start again",
    },
    Rule {
        prefix: "refusing to replace incompatible runtime",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "an incompatible runtime cannot be replaced automatically; stop it explicitly and start again",
    },
    Rule {
        prefix: "safe incompatible-runtime replacement is not supported",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "safe incompatible-runtime replacement is not supported on this platform; stop the runtime explicitly first",
    },
    Rule {
        prefix: "runtime became incompatible during startup stabilization",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_contract_incompatible",
        retryable: false,
        message: "the runtime became incompatible during startup; inspect `holon daemon logs` before retrying",
    },
    Rule {
        prefix: "runtime became unresponsive during startup stabilization",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_start_failed",
        retryable: true,
        message: "the runtime became unresponsive during startup; inspect `holon daemon logs` and retry",
    },
    Rule {
        prefix: "runtime became unreachable during startup stabilization",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_start_failed",
        retryable: true,
        message: "the runtime became unreachable during startup; inspect `holon daemon logs` and retry",
    },
    Rule {
        prefix: "runtime config fingerprint changed during startup stabilization",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_start_failed",
        retryable: true,
        message: "the runtime reported a different effective config during startup; inspect `holon daemon status` and retry",
    },
    Rule {
        prefix: "runtime pid changed during startup stabilization",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_start_failed",
        retryable: true,
        message: "the runtime process changed during startup; inspect `holon daemon status` and retry",
    },
    Rule {
        prefix: "daemon start failed",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_start_failed",
        retryable: true,
        message: "the daemon failed to start; inspect `holon daemon logs --tail 40` and retry",
    },
    Rule {
        prefix: "daemon failed to start",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_start_failed",
        retryable: true,
        message: "the daemon failed to start; inspect `holon daemon logs --tail 40` and retry",
    },
    Rule {
        prefix: "serve exited with status",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_start_failed",
        retryable: true,
        message: "the daemon serve process exited during startup; inspect `holon daemon logs --tail 40`",
    },
    Rule {
        prefix: "timed out waiting for healthy runtime",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_start_failed",
        retryable: true,
        message: "the runtime did not become healthy in time; inspect `holon daemon logs --tail 40` and retry",
    },
    Rule {
        prefix: "timed out waiting for runtime shutdown",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_stop_timeout",
        retryable: true,
        message: "the runtime did not shut down in time; inspect `holon daemon status` before retrying `holon daemon stop`",
    },
    Rule {
        prefix: "timed out waiting for Holon runtime PID",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::DaemonUnavailable,
        code: "daemon_stop_timeout",
        retryable: true,
        message: "the runtime process did not exit in time; inspect `holon daemon status` before retrying",
    },
    Rule {
        prefix: "cannot stop daemon PID",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_stop_failed",
        retryable: false,
        message: "the runtime process did not exit; its state files were left intact for inspection",
    },
    Rule {
        prefix: "stale daemon state detected",
        status: DaemonStatus::Blocked,
        kind: DaemonErrorKind::Precondition,
        code: "daemon_stale_state",
        retryable: false,
        message: "stale daemon state was detected; inspect `holon daemon status` before clearing it",
    },
    Rule {
        prefix: "daemon start timeout must be between",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Usage,
        code: "invalid_start_timeout",
        retryable: false,
        message: "the daemon start timeout must be between 1 and 86400 seconds",
    },
    Rule {
        prefix: "use only one of --listen or --port",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Usage,
        code: "invalid_serve_options",
        retryable: false,
        message: "use only one of --listen or --port",
    },
    Rule {
        prefix: "use only one of --token or --token-file",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Usage,
        code: "invalid_serve_options",
        retryable: false,
        message: "use only one of --token or --token-file",
    },
    Rule {
        prefix: "--web-dist must point to a directory",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Usage,
        code: "invalid_serve_options",
        retryable: false,
        message: "--web-dist must point to a directory",
    },
    Rule {
        prefix: "advertise URL must not contain credentials",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Usage,
        code: "invalid_serve_options",
        retryable: false,
        message: "the advertise URL must not contain credentials",
    },
    Rule {
        prefix: "desktop integration requires macOS",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Usage,
        code: "unsupported_platform",
        retryable: false,
        message: "desktop integration requires macOS",
    },
    Rule {
        prefix: "saved daemon authentication requires",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Usage,
        code: "missing_daemon_authentication",
        retryable: false,
        message: "saved daemon authentication requires --token, --token-file, or a control token; use --reset-serve-options to reset all serve intent",
    },
    Rule {
        prefix: "legacy daemon metadata must not contain",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Domain,
        code: "invalid_legacy_daemon_metadata",
        retryable: false,
        message: "recorded legacy daemon metadata is not accepted; reset it explicitly with --reset-serve-options",
    },
    Rule {
        prefix: "invalid legacy daemon serve options",
        status: DaemonStatus::Failed,
        kind: DaemonErrorKind::Domain,
        code: "invalid_legacy_daemon_metadata",
        retryable: false,
        message: "recorded legacy daemon serve options are invalid; reset them explicitly with --reset-serve-options",
    },
];

pub fn classify_error(error: &Error) -> ClassifiedError {
    // `Display` on `anyhow::Error` yields only the outermost message, so raw
    // context chains never reach the envelope.
    let text = error.to_string();
    if let Some(rule) = RULES.iter().find(|rule| text.starts_with(rule.prefix)) {
        return ClassifiedError {
            status: rule.status,
            error: DaemonError::new(rule.kind, rule.code, rule.message, rule.retryable),
        };
    }
    ClassifiedError {
        status: DaemonStatus::Failed,
        error: DaemonError::new(
            DaemonErrorKind::Internal,
            "daemon_internal_error",
            "unexpected internal error while running the daemon command; see stderr diagnostics",
            false,
        ),
    }
}

/// Stable-prefix table surface used by tests that pin daemon error text.
pub fn known_error_codes() -> Vec<(&'static str, &'static str)> {
    RULES.iter().map(|rule| (rule.prefix, rule.code)).collect()
}

/// Clap usage-error code mapping shared by both renderers.
pub fn usage_code(kind: clap::error::ErrorKind) -> &'static str {
    use clap::error::ErrorKind;
    match kind {
        ErrorKind::MissingSubcommand => "missing_command",
        ErrorKind::InvalidSubcommand => "unknown_command",
        ErrorKind::UnknownArgument => "unknown_argument",
        ErrorKind::MissingRequiredArgument => "missing_argument",
        ErrorKind::InvalidValue
        | ErrorKind::ValueValidation
        | ErrorKind::TooManyValues
        | ErrorKind::WrongNumberOfValues
        | ErrorKind::NoEquals => "invalid_value",
        _ => "invalid_arguments",
    }
}

/// Write one command outcome in the selected mode.
pub fn emit(envelope: &DaemonEnvelope, json_mode: bool) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    if json_mode {
        serde_json::to_writer(&mut stdout, envelope)?;
        writeln!(stdout)?;
    } else {
        write_human(envelope, &mut stdout)?;
    }
    stdout.flush()?;
    Ok(())
}

/// Human mode: bounded, secret-safe text. Not a machine contract.
pub fn write_human<W: Write>(envelope: &DaemonEnvelope, out: &mut W) -> std::io::Result<()> {
    writeln!(out, "status: {}", envelope.status.as_str())?;
    if let Some(error) = &envelope.error {
        writeln!(out, "error: {} (code: {})", error.message, error.code)?;
        writeln!(out, "next: {}", next_action(error))?;
    }
    match (&envelope.data, &envelope.error) {
        (Some(data), error) => {
            for line in human_detail_lines(&envelope.command, data) {
                writeln!(out, "{line}")?;
            }
            if let Some(error) = error {
                writeln!(out, "error: {} (code: {})", error.message, error.code)?;
                writeln!(out, "next: {}", next_action(error))?;
            }
        }
        (None, _) => {}
    }
    Ok(())
}

fn next_action(error: &DaemonError) -> &'static str {
    match error.code.as_str() {
        "daemon_not_running" => "start it with `holon daemon start`",
        "daemon_running" => "inspect it with `holon daemon status`, or use `holon daemon restart`",
        "daemon_lease_held" => "retry once the other holon process finishes",
        "daemon_unresponsive" => {
            "inspect `holon daemon logs`, then confirm the runtime with `holon daemon status`"
        }
        "daemon_contract_incompatible" => {
            "stop the incompatible runtime explicitly, then start again"
        }
        "daemon_start_failed" => "check `holon daemon logs --tail 40` for the startup failure",
        "daemon_stop_timeout" | "daemon_stop_failed" => {
            "confirm the runtime state with `holon daemon status` before retrying"
        }
        "daemon_socket_occupied" => "inspect the control socket path before clearing it",
        "daemon_foreign_runtime" | "daemon_identity_mismatch" => {
            "run the command from the Holon home that owns the running runtime"
        }
        "daemon_stale_state" => "inspect `holon daemon status` before clearing stale state",
        "daemon_internal_error" => "check stderr diagnostics and `holon daemon logs`",
        "invalid_start_timeout"
        | "invalid_serve_options"
        | "unsupported_platform"
        | "missing_daemon_authentication"
        | "invalid_legacy_daemon_metadata" => "run `holon daemon --help` for the accepted options",
        "missing_command" | "unknown_command" | "unknown_argument" | "invalid_value"
        | "missing_argument" | "invalid_arguments" => {
            "run `holon daemon --help` for the accepted syntax"
        }
        "db_missing" => "point HOLON_HOME at the right library, or start holon once to create the runtime database",
        "db_not_readable" => "stop the daemon cleanly so the WAL is checkpointed, then re-run validation",
        "migration_required" => "re-run with `--migrate` against an explicit copy or maintenance window",
        "migration_failed" | "backfill_resource_exhausted" => "inspect the migration metadata and logs; do not retry against live state",
        "schema_newer_than_binary" => "use a binary at least as new as this library",
        "migration_name_mismatch" => "verify the library provenance; do not migrate a foreign or hand-edited database",
        "storage_domain_missing" => "restore a complete runtime library snapshot",
        "config_invalid" | "paths_unresolved" => "check HOLON_HOME, the data directory, and the control socket configuration",
        _ => "run `holon daemon status` for the current state",
    }
}

fn human_detail_lines(command: &str, data: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    match command {
        "daemon.validate-startup" => {
            push_scalar(&mut lines, data, "mode");
            if let Some(db) = data.get("db") {
                let max_known = db.get("max_known").and_then(Value::as_i64).unwrap_or(0);
                match db.get("schema_version").and_then(Value::as_i64) {
                    Some(version) => {
                        lines.push(format!("schema: {version} (max known {max_known})"))
                    }
                    None => lines.push(format!("schema: unavailable (max known {max_known})")),
                }
            }
            if let Some(agents) = data.get("agents") {
                let total = agents.get("total").and_then(Value::as_u64).unwrap_or(0);
                let resolution = agents
                    .get("default_resolution")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                lines.push(format!("agents: {total} (default {resolution})"));
            }
            for (label, section, key) in [
                ("quarantine", "quarantine", "status"),
                ("eligibility snapshot", "eligibility", "snapshot"),
            ] {
                if let Some(value) = data
                    .get(section)
                    .and_then(|value| value.get(key))
                    .and_then(Value::as_str)
                {
                    lines.push(format!("{label}: {value}"));
                }
            }
            if let Some(phases) = data.get("phases").and_then(Value::as_array) {
                let count = |status: &str| {
                    phases
                        .iter()
                        .filter(|phase| phase.get("status").and_then(Value::as_str) == Some(status))
                        .count()
                };
                lines.push(format!(
                    "phases: {} ok, {} failed, {} skipped",
                    count("ok"),
                    count("failed"),
                    count("skipped")
                ));
            }
            if let Some(warnings) = data.get("warnings").and_then(Value::as_array) {
                for code in warnings
                    .iter()
                    .filter_map(|warning| warning.get("code").and_then(Value::as_str))
                {
                    lines.push(format!("warning: {code}"));
                }
            }
        }
        "daemon.status" => {
            for key in ["state", "healthy", "pid", "http_addr", "web_url", "message"] {
                push_scalar(&mut lines, data, key);
            }
        }
        "daemon.start" | "daemon.restart" | "daemon.stop" | "daemon.prepare-update" => {
            for key in ["action", "process_created", "state", "pid", "http_addr"] {
                push_scalar(&mut lines, data, key);
            }
            push_scalar(&mut lines, data, "message");
        }
        "daemon.quarantine" => {
            push_scalar(&mut lines, data, "status");
            push_scalar(&mut lines, data, "reason");
        }
        "daemon.logs" => {
            push_scalar(&mut lines, data, "log_path");
            push_scalar(&mut lines, data, "message");
            if let Some(tail) = data.get("tail").and_then(Value::as_array) {
                if !tail.is_empty() {
                    lines.push("tail:".to_string());
                }
                for entry in tail.iter().filter_map(Value::as_str).take(MAX_LOG_LINES) {
                    lines.push(format!("  {}", truncate_chars(entry, MAX_LOG_LINE_CHARS)));
                }
            }
        }
        _ => push_scalar(&mut lines, data, "message"),
    }
    lines
}

fn push_scalar(lines: &mut Vec<String>, data: &Value, key: &str) {
    match data.get(key) {
        Some(Value::String(text)) => lines.push(format!("{key}: {text}")),
        Some(Value::Number(number)) => lines.push(format!("{key}: {number}")),
        Some(Value::Bool(value)) => lines.push(format!("{key}: {value}")),
        _ => {}
    }
}

/// Invocation facts recovered from `argv` before clap parses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInvocation {
    pub command_path: String,
    pub json_mode: bool,
}

/// Recover daemon output mode and command path from raw arguments.
///
/// Used only for parse/usage failures, where clap has not produced a typed
/// command yet. Requires `daemon` to be the first non-flag token.
pub fn probe_daemon_invocation<I, S>(args: I) -> Option<DaemonInvocation>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut tokens: Vec<String> = Vec::new();
    for (index, arg) in args.into_iter().enumerate() {
        if index == 0 {
            continue;
        }
        let token = arg.as_ref().to_string_lossy().into_owned();
        if token == "--" {
            break;
        }
        tokens.push(token);
    }
    let first_non_flag = tokens.iter().find(|token| !token.starts_with('-'))?;
    if first_non_flag != "daemon" {
        return None;
    }
    let daemon_index = tokens.iter().position(|token| token == "daemon")?;
    let json_mode = tokens.iter().any(|token| token == "--json");
    let mut command_path = String::from("daemon");
    if let Some(subcommand) = tokens[daemon_index + 1..]
        .iter()
        .find(|token| !token.starts_with('-'))
    {
        command_path.push('.');
        command_path.push_str(subcommand);
    }
    Some(DaemonInvocation {
        command_path,
        json_mode,
    })
}

/// Collapse whitespace, redact secrets, and bound operator-facing text.
pub fn sanitize_message(text: &str) -> String {
    let collapsed = collapse_whitespace(text);
    let redacted = redact_secrets(&collapsed);
    truncate_chars(&redacted, MAX_MESSAGE_CHARS)
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    for character in text.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if character == ' ' {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(character);
            last_was_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn redact_secrets(text: &str) -> String {
    let text = redact_urls(text);
    let text = redact_clap_invalid_values(&text, SENSITIVE_FLAGS);
    let text = redact_flag_values(&text, SENSITIVE_FLAGS);
    redact_bearer_tokens(&text)
}

/// Clap renders a rejected value before the flag it belongs to:
/// `invalid value 'secret' for '--token <TOKEN>'`. Redact that leading value
/// when the following flag is sensitive; `redact_flag_values` already covers
/// the `--flag value` and `--flag=value` forms.
fn redact_clap_invalid_values(text: &str, flags: &[&str]) -> String {
    const MARKER: &str = "invalid value '";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(MARKER) {
        let (before, after) = rest.split_at(index + MARKER.len());
        out.push_str(before);
        let Some(value_end) = after.find('\'') else {
            out.push_str(after);
            return out;
        };
        let value = &after[..value_end];
        let tail = &after[value_end..];
        let sensitive = tail
            .strip_prefix("' for '")
            .is_some_and(|flag_tail| flag_tail_matches(flag_tail, flags));
        if sensitive && !value.is_empty() {
            out.push_str(REDACTED);
        } else {
            out.push_str(value);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn flag_tail_matches(tail: &str, flags: &[&str]) -> bool {
    flags.iter().any(|flag| {
        tail.strip_prefix(flag).is_some_and(|remainder| {
            remainder.is_empty() || remainder.starts_with([' ', '<', '=', '\''])
        })
    })
}

fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find("://") {
        let (before, after_scheme) = rest.split_at(index + 3);
        out.push_str(before);
        let end = after_scheme
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | ')' | ']' | ',' | ';')
            })
            .unwrap_or(after_scheme.len());
        let (token, tail) = after_scheme.split_at(end);
        out.push_str(&redact_url_token(token));
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn redact_url_token(token: &str) -> String {
    let mut out = String::new();
    let authority_end = token
        .find(|character| matches!(character, '/' | '?' | '#'))
        .unwrap_or(token.len());
    let (authority, remainder) = token.split_at(authority_end);
    match authority.rfind('@') {
        Some(at) => {
            out.push_str(REDACTED);
            out.push_str(&authority[at..]);
        }
        None => out.push_str(authority),
    }
    if remainder.is_empty() {
        return out;
    }
    let path_end = remainder
        .find(|character| matches!(character, '?' | '#'))
        .unwrap_or(remainder.len());
    out.push_str(&remainder[..path_end]);
    if path_end < remainder.len() {
        out.push_str(REDACTED);
    }
    out
}

fn redact_flag_values(text: &str, flags: &[&str]) -> String {
    let mut out = text.to_string();
    for flag in flags {
        let mut cursor = 0usize;
        while let Some(relative) = out[cursor..].find(flag) {
            let flag_start = cursor + relative;
            let flag_end = flag_start + flag.len();
            let next = out[flag_end..].chars().next();
            match next {
                Some('=') => {
                    let value_start = flag_end + 1;
                    let value_end = value_end_index(&out, value_start);
                    out.replace_range(value_start..value_end, REDACTED);
                    cursor = value_start + REDACTED.len();
                }
                Some(character) if character.is_whitespace() => {
                    let value_start = value_start_index(&out, flag_end);
                    if value_start >= out.len() {
                        break;
                    }
                    let value_end = value_end_index(&out, value_start);
                    out.replace_range(value_start..value_end, REDACTED);
                    cursor = value_start + REDACTED.len();
                }
                _ => cursor = flag_end,
            }
        }
    }
    out
}

fn redact_bearer_tokens(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find("bearer ") {
        let value_start = cursor + relative + "bearer ".len();
        out.push_str(&text[cursor..value_start]);
        let value_end = value_end_index(text, value_start);
        out.push_str(REDACTED);
        cursor = value_end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn value_start_index(text: &str, from: usize) -> usize {
    text[from..]
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map(|(index, _)| from + index)
        .unwrap_or(text.len())
}

fn value_end_index(text: &str, from: usize) -> usize {
    text[from..]
        .char_indices()
        .find(|(_, character)| {
            character.is_whitespace()
                || matches!(character, '"' | '\'' | '`' | ',' | ';' | ')' | ']')
        })
        .map(|(index, _)| from + index)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serializes_contract_fields() {
        let envelope = DaemonEnvelope::ok("daemon.status", serde_json::json!({"state": "running"}));
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["command"], "daemon.status");
        assert_eq!(value["status"], "ok");
        assert_eq!(value["data"]["state"], "running");
        assert!(value["error"].is_null());
        assert_eq!(envelope.exit_code(), exit_codes::OK);
    }

    #[test]
    fn blocked_error_uses_blocked_exit_code() {
        let envelope = DaemonEnvelope::blocked(
            "daemon.status",
            DaemonError::new(
                DaemonErrorKind::DaemonUnavailable,
                "daemon_not_running",
                "the daemon is not running",
                true,
            ),
        );
        assert_eq!(envelope.exit_code(), exit_codes::BLOCKED);
        assert!(envelope.data.is_none());
    }

    #[test]
    fn domain_failure_uses_domain_exit_code() {
        let envelope = DaemonEnvelope {
            schema_version: SCHEMA_VERSION,
            command: "daemon.status".to_string(),
            status: DaemonStatus::Failed,
            data: None,
            error: Some(DaemonError::new(
                DaemonErrorKind::Domain,
                "invalid_legacy_daemon_metadata",
                "invalid",
                false,
            )),
        };
        assert_eq!(envelope.exit_code(), exit_codes::DOMAIN_FAILURE);
    }

    #[test]
    fn known_error_codes_map_unknown_errors_to_internal() {
        let error = anyhow::anyhow!("totally unexpected failure");
        let classified = classify_error(&error);
        assert_eq!(classified.status, DaemonStatus::Failed);
        assert_eq!(classified.error.kind, DaemonErrorKind::Internal);
        assert_eq!(classified.error.code, "daemon_internal_error");
    }

    #[test]
    fn classification_ignores_context_chains() {
        let error = anyhow::anyhow!("runtime is already running").context("failed to start");
        // `Display` shows only the outermost message, so the pinned prefix table
        // matches the outer context, not the inner cause.
        let classified = classify_error(&error);
        assert_eq!(classified.error.code, "daemon_internal_error");
    }

    #[test]
    fn sanitize_collapses_newlines_and_bounds_length() {
        let long = format!("line one\nline two {}", "x".repeat(MAX_MESSAGE_CHARS * 2));
        let sanitized = sanitize_message(&long);
        assert!(!sanitized.contains('\n'));
        assert_eq!(sanitized.chars().count(), MAX_MESSAGE_CHARS);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn sanitize_redacts_url_credentials_and_query() {
        let sanitized = sanitize_message(
            "connect failed for https://user:pass@example.test:8443/path?token=abc123#frag",
        );
        assert!(!sanitized.contains("user:pass"), "{sanitized}");
        assert!(!sanitized.contains("abc123"), "{sanitized}");
        assert!(sanitized.contains("example.test:8443/path"), "{sanitized}");
    }

    #[test]
    fn sanitize_redacts_sensitive_flag_values() {
        let sanitized =
            sanitize_message("invalid value 'sekret' for '--token <TOKEN>' and --password=hunter2");
        assert!(!sanitized.contains("sekret"), "{sanitized}");
        assert!(!sanitized.contains("hunter2"), "{sanitized}");
        assert!(sanitized.contains("--token"), "{sanitized}");
    }

    #[test]
    fn sanitize_redacts_bearer_tokens() {
        let sanitized = sanitize_message("authorization: Bearer abc.def.ghi was rejected");
        assert!(!sanitized.contains("abc.def.ghi"), "{sanitized}");
    }

    #[test]
    fn probe_recovers_daemon_json_invocation() {
        let invocation = probe_daemon_invocation(["holon", "daemon", "--json", "status"]).unwrap();
        assert_eq!(invocation.command_path, "daemon.status");
        assert!(invocation.json_mode);

        let invocation = probe_daemon_invocation(["holon", "daemon", "--json"]).unwrap();
        assert_eq!(invocation.command_path, "daemon");
        assert!(invocation.json_mode);

        let invocation = probe_daemon_invocation(["holon", "daemon", "logs", "--json"]).unwrap();
        assert_eq!(invocation.command_path, "daemon.logs");
        assert!(invocation.json_mode);

        assert_eq!(probe_daemon_invocation(["holon", "status"]), None);
        assert_eq!(probe_daemon_invocation(["holon", "prompt", "daemon"]), None);
    }

    #[test]
    fn human_renderer_starts_with_status_line() {
        let envelope = DaemonEnvelope::ok(
            "daemon.status",
            serde_json::json!({"state": "running", "healthy": true, "pid": 42}),
        );
        let mut rendered = Vec::new();
        write_human(&envelope, &mut rendered).unwrap();
        let text = String::from_utf8(rendered).unwrap();
        assert_eq!(
            text.lines().next().unwrap(),
            "status: ok",
            "unexpected human output: {text}"
        );
        assert!(text.contains("state: running"), "{text}");
        assert!(text.contains("pid: 42"), "{text}");
    }

    #[test]
    fn human_renderer_reports_error_and_next_action() {
        let envelope = DaemonEnvelope::blocked(
            "daemon.status",
            DaemonError::new(
                DaemonErrorKind::DaemonUnavailable,
                "daemon_not_running",
                "the daemon is not running",
                true,
            ),
        );
        let mut rendered = Vec::new();
        write_human(&envelope, &mut rendered).unwrap();
        let text = String::from_utf8(rendered).unwrap();
        assert_eq!(text.lines().next().unwrap(), "status: blocked");
        assert!(text.contains("code: daemon_not_running"), "{text}");
        assert!(text.contains("next: "), "{text}");
    }
}
