use std::{
    error::Error as StdError,
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct RuntimeDbRetryableError {
    operation: &'static str,
    path: PathBuf,
    source: String,
}

impl RuntimeDbRetryableError {
    pub fn new(operation: &'static str, path: &Path, source: impl fmt::Display) -> Self {
        Self {
            operation,
            path: path.to_path_buf(),
            source: source.to_string(),
        }
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }
}

impl fmt::Display for RuntimeDbRetryableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "retryable runtime db error while {} for {}: {}",
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl StdError for RuntimeDbRetryableError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDbProtectionError {
    path: PathBuf,
    evidence: String,
}

impl RuntimeDbProtectionError {
    pub fn new(path: &Path, evidence: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            evidence: evidence.into(),
        }
    }

    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

impl fmt::Display for RuntimeDbProtectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "runtime database is quarantined after sidecar protection failed for {}: {}; preserve the database, WAL, SHM, and open-FD evidence, then restart or perform offline recovery",
            self.path.display(),
            self.evidence
        )
    }
}

impl StdError for RuntimeDbProtectionError {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDbProtectionState {
    Starting,
    Protected,
    Unsupported,
    Quarantined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDbProtectionStatus {
    pub state: RuntimeDbProtectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl RuntimeDbProtectionStatus {
    pub fn is_healthy(&self) -> bool {
        self.state != RuntimeDbProtectionState::Quarantined
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStateTransitionConflict {
    domain: &'static str,
    record_id: String,
    code: &'static str,
    existing_status: String,
    incoming_status: String,
    expected_revision: Option<u64>,
    actual_revision: Option<u64>,
    retryable: bool,
}

impl RuntimeStateTransitionConflict {
    pub fn new(
        domain: &'static str,
        record_id: impl Into<String>,
        existing_status: impl Into<String>,
        incoming_status: impl Into<String>,
    ) -> Self {
        Self {
            domain,
            record_id: record_id.into(),
            code: "state_transition_conflict",
            existing_status: existing_status.into(),
            incoming_status: incoming_status.into(),
            expected_revision: None,
            actual_revision: None,
            retryable: false,
        }
    }

    pub fn revision(
        record_id: impl Into<String>,
        code: &'static str,
        expected_revision: Option<u64>,
        actual_revision: Option<u64>,
        retryable: bool,
    ) -> Self {
        Self {
            domain: "work_item",
            record_id: record_id.into(),
            code,
            existing_status: actual_revision
                .map_or_else(|| "missing".to_string(), |revision| revision.to_string()),
            incoming_status: expected_revision
                .map_or_else(|| "none".to_string(), |revision| revision.to_string()),
            expected_revision,
            actual_revision,
            retryable,
        }
    }

    pub fn concurrent_mutation(domain: &'static str, record_id: impl Into<String>) -> Self {
        Self {
            domain,
            record_id: record_id.into(),
            code: "revision_conflict",
            existing_status: "changed".into(),
            incoming_status: "expected_snapshot".into(),
            expected_revision: None,
            actual_revision: None,
            retryable: true,
        }
    }

    pub fn domain(&self) -> &'static str {
        self.domain
    }

    pub fn record_id(&self) -> &str {
        &self.record_id
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn existing_status(&self) -> &str {
        &self.existing_status
    }

    pub fn incoming_status(&self) -> &str {
        &self.incoming_status
    }

    pub fn expected_revision(&self) -> Option<u64> {
        self.expected_revision
    }

    pub fn actual_revision(&self) -> Option<u64> {
        self.actual_revision
    }

    pub fn retryable(&self) -> bool {
        self.retryable
    }
}

impl fmt::Display for RuntimeStateTransitionConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.expected_revision.is_some() || self.actual_revision.is_some() {
            write!(
                formatter,
                "{} for {} {}: expected revision {:?}, actual revision {:?}",
                self.code,
                self.domain,
                self.record_id,
                self.expected_revision,
                self.actual_revision
            )
        } else {
            write!(
                formatter,
                "conflicting {} transition for {}: {} -> {}",
                self.domain, self.record_id, self.existing_status, self.incoming_status
            )
        }
    }
}

impl StdError for RuntimeStateTransitionConflict {}
