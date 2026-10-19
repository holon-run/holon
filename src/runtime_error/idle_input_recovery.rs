use super::{RuntimeError, RuntimeErrorDescriptor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdleInputRecoveryPredicate {
    PredecessorMissing,
    PredecessorNotInterrupted,
    PredecessorAgentMismatch,
    PredecessorSourceMismatch,
    PredecessorBindingMismatch,
    ActivationMismatch,
    ReplaySourceMismatch,
    ReplayReasonMismatch,
    SourceAgentMismatch,
    ReplayMessageMismatch,
    OwnerMismatch,
    SourceTriggerMismatch,
    BatchAnchorMismatch,
    BatchIntegrityMismatch,
    AttachmentEvidenceMismatch,
}

impl IdleInputRecoveryPredicate {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PredecessorMissing => "predecessor_missing",
            Self::PredecessorNotInterrupted => "predecessor_not_interrupted",
            Self::PredecessorAgentMismatch => "predecessor_agent_mismatch",
            Self::PredecessorSourceMismatch => "predecessor_source_mismatch",
            Self::PredecessorBindingMismatch => "predecessor_binding_mismatch",
            Self::ActivationMismatch => "activation_mismatch",
            Self::ReplaySourceMismatch => "replay_source_mismatch",
            Self::ReplayReasonMismatch => "replay_reason_mismatch",
            Self::SourceAgentMismatch => "source_agent_mismatch",
            Self::ReplayMessageMismatch => "replay_message_mismatch",
            Self::OwnerMismatch => "owner_mismatch",
            Self::SourceTriggerMismatch => "source_trigger_mismatch",
            Self::BatchAnchorMismatch => "batch_anchor_mismatch",
            Self::BatchIntegrityMismatch => "batch_integrity_mismatch",
            Self::AttachmentEvidenceMismatch => "attachment_evidence_mismatch",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IdleInputRecoveryRejection {
    pub(crate) predicate: IdleInputRecoveryPredicate,
    pub(crate) field: &'static str,
    error: RuntimeError,
}

impl IdleInputRecoveryRejection {
    pub(crate) fn new(predicate: IdleInputRecoveryPredicate, field: &'static str) -> Self {
        Self {
            predicate,
            field,
            error: RuntimeError::validation(
                format!("idle_input_recovery_{}", predicate.as_str()),
                "Idle input recovery was rejected; no recovered input was attached.",
            )
            .with_recovery_hint(
                "Recovery was rejected and is not retried automatically. Inspect the safe \
                 predicate and field metadata before using a supported recovery path.",
            )
            .with_safe_context("operation", "idle_input_recovery")
            .with_safe_context("stage", "attach")
            .with_safe_context("predicate", predicate.as_str())
            .with_safe_context("field", field)
            .with_safe_context("recovery_disposition", "rejected"),
        }
    }

    pub(crate) fn with_id(mut self, key: &'static str, value: Option<&str>) -> Self {
        if let Some(value) = value {
            self.error = self
                .error
                .with_safe_context(key, super::safe_runtime_evidence_id(value));
        }
        self
    }

    pub(crate) fn descriptor(&self) -> RuntimeErrorDescriptor {
        self.error.descriptor().clone()
    }
}

impl std::fmt::Display for IdleInputRecoveryRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Idle input recovery rejected: {} ({})",
            self.predicate.as_str(),
            self.field
        )
    }
}

impl std::error::Error for IdleInputRecoveryRejection {}

pub(crate) fn safe_recovery_id(value: &str) -> &str {
    let valid_plain = |value: &str| {
        ["msg_", "turn_", "attempt_", "act_"].iter().any(|prefix| {
            value.strip_prefix(prefix).is_some_and(|suffix| {
                (1..=64).contains(&suffix.len())
                    && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
    };
    let valid = valid_plain(value)
        || value
            .strip_prefix("activation:message:")
            .is_some_and(|rest| {
                let (message, generation) = rest
                    .split_once(":attempt:")
                    .map_or((rest, None), |(message, generation)| {
                        (message, Some(generation))
                    });
                message.starts_with("msg_")
                    && valid_plain(message)
                    && generation.is_none_or(|generation| {
                        !generation.is_empty()
                            && generation.len() <= 20
                            && generation.bytes().all(|byte| byte.is_ascii_digit())
                            && generation
                                .parse::<u64>()
                                .is_ok_and(|generation| generation > 0)
                    })
            });
    if valid {
        value
    } else {
        "redacted_invalid_id"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_error::{describe_runtime_error, RuntimeErrorDomain};

    #[test]
    fn idle_recovery_descriptor_is_typed_bounded_and_does_not_copy_error_context() {
        let sentinel = "sentinel-secret?token=credential";
        let error = anyhow::Error::new(
            IdleInputRecoveryRejection::new(
                IdleInputRecoveryPredicate::ActivationMismatch,
                "batch.activation_id",
            )
            .with_id("activation_id", Some("act_abc123"))
            .with_id("batch_activation_id", Some(sentinel)),
        )
        .context(sentinel);
        let descriptor = describe_runtime_error(&error);
        assert_eq!(descriptor.domain, RuntimeErrorDomain::Validation);
        assert_eq!(descriptor.code, "idle_input_recovery_activation_mismatch");
        assert!(!descriptor.retryable);
        assert_eq!(descriptor.safe_context["field"], "batch.activation_id");
        assert_eq!(descriptor.safe_context["activation_id"], "act_abc123");
        assert_eq!(
            descriptor.safe_context["batch_activation_id"],
            "redacted_invalid_id"
        );
        assert!(descriptor.source_chain.is_empty());
        assert!(!serde_json::to_string(&descriptor)
            .unwrap()
            .contains(sentinel));
        assert_eq!(
            safe_recovery_id(&format!("turn_{}", "a".repeat(65))),
            "redacted_invalid_id"
        );
        assert_eq!(safe_recovery_id("turn_a\nsecret"), "redacted_invalid_id");
        for id in [
            "activation:message:msg_ab12",
            "activation:message:msg_ab12:attempt:2",
        ] {
            assert_eq!(safe_recovery_id(id), id);
        }
        for id in [
            "activation:message:msg_ab12:attempt:0",
            "activation:message:msg_ab12:attempt:2?token=secret",
        ] {
            assert_eq!(safe_recovery_id(id), "redacted_invalid_id");
        }
    }
}
