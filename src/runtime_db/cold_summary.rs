//! Finite inactive-instance Summary preparation under the original owner.
//! These domains share this SQL view, not a fabricated publication token.
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use rusqlite::Transaction;

use super::{
    context_inputs::ContextDurableRead,
    current_query::{scheduler_metadata, CurrentSchedulerMetadata},
    input_evidence_snapshot::{
        prepare_input_evidence_with_scope_read_tx, InputEvidenceBootstrapSnapshot,
        InputEvidenceLoadRequest,
    },
    scheduling_snapshot::{
        prepare_cold_scheduling_query_tx, ColdSchedulingQuery, SchedulingDomainLoadRequest,
    },
};
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_input_evidence::InputEvidenceScope,
    resident_scheduling::SchedulingCaptureRequest,
    resident_work::{WorkCaptureRequest, WorkFamily, WorkKey},
    SchedulerReadDomains,
};
use crate::types::{AgentIdentityRecord, OperatorNotificationRecord};

pub(crate) struct ColdSummaryRead {
    pub identity: AgentIdentityRecord,
    pub domains: Option<SchedulerReadDomains>,
    pub metadata: CurrentSchedulerMetadata,
    pub notifications: Vec<OperatorNotificationRecord>,
    pub scheduling_usage: (usize, usize),
    pub scheduling_guards: Vec<Arc<BudgetViewGuard>>,
}

pub(crate) fn prepare_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ColdSummaryRead> {
    let limits = ledger.limits();
    let records = usize::try_from(limits.record_limit)?;
    let bytes = usize::try_from(
        limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("cold Summary limit overflow")?,
    )?;
    let scheduling = prepare_cold_scheduling_query_tx(
        tx,
        scope,
        &SchedulingDomainLoadRequest {
            capture: SchedulingCaptureRequest::current(),
            record_limit: records,
            byte_limit: bytes,
        },
        ledger,
    )?;
    let metadata = scheduler_metadata(tx, scope)?;
    let current = match scheduling {
        ColdSchedulingQuery::Current(current) => current,
        ColdSchedulingQuery::ProvenEmptyNoState {
            identity,
            material_usage,
            guards,
        } => {
            prove_empty_summary_domains(tx, scope)?;
            let notifications =
                super::repositories::read_operator_notifications_on(tx, &scope.agent_id, 10)?;
            return Ok(ColdSummaryRead {
                identity,
                domains: None,
                metadata,
                notifications,
                scheduling_usage: material_usage,
                scheduling_guards: guards,
            });
        }
    };
    let mut facts = current.facts;
    let state = facts
        .state
        .as_deref()
        .context("cold Summary State missing")?;
    let mut work_request = WorkCaptureRequest::current();
    let (task_request, task_selector_budget) = crate::runtime::scheduler_task_request(
        scope,
        &work_request,
        None,
        None,
        ledger,
        records,
        bytes,
    )?;
    let tasks = super::task_snapshot::prepare_task_domain_snapshot_with_budget_tx(
        tx,
        scope,
        &super::task_snapshot::TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: task_request.clone(),
            record_limit: records,
            byte_limit: bytes,
        },
        ledger,
    )?
    .require_current()?;
    crate::diagnostics::resource_io::admit_cloned_value(&tasks.work_roots)?;
    work_request.mandatory_roots.extend(
        tasks
            .work_roots
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    crate::diagnostics::resource_io::admit_cloned_value(&state.current_work_item_id)?;
    work_request
        .latest_result_work_ids
        .extend(state.current_work_item_id.clone());
    let work = super::work_snapshot::prepare_work_domain_snapshot_tx(
        tx,
        scope,
        &super::work_snapshot::WorkDomainLoadRequest {
            capture: work_request.clone(),
            record_limit: records,
            byte_limit: bytes,
        },
    )?
    .require_current()?;
    ensure!(
        tasks.facts.identity == facts.identity
            && tasks.facts.state == facts.state
            && tasks.facts.control_revision == facts.control_revision
            && work.facts.identity == facts.identity
            && work.facts.state == facts.state
            && work.facts.control_revision == facts.control_revision,
        "cold Summary domain read views differ"
    );
    let mut windows = Vec::new();
    crate::runtime::include_scheduler_brief_window(&mut windows);
    let request = InputEvidenceLoadRequest {
        query: None,
        windows,
        mandatory_roots: work.owning_input_roots,
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: records,
        byte_limit: bytes,
    };
    let scope_read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
    let input = match prepare_input_evidence_with_scope_read_tx(
        tx,
        scope,
        request.borrowed(),
        false,
        scope_read,
    )? {
        InputEvidenceBootstrapSnapshot::Current(current) => current,
        InputEvidenceBootstrapSnapshot::Retired { .. } => {
            anyhow::bail!("cold Summary Input retired")
        }
    };
    ensure!(
        input.facts.identity == facts.identity
            && input.facts.state == facts.state
            && input.facts.control_revision == facts.control_revision,
        "cold Summary Input read view differs"
    );
    let domains = SchedulerReadDomains {
        state: facts.state.take().context("cold Summary State absent")?,
        work: work.resident.capture(&work_request)?,
        tasks: tasks.resident.capture(&task_request)?,
        inputs: input.resident.capture()?,
        scheduling: current
            .resident
            .capture(&SchedulingCaptureRequest::current())?,
        task_selector_budget,
    };
    Ok(ColdSummaryRead {
        identity: facts
            .identity
            .take()
            .context("cold Summary identity missing")?,
        domains: Some(domains),
        metadata,
        notifications: super::repositories::read_operator_notifications_on(
            tx,
            &scope.agent_id,
            10,
        )?,
        scheduling_usage: current.material_usage,
        scheduling_guards: current._read_material,
    })
}

fn prove_empty_summary_domains(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<()> {
    use rusqlite::params;
    // Metadata predicates only: never decode a mixed-domain body in NoState.
    for (table, column) in [
        ("tasks", "owner_agent_id"),
        ("task_result_settlements", "agent_id"),
        ("work_items", "agent_id"),
        ("wait_conditions", "agent_id"),
        ("work_item_continuations", "agent_id"),
        ("messages", "agent_id"),
        ("briefs", "agent_id"),
        ("turn_records", "agent_id"),
        ("tool_executions", "agent_id"),
        ("execution_protocol_attempts", "agent_id"),
        ("execution_protocol_work_items", "agent_id"),
        ("work_domain_records", "agent_id"),
        ("task_domain_records", "agent_id"),
        ("input_evidence_current", "agent_id"),
        ("task_domain_import_inventory", "agent_id"),
    ] {
        let nonempty: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column}=?1)"),
            [&scope.agent_id],
            |row| row.get(0),
        )?;
        ensure!(!nonempty, "NoState Summary has unproved {table} members");
    }
    let proven: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_episode_continuity c
         WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.policy_version=1
         AND ((c.mode='fresh_empty' AND c.seed_id IS NULL) OR
         (c.mode='legacy_adopted'
          AND EXISTS(SELECT 1 FROM episode_continuity_seeds s WHERE s.seed_id=c.seed_id
              AND s.agent_id=c.agent_id AND s.identity_incarnation=c.identity_incarnation
              AND s.import_required=0)
          AND EXISTS(SELECT 1 FROM work_domain_captures w WHERE w.seed_id=c.seed_id)
          AND EXISTS(SELECT 1 FROM task_domain_captures t WHERE t.seed_id=c.seed_id)
          AND EXISTS(SELECT 1 FROM input_evidence_captures i WHERE i.seed_id=c.seed_id)
          AND NOT EXISTS(SELECT 1 FROM work_domain_eligibility w WHERE w.seed_id=c.seed_id)
          AND NOT EXISTS(SELECT 1 FROM task_domain_eligibility t WHERE t.seed_id=c.seed_id)
          AND NOT EXISTS(SELECT 1 FROM input_evidence_eligibility i WHERE i.seed_id=c.seed_id)
          AND NOT EXISTS(SELECT 1 FROM episode_continuity_members e WHERE e.seed_id=c.seed_id))))",
        params![scope.agent_id, scope.identity_incarnation],
        |row| row.get(0),
    )?;
    ensure!(
        proven,
        "NoState Summary lacks complete empty-domain continuity"
    );
    Ok(())
}
