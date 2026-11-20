//! A same-transaction read facet for one explicit full/historical Task output.
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime::resident_input_evidence::{
    CapturedInputEvidence, EvidenceLocator, InputEvidenceFamily as Family, InputEvidenceKey as Key,
    ResidentInputEvidence,
};
use crate::runtime_db::input_evidence_snapshot::{
    InputEvidenceLoadRequest, TaskOutputInputRecall, TaskOutputRecallMaterial,
};
use crate::types::{TaskKind, TaskStatus};

/// Neither raw Task/Source DTOs nor a query lease can construct this facet.
/// It grants only strict selected Input reads; it never mints D3 aliases.
pub(in crate::runtime_db) struct TaskOutputReadAuthorization<'a, 'read, 'tx, 'conn> {
    read: &'read mut super::qualified_input_read::TaskInputReadAuthorization<'a, 'tx, 'conn>,
    task: Option<&'a TaskFactLocator>,
    roots: BTreeSet<Key>,
    sources: Vec<&'a EvidenceLocator>,
}

/// One explicit final-product read. The union is private to these exact Task
/// facets and never enters the generic D3 domain or grants writer authority.
pub(crate) struct TaskOutputInputBatch {
    domain: ResidentInputEvidence,
    facets: BTreeMap<TaskKey, (TaskFactLocator, BTreeSet<Key>)>,
    material: TaskOutputRecallMaterial,
    record_limit: usize,
    byte_limit: usize,
}

impl TaskOutputInputBatch {
    pub(crate) fn admitted_material(&self) -> (usize, usize) {
        self.material.admitted_material()
    }

    pub(crate) fn into_material(self) -> Arc<TaskOutputRecallMaterial> {
        Arc::new(self.material)
    }

    pub(crate) fn capture(
        &mut self,
        task: &TaskFactLocator,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<CapturedInputEvidence> {
        let (accepted, roots) = self
            .facets
            .get(&task.key)
            .context("Output batch Task not selected")?;
        ensure!(accepted == task, "Output batch Task version differs");
        self.material.validate_ledger(ledger)?;
        let (bytes, records) = self.domain.task_output_recall_copy_material(0)?;
        self.material.admit_copy(ledger, bytes, records)?;
        self.domain.capture_for_request(&InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: roots.clone(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: self.record_limit,
            byte_limit: self.byte_limit,
        })
    }
}

/// Every target and Source comes from one captured Task publication. The shared
/// scope, closure and material phase are read once in this original transaction.
pub(crate) fn prepare_task_output_input_batch_tx(
    tx: &Transaction<'_>,
    expected_state: &AgentState,
    tasks: &CapturedTasks,
    ids: &[&str],
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<TaskOutputInputBatch>> {
    let Some(first) = ids.first() else {
        return Ok(None);
    };
    let first_key = TaskKey::new(TaskFamily::Task, *first);
    let first_task = tasks
        .lookup(&first_key)?
        .context("Output batch Task absent")?;
    let mut read = super::qualified_input_read::prepare_task_input_read_base_tx(
        tx,
        &tasks.scope,
        expected_state,
        tasks.revision,
        &first_key,
        Some(first_task),
        tasks.output_source(first)?,
        request,
        ledger,
    )?;
    read.material.charge_ids(
        read_preparation::encoded_metadata_bytes(&(&tasks.scope, ids))?
            .checked_add(
                ids.len()
                    .checked_mul(512)
                    .context("Output batch selectors overflow")?,
            )
            .context("Output batch selectors overflow")?,
        ids.len()
            .checked_mul(8)
            .context("Output batch count overflow")?,
    )?;
    for id in ids {
        let task = tasks
            .lookup(&TaskKey::new(TaskFamily::Task, *id))?
            .context("Output batch Task absent")?;
        let CanonicalTaskFact::Task(body) = &task.fact else {
            anyhow::bail!("Output batch target is not a Task");
        };
        let reply = (body.kind == TaskKind::AgentMessageWait
            && body.status == TaskStatus::Completed)
            .then(|| {
                body.detail
                    .as_ref()
                    .and_then(|d| d.get("message_id"))
                    .and_then(serde_json::Value::as_str)
            })
            .flatten();
        read.material.charge_ids(
            read_preparation::encoded_metadata_bytes(&(
                &task.locator,
                tasks.output_source(id)?,
                reply,
            ))?
            .checked_mul(4)
            .context("Output batch facet material overflow")?,
            8,
        )?;
    }
    let mut locators = Vec::with_capacity(ids.len());
    let mut facets = BTreeMap::new();
    let mut roots = BTreeSet::new();
    let mut sources = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let key = TaskKey::new(TaskFamily::Task, *id);
        let task = tasks.lookup(&key)?.context("Output batch Task absent")?;
        if index != 0 {
            read.validate_target(tx, &key, Some(task))?;
        }
        let mut selected = BTreeSet::new();
        if let Some(source) = tasks.output_source(id)? {
            ensure!(
                source.availability == TaskOutputSourceAvailability::Present,
                "Task output Source is unavailable"
            );
            selected.insert(source.message.key.clone());
            sources.push(&source.message);
        }
        let CanonicalTaskFact::Task(body) = &task.fact else {
            anyhow::bail!("Output batch target is not a Task");
        };
        if body.kind == TaskKind::AgentMessageWait && body.status == TaskStatus::Completed {
            selected.extend(
                body.detail
                    .as_ref()
                    .and_then(|d| d.get("message_id"))
                    .and_then(serde_json::Value::as_str)
                    .map(|id| Key::new(Family::Message, id)),
            );
        }
        roots.extend(selected.iter().cloned());
        facets.insert(key, (task.locator.clone(), selected));
        locators.push(&task.locator);
    }
    let actual =
        query_sources::observe_output_heads_tx(tx, &tasks.scope, &locators, |bytes, count| {
            read.material.charge_ids(bytes, count)
        })?;
    for id in ids {
        ensure!(
            actual.heads.get(*id).and_then(|head| head.source.as_ref())
                == tasks.output_source(id)?,
            "Task output Source advanced from its original observation"
        );
    }
    let mut authorization = TaskOutputReadAuthorization {
        read: &mut read,
        task: None,
        roots,
        sources,
    };
    let domain =
        crate::runtime_db::input_evidence_snapshot::prepare_authorized_task_output_domain_tx(
            tx,
            &mut authorization,
            request.record_limit,
            request.byte_limit,
        )?;
    drop(authorization);
    let admitted = read.material.admitted_material();
    let material = TaskOutputRecallMaterial::new(
        read.material.into_guards(),
        ledger.clone(),
        admitted.0,
        admitted.1,
        request.byte_limit.min(MAX_BYTES),
        request.record_limit.min(MAX_RECORDS),
    )?;
    Ok(Some(TaskOutputInputBatch {
        domain: domain.with_budget(ledger.clone())?,
        facets,
        material,
        record_limit: request.record_limit,
        byte_limit: request.byte_limit,
    }))
}
impl<'a, 'tx, 'conn> TaskOutputReadAuthorization<'a, '_, 'tx, 'conn> {
    pub(in crate::runtime_db) fn read(
        &mut self,
    ) -> &mut super::qualified_input_read::TaskInputReadAuthorization<'a, 'tx, 'conn> {
        self.read
    }
    pub(in crate::runtime_db) fn scope(&self) -> &InputEvidenceScope {
        self.read.scope()
    }
    pub(in crate::runtime_db) fn task(&self) -> Option<&TaskFactLocator> {
        self.task
    }
    pub(in crate::runtime_db) fn roots(&self) -> &BTreeSet<Key> {
        &self.roots
    }
    pub(in crate::runtime_db) fn sources<'b>(
        &'b self,
        key: &'b Key,
    ) -> impl Iterator<Item = &'b EvidenceLocator> {
        self.sources
            .iter()
            .copied()
            .filter(|source| source.key == *key)
    }
}

/// Called with the held owner and the exact captured Task + Source observation.
/// Every decoded canonical/header/manifest allocation is admitted first.
pub(crate) fn prepare_task_output_input_recall_tx(
    tx: &Transaction<'_>,
    expected_state: &AgentState,
    revision: u64,
    task: &QualifiedTaskFact,
    source: Option<&TaskOutputSourceHead>,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<TaskOutputInputRecall> {
    let scope = &task.locator.scope;
    ensure!(
        task.locator.key.family == TaskFamily::Task && expected_state.id == scope.agent_id,
        "Task output original scope differs"
    );
    let mut read = super::qualified_input_read::prepare_task_input_read_base_tx(
        tx,
        scope,
        expected_state,
        revision,
        &task.locator.key,
        Some(task),
        source,
        request,
        ledger,
    )?;
    let actual_output =
        query_sources::observe_output_heads_tx(tx, scope, &[&task.locator], |bytes, count| {
            read.material.charge_ids(bytes, count)
        })?;
    ensure!(
        actual_output
            .heads
            .get(&task.locator.key.id)
            .and_then(|head| head.source.as_ref())
            == source,
        "Task output Source advanced from its original observation"
    );
    if let Some(source) = source {
        ensure!(
            source.availability == TaskOutputSourceAvailability::Present,
            "Task output Source is unavailable"
        );
    }
    let roots_bytes = read_preparation::encoded_metadata_bytes(&(
        source,
        &task.fact,
        read.fixed_inventory.as_deref(),
    ))?;
    read.material.charge_ids(roots_bytes, 8)?;
    let mut roots = BTreeSet::new();
    roots.extend(source.map(|head| head.message.key.clone()));
    let CanonicalTaskFact::Task(current) = &task.fact else {
        unreachable!()
    };
    if current.kind == TaskKind::AgentMessageWait && current.status == TaskStatus::Completed {
        roots.extend(
            current
                .detail
                .as_ref()
                .and_then(|detail| detail.get("message_id"))
                .and_then(serde_json::Value::as_str)
                .map(|id| Key::new(Family::Message, id)),
        );
    }
    let mut authorization = TaskOutputReadAuthorization {
        read: &mut read,
        task: Some(&task.locator),
        roots,
        sources: source.map(|head| &head.message).into_iter().collect(),
    };
    let recall = super::super::input_evidence_snapshot::prepare_authorized_task_output_inputs_tx(
        tx,
        &mut authorization,
        request.record_limit,
        request.byte_limit,
    )?;
    drop(authorization);
    let (bytes, records) = read.material.admitted_material();
    recall.with_material(
        read.material.into_guards(),
        ledger.clone(),
        bytes,
        records,
        request.byte_limit.min(MAX_BYTES),
        request.record_limit.min(MAX_RECORDS),
    )
}
