//! Owner-held joint recovery prepares only the original qualified Task namespace.
//! Old receipt selectors select current Task facts; they never authorize adoption.
use std::{collections::BTreeSet, sync::Arc};

use anyhow::{ensure, Context, Result};
use rusqlite::{OptionalExtension, Transaction};

use super::{context_inputs::ContextDurableRead, task_snapshot};
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
            OwnedBytesEstimate,
        },
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope},
        resident_task::{
            CanonicalTaskFact, CapturedTasks, ResidentTaskDomain, TaskCaptureRequest, TaskFamily,
            TaskKey, TaskResultDependencyRelation,
        },
    },
    types::{AgentState, MessageEnvelope, MessageKind, MessageOrigin},
};

pub(crate) struct PreparedTaskDomains {
    pub resident: Arc<ResidentTaskDomain>,
    pub input_roots: BTreeSet<InputEvidenceKey>,
    pub work_roots: BTreeSet<String>,
    // These IDs are separate owned copies, held through joint domain preparation.
    _dependency_budget_view: Arc<BudgetViewGuard>,
    _selector_budget_views: Vec<Arc<BudgetViewGuard>>,
    // The finite predicate observation owns its actual predecode guards until
    // all joint Work/Input dependency preparation has completed.
    _result_dependencies: Vec<task_snapshot::PreparedTaskResultDependency>,
}

/// These selectors coexist with the R1 body loader, rather than borrowing its
/// later guard. The guards retain estimates only, never Message/Task bodies.
struct TaskSelectorMaterial<'scope, 'phase, 'material> {
    scope: &'scope InputEvidenceScope,
    budget: &'scope InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
    records: usize,
    bytes: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
    phase: Option<&'phase mut task_snapshot::read_preparation::TaskReadMaterial<'material>>,
}
impl<'scope, 'phase, 'material> TaskSelectorMaterial<'scope, 'phase, 'material> {
    fn new(
        scope: &'scope InputEvidenceScope,
        budget: &'scope InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
        phase: Option<&'phase mut task_snapshot::read_preparation::TaskReadMaterial<'material>>,
    ) -> Self {
        Self {
            scope,
            budget,
            record_limit: record_limit.min(65_536),
            byte_limit: byte_limit.min(64 * 1024 * 1024),
            records: 0,
            bytes: 0,
            guards: Vec::new(),
            phase,
        }
    }

    // Four copies cover current keys, the merged request, the qualified
    // selector result and the short current-dependency capture request.
    fn charge(&mut self, bytes: usize, records: usize) -> Result<()> {
        if let Some(phase) = self.phase.as_deref_mut() {
            return phase.charge_ids(bytes, records);
        }
        let bytes = bytes
            .checked_mul(4)
            .context("Task selector bytes overflow")?;
        let records = records
            .checked_mul(4)
            .context("Task selector count overflow")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("Task selector total overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("Task selector total overflow")?;
        ensure!(
            self.bytes <= self.byte_limit && self.records <= self.record_limit,
            task_snapshot::TaskSnapshotError::ResourceExhausted("current selectors")
        );
        self.guards.push(self.budget.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(bytes)?,
            },
        )?);
        Ok(())
    }

    fn charge_key(&mut self, id: &str) -> Result<()> {
        self.charge(
            usize::try_from(serialized_bytes(&id)?)?
                .checked_add(128)
                .context("Task selector key overflow")?,
            1,
        )
    }
}

type TaskSourceHeader = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn current_source_header_tx(
    tx: &Transaction<'_>,
    source_id: &str,
    material: &mut TaskSelectorMaterial<'_, '_, '_>,
) -> Result<Option<TaskSourceHeader>> {
    // A bound source may be Dequeued, outside pending's body inventory. Admit
    // its raw JSON before even SQLite parses the finite header projection.
    let raw: Option<usize> = tx
        .query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE message_id=?1",
            [source_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else { return Ok(None) };
    material.charge(raw, 1)?;
    // Count actual projected Strings before their first owned decode.
    let bytes: Option<usize> = tx.query_row(
        "SELECT length(CAST(message_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(kind AS BLOB))+COALESCE(length(CAST(json_extract(payload_json,'$.id') AS BLOB)),0)+COALESCE(length(CAST(json_extract(payload_json,'$.agent_id') AS BLOB)),0)+COALESCE(length(CAST(json_extract(payload_json,'$.task_id') AS BLOB)),0)+CASE WHEN json_extract(payload_json,'$.origin.kind')='task' THEN COALESCE(length(CAST(json_extract(payload_json,'$.origin.task_id') AS BLOB)),0) ELSE 0 END FROM messages WHERE message_id=?1",
        [source_id],|row| row.get(0),
    ).optional()?;
    let Some(bytes) = bytes else { return Ok(None) };
    // JSON-encoded selector IDs can expand each byte to a six-byte escape.
    material.charge(
        bytes
            .checked_mul(6)
            .and_then(|x| x.checked_add(7 * 130))
            .context("Task source header estimate overflow")?,
        7,
    )?;
    tx.query_row(
        "SELECT message_id,agent_id,kind,json_extract(payload_json,'$.id'),json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.task_id'),CASE WHEN json_extract(payload_json,'$.origin.kind')='task' THEN json_extract(payload_json,'$.origin.task_id') END FROM messages WHERE message_id=?1",
        [source_id],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    ).optional().map_err(Into::into)
}

pub(crate) fn prepare_bootstrap_task_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    selectors: &TaskCaptureRequest,
    pending: &[MessageEnvelope],
    record_limit: usize,
    byte_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskDomains> {
    prepare(
        tx,
        scope,
        state,
        selectors,
        pending,
        record_limit,
        byte_limit,
        budget,
        false,
        None,
        None,
    )
}

/// Only an original positive receipt or exclusive startup owner calls this entry.
/// The underlying R1 loader accepts same-incarnation Active|Deleting, not new work.
#[cfg(test)]
pub(crate) fn prepare_original_task_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    selectors: &TaskCaptureRequest,
    pending: &[MessageEnvelope],
    record_limit: usize,
    byte_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskDomains> {
    prepare(
        tx,
        scope,
        state,
        selectors,
        pending,
        record_limit,
        byte_limit,
        budget,
        true,
        None,
        None,
    )
}

pub(crate) fn prepare_original_task_domains_with_queries_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    selectors: &TaskCaptureRequest,
    pending: &[MessageEnvelope],
    record_limit: usize,
    byte_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
) -> Result<PreparedTaskDomains> {
    prepare(
        tx,
        scope,
        state,
        selectors,
        pending,
        record_limit,
        byte_limit,
        budget,
        true,
        query,
        None,
    )
}

pub(in crate::runtime_db) fn prepare_original_task_domains_using_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    selectors: &TaskCaptureRequest,
    pending: &[MessageEnvelope],
    record_limit: usize,
    byte_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    material: &mut task_snapshot::read_preparation::TaskReadMaterial<'_>,
) -> Result<PreparedTaskDomains> {
    prepare(
        tx,
        scope,
        state,
        selectors,
        pending,
        record_limit,
        byte_limit,
        budget,
        true,
        query,
        Some(material),
    )
}

fn prepare(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    selectors: &TaskCaptureRequest,
    pending: &[MessageEnvelope],
    record_limit: usize,
    byte_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    original: bool,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    mut material: Option<&mut task_snapshot::read_preparation::TaskReadMaterial<'_>>,
) -> Result<PreparedTaskDomains> {
    let facts = if let Some(phase) = material.as_deref_mut() {
        phase
            .scope_facts_tx(tx, scope, original)?
            .ok_or(task_snapshot::TaskSnapshotError::RetiredScope)?
    } else {
        ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?
    };
    ensure!(
        facts.state.as_ref() == Some(state) && facts.control_revision.is_some(),
        "joint Task preparation State differs from its original view"
    );
    // Compatibility selectors include old raw cleanup targets. Only existing
    // qualified membership may select them; current roots never invent membership.
    let mut selector_material =
        TaskSelectorMaterial::new(scope, budget, record_limit, byte_limit, material);
    // Count borrowed IDs before cloning either the original or qualified sets.
    for key in &selectors.required_keys {
        selector_material.charge_key(&key.id)?;
    }
    for message in pending {
        ensure!(
            message.agent_id == scope.agent_id,
            "joint Task pending scope differs"
        );
        if matches!(
            message.kind,
            MessageKind::TaskResult | MessageKind::TaskStatus
        ) {
            if let MessageOrigin::Task { task_id } = &message.origin {
                if message.task_id.as_deref() == Some(task_id.as_str()) {
                    selector_material.charge_key(task_id)?;
                }
            }
        }
    }
    if let Some(query) = query {
        for key in &query.required_keys {
            selector_material.charge_key(&key.id)?;
        }
        for id in query.output_task_ids.iter().chain(&query.reply_message_ids) {
            selector_material.charge_key(id)?;
        }
    }
    let mut capture = selectors.clone();
    capture.required_keys =
        task_snapshot::qualified_task_recovery_selectors_tx(tx, scope, &capture.required_keys)?;
    let relation_request = task_snapshot::TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit,
        byte_limit,
    };
    let mut current_task_keys = BTreeSet::new();
    let mut result_dependencies = Vec::new();
    for message in pending {
        ensure!(
            message.agent_id == scope.agent_id,
            "joint Task pending scope differs"
        );
        if matches!(
            message.kind,
            MessageKind::TaskResult | MessageKind::TaskStatus
        ) {
            if let MessageOrigin::Task { task_id } = &message.origin {
                // Malformed origin/task pairs still reach canonical authority.
                if message.task_id.as_deref() == Some(task_id.as_str()) {
                    let observed = if message.kind == MessageKind::TaskResult {
                        if let Some(material) = selector_material.phase.as_deref_mut() {
                            task_snapshot::prepare_pending_task_result_dependency_using_material_tx(
                                tx,
                                scope,
                                &message.id,
                                task_id,
                                &relation_request,
                                material,
                            )?
                        } else {
                            task_snapshot::prepare_pending_task_result_dependency_tx(
                                tx,
                                scope,
                                &message.id,
                                task_id,
                                &relation_request,
                                budget,
                            )?
                        }
                    } else {
                        None
                    };
                    match observed {
                        Some(dependency) => {
                            selector_material.charge_key(&dependency.observation.result.key.id)?;
                            current_task_keys.insert(dependency.observation.result.key.clone());
                            if matches!(
                                &dependency.observation.relation,
                                TaskResultDependencyRelation::QualifiedCurrent { .. }
                            ) {
                                current_task_keys.insert(TaskKey::new(TaskFamily::Task, task_id));
                            }
                            result_dependencies.push(dependency);
                        }
                        // No settlement is not an Invalid relation or Task absence.
                        // Preserve the original independent current Task request.
                        None => {
                            current_task_keys.insert(TaskKey::new(TaskFamily::Task, task_id));
                        }
                    }
                }
            }
        }
    }
    if let Some(binding) = &state.current_execution_binding {
        // Metadata only. Input preparation independently validates the complete
        // original Message/body/assignment and current execution relation.
        let header =
            current_source_header_tx(tx, &binding.source_message_id, &mut selector_material)?;
        let (id, agent, kind, payload_id, payload_agent, task_id, origin_task) =
            header.context("current Task binding source is missing")?;
        ensure!(
            id == binding.source_message_id
                && agent == scope.agent_id
                && payload_id.as_deref() == Some(id.as_str())
                && payload_agent.as_deref() == Some(agent.as_str()),
            "current Task binding source normalized scope differs"
        );
        if matches!(kind.as_str(), "task_result" | "task_status") {
            if let Some(origin_task) = origin_task {
                if task_id.as_deref() == Some(origin_task.as_str()) {
                    current_task_keys.insert(TaskKey::new(TaskFamily::Task, origin_task));
                }
            }
        }
    }
    // The selected Result carries its exact predicate observation. Only a
    // QualifiedCurrent relation follows Task body; an independently bound Task
    // above remains strict regardless of the pending Result's observation.
    capture
        .required_keys
        .extend(current_task_keys.iter().cloned());
    let request = task_snapshot::TaskDomainLoadRequest {
        query: query.cloned().unwrap_or_default(),
        capture,
        record_limit,
        byte_limit,
    };
    let prepared = if let Some(material) = selector_material.phase.as_deref_mut() {
        task_snapshot::load_selected_using_material(
            tx,
            scope,
            &request,
            true,
            true,
            false,
            Some(budget),
            None,
            None,
            material,
        )?
    } else if original {
        task_snapshot::prepare_original_task_recovery_snapshot_with_budget_tx(
            tx, scope, &request, budget,
        )?
    } else {
        task_snapshot::prepare_task_domain_snapshot_with_budget_tx(tx, scope, &request, budget)?
    }
    .require_current()?;
    ensure!(
        prepared.facts.identity == facts.identity
            && prepared.facts.state == facts.state
            && prepared.facts.control_revision == facts.control_revision,
        "joint Task domains were prepared from different canonical facts"
    );
    let resident = Arc::new(prepared.resident);
    for dependency in &result_dependencies {
        ensure!(
            resident
                .coverage()
                .result_dependencies
                .get(&dependency.observation.result.key.id)
                == Some(&dependency.observation),
            "joint Result predicate differs from its original view"
        );
    }
    let (input_roots, work_roots, dependency_budget_view) = current_dependencies(
        &resident,
        current_task_keys,
        budget,
        record_limit,
        byte_limit,
        selector_material.phase.as_deref_mut(),
    )?;
    Ok(PreparedTaskDomains {
        resident,
        input_roots,
        work_roots,
        _dependency_budget_view: dependency_budget_view,
        _selector_budget_views: selector_material.guards,
        _result_dependencies: result_dependencies,
    })
}

fn current_dependencies(
    tasks: &ResidentTaskDomain,
    binding_tasks: BTreeSet<TaskKey>,
    budget: &InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
    material: Option<&mut task_snapshot::read_preparation::TaskReadMaterial<'_>>,
) -> Result<(
    BTreeSet<InputEvidenceKey>,
    BTreeSet<String>,
    Arc<BudgetViewGuard>,
)> {
    let mut request = TaskCaptureRequest::current();
    request.terminal_limit = 0;
    request.required_keys = binding_tasks.clone();
    let current = tasks.capture(&request)?;
    let mut bytes = 0usize;
    let mut records = 0usize;
    visit_current_dependencies(&current, &binding_tasks, |_, id| {
        records = records
            .checked_add(1)
            .context("Task dependency count overflow")?;
        bytes = bytes
            .checked_add(
                usize::try_from(serialized_bytes(&id)?)?
                    .checked_add(128)
                    .context("Task dependency key size overflow")?,
            )
            .context("Task dependency bytes overflow")?;
        Ok(())
    })?;
    // The returned selectors and their merged Input/Work request coexist.
    records = records
        .checked_mul(2)
        .context("Task dependency copies overflow")?;
    bytes = bytes
        .checked_mul(2)
        .context("Task dependency copies overflow")?;
    ensure!(
        records <= record_limit && bytes <= byte_limit,
        task_snapshot::TaskSnapshotError::ResourceExhausted("current dependencies")
    );
    // The grouped original recovery must debit the same remaining allowance
    // before these distinct selectors coexist with its primary Task bodies.
    // The phase's original allocation guard also follows the returned selectors.
    // It is the same allocation, not a second ledger registration.
    let guard = if let Some(material) = material {
        material.charge_ids_for_handoff(bytes, records)?
    } else {
        budget.register_owned_bytes(
            tasks.scope(),
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(bytes)?,
            },
        )?
    };
    let mut inputs = BTreeSet::new();
    let mut works = BTreeSet::new();
    visit_current_dependencies(&current, &binding_tasks, |family, id| {
        match family {
            Some(family) => {
                inputs.insert(InputEvidenceKey::new(family, id));
            }
            None => {
                works.insert(id.to_owned());
            }
        }
        Ok(())
    })?;
    Ok((inputs, works, guard))
}

fn visit_current_dependencies(
    current: &CapturedTasks,
    binding_tasks: &BTreeSet<TaskKey>,
    mut visit: impl FnMut(Option<InputEvidenceFamily>, &str) -> Result<()>,
) -> Result<()> {
    for record in &current.records {
        match &record.fact {
            CanonicalTaskFact::Task(task)
                if current.coverage.active_task_ids.contains(&task.id)
                    || binding_tasks.contains(&record.locator.key) =>
            {
                if let Some(id) = &task.parent_message_id {
                    visit(Some(InputEvidenceFamily::Message), id)?;
                }
                let metadata = current
                    .coverage
                    .task_metadata
                    .get(&task.id)
                    .context("current Task has no complete metadata")?;
                if let Some(fence) = &metadata.rejoin {
                    visit(Some(InputEvidenceFamily::Turn), &fence.parent_turn_id)?;
                }
                if let Some(id) = task.effective_work_item_id() {
                    visit(None, id)?;
                }
            }
            CanonicalTaskFact::ResultSettlement(result)
                if current
                    .coverage
                    .unsettled_result_ids
                    .contains(&result.result_identity)
                    || binding_tasks.contains(&record.locator.key) =>
            {
                visit(Some(InputEvidenceFamily::Message), &result.message_id)?;
                if matches!(
                    &current.result_dependency(&result.result_identity)?.relation,
                    TaskResultDependencyRelation::QualifiedCurrent { .. }
                ) {
                    visit(Some(InputEvidenceFamily::Turn), &result.parent_turn_id)?;
                    if let Some(id) = &result.work_item_id {
                        visit(None, id)?;
                    }
                }
                let metadata = current
                    .coverage
                    .result_metadata
                    .get(&result.result_identity)
                    .context("current Task result has no complete metadata")?;
                if let Some(id) = &metadata.reply_message_id {
                    visit(Some(InputEvidenceFamily::Message), id)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "task_recovery_tests.rs"]
mod tests;
