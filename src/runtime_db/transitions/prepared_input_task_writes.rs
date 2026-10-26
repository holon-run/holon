//! Task qualification joins the original business transaction, never a later callback.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_task::{TaskFamily, TaskKey},
};
use crate::runtime_db::{task_result_settlement as result, task_snapshot as tasks};
use std::sync::Arc;

struct Material<'a, 'memory> {
    scope: &'a InputEvidenceScope,
    request: &'a tasks::TaskDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    memory_budget:
        Option<&'memory mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl Material<'_, '_> {
    fn charge(&mut self, bytes: usize, records: usize, copies: usize) -> Result<()> {
        let bytes = bytes
            .checked_mul(copies)
            .context("Task writer byte overflow")?;
        let records = records
            .checked_mul(copies)
            .context("Task writer record overflow")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("Task writer material overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("Task writer count overflow")?;
        anyhow::ensure!(
            self.bytes <= self.request.byte_limit.min(64 * 1024 * 1024)
                && self.records <= self.request.record_limit.min(65_536),
            tasks::TaskSnapshotError::ResourceExhausted("original writer material")
        );
        if let Some(memory) = &mut self.memory_budget {
            memory.additional_material(bytes, records, 1)?;
        }
        self.guards.push(self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(bytes)?,
                metadata_estimate_bytes: 0,
            },
        )?);
        Ok(())
    }
    fn remaining(&self) -> Result<tasks::TaskDomainLoadRequest> {
        let byte_limit = self
            .request
            .byte_limit
            .min(64 * 1024 * 1024)
            .checked_sub(self.bytes)
            .filter(|limit| *limit > 0)
            .ok_or(tasks::TaskSnapshotError::ResourceExhausted(
                "original predicate allowance",
            ))?;
        let record_limit = self
            .request
            .record_limit
            .min(65_536)
            .checked_sub(self.records)
            .filter(|limit| *limit > 0)
            .ok_or(tasks::TaskSnapshotError::ResourceExhausted(
                "original predicate allowance",
            ))?;
        Ok(tasks::TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            // Token observes the already-closed selection, never new roots.
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            byte_limit,
            record_limit,
        })
    }
    fn absorb_predicate(&mut self, observed: tasks::TaskMaintenanceMaterial) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(observed.bytes)
            .context("Task predicate material overflow")?;
        self.records = self
            .records
            .checked_add(observed.records)
            .context("Task predicate count overflow")?;
        anyhow::ensure!(
            self.bytes <= self.request.byte_limit.min(64 * 1024 * 1024)
                && self.records <= self.request.record_limit.min(65_536),
            tasks::TaskSnapshotError::ResourceExhausted("original predicate allowance")
        );
        // The token has already charged real Host + the same Memory callback.
        // This is just cumulative accounting, never a second charge/guard.
        Ok(())
    }
    fn result(&mut self, estimate: result::TaskResultMaterialEstimate) -> Result<()> {
        self.charge(
            estimate
                .payload_bytes
                .checked_add(estimate.metadata_estimate_bytes)
                .context("TaskResult material overflow")?,
            estimate
                .records
                .checked_mul(12)
                .context("TaskResult count overflow")?,
            1,
        )
    }
    fn keys(&mut self, keys: &BTreeSet<TaskKey>, copies: usize) -> Result<()> {
        self.charge(
            usize::try_from(serialized_bytes(keys)?)?,
            keys.len(),
            copies,
        )
    }
    fn message(&mut self, tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
        // D3 may validate a larger already-stored body before a duplicate append
        // is rejected. Reserve both actual materials without loading either again.
        let (existing, metadata_records): (usize, usize) = tx.query_row(
            "SELECT COALESCE((SELECT SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(content_hash AS BLOB)),0)+length(CAST(created_at AS BLOB))) FROM messages WHERE evidence_id=?1 OR message_id=?1),0)+COALESCE((SELECT length(CAST(turn_id AS BLOB)) FROM conversation_input_assignments WHERE agent_id=?2 AND message_id=?1),0)+COALESCE((SELECT SUM(length(CAST(fingerprint_json AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(attempt_turn_proof_json AS BLOB)),0)) FROM input_evidence_current WHERE agent_id=?2 AND identity_incarnation=?3 AND family='message' AND record_id=?1),0)+COALESCE((SELECT SUM(length(CAST(m.fingerprint_json AS BLOB))) FROM input_evidence_seed_membership m JOIN agent_episode_continuity b ON b.seed_id=m.seed_id WHERE b.agent_id=?2 AND b.identity_incarnation=?3 AND b.policy_version=1 AND m.family='message' AND m.record_id=?1),0),1+(SELECT COUNT(*) FROM messages WHERE evidence_id=?1 OR message_id=?1)+(SELECT COUNT(*) FROM conversation_input_assignments WHERE agent_id=?2 AND message_id=?1)+(SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?2 AND identity_incarnation=?3 AND family='message' AND record_id=?1)+(SELECT COUNT(*) FROM input_evidence_seed_membership m JOIN agent_episode_continuity b ON b.seed_id=m.seed_id WHERE b.agent_id=?2 AND b.identity_incarnation=?3 AND b.policy_version=1 AND m.family='message' AND m.record_id=?1)",
            params![message.id, self.scope.agent_id, self.scope.identity_incarnation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let frozen = usize::try_from(serialized_bytes(message)?)?;
        let bytes = existing
            .checked_add(frozen)
            .and_then(|n| n.checked_add(message.id.len() + 256))
            .context("Task Message material overflow")?;
        self.charge(bytes, metadata_records, 8)
    }
    fn checkpoint(&mut self, tx: &Transaction<'_>) -> Result<()> {
        let (bytes, records): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(s.roots_json AS BLOB))+COALESCE(length(CAST(i.roots_json AS BLOB)),0)),0),COUNT(*) FROM agent_episode_continuity b JOIN episode_continuity_seeds s ON s.seed_id=b.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=s.seed_id WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1",
            params![self.scope.agent_id,self.scope.identity_incarnation], |r| Ok((r.get(0)?,r.get(1)?)))?;
        self.charge(bytes, records, 4)
    }
}

pub(super) struct TaskTransaction<'tx, 'connection, 'command> {
    proof: tasks::TaskWriteProof,
    cleanup: Option<tasks::OriginalTaskResultCleanupProof<'tx, 'connection, 'command>>,
    allowed: BTreeSet<TaskKey>,
    observations: BTreeSet<TaskKey>,
    result: Option<result::PreparedTaskResultMutation<'tx, 'connection>>,
    predicate: Option<tasks::OriginalResultPredicateProof<'tx, 'connection>>,
    request: tasks::TaskDomainLoadRequest,
    guards: Vec<Arc<BudgetViewGuard>>,
    original_material: (usize, usize),
    material_total: (usize, usize),
    original_state_material: usize,
    child_phase: bool,
}
impl<'tx, 'connection, 'command> TaskTransaction<'tx, 'connection, 'command> {
    pub(super) fn task_write_proof(&self) -> &tasks::TaskWriteProof {
        &self.proof
    }

    pub(super) fn allowed_keys(&self) -> &BTreeSet<TaskKey> {
        &self.allowed
    }

    /// Reserve all Stop-owned preproof/ACK/cap copies before Root's bounded reads.
    pub(super) fn prepare_stop_material_tx(
        &mut self,
        tx: &Transaction<'_>,
        descriptor: &crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
        intent: &crate::runtime_db::task_stop::PreparedTaskStop,
        expected: &crate::types::TaskRecord,
        ledger: &InputEvidenceBudgetLedger,
        memory_budget: Option<
            &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
    ) -> Result<()> {
        let before = self.proof.task_before(&intent.task.key.id)?;
        let sql_bytes = tasks::checkpoint_material_size_on(tx, &intent.task.key)?
            .context("TaskStop original SQL Task missing")?;
        let scope_bytes: usize = tx.query_row(
            "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(mode AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1),0)",
            params![intent.task.scope.agent_id,intent.task.scope.identity_incarnation], |row|row.get(0))?;
        let bytes = usize::try_from(serialized_bytes(&(descriptor, intent, before, expected))?)?
            .checked_add(sql_bytes)
            .and_then(|n| n.checked_add(scope_bytes))
            .and_then(|n| n.checked_add(intent.task.key.id.len() + 2048))
            .context("TaskStop original material overflow")?;
        let mut material = Material {
            scope: self.proof.scope(),
            request: &self.request,
            ledger,
            memory_budget,
            bytes: self.material_total.0,
            records: self.material_total.1,
            guards: Vec::new(),
        };
        // Four bounded phases (original readback, typed intent, actual ACK and
        // inserted observation) each use the existing eight-copy envelope.
        material.charge(bytes, 4, 32)?;
        self.material_total = (material.bytes, material.records);
        self.guards.extend(material.guards);
        Ok(())
    }

    /// Original Command uses the same cumulative Task and Memory allowance.
    pub(super) fn prepare_original_terminal_material_tx(
        &mut self,
        tx: &Transaction<'_>,
        command: &TaskTransitionCommand,
        ledger: &InputEvidenceBudgetLedger,
        memory_budget: Option<
            &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
    ) -> Result<()> {
        let scope = self.proof.scope();
        let extra_task =
            command.task_result_settlement.is_none() && !command.message_evidence.is_empty();
        let mut bytes = if extra_task {
            tasks::task_maintenance_material_tx(
                tx,
                scope,
                &BTreeSet::from([TaskKey::new(TaskFamily::Task, &command.task.id)]),
            )?
            .bytes
            .checked_div(8)
            .context("Command original material copies invalid")?
        } else {
            0
        };
        if !command.audit_events.is_empty() {
            let epoch:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(value AS BLOB)) FROM runtime_metadata WHERE key='event_log_epoch'),0)",[],|r|r.get(0))?;
            bytes = bytes
                .checked_add(epoch)
                .context("Command event epoch material overflow")?;
            for event in &command.audit_events {
                let n:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(data_json AS BLOB))+COALESCE(length(CAST(agent_id AS BLOB)),0) FROM audit_events WHERE audit_event_id=?1),0)",[&event.id],|r|r.get(0))?;
                bytes = bytes
                    .checked_add(n)
                    .context("Command original audit material overflow")?;
            }
        }
        let records = command
            .audit_events
            .len()
            .checked_add(usize::from(extra_task))
            .and_then(|n| n.checked_add(2))
            .context("Command material records overflow")?;
        let mut material = Material {
            scope,
            request: &self.request,
            ledger,
            memory_budget,
            bytes: self.material_total.0,
            records: self.material_total.1,
            guards: Vec::new(),
        };
        material.charge(
            bytes
                .checked_add(512)
                .context("Command material metadata overflow")?,
            records,
            8,
        )?;
        if let Some(deletion) = &command.agent_deletion {
            anyhow::ensure!(
                self.child_phase,
                "original producer deletion requires its authentic child facade"
            );
            // Every finite row actually decoded by the original deletion kernel,
            // including canonical relation rules and accepted-delivery cleanup.
            let (raw,rows):(usize,usize)=tx.query_row(
                "WITH material(bytes,n) AS (
                 SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))),0),COUNT(*) FROM agent_identities WHERE agent_id=?1
                 UNION ALL SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0),COUNT(*) FROM agent_deletion_jobs WHERE agent_id=?1
                 UNION ALL SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0),COUNT(*) FROM tasks WHERE task_id=?2 OR task_id=(SELECT json_extract(payload_json,'$.delegated_from_task_id') FROM agent_identities WHERE agent_id=?1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(json_array(parent_agent_id,creation_cause,created_at) AS BLOB))),0),COUNT(*) FROM agent_lineages WHERE child_agent_id=?1
                 UNION ALL SELECT COALESCE(SUM(length(CAST(json_array(supervision_id,supervisor_agent_id,delegated_from_work_item_id,delegated_from_task_id,state,created_at,updated_at,payload_json) AS BLOB))),0),COUNT(*) FROM (SELECT * FROM agent_supervisions WHERE child_agent_id=?1 ORDER BY revision DESC,updated_at DESC,supervision_id DESC LIMIT 1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(json_array(durability,created_at) AS BLOB))),0),COUNT(*) FROM (SELECT * FROM agent_durability_records WHERE agent_id=?1 ORDER BY revision DESC LIMIT 1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(json_array(attachment,created_at) AS BLOB))),0),COUNT(*) FROM (SELECT * FROM agent_lifecycle_attachment_records WHERE agent_id=?1 ORDER BY revision DESC LIMIT 1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(created_at AS BLOB))),0),COUNT(*) FROM (SELECT * FROM agent_capability_policy_records WHERE agent_id=?1 ORDER BY revision DESC LIMIT 1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(json_array(capability_family,effect) AS BLOB))),0),COUNT(*) FROM agent_capability_policy_rules WHERE agent_id=?1 AND revision=(SELECT MAX(revision) FROM agent_capability_policy_records WHERE agent_id=?1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(json_array(default_effect,created_at) AS BLOB))),0),COUNT(*) FROM (SELECT * FROM agent_message_policy_records WHERE agent_id=?1 ORDER BY revision DESC LIMIT 1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(json_array(principal_kind,principal_id,route,effect) AS BLOB))),0),COUNT(*) FROM agent_message_policy_rules WHERE agent_id=?1 AND revision=(SELECT MAX(revision) FROM agent_message_policy_records WHERE agent_id=?1)
                 UNION ALL SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0),COUNT(*) FROM agent_message_deliveries WHERE target_agent_id=?1 AND state IN ('queued','dispatched'))
                 SELECT SUM(bytes),SUM(n) FROM material",
                params![deletion.agent_id,command.task.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            material.charge(
                raw.checked_add(
                    rows.checked_mul(512)
                        .context("child deletion row metadata overflow")?,
                )
                .context("child deletion bytes overflow")?,
                rows,
                8,
            )?;
            // Native JSON traversal happens only after the complete raw gate.
            let nodes:usize=tx.query_row(
                "SELECT (SELECT COUNT(*) FROM agent_identities a,json_tree(a.payload_json) WHERE a.agent_id=?1)
                    +(SELECT COUNT(*) FROM agent_deletion_jobs j,json_tree(j.payload_json) WHERE j.agent_id=?1)
                    +(SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?2 OR t.task_id=(SELECT json_extract(payload_json,'$.delegated_from_task_id') FROM agent_identities WHERE agent_id=?1))
                    +(SELECT COUNT(*) FROM agent_message_deliveries d,json_tree(d.payload_json) WHERE d.target_agent_id=?1 AND d.state IN ('queued','dispatched'))",
                params![deletion.agent_id,command.task.id],|r|r.get(0))?;
            material.charge(
                nodes
                    .checked_mul(256)
                    .context("child deletion JSON nodes overflow")?,
                nodes,
                8,
            )?;
            // The original kernel decodes this pool, then looks each delivery
            // up again by its payload Message ID. Prove that second lookup
            // remains the same budgeted canonical row before any business SQL.
            let drift:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_message_deliveries WHERE target_agent_id=?1 AND state IN ('queued','dispatched') AND (json_extract(payload_json,'$.delivery_id') IS NOT delivery_id OR json_extract(payload_json,'$.target_agent_id') IS NOT target_agent_id OR json_extract(payload_json,'$.message_id') IS NOT message_id OR json_extract(payload_json,'$.state') IS NOT state OR json_extract(payload_json,'$.state_version') IS NOT state_version OR json_extract(payload_json,'$.outcome') IS NOT outcome))",[&deletion.agent_id],|row|row.get(0))?;
            anyhow::ensure!(
                !drift,
                "original child deletion delivery normalized projection differs"
            );
        }
        self.material_total = (material.bytes, material.records);
        self.guards.extend(material.guards);
        Ok(())
    }

    pub(super) fn apply_original_child_source_tx(
        &mut self,
        tx: &Transaction<'_>,
        prepared: &PreparedInputTransition,
        ledger: &InputEvidenceBudgetLedger,
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<super::child_terminal::ChildSourcePhase> {
        anyhow::ensure!(
            self.child_phase,
            "child Source requires its enclosing original Task proof"
        );
        let mut material = tasks::read_preparation::TaskReadMaterial::new(
            self.proof.scope(),
            &self.request,
            Some(ledger),
        )
        .with_memory_budget(memory)
        .with_admitted_material(tasks::TaskMaintenanceMaterial {
            bytes: self.material_total.0,
            records: self.material_total.1,
        })?;
        let result = super::child_terminal::apply_source_phase_tx(
            tx,
            prepared,
            &self.request,
            ledger,
            &mut material,
        )?;
        let observed = material.material_estimate();
        self.material_total = (observed.bytes, observed.records);
        self.guards.extend(material.into_guards());
        Ok(result)
    }
    pub(super) fn finish_original_child(
        self,
        tx: &Transaction<'_>,
        value: &mut TransitionCommit,
        ledger: &InputEvidenceBudgetLedger,
        memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<tasks::TaskMaintenanceMaterial> {
        anyhow::ensure!(
            self.child_phase,
            "child finalizer requires original Task facet"
        );
        self.finish_inner(tx, value, ledger, memory, None)
    }

    pub(super) fn cleanup(
        &self,
    ) -> Option<&tasks::OriginalTaskResultCleanupProof<'tx, 'connection, 'command>> {
        self.cleanup.as_ref()
    }
    pub(super) fn take_result(
        &mut self,
    ) -> Option<result::PreparedTaskResultMutation<'tx, 'connection>> {
        self.result.take()
    }
    pub(super) fn execute_result(&mut self) -> Result<result::TaskResultWriteAck> {
        let operation = self
            .result
            .take()
            .context("TaskResult omitted its closed operation")?;
        if let Some(predicate) = self.predicate.take() {
            let (ack, retained) = operation.execute_with_original_predicate(predicate)?;
            self.guards.extend(retained.into_budget_views());
            Ok(ack)
        } else {
            operation.execute()
        }
    }
    pub(super) fn finish(
        self,
        tx: &Transaction<'_>,
        value: &mut TransitionCommit,
        ledger: &InputEvidenceBudgetLedger,
        memory_budget: Option<
            &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
    ) -> Result<()> {
        self.finish_inner(tx, value, ledger, memory_budget, None)
            .map(|_| ())
    }
    pub(super) fn finish_with_stop_observation(
        self,
        tx: &Transaction<'_>,
        value: &mut TransitionCommit,
        ledger: &InputEvidenceBudgetLedger,
        memory_budget: Option<
            &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
        inserted: &crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>,
    ) -> Result<()> {
        self.finish_inner(tx, value, ledger, memory_budget, Some(inserted))
            .map(|_| ())
    }
    fn finish_inner(
        mut self,
        tx: &Transaction<'_>,
        value: &mut TransitionCommit,
        ledger: &InputEvidenceBudgetLedger,
        mut memory_budget: Option<
            &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
        inserted: Option<&crate::runtime_db::task_stop::TaskStopInsertedObservation<'_, '_>>,
    ) -> Result<tasks::TaskMaintenanceMaterial> {
        anyhow::ensure!(
            self.result.is_none() && self.predicate.is_none(),
            "original TaskResult operation or predicate was not consumed"
        );
        let actual: BTreeSet<_> = value
            .effects
            .task_write_keys
            .intersection(&self.allowed)
            .cloned()
            .collect();
        // Raw cleanup targets are observed by the business ACK but never adopted.
        value
            .effects
            .task_observation_keys
            .extend(self.observations);
        let after = tasks::task_maintenance_material_tx(tx, self.proof.scope(), &self.allowed)?;
        let state_material = state_material_tx(tx, self.proof.scope())?;
        let additional_bytes = after
            .bytes
            .saturating_sub(self.original_material.0)
            .checked_add(state_material.saturating_sub(self.original_state_material))
            .context("Task final State material overflow")?;
        let additional_records = after.records.saturating_sub(self.original_material.1);
        if additional_bytes > 0 || additional_records > 0 {
            let mut material = Material {
                scope: self.proof.scope(),
                request: &self.request,
                ledger,
                memory_budget: memory_budget.as_deref_mut(),
                bytes: self.material_total.0,
                records: self.material_total.1,
                guards: Vec::new(),
            };
            material.charge(additional_bytes, additional_records, 1)?;
            self.material_total = (material.bytes, material.records);
            self.guards.extend(material.guards);
        }
        if let Some(cleanup) = self.cleanup.take() {
            let actual_result_ids = value
                .effects
                .task_write_keys
                .iter()
                .filter(|key| key.family == TaskFamily::ResultSettlement)
                .map(|key| key.id.clone())
                .collect();
            self.proof = cleanup.into_task_write_proof(tx, &actual_result_ids)?;
        }
        let final_state = crate::runtime_db::context_inputs::ContextDurableRead::new(
            tx,
            &self.proof.scope().agent_id,
        )
        .scope_facts()?
        .state
        .context("Task finalizer requires actual final State")?;
        let facts = if self.child_phase {
            anyhow::ensure!(
                inserted.is_none(),
                "child terminal cannot combine Stop insertion"
            );
            let mut material = tasks::read_preparation::TaskReadMaterial::new(
                self.proof.scope(),
                &self.request,
                Some(ledger),
            )
            .with_memory_budget(memory_budget.as_deref_mut())
            .with_admitted_material(tasks::TaskMaintenanceMaterial {
                bytes: self.material_total.0,
                records: self.material_total.1,
            })?;
            let facts = tasks::register_original_task_writes_using_material_tx(
                tx,
                &self.proof,
                &actual,
                &final_state,
                &self.request,
                ledger,
                &mut material,
            )?;
            let usage = material.material_estimate();
            self.material_total = (usage.bytes, usage.records);
            self.guards.extend(material.into_guards());
            facts
        } else {
            match inserted {
                Some(inserted) => tasks::register_task_writes_with_stop_observation_tx(
                    tx,
                    &self.proof,
                    &actual,
                    &final_state,
                    &self.request,
                    ledger,
                    memory_budget.as_deref_mut(),
                    inserted,
                )?,
                None => tasks::register_task_writes_with_memory_budget_tx(
                    tx,
                    &self.proof,
                    &actual,
                    &final_state,
                    &self.request,
                    ledger,
                    memory_budget.as_deref_mut(),
                )?,
            }
        };
        // Delta owns the selected Arcs and a by-value coverage. These observations
        // outlive SQL and remain with the ACK until authoritative root installation.
        self.guards.push(ledger.register_task_view(
            self.proof.scope(),
            InputMemoryCategory::Candidate,
            facts.delta.records.iter(),
        )?);
        self.guards.push(ledger.register_task_stop_view(
            self.proof.scope(),
            InputMemoryCategory::Candidate,
            facts.delta.stop_records.iter(),
        )?);
        self.guards.push(ledger.register_owned_bytes(
            self.proof.scope(),
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: serialized_bytes(&facts.delta.coverage)?,
            },
        )?);
        value.effects.task_delta = Some(facts.delta);
        value.effects.task_delta_requires_prepared_domain = true;
        value.effects.task_material_budget_views.extend(self.guards);
        Ok(tasks::TaskMaintenanceMaterial {
            bytes: self.material_total.0,
            records: self.material_total.1,
        })
    }
}

pub(super) fn prepare_supervised_parent_transaction_tx<'tx, 'connection, 'command: 'tx>(
    tx: &'tx Transaction<'connection>,
    prepared: &'command PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Option<TaskTransaction<'tx, 'connection, 'command>>> {
    let InputTransitionCommand::Queue(queue) = prepared.command.as_ref() else {
        return Ok(None);
    };
    let Some(admission) = &queue.supervised_invocation else {
        return Ok(None);
    };
    let scope = &admission.parent_scope;
    crate::runtime_db::input_evidence_writes::validate_input_evidence_scope_tx(
        tx, scope, &admission.parent_task.agent_id,
    )?;
    let allowed = BTreeSet::from([TaskKey::new(TaskFamily::Task, &admission.parent_task.id)]);
    let mut capture = crate::runtime::resident_task::TaskCaptureRequest::current();
    capture.required_keys = allowed.clone();
    let request = tasks::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture,
        byte_limit: request.byte_limit,
        record_limit: request.record_limit,
    };
    let mut material = Material {
        scope,
        request: &request,
        ledger,
        memory_budget,
        bytes: 0,
        records: 0,
        guards: Vec::new(),
    };
    let before = tasks::task_maintenance_material_tx(tx, scope, &allowed)?;
    material.charge(before.bytes, before.records, 1)?;
    let state_material = state_material_tx(tx, scope)?;
    material.charge(state_material, 1, 1)?;
    material.keys(&allowed, 4)?;
    for message in &queue.command.message_evidence {
        material.message(tx, message)?;
    }
    let proof = tasks::prepare_task_write_tx(tx, scope, &allowed)?;
    tasks::preflight_task_write_snapshot_with_memory_budget_tx(
        tx, &proof, &request, ledger, material.memory_budget.take(),
    )?.require_current()?;
    let original_material = (before.bytes, before.records);
    let material_total = (material.bytes, material.records);
    let guards = material.guards;
    Ok(Some(TaskTransaction {
        proof,
        cleanup: None,
        allowed,
        observations: BTreeSet::new(),
        result: None,
        predicate: None,
        request,
        guards,
        original_material,
        material_total,
        original_state_material: state_material,
        child_phase: false,
    }))
}

pub(super) fn finish_supervised_parent_tx(
    tx: &Transaction<'_>,
    tasks: TaskTransaction<'_, '_, '_>,
    value: &mut TransitionCommit,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<()> {
    // Keep the related writer's actual keys out of the child namespace.
    let mut parent = TransitionCommit {
        applied: true,
        effects: Box::new(PostCommitEffects {
            task_write_keys: tasks.allowed.clone(),
            ..Default::default()
        }),
        ..Default::default()
    };
    tasks.finish(tx, &mut parent, ledger, memory)?;
    value.effects.related_task_deltas.push(
        parent.effects.task_delta.take().context("supervised parent omitted Task delta")?,
    );
    value.effects.task_material_budget_views.extend(parent.effects.task_material_budget_views);
    Ok(())
}

pub(super) fn prepare_task_transaction_tx<'tx, 'connection, 'command: 'tx>(
    tx: &'tx Transaction<'connection>,
    prepared: &'command PreparedInputTransition,
    scope: &InputEvidenceScope,
    deleting: bool,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Option<TaskTransaction<'tx, 'connection, 'command>>> {
    prepare_task_transaction_inner_tx(
        tx,
        prepared,
        scope,
        deleting,
        request,
        ledger,
        memory_budget,
        None,
    )
}
pub(super) fn prepare_child_task_transaction_tx<'tx, 'connection, 'command: 'tx>(
    tx: &'tx Transaction<'connection>,
    prepared: &'command PreparedInputTransition,
    scope: &InputEvidenceScope,
    deleting: bool,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    child_entry: super::child_terminal::ChildCurrentEntryMaterial,
) -> Result<Option<TaskTransaction<'tx, 'connection, 'command>>> {
    prepare_task_transaction_inner_tx(
        tx,
        prepared,
        scope,
        deleting,
        request,
        ledger,
        memory_budget,
        Some(child_entry),
    )
}
pub(super) fn prepare_task_transaction_inner_tx<'tx, 'connection, 'command: 'tx>(
    tx: &'tx Transaction<'connection>,
    prepared: &'command PreparedInputTransition,
    scope: &InputEvidenceScope,
    deleting: bool,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    mut memory_budget: Option<
        &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
    >,
    child_entry: Option<super::child_terminal::ChildCurrentEntryMaterial>,
) -> Result<Option<TaskTransaction<'tx, 'connection, 'command>>> {
    let protocol = match prepared.command.as_ref() {
        InputTransitionCommand::Task(task) => Some(&task.execution_protocol),
        InputTransitionCommand::Queue(queue) => Some(&queue.execution_protocol),
        InputTransitionCommand::Work {
            execution_protocol, ..
        }
        | InputTransitionCommand::Focus {
            execution_protocol, ..
        }
        | InputTransitionCommand::Wait {
            execution_protocol, ..
        } => Some(execution_protocol),
        _ => None,
    };
    let has_attempt = protocol.is_some_and(|p| {
        p.bootstrap.as_ref().is_some_and(|p| !p.attempts.is_empty())
            || p.commands.iter().any(|c| attempt_id(c).is_some())
    });
    let has_result = matches!(
        prepared.command.as_ref(),
        InputTransitionCommand::TaskResult(_)
            | InputTransitionCommand::LiveInputAttachment(_)
            | InputTransitionCommand::LiveInputRecovery(_)
            | InputTransitionCommand::ProviderResultHandoff(_)
    ) || matches!(prepared.command.as_ref(),InputTransitionCommand::Queue(q)
        if q.task_result_settlement.is_some());
    if !matches!(prepared.command.as_ref(), InputTransitionCommand::Task(_))
        && !has_attempt
        && !has_result
    {
        return Ok(None);
    }
    let mut material = Material {
        scope,
        request,
        ledger,
        memory_budget: memory_budget.as_deref_mut(),
        bytes: 0,
        records: 0,
        guards: Vec::new(),
    };
    anyhow::ensure!(
        request.record_limit > 0 && request.byte_limit > 0,
        "zero Task writer limits"
    );
    let child_phase = prepared.original_child_terminal().is_some();
    if let Some(original) = prepared.original_child_terminal() {
        let observed = original.material_estimate();
        // Source bodies already have actual completion/selector guards. They are
        // debited once from this Task and Memory allowance before any Task decode.
        material.absorb_predicate(observed)?;
        if let Some(memory) = &mut material.memory_budget {
            memory.additional_material(observed.bytes, observed.records, 1)?;
        }
    }
    if let Some(binding) = prepared.original_child_monitor_binding() {
        let observed = binding.material_estimate();
        material.absorb_predicate(observed)?;
        if let Some(memory) = &mut material.memory_budget {
            memory.additional_material(observed.bytes, observed.records, 1)?;
        }
    }
    if let Some(entry) = child_entry {
        anyhow::ensure!(
            child_phase || prepared.original_child_monitor_binding().is_some(),
            "child scalar material entered another writer"
        );
        material.absorb_predicate(entry.usage)?;
        if let Some(memory) = &mut material.memory_budget {
            memory.additional_material(entry.usage.bytes, entry.usage.records, 1)?;
        }
        material.guards.extend(entry.guards);
    }
    // Request and command address copies are owned before the first selector clone.
    let mut address_bytes = 256usize;
    let mut address_count = 1usize;
    if let InputTransitionCommand::Task(task) = prepared.command.as_ref() {
        address_bytes = address_bytes
            .checked_add(task.command.task.id.len())
            .context("Task command address overflow")?;
    }
    if let Some(protocol) = protocol {
        for command in &protocol.commands {
            if let Some(id) = attempt_id(command) {
                address_bytes = address_bytes
                    .checked_add(id.len() + 128)
                    .context("Task activation address overflow")?;
                address_count = address_count
                    .checked_add(1)
                    .context("Task activation count overflow")?;
            }
        }
        if let Some(state) = &protocol.bootstrap {
            for id in state.attempts.keys() {
                address_bytes = address_bytes
                    .checked_add(id.len() + 128)
                    .context("Task bootstrap address overflow")?;
                address_count = address_count
                    .checked_add(1)
                    .context("Task bootstrap count overflow")?;
            }
        }
    }
    material.charge(address_bytes, address_count, 4)?;
    material.charge(
        usize::try_from(serialized_bytes(&request.capture.required_keys)?)?,
        request.capture.required_keys.len(),
        4,
    )?;
    material.charge(
        usize::try_from(serialized_bytes(&request.query)?)?,
        request.query.selector_record_count()?,
        8,
    )?;
    material.checkpoint(tx)?;
    let original_state_material = state_material_tx(tx, scope)?;
    material.charge(original_state_material, 8, 1)?;
    let limits = result::TaskResultWriteLimits {
        record_limit: request.record_limit,
        byte_limit: request.byte_limit,
    };
    if let InputTransitionCommand::Task(task) = prepared.command.as_ref() {
        for message in &task.command.message_evidence {
            material.message(tx, message)?;
        }
    }
    let operation = match prepared.command.as_ref() {
        InputTransitionCommand::TaskResult(command) => {
            material.charge(
                usize::try_from(serialized_bytes(&command.operation)?)?,
                1,
                8,
            )?;
            use super::super::prepared_task_result::TaskResultOperation as C;
            let operation = match &command.operation {
                C::EnsurePending {
                    task_id,
                    message,
                    at,
                } => {
                    material.message(tx, message)?;
                    material.result(result::ensure_pending_material_estimate_tx(
                        tx,
                        task_id,
                        &message.id,
                    )?)?;
                    result::prepare_ensure_pending_tx(tx, task_id, message, *at, limits)?
                }
                C::SettleInvalidOnly { message_id, .. } => {
                    material.result(result::settle_invalid_only_material_estimate_tx(
                        tx,
                        scope.agent_id.as_str(),
                        message_id,
                    )?)?;
                    command.selection_tx(tx, limits)?.materialize(limits)?
                }
                _ => {
                    let selection = command.selection_tx(tx, limits)?;
                    material.result(selection.material_estimate())?;
                    selection.materialize(limits)?
                }
            };
            Some(operation)
        }
        InputTransitionCommand::Task(task) => match &task.command.task_result_settlement {
            Some(record) => {
                material.result(result::upsert_pending_material_estimate_tx(
                    tx,
                    &task.command.task,
                    record,
                )?)?;
                Some(result::prepare_upsert_pending_tx(
                    tx,
                    &task.command.task,
                    record,
                    limits,
                )?)
            }
            None => None,
        },
        InputTransitionCommand::Queue(queue) => match &queue.task_result_settlement {
            Some(settlement) => {
                let (count,bytes):(usize,usize)=tx.query_row(
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(result_identity AS BLOB))+length(CAST(task_id AS BLOB))+256),0) FROM task_result_settlements WHERE agent_id=?1 AND activation_id=?2 AND state='caller_admitted'",
                    params![scope.agent_id,settlement.activation_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
                material.charge(bytes, count, 4)?;
                let selection = result::select_settle_activation_tx(
                    tx,
                    &scope.agent_id,
                    &settlement.activation_id,
                    settlement.disposition,
                    settlement.settled_at,
                    limits,
                )?;
                material.result(selection.material_estimate())?;
                Some(selection.materialize(limits)?)
            }
            None => None,
        },
        _ => None,
    };
    let needs_predicate = match prepared.command.as_ref() {
        InputTransitionCommand::TaskResult(command) => match command.operation {
            super::super::prepared_task_result::TaskResultOperation::AdmitSnapshot { .. }
            | super::super::prepared_task_result::TaskResultOperation::AdmitMessage { .. }
            | super::super::prepared_task_result::TaskResultOperation::SettleInvalidOnly {
                ..
            } => true,
            #[cfg(test)]
            super::super::prepared_task_result::TaskResultOperation::AdmitUnsettled { .. }
            | super::super::prepared_task_result::TaskResultOperation::AdmitReply { .. } => true,
            _ => false,
        },
        _ => false,
    };
    let predicate = if needs_predicate {
        let remaining = material.remaining()?;
        let proof = tasks::prepare_original_result_predicate_tx(
            tx,
            scope,
            operation
                .as_ref()
                .context("original admit omitted closed Result selection")?,
            &remaining,
            ledger,
            material.memory_budget.as_deref_mut(),
        )?;
        material.absorb_predicate(proof.material_estimate())?;
        Some(proof)
    } else {
        None
    };
    let mut candidates = BTreeSet::new();
    let (address_bytes, address_count) =
        super::live_input::result_address_material_tx(tx, prepared)?;
    material.charge(address_bytes, address_count, 4)?;
    candidates.extend(super::live_input::result_keys_tx(tx, prepared)?);
    if let Some(operation) = &operation {
        candidates.extend(
            operation
                .candidate_result_ids()
                .iter()
                .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
        );
    }
    if matches!(
        prepared.command.as_ref(),
        InputTransitionCommand::TaskResult(_)
    ) {
        if let Some(operation) = &operation {
            for id in operation.candidate_result_ids() {
                let agent: Option<String> = tx
                    .query_row(
                        "SELECT agent_id FROM task_result_settlements WHERE result_identity=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                anyhow::ensure!(
                    agent.as_deref().is_none_or(|agent| agent == scope.agent_id),
                    "original TaskResult target belongs to another namespace"
                );
            }
            candidates.extend(
                operation
                    .dependency_task_ids()
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::Task, id)),
            );
        }
    }
    if let InputTransitionCommand::Task(task) = prepared.command.as_ref() {
        candidates.insert(TaskKey::new(TaskFamily::Task, &task.command.task.id));
    }
    if let Some(protocol) = protocol {
        // Only affected original attempt addresses, not the latest protocol tail.
        let mut activations = BTreeSet::new();
        for command in &protocol.commands {
            if let Some(id) = attempt_id(command) {
                activations.insert(id);
            }
        }
        if let Some(state) = &protocol.bootstrap {
            activations.extend(state.attempts.keys().map(String::as_str));
        }
        for id in activations {
            let (count,bytes):(usize,usize)=tx.query_row(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(result_identity AS BLOB))+128),0) FROM task_result_settlements WHERE agent_id=?1 AND activation_id=?2",
                params![scope.agent_id,id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            material.charge(bytes, count, 4)?;
            let mut query=tx.prepare("SELECT result_identity FROM task_result_settlements WHERE agent_id=?1 AND activation_id=?2 ORDER BY result_identity")?;
            for id in query.query_map(params![scope.agent_id, id], |r| r.get::<_, String>(0))? {
                candidates.insert(TaskKey::new(TaskFamily::ResultSettlement, id?));
            }
        }
    }
    material.keys(&candidates, 4)?;
    let before_material = tasks::task_maintenance_material_tx(tx, scope, &candidates)?;
    material.charge(before_material.bytes, before_material.records, 1)?;
    let mut allowed = tasks::qualified_task_recovery_selectors_tx(tx, scope, &candidates)?;
    if matches!(prepared.command.as_ref(), InputTransitionCommand::LiveInputAttachment(_)
        | InputTransitionCommand::LiveInputRecovery(_)
        | InputTransitionCommand::ProviderResultHandoff(_)) {
        anyhow::ensure!(
            candidates.iter().filter(|key| key.family == TaskFamily::ResultSettlement)
                .all(|key| allowed.contains(key)),
            "live Result source requires original Task preparation"
        );
    }
    let mut observations = BTreeSet::new();
    for key in &candidates {
        let exists = tasks::checkpoint_material_size_on(tx, key)?.is_some();
        if exists {
            if matches!(
                prepared.command.as_ref(),
                InputTransitionCommand::TaskResult(_)
            ) && key.family == TaskFamily::Task
            {
                if allowed.contains(key) {
                    observations.insert(key.clone());
                }
            } else {
                observations.insert(key.clone());
            }
        } else if let InputTransitionCommand::TaskResult(command) = prepared.command.as_ref() {
            if key.family == TaskFamily::ResultSettlement {
                anyhow::ensure!(
                    !deleting,
                    "Deleting first Result requires original Task/Control cleanup capability"
                );
                let super::super::prepared_task_result::TaskResultOperation::EnsurePending {
                    task_id,
                    message,
                    ..
                } = &command.operation
                else {
                    anyhow::bail!("TaskResult operation cannot create a new key")
                };
                anyhow::ensure!(
                    allowed.contains(&TaskKey::new(TaskFamily::Task, task_id)),
                    tasks::TaskSnapshotError::NeedsPreparation(TaskKey::new(
                        TaskFamily::Task,
                        task_id
                    ))
                );
                let input = crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    &message.id,
                );
                let size:usize=tx.query_row("SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE message_id=?1 AND agent_id=?2",
                    params![message.id,scope.agent_id],|r|r.get(0))?;
                material.charge(size, 1, 4)?;
                crate::runtime_db::input_evidence_snapshot::validate_existing_input_evidence_tx(
                    tx, scope, &input,
                )?;
                let actual =
                    crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                        .message_by_id(&message.id)?
                        .context("TaskResult original source is missing")?;
                anyhow::ensure!(
                    actual == **message,
                    "TaskResult frozen source differs from actual qualified Message"
                );
                allowed.insert(key.clone());
            }
        } else if !deleting {
            // Only the typed Task/pending Result intent may initially create a key.
            if let InputTransitionCommand::Task(task) = prepared.command.as_ref() {
                if (key.family == TaskFamily::Task && key.id == task.command.task.id)
                    || task
                        .command
                        .task_result_settlement
                        .as_ref()
                        .is_some_and(|r| {
                            key.family == TaskFamily::ResultSettlement
                                && key.id == r.result_identity
                        })
                {
                    allowed.insert(key.clone());
                }
            }
        }
    }
    let cleanup = if deleting
        && matches!(prepared.command.as_ref(),InputTransitionCommand::Task(task)
        if task.command.task_result_settlement.is_some())
    {
        let pending = operation
            .as_ref()
            .context("original cleanup omitted closed pending")?;
        let extra =
            tasks::original_task_result_cleanup_material_tx(tx, scope, &candidates, pending)?;
        material.charge(extra.bytes, extra.records, 1)?;
        let existing = tasks::prepare_existing_task_maintenance_tx(tx, scope, &candidates)?;
        let cap = tasks::prepare_original_task_result_cleanup_tx(
            tx,
            existing,
            tasks::OriginalTaskResultCleanupSource::Task(prepared.original_task_cleanup()?),
            pending,
        )?;
        Some(cap)
    } else {
        None
    };
    material.keys(&allowed, 4)?;
    material.keys(&observations, 4)?;
    let proof = if deleting {
        tasks::prepare_original_task_cleanup_tx(tx, scope, &allowed)?
    } else {
        tasks::prepare_task_write_tx(tx, scope, &allowed)?
    };
    if let Some(cap) = &cleanup {
        allowed.insert(cap.result_key());
    }
    let final_request = request.clone();
    let memory = material.memory_budget.take();
    if child_phase {
        let mut phase =
            tasks::read_preparation::TaskReadMaterial::new(scope, &final_request, Some(ledger))
                .with_memory_budget(memory)
                .with_admitted_material(tasks::TaskMaintenanceMaterial {
                    bytes: material.bytes,
                    records: material.records,
                })?;
        tasks::load_selected_using_material(
            tx,
            scope,
            &final_request,
            false,
            deleting,
            true,
            Some(ledger),
            None,
            None,
            &mut phase,
        )?
        .require_current()?;
        let usage = phase.material_estimate();
        material.bytes = usage.bytes;
        material.records = usage.records;
        material.guards.extend(phase.into_guards());
    } else {
        tasks::preflight_task_write_snapshot_with_memory_budget_tx(
            tx,
            &proof,
            &final_request,
            ledger,
            memory,
        )?
        .require_current()?;
    }
    Ok(Some(TaskTransaction {
        proof,
        cleanup,
        allowed,
        observations,
        result: operation,
        predicate,
        request: final_request,
        original_material: (before_material.bytes, before_material.records),
        material_total: (material.bytes, material.records),
        original_state_material,
        child_phase,
        guards: material.guards,
    }))
}
fn attempt_id(
    command: &crate::domain::execution_protocol::ExecutionProtocolCommand,
) -> Option<&str> {
    use crate::domain::execution_protocol::ExecutionProtocolCommand as C;
    match command {
        C::Admit(c) => Some(&c.attempt.attempt_id),
        C::Settle(c) => Some(&c.outcome.attempt_id),
        C::Interrupt(c) => Some(&c.attempt_id),
        C::RecoverInterruptedTaskResultClaim(c) => Some(&c.attempt_id),
        C::RecoverUnadvancedTaskResultClaim(c) => Some(&c.attempt_id),
        _ => None,
    }
}

fn state_material_tx(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<usize> {
    let bytes:usize=tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)",
        [&scope.agent_id],|r|r.get(0))?;
    bytes.checked_mul(8).context("Task State material overflow")
}

/// Reuses the original writer's material predicates; no qualification/authority.
pub(super) fn prepare_stop_result_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    task: &crate::types::TaskRecord,
    result_record: Option<&result::TaskResultSettlementRecord>,
    message: &MessageEnvelope,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Vec<Arc<BudgetViewGuard>>> {
    let mut material = Material {
        scope,
        request,
        ledger,
        memory_budget,
        bytes: 0,
        records: 0,
        guards: Vec::new(),
    };
    let mut keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
    if let Some(result) = result_record {
        keys.insert(TaskKey::new(
            TaskFamily::ResultSettlement,
            &result.result_identity,
        ));
    }
    material.keys(&keys, 8)?;
    material.message(tx, message)?;
    material.checkpoint(tx)?;
    if let Some(result_record) = result_record {
        material.result(result::upsert_pending_material_estimate_tx(
            tx,
            task,
            result_record,
        )?)?;
    }
    let maintenance = tasks::task_maintenance_material_tx(tx, scope, &keys)?;
    material.charge(maintenance.bytes, maintenance.records, 1)?;
    material.charge(state_material_tx(tx, scope)?, 1, 1)?;
    Ok(material.guards)
}

/// Reuse the original exact Message material preflight; this grants no Task role.
pub(super) fn prepare_timer_message_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message: &MessageEnvelope,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<(Vec<Arc<BudgetViewGuard>>, usize, usize)> {
    let mut material = Material {
        scope,
        request,
        ledger,
        memory_budget,
        bytes: 0,
        records: 0,
        guards: Vec::new(),
    };
    material.message(tx, message)?;
    Ok((material.guards, material.bytes, material.records))
}
