//! Original ingress provisioning/accounting; wake submission remains its own ACK.
use super::*;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};
use crate::runtime::resident_scheduling::{
    CanonicalSchedulingFact, SchedulingCaptureRequest, SchedulingFamily, SchedulingKey,
};
use crate::runtime_db::scheduling_snapshot::{
    self as scheduling, ExternalTriggerWriterObservation,
};
use crate::types::{
    CallbackDeliveryMode, CallbackIngressDisposition, ExternalTriggerRecord, ExternalTriggerScope,
    ExternalTriggerStatus,
};
use anyhow::ensure;

#[cfg(test)]
#[path = "prepared_external_trigger/selector_budget_tests.rs"]
mod selector_budget_tests;

#[cfg(test)]
#[path = "prepared_external_trigger/commit_body_budget_tests.rs"]
mod commit_body_budget_tests;

#[derive(Debug, serde::Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum ExternalTriggerOperation {
    Ensure {
        fresh: ExternalTriggerRecord,
        expected: Option<ExternalTriggerRecord>,
    },
    Reset {
        fresh: ExternalTriggerRecord,
        expected: Option<ExternalTriggerRecord>,
    },
    Revoke {
        expected: ExternalTriggerRecord,
        at: chrono::DateTime<Utc>,
    },
    Account {
        expected: ExternalTriggerRecord,
        at: chrono::DateTime<Utc>,
        disposition: CallbackIngressDisposition,
    },
}
#[derive(Debug, serde::Serialize)]
pub(crate) struct PreparedExternalTriggerTransition {
    #[serde(flatten)]
    operation: ExternalTriggerOperation,
    #[serde(skip)]
    observation: ExternalTriggerWriterObservation,
    #[serde(skip)]
    fault: Option<TransitionFaultPoint>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ExternalTriggerMutationOutcome {
    Ensured {
        record: ExternalTriggerRecord,
        revoked: Option<ExternalTriggerRecord>,
        created: bool,
    },
    Reset {
        record: ExternalTriggerRecord,
        revoked: Option<ExternalTriggerRecord>,
    },
    Revoked {
        record: ExternalTriggerRecord,
        changed: bool,
    },
    Accounted {
        record: ExternalTriggerRecord,
        disposition: CallbackIngressDisposition,
    },
}
impl ExternalTriggerMutationOutcome {
    pub(crate) fn validate(&self, prepared: &PreparedInputMutation) -> Result<()> {
        ensure!(
            prepared.family() == InputMutationFamily::ExternalTrigger
                && prepared.scopes().len() == 1,
            "Trigger ACK has another original family/group"
        );
        let (record, revoked) = match self {
            Self::Ensured {
                record, revoked, ..
            }
            | Self::Reset { record, revoked } => (record, revoked.as_ref()),
            Self::Revoked { record, .. } | Self::Accounted { record, .. } => (record, None),
        };
        for record in std::iter::once(record).chain(revoked) {
            ensure!(
                record.target_agent_id == prepared.scopes()[0].agent_id
                    && !record.external_trigger_id.is_empty(),
                "Trigger ACK scope/identity differs"
            );
        }
        if let Self::Revoked { record, .. } = self {
            ensure!(
                record.status == ExternalTriggerStatus::Revoked,
                "Trigger revoke ACK is Active"
            );
        }
        Ok(())
    }
}
impl PreparedExternalTriggerTransition {
    fn fresh(
        scope: &InputEvidenceScope,
        id: String,
        token: String,
        at: chrono::DateTime<Utc>,
    ) -> Result<ExternalTriggerRecord> {
        crate::diagnostics::resource_io::admit_cloned_value(&scope.agent_id)?;
        // The encoder writes into one fixed-length hex result.
        crate::diagnostics::resource_io::admit_cloned_value(
            &"0000000000000000000000000000000000000000000000000000000000000000",
        )?;
        Ok(ExternalTriggerRecord {
            external_trigger_id: id,
            target_agent_id: scope.agent_id.clone(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            token_hash: crate::callbacks::hash_callback_token(&token),
            token: Some(token),
            status: ExternalTriggerStatus::Active,
            created_at: at,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        })
    }
    pub(crate) fn ensure(
        observation: ExternalTriggerWriterObservation,
        id: String,
        token: String,
        at: chrono::DateTime<Utc>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let (fresh, expected) = observation.with_command_phase(|| {
            let fresh = Self::fresh(observation.scope(), id, token, at)?;
            Ok((fresh, clone_expected(&observation)?))
        })?;
        Ok(Self {
            operation: ExternalTriggerOperation::Ensure { fresh, expected },
            observation,
            fault,
        })
    }
    pub(crate) fn reset(
        observation: ExternalTriggerWriterObservation,
        id: String,
        token: String,
        at: chrono::DateTime<Utc>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let (fresh, expected) = observation.with_command_phase(|| {
            let fresh = Self::fresh(observation.scope(), id, token, at)?;
            Ok((fresh, clone_expected(&observation)?))
        })?;
        Ok(Self {
            operation: ExternalTriggerOperation::Reset { fresh, expected },
            observation,
            fault,
        })
    }
    pub(crate) fn revoke(
        observation: ExternalTriggerWriterObservation,
        at: chrono::DateTime<Utc>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let expected = observation.with_command_phase(|| {
            clone_expected(&observation)?.context("external trigger not found")
        })?;
        Ok(Self {
            operation: ExternalTriggerOperation::Revoke { expected, at },
            observation,
            fault,
        })
    }
    pub(crate) fn account(
        observation: ExternalTriggerWriterObservation,
        at: chrono::DateTime<Utc>,
        disposition: CallbackIngressDisposition,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let expected = observation.with_command_phase(|| {
            clone_expected(&observation)?.context("external trigger not found")
        })?;
        Ok(Self {
            operation: ExternalTriggerOperation::Account {
                expected,
                at,
                disposition,
            },
            observation,
            fault,
        })
    }
    pub(super) fn agent_id(&self) -> &str {
        &self.observation.scope().agent_id
    }
    pub(super) fn validate(&self, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.observation.scope() == scope,
            "Trigger observation has another original scope"
        );
        let expected = match &self.operation {
            ExternalTriggerOperation::Ensure { fresh, expected }
            | ExternalTriggerOperation::Reset { fresh, expected } => {
                ensure!(
                    fresh.target_agent_id == scope.agent_id
                        && !fresh.external_trigger_id.is_empty()
                        && fresh.token.as_ref().is_some_and(|token| !token.is_empty())
                        && fresh.token.as_deref().is_some_and(|token| {
                            crate::callbacks::callback_token_hash_matches(token, &fresh.token_hash)
                        }),
                    "Trigger fresh capability is invalid"
                );
                expected.as_ref()
            }
            ExternalTriggerOperation::Revoke { expected, .. }
            | ExternalTriggerOperation::Account { expected, .. } => Some(expected),
        };
        ensure!(
            expected == self.observation.record(),
            "Trigger command differs from its original observation"
        );
        ensure!(
            expected.is_none_or(|record| record.target_agent_id == scope.agent_id),
            "Trigger command owner differs"
        );
        Ok(())
    }
    fn keys(&self) -> Result<BTreeSet<SchedulingKey>> {
        let mut keys = BTreeSet::new();
        match &self.operation {
            ExternalTriggerOperation::Ensure { fresh, expected } => {
                if expected
                    .as_ref()
                    .is_none_or(|record| record.token.is_none())
                {
                    keys.insert(trigger_selector(&fresh.external_trigger_id)?);
                }
                if let Some(record) = expected {
                    keys.insert(trigger_selector(&record.external_trigger_id)?);
                }
            }
            ExternalTriggerOperation::Reset { fresh, expected } => {
                keys.insert(trigger_selector(&fresh.external_trigger_id)?);
                if let Some(record) = expected {
                    keys.insert(trigger_selector(&record.external_trigger_id)?);
                }
            }
            ExternalTriggerOperation::Revoke { expected, .. }
            | ExternalTriggerOperation::Account { expected, .. } => {
                keys.insert(trigger_selector(&expected.external_trigger_id)?);
            }
        }
        Ok(keys)
    }
}
fn clone_expected(
    observation: &ExternalTriggerWriterObservation,
) -> Result<Option<ExternalTriggerRecord>> {
    observation.record().map(clone_trigger_record).transpose()
}
fn clone_trigger_record(record: &ExternalTriggerRecord) -> Result<ExternalTriggerRecord> {
    crate::diagnostics::resource_io::admit_cloned_value(record)?;
    Ok(record.clone())
}
fn push_trigger_effect(
    effects: &mut PostCommitEffects,
    record: &ExternalTriggerRecord,
) -> Result<()> {
    effects
        .external_triggers
        .push(clone_trigger_record(record)?);
    Ok(())
}
fn trigger_selector(id: &str) -> Result<SchedulingKey> {
    crate::diagnostics::resource_io::admit_cloned_value(&id)?;
    Ok(SchedulingKey::new(SchedulingFamily::ExternalTrigger, id))
}

impl PreparedInputTransition {
    pub(crate) fn external_trigger(
        scope: InputEvidenceScope,
        nonce: String,
        at: chrono::DateTime<Utc>,
        command: PreparedExternalTriggerTransition,
    ) -> Result<Self> {
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::ExternalTrigger(command),
        )
    }
}
fn remaining(
    request: &scheduling::SchedulingDomainLoadRequest,
    bytes: usize,
    records: usize,
) -> Result<scheduling::SchedulingDomainLoadRequest> {
    Ok(scheduling::SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        byte_limit: request
            .byte_limit
            .min(64 * 1024 * 1024)
            .checked_sub(bytes)
            .filter(|n| *n > 0)
            .context(scheduling::SchedulingSnapshotError::ResourceExhausted(
                "Trigger combined bytes",
            ))?,
        record_limit: request
            .record_limit
            .min(65536)
            .checked_sub(records)
            .filter(|n| *n > 0)
            .context(scheduling::SchedulingSnapshotError::ResourceExhausted(
                "Trigger combined records",
            ))?,
    })
}
pub(super) fn commit_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    command: &PreparedExternalTriggerTransition,
    request: &scheduling::SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let scope = &prepared.primary_scope;
    ensure!(
        prepared.descriptor.scopes() == std::slice::from_ref(scope),
        "Trigger command group differs"
    );
    let transaction_material = command.observation.transaction_material()?;
    let _resource_io = transaction_material.enter();
    command.validate(scope)?;
    let (mut bytes, mut records) = command.observation.material(ledger)?;
    let intent_bytes = usize::try_from(serialized_bytes(command)?)?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .context("Trigger intent bytes overflow")?;
    bytes = bytes
        .checked_add(intent_bytes)
        .context("Trigger combined bytes overflow")?;
    records = records
        .checked_add(32)
        .context("Trigger combined records overflow")?;
    let allowance = remaining(request, bytes, records)?;
    let intent_guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(intent_bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let receipt_request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        byte_limit: allowance.byte_limit,
        record_limit: allowance.record_limit,
    };
    let (receipt_guard, receipt_bytes, receipt_records) =
        super::super::prepared_task_result::receipt_budget_with_material_tx(
            tx,
            prepared.descriptor(),
            scope,
            &receipt_request,
            ledger,
        )?;
    bytes = bytes
        .checked_add(receipt_bytes)
        .context("Trigger receipt bytes overflow")?;
    records = records
        .checked_add(receipt_records)
        .context("Trigger receipt count overflow")?;
    let allowance = remaining(request, bytes, records)?;
    if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, prepared.descriptor())? {
        return Ok(InputMutationExecution::Replayed {
            receipt,
            _material_budget_views: {
                let mut guards = vec![intent_guard, receipt_guard];
                guards.extend(command.observation.budget_views());
                guards
                    .extend(crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?);
                guards
            },
        });
    }
    let keys = command.keys()?;
    let existing = matches!(
        &command.operation,
        ExternalTriggerOperation::Revoke { .. } | ExternalTriggerOperation::Account { .. }
    );
    let mut proof = if existing {
        scheduling::prepare_original_scheduling_cleanup_tx(tx, scope, &keys, &allowance, ledger)?
    } else {
        scheduling::prepare_scheduling_write_tx(tx, scope, &keys, &allowance, ledger)?
    };
    proof.absorb_material(bytes, records, request)?;
    match &command.operation {
        ExternalTriggerOperation::Ensure { expected, .. }
        | ExternalTriggerOperation::Reset { expected, .. } => {
            let expected_id = expected
                .as_ref()
                .map(|record| record.external_trigger_id.as_str());
            let exact:bool=tx.query_row("SELECT (SELECT external_trigger_id FROM external_triggers WHERE target_agent_id=?1 AND status='active' ORDER BY created_at DESC,external_trigger_id ASC LIMIT 1) IS ?2",params![scope.agent_id,expected_id],|row|row.get(0))?;
            ensure!(exact, "Trigger original default changed");
            if let Some(expected) = expected {
                ensure!(
                    matches!(proof.fact_before(&trigger_selector(&expected.external_trigger_id)?),Some(CanonicalSchedulingFact::ExternalTrigger(actual)) if actual==expected),
                    "Trigger default canonical body changed"
                );
            }
        }
        ExternalTriggerOperation::Revoke { expected, .. }
        | ExternalTriggerOperation::Account { expected, .. } => {
            ensure!(
                matches!(proof.fact_before(&trigger_selector(&expected.external_trigger_id)?),Some(CanonicalSchedulingFact::ExternalTrigger(actual)) if actual==expected),
                "Trigger original accounting/revocation changed"
            );
        }
    }
    inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
    let mut actual_ids = BTreeSet::new();
    let outcome = match &command.operation {
        ExternalTriggerOperation::Ensure { fresh, .. } => {
            let (record, revoked, created) = repositories::ensure_default_external_trigger_tx(
                tx,
                &scope.agent_id,
                fresh.delivery_mode.clone(),
                fresh.created_at,
                &fresh.external_trigger_id,
                fresh.token.as_deref().expect("validated token"),
                &mut actual_ids,
            )?;
            let record =
                proof.read_external_trigger_ack_tx(tx, &record.external_trigger_id, request)?;
            let revoked = revoked
                .map(|record| {
                    proof.read_external_trigger_ack_tx(tx, &record.external_trigger_id, request)
                })
                .transpose()?;
            ExternalTriggerMutationOutcome::Ensured {
                record,
                revoked,
                created,
            }
        }
        ExternalTriggerOperation::Reset { fresh, .. } => {
            let (record, revoked) = repositories::reset_default_external_trigger_tx(
                tx,
                &scope.agent_id,
                fresh.delivery_mode.clone(),
                fresh.created_at,
                &fresh.external_trigger_id,
                fresh.token.as_deref().expect("validated token"),
                &mut actual_ids,
            )?;
            let record =
                proof.read_external_trigger_ack_tx(tx, &record.external_trigger_id, request)?;
            let revoked = revoked
                .map(|record| {
                    proof.read_external_trigger_ack_tx(tx, &record.external_trigger_id, request)
                })
                .transpose()?;
            ExternalTriggerMutationOutcome::Reset { record, revoked }
        }
        ExternalTriggerOperation::Revoke { expected, at } => {
            let changed = if expected.status == ExternalTriggerStatus::Revoked {
                false
            } else {
                let mut revoked = clone_trigger_record(expected)?;
                revoked.status = ExternalTriggerStatus::Revoked;
                revoked.revoked_at = Some(*at);
                let changed = repositories::upsert_external_trigger_with_write_tx(tx, &revoked)?;
                if changed {
                    crate::diagnostics::resource_io::admit_cloned_value(
                        &revoked.external_trigger_id,
                    )?;
                    actual_ids.insert(revoked.external_trigger_id);
                }
                changed
            };
            let record =
                proof.read_external_trigger_ack_tx(tx, &expected.external_trigger_id, request)?;
            ExternalTriggerMutationOutcome::Revoked { record, changed }
        }
        ExternalTriggerOperation::Account {
            expected,
            at,
            disposition,
        } => {
            let mut updated = clone_trigger_record(expected)?;
            updated.last_delivered_at = Some(*at);
            updated.delivery_count += 1;
            if repositories::upsert_external_trigger_with_write_tx(tx, &updated)? {
                crate::diagnostics::resource_io::admit_cloned_value(&updated.external_trigger_id)?;
                actual_ids.insert(updated.external_trigger_id.clone());
            }
            let record =
                proof.read_external_trigger_ack_tx(tx, &updated.external_trigger_id, request)?;
            ExternalTriggerMutationOutcome::Accounted {
                record,
                disposition: disposition.clone(),
            }
        }
    };
    inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;
    let actual_keys = actual_ids
        .iter()
        .map(|id| trigger_selector(id))
        .collect::<Result<BTreeSet<_>>>()?;
    // No Trigger operation changes business State or starts a dispatch.
    let mut facts = scheduling::register_scheduling_writes_tx(
        tx,
        &proof,
        &actual_keys,
        proof.state_before()?,
        request,
    )?;
    let mut effects = PostCommitEffects::default();
    effects.scheduling_write_keys = actual_keys;
    effects.scheduling_delta_requires_prepared_domain = facts.requires_prepared_domain;
    effects
        .scheduling_material_budget_views
        .extend(command.observation.budget_views());
    effects
        .scheduling_material_budget_views
        .extend([intent_guard, receipt_guard]);
    effects
        .scheduling_material_budget_views
        .extend(facts.take_budget_views());
    effects.scheduling_delta = Some(Box::new(facts.delta));
    effects.fault = command.fault;
    match &outcome {
        ExternalTriggerMutationOutcome::Ensured {
            record,
            revoked,
            created,
        } => {
            if *created {
                if let Some(revoked) = revoked {
                    push_trigger_effect(&mut effects, revoked)?;
                }
                push_trigger_effect(&mut effects, record)?;
            }
        }
        ExternalTriggerMutationOutcome::Reset { record, revoked } => {
            if let Some(revoked) = revoked {
                push_trigger_effect(&mut effects, revoked)?;
            }
            push_trigger_effect(&mut effects, record)?;
        }
        ExternalTriggerMutationOutcome::Revoked { record, changed } => {
            if *changed {
                push_trigger_effect(&mut effects, record)?;
            }
        }
        ExternalTriggerMutationOutcome::Accounted { record, .. } => {
            push_trigger_effect(&mut effects, record)?
        }
    }
    let applied = !actual_ids.is_empty();
    let install_keys =
        crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
            scope, &effects,
        )?;
    let receipt_outcome = InputMutationOutcome {
        result: InputMutationResult::ExternalTrigger {
            outcome: Box::new(outcome),
        },
        applied,
        delivery_receipt: None,
        scopes: vec![InputMutationScopeOutcome {
            scope: scope.clone(),
            install_keys,
            audit_events: vec![],
            notify_memory_index: false,
            notify_scheduler: false,
        }],
    };
    let receipt = insert_input_mutation_receipt_tx(
        tx,
        prepared.descriptor(),
        &receipt_outcome,
        prepared.descriptor.prepared_at(),
    )?;
    inject_fault(command.fault, TransitionFaultPoint::BeforeCommit)?;
    Ok(InputMutationExecution::Committed {
        value: TransitionCommit {
            applied,
            effects: Box::new(effects),
            ..Default::default()
        },
        receipt,
    })
}
