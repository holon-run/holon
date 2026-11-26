//! Durable command identity for an original input mutation.

use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::InputEvidenceScope;

const PHASE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputMutationFamily {
    Audit,
    AgentPosture,
    Evidence,
    Retention,
    Queue,
    QueueHeadNoProgress,
    Terminal,
    Task,
    TaskResult,
    LiveInputAttachment,
    LiveInputRecovery,
    ContextBudgetStopRecovery,
    ProviderResultHandoff,
    Timer,
    ExternalTrigger,
    WorkItem,
    Wait,
    Focus,
}
impl InputMutationFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::AgentPosture => "agent_posture",
            Self::Evidence => "evidence",
            Self::Retention => "retention",
            Self::Queue => "queue",
            Self::QueueHeadNoProgress => "queue_head_no_progress",
            Self::Terminal => "terminal",
            Self::Task => "task",
            Self::TaskResult => "task_result",
            Self::LiveInputAttachment => "live_input_attachment",
            Self::LiveInputRecovery => "live_input_recovery",
            Self::ContextBudgetStopRecovery => "context_budget_stop_recovery",
            Self::ProviderResultHandoff => "provider_result_handoff",
            Self::Timer => "timer",
            Self::ExternalTrigger => "external_trigger",
            Self::WorkItem => "work_item",
            Self::Wait => "wait",
            Self::Focus => "focus",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparedInputMutation {
    family: InputMutationFamily,
    phase_version: u32,
    command_nonce: String,
    command_digest: String,
    scopes: Vec<InputEvidenceScope>,
    scope_digest: String,
    prepared_at: DateTime<Utc>,
}
impl PreparedInputMutation {
    /// `command_digest` hashes the frozen typed command, never Debug output or
    /// SQL-produced sequences/counts. The nonce is the original attempt address.
    pub fn new(
        family: InputMutationFamily,
        command_nonce: String,
        mut scopes: Vec<InputEvidenceScope>,
        command_digest: String,
        prepared_at: DateTime<Utc>,
    ) -> Result<Self> {
        ensure!(
            !command_nonce.trim().is_empty(),
            ReceiptError::InvalidDescriptor("empty nonce")
        );
        ensure!(
            valid_sha256(&command_digest),
            ReceiptError::InvalidDescriptor("invalid SHA256 command digest")
        );
        ensure!(
            !scopes.is_empty(),
            ReceiptError::InvalidDescriptor("empty scope group")
        );
        ensure!(
            scopes.iter().all(|scope| !scope.agent_id.trim().is_empty()
                && scope.identity_incarnation > 0
                && scope.identity_incarnation <= i64::MAX as u64),
            ReceiptError::InvalidDescriptor("invalid scope")
        );
        scopes.sort_by(|a, b| {
            (&a.agent_id, a.identity_incarnation).cmp(&(&b.agent_id, b.identity_incarnation))
        });
        ensure!(
            scopes.windows(2).all(|pair| pair[0] != pair[1]),
            ReceiptError::InvalidDescriptor("duplicate scope")
        );
        let scope_digest = canonical_command_digest(&serde_json::json!({
            "family": family,
            "phase_version": PHASE_VERSION,
            "command_digest": command_digest,
            "scopes": scopes,
        }))?;
        Ok(Self {
            family,
            phase_version: PHASE_VERSION,
            command_nonce,
            command_digest,
            scopes,
            scope_digest,
            prepared_at,
        })
    }
    pub fn family(&self) -> InputMutationFamily {
        self.family
    }
    pub fn phase_version(&self) -> u32 {
        self.phase_version
    }
    pub fn command_nonce(&self) -> &str {
        &self.command_nonce
    }
    pub fn command_digest(&self) -> &str {
        &self.command_digest
    }
    pub fn scopes(&self) -> &[InputEvidenceScope] {
        &self.scopes
    }
    pub fn prepared_at(&self) -> DateTime<Utc> {
        self.prepared_at
    }
    pub fn scope_digest(&self) -> &str {
        &self.scope_digest
    }

    pub fn validate(&self) -> Result<()> {
        let expected = Self::new(
            self.family,
            self.command_nonce.clone(),
            self.scopes.clone(),
            self.command_digest.clone(),
            self.prepared_at,
        )?;
        ensure!(
            *self == expected,
            ReceiptError::CorruptReceipt("invalid prepared projection")
        );
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("invalid prepared input mutation: {0}")]
    InvalidDescriptor(&'static str),
    #[error("original input command nonce has a different family or payload")]
    CommandConflict,
    #[error("original input command has a different complete scope group")]
    GroupConflict,
    #[error("input mutation receipt is inconsistent: {0}")]
    CorruptReceipt(&'static str),
    #[error("invalid input mutation outcome: {0}")]
    InvalidOutcome(&'static str),
    #[error("original synchronous input mutation attempt has not finished")]
    OriginalAttemptNotFinished,
}

/// Stable serde encoding with recursively ordered object keys. Callers freeze
/// typed command clocks/IDs first; results allocated by SQL are not inputs.
pub fn canonical_command_digest(command: &impl Serialize) -> Result<String> {
    let mut command = serde_json::to_value(command)?;
    command.sort_all_objects();
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&command)?)
    ))
}
pub fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Hash the original result after its business payload has been typed.
pub fn canonical_result_digest(
    prepared: &PreparedInputMutation,
    outcome: &impl Serialize,
    committed_at: DateTime<Utc>,
) -> Result<String> {
    canonical_command_digest(&serde_json::json!({
        "prepared": prepared,
        "outcome": outcome,
        "committed_at": committed_at,
    }))
}

#[cfg(test)]
mod tests;
