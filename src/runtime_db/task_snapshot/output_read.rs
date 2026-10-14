//! A same-transaction read facet for one explicit full/historical Task output.
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime::resident_input_evidence::{
    EvidenceLocator, InputEvidenceFamily as Family, InputEvidenceKey as Key,
};
use crate::runtime_db::input_evidence_snapshot::TaskOutputInputRecall;
use crate::types::{TaskKind, TaskStatus};

/// Neither raw Task/Source DTOs nor a query lease can construct this facet.
/// It grants only strict selected Input reads; it never mints D3 aliases.
pub(in crate::runtime_db) struct TaskOutputReadAuthorization<'a, 'tx, 'conn> {
    read: super::qualified_input_read::TaskInputReadAuthorization<'a, 'tx, 'conn>,
    task: &'a TaskFactLocator,
    roots: BTreeSet<Key>,
    source: Option<&'a EvidenceLocator>,
}
impl<'a, 'tx, 'conn> TaskOutputReadAuthorization<'a, 'tx, 'conn> {
    pub(in crate::runtime_db) fn read(
        &mut self,
    ) -> &mut super::qualified_input_read::TaskInputReadAuthorization<'a, 'tx, 'conn> {
        &mut self.read
    }
    pub(in crate::runtime_db) fn scope(&self) -> &InputEvidenceScope {
        self.read.scope()
    }
    pub(in crate::runtime_db) fn task(&self) -> &TaskFactLocator {
        self.task
    }
    pub(in crate::runtime_db) fn roots(&self) -> &BTreeSet<Key> {
        &self.roots
    }
    pub(in crate::runtime_db) fn source(&self) -> Option<&EvidenceLocator> {
        self.source
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
        read,
        task: &task.locator,
        roots,
        source: source.map(|head| &head.message),
    };
    let recall = super::super::input_evidence_snapshot::prepare_authorized_task_output_inputs_tx(
        tx,
        &mut authorization,
        request.record_limit,
        request.byte_limit,
    )?;
    let (bytes, records) = authorization.read.material.admitted_material();
    recall.with_material(
        authorization.read.material.into_guards(),
        ledger.clone(),
        bytes,
        records,
        request.byte_limit.min(MAX_BYTES),
        request.record_limit.min(MAX_RECORDS),
    )
}
