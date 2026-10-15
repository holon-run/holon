//! Domain repository implementations and their transaction helpers.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, OptionalExtension, ToSql, Transaction};
use sha2::{Digest, Sha256};

use crate::runtime_db::agent_relations::{
    canonical_relations_from_connection, transition_supervision_state_tx,
    upsert_canonical_record_set_tx,
};
use crate::runtime_db::conversation::{
    assign_input_to_turn_tx, assigned_turn_id_tx, bump_source_revision_tx, bump_turn_revision_tx,
    ensure_turn_revision_tx, settle_turn_result_tx, SOURCE_OPERATOR, SOURCE_WAIT,
};
use crate::runtime_db::current_read_snapshot::CurrentReadBusinessTransaction;
use crate::runtime_db::evidence::*;
use crate::runtime_db::index_outbox::RuntimeIndexChange;
use crate::runtime_db::transitions::{
    inject_fault, trigger_wait_and_enqueue_tx, trigger_wait_and_enqueue_with_current_read_tx,
};
use crate::runtime_db::types::*;
use crate::runtime_db::write_queue::RuntimeDbWriteContext;
use crate::runtime_db::{
    RuntimeStateTransitionConflict, CONTEXT_EPISODE_ANCHORS_DOMAIN, TASK_PAYLOAD_ARRAY_LIMIT,
    TASK_PAYLOAD_STRING_LIMIT,
};
use crate::types::*;

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) enum AgentDeletionAdmission {
    ExpectedRevision(u64),
    SupervisingParent {
        parent_agent_id: String,
        expected_incarnation: u64,
        parent_incarnation: u64,
    },
    TerminalEphemeralChild {
        parent_agent_id: String,
        task_id: String,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AgentDeletionRequest {
    pub agent_id: String,
    pub admission: AgentDeletionAdmission,
    pub requested_by: String,
    pub cascade_private_children: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct LegacyDeletionScanBatch {
    pub identities: Vec<AgentIdentityRecord>,
    start_cursor: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("terminal child deletion admission rejected for agent {agent_id}: {reason}")]
pub(crate) struct TerminalChildDeletionAdmissionRejected {
    agent_id: String,
    reason: String,
}

impl TerminalChildDeletionAdmissionRejected {
    pub(crate) fn new(agent_id: &str, reason: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            reason: reason.into(),
        }
    }
}

pub(crate) fn is_terminal_child_deletion_admission_rejected(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<TerminalChildDeletionAdmissionRejected>()
            .is_some()
    })
}

impl WorkItemRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkItemRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("work_items", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_items", "jsonl", "db", |tx| {
                let mut latest = BTreeMap::<String, WorkItemRecord>::new();
                for record in records {
                    let should_replace = latest
                        .get(&record.id)
                        .is_none_or(|existing| newer_work_item_record(&record, existing));
                    if should_replace {
                        latest.insert(record.id.clone(), record);
                    }
                }
                let mut work_writes = super::episode_snapshot::work_checkpoint::imports::ImportWrites::default();
                for record in latest.values() {
                    let origin = match import_work_item_tx(tx, record)? {
                        WorkImportOutcome::ActualWrite => Some(super::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite),
                        WorkImportOutcome::LegacyEqual => Some(super::episode_snapshot::work_checkpoint::imports::ImportOrigin::LegacyImportEqual),
                        WorkImportOutcome::Ignored => None,
                    };
                    if let Some(origin) = origin { work_writes.record("work_item", &record.id, &record.agent_id, origin)?; }
                }
                Ok(serde_json::json!({ "imported_records": latest.len(), "work_domain_actual_writes": work_writes.finish(tx)? }))
            })
    }

    pub fn insert_new(&self, record: &WorkItemRecord) -> Result<bool> {
        self.db
            .transaction(|tx| insert_new_work_item_tx(tx, record))
    }

    pub fn insert_new_with_index_changes(
        &self,
        record: &WorkItemRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            let inserted = insert_new_work_item_tx(tx, record)?;
            if inserted {
                insert_runtime_index_changes_tx(tx, changes)?;
            }
            Ok(inserted)
        })
    }

    pub fn update_expected(&self, record: &WorkItemRecord, expected_revision: u64) -> Result<bool> {
        self.db
            .transaction(|tx| update_expected_work_item_tx(tx, record, expected_revision))
    }

    pub fn update_expected_with_index_changes(
        &self,
        record: &WorkItemRecord,
        expected_revision: u64,
        changes: &[RuntimeIndexChange],
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            let updated = update_expected_work_item_tx(tx, record, expected_revision)?;
            if updated {
                insert_runtime_index_changes_tx(tx, changes)?;
            }
            Ok(updated)
        })
    }

    pub fn latest(&self, work_item_id: &str) -> Result<Option<WorkItemRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
                [work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<WorkItemRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }

    /// All open work items for one agent. Open items are scheduler candidates,
    /// so this set is naturally bounded by live scheduling state.
    pub fn open_for_agent(&self, agent_id: &str) -> Result<Vec<WorkItemRecord>> {
        let connection = self.db.connection()?;
        context_open_work_items_on(&connection, agent_id)
    }

    /// Most recent non-open work items for one agent, bounded by `limit`.
    /// Projections only surface a small window of terminal history.
    pub fn latest_non_open_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.db.connection()?;
        context_non_open_work_items_on(&connection, agent_id, limit)
    }

    /// Batched lookup of work items by id, replacing per-id connections on
    /// projection paths. Ids without a row are silently absent.
    pub fn latest_many(&self, work_item_ids: &[String]) -> Result<Vec<WorkItemRecord>> {
        let mut records = Vec::with_capacity(work_item_ids.len());
        for chunk in work_item_ids.chunks(128) {
            let placeholders = (0..chunk.len()).map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT payload_json FROM work_items WHERE work_item_id IN ({placeholders})"
            );
            let connection = self.db.connection()?;
            let mut statement = connection.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            let rows = statement.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
            records.extend(
                rows.map(|row| decode_work_item_payload(&row?))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        Ok(records)
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn due_blocked_rechecks(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<WorkItemRecord>> {
        let now_str = timestamp(now);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             WHERE agent_id = ?1
               AND state = 'open'
               AND blocked_by IS NOT NULL
               AND recheck_at IS NOT NULL
               AND recheck_at <= ?2
               AND (recheck_consumed_at IS NULL OR recheck_consumed_at < recheck_at)
             ORDER BY recheck_at ASC",
        )?;
        let rows =
            statement.query_map(params![agent_id, now_str], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }

    pub fn next_recheck_at(&self, agent_id: &str) -> Result<Option<DateTime<Utc>>> {
        let connection = self.db.connection()?;
        let result: Option<String> = connection
            .query_row(
                "SELECT MIN(recheck_at)
                 FROM work_items
                 WHERE agent_id = ?1
                   AND state = 'open'
                   AND blocked_by IS NOT NULL
                   AND recheck_at IS NOT NULL
                   AND (recheck_consumed_at IS NULL OR recheck_consumed_at < recheck_at)",
                [agent_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        result
            .map(|s| parse_timestamp(&s).context("parsing recheck_at from work_items query"))
            .transpose()
    }
}

impl AgentStateRepository<'_> {
    pub fn import_legacy(&self, record: Option<AgentState>) -> Result<()> {
        self.import_legacy_many(record.into_iter().collect())
    }

    /// First Host cutover imports all fixed scoped JSON roots before global completion.
    pub(crate) fn import_legacy_many(&self, records: Vec<AgentState>) -> Result<()> {
        if self.db.storage_domain_is_complete("agent_states", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("agent_states", "json", "db", |tx| {
                for record in &records {
                    upsert_agent_state_tx(tx, record)?;
                    super::episode_snapshot::prepare_imported_state_continuity_tx(
                        tx,
                        record,
                        Utc::now(),
                    )?;
                }
                Ok(serde_json::json!({ "imported_records": records.len() }))
            })
    }

    /// Includes orphan rows whose agent identity has not been materialized.
    pub(crate) fn startup_mutation_agent_ids(&self) -> Result<Vec<String>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT agent_id FROM agent_identities
             UNION SELECT agent_id FROM agent_states
             UNION SELECT agent_id FROM queue_entries
             UNION SELECT agent_id FROM turn_records
             UNION SELECT agent_id FROM execution_protocol_attempts
             UNION SELECT agent_id FROM wait_conditions
             UNION SELECT owner_agent_id FROM tasks
             UNION SELECT agent_id FROM timers
             UNION SELECT target_agent_id FROM external_triggers
             ORDER BY 1",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The Host reserves all startup scopes and excludes loaded/draining
    /// runtimes. Stale waits and Stopped timer/queue/state cleanup share one ACK.
    #[cfg(test)]
    pub(crate) fn reconcile_stale_waits_at_startup(
        &self,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<(usize, BTreeMap<String, AgentState>)> {
        self.reconcile_stale_waits_at_startup_inner(None, None, fault)
            .map(|(n, states, _)| (n, states))
    }

    #[cfg(test)]
    pub(crate) fn reconcile_stale_waits_at_startup_in_scopes(
        &self,
        scopes: &[crate::runtime_db::transitions::StartupInputRecoveryScope],
        admission: &super::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery<
            '_,
        >,
        request: &super::work_snapshot::WorkDomainLoadRequest,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<(usize, BTreeMap<String, AgentState>)> {
        admission.validate_database(self.db)?;
        self.reconcile_stale_waits_at_startup_inner(Some((scopes, admission, request)), None, fault)
            .map(|(n, states, _)| (n, states))
    }

    pub(crate) fn reconcile_stale_waits_at_startup_with_scheduling(
        &self,
        scopes: &[crate::runtime_db::transitions::StartupInputRecoveryScope],
        admission: &super::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery<
            '_,
        >,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        scheduling_request: &super::scheduling_snapshot::SchedulingDomainLoadRequest,
        accounting: &[crate::runtime::resident_input_evidence::InputEvidenceScope],
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<(
        usize,
        BTreeMap<String, AgentState>,
        super::scheduling_snapshot::StartupSchedulingAck,
    )> {
        admission.validate_database(self.db)?;
        let (count, states, ack) = self.reconcile_stale_waits_at_startup_inner(
            Some((scopes, admission, work_request)),
            Some((scheduling_request, accounting, ledger)),
            fault,
        )?;
        Ok((
            count,
            states,
            ack.context("startup Scheduling ACK missing")?,
        ))
    }

    fn reconcile_stale_waits_at_startup_inner(
        &self,
        preparation: Option<(
            &[crate::runtime_db::transitions::StartupInputRecoveryScope],
            &super::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery<'_>,
            &super::work_snapshot::WorkDomainLoadRequest,
        )>,
        scheduling: Option<(
            &super::scheduling_snapshot::SchedulingDomainLoadRequest,
            &[crate::runtime::resident_input_evidence::InputEvidenceScope],
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<(
        usize,
        BTreeMap<String, AgentState>,
        Option<super::scheduling_snapshot::StartupSchedulingAck>,
    )> {
        use super::{
            input_evidence_snapshot::legacy_recovery::legacy_seed_recovery_scope_tx, work_snapshot,
        };
        use crate::runtime::resident_input_evidence::InputEvidenceScope;
        use crate::runtime_db::transitions::{StartupInputRecoveryScope, TransitionFaultPoint};

        self.db.transaction(|tx| {
            let scheduling_material=if let Some((request,accounting,ledger))=scheduling {
                let (scopes,admission,_)=preparation.context("Scheduling startup lacks original exclusive actor")?;
                Some(super::scheduling_snapshot::StartupSchedulingMaterial::prepare_tx(tx,scopes,accounting,request,ledger,admission)?)
            }else{None};
            let mut states = {
                let mut statement = tx.prepare("SELECT payload_json FROM agent_states")?;
                let records = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .map(|row| decode_agent_state_payload(&row?))
                    .collect::<Result<Vec<_>>>()?;
                records.into_iter().map(|state| (state.id.clone(), state)).collect::<BTreeMap<_, _>>()
            };
            let waits = {
                let mut statement = tx.prepare(
                    "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                        subject_ref, waiting_for, created_at, updated_at, expires_at,
                        resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                        wake_sources_json, continuation_json
                     FROM wait_conditions WHERE status IN ('active', 'triggered')",
                )?;
                let records = statement.query_map([], |row| {
                    decode_wait_condition_row(row).map_err(wait_condition_decode_error)
                })?.collect::<std::result::Result<Vec<_>, _>>()?;
                records
            };
            let tasks = {
                let mut statement = tx.prepare(if scheduling.is_some(){super::scheduling_snapshot::startup_maintenance::WAIT_TASK_POOL}else{"SELECT payload_json FROM tasks"})?;
                let records = statement.query_map([], |row| row.get::<_, String>(0))?
                    .map(|row| decode_task_payload(&row?)).collect::<Result<Vec<_>>>()?;
                records
            };
            let external_triggers = {
                let mut statement = tx.prepare(if scheduling.is_some(){super::scheduling_snapshot::startup_maintenance::WAIT_TRIGGER_POOL}else{"SELECT payload_json FROM external_triggers"})?;
                let records = statement.query_map([], |row| row.get::<_, String>(0))?
                    .map(|row| decode_external_trigger_payload(&row?)).collect::<Result<Vec<_>>>()?;
                records
            };
            let timers = {
                let mut statement = tx.prepare(if scheduling.is_some(){super::scheduling_snapshot::startup_maintenance::WAIT_TIMER_POOL}else{"SELECT payload_json FROM timers"})?;
                let records = statement.query_map([], |row| row.get::<_, String>(0))?
                    .map(|row| decode_timer_payload(&row?)).collect::<Result<Vec<_>>>()?;
                records
            };
            let mut stale_waits = Vec::new();
            for wait in waits {
                let stopped = states.get(&wait.agent_id)
                    .is_some_and(|state| state.status == AgentStatus::Stopped);
                let task_stale = wait.wake_sources.iter().any(|source| {
                    let WakeSource::TaskResult { task_id } = source else { return false; };
                    tasks.iter().find(|task| task.id == *task_id && task.agent_id == wait.agent_id)
                        .is_none_or(|task| matches!(task.status,
                            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted))
                });
                let external_stale = wait.wake_sources.iter().any(|source| {
                    let WakeSource::ExternalIngress { external_trigger_id } = source else { return false; };
                    external_trigger_id.as_ref().is_none_or(|trigger_id| {
                        !external_triggers.iter().any(|trigger| {
                            trigger.external_trigger_id == *trigger_id
                                && trigger.target_agent_id == wait.agent_id
                                && trigger.status == ExternalTriggerStatus::Active
                        })
                    })
                });
                let timer_stale = wait.kind == WaitConditionKind::Timer
                    && wait.subject_ref.as_ref().is_none_or(|timer_id| {
                        !timers.iter().any(|timer| timer.id == *timer_id
                            && timer.agent_id == wait.agent_id && timer.status == TimerStatus::Active)
                    });
                if stopped || task_stale || external_stale || timer_stale {
                    stale_waits.push(wait);
                }
            }
            let mut prepared_bound = BTreeMap::new();
            let mut prepared_unbound = BTreeMap::new();
            if let Some((scopes, admission, request)) = preparation {
                let candidate_ids = stale_waits.iter().fold(BTreeMap::<String,BTreeSet<String>>::new(),|mut ids,wait| {
                    ids.entry(wait.agent_id.clone()).or_default().insert(wait.id.clone());ids
                });
                let mut agents=BTreeSet::new();
                for authority in scopes {
                    let agent=authority.agent_id();
                    anyhow::ensure!(agents.insert(agent.to_owned()),"duplicate startup Wait scope");
                    let ids=candidate_ids.get(agent).cloned().unwrap_or_default();
                    match authority {
                        StartupInputRecoveryScope::Current(scope)=>{
                            super::input_evidence_writes::validate_input_evidence_scope_tx(tx,scope,agent)?;
                            if !ids.is_empty() {
                                prepared_bound.insert(agent.to_owned(),work_snapshot::prepare_existing_work_wait_cleanup_tx(tx,scope,&ids,request,false)?);
                            }
                        }
                        StartupInputRecoveryScope::Unbound(scope)=>{
                            anyhow::ensure!(legacy_seed_recovery_scope_tx(tx,agent)?==*scope,"startup Wait seed proof changed");
                            if !ids.is_empty() {
                                prepared_unbound.insert(agent.to_owned(),work_snapshot::prepare_unbound_work_wait_cleanup_tx(tx,scope,&ids,request,admission)?);
                            }
                        }
                        StartupInputRecoveryScope::Retired{identity_incarnation,status,..}=>{
                            anyhow::ensure!(*identity_incarnation>0 && *status!=crate::types::AgentRegistryStatus::Active,"invalid retired startup Wait scope");
                            let (normalized,payload):(String,String)=tx.query_row("SELECT status,payload_json FROM agent_identities WHERE agent_id=?1",[agent],|r|Ok((r.get(0)?,r.get(1)?)))?;
                            let identity=decode_agent_identity_payload(&payload)?;
                            anyhow::ensure!(identity.agent_id==agent && identity.incarnation==*identity_incarnation && identity.status==*status
                                && serde_json::to_value(&identity.status)?.as_str()==Some(normalized.as_str()),"retired startup Wait identity changed");
                            if *status == crate::types::AgentRegistryStatus::Deleting && !ids.is_empty() {
                                let scope=InputEvidenceScope{agent_id:agent.into(),identity_incarnation:*identity_incarnation};
                                prepared_bound.insert(agent.to_owned(),work_snapshot::prepare_existing_work_wait_cleanup_tx(tx,&scope,&ids,request,true)?);
                            }
                        }
                    }
                }
                anyhow::ensure!(candidate_ids.keys().all(|agent|agents.contains(agent))
                    && states.values().filter(|state|state.status==AgentStatus::Stopped).all(|state|agents.contains(&state.id)),
                    "startup Wait cleanup has an unreserved write scope");
            }
            let scheduling_cleanup=if let Some(material)=scheduling_material {
                let (scopes,admission,_)=preparation.context("Scheduling startup preparation missing")?;
                let (_,_,ledger)=scheduling.context("Scheduling startup ledger missing")?;
                Some(material.prepare_stopped_tx(tx,&states,scopes,ledger,admission)?)
            }else{None};
            inject_fault(fault, TransitionFaultPoint::AfterValidation)?;
            let now = Utc::now();
            let cancelled=stale_waits.len();
            let mut actual=BTreeMap::<String,WaitConditionWrites>::new();
            for mut wait in stale_waits {
                wait.status=WaitConditionStatus::Cancelled;
                wait.updated_at=now;
                wait.cancelled_at=Some(now);
                let writes=upsert_wait_condition_with_writes_tx(tx,&wait)?;
                actual.entry(wait.agent_id.clone()).or_default().extend(writes);
            }
            let mut scheduling_actual=BTreeMap::new();
            for state in states.values_mut().filter(|state| state.status == AgentStatus::Stopped) {
                let writes=cancel_active_timers_with_writes_tx(tx,&state.id,now)?;
                scheduling_actual.insert(state.id.clone(),writes);
                let pending = {
                    let mut statement = tx.prepare(
                        "SELECT payload_json FROM queue_entries WHERE agent_id = ?1 AND status IN ('queued', 'interrupted')",
                    )?;
                    let records = statement.query_map([&state.id], |row| row.get::<_, String>(0))?
                        .map(|row| decode_queue_entry_payload(&row?)).collect::<Result<Vec<_>>>()?;
                    records
                };
                for mut entry in pending {
                    entry.status = QueueEntryStatus::Aborted;
                    entry.updated_at = now;
                    upsert_queue_entry_tx(tx, &entry)?;
                }
                if state.pending != 0 {
                    state.pending = 0;
                    upsert_agent_state_tx(tx, state)?;
                }
            }
            inject_fault(fault, TransitionFaultPoint::AfterCanonicalWrites)?;
            if let Some((_, _, request)) = preparation {
                for (agent, proof) in &prepared_bound {
                    if let Some(writes) = actual.get(agent) {
                        // Host is preactivation. Preserve exact qualification in
                        // this ACK; its next bootstrap builds the resident root.
                        work_snapshot::register_existing_work_wait_cleanup_tx(tx,proof,writes,states.get(agent),request)?;
                    }
                }
                for (agent, proof) in &prepared_unbound {
                    if let Some(writes) = actual.get(agent) {
                        work_snapshot::register_unbound_work_wait_cleanup_tx(tx,proof,writes)?;
                    }
                }
            }
            let scheduling_ack=if let Some(cleanup)=scheduling_cleanup {
                let (_,admission,_)=preparation.context("Scheduling startup original actor missing")?;
                let (_,_,ledger)=scheduling.context("Scheduling startup ledger missing")?;
                Some(cleanup.finish_tx(tx,&scheduling_actual,ledger,admission)?)
            }else{None};
            inject_fault(fault, TransitionFaultPoint::BeforeCommit)?;
            Ok((cancelled, states,scheduling_ack))
        })
    }

    /// Fixture setup only: bypasses live runtime mutation ownership.
    #[doc(hidden)]
    pub fn upsert_for_fixture(&self, record: &AgentState) -> Result<()> {
        self.db.transaction(|tx| upsert_agent_state_tx(tx, record))
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentState>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_agent_state_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<AgentState>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare("SELECT payload_json FROM agent_states")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_agent_state_payload(&row?))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading all agent states: {e}"))
    }
}

impl WorkspaceEntryRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkspaceEntry>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("workspace_entries", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("workspace_entries", "jsonl", "db", |tx| {
                let latest = reduce_workspace_entry_records(records);
                for record in latest.values() {
                    upsert_workspace_entry_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkspaceEntry) -> Result<()> {
        self.db
            .transaction(|tx| upsert_workspace_entry_tx(tx, record))
    }

    pub fn upsert_with_index_changes(
        &self,
        record: &WorkspaceEntry,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            upsert_workspace_entry_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn latest_all(&self) -> Result<Vec<WorkspaceEntry>> {
        let connection = self.db.connection()?;
        context_workspace_entries_on(&connection)
    }
}

impl WorkspaceOccupancyRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkspaceOccupancyRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("workspace_occupancies", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("workspace_occupancies", "jsonl", "db", |tx| {
                let latest = reduce_workspace_occupancy_records(records);
                for record in latest.values() {
                    upsert_workspace_occupancy_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkspaceOccupancyRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_workspace_occupancy_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM workspace_occupancies
             ORDER BY acquired_at DESC, occupancy_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_workspace_occupancy_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl ExecutionRootEntryRepository<'_> {
    pub fn upsert(&self, record: &ExecutionRootEntry) -> Result<()> {
        self.db
            .transaction(|tx| upsert_execution_root_entry_tx(tx, record))
    }

    /// Look up an execution root entry by its `execution_root_id`.
    pub fn get(&self, execution_root_id: &str) -> Result<Option<ExecutionRootEntry>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM execution_root_entries
                 WHERE execution_root_id = ?1",
                [execution_root_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_execution_root_entry_payload(&payload))
            .transpose()
    }

    /// Soft-delete: mark an execution root entry as removed.
    pub fn mark_removed(&self, execution_root_id: &str) -> Result<bool> {
        let now = crate::runtime_db::migrations::timestamp(Utc::now());
        let affected = self.db.transaction(|tx| {
            // Read the current payload, set removed_at, and write it back
            // so that reads via decode_execution_root_entry_payload see the change.
            let payload: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM execution_root_entries
                     WHERE execution_root_id = ?1 AND removed_at IS NULL",
                    [execution_root_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(payload) = payload else {
                return Ok(0);
            };
            let mut json: serde_json::Value = serde_json::from_str(&payload)?;
            json["removed_at"] = serde_json::Value::String(now.clone());
            let updated_payload = serde_json::to_string(&json)?;
            let n = tx.execute(
                "UPDATE execution_root_entries
                 SET removed_at = ?1, payload_json = ?2
                 WHERE execution_root_id = ?3 AND removed_at IS NULL",
                params![now, updated_payload, execution_root_id],
            )?;
            Ok(n)
        })?;
        Ok(affected > 0)
    }

    /// Return all non-removed entries for a workspace.
    pub fn active_for_workspace(&self, workspace_id: &str) -> Result<Vec<ExecutionRootEntry>> {
        let connection = self.db.connection()?;
        context_execution_roots_on(&connection, workspace_id)
    }

    /// Return all entries, including removed tombstones, in stable order.
    pub fn latest_all(&self) -> Result<Vec<ExecutionRootEntry>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json FROM execution_root_entries
             ORDER BY created_at ASC, execution_root_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_execution_root_entry_payload(&row?))
            .collect()
    }
}

impl AgentIdentityRepository<'_> {
    pub fn import_legacy(&self, records: Vec<AgentIdentityRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("agent_identities", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("agent_identities", "jsonl", "db", |tx| {
                let latest = reduce_agent_identity_records(records);
                for record in latest.values() {
                    super::episode_snapshot::prepare_imported_identity_continuity_tx(
                        tx,
                        record,
                        Utc::now(),
                    )?;
                    upsert_agent_identity_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &AgentIdentityRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_agent_identity_tx(tx, record))
    }

    pub fn create_with_bootstrap(
        &self,
        identity: &AgentIdentityRecord,
        bootstrap: &AgentBootstrapRecord,
    ) -> Result<()> {
        self.create_with_bootstrap_and_relations(
            identity,
            bootstrap,
            &AgentCanonicalRecordSet::default(),
        )
    }

    pub fn create_with_bootstrap_and_relations(
        &self,
        identity: &AgentIdentityRecord,
        bootstrap: &AgentBootstrapRecord,
        relations: &AgentCanonicalRecordSet,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            let existing = tx
                .query_row(
                    "SELECT agent_id FROM agent_identities WHERE agent_id = ?1",
                    [&identity.agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            anyhow::ensure!(
                existing.is_none(),
                "agent_identity_conflict: agent {} already exists",
                identity.agent_id
            );
            upsert_agent_identity_tx(tx, identity)?;
            upsert_agent_bootstrap_tx(tx, bootstrap)?;
            upsert_canonical_record_set_tx(tx, relations)
        })
    }

    pub fn create_with_relations(
        &self,
        identity: &AgentIdentityRecord,
        relations: &AgentCanonicalRecordSet,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            let existing = tx
                .query_row(
                    "SELECT agent_id FROM agent_identities WHERE agent_id = ?1",
                    [&identity.agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            anyhow::ensure!(
                existing.is_none(),
                "agent_identity_conflict: agent {} already exists",
                identity.agent_id
            );
            upsert_agent_identity_tx(tx, identity)?;
            upsert_canonical_record_set_tx(tx, relations)
        })
    }

    /// Explicit default/standalone legacy bootstrap; ordinary create never consumes a seed.
    pub(crate) fn create_legacy_materialized_with_relations(
        &self,
        identity: &AgentIdentityRecord,
        relations: &AgentCanonicalRecordSet,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM agent_identities WHERE agent_id = ?1)",
                [&identity.agent_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                !exists,
                "agent_identity_conflict: agent {} already exists",
                identity.agent_id
            );
            super::episode_snapshot::bind_unbound_episode_continuity_tx(tx, identity, Utc::now())?;
            upsert_agent_identity_tx(tx, identity)?;
            upsert_canonical_record_set_tx(tx, relations)
        })
    }

    /// Replaces a fully deleted tombstone with a fresh Active identity for
    /// the same agent id: a new incarnation. Requires the latest deletion
    /// job to be `Completed`, flips the identity reservation back to
    /// `active` in the same transaction, resets per-agent runtime
    /// projections so no previous-incarnation state leaks, writes the fresh
    /// bootstrap and canonical relations, and appends the `agent_recreated`
    /// audit boundary event. This is the only sanctioned retired -> active
    /// reservation transition; the builder callback receives the tombstone
    /// and must return the fresh Active record with `incarnation` and
    /// `revision` continued monotonically.
    pub fn reincarnate_with_bootstrap_and_relations(
        &self,
        agent_id: &str,
        requested_by: &str,
        build: impl Fn(
            &AgentIdentityRecord,
        ) -> (
            AgentIdentityRecord,
            AgentBootstrapRecord,
            AgentCanonicalRecordSet,
        ),
    ) -> Result<AgentIdentityRecord> {
        self.db.transaction(|tx| {
            let payload = tx
                .query_row(
                    "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                    [agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    anyhow!("agent_reincarnation_rejected: agent {agent_id} not found")
                })?;
            let tombstone = decode_agent_identity_payload(&payload)?;
            if tombstone.status != AgentRegistryStatus::Deleted {
                return Err(anyhow!(
                    "agent_reincarnation_rejected: agent {agent_id} is {:?}, not deleted",
                    tombstone.status
                ));
            }
            let job_payload = tx
                .query_row(
                    "SELECT payload_json FROM agent_deletion_jobs WHERE agent_id = ?1",
                    [agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let job: Option<AgentDeletionJob> = job_payload
                .map(|payload| {
                    serde_json::from_str(&payload).context("decoding agent deletion job payload")
                })
                .transpose()?;
            let job = job.ok_or_else(|| {
                anyhow!("agent_reincarnation_rejected: agent {agent_id} has no deletion job")
            })?;
            if job.status != AgentDeletionStatus::Completed {
                return Err(anyhow!(
                    "agent_reincarnation_rejected: agent {agent_id} deletion job is {:?}",
                    job.status
                ));
            }
            let (identity, bootstrap, relations) = build(&tombstone);
            anyhow::ensure!(
                identity.agent_id == tombstone.agent_id,
                "agent_reincarnation_rejected: fresh identity agent id mismatch"
            );
            anyhow::ensure!(
                identity.status == AgentRegistryStatus::Active,
                "agent_reincarnation_rejected: fresh identity must be active"
            );
            anyhow::ensure!(
                identity.incarnation == tombstone.incarnation.saturating_add(1),
                "agent_reincarnation_rejected: incarnation must continue monotonically"
            );
            anyhow::ensure!(
                identity.revision > tombstone.revision,
                "agent_reincarnation_rejected: revision must continue monotonically"
            );
            release_agent_identity_reservation_tx(tx, agent_id)?;
            upsert_agent_identity_tx(tx, &identity)?;
            // Fresh per-agent runtime projections: the previous
            // incarnation's live state must never leak into the new one.
            tx.execute(
                "DELETE FROM agent_states WHERE agent_id = ?1",
                params![agent_id],
            )?;
            tx.execute(
                "DELETE FROM agent_bootstraps WHERE agent_id = ?1",
                params![agent_id],
            )?;
            tx.execute(
                "DELETE FROM agent_lineages WHERE child_agent_id = ?1",
                params![agent_id],
            )?;
            tx.execute(
                "DELETE FROM agent_supervisions WHERE child_agent_id = ?1",
                params![agent_id],
            )?;
            upsert_agent_bootstrap_tx(tx, &bootstrap)?;
            upsert_canonical_record_set_tx(tx, &relations)?;
            append_audit_event_tx(
                tx,
                Some(agent_id),
                &AuditEvent::legacy(
                    "agent_recreated",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "incarnation": identity.incarnation,
                        "previous_deleted_at": tombstone.deleted_at,
                        "deletion_id": job.deletion_id,
                        "requested_by": requested_by,
                    }),
                ),
            )?;
            Ok(identity)
        })
    }

    pub fn tombstone_with_closed_supervision(&self, agent_id: &str) -> Result<AgentIdentityRecord> {
        self.db.transaction(|tx| {
            let payload = tx
                .query_row(
                    "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                    [agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("agent {agent_id} not found"))?;
            let mut identity = decode_agent_identity_payload(&payload)?;
            if identity.status != AgentRegistryStatus::Deleted {
                let now = std::cmp::max(
                    Utc::now(),
                    identity.updated_at + chrono::Duration::nanoseconds(1),
                );
                identity.status = AgentRegistryStatus::Deleted;
                identity.deleted_at = Some(now);
                identity.updated_at = now;
                identity.revision = identity.revision.saturating_add(1);
                upsert_agent_identity_tx(tx, &identity)?;
            }
            transition_supervision_state_tx(
                tx,
                agent_id,
                AgentSupervisionState::Closed,
                identity.revision,
                identity.updated_at,
            )?;
            Ok(identity)
        })
    }

    pub fn latest_all(&self) -> Result<Vec<AgentIdentityRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM agent_identities
             ORDER BY updated_at DESC, created_at DESC, agent_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_agent_identity_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub(crate) fn next_legacy_deletion_scan_batch(
        &self,
        limit: usize,
    ) -> Result<LegacyDeletionScanBatch> {
        const CURSOR_KEY: &str = "legacy_deletion_repair_cursor";
        if limit == 0 {
            return Ok(LegacyDeletionScanBatch {
                identities: Vec::new(),
                start_cursor: None,
                cursor: None,
            });
        }
        self.db.transaction(|tx| {
            let cursor = tx
                .query_row(
                    "SELECT value FROM runtime_metadata WHERE key = ?1",
                    [CURSOR_KEY],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let payloads = {
                let mut statement = tx.prepare(
                    "SELECT payload_json
                     FROM agent_identities
                     WHERE agent_id > ?1
                     ORDER BY agent_id ASC
                     LIMIT ?2",
                )?;
                let rows = statement
                    .query_map(
                        params![
                            cursor.as_deref().unwrap_or_default(),
                            i64::try_from(limit).unwrap_or(i64::MAX)
                        ],
                        |row| row.get::<_, String>(0),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            if payloads.is_empty() {
                tx.execute("DELETE FROM runtime_metadata WHERE key = ?1", [CURSOR_KEY])?;
                return Ok(LegacyDeletionScanBatch {
                    identities: Vec::new(),
                    start_cursor: cursor,
                    cursor: None,
                });
            }
            let identities = payloads
                .into_iter()
                .map(|payload| decode_agent_identity_payload(&payload))
                .collect::<Result<Vec<_>>>()?;
            let next_cursor = identities
                .last()
                .map(|identity| identity.agent_id.clone())
                .expect("non-empty identity scan batch has a cursor");
            Ok(LegacyDeletionScanBatch {
                identities,
                start_cursor: cursor,
                cursor: Some(next_cursor),
            })
        })
    }

    pub(crate) fn commit_legacy_deletion_scan_batch(
        &self,
        batch: &LegacyDeletionScanBatch,
    ) -> Result<()> {
        const CURSOR_KEY: &str = "legacy_deletion_repair_cursor";
        let Some(next_cursor) = batch.cursor.as_deref() else {
            return Ok(());
        };
        self.db.transaction(|tx| {
            let current_cursor = tx
                .query_row(
                    "SELECT value FROM runtime_metadata WHERE key = ?1",
                    [CURSOR_KEY],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            anyhow::ensure!(
                current_cursor == batch.start_cursor,
                "legacy deletion repair cursor changed while processing batch"
            );
            let now = timestamp(Utc::now());
            tx.execute(
                "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                   value = excluded.value,
                   updated_at = excluded.updated_at",
                params![CURSOR_KEY, next_cursor, now],
            )?;
            Ok(())
        })
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_agent_identity_payload(&payload))
            .transpose()
    }

    pub fn rename(
        &self,
        agent_id: &str,
        requested_name: &str,
        actor: &str,
    ) -> Result<AgentIdentityRecord> {
        let name = normalize_agent_name(requested_name)?;
        let name_key = agent_name_key(&name);
        self.db.transaction(|tx| {
            let payload = tx
                .query_row(
                    "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                    [agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("agent {agent_id} not found"))?;
            let mut identity = decode_agent_identity_payload(&payload)?;
            anyhow::ensure!(
                identity.status == AgentRegistryStatus::Active,
                "agent {agent_id} cannot be renamed while it is {:?}",
                identity.status
            );
            let conflict = tx
                .query_row(
                    "SELECT agent_id FROM agent_identities
                     WHERE name_key = ?1 AND agent_id <> ?2",
                    params![name_key, agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(conflict) = conflict {
                return Err(anyhow!(
                    "agent_name_conflict: name {name:?} is already used by agent {conflict}"
                ));
            }
            let previous_name = identity.name.clone();
            let now = std::cmp::max(
                Utc::now(),
                identity.updated_at + chrono::Duration::nanoseconds(1),
            );
            identity.name = Some(name.clone());
            identity.revision = identity.revision.saturating_add(1);
            identity.updated_at = now;
            upsert_agent_identity_tx(tx, &identity)?;
            let event = AuditEvent::legacy(
                "agent_renamed",
                serde_json::json!({
                    "agent_id": agent_id,
                    "actor": actor,
                    "previous_name": previous_name,
                    "name": identity.name,
                }),
            );
            append_audit_event_tx(tx, Some(agent_id), &event)?;
            Ok(identity)
        })
    }
}

impl AgentBootstrapRepository<'_> {
    pub fn upsert(&self, record: &AgentBootstrapRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_agent_bootstrap_tx(tx, record))
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentBootstrapRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_bootstraps WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| {
                serde_json::from_str(&payload).context("decoding agent bootstrap payload")
            })
            .transpose()
    }
}

/// Used before filesystem inspection and again in the fenced admission transaction.
pub(crate) fn validate_parent_cleanup_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    parent_agent_id: &str,
    expected_incarnation: u64,
    parent_incarnation: u64,
) -> Result<()> {
    let payload: String = tx.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [agent_id],
        |r| r.get(0),
    )?;
    let identity = decode_agent_identity_payload(&payload)?;
    anyhow::ensure!(
        identity.incarnation == expected_incarnation,
        "agent_cleanup_stale_incarnation"
    );
    anyhow::ensure!(
        identity.kind == AgentKind::Child
            && identity.visibility == AgentVisibility::Private
            && identity.ownership() == AgentOwnership::ParentSupervised,
        "agent_cleanup_not_supervised_child"
    );
    let relations = canonical_relations_from_connection(tx, agent_id)?
        .ok_or_else(|| anyhow!("agent_cleanup_missing_relations"))?;
    anyhow::ensure!(
        relations.resolution == AgentCanonicalResolution::Resolved
            && relations
                .supervision
                .as_ref()
                .is_some_and(|s| s.supervisor_agent_id == parent_agent_id)
            && relations
                .durability
                .as_ref()
                .is_some_and(|d| d.durability == AgentCanonicalDurability::Ephemeral)
            && relations
                .lifecycle_attachment
                .as_ref()
                .is_some_and(|a| a.attachment == AgentLifecycleAttachment::SupervisionAttached),
        "agent_cleanup_parent_not_authorized"
    );
    let parent_active: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id = ?1 AND status = 'active')",
        [parent_agent_id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(parent_active, "agent_cleanup_parent_unavailable");
    let current_parent_incarnation: u64 = tx.query_row("SELECT COALESCE(json_extract(payload_json, '$.incarnation'),1) FROM agent_identities WHERE agent_id=?1",[parent_agent_id],|row|row.get(0))?;
    anyhow::ensure!(
        current_parent_incarnation == parent_incarnation,
        "agent_cleanup_stale_parent_incarnation"
    );
    let parent_relations = canonical_relations_from_connection(tx, parent_agent_id)?
        .ok_or_else(|| anyhow!("agent_cleanup_parent_missing_relations"))?;
    anyhow::ensure!(
        parent_relations.resolution == AgentCanonicalResolution::Resolved
            && parent_relations
                .capability_policy
                .as_ref()
                .is_some_and(|policy| policy.allows(AgentCapabilityFamily::AgentCreation)),
        "agent_cleanup_capability_denied"
    );
    if identity.status == AgentRegistryStatus::Active {
        anyhow::ensure!(
            relations.supervision.as_ref().is_some_and(|s| matches!(
                s.state,
                AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
            )),
            "agent_cleanup_supervision_closed"
        );
        if let Some(blocker) = super::reclamation::parent_cleanup_blocker_tx(tx, agent_id)? {
            return Err(anyhow!("agent_cleanup_blocked: {blocker}"));
        }
    }
    Ok(())
}

pub(crate) fn ensure_agent_deletion_tx(
    tx: &Transaction<'_>,
    request: &AgentDeletionRequest,
) -> Result<(AgentIdentityRecord, AgentDeletionJob, bool)> {
    ensure_agent_deletion_inner_tx(tx, request, None)
}

/// Freeze the original deletion identity and clock in its enclosing receipt.
/// The original authorization/idempotent branches remain in the same TX.
pub(crate) fn ensure_agent_deletion_at_tx(
    tx: &Transaction<'_>,
    request: &AgentDeletionRequest,
    deletion_id: &str,
    prepared_at: DateTime<Utc>,
) -> Result<(AgentIdentityRecord, AgentDeletionJob, bool)> {
    anyhow::ensure!(
        !deletion_id.trim().is_empty(),
        "empty original deletion identity"
    );
    ensure_agent_deletion_inner_tx(tx, request, Some((deletion_id, prepared_at)))
}

fn ensure_agent_deletion_inner_tx(
    tx: &Transaction<'_>,
    request: &AgentDeletionRequest,
    creation: Option<(&str, DateTime<Utc>)>,
) -> Result<(AgentIdentityRecord, AgentDeletionJob, bool)> {
    let agent_id = &request.agent_id;
    let payload = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let payload = match payload {
        Some(payload) => payload,
        None if matches!(
            request.admission,
            AgentDeletionAdmission::TerminalEphemeralChild { .. }
        ) =>
        {
            return Err(TerminalChildDeletionAdmissionRejected::new(
                agent_id,
                "identity not found",
            )
            .into());
        }
        None => return Err(anyhow!("agent {agent_id} not found")),
    };
    let mut identity = decode_agent_identity_payload(&payload)?;

    let existing_job = tx
        .query_row(
            "SELECT payload_json FROM agent_deletion_jobs WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            serde_json::from_str::<AgentDeletionJob>(&payload)
                .context("decoding existing agent deletion job payload")
        })
        .transpose()?;

    // Authorization precedes idempotent replay, including deleting/deleted targets.
    if let AgentDeletionAdmission::SupervisingParent {
        parent_agent_id,
        expected_incarnation,
        parent_incarnation,
    } = &request.admission
    {
        validate_parent_cleanup_tx(
            tx,
            agent_id,
            parent_agent_id,
            *expected_incarnation,
            *parent_incarnation,
        )?;
    }

    if identity.status != AgentRegistryStatus::Active {
        transition_supervision_state_tx(
            tx,
            agent_id,
            match identity.status {
                AgentRegistryStatus::Active => unreachable!(),
                AgentRegistryStatus::Deleting => AgentSupervisionState::CleanupRequired,
                AgentRegistryStatus::Deleted => AgentSupervisionState::Closed,
            },
            identity.revision,
            identity.updated_at,
        )?;
        if let Some(job) = existing_job.as_ref() {
            if matches!(
                request.admission,
                AgentDeletionAdmission::TerminalEphemeralChild { .. }
                    | AgentDeletionAdmission::SupervisingParent { .. }
            ) {
                return Ok((identity, job.clone(), false));
            }
            if identity.status != AgentRegistryStatus::Deleted
                || job.status != AgentDeletionStatus::Completed
                || job.mode == AgentDeletionMode::CleanupRepair
            {
                return Ok((identity, job.clone(), false));
            }
        }
    }

    let expected_identity_revision = match &request.admission {
        AgentDeletionAdmission::ExpectedRevision(revision) => *revision,
        AgentDeletionAdmission::SupervisingParent { .. } => identity.revision,
        AgentDeletionAdmission::TerminalEphemeralChild {
            parent_agent_id,
            task_id,
        } => {
            let reject = |reason| {
                anyhow::Error::new(TerminalChildDeletionAdmissionRejected::new(
                    agent_id, reason,
                ))
            };
            if identity.status == AgentRegistryStatus::Deleted {
                return Err(reject("identity is already deleted"));
            }
            if identity.kind != AgentKind::Child
                || identity.visibility != AgentVisibility::Private
                || identity.ownership() != AgentOwnership::ParentSupervised
            {
                return Err(reject("identity is not a private parent-supervised child"));
            }
            let relations = canonical_relations_from_connection(tx, agent_id)?
                .ok_or_else(|| reject("canonical relation projection is missing"))?;
            if relations.resolution != AgentCanonicalResolution::Resolved {
                return Err(reject("canonical relations are not resolved"));
            }
            let lineage = relations
                .lineage
                .as_ref()
                .ok_or_else(|| reject("canonical lineage is missing"))?;
            let supervision = relations
                .supervision
                .as_ref()
                .ok_or_else(|| reject("canonical supervision is missing"))?;
            let durability = relations
                .durability
                .as_ref()
                .ok_or_else(|| reject("canonical durability is missing"))?;
            let lifecycle_attachment = relations
                .lifecycle_attachment
                .as_ref()
                .ok_or_else(|| reject("canonical lifecycle attachment is missing"))?;
            if lineage.parent_agent_id != *parent_agent_id
                || supervision.supervisor_agent_id != *parent_agent_id
                || supervision.delegated_from_task_id.as_deref() != Some(task_id.as_str())
                || durability.durability != AgentCanonicalDurability::Ephemeral
                || lifecycle_attachment.attachment != AgentLifecycleAttachment::SupervisionAttached
            {
                return Err(reject(&format!(
                    "canonical relations do not attach the child to parent task {task_id}"
                )));
            }
            let task_payload = tx
                .query_row(
                    "SELECT payload_json FROM tasks WHERE task_id = ?1",
                    [task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| reject("delegated parent task is missing"))?;
            let task = decode_task_payload(&task_payload)?;
            if task.agent_id != *parent_agent_id {
                return Err(reject(
                    "delegated parent task owner does not match supervision",
                ));
            }
            if !matches!(
                task.kind,
                TaskKind::ChildAgentTask | TaskKind::SubagentTask | TaskKind::WorktreeSubagentTask
            ) {
                return Err(reject("delegated task is not a one-shot child task"));
            }
            if !is_terminal_task_status(&task.status) {
                return Err(reject("delegated parent task is not terminal"));
            }
            let delete_on_terminal = match task.recovery.as_ref() {
                Some(TaskRecoverySpec::ChildAgentTask {
                    lifecycle_disposition,
                    ..
                }) => *lifecycle_disposition == AgentLifecycleDisposition::DeleteOnTerminal,
                Some(TaskRecoverySpec::SubagentTask { .. })
                | Some(TaskRecoverySpec::WorktreeSubagentTask { .. })
                | None
                    if task.kind.is_child_agent() =>
                {
                    true
                }
                _ => false,
            };
            if !delete_on_terminal {
                return Err(reject(
                    "delegated task does not require deletion on terminal",
                ));
            }
            let detail_child_id = task
                .detail
                .as_ref()
                .and_then(|detail| detail.get("child_agent_id"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| reject("delegated task child identity link is missing"))?;
            if detail_child_id != agent_id {
                return Err(reject("delegated task child identity does not match"));
            }
            identity.revision
        }
    };

    if identity.revision != expected_identity_revision {
        return Err(anyhow!(
            "agent {agent_id} identity revision mismatch: expected {expected_identity_revision}, current {}",
            identity.revision
        ));
    }

    if let Some(job) = existing_job {
        anyhow::ensure!(
            identity.status == AgentRegistryStatus::Deleted
                || job.status == AgentDeletionStatus::Completed,
            "agent {agent_id} has a non-completed deletion job while its identity is active"
        );
        tx.execute(
            "DELETE FROM agent_deletion_jobs WHERE agent_id = ?1",
            params![agent_id],
        )?;
    }

    let now = std::cmp::max(
        creation.map_or_else(Utc::now, |(_, at)| at),
        identity.updated_at + chrono::Duration::nanoseconds(1),
    );
    let job = AgentDeletionJob {
        deletion_id: creation.map_or_else(crate::ids::agent_deletion_id, |(id, _)| id.to_owned()),
        agent_id: agent_id.to_string(),
        mode: if identity.status == AgentRegistryStatus::Deleted {
            AgentDeletionMode::CleanupRepair
        } else if matches!(
            request.admission,
            AgentDeletionAdmission::SupervisingParent { .. }
        ) {
            AgentDeletionMode::ParentCleanup
        } else {
            AgentDeletionMode::Delete
        },
        status: AgentDeletionStatus::Pending,
        phase: if identity.status == AgentRegistryStatus::Deleted {
            AgentDeletionPhase::Quiesce
        } else {
            AgentDeletionPhase::Fence
        },
        requested_by: request.requested_by.clone(),
        expected_identity_revision,
        cascade_private_children: request.cascade_private_children,
        attempts: 0,
        last_error: None,
        next_attempt_at: None,
        created_at: now,
        updated_at: now,
        completed_at: None,
    };
    if identity.status == AgentRegistryStatus::Active {
        identity.status = AgentRegistryStatus::Deleting;
        identity.revision = identity.revision.saturating_add(1);
        identity.updated_at = now;
        upsert_agent_identity_tx(tx, &identity)?;
    }
    transition_supervision_state_tx(
        tx,
        agent_id,
        if identity.status == AgentRegistryStatus::Deleted {
            AgentSupervisionState::Closed
        } else {
            AgentSupervisionState::CleanupRequired
        },
        identity.revision,
        identity.updated_at,
    )?;
    insert_agent_deletion_job_tx(tx, &job)?;
    crate::runtime_db::agent_message_delivery::cancel_active_deliveries_for_target_tx(
        tx, agent_id,
    )?;
    Ok((identity, job, true))
}

impl AgentDeletionRepository<'_> {
    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Option<AgentDeletionJob>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_deletion_jobs WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| {
                serde_json::from_str(&payload).context("decoding agent deletion job payload")
            })
            .transpose()
    }

    pub fn begin(
        &self,
        agent_id: &str,
        expected_identity_revision: u64,
        requested_by: &str,
        cascade_private_children: bool,
    ) -> Result<(AgentIdentityRecord, AgentDeletionJob, bool)> {
        self.db.transaction(|tx| {
            ensure_agent_deletion_tx(
                tx,
                &AgentDeletionRequest {
                    agent_id: agent_id.to_string(),
                    admission: AgentDeletionAdmission::ExpectedRevision(expected_identity_revision),
                    requested_by: requested_by.to_string(),
                    cascade_private_children,
                },
            )
        })
    }

    pub(crate) fn begin_parent_cleanup(
        &self,
        agent_id: &str,
        parent_agent_id: &str,
        expected_incarnation: u64,
        parent_incarnation: u64,
    ) -> Result<(AgentIdentityRecord, AgentDeletionJob, bool)> {
        self.db.transaction(|tx| {
            ensure_agent_deletion_tx(
                tx,
                &AgentDeletionRequest {
                    agent_id: agent_id.into(),
                    admission: AgentDeletionAdmission::SupervisingParent {
                        parent_agent_id: parent_agent_id.into(),
                        expected_incarnation,
                        parent_incarnation,
                    },
                    requested_by: format!("agent:{parent_agent_id}"),
                    cascade_private_children: false,
                },
            )
        })
    }

    pub(crate) fn begin_terminal_ephemeral_child(
        &self,
        agent_id: &str,
        parent_agent_id: &str,
        task_id: &str,
        requested_by: &str,
    ) -> Result<(AgentIdentityRecord, AgentDeletionJob, bool)> {
        self.db.transaction(|tx| {
            ensure_agent_deletion_tx(
                tx,
                &AgentDeletionRequest {
                    agent_id: agent_id.to_string(),
                    admission: AgentDeletionAdmission::TerminalEphemeralChild {
                        parent_agent_id: parent_agent_id.to_string(),
                        task_id: task_id.to_string(),
                    },
                    requested_by: requested_by.to_string(),
                    cascade_private_children: false,
                },
            )
        })
    }

    pub fn finalize(
        &self,
        job: &AgentDeletionJob,
    ) -> Result<(AgentIdentityRecord, AgentDeletionJob)> {
        self.db.transaction(|tx| {
            let job_payload = tx
                .query_row(
                    "SELECT payload_json FROM agent_deletion_jobs WHERE deletion_id = ?1",
                    [&job.deletion_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("deletion job {} not found", job.deletion_id))?;
            let mut completed_job: AgentDeletionJob = serde_json::from_str(&job_payload)
                .context("decoding deletion job for finalization")?;
            let payload = tx
                .query_row(
                    "SELECT payload_json,
                            COALESCE(json_extract(payload_json, '$.status'), '')
                     FROM agent_identities WHERE agent_id = ?1",
                    [&completed_job.agent_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .ok_or_else(|| anyhow!("agent {} not found", completed_job.agent_id))?;
            let (payload, stored_status) = payload;
            let mut identity = decode_agent_identity_payload(&payload)?;
            if completed_job.status == AgentDeletionStatus::Completed {
                // Idempotent re-entry: the job already reached its terminal
                // state, but the registry entry must still converge to the
                // same terminal state. Legacy rows can carry the pre-rename
                // `archived` status text (decoded as `Deleted` above), and a
                // crash window can leave `Deleting`; either shape keeps the
                // completed-deletion invariant violated forever, so drive the
                // entry to the canonical tombstone here. A sanctioned
                // reincarnation (active identity with a reincarnation
                // reservation source) is exempt: tombstoning it here would
                // destroy the re-created agent its completed predecessor
                // sanctioned.
                let reservation_source: Option<String> = tx
                    .query_row(
                        "SELECT source FROM agent_identity_reservations WHERE agent_id = ?1",
                        [&completed_job.agent_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let sanctioned_reincarnation = identity.status == AgentRegistryStatus::Active
                    && reservation_source.as_deref() == Some("reincarnation");
                if sanctioned_reincarnation {
                    // The completed predecessor job is already terminal and
                    // the living incarnation owns the registry, reservation,
                    // and supervision state: converge nothing.
                    return Ok((identity, completed_job));
                }
                let now = std::cmp::max(
                    Utc::now(),
                    std::cmp::max(identity.updated_at, completed_job.updated_at)
                        + chrono::Duration::nanoseconds(1),
                );
                ensure_deleted_tombstone_tx(tx, &mut identity, &stored_status, now)?;
                transition_supervision_state_tx(
                    tx,
                    &completed_job.agent_id,
                    AgentSupervisionState::Closed,
                    identity.revision,
                    identity.updated_at,
                )?;
                return Ok((identity, completed_job));
            }
            if completed_job.mode == AgentDeletionMode::CleanupRepair {
                anyhow::ensure!(
                    identity.status == AgentRegistryStatus::Deleted,
                    "cleanup-repair job {} requires a deleted identity, found {:?}",
                    completed_job.deletion_id,
                    identity.status
                );
            }
            let now = std::cmp::max(
                Utc::now(),
                std::cmp::max(identity.updated_at, completed_job.updated_at)
                    + chrono::Duration::nanoseconds(1),
            );
            ensure_deleted_tombstone_tx(tx, &mut identity, &stored_status, now)?;
            transition_supervision_state_tx(
                tx,
                &completed_job.agent_id,
                AgentSupervisionState::Closed,
                identity.revision,
                identity.updated_at,
            )?;
            completed_job.status = AgentDeletionStatus::Completed;
            completed_job.phase = AgentDeletionPhase::Finalize;
            completed_job.last_error = None;
            completed_job.next_attempt_at = None;
            completed_job.updated_at = now;
            completed_job.completed_at = Some(now);
            let payload_json = serde_json::to_string(&completed_job)?;
            let updated = tx.execute(
                "UPDATE agent_deletion_jobs
                 SET status = ?1, phase = ?2, updated_at = ?3,
                     completed_at = ?4, next_attempt_at = ?5, payload_json = ?6
                 WHERE deletion_id = ?7 AND agent_id = ?8",
                params![
                    enum_string(&completed_job.status)?,
                    enum_string(&completed_job.phase)?,
                    timestamp(completed_job.updated_at),
                    completed_job.completed_at.map(timestamp),
                    completed_job.next_attempt_at.map(timestamp),
                    payload_json,
                    completed_job.deletion_id,
                    completed_job.agent_id,
                ],
            )?;
            anyhow::ensure!(
                updated == 1,
                "deletion job {} not found for finalization",
                completed_job.deletion_id
            );
            Ok((identity, completed_job))
        })
    }

    /// Persist an updated deletion job (phase advance, status change, etc.).
    pub fn update(&self, job: &AgentDeletionJob) -> Result<()> {
        self.db.transaction(|tx| {
            let payload_json = serde_json::to_string(job)?;
            let updated = tx.execute(
                "UPDATE agent_deletion_jobs
                 SET status = ?1, phase = ?2, updated_at = ?3,
                     completed_at = ?4, next_attempt_at = ?5, payload_json = ?6
                 WHERE deletion_id = ?7",
                params![
                    enum_string(&job.status)?,
                    enum_string(&job.phase)?,
                    timestamp(job.updated_at),
                    job.completed_at.map(timestamp),
                    job.next_attempt_at.map(timestamp),
                    payload_json,
                    job.deletion_id,
                ],
            )?;
            if updated == 0 {
                return Err(anyhow!(
                    "deletion job {} not found for update",
                    job.deletion_id
                ));
            }
            Ok(())
        })
    }

    /// Job progress has its own authority. Requeue only the exact Running
    /// checkpoint; a metadata ACK may already have completed or advanced it.
    pub(crate) fn requeue_metadata_pending(
        &self,
        expected: &AgentDeletionJob,
        error: &str,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<bool> {
        anyhow::ensure!(
            expected.status == AgentDeletionStatus::Running,
            "metadata recovery requires a Running deletion checkpoint"
        );
        let expected_payload = serde_json::to_string(expected)?;
        let mut retry = expected.clone();
        retry.status = AgentDeletionStatus::RetryableFailed;
        retry.last_error = Some(error.to_string());
        retry.updated_at =
            std::cmp::max(now, expected.updated_at + chrono::Duration::nanoseconds(1));
        retry.next_attempt_at = Some(next_attempt_at);
        let retry_payload = serde_json::to_string(&retry)?;
        self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE agent_deletion_jobs
                 SET status = 'retryable_failed', updated_at = ?1,
                     next_attempt_at = ?2, payload_json = ?3
                 WHERE deletion_id = ?4 AND agent_id = ?5
                   AND status = 'running' AND phase = ?6
                   AND updated_at = ?7 AND payload_json = ?8",
                params![
                    timestamp(retry.updated_at),
                    timestamp(next_attempt_at),
                    retry_payload,
                    expected.deletion_id,
                    expected.agent_id,
                    enum_string(&expected.phase)?,
                    timestamp(expected.updated_at),
                    expected_payload
                ],
            )?;
            Ok(changed == 1)
        })
    }

    /// Atomically claim one due job for the process-local coordinator.
    pub fn claim_due(&self, job: &AgentDeletionJob, now: DateTime<Utc>) -> Result<bool> {
        self.db.transaction(|tx| {
            let payload_json = serde_json::to_string(job)?;
            let updated = tx.execute(
                "UPDATE agent_deletion_jobs
                 SET status = ?1, phase = ?2, updated_at = ?3,
                     completed_at = ?4, next_attempt_at = ?5, payload_json = ?6
                 WHERE deletion_id = ?7
                   AND (
                       status = 'pending'
                       OR (
                           status = 'retryable_failed'
                           AND (next_attempt_at IS NULL OR next_attempt_at <= ?8)
                       )
                   )",
                params![
                    enum_string(&job.status)?,
                    enum_string(&job.phase)?,
                    timestamp(job.updated_at),
                    job.completed_at.map(timestamp),
                    job.next_attempt_at.map(timestamp),
                    payload_json,
                    job.deletion_id,
                    timestamp(now),
                ],
            )?;
            Ok(updated == 1)
        })
    }

    /// Find pending jobs and retries whose persisted deadline has elapsed.
    pub fn due_jobs(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<AgentDeletionJob>> {
        let connection = self.db.connection()?;
        let mut stmt = connection.prepare(
            "SELECT payload_json FROM agent_deletion_jobs
             WHERE status = 'pending'
                OR (
                    status = 'retryable_failed'
                    AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
                )
             ORDER BY COALESCE(next_attempt_at, created_at) ASC, created_at ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![timestamp(now), i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| serde_json::from_str(&row?).context("decoding agent deletion job payload"))
            .collect()
    }

    pub fn earliest_retry_at(&self) -> Result<Option<DateTime<Utc>>> {
        let connection = self.db.connection()?;
        let value = connection.query_row(
            "SELECT MIN(next_attempt_at)
             FROM agent_deletion_jobs
             WHERE status = 'retryable_failed' AND next_attempt_at IS NOT NULL",
            [],
            |row| row.get::<_, Option<String>>(0),
        )?;
        value
            .map(|value| parse_timestamp(&value).context("parsing deletion retry deadline"))
            .transpose()
    }

    /// Recover jobs whose process-local owner disappeared before completion.
    pub fn recover_running_jobs(&self, now: DateTime<Utc>) -> Result<usize> {
        self.db.transaction(|tx| {
            let payloads = {
                let mut stmt = tx.prepare(
                    "SELECT payload_json FROM agent_deletion_jobs WHERE status = 'running'",
                )?;
                let payloads = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                payloads
            };
            let mut recovered = 0;
            for payload in payloads {
                let mut job: AgentDeletionJob = serde_json::from_str(&payload)
                    .context("decoding running deletion job for recovery")?;
                job.status = AgentDeletionStatus::RetryableFailed;
                job.next_attempt_at = Some(now);
                job.updated_at =
                    std::cmp::max(now, job.updated_at + chrono::Duration::nanoseconds(1));
                let payload_json = serde_json::to_string(&job)?;
                recovered += tx.execute(
                    "UPDATE agent_deletion_jobs
                     SET status = ?1, updated_at = ?2, next_attempt_at = ?3, payload_json = ?4
                     WHERE deletion_id = ?5 AND status = 'running'",
                    params![
                        enum_string(&job.status)?,
                        timestamp(job.updated_at),
                        job.next_attempt_at.map(timestamp),
                        payload_json,
                        job.deletion_id,
                    ],
                )?;
            }
            Ok(recovered)
        })
    }
}

impl WorkItemDelegationRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkItemDelegationRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("work_item_delegations", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_item_delegations", "jsonl", "db", |tx| {
                let latest = reduce_work_item_delegation_records(records);
                for record in latest.values() {
                    upsert_work_item_delegation_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkItemDelegationRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_work_item_delegation_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemDelegationRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WorkItemDelegationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemDelegationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             WHERE parent_agent_id = ?1 OR child_agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn latest_for_child(
        &self,
        child_agent_id: &str,
    ) -> Result<Option<WorkItemDelegationRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM work_item_delegations
                 WHERE child_agent_id = ?1
                 ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
                 LIMIT 1",
                [child_agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_delegation_payload(&payload))
            .transpose()
    }
}

impl WorkItemContinuationRepository<'_> {
    pub fn import_empty(&self) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("work_item_continuations", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_item_continuations", "new-domain", "db", |_tx| {
                Ok(serde_json::json!({ "imported_records": 0 }))
            })
    }

    pub fn upsert(&self, record: &WorkItemContinuationFrame) -> Result<()> {
        self.db.transaction(|tx| {
            upsert_work_item_continuation_tx(tx, record)?;
            Ok(())
        })
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WorkItemContinuationFrame>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemContinuationFrame>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn active_for_agent(&self, agent_id: &str) -> Result<Vec<WorkItemContinuationFrame>> {
        let connection = self.db.connection()?;
        context_active_continuations_on(&connection, agent_id)
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemContinuationFrame>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl ContextEpisodeRepository<'_> {
    pub fn import_legacy(&self, records: Vec<ContextEpisodeRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete(CONTEXT_EPISODE_ANCHORS_DOMAIN, "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import(CONTEXT_EPISODE_ANCHORS_DOMAIN, "jsonl", "db", |tx| {
                let latest = reduce_context_episode_records(records);
                for record in latest.values() {
                    upsert_context_episode_tx(tx, record)?;
                }
                super::episode_snapshot::seal_imported_episode_continuity_tx(
                    tx,
                    &latest.values().cloned().collect::<Vec<_>>(),
                    Utc::now(),
                )?;
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &ContextEpisodeRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_context_episode_tx(tx, record))
    }

    pub fn upsert_with_index_changes(
        &self,
        record: &ContextEpisodeRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            upsert_context_episode_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<ContextEpisodeRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM context_episode_anchors
             ORDER BY ended_at DESC, started_at DESC, episode_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_context_episode_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ContextEpisodeRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM context_episode_anchors
             WHERE agent_id = ?1
             ORDER BY ended_at DESC, started_at DESC, episode_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_context_episode_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl ExternalTriggerRepository<'_> {
    pub fn import_legacy(&self, records: Vec<ExternalTriggerRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("external_triggers", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("external_triggers", "jsonl", "db", |tx| {
                let latest = reduce_external_trigger_records(records);
                for record in latest.values() {
                    upsert_external_trigger_tx(tx, record)?;
                    super::scheduling_snapshot::record_scheduling_import_tx(
                        tx,
                        super::scheduling_snapshot::SchedulingImportRecord::ExternalTrigger(record),
                    )?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &ExternalTriggerRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_external_trigger_tx(tx, record))
    }

    pub fn ensure_default_for_agent(
        &self,
        agent_id: &str,
        delivery_mode: CallbackDeliveryMode,
        now: DateTime<Utc>,
        external_trigger_id: String,
        token: String,
    ) -> Result<(ExternalTriggerRecord, Option<ExternalTriggerRecord>, bool)> {
        self.db.transaction(|tx| {
            ensure_default_external_trigger_tx(
                tx,
                agent_id,
                delivery_mode.clone(),
                now,
                external_trigger_id.clone(),
                token.clone(),
                &mut BTreeSet::new(),
            )
        })
    }

    pub fn reset_default_for_agent(
        &self,
        agent_id: &str,
        delivery_mode: CallbackDeliveryMode,
        now: DateTime<Utc>,
        external_trigger_id: String,
        token: String,
    ) -> Result<(ExternalTriggerRecord, Option<ExternalTriggerRecord>)> {
        self.db.transaction(|tx| {
            reset_default_external_trigger_tx(
                tx,
                agent_id,
                delivery_mode.clone(),
                now,
                external_trigger_id.clone(),
                token.clone(),
                &mut BTreeSet::new(),
            )
        })
    }

    pub fn latest(&self, external_trigger_id: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM external_triggers WHERE external_trigger_id = ?1",
                [external_trigger_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        context_external_triggers_on(&connection, agent_id)
    }

    pub fn latest_for_agent_limit(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ExternalTriggerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, external_trigger_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_external_trigger_payload(&row?))
            .collect()
    }

    pub fn active_default_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE target_agent_id = ?1 AND status = 'active'
                 ORDER BY created_at DESC, external_trigger_id ASC
                 LIMIT 1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    /// Legacy persistence inspection; this does not authorize runtime ingress or
    /// grant a qualified Scheduling scope. Runtime and HTTP callback admission
    /// use the bounded, owner-held token locator and original delivery fences.
    pub fn active_by_token_hash(&self, token_hash: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE token_hash = ?1 AND status = 'active'
                 LIMIT 1",
                [token_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             ORDER BY created_at DESC, external_trigger_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_external_trigger_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl OperatorNotificationRepository<'_> {
    pub fn insert(&self, agent_id: &str, record: &OperatorNotificationRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_operator_notification_tx(tx, agent_id, record))
    }

    pub fn read_recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<OperatorNotificationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_notifications
             WHERE agent_id = ?1
             ORDER BY created_at DESC, notification_id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_operator_notification_payload(&row?))
            .collect()
    }
}

impl OperatorTransportBindingRepository<'_> {
    pub fn upsert(&self, agent_id: &str, record: &OperatorTransportBinding) -> Result<()> {
        self.db
            .transaction(|tx| upsert_operator_transport_binding_tx(tx, agent_id, record))
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<OperatorTransportBinding>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_transport_bindings
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, binding_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_transport_binding_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn read_recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<OperatorTransportBinding>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_transport_bindings
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, binding_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_operator_transport_binding_payload(&row?))
            .collect()
    }

    pub fn latest_all(&self) -> Result<Vec<OperatorTransportBinding>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_transport_bindings
             ORDER BY created_at DESC, binding_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_transport_binding_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl OperatorDeliveryRepository<'_> {
    pub fn upsert(&self, agent_id: &str, record: &OperatorDeliveryRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_operator_delivery_record_tx(tx, agent_id, record))
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<OperatorDeliveryRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_delivery_records
             WHERE agent_id = ?1
             ORDER BY created_at DESC, delivery_intent_id DESC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_delivery_record_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn read_recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<OperatorDeliveryRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_delivery_records
             WHERE agent_id = ?1
             ORDER BY created_at DESC, delivery_intent_id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_operator_delivery_record_payload(&row?))
            .collect()
    }

    pub fn latest_all(&self) -> Result<Vec<OperatorDeliveryRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_delivery_records
             ORDER BY created_at DESC, delivery_intent_id DESC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_delivery_record_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl TaskRepository<'_> {
    pub fn import_legacy(&self, records: Vec<TaskRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("tasks", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("tasks", "jsonl", "db", |tx| {
                let latest = reduce_task_records(records);
                for record in latest.values() {
                    let changed = upsert_task_tx(tx, record)?;
                    super::task_snapshot::qualification::record_task_import_tx(
                        tx, record, changed,
                    )?;
                }
                let active_records = latest
                    .values()
                    .filter(|record| is_active_task_status(&record.status))
                    .count();
                Ok(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                }))
            })
    }

    pub fn upsert(&self, record: &TaskRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_task_tx(tx, record).map(|_| ()))
    }

    pub fn upsert_with_index_changes(
        &self,
        record: &TaskRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            let _ = upsert_task_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn latest(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                [task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_task_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<TaskRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM tasks
             ORDER BY updated_at DESC, created_at DESC, task_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_task_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(agent_id, "owner_agent_id = ?1", [agent_id], limit)
    }

    #[cfg(test)]
    pub(crate) fn message_waits_for_reply(
        &self,
        agent_id: &str,
        message_id: &str,
    ) -> Result<Vec<TaskRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json FROM tasks
             WHERE owner_agent_id = ?1 AND kind = 'agent_message_wait' AND status = 'completed'
               AND json_extract(payload_json, '$.detail.message_id') = ?2
             ORDER BY created_at ASC, task_id ASC",
        )?;
        let rows =
            statement.query_map(params![agent_id, message_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_task_payload(&row?)).collect()
    }

    pub fn activity_watermark_for_agent(&self, agent_id: Option<&str>) -> Result<(u64, u128)> {
        let connection = self.db.connection()?;
        let mut hasher = Sha256::new();
        let mut count = 0u64;
        if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM tasks
                 WHERE owner_agent_id = ?1
                 ORDER BY task_id ASC",
            )?;
            let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
            for row in rows {
                let payload = row?;
                count += 1;
                hasher.update(payload.as_bytes());
                hasher.update(b"\n");
            }
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM tasks
                 ORDER BY task_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                let payload = row?;
                count += 1;
                hasher.update(payload.as_bytes());
                hasher.update(b"\n");
            }
        }

        let digest = hasher.finalize();
        let mut marker_bytes = [0u8; 16];
        marker_bytes.copy_from_slice(&digest[..16]);
        Ok((count, u128::from_be_bytes(marker_bytes)))
    }

    pub fn active_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(
            agent_id,
            "owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')",
            [agent_id],
            limit,
        )
    }

    pub fn active_owner_agent_ids(&self) -> Result<Vec<String>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT t.owner_agent_id
             FROM tasks t
             JOIN agent_identities i
               ON i.agent_id = t.owner_agent_id
              AND i.status = 'active'
             WHERE t.status IN ('queued', 'running', 'cancelling')
             ORDER BY t.owner_agent_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn query_for_agent(
        &self,
        _agent_id: &str,
        where_clause: &str,
        params: [&str; 1],
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.db.connection()?;
        context_tasks_query_on(&connection, _agent_id, where_clause, params, limit)
    }
}

impl WaitConditionRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WaitConditionRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("wait_conditions", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("wait_conditions", "jsonl", "db", |tx| {
                let mut imported_ids = BTreeMap::<String, ()>::new();
                let mut work_writes = super::episode_snapshot::work_checkpoint::imports::ImportWrites::default();
                for record in records {
                    imported_ids.insert(record.id.clone(), ());
                    let writes = upsert_wait_condition_with_writes_tx(tx, &record)?;
                    for id in writes.wait_ids { work_writes.record("wait", &id, &record.agent_id,
                        super::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite)?; }
                }
                Ok(serde_json::json!({ "imported_records": imported_ids.len(), "work_domain_actual_writes": work_writes.finish(tx)? }))
            })
    }

    pub fn upsert(&self, record: &WaitConditionRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_wait_condition_tx(tx, record).map(|_| ()))
    }

    pub fn latest_all(&self) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             ORDER BY wait_condition_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))
    }

    /// All wait conditions for one agent, in the same id order as
    /// [`WaitConditionRepository::latest_all`], so downstream filters and
    /// `find` selections keep identical semantics while skipping the
    /// full-table decode of every other agent's rows.
    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE agent_id = ?1
             ORDER BY wait_condition_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WaitConditionRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        let mut records: Vec<_> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WaitConditionRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        let mut records: Vec<_> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))?;
        records.reverse();
        Ok(records)
    }

    pub fn for_turn(&self, agent_id: &str, turn_id: &str) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE agent_id = ?1 AND last_turn_id = ?2
             ORDER BY created_at ASC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map(params![agent_id, turn_id], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions for turn: {e}"))
    }

    pub fn active_for_agent(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        context_active_waits_on(&connection, agent_id)
    }

    pub fn unresolved_for_agent(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE agent_id = ?1 AND status IN ('active', 'triggered')
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading unresolved wait conditions: {e}"))
    }

    pub fn active_all(&self) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE status = 'active'
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))
    }

    pub fn unresolved_all(&self) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE status IN ('active', 'triggered')
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading unresolved wait conditions: {e}"))
    }

    pub fn cancel_unresolved_for_agent(
        &self,
        agent_id: &str,
        cancelled_at: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        self.db.transaction(|tx| {
            let mut statement = tx.prepare(
                "SELECT payload_json
                 FROM wait_conditions
                 WHERE agent_id = ?1 AND status IN ('active', 'triggered')",
            )?;
            let records = statement
                .query_map([agent_id], |row| row.get::<_, String>(0))?
                .map(|row| decode_wait_condition_payload(&row?))
                .collect::<Result<Vec<_>>>()?;
            drop(statement);

            let mut count = 0;
            for mut record in records {
                record.status = WaitConditionStatus::Cancelled;
                record.updated_at = cancelled_at;
                record.cancelled_at = Some(cancelled_at);
                upsert_wait_condition_tx(tx, &record)?;
                count += 1;
            }
            Ok(count)
        })
    }
}

impl QueueEntryRepository<'_> {
    pub fn import_legacy(&self, records: Vec<QueueEntryRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("queue_entries", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("queue_entries", "jsonl", "db", |tx| {
                let mut imported_ids = BTreeMap::<String, ()>::new();
                for record in records {
                    imported_ids.insert(record.message_id.clone(), ());
                    let _ = upsert_queue_entry_tx(tx, &record)?;
                    super::current_read_snapshot::record_current_read_import_tx(
                        tx,
                        &record.agent_id,
                        crate::runtime::current_read_facts::CurrentReadFamily::Queue,
                        &record.message_id,
                        &super::current_read_snapshot::typed_digest(&record)?,
                    )?;
                }
                Ok(serde_json::json!({ "imported_records": imported_ids.len() }))
            })
    }

    pub fn upsert(&self, record: &QueueEntryRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_queue_entry_tx(tx, record).map(|_| ()))
    }

    pub fn try_claim_queued_message(&self, record: &QueueEntryRecord) -> Result<bool> {
        self.db
            .transaction(|tx| try_claim_queued_message_tx(tx, record))
    }

    pub fn latest(&self, message_id: &str) -> Result<Option<QueueEntryRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
                [message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_queue_entry_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<QueueEntryRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM queue_entries
             ORDER BY updated_at DESC, created_at DESC, message_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_queue_entry_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<QueueEntryRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM queue_entries
                 WHERE agent_id = ?1
                 ORDER BY updated_at DESC, created_at DESC, message_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_queue_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM queue_entries
                 ORDER BY updated_at DESC, created_at DESC, message_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_queue_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }

    /// Returns true if the given agent has any messages currently queued.
    pub fn has_queued_for_agent(&self, agent_id: &str) -> Result<bool> {
        let connection = self.db.connection()?;
        let status = enum_string(&crate::types::QueueEntryStatus::Queued)?;
        let interrupted_status = enum_string(&crate::types::QueueEntryStatus::Interrupted)?;
        let exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM queue_entries WHERE agent_id = ?1 AND status IN (?2, ?3) LIMIT 1",
                params![agent_id, status, interrupted_status],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    pub fn recovery_candidate_agent_ids(&self) -> Result<Vec<String>> {
        let connection = self.db.connection()?;
        let dequeued_status = enum_string(&crate::types::QueueEntryStatus::Dequeued)?;
        let interrupted_status = enum_string(&crate::types::QueueEntryStatus::Interrupted)?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT q.agent_id
             FROM queue_entries q
             JOIN agent_identities i
               ON i.agent_id = q.agent_id
              AND i.status = 'active'
             WHERE q.status IN (?1, ?2)
             ORDER BY q.agent_id ASC",
        )?;
        let rows = statement.query_map(params![dequeued_status, interrupted_status], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn has_interrupted_for_agent(&self, agent_id: &str) -> Result<bool> {
        let connection = self.db.connection()?;
        let interrupted_status = enum_string(&crate::types::QueueEntryStatus::Interrupted)?;
        let exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM queue_entries WHERE agent_id = ?1 AND status = ?2 LIMIT 1",
                params![agent_id, interrupted_status],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    /// Returns only entries currently queued for a specific agent.
    pub fn queued_for_agent(&self, agent_id: &str) -> Result<Vec<QueueEntryRecord>> {
        let connection = self.db.connection()?;
        let queued_status = enum_string(&crate::types::QueueEntryStatus::Queued)?;
        let interrupted_status = enum_string(&crate::types::QueueEntryStatus::Interrupted)?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM queue_entries
             WHERE agent_id = ?1 AND status IN (?2, ?3)
             ORDER BY updated_at DESC, created_at DESC, message_id ASC",
        )?;
        let rows = statement.query_map(
            params![agent_id, queued_status, interrupted_status],
            |row| row.get::<_, String>(0),
        )?;
        let mut records: Vec<_> = rows
            .map(|row| decode_queue_entry_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    /// Abort all pending (Queued or Interrupted) queue entries for an agent.
    ///
    /// Used during agent stop/deletion to prevent orphaned entries that would
    /// never be consumed. Returns the number of entries aborted.
    pub fn abort_pending_for_agent(&self, agent_id: &str) -> Result<usize> {
        let entries = self.queued_for_agent(agent_id)?;
        let now = chrono::Utc::now();
        let mut count = 0;
        for mut entry in entries {
            entry.status = QueueEntryStatus::Aborted;
            entry.updated_at = now;
            self.upsert(&entry)?;
            count += 1;
        }
        Ok(count)
    }
}

/// Original timer cancellation ACK; keys come only from actual SQL exits.
#[derive(Default)]
pub(in crate::runtime_db) struct TimerCancellationWrites {
    pub cancelled_timers: usize,
    pub timer_ids: std::collections::BTreeSet<String>,
    pub wake_pairs: std::collections::BTreeSet<(String, String)>,
    pub queue_entries: Vec<QueueEntryRecord>,
}
fn cancel_active_timers_for_agent_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    cancelled_at: chrono::DateTime<Utc>,
) -> Result<usize> {
    Ok(cancel_active_timers_with_writes_tx(tx, agent_id, cancelled_at)?.cancelled_timers)
}

pub(in crate::runtime_db) fn cancel_active_timers_with_writes_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    cancelled_at: chrono::DateTime<Utc>,
) -> Result<TimerCancellationWrites> {
    let mut statement = tx.prepare(
        "SELECT payload_json
             FROM timers
             WHERE agent_id = ?1 AND status = 'active'",
    )?;
    let records = statement
        .query_map([agent_id], |row| row.get::<_, String>(0))?
        .map(|row| decode_timer_payload(&row?))
        .collect::<Result<Vec<_>>>()?;
    drop(statement);

    let mut writes = TimerCancellationWrites::default();
    for mut record in records {
        record.status = TimerStatus::Cancelled;
        if upsert_timer_with_write_tx(tx, &record)? {
            writes.timer_ids.insert(record.id.clone());
        }

        let message_ids = {
            let mut wake_statement = tx.prepare(
                "SELECT message_id
                     FROM timer_wakes
                     WHERE timer_id = ?1 AND status = 'pending'",
            )?;
            let message_ids = wake_statement
                .query_map([&record.id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            message_ids
        };
        // RETURNING is the actual write exit; the original predicate and one
        // bulk UPDATE stay intact, including independently pending old wakes.
        let mut update = tx.prepare(
            "UPDATE timer_wakes SET status='cancelled', updated_at=?2, cancelled_at=?2
             WHERE timer_id=?1 AND status='pending' RETURNING timer_id,message_id",
        )?;
        for row in update.query_map(params![record.id, timestamp(cancelled_at)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            writes.wake_pairs.insert(row?);
        }
        drop(update);
        for message_id in message_ids {
            let payload = tx
                .query_row(
                    "SELECT payload_json
                         FROM queue_entries
                         WHERE message_id = ?1 AND status IN ('queued', 'interrupted')",
                    [&message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(payload) = payload {
                let mut entry = decode_queue_entry_payload(&payload)?;
                entry.status = QueueEntryStatus::Dropped;
                entry.updated_at = cancelled_at;
                if upsert_queue_entry_tx(tx, &entry)? {
                    writes.queue_entries.push(entry);
                }
            }
        }
        writes.cancelled_timers += 1;
    }
    Ok(writes)
}

pub(crate) struct ScopedTimerWakeRecovery {
    pub result: TimerWakeRecoveryResult,
    pub queue_entries: Vec<QueueEntryRecord>,
    pub scope_facts: super::context_inputs::ContextScopeFacts,
    pub scheduling_ack: super::scheduling_snapshot::SchedulingMaintenanceAck,
}

pub(in crate::runtime_db) struct TimerWakeNormalizationInput {
    pub live_messages: Vec<(QueueEntryRecord, MessageEnvelope)>,
    pub pending_wakes: Vec<(String, String, i64)>,
}
pub(in crate::runtime_db) struct TimerWakeNormalizationWrites {
    pub result: TimerWakeRecoveryResult,
    pub wake_pairs: BTreeSet<(String, String)>,
}
pub(in crate::runtime_db) fn read_timer_wake_normalization_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<TimerWakeNormalizationInput> {
    let live_messages = {
        let mut statement = tx.prepare(
            "SELECT q.payload_json, m.payload_json
             FROM queue_entries q
             JOIN messages m ON m.message_id = q.message_id
             WHERE q.agent_id = ?1 AND q.status IN ('queued', 'interrupted')
             ORDER BY q.created_at ASC, q.message_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (queue_payload, message_payload) = row?;
            Ok((
                decode_queue_entry_payload(&queue_payload)?,
                decode_message_payload(&message_payload)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?
    };
    let pending_wakes = {
        let mut statement = tx.prepare(
            "SELECT w.timer_id, w.message_id, w.fire_count
             FROM timer_wakes w
             JOIN timers t ON t.timer_id = w.timer_id
             WHERE t.agent_id = ?1 AND w.status = 'pending'",
        )?;
        let rows = statement.query_map([agent_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    Ok(TimerWakeNormalizationInput {
        live_messages,
        pending_wakes,
    })
}

fn cancel_pending_timer_wakes_with_writes_tx(
    tx: &Transaction<'_>,
    timer: &str,
    at: chrono::DateTime<Utc>,
    actual: &mut BTreeSet<(String, String)>,
) -> Result<usize> {
    let mut update=tx.prepare("UPDATE timer_wakes SET status='cancelled',updated_at=?2,cancelled_at=?2 WHERE timer_id=?1 AND status='pending' RETURNING timer_id,message_id")?;
    let mut changed = 0;
    for row in update.query_map(params![timer, timestamp(at)], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        actual.insert(row?);
        changed += 1;
    }
    Ok(changed)
}
pub(in crate::runtime_db) fn normalize_timer_wakes_with_writes_tx(
    tx: &Transaction<'_>,
    input: TimerWakeNormalizationInput,
) -> Result<TimerWakeNormalizationWrites> {
    let TimerWakeNormalizationInput {
        live_messages,
        pending_wakes,
    } = input;
    let mut result = TimerWakeRecoveryResult::default();
    let mut actual = BTreeSet::new();
    let mut wakes_by_timer = pending_wakes
        .into_iter()
        .map(|(timer_id, message_id, fire_count)| (timer_id, (message_id, fire_count)))
        .collect::<BTreeMap<_, _>>();
    let mut messages_by_timer = BTreeMap::<String, Vec<(QueueEntryRecord, MessageEnvelope)>>::new();
    for (entry, message) in live_messages {
        let MessageOrigin::Timer { timer_id } = &message.origin else {
            continue;
        };
        messages_by_timer
            .entry(timer_id.clone())
            .or_default()
            .push((entry, message));
    }

    for (timer_id, messages) in messages_by_timer {
        let timer = timer_tx(tx, &timer_id)?;
        let timer_accepts_wake = timer.as_ref().is_some_and(|timer| {
            matches!(timer.status, TimerStatus::Active | TimerStatus::Completed)
        });
        let existing_wake = wakes_by_timer.remove(&timer_id);
        let keep_index = if timer_accepts_wake {
            existing_wake
                .as_ref()
                .and_then(|(message_id, _)| {
                    messages
                        .iter()
                        .position(|(_, message)| message.id == *message_id)
                })
                .or(Some(0))
        } else {
            None
        };

        for (index, (mut entry, _)) in messages.iter().cloned().enumerate() {
            if Some(index) == keep_index {
                continue;
            }
            entry.status = QueueEntryStatus::Dropped;
            entry.updated_at = Utc::now();
            upsert_queue_entry_tx(tx, &entry)?;
            result.dropped_message_ids.push(entry.message_id);
        }

        match keep_index {
            Some(index) => {
                let (entry, message) = &messages[index];
                if existing_wake
                    .as_ref()
                    .is_some_and(|(message_id, _)| message_id == &message.id)
                {
                    result.retained_wakes += 1;
                    continue;
                }
                if existing_wake.is_some() {
                    result.invalidated_wakes += cancel_pending_timer_wakes_with_writes_tx(
                        tx,
                        &timer_id,
                        Utc::now(),
                        &mut actual,
                    )?;
                }
                let fire_count = message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("timer_fire_count"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_else(|| timer.as_ref().map_or(1, |timer| timer.fire_count.max(1)));
                // A wake row for this exact pair may already exist when a claimed
                // message was restored to the queue after a restart. Reactivate it
                // instead of inserting a duplicate row for the composite primary key.
                let now = timestamp(Utc::now());
                let reactivated = tx.execute(
                    "UPDATE timer_wakes
                     SET status = 'pending', fire_count = ?3, updated_at = ?4,
                         incorporated_at = NULL, cancelled_at = NULL
                     WHERE timer_id = ?1 AND message_id = ?2 AND status != 'pending'",
                    params![timer_id, message.id, fire_count as i64, now],
                )?;
                if reactivated > 0 {
                    actual.insert((timer_id.clone(), message.id.clone()));
                    result.reactivated_wakes += reactivated;
                    continue;
                }
                let created_at = timestamp(entry.created_at);
                tx.execute(
                    "INSERT INTO timer_wakes
                     (timer_id, message_id, fire_count, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
                    params![timer_id, message.id, fire_count as i64, created_at],
                )?;
                actual.insert((timer_id.clone(), message.id.clone()));
                result.created_wakes += 1;
            }
            None => {
                if existing_wake.is_some() {
                    result.invalidated_wakes += cancel_pending_timer_wakes_with_writes_tx(
                        tx,
                        &timer_id,
                        Utc::now(),
                        &mut actual,
                    )?;
                }
            }
        }
    }

    for (timer_id, _) in wakes_by_timer {
        result.invalidated_wakes +=
            cancel_pending_timer_wakes_with_writes_tx(tx, &timer_id, Utc::now(), &mut actual)?;
    }
    Ok(TimerWakeNormalizationWrites {
        result,
        wake_pairs: actual,
    })
}

fn normalize_timer_wakes_for_recovery_tx(
    tx: &Transaction<'_>,
    agent: &str,
) -> Result<TimerWakeRecoveryResult> {
    normalize_timer_wakes_with_writes_tx(tx, read_timer_wake_normalization_tx(tx, agent)?)
        .map(|ack| ack.result)
}

impl TimerRepository<'_> {
    pub fn cancel_active_for_agent(
        &self,
        agent_id: &str,
        cancelled_at: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        self.db
            .transaction(|tx| cancel_active_timers_for_agent_tx(tx, agent_id, cancelled_at))
    }

    pub fn pending_wake(&self, timer_id: &str) -> Result<Option<TimerWakeRecord>> {
        let connection = self.db.connection()?;
        let row = connection
            .query_row(
                "SELECT timer_id, message_id, fire_count, status, created_at, updated_at,
                        incorporated_at, cancelled_at
                 FROM timer_wakes
                 WHERE timer_id = ?1 AND status = 'pending'",
                [timer_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(
                timer_id,
                message_id,
                fire_count,
                created_at,
                updated_at,
                incorporated_at,
                cancelled_at,
            )| {
                Ok(TimerWakeRecord {
                    timer_id,
                    message_id,
                    fire_count: u64::try_from(fire_count)
                        .context("timer wake fire_count is negative")?,
                    status: TimerWakeStatus::Pending,
                    created_at: parse_timestamp(&created_at)?,
                    updated_at: parse_timestamp(&updated_at)?,
                    incorporated_at: incorporated_at
                        .map(|value| parse_timestamp(&value))
                        .transpose()?,
                    cancelled_at: cancelled_at
                        .map(|value| parse_timestamp(&value))
                        .transpose()?,
                })
            },
        )
        .transpose()
    }

    pub fn active_owner_agent_ids(&self) -> Result<Vec<String>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT t.agent_id
             FROM timers t
             JOIN agent_identities i
               ON i.agent_id = t.agent_id
              AND i.status = 'active'
             WHERE t.status IN ('active', 'scheduled')
             UNION
             SELECT DISTINCT t.agent_id
             FROM timer_wakes w
             JOIN timers t ON t.timer_id = w.timer_id
             JOIN agent_identities i
               ON i.agent_id = t.agent_id
              AND i.status = 'active'
             WHERE w.status = 'pending'
             ORDER BY 1 ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn normalize_wakes_for_recovery(&self, agent_id: &str) -> Result<TimerWakeRecoveryResult> {
        self.db
            .transaction(|tx| normalize_timer_wakes_for_recovery_tx(tx, agent_id))
    }

    pub(crate) fn normalize_wakes_for_recovery_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<ScopedTimerWakeRecovery> {
        self.db.transaction(|tx| {
            let proof = super::scheduling_snapshot::prepare_bootstrap_wake_normalization_tx(
                tx, scope, budget,
            )?;
            let input = read_timer_wake_normalization_tx(tx, &scope.agent_id)?;
            let proof = proof.observe_inputs_tx(tx, &input)?;
            let writes = normalize_timer_wakes_with_writes_tx(tx, input)?;
            let result = writes.result;
            let scheduling_ack = proof.finish_tx(tx, &writes.wake_pairs)?;
            let mut queue_entries = Vec::new();
            for id in &result.dropped_message_ids {
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM queue_entries WHERE message_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                let record = decode_queue_entry_payload(&payload)?;
                anyhow::ensure!(
                    record.agent_id == scope.agent_id
                        && record.message_id == *id
                        && record.status == QueueEntryStatus::Dropped,
                    "timer recovery queue ACK mismatch"
                );
                queue_entries.push(record);
            }
            let scope_facts = super::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                .scope_facts()?;
            anyhow::ensure!(
                scope_facts
                    .identity
                    .as_ref()
                    .is_some_and(
                        |identity| identity.incarnation == scope.identity_incarnation
                            && identity.status == AgentRegistryStatus::Active
                    ),
                "timer recovery identity changed inside transaction"
            );
            Ok(ScopedTimerWakeRecovery {
                result,
                queue_entries,
                scope_facts,
                scheduling_ack,
            })
        })
    }

    pub fn fire(&self, command: &TimerFire) -> Result<TimerFireResult> {
        self.fire_with_wait(command, None)
    }

    pub(crate) fn fire_with_wait(
        &self,
        command: &TimerFire,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<TimerFireResult> {
        self.fire_with_wait_inner(command, fault, None)
            .map(|ack| ack.record)
    }

    #[cfg(test)]
    pub(crate) fn fire_with_wait_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        command: &TimerFire,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TimerFireResult>> {
        let task_request = super::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit: work_request.record_limit,
            byte_limit: work_request.byte_limit,
        };
        self.fire_with_wait_in_scope_with_task_request(
            scope,
            command,
            work_request,
            &task_request,
            ledger,
            fault,
        )
    }

    #[cfg(test)]
    pub(crate) fn fire_with_wait_in_scope_with_task_request(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        command: &TimerFire,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_request: &super::task_snapshot::TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TimerFireResult>> {
        self.fire_with_wait_inner(
            command,
            fault,
            Some((scope, work_request, task_request, ledger)),
        )
    }

    fn fire_with_wait_inner(
        &self,
        command: &TimerFire,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
        scoped_work: Option<(
            &crate::runtime::resident_input_evidence::InputEvidenceScope,
            &super::work_snapshot::WorkDomainLoadRequest,
            &super::task_snapshot::TaskDomainLoadRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TimerFireResult>> {
        self.db.transaction(|tx| {
            self.fire_with_wait_tx(
                tx,
                command,
                fault,
                scoped_work,
                false,
                &mut WaitConditionWrites::default(),
                &mut Vec::new(),
                None,
            )
        })
    }

    /// Same original business kernel; the Prepared outer TX owns Work finalization.
    pub(crate) fn fire_with_wait_tx(
        &self,
        tx: &Transaction<'_>,
        command: &TimerFire,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
        scoped_work: Option<(
            &crate::runtime::resident_input_evidence::InputEvidenceScope,
            &super::work_snapshot::WorkDomainLoadRequest,
            &super::task_snapshot::TaskDomainLoadRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
        defer_work: bool,
        actual_wait_writes: &mut WaitConditionWrites,
        actual_audits: &mut Vec<AuditEvent>,
        memory: Option<&mut super::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TimerFireResult>> {
        self.fire_with_wait_with_current_read_tx(
            tx,
            command,
            fault,
            scoped_work,
            defer_work,
            actual_wait_writes,
            actual_audits,
            memory,
            None,
        )
    }

    pub(crate) fn fire_with_wait_with_current_read_tx(
        &self,
        tx: &Transaction<'_>,
        command: &TimerFire,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
        scoped_work: Option<(
            &crate::runtime::resident_input_evidence::InputEvidenceScope,
            &super::work_snapshot::WorkDomainLoadRequest,
            &super::task_snapshot::TaskDomainLoadRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
        defer_work: bool,
        actual_wait_writes: &mut WaitConditionWrites,
        actual_audits: &mut Vec<AuditEvent>,
        mut memory: Option<&mut super::dispatch_memory::DispatchMemoryTransactionBudget>,
        mut current_reads: Option<&mut CurrentReadBusinessTransaction<'_, '_>>,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TimerFireResult>> {
        if let Some((scope, _, _, _)) = scoped_work {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &command.expected.agent_id,
            )?;
        }
        if timer_tx(tx, &command.expected.id)?.as_ref() != Some(&command.expected)
            || command.expected.status != TimerStatus::Active
        {
            return Ok(super::input_evidence_writes::InputEvidenceCommit {
                work_delta: None,
                task_delta: None,
                work_material_budget_views: None,
                task_material_budget_views: Vec::new(),
                record: TimerFireResult {
                    advanced: false,
                    wake_created: false,
                    message: None,
                },
                input_delta: Default::default(),
                agent_state: None,
            });
        }
        if command.record.id != command.expected.id
            || command.record.agent_id != command.expected.agent_id
            || command.record.fire_count != command.expected.fire_count + 1
        {
            return Err(anyhow!("invalid timer fire transition"));
        }
        let pending = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM timer_wakes WHERE timer_id = ?1 AND status = 'pending')",
            [&command.record.id],
            |row| row.get::<_, bool>(0),
        )?;
        if pending {
            upsert_timer_tx(tx, &command.record)?;
            return Ok(super::input_evidence_writes::InputEvidenceCommit {
                work_delta: None,
                task_delta: None,
                work_material_budget_views: None,
                task_material_budget_views: Vec::new(),
                record: TimerFireResult {
                    advanced: true,
                    wake_created: false,
                    message: None,
                },
                input_delta: Default::default(),
                agent_state: None,
            });
        }
        let actual_agent_state = tx
            .query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
                [&command.record.agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| serde_json::from_str::<AgentState>(&value))
            .transpose()?;
        if actual_agent_state.as_ref() != Some(&command.agent_state.0) {
            return Err(RuntimeStateTransitionConflict::concurrent_mutation(
                "agent_state",
                &command.record.agent_id,
            )
            .into());
        }
        if command.message.agent_id != command.record.agent_id
            || command.queue_entry.agent_id != command.record.agent_id
        {
            return Err(anyhow!("invalid timer wake message or queue entry"));
        }
        if let Some((scope, _, _, _)) = scoped_work {
            // A retried append cannot grant a same-name historical Message
            // to a Fresh successor or resurrect a pruned evidence ID.
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    &command.message.id,
                ),
            )?;
        }
        // Freeze the original reducer's possible Wait writes before it
        // changes any qualified body. Existing admission/no-match is empty.
        let (predicted_wait, projection_guard) =
            if let Some((scope, request, _, ledger)) = scoped_work.filter(|_| !defer_work) {
                super::transitions::predicted_trigger_wait_tx(
                    tx,
                    scope,
                    &command.message,
                    &command.queue_entry,
                    request,
                    ledger,
                    memory.as_deref_mut(),
                )?
            } else {
                (None, Vec::new())
            };
        // A no-match still materializes the original reducer's pool below.
        let _projection_guard = projection_guard;
        let work_proof = if let (Some((scope, _, _, _)), Some(wait)) = (scoped_work, predicted_wait)
        {
            let keys = super::work_snapshot::work_wait_write_keys_tx(tx, &[wait])?;
            Some(super::work_snapshot::prepare_work_write_tx(
                tx, scope, &keys,
            )?)
        } else {
            None
        };
        upsert_timer_tx(tx, &command.record)?;
        inject_fault(
            fault,
            crate::runtime_db::transitions::TransitionFaultPoint::AfterValidation,
        )?;
        let wake = trigger_wait_and_enqueue_with_current_read_tx(
            tx,
            &command.message,
            &command.queue_entry,
            None,
            current_reads.as_deref_mut(),
        )?;
        if wake.message_inserted {
            insert_runtime_index_changes_tx(tx, &[RuntimeIndexChange::for_message(&wake.message)])?;
        }
        let now = timestamp(command.queue_entry.updated_at);
        tx.execute(
            "INSERT INTO timer_wakes
                 (timer_id, message_id, fire_count, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
            params![
                command.record.id,
                command.message.id,
                command.record.fire_count as i64,
                now
            ],
        )?;
        upsert_agent_state_tx(tx, &command.agent_state.1)?;
        actual_wait_writes.extend(wake.wait_writes.clone());
        actual_audits.extend(wake.audit_events);
        let wait_writes = wake.wait_writes;
        let result = TimerFireResult {
            advanced: true,
            wake_created: true,
            message: Some(wake.message),
        };
        let acknowledgement = if let Some((scope, work_request, task_request, ledger)) = scoped_work
        {
            let actual = committed_message_tx(tx, &scope.agent_id, &command.message.id)?;
            let facts =
                [super::input_evidence_snapshot::CanonicalInputEvidence::Message(actual.clone())];
            let mut acknowledgement = super::input_evidence_writes::acknowledge_input_evidence_tx(
                tx,
                scope,
                TimerFireResult {
                    message: Some(actual),
                    ..result
                },
                &facts,
            )?;
            let payload: String = tx.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                [&scope.agent_id],
                |row| row.get(0),
            )?;
            let actual: AgentState = serde_json::from_str(&payload)?;
            let mut expected_ack = command.agent_state.1.clone();
            expected_ack.total_message_count = actual.total_message_count;
            anyhow::ensure!(
                actual == expected_ack,
                "timer fire State ACK differs from the committed wake projection"
            );
            if !defer_work && !wait_writes.wait_ids.is_empty() {
                let proof = work_proof
                    .as_ref()
                    .context("timer Wait writes omitted their original Work proof")?;
                let actual_keys = wait_writes
                    .wait_ids
                    .iter()
                    .map(|id| {
                        crate::runtime::resident_work::WorkKey::new(
                            crate::runtime::resident_work::WorkFamily::Wait,
                            id,
                        )
                    })
                    .collect();
                acknowledgement.work_delta = Some(
                    super::work_snapshot::register_work_writes_tx(
                        tx,
                        proof,
                        &actual_keys,
                        &Default::default(),
                        &actual,
                        work_request,
                    )?
                    .delta,
                );
            }
            if let Some((delta, guards)) = super::task_snapshot::register_task_source_metadata_tx(
                tx,
                scope,
                &acknowledgement.input_delta,
                task_request,
                ledger,
                memory.as_deref_mut(),
            )? {
                acknowledgement.task_delta = Some(delta);
                acknowledgement.task_material_budget_views = guards;
            }
            acknowledgement.agent_state = Some(super::transitions::AgentStateMutation {
                expected: Some(Box::new(command.agent_state.0.clone())),
                record: Box::new(actual),
            });
            acknowledgement
        } else {
            super::input_evidence_writes::InputEvidenceCommit {
                work_delta: None,
                task_delta: None,
                work_material_budget_views: None,
                task_material_budget_views: Vec::new(),
                record: result,
                input_delta: Default::default(),
                agent_state: None,
            }
        };
        inject_fault(
            fault,
            crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites,
        )?;
        Ok(acknowledgement)
    }

    pub fn cancel(&self, command: &TimerCancel) -> Result<TimerCancelResult> {
        self.db.transaction(|tx| self.cancel_tx(tx, command))
    }

    pub(crate) fn cancel_tx(
        &self,
        tx: &Transaction<'_>,
        command: &TimerCancel,
    ) -> Result<TimerCancelResult> {
        self.cancel_with_current_read_tx(tx, command, None)
    }

    pub(crate) fn cancel_with_current_read_tx(
        &self,
        tx: &Transaction<'_>,
        command: &TimerCancel,
        mut current_reads: Option<&mut CurrentReadBusinessTransaction<'_, '_>>,
    ) -> Result<TimerCancelResult> {
        if timer_tx(tx, &command.expected.id)?.as_ref() != Some(&command.expected)
            || command.expected.status != TimerStatus::Active
        {
            return Ok(TimerCancelResult {
                cancelled: false,
                dropped_message_id: None,
            });
        }
        if command.record.id != command.expected.id
            || command.record.agent_id != command.expected.agent_id
            || command.record.status != TimerStatus::Cancelled
            || command.record.fire_count != command.expected.fire_count
        {
            return Err(anyhow!("invalid timer cancellation transition"));
        }
        if let Some((expected, record)) = command.agent_state.as_ref() {
            let actual = tx
                .query_row(
                    "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
                    [&record.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|value| serde_json::from_str::<AgentState>(&value))
                .transpose()?;
            if actual.as_ref() != Some(expected) {
                return Err(RuntimeStateTransitionConflict::concurrent_mutation(
                    "agent_state",
                    &record.id,
                )
                .into());
            }
        }
        upsert_timer_tx(tx, &command.record)?;
        let now = timestamp(timer_updated_at(&command.record));
        let message_id = tx
            .query_row(
                "SELECT message_id FROM timer_wakes WHERE timer_id = ?1 AND status = 'pending'",
                [&command.record.id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        tx.execute(
            "UPDATE timer_wakes SET status = 'cancelled', updated_at = ?2, cancelled_at = ?2
                 WHERE timer_id = ?1 AND status = 'pending'",
            params![command.record.id, now],
        )?;
        if let Some(message_id) = message_id.as_deref() {
            let payload = tx
                .query_row(
                    "SELECT payload_json FROM queue_entries WHERE message_id = ?1
                     AND status IN ('queued', 'interrupted')",
                    [message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(payload) = payload {
                let mut entry = decode_queue_entry_payload(&payload)?;
                entry.status = QueueEntryStatus::Dropped;
                entry.updated_at = timer_updated_at(&command.record);
                crate::runtime_db::transitions::upsert_queue_entry_tx_with_current_reads(
                    tx,
                    &entry,
                    current_reads.as_deref_mut(),
                )?;
            }
        }
        if let Some((_, record)) = command.agent_state.as_ref() {
            upsert_agent_state_tx(tx, record)?;
        }
        Ok(TimerCancelResult {
            cancelled: true,
            dropped_message_id: message_id,
        })
    }

    pub fn import_legacy(&self, records: Vec<TimerRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("timers", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("timers", "jsonl", "db", |tx| {
                let latest = reduce_timer_records(records);
                for record in latest.values() {
                    upsert_timer_tx(tx, record)?;
                    super::scheduling_snapshot::record_scheduling_import_tx(
                        tx,
                        super::scheduling_snapshot::SchedulingImportRecord::Timer(record),
                    )?;
                }
                let active_records = latest
                    .values()
                    .filter(|record| record.status == TimerStatus::Active)
                    .count();
                Ok(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                }))
            })
    }

    pub fn upsert(&self, record: &TimerRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_timer_tx(tx, record))
    }

    pub fn latest(&self, timer_id: &str) -> Result<Option<TimerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM timers WHERE timer_id = ?1",
                [timer_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_timer_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<TimerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TimerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl TurnRecordRepository<'_> {
    pub fn import_legacy(
        &self,
        messages: Vec<serde_json::Value>,
        tool_executions: Vec<ToolExecutionRecord>,
        briefs: Vec<BriefRecord>,
        delivery_summaries: Vec<DeliverySummaryRecord>,
        wait_conditions: Vec<WaitConditionRecord>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("turn_records", "db")? {
            return Ok(());
        }
        // One original pre-Host actor, not an Input scope or a second source
        // provider. The fixed header guard survives the complete/failed marker.
        let scratch = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
            Default::default(),
        )?;
        let _checkpoint_material = scratch.register_host_owned_bytes(
            crate::runtime::input_evidence_budget::InputMemoryCategory::Candidate,
            crate::runtime::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: 4096,
            },
        )?;
        let mut failed_material = None;
        let mut failed_row = None;
        let result =
            self.db
                .run_storage_domain_import("turn_records", "jsonl-derived", "db", |tx| {
                    let mut material =
                        super::input_evidence_snapshot::legacy_delivery::original_episode::received_turns::ReceivedTurnMaterial::new(
                            &scratch,
                            &messages,
                            &tool_executions,
                            &briefs,
                            &delivery_summaries,
                            &wait_conditions,
                        )?;
                    let imported = (|| {
                        let records = derive_turn_records_from_legacy_evidence_observed(
                            messages,
                            tool_executions,
                            briefs,
                            delivery_summaries,
                            wait_conditions,
                            Some(&mut |bytes, nodes| material.observe_derived(bytes, nodes)),
                        )?;
                        material.source_consumed();
                        for record in &records {
                            let mut row_material =
                                material.existing_turn_tx(tx, &record.turn_id)?;
                            let written = (|| {
                                let record = merge_legacy_turn_evidence_tx(tx, record)?;
                                material.assignment_material_tx(tx, &record, &mut row_material)?;
                                upsert_turn_record_tx(tx, &record)
                            })();
                            if let Err(error) = written {
                                failed_row = Some(row_material);
                                return Err(error);
                            }
                        }
                        Ok(serde_json::json!({
                            "imported_records": records.len(),
                            "source": "legacy evidence jsonl",
                            "ignored": "turns.jsonl"
                        }))
                    })();
                    if imported.is_err() {
                        // The original failed-marker actor formats the real error
                        // after rollback; retain its original allocation estimates.
                        failed_material = Some(material);
                    }
                    imported
                });
        drop((failed_row, failed_material));
        result
    }

    pub fn upsert(&self, record: &TurnRecord) -> Result<()> {
        // Legacy import may run before conversation projection tables exist.
        self.db.transaction(|tx| upsert_turn_record_tx(tx, record))
    }

    pub fn upsert_committed(&self, record: &TurnRecord) -> Result<CommittedTurnRecord> {
        self.db.transaction(|tx| {
            upsert_turn_record_tx(tx, record)?;
            committed_turn_record_tx(tx, &record.agent_id, &record.turn_id)
        })
    }

    pub fn by_id(&self, agent_id: Option<&str>, turn_id: &str) -> Result<Option<TurnRecord>> {
        let connection = self.db.connection()?;
        context_turn_by_id_on(&connection, agent_id, turn_id)
    }

    pub fn recent_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TurnRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.db.connection()?;
        context_recent_turns_on(&connection, agent_id, limit)
    }

    pub fn recent_for_owner(
        &self,
        agent_id: &str,
        owner: &crate::types::TurnOwner,
        limit: usize,
    ) -> Result<Vec<TurnRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.db.connection()?;
        context_owner_turns_on(&connection, agent_id, owner, limit)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM turn_records
             ORDER BY turn_index DESC, created_at DESC, turn_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records = rows
            .map(|row| decode_turn_record_payload(&row?))
            .collect::<Result<Vec<_>>>()?;
        records.reverse();
        Ok(records)
    }
}

impl MessageRepository<'_> {
    pub fn import_legacy(&self, messages: Vec<serde_json::Value>) -> Result<()> {
        if self.db.storage_domain_is_complete("messages", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("messages", "jsonl", "db", |tx| {
                let mut imported_messages = 0_u64;
                let mut dropped_messages = 0_u64;
                let mut work_writes = super::episode_snapshot::work_checkpoint::imports::ImportWrites::default();
                for raw_message in messages {
                    match normalize_legacy_message_value(raw_message)? {
                        Some(message) => {
                            upsert_message_tx(tx, &message)?;
                            work_writes.record("message", &message.id, &message.agent_id,
                                super::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite)?;
                            imported_messages += 1;
                        }
                        None => dropped_messages += 1,
                    }
                }
                Ok(serde_json::json!({
                    "imported_messages": imported_messages,
                    "dropped_messages": dropped_messages,
                    "work_domain_actual_writes": work_writes.finish(tx)?,
                }))
            })
    }

    pub fn upsert(&self, message: &MessageEnvelope) -> Result<()> {
        self.upsert_committed(message).map(|_| ())
    }

    pub fn upsert_committed(&self, message: &MessageEnvelope) -> Result<MessageEnvelope> {
        self.db.transaction(|tx| {
            upsert_message_tx(tx, message)?;
            committed_message_tx(tx, &message.agent_id, &message.id)
        })
    }

    pub fn append_with_index_changes(
        &self,
        message: &MessageEnvelope,
        changes: &[RuntimeIndexChange],
    ) -> Result<MessageEnvelope> {
        self.db.transaction(|tx| {
            let (appended, inserted) = append_message_tx(tx, message)?;
            if inserted {
                insert_runtime_index_changes_tx(tx, changes)?;
            }
            committed_message_tx(tx, &appended.agent_id, &appended.id)
        })
    }

    pub fn upsert_with_index_changes(
        &self,
        message: &MessageEnvelope,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.upsert_with_index_changes_committed(message, changes)
            .map(|_| ())
    }

    pub fn upsert_with_index_changes_committed(
        &self,
        message: &MessageEnvelope,
        changes: &[RuntimeIndexChange],
    ) -> Result<MessageEnvelope> {
        self.db.transaction(|tx| {
            upsert_message_tx(tx, message)?;
            insert_runtime_index_changes_tx(tx, changes)?;
            committed_message_tx(tx, &message.agent_id, &message.id)
        })
    }

    pub fn upsert_many(&self, messages: &[MessageEnvelope]) -> Result<()> {
        self.upsert_many_committed(messages).map(|_| ())
    }

    pub fn upsert_many_committed(
        &self,
        messages: &[MessageEnvelope],
    ) -> Result<Vec<MessageEnvelope>> {
        self.db.transaction(|tx| {
            for message in messages {
                upsert_message_tx(tx, message)?;
            }
            let mut seen = std::collections::BTreeSet::new();
            messages
                .iter()
                .filter(|message| seen.insert((&message.agent_id, &message.id)))
                .map(|message| committed_message_tx(tx, &message.agent_id, &message.id))
                .collect()
        })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<MessageEnvelope>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY message_seq IS NULL ASC, message_seq DESC, created_at DESC, message_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY message_seq IS NULL ASC, message_seq DESC, created_at DESC, message_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }

    pub fn from(
        &self,
        agent_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<MessageEnvelope>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC
                 LIMIT -1 OFFSET ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, offset], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC
                 LIMIT -1 OFFSET ?1",
            )?;
            let rows = statement.query_map([offset], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        if records.len() > limit {
            records.drain(0..(records.len() - limit));
        }
        Ok(records)
    }

    pub fn all(&self, agent_id: Option<&str>) -> Result<Vec<MessageEnvelope>> {
        let connection = self.db.connection()?;
        context_anchor_messages_on(&connection, agent_id)
    }

    pub fn all_values(&self, agent_id: Option<&str>) -> Result<Vec<serde_json::Value>> {
        self.all(agent_id)?
            .into_iter()
            .map(|message| serde_json::to_value(message).map_err(Into::into))
            .collect()
    }

    pub fn by_id(
        &self,
        agent_id: Option<&str>,
        message_id: &str,
    ) -> Result<Option<MessageEnvelope>> {
        let connection = self.db.connection()?;
        context_message_by_id_on(&connection, agent_id, message_id)
    }

    pub fn for_turn(
        &self,
        agent_id: &str,
        turn_id: &str,
        source_message_id: Option<&str>,
    ) -> Result<Vec<MessageEnvelope>> {
        let connection = self.db.connection()?;
        let mut records = if let Some(source_message_id) = source_message_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                   AND (turn_id = ?2 OR message_id = ?3)
                 ORDER BY message_seq IS NULL DESC,
                          message_seq ASC,
                          created_at ASC,
                          message_id ASC",
            )?;
            let records = statement
                .query_map(params![agent_id, turn_id, source_message_id], |row| {
                    row.get::<_, String>(0)
                })?
                .map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?;
            records
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1 AND turn_id = ?2
                 ORDER BY message_seq IS NULL DESC,
                          message_seq ASC,
                          created_at ASC,
                          message_id ASC",
            )?;
            let records = statement
                .query_map(params![agent_id, turn_id], |row| row.get::<_, String>(0))?
                .map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?;
            records
        };
        records.dedup_by(|left, right| left.id == right.id);
        Ok(records)
    }

    pub fn count(&self, agent_id: Option<&str>) -> Result<usize> {
        let connection = self.db.connection()?;
        let count: i64 = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT COUNT(*) FROM messages WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?
        };
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub fn max_message_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        let connection = self.db.connection()?;
        let max_seq: Option<i64> = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(message_seq) FROM messages WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row("SELECT MAX(message_seq) FROM messages", [], |row| {
                row.get(0)
            })?
        };
        Ok(max_seq.unwrap_or_default().max(0) as u64)
    }
}

impl TranscriptRepository<'_> {
    pub fn import_legacy(&self, entries: Vec<TranscriptEntry>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("transcript_entries", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("transcript_entries", "jsonl", "db", |tx| {
                let mut work_writes = super::episode_snapshot::work_checkpoint::imports::ImportWrites::default();
                for entry in &entries {
                    upsert_transcript_entry_tx(tx, entry)?;
                    work_writes.record("transcript", &entry.id, &entry.agent_id,
                        super::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite)?;
                }
                Ok(serde_json::json!({
                    "imported_transcript_entries": entries.len(),
                    "work_domain_actual_writes": work_writes.finish(tx)?,
                }))
            })
    }

    pub fn upsert(&self, entry: &TranscriptEntry) -> Result<()> {
        self.upsert_committed(entry).map(|_| ())
    }

    pub fn upsert_committed(&self, entry: &TranscriptEntry) -> Result<TranscriptEntry> {
        self.db.transaction(|tx| {
            upsert_transcript_entry_tx(tx, entry)?;
            committed_transcript_tx(tx, &entry.agent_id, &entry.id)
        })
    }

    pub fn append(&self, entry: &TranscriptEntry) -> Result<TranscriptEntry> {
        self.db.transaction(|tx| {
            append_transcript_entry_tx(tx, entry)?;
            committed_transcript_tx(tx, &entry.agent_id, &entry.id)
        })
    }

    pub fn upsert_many(&self, entries: &[TranscriptEntry]) -> Result<()> {
        self.upsert_many_committed(entries).map(|_| ())
    }

    pub fn upsert_many_committed(
        &self,
        entries: &[TranscriptEntry],
    ) -> Result<Vec<TranscriptEntry>> {
        self.db.transaction(|tx| {
            for entry in entries {
                upsert_transcript_entry_tx(tx, entry)?;
            }
            let mut seen = std::collections::BTreeSet::new();
            entries
                .iter()
                .filter(|entry| seen.insert((&entry.agent_id, &entry.id)))
                .map(|entry| committed_transcript_tx(tx, &entry.agent_id, &entry.id))
                .collect()
        })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<TranscriptEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.db.connection()?;
        context_recent_transcript_on(&connection, agent_id, limit)
    }

    pub fn all(&self, agent_id: Option<&str>) -> Result<Vec<TranscriptEntry>> {
        let connection = self.db.connection()?;
        if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) ASC, created_at ASC, evidence_id ASC",
            )?;
            let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect()
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) ASC, created_at ASC, evidence_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect()
        }
    }

    pub fn for_turn_ids(
        &self,
        agent_id: &str,
        turn_ids: &[String],
    ) -> Result<Vec<TranscriptEntry>> {
        if turn_ids.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.db.connection()?;
        context_transcript_for_turns_on(&connection, agent_id, turn_ids)
    }

    pub fn by_id(&self, agent_id: Option<&str>, entry_id: &str) -> Result<Option<TranscriptEntry>> {
        let connection = self.db.connection()?;
        context_transcript_by_id_on(&connection, agent_id, entry_id)
    }

    pub fn max_transcript_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        let connection = self.db.connection()?;
        let max_seq: Option<i64> = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(transcript_seq) FROM transcript_entries WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row(
                "SELECT MAX(transcript_seq) FROM transcript_entries",
                [],
                |row| row.get(0),
            )?
        };
        Ok(max_seq.unwrap_or_default().max(0) as u64)
    }
}

impl EvidenceRepository<'_> {
    pub fn import_legacy(
        &self,
        messages: Vec<serde_json::Value>,
        transcript_entries: Vec<TranscriptEntry>,
        tool_executions: Vec<ToolExecutionRecord>,
        briefs: Vec<BriefRecord>,
        delivery_summaries: Vec<DeliverySummaryRecord>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("evidence", "db")? {
            return Ok(());
        }
        // One actual pre-Host Evidence import owns the shared scratch ledger.
        // It is never a runtime identity scope or publication authority.
        let input_import_scratch =
            crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
                Default::default(),
            )?;
        let _input_import_header = input_import_scratch.register_host_owned_bytes(
            crate::runtime::input_evidence_budget::InputMemoryCategory::Candidate,
            crate::runtime::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: 4096,
            },
        )?;
        // The received adapter still owns every input collection through the
        // original transaction. Admit that complete borrowed source first.
        let received = super::input_evidence_snapshot::legacy_delivery::original_episode::received_evidence_material(
            &messages, &transcript_entries, &tool_executions, &briefs, &delivery_summaries,
        )?;
        let _received_guard = super::input_evidence_snapshot::legacy_delivery::imports::observe_existing_import_metadata(&input_import_scratch, received)?;
        let mut input_import_checkpoint_guard = None;
        let result = self.db
            .run_storage_domain_import("evidence", "jsonl", "db", |tx| {
                let mut imported_messages = 0_u64;
                let mut dropped_messages = 0_u64;
                let mut work_writes = super::episode_snapshot::work_checkpoint::imports::ImportWrites::default();
                for raw_message in messages {
                    match normalize_legacy_message_value(raw_message)? {
                        Some(message) => {
                            insert_message_evidence_tx(tx, &message)?;
                            work_writes.record("message", &message.id, &message.agent_id,
                                super::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite)?;
                            imported_messages += 1;
                        }
                        None => dropped_messages += 1,
                    }
                }
                for entry in &transcript_entries {
                    insert_transcript_evidence_tx(tx, entry)?;
                    work_writes.record("transcript", &entry.id, &entry.agent_id,
                        super::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite)?;
                }
                for record in &tool_executions {
                    insert_tool_evidence_tx(tx, record)?;
                }
                for brief in &briefs {
                    let existed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM briefs WHERE evidence_id=?1)", [&brief.id], |row| row.get(0))?;
                    insert_brief_evidence_tx(tx, brief)?;
                    work_writes.record("brief", &brief.id, &brief.agent_id, if existed {
                        super::episode_snapshot::work_checkpoint::imports::ImportOrigin::LegacyImportEqual
                    } else { super::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite })?;
                }
                let work_prior = work_writes.input_import_material_estimate()?;
                let prior_records = super::input_evidence_snapshot::legacy_delivery::original_episode::received_joint_records(received.1, work_prior.1)?;
                let work_prior_guard = super::input_evidence_snapshot::legacy_delivery::imports::observe_existing_import_metadata(&input_import_scratch, work_prior)?;
                let delivery_import = super::input_evidence_snapshot::legacy_delivery::imports::LegacyDeliveryImport::begin_if_installed(tx, &input_import_scratch, prior_records)?;
                for summary in &delivery_summaries {
                    if let Some(original) = delivery_import.as_ref() {
                        original.append_original(summary)?;
                    } else {
                        insert_delivery_summary_evidence_tx(tx, summary)?;
                    }
                }
                let legacy_delivery_source = delivery_import.map(|original| original.finish()).transpose()?;
                let work_checkpoint = work_writes.finish(tx)?;
                drop(work_prior_guard);
                let checkpoint_cost = super::input_evidence_snapshot::legacy_delivery::imports::checkpoint_material(&work_checkpoint)?;
                super::input_evidence_snapshot::legacy_delivery::original_episode::received_joint_records(received.1, checkpoint_cost.1)?;
                input_import_checkpoint_guard = Some(super::input_evidence_snapshot::legacy_delivery::imports::observe_existing_import_metadata(&input_import_scratch, checkpoint_cost)?);
                Ok(serde_json::json!({
                    "imported_messages": imported_messages,
                    "dropped_messages": dropped_messages,
                    "imported_transcript_entries": transcript_entries.len(),
                    "imported_tool_executions": tool_executions.len(),
                    "imported_briefs": briefs.len(),
                    "imported_delivery_summaries": delivery_summaries.len(),
                    "work_domain_actual_writes": work_checkpoint,
                    "legacy_delivery_source": legacy_delivery_source,
                }))
            });
        // The actual complete checkpoint serialization and SQL commit have
        // finished before this final owned checkpoint observation is released.
        drop(input_import_checkpoint_guard);
        result
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        self.append_message_committed(message).map(|_| ())
    }

    pub fn append_message_committed(&self, message: &MessageEnvelope) -> Result<MessageEnvelope> {
        self.db.transaction(|tx| {
            insert_message_evidence_tx(tx, message)?;
            committed_message_tx(tx, &message.agent_id, &message.id)
        })
    }

    pub fn append_transcript_entry(&self, entry: &TranscriptEntry) -> Result<()> {
        self.append_transcript_entry_committed(entry).map(|_| ())
    }

    pub fn append_transcript_entry_committed(
        &self,
        entry: &TranscriptEntry,
    ) -> Result<TranscriptEntry> {
        self.db.transaction(|tx| {
            insert_transcript_evidence_tx(tx, entry)?;
            committed_transcript_tx(tx, &entry.agent_id, &entry.id)
        })
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        self.append_tool_execution_committed(record).map(|_| ())
    }

    pub fn append_tool_execution_committed(
        &self,
        record: &ToolExecutionRecord,
    ) -> Result<ToolExecutionRecord> {
        self.db.transaction(|tx| {
            insert_tool_evidence_tx(tx, record)?;
            committed_tool_tx(tx, &record.agent_id, &record.id)
        })
    }

    pub fn append_tool_execution_with_index_changes(
        &self,
        record: &ToolExecutionRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.append_tool_execution_with_index_changes_committed(record, changes)
            .map(|_| ())
    }

    pub fn append_tool_execution_with_index_changes_committed(
        &self,
        record: &ToolExecutionRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<ToolExecutionRecord> {
        self.db.transaction(|tx| {
            insert_tool_evidence_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)?;
            committed_tool_tx(tx, &record.agent_id, &record.id)
        })
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        self.append_brief_committed(brief).map(|_| ())
    }

    pub fn append_brief_committed(&self, brief: &BriefRecord) -> Result<BriefRecord> {
        self.db.transaction(|tx| {
            insert_brief_evidence_tx(tx, brief)?;
            committed_brief_tx(tx, &brief.agent_id, &brief.id)
        })
    }

    pub fn append_brief_with_index_changes(
        &self,
        brief: &BriefRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.append_brief_with_index_changes_committed(brief, changes)
            .map(|_| ())
    }

    pub fn append_brief_with_index_changes_committed(
        &self,
        brief: &BriefRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<BriefRecord> {
        self.db.transaction(|tx| {
            insert_brief_evidence_tx(tx, brief)?;
            insert_runtime_index_changes_tx(tx, changes)?;
            committed_brief_tx(tx, &brief.agent_id, &brief.id)
        })
    }

    /// Single-transaction Brief publication: audit event sequence
    /// allocation, the `brief_created` event, the Brief record, and its
    /// immutable `created_event_seq` linkage commit together.
    pub fn append_brief_with_created_event(
        &self,
        agent_id: Option<&str>,
        brief: &BriefRecord,
        event: &crate::types::AuditEvent,
        changes: &[RuntimeIndexChange],
    ) -> Result<crate::runtime_db::evidence::BriefCreatedCommit> {
        self.db.transaction(|tx| {
            append_brief_with_created_event_tx(tx, agent_id, brief, event, changes)
        })
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_delivery_summary_evidence_tx(tx, record))
    }

    pub fn query(&self, kind: EvidenceKind, query: EvidenceQuery<'_>) -> Result<Vec<EvidenceRow>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let mut clauses = Vec::new();
        let mut params = Vec::<String>::new();
        push_optional_clause(&mut clauses, &mut params, "agent_id", query.agent_id);
        push_optional_clause(&mut clauses, &mut params, "turn_id", query.turn_id);
        push_optional_clause(&mut clauses, &mut params, "message_id", query.message_id);
        push_optional_clause(&mut clauses, &mut params, "task_id", query.task_id);
        push_optional_clause(
            &mut clauses,
            &mut params,
            "work_item_id",
            query.work_item_id,
        );
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT evidence_id, agent_id, turn_id, message_id, task_id, work_item_id, created_at, preview
             FROM {}{}
             ORDER BY created_at DESC, evidence_id ASC
             LIMIT {}",
            kind.table_name(),
            where_clause,
            limit
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(EvidenceRow {
                evidence_id: row.get(0)?,
                agent_id: row.get(1)?,
                turn_id: row.get(2)?,
                message_id: row.get(3)?,
                task_id: row.get(4)?,
                work_item_id: row.get(5)?,
                created_at: row.get(6)?,
                preview: row.get(7)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn recent_payloads(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<EvidencePayloadRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.db.connection()?;
        context_recent_evidence_on(&connection, kind, agent_id, limit)
    }

    fn payloads_for_turn(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        turn_id: &str,
    ) -> Result<Vec<EvidencePayloadRow>> {
        let sql = format!(
            "SELECT payload_json
             FROM {}
             WHERE agent_id = ?1 AND turn_id = ?2
             ORDER BY created_at ASC, evidence_id ASC",
            kind.table_name()
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![agent_id, turn_id], |row| {
            Ok(EvidencePayloadRow {
                payload_json: row.get(0)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn payload_by_id(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        evidence_id: &str,
    ) -> Result<Option<EvidencePayloadRow>> {
        let connection = self.db.connection()?;
        context_evidence_by_id_on(&connection, kind, agent_id, evidence_id)
    }

    pub fn payloads_by_ids(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        evidence_ids: &[String],
    ) -> Result<Vec<EvidencePayloadRow>> {
        if evidence_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = evidence_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT payload_json FROM {} WHERE agent_id = ?1 AND evidence_id IN ({})",
            kind.table_name(),
            placeholders
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let mut all_params: Vec<Box<dyn ToSql>> = Vec::with_capacity(1 + evidence_ids.len());
        all_params.push(Box::new(agent_id.to_string()));
        for id in evidence_ids {
            all_params.push(Box::new(id.clone()));
        }
        let param_refs: Vec<&dyn ToSql> = all_params.iter().map(|p| p.as_ref()).collect();
        let rows = statement.query_map(param_refs.as_slice(), |row| {
            Ok(EvidencePayloadRow {
                payload_json: row.get(0)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn recent_briefs(&self, agent_id: &str, limit: usize) -> Result<Vec<BriefRecord>> {
        self.recent_payloads(EvidenceKind::Brief, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn briefs_for_turn(&self, agent_id: &str, turn_id: &str) -> Result<Vec<BriefRecord>> {
        self.payloads_for_turn(EvidenceKind::Brief, agent_id, turn_id)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn brief_by_id(&self, agent_id: &str, brief_id: &str) -> Result<Option<BriefRecord>> {
        self.payload_by_id(EvidenceKind::Brief, agent_id, brief_id)?
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .transpose()
    }

    pub fn briefs_by_ids(&self, agent_id: &str, brief_ids: &[String]) -> Result<Vec<BriefRecord>> {
        self.payloads_by_ids(EvidenceKind::Brief, agent_id, brief_ids)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn recent_tool_executions(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ToolExecutionRecord>> {
        self.recent_payloads(EvidenceKind::ToolExecution, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn tool_executions_for_turn(
        &self,
        agent_id: &str,
        turn_id: &str,
    ) -> Result<Vec<ToolExecutionRecord>> {
        self.payloads_for_turn(EvidenceKind::ToolExecution, agent_id, turn_id)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn tool_execution_by_id(
        &self,
        agent_id: &str,
        tool_id: &str,
    ) -> Result<Option<ToolExecutionRecord>> {
        self.payload_by_id(EvidenceKind::ToolExecution, agent_id, tool_id)?
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .transpose()
    }

    pub fn recent_delivery_summaries(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        self.recent_payloads(EvidenceKind::DeliverySummary, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn latest_delivery_summary(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<Option<DeliverySummaryRecord>> {
        let connection = self.db.connection()?;
        let payload = connection
            .query_row(
                "SELECT payload_json
                 FROM delivery_summaries
                 WHERE agent_id = ?1 AND work_item_id = ?2
                 ORDER BY created_at DESC, evidence_id DESC
                 LIMIT 1",
                params![agent_id, work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .transpose()
    }

    pub fn count_briefs(&self, agent_id: &str) -> Result<usize> {
        let connection = self.db.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM briefs WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }
}

impl AuditEventSink<'_> {
    pub fn append(&self, agent_id: Option<&str>, event: &AuditEvent) -> Result<AuditEvent> {
        self.db.transaction_with_context(
            RuntimeDbWriteContext::sync("audit_events.append", "audit_events"),
            |tx| append_audit_event_tx(tx, agent_id, event).map(|(event, _)| event),
        )
    }

    /// Check whether an audit event with the given id already exists.
    pub fn has_event_by_id(&self, event_id: &str) -> Result<bool> {
        let connection = self.db.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM audit_events WHERE audit_event_id = ?1",
                [event_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn append_many(
        &self,
        agent_id: Option<&str>,
        events: &[AuditEvent],
    ) -> Result<Vec<AuditEvent>> {
        self.db.transaction_with_context(
            RuntimeDbWriteContext::sync("audit_events.append_many", "audit_events"),
            |tx| {
                let mut appended = Vec::with_capacity(events.len());
                for event in events {
                    appended.push(append_audit_event_tx(tx, agent_id, event)?.0);
                }
                Ok(appended)
            },
        )
    }

    pub fn import_legacy(&self, agent_id: Option<&str>, events: Vec<AuditEvent>) -> Result<()> {
        if self.db.storage_domain_is_complete("audit_events", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("audit_events", "jsonl", "db", |tx| {
                for event in &events {
                    import_audit_event_tx(tx, agent_id, event)?;
                }
                Ok(serde_json::json!({ "imported_records": events.len() }))
            })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events");
        if agent_id.is_some() {
            sql.push_str(" WHERE agent_id = ?1");
        }
        sql.push_str(" ORDER BY event_seq DESC, created_at DESC LIMIT ?");
        let mut statement = connection.prepare(&sql)?;
        let mut events = if let Some(agent_id) = agent_id {
            statement
                .query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?
                .map(|row| {
                    let payload = row?;
                    serde_json::from_str(&payload).map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            statement
                .query_map(params![limit], |row| row.get::<_, String>(0))?
                .map(|row| {
                    let payload = row?;
                    serde_json::from_str(&payload).map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()?
        };
        events.reverse();
        Ok(events)
    }

    pub fn latest_event_seq(&self, agent_id: Option<&str>) -> Result<Option<u64>> {
        let connection = self.db.connection()?;
        let value = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(event_seq) FROM audit_events WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, Option<i64>>(0),
            )?
        } else {
            connection.query_row("SELECT MAX(event_seq) FROM audit_events", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?
        };
        value
            .map(|seq| u64::try_from(seq).context("stored audit event sequence is negative"))
            .transpose()
    }

    pub fn max_event_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        Ok(self.latest_event_seq(agent_id)?.unwrap_or(0))
    }

    pub fn range(
        &self,
        agent_id: Option<&str>,
        before_seq: Option<u64>,
        after_seq: Option<u64>,
        descending: bool,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let lower = i64::try_from(after_seq.unwrap_or(0))
            .context("audit event lower cursor exceeds SQLite integer range")?;
        let upper = before_seq
            .map(|seq| {
                i64::try_from(seq).context("audit event upper cursor exceeds SQLite integer range")
            })
            .transpose()?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events WHERE event_seq > ?1");
        if upper.is_some() {
            sql.push_str(" AND event_seq < ?2");
        }
        if agent_id.is_some() {
            let param_index = if upper.is_some() { 3 } else { 2 };
            sql.push_str(&format!(" AND agent_id = ?{param_index}"));
        }
        if descending {
            sql.push_str(" ORDER BY event_seq DESC, created_at DESC");
        } else {
            sql.push_str(" ORDER BY event_seq ASC, created_at ASC");
        }
        let limit_param_index = 2 + usize::from(upper.is_some()) + usize::from(agent_id.is_some());
        sql.push_str(&format!(" LIMIT ?{limit_param_index}"));
        let mut statement = connection.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(lower)];
        if let Some(upper) = upper {
            params.push(Box::new(upper));
        }
        if let Some(agent_id) = agent_id {
            params.push(Box::new(agent_id.to_owned()));
        }
        params.push(Box::new(limit));
        let events = statement
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect();
        events
    }

    pub fn page_after(
        &self,
        agent_id: Option<&str>,
        after_event_seq: u64,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events WHERE event_seq > ?1");
        if agent_id.is_some() {
            sql.push_str(" AND agent_id = ?2");
        }
        let limit_param_index = if agent_id.is_some() { 3 } else { 2 };
        sql.push_str(&format!(
            " ORDER BY event_seq ASC, created_at ASC LIMIT ?{limit_param_index}"
        ));
        let mut statement = connection.prepare(&sql)?;
        if let Some(agent_id) = agent_id {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows = statement.query_map(params![after_event_seq, agent_id, limit], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        } else {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows = statement.query_map(params![after_event_seq, limit], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        }
    }
}

/// Only this repository's successful original SQL write can mint the source.
pub(in crate::runtime_db) struct OriginalDelegationWrite<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    delegation_id: &'tx str,
}
impl OriginalDelegationWrite<'_, '_> {
    pub(in crate::runtime_db) fn source_in<'a>(&'a self, tx: &Transaction<'_>) -> Result<&'a str> {
        anyhow::ensure!(
            std::ptr::eq(self.tx, tx),
            "Delegation source crossed transaction"
        );
        Ok(self.delegation_id)
    }
}

pub(in crate::runtime_db) fn upsert_work_item_delegation_tx(
    tx: &Transaction<'_>,
    record: &WorkItemDelegationRecord,
) -> Result<()> {
    upsert_work_item_delegation_with_ack_tx(tx, record).map(|_| ())
}

pub(in crate::runtime_db) fn upsert_work_item_delegation_with_ack_tx(
    tx: &Transaction<'_>,
    record: &WorkItemDelegationRecord,
) -> Result<bool> {
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    let changed = tx.execute(
        "INSERT INTO work_item_delegations (
            delegation_id, parent_agent_id, parent_work_item_id, child_agent_id,
            child_work_item_id, state, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(delegation_id) DO UPDATE SET
            parent_agent_id = excluded.parent_agent_id,
            parent_work_item_id = excluded.parent_work_item_id,
            child_agent_id = excluded.child_agent_id,
            child_work_item_id = excluded.child_work_item_id,
            state = excluded.state,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= work_item_delegations.updated_at",
        params![
            record.delegation_id,
            record.parent_agent_id,
            record.parent_work_item_id,
            record.child_agent_id,
            record.child_work_item_id,
            state,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    if changed == 1 {
        // Older schema fixtures and the upgrade collector precede Task schema.
        // They receive their fixed inventory at the migration cut instead.
        let installed: bool = tx.query_row(
            "SELECT COALESCE(MAX(version),0)>=?1 FROM schema_migrations",
            [super::task_snapshot::TASK_DOMAIN_VERSION],
            |row| row.get(0),
        )?;
        if installed {
            let observed = OriginalDelegationWrite {
                tx,
                delegation_id: &record.delegation_id,
            };
            super::task_snapshot::register_delegation_write_tx(tx, &observed)?;
        }
    }
    Ok(changed == 1)
}

pub(crate) fn upsert_work_item_continuation_tx(
    tx: &Transaction<'_>,
    record: &WorkItemContinuationFrame,
) -> Result<bool> {
    let payload_json = serde_json::to_string(record)?;
    let existing = tx
        .query_row(
            "SELECT payload_json FROM work_item_continuations WHERE continuation_id = ?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing.as_deref() == Some(payload_json.as_str()) {
        return Ok(false);
    }
    let return_policy = enum_string(&record.return_policy)?;
    let state = enum_string(&record.state)?;
    let changed = tx.execute(
        "INSERT INTO work_item_continuations (
            continuation_id, agent_id, suspended_work_item_id, active_work_item_id,
            return_policy, state, created_at, updated_at, resolved_at, cancelled_at,
            resolution_reason, last_turn_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(continuation_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            suspended_work_item_id = excluded.suspended_work_item_id,
            active_work_item_id = excluded.active_work_item_id,
            return_policy = excluded.return_policy,
            state = excluded.state,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            resolved_at = excluded.resolved_at,
            cancelled_at = excluded.cancelled_at,
            resolution_reason = excluded.resolution_reason,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= work_item_continuations.updated_at",
        params![
            record.id,
            record.agent_id,
            record.suspended_work_item_id,
            record.active_work_item_id,
            return_policy,
            state,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.resolved_at.map(timestamp),
            record.cancelled_at.map(timestamp),
            record.resolution_reason.as_deref(),
            record.turn_id.as_deref(),
            payload_json,
        ],
    )?;
    Ok(changed != 0)
}

fn upsert_context_episode_tx(tx: &Transaction<'_>, record: &ContextEpisodeRecord) -> Result<()> {
    if super::episode_snapshot::validate_immutable_episode_upsert_tx(tx, record)? {
        return Ok(());
    }
    let payload_json = serde_json::to_string(record)?;
    let boundary_reason = enum_string(&record.boundary_reason)?;
    tx.execute(
        "INSERT INTO context_episode_anchors (
            episode_id, agent_id, workspace_id, work_item_id, boundary_reason,
            start_turn_index, end_turn_index, started_at, ended_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(episode_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            workspace_id = excluded.workspace_id,
            work_item_id = excluded.work_item_id,
            boundary_reason = excluded.boundary_reason,
            start_turn_index = excluded.start_turn_index,
            end_turn_index = excluded.end_turn_index,
            started_at = excluded.started_at,
            ended_at = excluded.ended_at,
            payload_json = excluded.payload_json
         WHERE excluded.ended_at >= context_episode_anchors.ended_at",
        params![
            record.id,
            record.agent_id,
            record.workspace_id,
            record.current_work_item_id,
            boundary_reason,
            record.start_turn_index as i64,
            record.end_turn_index as i64,
            timestamp(record.created_at),
            timestamp(record.finalized_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_external_trigger_tx(
    tx: &Transaction<'_>,
    record: &ExternalTriggerRecord,
) -> Result<()> {
    upsert_external_trigger_with_write_tx(tx, record).map(|_| ())
}

pub(crate) fn upsert_external_trigger_with_write_tx(
    tx: &Transaction<'_>,
    record: &ExternalTriggerRecord,
) -> Result<bool> {
    if record.status == ExternalTriggerStatus::Active {
        super::reclamation::ensure_work_admission_tx(tx, &record.target_agent_id)?;
    }
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let revoked_at = record.revoked_at.map(timestamp);
    let last_delivered_at = record.last_delivered_at.map(timestamp);
    let changed = tx.execute(
        "INSERT INTO external_triggers (
            external_trigger_id, target_agent_id, trigger_url,
            token_hash, status, created_at, revoked_at, last_delivered_at,
           delivery_count, payload_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(external_trigger_id) DO UPDATE SET
            target_agent_id = excluded.target_agent_id,
            trigger_url = excluded.trigger_url,
            token_hash = excluded.token_hash,
            status = excluded.status,
            created_at = excluded.created_at,
            revoked_at = excluded.revoked_at,
            last_delivered_at = excluded.last_delivered_at,
            delivery_count = excluded.delivery_count,
            payload_json = excluded.payload_json
         WHERE excluded.delivery_count > external_triggers.delivery_count
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') > COALESCE(external_triggers.last_delivered_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') > COALESCE(external_triggers.revoked_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') = COALESCE(external_triggers.revoked_at, '')
                AND excluded.created_at >= external_triggers.created_at
            )",
        params![
            record.external_trigger_id,
            record.target_agent_id,
           record.token,
            record.token_hash,
            status,
            timestamp(record.created_at),
            revoked_at,
            last_delivered_at,
            record.delivery_count as i64,
            payload_json,
        ],
    )?;
    Ok(changed == 1)
}

/// Actual affected IDs from the original ensure/reset SQL exits.
pub(crate) fn ensure_default_external_trigger_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    delivery_mode: CallbackDeliveryMode,
    now: DateTime<Utc>,
    external_trigger_id: String,
    token: String,
    actual_ids: &mut BTreeSet<String>,
) -> Result<(ExternalTriggerRecord, Option<ExternalTriggerRecord>, bool)> {
    if let Some(descriptor) = active_default_for_agent_tx(tx, agent_id)? {
        if descriptor.token.is_some() {
            return Ok((descriptor, None, false));
        }

        let mut revoked = descriptor;
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(now);
        if upsert_external_trigger_with_write_tx(tx, &revoked)? {
            actual_ids.insert(revoked.external_trigger_id.clone());
        }

        let descriptor = default_external_trigger_record(
            agent_id,
            delivery_mode.clone(),
            now,
            external_trigger_id.clone(),
            token.clone(),
        );
        if upsert_external_trigger_with_write_tx(tx, &descriptor)? {
            actual_ids.insert(descriptor.external_trigger_id.clone());
        }
        return Ok((descriptor, Some(revoked), true));
    }

    let descriptor = default_external_trigger_record(
        agent_id,
        delivery_mode.clone(),
        now,
        external_trigger_id.clone(),
        token.clone(),
    );
    if upsert_external_trigger_with_write_tx(tx, &descriptor)? {
        actual_ids.insert(descriptor.external_trigger_id.clone());
    }
    Ok((descriptor, None, true))
}
pub(crate) fn reset_default_external_trigger_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    delivery_mode: CallbackDeliveryMode,
    now: DateTime<Utc>,
    external_trigger_id: String,
    token: String,
    actual_ids: &mut BTreeSet<String>,
) -> Result<(ExternalTriggerRecord, Option<ExternalTriggerRecord>)> {
    let revoked = if let Some(descriptor) = active_default_for_agent_tx(tx, agent_id)? {
        let mut revoked = descriptor;
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(now);
        if upsert_external_trigger_with_write_tx(tx, &revoked)? {
            actual_ids.insert(revoked.external_trigger_id.clone());
        }
        Some(revoked)
    } else {
        None
    };

    let descriptor = default_external_trigger_record(
        agent_id,
        delivery_mode.clone(),
        now,
        external_trigger_id.clone(),
        token.clone(),
    );
    if upsert_external_trigger_with_write_tx(tx, &descriptor)? {
        actual_ids.insert(descriptor.external_trigger_id.clone());
    }
    Ok((descriptor, revoked))
}
fn active_default_for_agent_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<Option<ExternalTriggerRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM external_triggers
         WHERE target_agent_id = ?1 AND status = 'active'
         ORDER BY created_at DESC, external_trigger_id ASC
         LIMIT 1",
        [agent_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_external_trigger_payload(&payload))
    .transpose()
}

fn default_external_trigger_record(
    agent_id: &str,
    delivery_mode: CallbackDeliveryMode,
    now: DateTime<Utc>,
    external_trigger_id: String,
    token: String,
) -> ExternalTriggerRecord {
    ExternalTriggerRecord {
        external_trigger_id,
        target_agent_id: agent_id.to_string(),
        scope: ExternalTriggerScope::Agent,
        delivery_mode,
        token: Some(token.clone()),
        token_hash: crate::callbacks::hash_callback_token(&token),
        status: ExternalTriggerStatus::Active,
        created_at: now,
        revoked_at: None,
        last_delivered_at: None,
        delivery_count: 0,
    }
}

fn insert_operator_notification_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    record: &OperatorNotificationRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT OR IGNORE INTO operator_notifications (
            notification_id, agent_id, created_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            record.notification_id,
            agent_id,
            timestamp(record.created_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_operator_transport_binding_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    record: &OperatorTransportBinding,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    tx.execute(
        "INSERT INTO operator_transport_bindings (
            binding_id, target_agent_id, status, created_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(binding_id) DO UPDATE SET
            target_agent_id = excluded.target_agent_id,
            status = excluded.status,
            created_at = excluded.created_at,
            payload_json = excluded.payload_json",
        params![
            record.binding_id,
            agent_id,
            status,
            timestamp(record.created_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_operator_delivery_record_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    record: &OperatorDeliveryRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT INTO operator_delivery_records (
            delivery_intent_id, agent_id, created_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(delivery_intent_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            created_at = excluded.created_at,
            payload_json = excluded.payload_json",
        params![
            record.delivery_intent_id,
            agent_id,
            timestamp(record.created_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_new_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
) -> Result<bool> {
    if record.revision != 1 {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_initial_revision",
            Some(1),
            Some(record.revision),
            false,
        )
        .into());
    }
    let actual_revision = tx
        .query_row(
            "SELECT revision FROM work_items WHERE work_item_id = ?1",
            [&record.id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|revision| u64::try_from(revision).context("stored work item revision is negative"))
        .transpose()?;
    if let Some(actual_revision) = actual_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "record_exists",
            None,
            Some(actual_revision),
            false,
        )
        .into());
    }
    import_work_item_tx(tx, record)?;
    Ok(true)
}

pub(crate) fn update_expected_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    expected_revision: u64,
) -> Result<bool> {
    let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
        RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_revision_transition",
            Some(expected_revision),
            Some(record.revision),
            false,
        )
    })?;
    if record.revision != next_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_revision_transition",
            Some(expected_revision),
            Some(record.revision),
            false,
        )
        .into());
    }

    let existing = tx
        .query_row(
            "SELECT revision, payload_json FROM work_items WHERE work_item_id = ?1",
            [&record.id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((actual_revision, existing_payload)) = existing else {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "not_found",
            Some(expected_revision),
            None,
            false,
        )
        .into());
    };
    let actual_revision =
        u64::try_from(actual_revision).context("stored work item revision is negative")?;
    let payload_json = serde_json::to_string(record)?;
    if actual_revision == next_revision {
        if existing_payload == payload_json {
            return Ok(false);
        }
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "same_revision_payload_conflict",
            Some(expected_revision),
            Some(actual_revision),
            false,
        )
        .into());
    }
    if actual_revision != expected_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "revision_conflict",
            Some(expected_revision),
            Some(actual_revision),
            true,
        )
        .into());
    }

    update_work_item_row_tx(tx, record, &payload_json, expected_revision)?;
    Ok(true)
}

fn update_work_item_row_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    payload_json: &str,
    expected_revision: u64,
) -> Result<()> {
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    let changed = tx.execute(
        "UPDATE work_items SET
            agent_id = ?1,
            state = ?2,
            objective = ?3,
            plan_status = ?4,
            revision = ?5,
            created_at = ?6,
            updated_at = ?7,
            completed_at = ?8,
            plan_artifact_path = ?9,
            last_turn_id = ?10,
            payload_json = ?11,
            blocked_by = ?12,
            recheck_at = ?13,
            recheck_consumed_at = ?14
         WHERE work_item_id = ?15 AND revision = ?16",
        params![
            record.agent_id,
            state,
            record.objective,
            plan_status,
            record.revision as i64,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
            record.blocked_by,
            record.recheck_at.map(timestamp),
            record.recheck_consumed_at.map(timestamp),
            record.id,
            expected_revision as i64,
        ],
    )?;
    if changed != 1 {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "revision_conflict",
            Some(expected_revision),
            None,
            true,
        )
        .into());
    }
    Ok(())
}

enum WorkImportOutcome {
    ActualWrite,
    LegacyEqual,
    Ignored,
}

fn import_work_item_tx(tx: &Transaction<'_>, record: &WorkItemRecord) -> Result<WorkImportOutcome> {
    if record.state != WorkItemState::Completed {
        super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    }
    let existing: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id=?1",
            [&record.id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(payload) = existing {
        let existing: WorkItemRecord = decode_work_item_payload(&payload)?;
        if existing == *record && work_import_projection_equal_tx(tx, record)? {
            return Ok(WorkImportOutcome::LegacyEqual);
        }
    }
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    let blocked_by = record.blocked_by.clone();
    let recheck_at = record.recheck_at.map(|t| timestamp(t));
    let recheck_consumed_at = record.recheck_consumed_at.map(|t| timestamp(t));
    let changed = tx.execute(
        "INSERT INTO work_items (
            work_item_id, agent_id, state, objective, plan_status,
            revision, current_focus, created_at, updated_at, completed_at,
            plan_artifact_path, last_turn_id, payload_json,
            blocked_by, recheck_at, recheck_consumed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(work_item_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            state = excluded.state,
            objective = excluded.objective,
            plan_status = excluded.plan_status,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            plan_artifact_path = excluded.plan_artifact_path,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json,
            blocked_by = excluded.blocked_by,
            recheck_at = excluded.recheck_at,
            recheck_consumed_at = excluded.recheck_consumed_at
         WHERE excluded.revision >= work_items.revision",
        params![
            record.id,
            record.agent_id,
            state,
            record.objective,
            plan_status,
            record.revision as i64,
            0_i64,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
            blocked_by,
            recheck_at,
            recheck_consumed_at,
        ],
    )?;
    Ok(if changed == 1 {
        WorkImportOutcome::ActualWrite
    } else {
        WorkImportOutcome::Ignored
    })
}

fn work_import_projection_equal_tx(tx: &Transaction<'_>, record: &WorkItemRecord) -> Result<bool> {
    let actual: String = tx.query_row(
        "SELECT json_array(agent_id,state,objective,plan_status,revision,created_at,updated_at,completed_at,plan_artifact_path,last_turn_id,blocked_by,recheck_at,recheck_consumed_at) FROM work_items WHERE work_item_id=?1",
        [&record.id], |row| row.get(0),
    )?;
    let expected = serde_json::json!([
        record.agent_id,
        record.state,
        record.objective,
        record.plan_status,
        record.revision,
        timestamp(record.created_at),
        timestamp(record.updated_at),
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at)),
        record
            .plan_artifact
            .as_ref()
            .map(|artifact| artifact.path.display().to_string()),
        record.turn_id,
        record.blocked_by,
        record.recheck_at.map(timestamp),
        record.recheck_consumed_at.map(timestamp)
    ]);
    Ok(serde_json::from_str::<serde_json::Value>(&actual)? == expected)
}

pub(crate) fn upsert_task_tx(tx: &Transaction<'_>, record: &TaskRecord) -> Result<bool> {
    if is_active_task_status(&record.status) {
        super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
        if let Some(child_id) = task_detail_string(&record.detail, "child_agent_id") {
            super::reclamation::ensure_work_admission_tx(tx, &child_id)?;
        }
    }
    let existing = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_task_payload(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        match task_transition(existing, record)? {
            StateTransitionOutcome::Applied => {}
            StateTransitionOutcome::Idempotent => return Ok(false),
        }
    }

    write_task_row_tx(tx, record)
}

pub(crate) fn upsert_interrupted_child_task_stop_tx(
    tx: &Transaction<'_>,
    proof: &super::task_stop::TaskStopWriteProof<'_, '_>,
    record: &TaskRecord,
) -> Result<bool> {
    anyhow::ensure!(
        proof.interrupted_child_cancellation(tx, record)?,
        "TaskStop does not authorize interrupted child cancellation"
    );
    write_task_row_tx(tx, record)
}

pub(in crate::runtime_db) fn update_original_child_monitor_binding_tx(
    tx: &Transaction<'_>,
    proof: &super::transitions::ExistingOriginalChildBindingWriteProof<'_, '_, '_>,
    record: &TaskRecord,
) -> Result<bool> {
    proof.validate_transaction(tx)?;
    let existing = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id=?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|raw| decode_task_payload(&raw))
        .transpose()?
        .context("existing child binding Task disappeared")?;
    proof.validate_existing(tx, &existing, record)?;
    if matches!(
        task_transition(&existing, record)?,
        StateTransitionOutcome::Idempotent
    ) {
        return Ok(false);
    }
    write_task_row_with_mode_tx(tx, record, TaskRowWriteMode::ExistingOriginalBinding)
}
#[derive(Clone, Copy)]
enum TaskRowWriteMode {
    Upsert,
    ExistingOriginalBinding,
}
fn write_task_row_tx(tx: &Transaction<'_>, record: &TaskRecord) -> Result<bool> {
    write_task_row_with_mode_tx(tx, record, TaskRowWriteMode::Upsert)
}
fn write_task_row_with_mode_tx(
    tx: &Transaction<'_>,
    record: &TaskRecord,
    mode: TaskRowWriteMode,
) -> Result<bool> {
    let kind = record.kind.as_str();
    let status = enum_string(&record.status)?;
    let status_phase = i64::from(task_status_phase(&record.status));
    let child_agent_id = task_detail_string(&record.detail, "child_agent_id");
    let parent_agent_id = child_agent_id.as_ref().map(|_| record.agent_id.clone());
    let input_target = task_detail_string(&record.detail, "input_target");
    let wait_policy = enum_string(&record.wait_policy())?;
    let output_path = task_detail_string(&record.detail, "output_path");
    let result_summary = task_detail_string(&record.detail, "output_summary")
        .map(|summary| truncate_task_payload_string(&summary));
    let exit_status = task_detail_i64(&record.detail, "exit_status");
    let terminal_reentry = i64::from(record.terminal_reentry());
    let completed_at =
        is_terminal_task_status(&record.status).then(|| timestamp(record.updated_at));
    let payload_json = serde_json::to_string(&slim_task_record_for_payload(record))?;
    let sql=match mode {TaskRowWriteMode::Upsert=>"INSERT INTO tasks (
            task_id, owner_agent_id, parent_agent_id, child_agent_id, kind, status,
            summary, input_target, wait_policy, output_path, result_summary,
            exit_status, terminal_reentry, revision, created_at, updated_at,
            completed_at, last_message_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(task_id) DO UPDATE SET
            owner_agent_id = excluded.owner_agent_id,
            parent_agent_id = excluded.parent_agent_id,
            child_agent_id = excluded.child_agent_id,
            kind = excluded.kind,
            status = excluded.status,
            summary = excluded.summary,
            input_target = excluded.input_target,
            wait_policy = excluded.wait_policy,
            output_path = excluded.output_path,
            result_summary = excluded.result_summary,
            exit_status = excluded.exit_status,
            terminal_reentry = excluded.terminal_reentry,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            last_message_id = excluded.last_message_id,
            payload_json = excluded.payload_json
         WHERE excluded.revision > tasks.revision
            OR (excluded.revision = tasks.revision AND ?20 >= CASE tasks.status
                WHEN 'queued' THEN 0
                WHEN 'running' THEN 1
                WHEN 'cancelling' THEN 2
                ELSE 3
            END)",TaskRowWriteMode::ExistingOriginalBinding=>"UPDATE tasks SET owner_agent_id=?2,parent_agent_id=?3,child_agent_id=?4,kind=?5,status=?6,
         summary=?7,input_target=?8,wait_policy=?9,output_path=?10,result_summary=?11,exit_status=?12,
         terminal_reentry=?13,revision=?14,created_at=?15,updated_at=?16,completed_at=?17,last_message_id=?18,payload_json=?19
         WHERE task_id=?1 AND (?14>revision OR (?14=revision AND ?20>=CASE status
            WHEN 'queued' THEN 0 WHEN 'running' THEN 1 WHEN 'cancelling' THEN 2 ELSE 3 END))"};
    let actual = tx.execute(
        sql,
        params![
            record.id,
            record.agent_id,
            parent_agent_id,
            child_agent_id,
            kind,
            status,
            record.summary,
            input_target,
            wait_policy,
            output_path,
            result_summary,
            exit_status,
            terminal_reentry,
            task_revision(record),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            record.parent_message_id,
            payload_json,
            status_phase,
        ],
    )?;
    if matches!(mode, TaskRowWriteMode::ExistingOriginalBinding) {
        anyhow::ensure!(
            actual == 1,
            "existing child binding UPDATE did not affect its original Task"
        );
    }
    Ok(true)
}

pub(crate) fn task_status_phase(status: &TaskStatus) -> u8 {
    match status {
        TaskStatus::Queued => 0,
        TaskStatus::Running => 1,
        TaskStatus::Cancelling => 2,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 3,
    }
}

/// The primary row's outcome and every row actually changed by this write.
/// The owner validates this finite set before publishing final transaction facts.
#[derive(Debug, Clone, Default)]
pub(crate) struct WaitConditionWrites {
    pub primary_applied: bool,
    pub wait_ids: BTreeSet<String>,
    pub turn_ids: BTreeSet<String>,
}
impl WaitConditionWrites {
    pub(crate) fn extend(&mut self, other: Self) {
        self.primary_applied |= other.primary_applied;
        self.wait_ids.extend(other.wait_ids);
        self.turn_ids.extend(other.turn_ids);
    }
}

pub(crate) fn upsert_wait_condition_tx(
    tx: &Transaction<'_>,
    record: &WaitConditionRecord,
) -> Result<bool> {
    Ok(upsert_wait_condition_with_writes_tx(tx, record)?.primary_applied)
}

pub(crate) fn upsert_wait_condition_with_writes_tx(
    tx: &Transaction<'_>,
    record: &WaitConditionRecord,
) -> Result<WaitConditionWrites> {
    if matches!(
        record.status,
        WaitConditionStatus::Active | WaitConditionStatus::Triggered
    ) {
        super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    }
    let existing = tx
        .query_row(
            "SELECT payload_json FROM wait_conditions WHERE wait_condition_id = ?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_wait_condition_payload(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        match wait_condition_transition(existing, record)? {
            StateTransitionOutcome::Applied => {}
            StateTransitionOutcome::Idempotent => return Ok(WaitConditionWrites::default()),
        }
    }
    let mut writes = WaitConditionWrites::default();
    if matches!(
        record.status,
        crate::types::WaitConditionStatus::Active | crate::types::WaitConditionStatus::Triggered
    ) {
        writes.extend(cancel_stale_unresolved_wait_owner_rows_tx(tx, record)?);
    }

    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let kind = enum_string(&record.kind)?;
    let wake_sources_json = serde_json::to_string(&record.wake_sources)?;
    let continuation_json = record
        .continuation
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let changed = tx.execute(
        "INSERT INTO wait_conditions (
            wait_condition_id, agent_id, work_item_id, status, kind, source,
            subject_ref, waiting_for, created_at, updated_at, expires_at,
            resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
            wake_sources_json, continuation_json, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(wait_condition_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            work_item_id = excluded.work_item_id,
            status = excluded.status,
            kind = excluded.kind,
            source = excluded.source,
            subject_ref = excluded.subject_ref,
            waiting_for = excluded.waiting_for,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            expires_at = excluded.expires_at,
            resolved_at = excluded.resolved_at,
            cancelled_at = excluded.cancelled_at,
            last_turn_id = excluded.last_turn_id,
            trigger_message_id = excluded.trigger_message_id,
            triggered_at = excluded.triggered_at,
            wake_sources_json = excluded.wake_sources_json,
            continuation_json = excluded.continuation_json,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= wait_conditions.updated_at",
        params![
            record.id,
            record.agent_id,
            record.work_item_id,
            status,
            kind,
            record.source,
            record.subject_ref,
            record.waiting_for,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.expires_at.map(timestamp),
            record.resolved_at.map(timestamp),
            record.cancelled_at.map(timestamp),
            record.turn_id,
            record.trigger_message_id(),
            record.triggered_at().map(timestamp),
            wake_sources_json,
            continuation_json,
            payload_json,
        ],
    )?;
    if changed == 1 {
        writes.primary_applied = true;
        writes.wait_ids.insert(record.id.clone());
        writes.turn_ids.extend(record.turn_id.iter().cloned());
        let _ = bump_source_revision_tx(
            tx,
            SOURCE_WAIT,
            &record.id,
            &record.agent_id,
            record.turn_id.as_deref(),
            record.updated_at,
        )?;
        if let Some(turn_id) = record.turn_id.as_deref() {
            let _ = bump_turn_revision_tx(
                tx,
                &record.agent_id,
                turn_id,
                true,
                true,
                record.updated_at,
            )?;
        }
    }
    Ok(writes)
}

/// Cancel stale unresolved wait rows that own the same key as an incoming
/// active/triggered registration: (agent_id, work_item_id), or (agent_id) when
/// work_item_id is NULL. Migration 44 enforces at most one unresolved row per
/// owner via partial unique indexes; a trigger settlement racing a
/// re-registration can leave an older unresolved row behind, and writing the
/// next registration would violate the index and roll back the whole
/// transition — which the runtime classifies as a fatal loop failure. Newest
/// registration wins, matching converge_unresolved_wait_owners.
fn cancel_stale_unresolved_wait_owner_rows_tx(
    tx: &Transaction<'_>,
    record: &WaitConditionRecord,
) -> Result<WaitConditionWrites> {
    let rows = tx
        .prepare(
            "SELECT wait_condition_id, payload_json FROM wait_conditions
             WHERE agent_id = ?1
               AND work_item_id IS ?2
               AND wait_condition_id != ?3
               AND status IN ('active', 'triggered')",
        )?
        .query_map(
            params![record.agent_id, record.work_item_id, record.id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut cancelled = Vec::new();
    let mut writes = WaitConditionWrites::default();
    for (wait_id, payload_json) in rows {
        let mut stale = decode_wait_condition_payload(&payload_json)?;
        if stale.updated_at > record.updated_at {
            // A newer unresolved wait already owns this key; surface the
            // stale write as a conflict instead of discarding the live wait.
            continue;
        }
        stale.status = crate::types::WaitConditionStatus::Cancelled;
        stale.updated_at = record.updated_at;
        stale.resolved_at = None;
        stale.cancelled_at = Some(record.updated_at);
        let changed = tx.execute(
            "UPDATE wait_conditions
             SET status = 'cancelled',
                 updated_at = ?1,
                 resolved_at = NULL,
                 cancelled_at = ?1,
                 payload_json = ?2
             WHERE wait_condition_id = ?3",
            params![
                timestamp(record.updated_at),
                serde_json::to_string(&stale)?,
                wait_id
            ],
        )?;
        if changed > 0 {
            writes.wait_ids.insert(stale.id.clone());
        }
        writes.turn_ids.extend(stale.turn_id.iter().cloned());
        // Mirror the revision bookkeeping applied to every other wait
        // mutation: conversation consumers invalidate cached wait state via
        // source/turn revisions, so a cancelled row must not keep its
        // pre-cancellation revisions.
        let _ = bump_source_revision_tx(
            tx,
            SOURCE_WAIT,
            &stale.id,
            &stale.agent_id,
            stale.turn_id.as_deref(),
            stale.updated_at,
        )?;
        if let Some(turn_id) = stale.turn_id.as_deref() {
            let _ =
                bump_turn_revision_tx(tx, &stale.agent_id, turn_id, true, true, stale.updated_at)?;
        }
        cancelled.push(wait_id);
    }
    if !cancelled.is_empty() {
        tracing::warn!(
            agent_id = %record.agent_id,
            work_item_id = ?record.work_item_id,
            replacement_wait_condition_id = %record.id,
            cancelled_wait_condition_ids = ?cancelled,
            "cancelled stale unresolved wait conditions superseded by newer registration"
        );
    }
    Ok(writes)
}

pub(crate) fn upsert_queue_entry_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
) -> Result<bool> {
    if matches!(
        record.status,
        QueueEntryStatus::Queued | QueueEntryStatus::Dequeued | QueueEntryStatus::Interrupted
    ) {
        super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    }
    let existing = tx
        .query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_queue_entry_payload(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        match queue_entry_transition(existing, record)? {
            StateTransitionOutcome::Applied => {}
            StateTransitionOutcome::Idempotent => return Ok(false),
        }
    }

    let payload_json = serde_json::to_string(record)?;
    let priority = enum_string(&record.priority)?;
    let status = enum_string(&record.status)?;
    let changed = tx.execute(
        "INSERT INTO queue_entries (
            message_id, agent_id, priority, status, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(message_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            priority = excluded.priority,
            status = excluded.status,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= queue_entries.updated_at",
        params![
            record.message_id,
            record.agent_id,
            priority,
            status,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    if changed == 1 {
        let assigned_turn_id = assigned_turn_id_tx(tx, &record.agent_id, &record.message_id)?;
        let _ = bump_source_revision_tx(
            tx,
            SOURCE_OPERATOR,
            &record.message_id,
            &record.agent_id,
            assigned_turn_id.as_deref(),
            record.updated_at,
        )?;
        if let Some(turn_id) = assigned_turn_id.as_deref() {
            let _ = bump_turn_revision_tx(
                tx,
                &record.agent_id,
                turn_id,
                true,
                true,
                record.updated_at,
            )?;
        }
    }
    Ok(changed == 1)
}

pub(crate) fn compare_and_set_queue_entry_tx(
    tx: &Transaction<'_>,
    expected: &QueueEntryRecord,
    record: &QueueEntryRecord,
) -> Result<bool> {
    if expected.message_id != record.message_id || expected.agent_id != record.agent_id {
        return Err(anyhow!(
            "queue compare-and-set identity must remain unchanged"
        ));
    }
    queue_entry_transition(expected, record)?;
    if matches!(
        record.status,
        QueueEntryStatus::Queued | QueueEntryStatus::Dequeued | QueueEntryStatus::Interrupted
    ) {
        super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    }

    let expected_payload_json = serde_json::to_string(expected)?;
    let payload_json = serde_json::to_string(record)?;
    let priority = enum_string(&record.priority)?;
    let status = enum_string(&record.status)?;
    let changed = tx.execute(
        "UPDATE queue_entries
         SET priority = ?3,
             status = ?4,
             created_at = ?5,
             updated_at = ?6,
             payload_json = ?7
         WHERE message_id = ?1
           AND agent_id = ?2
           AND payload_json = ?8",
        params![
            record.message_id,
            record.agent_id,
            priority,
            status,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
            expected_payload_json,
        ],
    )?;
    if changed == 1 {
        let assigned_turn_id = assigned_turn_id_tx(tx, &record.agent_id, &record.message_id)?;
        let _ = bump_source_revision_tx(
            tx,
            SOURCE_OPERATOR,
            &record.message_id,
            &record.agent_id,
            assigned_turn_id.as_deref(),
            record.updated_at,
        )?;
        if let Some(turn_id) = assigned_turn_id.as_deref() {
            let _ = bump_turn_revision_tx(
                tx,
                &record.agent_id,
                turn_id,
                true,
                true,
                record.updated_at,
            )?;
        }
    }
    Ok(changed == 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateTransitionOutcome {
    Applied,
    Idempotent,
}

pub(crate) fn task_transition(
    existing: &TaskRecord,
    incoming: &TaskRecord,
) -> Result<StateTransitionOutcome> {
    if existing == incoming {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    if is_terminal_task_status(&existing.status) {
        if is_terminal_task_status(&incoming.status) && task_terminal_payload_eq(existing, incoming)
        {
            return Ok(StateTransitionOutcome::Idempotent);
        }
        return Err(RuntimeStateTransitionConflict::new(
            "task",
            &existing.id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    if task_revision(incoming) < task_revision(existing)
        || (task_revision(incoming) == task_revision(existing)
            && task_status_phase(&incoming.status) < task_status_phase(&existing.status))
    {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    Ok(StateTransitionOutcome::Applied)
}

pub(crate) fn queue_entry_transition(
    existing: &QueueEntryRecord,
    incoming: &QueueEntryRecord,
) -> Result<StateTransitionOutcome> {
    if existing == incoming {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    if is_terminal_queue_entry_status(&existing.status) {
        if is_terminal_queue_entry_status(&incoming.status)
            && queue_entry_terminal_payload_eq(existing, incoming)
        {
            return Ok(StateTransitionOutcome::Idempotent);
        }
        return Err(RuntimeStateTransitionConflict::new(
            "queue entry",
            &existing.message_id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    if incoming.updated_at < existing.updated_at {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    Ok(StateTransitionOutcome::Applied)
}

pub(crate) fn wait_condition_transition(
    existing: &WaitConditionRecord,
    incoming: &WaitConditionRecord,
) -> Result<StateTransitionOutcome> {
    if existing == incoming {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    if is_terminal_wait_condition_status(&existing.status) {
        if is_terminal_wait_condition_status(&incoming.status)
            && wait_condition_terminal_payload_eq(existing, incoming)
        {
            return Ok(StateTransitionOutcome::Idempotent);
        }
        return Err(RuntimeStateTransitionConflict::new(
            "wait condition",
            &existing.id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    if incoming.updated_at < existing.updated_at {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    let valid = matches!(
        (&existing.status, &incoming.status),
        (WaitConditionStatus::Active, WaitConditionStatus::Triggered)
            | (WaitConditionStatus::Active, WaitConditionStatus::Resolved)
            | (WaitConditionStatus::Active, WaitConditionStatus::Cancelled)
            | (WaitConditionStatus::Active, WaitConditionStatus::Expired)
            | (
                WaitConditionStatus::Triggered,
                WaitConditionStatus::Resolved
            )
            | (
                WaitConditionStatus::Triggered,
                WaitConditionStatus::Cancelled
            )
            | (WaitConditionStatus::Triggered, WaitConditionStatus::Expired)
    );
    if !valid {
        return Err(RuntimeStateTransitionConflict::new(
            "wait condition",
            &existing.id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    Ok(StateTransitionOutcome::Applied)
}

fn is_terminal_queue_entry_status(status: &QueueEntryStatus) -> bool {
    matches!(
        status,
        QueueEntryStatus::Processed
            | QueueEntryStatus::Aborted
            | QueueEntryStatus::Dropped
            | QueueEntryStatus::Quarantined
    )
}

fn is_terminal_wait_condition_status(status: &WaitConditionStatus) -> bool {
    matches!(
        status,
        WaitConditionStatus::Resolved
            | WaitConditionStatus::Cancelled
            | WaitConditionStatus::Expired
    )
}

fn queue_entry_terminal_payload_eq(
    existing: &QueueEntryRecord,
    incoming: &QueueEntryRecord,
) -> bool {
    let mut existing = existing.clone();
    existing.updated_at = incoming.updated_at;
    existing == *incoming
}

fn wait_condition_terminal_payload_eq(
    existing: &WaitConditionRecord,
    incoming: &WaitConditionRecord,
) -> bool {
    let mut existing = existing.clone();
    existing.updated_at = incoming.updated_at;
    existing == *incoming
}

fn task_terminal_payload_eq(existing: &TaskRecord, incoming: &TaskRecord) -> bool {
    let existing = canonical_task_terminal_payload(existing);
    let incoming = canonical_task_terminal_payload(incoming);
    existing.id == incoming.id
        && existing.agent_id == incoming.agent_id
        && existing.kind == incoming.kind
        && existing.status == incoming.status
        && existing.parent_message_id == incoming.parent_message_id
        && existing.work_item_id == incoming.work_item_id
        && existing.summary == incoming.summary
        && existing.detail == incoming.detail
        && existing.recovery == incoming.recovery
}

fn canonical_task_terminal_payload(record: &TaskRecord) -> TaskRecord {
    let mut canonical = record.clone();
    canonical.detail = canonical
        .detail
        .as_ref()
        .map(canonical_task_terminal_detail);
    canonical
}

fn canonical_task_terminal_detail(value: &serde_json::Value) -> serde_json::Value {
    // Terminal identity ignores derived observation fields even when storage retains them.
    match value {
        serde_json::Value::Object(map) => {
            let mut canonical = serde_json::Map::new();
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "initial_output"
                        | "output_summary"
                        | "output_preview"
                        | "terminal_snapshot_ready"
                ) {
                    continue;
                }
                canonical.insert(key.clone(), canonical_task_terminal_detail(value));
            }
            serde_json::Value::Object(canonical)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_task_terminal_detail).collect())
        }
        _ => value.clone(),
    }
}

pub(crate) fn try_claim_queued_message_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
) -> Result<bool> {
    try_transition_claimable_message_tx(tx, record, QueueEntryStatus::Dequeued, true)
}

pub(crate) fn queue_entry_is_claimable_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
    include_interrupted: bool,
) -> Result<bool> {
    let status = tx
        .query_row(
            "SELECT status
             FROM queue_entries
             WHERE message_id = ?1 AND agent_id = ?2",
            params![record.message_id, record.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(status.is_some_and(|status| {
        status == enum_string(&QueueEntryStatus::Queued).expect("queue status serializes")
            || (include_interrupted
                && status
                    == enum_string(&QueueEntryStatus::Interrupted)
                        .expect("queue status serializes"))
    }))
}

pub(crate) fn try_interject_queued_message_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
) -> Result<bool> {
    try_transition_claimable_message_tx(tx, record, QueueEntryStatus::Interjected, false)
}

fn try_transition_claimable_message_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
    target_status: QueueEntryStatus,
    include_interrupted: bool,
) -> Result<bool> {
    super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    let queued_status = enum_string(&QueueEntryStatus::Queued)?;
    let secondary_status = enum_string(if include_interrupted {
        &QueueEntryStatus::Interrupted
    } else {
        &QueueEntryStatus::Queued
    })?;
    let mut claimed = record.clone();
    claimed.status = target_status;
    let payload_json = serde_json::to_string(&claimed)?;
    let priority = enum_string(&claimed.priority)?;
    let status = enum_string(&claimed.status)?;
    let changed = tx.execute(
        "UPDATE queue_entries
         SET priority = ?3,
             status = ?4,
             created_at = ?5,
             updated_at = ?6,
             payload_json = ?7
         WHERE message_id = ?1
           AND agent_id = ?2
           AND status IN (?8, ?9)",
        params![
            claimed.message_id,
            claimed.agent_id,
            priority,
            status,
            timestamp(claimed.created_at),
            timestamp(claimed.updated_at),
            payload_json,
            queued_status,
            secondary_status,
        ],
    )?;
    if changed == 1 {
        let assigned_turn_id = assigned_turn_id_tx(tx, &claimed.agent_id, &claimed.message_id)?;
        let _ = bump_source_revision_tx(
            tx,
            SOURCE_OPERATOR,
            &claimed.message_id,
            &claimed.agent_id,
            assigned_turn_id.as_deref(),
            claimed.updated_at,
        )?;
        if let Some(turn_id) = assigned_turn_id.as_deref() {
            let _ = bump_turn_revision_tx(
                tx,
                &claimed.agent_id,
                turn_id,
                true,
                true,
                claimed.updated_at,
            )?;
        }
    }
    Ok(changed == 1)
}

pub(in crate::runtime_db) fn timer_tx(
    tx: &Transaction<'_>,
    timer_id: &str,
) -> Result<Option<TimerRecord>> {
    tx.query_row(
        "SELECT payload_json FROM timers WHERE timer_id = ?1",
        [timer_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_timer_payload(&payload))
    .transpose()
}

pub(crate) fn upsert_timer_tx(tx: &Transaction<'_>, record: &TimerRecord) -> Result<()> {
    upsert_timer_with_write_tx(tx, record).map(|_| ())
}

pub(crate) fn upsert_timer_with_write_tx(
    tx: &Transaction<'_>,
    record: &TimerRecord,
) -> Result<bool> {
    if record.status == TimerStatus::Active {
        super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    }
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let updated_at = timer_updated_at(record);
    let changed = tx.execute(
        "INSERT INTO timers (
            timer_id, agent_id, status, summary, created_at, duration_ms,
            interval_ms, repeat, next_fire_at, last_fired_at, fire_count,
            updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(timer_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            status = excluded.status,
            summary = excluded.summary,
            created_at = excluded.created_at,
            duration_ms = excluded.duration_ms,
            interval_ms = excluded.interval_ms,
            repeat = excluded.repeat,
            next_fire_at = excluded.next_fire_at,
            last_fired_at = excluded.last_fired_at,
            fire_count = excluded.fire_count,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE NOT (timers.status = 'cancelled' AND excluded.status = 'active')
           AND (
             excluded.fire_count > timers.fire_count
             OR (
                excluded.fire_count = timers.fire_count
                AND (
                    CASE excluded.status
                        WHEN 'active' THEN 0
                        WHEN 'cancelled' THEN 1
                        WHEN 'completed' THEN 2
                        ELSE 0
                    END
                    > CASE timers.status
                        WHEN 'active' THEN 0
                        WHEN 'cancelled' THEN 1
                        WHEN 'completed' THEN 2
                        ELSE 0
                    END
                    OR (
                        excluded.status = timers.status
                        AND excluded.updated_at >= timers.updated_at
                    )
                )
             )
           )",
        params![
            record.id,
            record.agent_id,
            status,
            record.summary,
            timestamp(record.created_at),
            record.duration_ms as i64,
            record.interval_ms.map(|value| value as i64),
            i64::from(record.repeat),
            record.next_fire_at.map(timestamp),
            record.last_fired_at.map(timestamp),
            record.fire_count as i64,
            timestamp(updated_at),
            payload_json,
        ],
    )?;
    Ok(changed == 1)
}

fn merge_legacy_turn_evidence_tx(tx: &Transaction<'_>, derived: &TurnRecord) -> Result<TurnRecord> {
    let Some(mut existing) = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
            [&derived.turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_turn_record_payload(&payload))
        .transpose()?
    else {
        return Ok(derived.clone());
    };
    if existing.agent_id != derived.agent_id {
        return Err(RuntimeStateTransitionConflict::new(
            "turn identity",
            &derived.turn_id,
            &existing.agent_id,
            &derived.agent_id,
        )
        .into());
    }
    existing
        .input_message_ids
        .extend(derived.input_message_ids.iter().cloned());
    existing
        .tool_execution_ids
        .extend(derived.tool_execution_ids.iter().cloned());
    existing
        .produced_brief_ids
        .extend(derived.produced_brief_ids.iter().cloned());
    existing
        .delivery_summary_ids
        .extend(derived.delivery_summary_ids.iter().cloned());
    existing
        .completed_work_item_ids
        .extend(derived.completed_work_item_ids.iter().cloned());
    existing
        .waiting_condition_ids
        .extend(derived.waiting_condition_ids.iter().cloned());
    existing.input_message_ids.sort();
    existing.input_message_ids.dedup();
    existing.tool_execution_ids.sort();
    existing.tool_execution_ids.dedup();
    existing.produced_brief_ids.sort();
    existing.produced_brief_ids.dedup();
    existing.delivery_summary_ids.sort();
    existing.delivery_summary_ids.dedup();
    existing.completed_work_item_ids.sort();
    existing.completed_work_item_ids.dedup();
    existing.waiting_condition_ids.sort();
    existing.waiting_condition_ids.dedup();
    Ok(existing)
}

pub(crate) fn committed_turn_record_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
) -> Result<CommittedTurnRecord> {
    let payload: String = tx.query_row(
        "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2",
        params![agent_id, turn_id],
        |row| row.get(0),
    )?;
    let record = decode_turn_record_payload(&payload)?;
    anyhow::ensure!(
        record.agent_id == agent_id && record.turn_id == turn_id,
        "committed Turn scope mismatch for {turn_id}"
    );
    let mut input_assignments = Vec::new();
    let mut affected_source_revisions = Vec::new();
    let mut affected_turn_ids = BTreeSet::from([turn_id.to_string()]);
    for message_id in record.input_message_ids.iter().collect::<BTreeSet<_>>() {
        let (assigned_turn_id, revision, assigned_at): (String, i64, String) = tx.query_row(
            "SELECT turn_id, revision, assigned_at FROM conversation_input_assignments WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, message_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).with_context(|| format!("reading committed input assignment for {message_id}"))?;
        affected_turn_ids.insert(assigned_turn_id.clone());
        let (source_agent_id, source_turn_id, source_revision, activity_seq, updated_at): (
            String,
            Option<String>,
            i64,
            Option<i64>,
            String,
        ) = tx
            .query_row(
                "SELECT agent_id, turn_id, revision, activity_seq, updated_at
                 FROM conversation_source_revisions WHERE source_kind = ?1 AND source_id = ?2",
                params![SOURCE_OPERATOR, message_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .with_context(|| {
                format!("reading committed input source projection for {message_id}")
            })?;
        anyhow::ensure!(
            source_agent_id == agent_id
                && source_turn_id.as_deref() == Some(assigned_turn_id.as_str()),
            "committed input source projection scope mismatch for {message_id}"
        );
        affected_source_revisions.push(CommittedConversationSourceRevision {
            agent_id: source_agent_id,
            source_kind: SOURCE_OPERATOR.into(),
            source_id: message_id.clone(),
            turn_id: source_turn_id,
            revision: u64::try_from(source_revision)?,
            activity_seq: activity_seq.map(u64::try_from).transpose()?,
            updated_at: parse_timestamp(&updated_at)?,
        });
        input_assignments.push(CommittedTurnInputAssignment {
            agent_id: agent_id.into(),
            message_id: message_id.clone(),
            turn_id: assigned_turn_id,
            revision: u64::try_from(revision)?,
            assigned_at: parse_timestamp(&assigned_at)?,
        });
    }
    let mut affected_turn_revisions = Vec::new();
    for affected_turn_id in affected_turn_ids {
        let (summary, detail, result_settled, updated_at): (i64, i64, bool, String) = tx
            .query_row(
                "SELECT summary_revision, detail_revision, result_settled, updated_at
             FROM conversation_turn_revisions WHERE agent_id = ?1 AND turn_id = ?2",
                params![agent_id, affected_turn_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .with_context(|| format!("reading committed Turn projection for {affected_turn_id}"))?;
        affected_turn_revisions.push(CommittedConversationTurnRevision {
            agent_id: agent_id.into(),
            turn_id: affected_turn_id,
            revision: super::conversation::ConversationTurnRevision {
                summary: u64::try_from(summary)?,
                detail: u64::try_from(detail)?,
                result_settled,
            },
            updated_at: parse_timestamp(&updated_at)?,
        });
    }
    let revision = affected_turn_revisions
        .iter()
        .find(|projection| projection.turn_id == turn_id)
        .ok_or_else(|| anyhow!("requested Turn projection missing from committed facts"))?
        .revision;
    Ok(CommittedTurnRecord {
        record,
        input_assignments,
        revision,
        affected_source_revisions,
        affected_turn_revisions,
    })
}

pub(crate) fn upsert_turn_record_tx(tx: &Transaction<'_>, record: &TurnRecord) -> Result<()> {
    if let Some(owner) = record.owner.as_ref() {
        let owner_work_item_id = owner.work_item_id();
        if record.current_work_item_id.as_deref() != owner_work_item_id {
            return Err(RuntimeStateTransitionConflict::new(
                "turn owner",
                &record.turn_id,
                owner_work_item_id.unwrap_or("unbound"),
                record.current_work_item_id.as_deref().unwrap_or("unbound"),
            )
            .into());
        }
    }
    let existing_payload = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
            [&record.turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let existing = existing_payload
        .as_deref()
        .map(decode_turn_record_payload)
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        let replay_identity_matches = match (&existing.replay, &record.replay) {
            (Some(existing), Some(record)) => {
                existing.source_message_id == record.source_message_id
                    && existing.source_turn_id == record.source_turn_id
                    && existing.reason == record.reason
            }
            (None, None) => true,
            _ => false,
        };
        let identity_matches = existing.agent_id == record.agent_id
            && existing.turn_index == record.turn_index
            && existing.run_id == record.run_id
            && existing.current_work_item_id == record.current_work_item_id
            && (existing.owner == record.owner || existing.owner.is_none())
            && existing
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                == record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
            && existing.created_at == record.created_at
            && replay_identity_matches;
        if !identity_matches {
            return Err(RuntimeStateTransitionConflict::new(
                "turn identity",
                &record.turn_id,
                "immutable",
                "conflicting",
            )
            .into());
        }
    }
    let payload_json = serde_json::to_string(record)?;
    let terminal_kind = record
        .terminal
        .as_ref()
        .map(|terminal| enum_string(&terminal.kind))
        .transpose()?;
    let completed_at = record
        .terminal
        .as_ref()
        .map(|terminal| timestamp(terminal.completed_at));
    let (owner_kind, owner_id) = match record.owner.as_ref() {
        Some(owner) => {
            let (kind, id) = owner.index_parts();
            (Some(kind), id)
        }
        None => (None, None),
    };
    let has_owner_columns = tx.query_row(
        "SELECT EXISTS(
            SELECT 1
              FROM pragma_table_info('turn_records')
             WHERE name = 'owner_kind'
        )",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    let has_replay_source_column = tx.query_row(
        "SELECT EXISTS(
            SELECT 1
              FROM pragma_table_info('turn_records')
             WHERE name = 'replay_source_turn_id'
        )",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    if has_owner_columns && has_replay_source_column {
        tx.execute(
            "INSERT INTO turn_records (
                turn_id, turn_index, agent_id, run_id, current_work_item_id,
                owner_kind, owner_id, trigger_message_id, replay_source_turn_id,
                terminal_kind, created_at, completed_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(turn_id) DO UPDATE SET
                owner_kind = COALESCE(turn_records.owner_kind, excluded.owner_kind),
                owner_id = COALESCE(turn_records.owner_id, excluded.owner_id),
                replay_source_turn_id = excluded.replay_source_turn_id,
                terminal_kind = excluded.terminal_kind,
                completed_at = excluded.completed_at,
                payload_json = excluded.payload_json
             WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
            params![
                record.turn_id,
                record.turn_index as i64,
                record.agent_id,
                record.run_id,
                record.current_work_item_id,
                owner_kind,
                owner_id,
                record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref()),
                record
                    .replay
                    .as_ref()
                    .map(|replay| replay.source_turn_id.as_str()),
                terminal_kind,
                timestamp(record.created_at),
                completed_at,
                payload_json,
            ],
        )?;
    } else if has_owner_columns {
        tx.execute(
            "INSERT INTO turn_records (
                turn_id, turn_index, agent_id, run_id, current_work_item_id,
                owner_kind, owner_id, trigger_message_id, terminal_kind, created_at,
                completed_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(turn_id) DO UPDATE SET
                owner_kind = COALESCE(turn_records.owner_kind, excluded.owner_kind),
                owner_id = COALESCE(turn_records.owner_id, excluded.owner_id),
                terminal_kind = excluded.terminal_kind,
                completed_at = excluded.completed_at,
                payload_json = excluded.payload_json
             WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
            params![
                record.turn_id,
                record.turn_index as i64,
                record.agent_id,
                record.run_id,
                record.current_work_item_id,
                owner_kind,
                owner_id,
                record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref()),
                terminal_kind,
                timestamp(record.created_at),
                completed_at,
                payload_json,
            ],
        )?;
    } else {
        tx.execute(
            "INSERT INTO turn_records (
                turn_id, turn_index, agent_id, run_id, current_work_item_id,
                trigger_message_id, terminal_kind, created_at, completed_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(turn_id) DO UPDATE SET
                terminal_kind = excluded.terminal_kind,
                completed_at = excluded.completed_at,
                payload_json = excluded.payload_json
             WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
            params![
                record.turn_id,
                record.turn_index as i64,
                record.agent_id,
                record.run_id,
                record.current_work_item_id,
                record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref()),
                terminal_kind,
                timestamp(record.created_at),
                completed_at,
                payload_json,
            ],
        )?;
    }
    let stored_payload = tx.query_row(
        "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
        [&record.turn_id],
        |row| row.get::<_, String>(0),
    )?;
    let stored = decode_turn_record_payload(&stored_payload)?;
    let changed = existing_payload.as_deref() != Some(stored_payload.as_str());
    finish_original_turn_revisions_tx(tx, &stored, changed)?;
    for message_id in &stored.input_message_ids {
        assign_original_turn_input_tx(tx, &stored, message_id)?;
    }
    Ok(())
}

/// Shared bookkeeping only; callers must already have persisted the complete
/// validated Turn row in this same transaction.
pub(in crate::runtime_db) fn finish_original_turn_revisions_tx(
    tx: &Transaction<'_>,
    stored: &TurnRecord,
    changed: bool,
) -> Result<()> {
    if changed {
        let _ = bump_turn_revision_tx(
            tx,
            &stored.agent_id,
            &stored.turn_id,
            true,
            false,
            stored
                .terminal
                .as_ref()
                .map_or(stored.created_at, |terminal| terminal.completed_at),
        )?;
    } else {
        let _ = ensure_turn_revision_tx(tx, &stored.agent_id, &stored.turn_id, stored.created_at)?;
    }
    if stored
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.no_brief_reason.as_ref())
        .is_some()
    {
        let updated_at = stored
            .terminal
            .as_ref()
            .map_or(stored.created_at, |terminal| terminal.completed_at);
        let _ = settle_turn_result_tx(tx, &stored.agent_id, &stored.turn_id, updated_at)?;
    }
    Ok(())
}

pub(in crate::runtime_db) fn assign_original_turn_input_tx(
    tx: &Transaction<'_>,
    stored: &TurnRecord,
    message_id: &str,
) -> Result<()> {
    let existing_assignment = assigned_turn_id_tx(tx, &stored.agent_id, message_id)?;
    let assignment_turn_id = if let Some(existing_turn_id) = existing_assignment.as_deref() {
        existing_turn_id
    } else if let Some(replay) = stored
        .replay
        .as_ref()
        .filter(|replay| replay.source_message_id == message_id)
    {
        let source_owns_input = tx.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM turn_records AS source
                WHERE source.agent_id = ?1
                  AND source.turn_id = ?2
                  AND EXISTS (
                    SELECT 1
                    FROM json_each(source.payload_json, '$.input_message_ids') AS input
                    WHERE input.type = 'text' AND input.value = ?3
                  )
             )",
            params![stored.agent_id, replay.source_turn_id, message_id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if source_owns_input {
            replay.source_turn_id.as_str()
        } else {
            stored.turn_id.as_str()
        }
    } else {
        stored.turn_id.as_str()
    };
    let assignment_target_exists = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM turn_records
            WHERE agent_id = ?1 AND turn_id = ?2
         )",
        params![stored.agent_id, assignment_turn_id],
        |row| row.get::<_, i64>(0),
    )? != 0;
    anyhow::ensure!(
        assignment_target_exists,
        "conversation input assignment target is not persisted: agent_id={}, message_id={}, turn_id={}",
        stored.agent_id,
        message_id,
        assignment_turn_id
    );
    let _ = assign_input_to_turn_tx(
        tx,
        &stored.agent_id,
        message_id,
        assignment_turn_id,
        stored.created_at,
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn derive_turn_records_from_legacy_evidence(
    messages: Vec<serde_json::Value>,
    tool_executions: Vec<ToolExecutionRecord>,
    briefs: Vec<BriefRecord>,
    delivery_summaries: Vec<DeliverySummaryRecord>,
    wait_conditions: Vec<WaitConditionRecord>,
) -> Result<Vec<TurnRecord>> {
    derive_turn_records_from_legacy_evidence_observed(
        messages,
        tool_executions,
        briefs,
        delivery_summaries,
        wait_conditions,
        None,
    )
}

fn observe_turn_derivation(
    observe: &mut Option<&mut dyn FnMut(u64, usize) -> Result<()>>,
    value: &impl serde::Serialize,
    new_turn: bool,
) -> Result<()> {
    if let Some(observe) = observe.as_deref_mut() {
        let (bytes, nodes) =
            super::input_evidence_snapshot::legacy_delivery::original_episode::received_turns::derivation_material(value, new_turn)?;
        observe(bytes, nodes)?;
    }
    Ok(())
}

pub(super) fn derive_turn_records_from_legacy_evidence_observed(
    messages: Vec<serde_json::Value>,
    tool_executions: Vec<ToolExecutionRecord>,
    briefs: Vec<BriefRecord>,
    delivery_summaries: Vec<DeliverySummaryRecord>,
    wait_conditions: Vec<WaitConditionRecord>,
    mut observe: Option<&mut dyn FnMut(u64, usize) -> Result<()>>,
) -> Result<Vec<TurnRecord>> {
    let mut records = BTreeMap::<String, TurnRecord>::new();
    for raw_message in messages {
        if let Some(message) = normalize_legacy_message_value(raw_message)? {
            let turn_key = turn_key_from_message(&message);
            let existing = records.get(&turn_key.turn_id);
            observe_turn_derivation(
                &mut observe,
                &(
                    existing.is_none().then_some((
                        &message.agent_id,
                        &turn_key.turn_id,
                        turn_key.turn_index,
                    )),
                    &message.id,
                    existing
                        .is_none_or(|record| record.current_work_item_id.is_none())
                        .then_some(&message.work_item_id),
                    existing
                        .is_none_or(|record| record.trigger.is_none())
                        .then_some((
                            &message.id,
                            &message.kind,
                            &message.origin,
                            &message.authority_class,
                            &message.priority,
                            &message.trigger_kind,
                            &message.task_id,
                        )),
                ),
                existing.is_none(),
            )?;
            let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
                TurnRecord::new(&message.agent_id, &turn_key.turn_id, turn_key.turn_index)
            });
            reinforce_turn_index(record, &turn_key);
            record.created_at = record.created_at.min(message.created_at);
            record.input_message_ids.push(message.id.clone());
            if record.trigger.is_none() {
                record.trigger = Some(crate::types::TurnTriggerSummary::from_message(&message));
            }
            if record.current_work_item_id.is_none() {
                record.current_work_item_id = message.work_item_id.clone();
            }
        }
    }
    for tool in tool_executions {
        let Some(turn_key) = turn_key_from_optional(tool.turn_id.as_deref(), tool.turn_index)
        else {
            continue;
        };
        let existing = records.get(&turn_key.turn_id);
        observe_turn_derivation(
            &mut observe,
            &(
                existing.is_none().then_some((
                    &tool.agent_id,
                    &turn_key.turn_id,
                    turn_key.turn_index,
                )),
                &tool.id,
                existing
                    .is_none_or(|record| record.current_work_item_id.is_none())
                    .then_some(&tool.work_item_id),
            ),
            existing.is_none(),
        )?;
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&tool.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(tool.created_at);
        record.tool_execution_ids.push(tool.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = tool.work_item_id.clone();
        }
    }
    for brief in briefs {
        let Some(turn_key) = turn_key_from_optional(
            brief.turn_id.as_deref(),
            brief.turn_index.unwrap_or_default(),
        ) else {
            continue;
        };
        let existing = records.get(&turn_key.turn_id);
        observe_turn_derivation(
            &mut observe,
            &(
                existing.is_none().then_some((
                    &brief.agent_id,
                    &turn_key.turn_id,
                    turn_key.turn_index,
                )),
                &brief.id,
                existing
                    .is_none_or(|record| record.current_work_item_id.is_none())
                    .then_some(&brief.work_item_id),
            ),
            existing.is_none(),
        )?;
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&brief.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(brief.created_at);
        record.produced_brief_ids.push(brief.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = brief.work_item_id.clone();
        }
    }
    for summary in delivery_summaries {
        let Some(turn_key) = turn_key_from_optional(
            summary.turn_id.as_deref(),
            summary.source_turn_index.unwrap_or_default(),
        ) else {
            continue;
        };
        let existing = records.get(&turn_key.turn_id);
        observe_turn_derivation(
            &mut observe,
            &(
                existing.is_none().then_some((
                    &summary.agent_id,
                    &turn_key.turn_id,
                    turn_key.turn_index,
                )),
                &summary.id,
                &summary.work_item_id,
                existing
                    .is_none_or(|record| record.current_work_item_id.is_none())
                    .then_some(&summary.work_item_id),
            ),
            existing.is_none(),
        )?;
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&summary.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(summary.created_at);
        record.delivery_summary_ids.push(summary.id.clone());
        record
            .completed_work_item_ids
            .push(summary.work_item_id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = Some(summary.work_item_id.clone());
        }
    }
    for condition in wait_conditions {
        let Some(turn_id) = condition
            .turn_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let existing = records.get(turn_id.trim());
        observe_turn_derivation(
            &mut observe,
            &(
                existing
                    .is_none()
                    .then_some((&condition.agent_id, turn_id.trim(), 0u64)),
                &condition.id,
                existing
                    .is_none_or(|record| record.current_work_item_id.is_none())
                    .then_some(&condition.work_item_id),
            ),
            existing.is_none(),
        )?;
        let record = records
            .entry(turn_id.trim().to_string())
            .or_insert_with(|| TurnRecord::new(&condition.agent_id, turn_id.trim(), 0));
        record.created_at = record.created_at.min(condition.created_at);
        record.waiting_condition_ids.push(condition.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = condition.work_item_id.clone();
        }
    }
    for record in records.values_mut() {
        record.input_message_ids.sort();
        record.input_message_ids.dedup();
        record.tool_execution_ids.sort();
        record.tool_execution_ids.dedup();
        record.produced_brief_ids.sort();
        record.produced_brief_ids.dedup();
        record.delivery_summary_ids.sort();
        record.delivery_summary_ids.dedup();
        record.completed_work_item_ids.sort();
        record.completed_work_item_ids.dedup();
        record.waiting_condition_ids.sort();
        record.waiting_condition_ids.dedup();
    }
    Ok(records.into_values().collect())
}

pub(crate) struct DerivedTurnKey {
    turn_id: String,
    turn_index: u64,
}

impl DerivedTurnKey {
    pub(in crate::runtime_db) fn parts(&self) -> (&str, u64) {
        (&self.turn_id, self.turn_index)
    }
}

pub(crate) fn reinforce_turn_index(record: &mut TurnRecord, turn_key: &DerivedTurnKey) {
    if record.turn_index == 0 && turn_key.turn_index != 0 {
        record.turn_index = turn_key.turn_index;
    }
}

pub(crate) fn turn_key_from_message(message: &MessageEnvelope) -> DerivedTurnKey {
    if let Some(turn_id) = message
        .turn_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return DerivedTurnKey {
            turn_id: turn_id.trim().to_string(),
            turn_index: 0,
        };
    }
    let turn_index = message.message_seq.unwrap_or_default();
    DerivedTurnKey {
        turn_id: format!("legacy-turn-{turn_index}"),
        turn_index,
    }
}

pub(crate) fn turn_key_from_optional(
    turn_id: Option<&str>,
    turn_index: u64,
) -> Option<DerivedTurnKey> {
    if let Some(turn_id) = turn_id.filter(|value| !value.trim().is_empty()) {
        return Some(DerivedTurnKey {
            turn_id: turn_id.trim().to_string(),
            turn_index,
        });
    }
    (turn_index != 0).then(|| DerivedTurnKey {
        turn_id: format!("legacy-turn-{turn_index}"),
        turn_index,
    })
}

pub(crate) fn newer_work_item_record(
    candidate: &WorkItemRecord,
    existing: &WorkItemRecord,
) -> bool {
    candidate
        .revision
        .cmp(&existing.revision)
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

pub(crate) fn reduce_timer_records(records: Vec<TimerRecord>) -> BTreeMap<String, TimerRecord> {
    let mut latest = BTreeMap::<String, TimerRecord>::new();
    for record in records {
        if latest
            .get(&record.id)
            .is_none_or(|existing| newer_timer_record(&record, existing))
        {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

pub(crate) fn newer_timer_record(candidate: &TimerRecord, existing: &TimerRecord) -> bool {
    candidate
        .fire_count
        .cmp(&existing.fire_count)
        .then_with(|| {
            timer_status_rank(&candidate.status).cmp(&timer_status_rank(&existing.status))
        })
        .then_with(|| timer_updated_at(candidate).cmp(&timer_updated_at(existing)))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| candidate.id.cmp(&existing.id))
        .is_gt()
}

pub(crate) fn timer_status_rank(status: &TimerStatus) -> u8 {
    match status {
        TimerStatus::Active => 0,
        TimerStatus::Cancelled => 1,
        TimerStatus::Completed => 2,
    }
}

pub(crate) fn timer_updated_at(record: &TimerRecord) -> DateTime<Utc> {
    record
        .last_fired_at
        .or(record.next_fire_at)
        .unwrap_or(record.created_at)
}

pub(crate) fn reduce_external_trigger_records(
    records: Vec<ExternalTriggerRecord>,
) -> BTreeMap<String, ExternalTriggerRecord> {
    let mut latest_by_id = BTreeMap::<String, ExternalTriggerRecord>::new();
    for record in records {
        if latest_by_id
            .get(&record.external_trigger_id)
            .is_none_or(|existing| newer_external_trigger_record(&record, existing))
        {
            latest_by_id.insert(
                record.external_trigger_id.clone(),
                normalize_external_trigger_record(record),
            );
        }
    }

    let mut active_by_agent = BTreeMap::<String, String>::new();
    for record in latest_by_id.values() {
        if record.status != ExternalTriggerStatus::Active {
            continue;
        }
        let replace = active_by_agent
            .get(&record.target_agent_id)
            .and_then(|id| latest_by_id.get(id))
            .is_none_or(|existing| newer_external_trigger_record(record, existing));
        if replace {
            active_by_agent.insert(
                record.target_agent_id.clone(),
                record.external_trigger_id.clone(),
            );
        }
    }

    for record in latest_by_id.values_mut() {
        if record.status == ExternalTriggerStatus::Active
            && active_by_agent.get(&record.target_agent_id) != Some(&record.external_trigger_id)
        {
            record.status = ExternalTriggerStatus::Revoked;
            record.revoked_at.get_or_insert(record.created_at);
        }
    }
    latest_by_id
}

pub(crate) fn normalize_external_trigger_record(
    mut record: ExternalTriggerRecord,
) -> ExternalTriggerRecord {
    record.scope = ExternalTriggerScope::Agent;
    record.delivery_mode = CallbackDeliveryMode::WakeHint;
    record
}

pub(crate) fn newer_external_trigger_record(
    candidate: &ExternalTriggerRecord,
    existing: &ExternalTriggerRecord,
) -> bool {
    candidate
        .delivery_count
        .cmp(&existing.delivery_count)
        .then_with(|| candidate.last_delivered_at.cmp(&existing.last_delivered_at))
        .then_with(|| candidate.revoked_at.cmp(&existing.revoked_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| {
            candidate
                .external_trigger_id
                .cmp(&existing.external_trigger_id)
        })
        .is_gt()
}

pub(crate) fn reduce_task_records(records: Vec<TaskRecord>) -> BTreeMap<String, TaskRecord> {
    let mut latest = BTreeMap::<String, TaskRecord>::new();
    for record in records {
        if let Some(previous) = latest.get(&record.id) {
            let mut merged = record.clone();
            if merged.summary.is_none() {
                merged.summary = previous.summary.clone();
            }
            if merged.detail.is_none() {
                merged.detail = previous.detail.clone();
            }
            if merged.recovery.is_none() {
                merged.recovery = previous.recovery.clone();
            }
            if newer_task_record(&merged, previous) {
                latest.insert(record.id.clone(), merged);
            }
        } else {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_work_item_delegation_records(
    records: Vec<WorkItemDelegationRecord>,
) -> BTreeMap<String, WorkItemDelegationRecord> {
    let mut latest = BTreeMap::<String, WorkItemDelegationRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.delegation_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.delegation_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_context_episode_records(
    records: Vec<ContextEpisodeRecord>,
) -> BTreeMap<String, ContextEpisodeRecord> {
    let mut latest = BTreeMap::<String, ContextEpisodeRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.id)
            .is_none_or(|existing| record.finalized_at >= existing.finalized_at);
        if should_replace {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_workspace_entry_records(
    records: Vec<WorkspaceEntry>,
) -> BTreeMap<String, WorkspaceEntry> {
    let mut latest = BTreeMap::<String, WorkspaceEntry>::new();
    for record in records {
        let should_replace = latest
            .get(&record.workspace_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.workspace_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_workspace_occupancy_records(
    records: Vec<WorkspaceOccupancyRecord>,
) -> BTreeMap<String, WorkspaceOccupancyRecord> {
    let mut latest = BTreeMap::<String, WorkspaceOccupancyRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.occupancy_id)
            .is_none_or(|existing| record.released_at >= existing.released_at);
        if should_replace {
            latest.insert(record.occupancy_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_agent_identity_records(
    records: Vec<AgentIdentityRecord>,
) -> BTreeMap<String, AgentIdentityRecord> {
    let mut latest = BTreeMap::<String, AgentIdentityRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.agent_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.agent_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn slim_task_record_for_payload(record: &TaskRecord) -> TaskRecord {
    let mut slim = record.clone();
    slim.detail = slim.detail.as_ref().map(slim_task_detail_value);
    slim
}

pub(crate) fn slim_task_detail_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut slim = serde_json::Map::new();
            for (key, value) in map {
                if key == "initial_output" {
                    continue;
                }
                slim.insert(key.clone(), slim_task_detail_value(value));
            }
            serde_json::Value::Object(slim)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(TASK_PAYLOAD_ARRAY_LIMIT)
                .map(slim_task_detail_value)
                .collect(),
        ),
        serde_json::Value::String(value) => {
            serde_json::Value::String(truncate_task_payload_string(value))
        }
        _ => value.clone(),
    }
}

pub(crate) fn truncate_task_payload_string(value: &str) -> String {
    value.chars().take(TASK_PAYLOAD_STRING_LIMIT).collect()
}

pub(crate) fn newer_task_record(candidate: &TaskRecord, existing: &TaskRecord) -> bool {
    task_revision(candidate)
        .cmp(&task_revision(existing))
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

pub(crate) fn task_revision(record: &TaskRecord) -> i64 {
    record.updated_at.timestamp_millis()
}

pub(crate) fn decode_agent_state_payload(payload: &str) -> Result<AgentState> {
    serde_json::from_str(payload).context("decoding agent state payload from runtime db")
}

pub(crate) fn decode_workspace_entry_payload(payload: &str) -> Result<WorkspaceEntry> {
    serde_json::from_str(payload).context("decoding workspace entry payload from runtime db")
}

pub(crate) fn decode_workspace_occupancy_payload(
    payload: &str,
) -> Result<WorkspaceOccupancyRecord> {
    serde_json::from_str(payload).context("decoding workspace occupancy payload from runtime db")
}

pub(crate) fn decode_execution_root_entry_payload(payload: &str) -> Result<ExecutionRootEntry> {
    serde_json::from_str(payload).context("decoding execution root entry payload from runtime db")
}

pub(crate) fn decode_agent_identity_payload(payload: &str) -> Result<AgentIdentityRecord> {
    serde_json::from_str(payload).context("decoding agent identity payload from runtime db")
}

/// Drives an identity to the canonical deleted tombstone during deletion
/// finalization; returns `true` when a registry write was performed. The
/// stored payload text is canonicalized even when the decoded status is
/// already `Deleted`: legacy rows can carry the pre-rename `archived` text,
/// which the observer-sync reservation invariants compare literally, so a
/// tombstone that is never rewritten keeps those invariants violated
/// forever.
fn ensure_deleted_tombstone_tx(
    tx: &Transaction<'_>,
    identity: &mut AgentIdentityRecord,
    stored_status: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    if identity.status == AgentRegistryStatus::Deleted
        && stored_status == enum_string(&AgentRegistryStatus::Deleted)?
        && identity.deleted_at.is_some()
    {
        return Ok(false);
    }
    identity.status = AgentRegistryStatus::Deleted;
    if identity.deleted_at.is_none() {
        identity.deleted_at = Some(now);
    }
    identity.updated_at = now;
    identity.revision = identity.revision.saturating_add(1);
    upsert_agent_identity_tx(tx, identity)?;
    Ok(true)
}

pub(crate) fn decode_work_item_payload(payload: &str) -> Result<WorkItemRecord> {
    serde_json::from_str(payload).context("decoding work item payload from runtime db")
}

pub(crate) fn decode_work_item_delegation_payload(
    payload: &str,
) -> Result<WorkItemDelegationRecord> {
    serde_json::from_str(payload).context("decoding work item delegation payload from runtime db")
}

pub(crate) fn decode_work_item_continuation_payload(
    payload: &str,
) -> Result<WorkItemContinuationFrame> {
    serde_json::from_str(payload).context("decoding work item continuation payload from runtime db")
}

pub(crate) fn decode_context_episode_payload(payload: &str) -> Result<ContextEpisodeRecord> {
    serde_json::from_str(payload).context("decoding context episode payload from runtime db")
}

pub(crate) fn decode_external_trigger_payload(payload: &str) -> Result<ExternalTriggerRecord> {
    serde_json::from_str(payload).context("decoding external trigger payload from runtime db")
}

pub(crate) fn decode_operator_notification_payload(
    payload: &str,
) -> Result<OperatorNotificationRecord> {
    serde_json::from_str(payload).context("decoding operator notification payload from runtime db")
}

pub(crate) fn decode_operator_transport_binding_payload(
    payload: &str,
) -> Result<OperatorTransportBinding> {
    serde_json::from_str(payload)
        .context("decoding operator transport binding payload from runtime db")
}

pub(crate) fn decode_operator_delivery_record_payload(
    payload: &str,
) -> Result<OperatorDeliveryRecord> {
    serde_json::from_str(payload)
        .context("decoding operator delivery record payload from runtime db")
}

pub(crate) fn decode_task_payload(payload: &str) -> Result<TaskRecord> {
    serde_json::from_str(payload).context("decoding task payload from runtime db")
}

pub(crate) fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("parsing timestamp: {value}"))
}

pub(crate) fn parse_optional_timestamp(value: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    value.map(parse_timestamp).transpose()
}

/// Convert an [`anyhow::Error`] from row decoding into a [`rusqlite::Error`]
/// that preserves the original message. Previously these errors were mapped to
/// [`rusqlite::Error::InvalidQuery`] whose Display text ("Query is not
/// read-only") is completely unrelated to the actual failure (e.g. a malformed
/// timestamp or invalid enum value), making diagnosis nearly impossible.
fn wait_condition_decode_error(e: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("{e:#}").into(),
    )
}

pub(crate) fn decode_wait_condition_row(row: &rusqlite::Row<'_>) -> Result<WaitConditionRecord> {
    let id: String = row.get(0)?;
    let agent_id: String = row.get(1)?;
    let work_item_id: Option<String> = row.get(2)?;
    let status_str: String = row.get(3)?;
    let kind_str: String = row.get(4)?;
    let source: Option<String> = row.get(5)?;
    let subject_ref: Option<String> = row.get(6)?;
    let waiting_for: String = row.get(7)?;
    let created_at_str: String = row.get(8)?;
    let updated_at_str: String = row.get(9)?;
    let expires_at_str: Option<String> = row.get(10)?;
    let resolved_at_str: Option<String> = row.get(11)?;
    let cancelled_at_str: Option<String> = row.get(12)?;
    let turn_id: Option<String> = row.get(13)?;
    let trigger_message_id: Option<String> = row.get(14)?;
    let triggered_at_str: Option<String> = row.get(15)?;
    let wake_sources_json: String = row.get(16)?;
    let continuation_json: Option<String> = row.get(17)?;
    let status: WaitConditionStatus = serde_json::from_value(serde_json::Value::String(status_str))
        .context("parsing wait condition status")?;
    let kind: WaitConditionKind = serde_json::from_value(serde_json::Value::String(kind_str))
        .context("parsing wait condition kind")?;
    let wake_sources: Vec<WakeSource> =
        serde_json::from_str(&wake_sources_json).context("parsing wake_sources_json")?;
    let continuation: Option<serde_json::Value> = continuation_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("parsing continuation_json")?;
    Ok(WaitConditionRecord {
        id,
        agent_id,
        work_item_id,
        status,
        kind,
        source,
        subject_ref,
        waiting_for,
        wake_sources,
        continuation,
        created_at: parse_timestamp(&created_at_str)?,
        updated_at: parse_timestamp(&updated_at_str)?,
        expires_at: parse_optional_timestamp(expires_at_str.as_deref())?,
        resolved_at: parse_optional_timestamp(resolved_at_str.as_deref())?,
        cancelled_at: parse_optional_timestamp(cancelled_at_str.as_deref())?,
        turn_id,
        trigger_message_id,
        triggered_at: parse_optional_timestamp(triggered_at_str.as_deref())?,
    })
}

fn upsert_agent_bootstrap_tx(tx: &Transaction<'_>, record: &AgentBootstrapRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.summary().status)?;
    tx.execute(
        "INSERT INTO agent_bootstraps (
            agent_id, status, revision, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(agent_id) DO UPDATE SET
            status = excluded.status,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.revision > agent_bootstraps.revision",
        params![
            record.agent_id,
            status,
            record.revision,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn decode_queue_entry_payload(payload: &str) -> Result<QueueEntryRecord> {
    serde_json::from_str(payload).context("decoding queue entry payload from runtime db")
}

pub(crate) fn decode_wait_condition_payload(payload: &str) -> Result<WaitConditionRecord> {
    serde_json::from_str(payload).context("decoding wait condition payload from runtime db")
}

pub(crate) fn decode_timer_payload(payload: &str) -> Result<TimerRecord> {
    serde_json::from_str(payload).context("decoding timer payload from runtime db")
}

pub(crate) fn decode_turn_record_payload(payload: &str) -> Result<TurnRecord> {
    serde_json::from_str(payload).context("decoding turn record payload from runtime db")
}

pub(crate) fn decode_message_payload(payload: &str) -> Result<MessageEnvelope> {
    serde_json::from_str(payload).context("decoding message payload from runtime db")
}

pub(crate) fn decode_transcript_entry_payload(payload: &str) -> Result<TranscriptEntry> {
    serde_json::from_str(payload).context("decoding transcript entry payload from runtime db")
}

pub(crate) fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("expected enum to serialize as string"))
}

pub(crate) fn task_detail_string(detail: &Option<serde_json::Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

pub(crate) fn task_detail_i64(detail: &Option<serde_json::Value>, key: &str) -> Option<i64> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_i64())
}

pub(crate) fn is_active_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    )
}

pub(crate) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

pub(crate) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// Same-query readers for one context acquisition transaction.
pub(crate) fn context_workspace_entries_on(
    connection: &rusqlite::Connection,
) -> Result<Vec<WorkspaceEntry>> {
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM workspace_entries
         ORDER BY updated_at DESC, created_at DESC, workspace_id ASC",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut records: Vec<_> = rows
        .map(|row| decode_workspace_entry_payload(&row?))
        .collect::<Result<_>>()?;
    records.reverse();
    Ok(records)
}

pub(crate) fn context_execution_roots_on(
    connection: &rusqlite::Connection,
    workspace_id: &str,
) -> Result<Vec<ExecutionRootEntry>> {
    let mut statement = connection.prepare(
        "SELECT payload_json FROM execution_root_entries
             WHERE workspace_id = ?1 AND removed_at IS NULL
             ORDER BY created_at ASC",
    )?;
    let rows = statement.query_map([workspace_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_execution_root_entry_payload(&row?))
        .collect()
}

pub(crate) fn context_open_work_items_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<Vec<WorkItemRecord>> {
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM work_items
         WHERE agent_id = ?1 AND state = 'open'",
    )?;
    let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_work_item_payload(&row?)).collect()
}

pub(crate) fn context_non_open_work_items_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
    limit: usize,
) -> Result<Vec<WorkItemRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM work_items
         WHERE agent_id = ?1 AND state != 'open'
         ORDER BY updated_at DESC, created_at DESC, work_item_id ASC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_work_item_payload(&row?)).collect()
}

pub(crate) fn context_active_continuations_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<Vec<WorkItemContinuationFrame>> {
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM work_item_continuations
         WHERE agent_id = ?1 AND state = 'active'
         ORDER BY updated_at DESC, created_at DESC, continuation_id ASC",
    )?;
    let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_work_item_continuation_payload(&row?))
        .collect()
}

pub(crate) fn context_external_triggers_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<Vec<ExternalTriggerRecord>> {
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM external_triggers
         WHERE target_agent_id = ?1
         ORDER BY created_at DESC, external_trigger_id ASC",
    )?;
    let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_external_trigger_payload(&row?))
        .collect()
}

pub(crate) fn context_tasks_query_on(
    connection: &rusqlite::Connection,
    _agent_id: &str,
    where_clause: &str,
    params: [&str; 1],
    limit: usize,
) -> Result<Vec<TaskRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT payload_json
         FROM tasks
         WHERE {where_clause}
         ORDER BY updated_at DESC, created_at DESC, task_id ASC
         LIMIT {limit}",
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_task_payload(&row?)).collect()
}

pub(crate) fn context_active_waits_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<Vec<WaitConditionRecord>> {
    let mut statement = connection.prepare(
        "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
            subject_ref, waiting_for, created_at, updated_at, expires_at,
            resolved_at, cancelled_at, last_turn_id, trigger_message_id, triggered_at,
            wake_sources_json, continuation_json
         FROM wait_conditions
         WHERE agent_id = ?1 AND status = 'active'
         ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        decode_wait_condition_row(row).map_err(wait_condition_decode_error)
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))
}

pub(crate) fn context_turn_by_id_on(
    connection: &rusqlite::Connection,
    agent_id: Option<&str>,
    turn_id: &str,
) -> Result<Option<TurnRecord>> {
    let payload = if let Some(agent_id) = agent_id {
        connection
            .query_row(
                "SELECT payload_json
                 FROM turn_records
                 WHERE agent_id = ?1 AND turn_id = ?2
                 LIMIT 1",
                params![agent_id, turn_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
    } else {
        connection
            .query_row(
                "SELECT payload_json
                 FROM turn_records
                 WHERE turn_id = ?1
                 LIMIT 1",
                [turn_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
    };
    payload
        .map(|payload| decode_turn_record_payload(&payload))
        .transpose()
}

pub(crate) fn context_recent_turns_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
    limit: usize,
) -> Result<Vec<TurnRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM turn_records
         WHERE agent_id = ?1
         ORDER BY turn_index DESC, created_at DESC, turn_id ASC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
    let mut records = rows
        .map(|row| decode_turn_record_payload(&row?))
        .collect::<Result<Vec<_>>>()?;
    records.reverse();
    Ok(records)
}

pub(crate) fn context_owner_turns_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
    owner: &crate::types::TurnOwner,
    limit: usize,
) -> Result<Vec<TurnRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let (owner_kind, owner_id) = owner.index_parts();
    let mut statement = connection.prepare(
        "SELECT payload_json
         FROM turn_records
         WHERE agent_id = ?1
           AND owner_kind = ?2
           AND owner_id IS ?3
         ORDER BY turn_index DESC, created_at DESC, turn_id ASC
         LIMIT ?4",
    )?;
    let rows = statement.query_map(params![agent_id, owner_kind, owner_id, limit], |row| {
        row.get::<_, String>(0)
    })?;
    let mut records = rows
        .map(|row| decode_turn_record_payload(&row?))
        .collect::<Result<Vec<_>>>()?;
    records.reverse();
    Ok(records)
}

pub(crate) fn context_anchor_messages_on(
    connection: &rusqlite::Connection,
    agent_id: Option<&str>,
) -> Result<Vec<MessageEnvelope>> {
    if let Some(agent_id) = agent_id {
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM messages
             WHERE agent_id = ?1
             ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_message_payload(&row?)).collect()
    } else {
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM messages
             ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_message_payload(&row?)).collect()
    }
}

pub(crate) fn context_message_by_id_on(
    connection: &rusqlite::Connection,
    agent_id: Option<&str>,
    message_id: &str,
) -> Result<Option<MessageEnvelope>> {
    let payload = if let Some(agent_id) = agent_id {
        connection
            .query_row(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1 AND message_id = ?2
                 LIMIT 1",
                params![agent_id, message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
    } else {
        connection
            .query_row(
                "SELECT payload_json
                 FROM messages
                 WHERE message_id = ?1
                 LIMIT 1",
                [message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
    };
    payload
        .map(|payload| decode_message_payload(&payload))
        .transpose()
}

pub(crate) fn context_recent_transcript_on(
    connection: &rusqlite::Connection,
    agent_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TranscriptEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut records = if let Some(agent_id) = agent_id {
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM transcript_entries
             WHERE agent_id = ?1
             ORDER BY COALESCE(transcript_seq, 9223372036854775807) DESC, created_at DESC, evidence_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_transcript_entry_payload(&row?))
            .collect::<Result<Vec<_>>>()?
    } else {
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM transcript_entries
             ORDER BY COALESCE(transcript_seq, 9223372036854775807) DESC, created_at DESC, evidence_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_transcript_entry_payload(&row?))
            .collect::<Result<Vec<_>>>()?
    };
    records.reverse();
    Ok(records)
}

pub(crate) fn context_transcript_for_turns_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
    turn_ids: &[String],
) -> Result<Vec<TranscriptEntry>> {
    if turn_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", turn_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection.prepare(&format!(
        "SELECT payload_json
         FROM transcript_entries
         WHERE agent_id = ?
           AND turn_id IN ({placeholders})
         ORDER BY COALESCE(transcript_seq, 9223372036854775807) ASC,
                  created_at ASC,
                  evidence_id ASC"
    ))?;
    let rows = statement.query_map(
        params_from_iter(std::iter::once(agent_id).chain(turn_ids.iter().map(String::as_str))),
        |row| row.get::<_, String>(0),
    )?;
    rows.map(|row| decode_transcript_entry_payload(&row?))
        .collect()
}

pub(crate) fn context_transcript_by_id_on(
    connection: &rusqlite::Connection,
    agent_id: Option<&str>,
    entry_id: &str,
) -> Result<Option<TranscriptEntry>> {
    let payload: Option<String> = if let Some(agent_id) = agent_id {
        connection
            .query_row(
                "SELECT payload_json
                 FROM transcript_entries
                 WHERE agent_id = ?1 AND evidence_id = ?2
                 LIMIT 1",
                params![agent_id, entry_id],
                |row| row.get(0),
            )
            .optional()?
    } else {
        connection
            .query_row(
                "SELECT payload_json
                 FROM transcript_entries
                 WHERE evidence_id = ?1
                 LIMIT 1",
                [entry_id],
                |row| row.get(0),
            )
            .optional()?
    };
    payload
        .map(|payload| decode_transcript_entry_payload(&payload))
        .transpose()
}

pub(crate) fn context_recent_evidence_on(
    connection: &rusqlite::Connection,
    kind: EvidenceKind,
    agent_id: &str,
    limit: usize,
) -> Result<Vec<EvidencePayloadRow>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT payload_json
         FROM {}
         WHERE agent_id = ?1
         ORDER BY created_at DESC, evidence_id DESC
         LIMIT ?2",
        kind.table_name()
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![agent_id, limit], |row| {
        Ok(EvidencePayloadRow {
            payload_json: row.get(0)?,
        })
    })?;
    let mut records: Vec<_> = rows
        .map(|row| row.map_err(Into::into))
        .collect::<Result<_>>()?;
    records.reverse();
    Ok(records)
}

pub(crate) fn context_evidence_by_id_on(
    connection: &rusqlite::Connection,
    kind: EvidenceKind,
    agent_id: &str,
    evidence_id: &str,
) -> Result<Option<EvidencePayloadRow>> {
    let sql = format!(
        "SELECT payload_json
         FROM {}
         WHERE agent_id = ?1 AND evidence_id = ?2
         LIMIT 1",
        kind.table_name()
    );
    connection
        .query_row(&sql, params![agent_id, evidence_id], |row| {
            Ok(EvidencePayloadRow {
                payload_json: row.get(0)?,
            })
        })
        .optional()
        .map_err(Into::into)
}

pub(crate) fn context_work_item_by_id_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
    work_item_id: &str,
) -> Result<Option<WorkItemRecord>> {
    connection
        .query_row(
            "SELECT payload_json FROM work_items WHERE agent_id = ?1 AND work_item_id = ?2",
            params![agent_id, work_item_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_work_item_payload(&payload))
        .transpose()
}

pub(crate) fn context_work_items_by_ids_on(
    connection: &rusqlite::Connection,
    agent_id: &str,
    ids: &[String],
) -> Result<Vec<WorkItemRecord>> {
    let mut records = Vec::new();
    for chunk in ids.chunks(128) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT payload_json FROM work_items WHERE agent_id = ? AND work_item_id IN ({placeholders})");
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(std::iter::once(agent_id).chain(chunk.iter().map(String::as_str))),
            |row| row.get::<_, String>(0),
        )?;
        records.extend(
            rows.map(|row| decode_work_item_payload(&row?))
                .collect::<Result<Vec<_>>>()?,
        );
    }
    Ok(records)
}

// Focused qualification fixtures use these non-receipted adapters. Production
// evidence writes use the prepared whole-transaction command.
#[cfg(test)]
impl TurnRecordRepository<'_> {
    pub(crate) fn upsert_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &TurnRecord,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<CommittedTurnRecord>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Turn,
                    &record.turn_id,
                ),
            )?;
            upsert_turn_record_tx(tx, record)?;
            let actual = committed_turn_record_tx(tx, &record.agent_id, &record.turn_id)?;
            let facts = [
                super::input_evidence_snapshot::CanonicalInputEvidence::Turn(actual.record.clone()),
            ];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
}
#[cfg(test)]
impl MessageRepository<'_> {
    pub(crate) fn upsert_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &MessageEnvelope,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<MessageEnvelope>> {
        self.upsert_with_index_changes_in_scope(scope, record, &[])
    }
    pub(crate) fn upsert_with_index_changes_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &MessageEnvelope,
        changes: &[RuntimeIndexChange],
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<MessageEnvelope>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    &record.id,
                ),
            )?;
            upsert_message_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)?;
            let actual = committed_message_tx(tx, &record.agent_id, &record.id)?;
            let facts =
                [super::input_evidence_snapshot::CanonicalInputEvidence::Message(actual.clone())];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
    pub(crate) fn append_with_index_changes_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &MessageEnvelope,
        changes: &[RuntimeIndexChange],
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<MessageEnvelope>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    &record.id,
                ),
            )?;
            let (stored, inserted) = append_message_tx(tx, record)?;
            if inserted {
                insert_runtime_index_changes_tx(tx, changes)?;
            }
            let actual = committed_message_tx(tx, &stored.agent_id, &stored.id)?;
            let facts =
                [super::input_evidence_snapshot::CanonicalInputEvidence::Message(actual.clone())];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
}
#[cfg(test)]
impl TranscriptRepository<'_> {
    pub(crate) fn upsert_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &TranscriptEntry,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TranscriptEntry>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Transcript,
                    &record.id,
                ),
            )?;
            upsert_transcript_entry_tx(tx, record)?;
            let actual = committed_transcript_tx(tx, &record.agent_id, &record.id)?;
            let facts = [
                super::input_evidence_snapshot::CanonicalInputEvidence::Transcript(actual.clone()),
            ];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
    pub(crate) fn append_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &TranscriptEntry,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TranscriptEntry>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Transcript,
                    &record.id,
                ),
            )?;
            append_transcript_entry_tx(tx, record)?;
            let actual = committed_transcript_tx(tx, &record.agent_id, &record.id)?;
            let facts = [
                super::input_evidence_snapshot::CanonicalInputEvidence::Transcript(actual.clone()),
            ];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
}
#[cfg(test)]
impl EvidenceRepository<'_> {
    pub(crate) fn append_message_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &MessageEnvelope,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<MessageEnvelope>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    &record.id,
                ),
            )?;
            insert_message_evidence_tx(tx, record)?;
            let actual = committed_message_tx(tx, &record.agent_id, &record.id)?;
            let facts =
                [super::input_evidence_snapshot::CanonicalInputEvidence::Message(actual.clone())];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
    pub(crate) fn append_transcript_entry_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &TranscriptEntry,
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<TranscriptEntry>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Transcript,
                    &record.id,
                ),
            )?;
            insert_transcript_evidence_tx(tx, record)?;
            let actual = committed_transcript_tx(tx, &record.agent_id, &record.id)?;
            let facts = [
                super::input_evidence_snapshot::CanonicalInputEvidence::Transcript(actual.clone()),
            ];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
    pub(crate) fn append_tool_execution_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &ToolExecutionRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<()>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::ToolExecution,
                    &record.id,
                ),
            )?;
            insert_tool_evidence_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)?;
            let actual = committed_tool_tx(tx, &record.agent_id, &record.id)?;
            let facts =
                [super::input_evidence_snapshot::CanonicalInputEvidence::ToolExecution(actual)];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, (), &facts)
        })
    }
    pub(crate) fn append_brief_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        record: &BriefRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<super::input_evidence_writes::InputEvidenceCommit<BriefRecord>> {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Brief,
                    &record.id,
                ),
            )?;
            insert_brief_evidence_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)?;
            let actual = committed_brief_tx(tx, &record.agent_id, &record.id)?;
            let facts =
                [super::input_evidence_snapshot::CanonicalInputEvidence::Brief(actual.clone())];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
    pub(crate) fn append_brief_with_created_event_in_scope(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        agent_id: Option<&str>,
        record: &BriefRecord,
        event: &AuditEvent,
        changes: &[RuntimeIndexChange],
    ) -> Result<
        super::input_evidence_writes::InputEvidenceCommit<super::evidence::BriefCreatedCommit>,
    > {
        self.db.transaction(|tx| {
            super::input_evidence_writes::validate_input_evidence_scope_tx(
                tx,
                scope,
                &record.agent_id,
            )?;
            anyhow::ensure!(
                agent_id == Some(scope.agent_id.as_str()),
                "Brief audit owner differs from reserved input scope"
            );
            super::input_evidence_snapshot::validate_existing_input_evidence_tx(
                tx,
                scope,
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Brief,
                    &record.id,
                ),
            )?;
            let actual = append_brief_with_created_event_tx(tx, agent_id, record, event, changes)?;
            let facts = [
                super::input_evidence_snapshot::CanonicalInputEvidence::Brief(actual.brief.clone()),
            ];
            super::input_evidence_writes::acknowledge_input_evidence_tx(tx, scope, actual, &facts)
        })
    }
}
