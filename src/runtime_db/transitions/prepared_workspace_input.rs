//! Original single-scope Input allowance before each ordinary writer phase.
use super::*;
use crate::runtime_db::input_evidence_snapshot::{
    read_material::InputReadMaterial, InputEvidenceLoadRequest, InputEvidenceSnapshotError,
};

pub(super) struct OriginalInputWriterMaterial<'a> {
    pub(super) input:InputReadMaterial<'a>,
    pub(super) current_prior:(usize,usize),
}
impl<'a> OriginalInputWriterMaterial<'a> {
    pub(super) fn prepare(
        prepared:&PreparedInputTransition, scope:&'a InputEvidenceScope,
        ledger:&'a crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        request:&crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        tasks:&crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        source:Option<&Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
    ) -> Result<Option<Self>> {
        let workspace=prepared.original_workspace_request_material();
        let sleep=prepared.original_sleep_override_material();
        let child=prepared.original_task_input_child_material();
        // Specialized/multi-scope preflight keeps its original closed kernels;
        // their earliest shared phase is a separate remaining migration.
        let original_phase=source.is_some()
            && prepared.descriptor.scopes().len()==1
            && prepared.original_child_terminal().is_none()
            && prepared.original_child_monitor_binding().is_none()
            && prepared.original_command_terminal().is_none()
            && !matches!(prepared.command.as_ref(),InputTransitionCommand::Task(task) if task.stop_intent.is_some());
        if workspace.is_none() && sleep.is_none() && child.is_none() && !original_phase {return Ok(None)}
        if workspace.is_some() {
            let original_shape=match prepared.command.as_ref() {
                InputTransitionCommand::Posture {started_turn:None,..}=>prepared.work_resource_material.is_none()
                    && prepared.memory.is_none(),
                // Runtime attaches Deferred after the closed Workspace setter.
                // Only that sealed automatic intent and one original selector
                // may borrow this same prefix; no explicit Memory is admitted.
                InputTransitionCommand::Task(_)=> {
                    let selector=prepared.work_resource_material.as_ref();
                    selector.is_none_or(|loan|loan.is_original_input_selection_only())
                        && (prepared.memory.is_none() || (prepared.auto_base_descriptor.is_some()
                            && selector.is_some_and(|loan|loan.is_original_input_selection_only())
                            && matches!(prepared.memory,Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Deferred {..}))))
                },
                _=>false,
            };
            anyhow::ensure!(original_shape
                && prepared.descriptor.scopes().len()==1,
                "Workspace material requires its original closed standalone State or Task command");
        }
        let source=source.context("first Input material requires actual original owner selectors")?;
        source.remaining_limits(scope,ledger,(request.byte_limit,request.record_limit))?;
        let child=child.map(|loan|{
            loan.validate_binding(scope,source.owner_epoch(),ledger)?;
            loan.material_estimate()
        }).transpose()?.unwrap_or((0,0));
        let workspace=workspace.map(|loan|{
            loan.validate_binding(scope,source.owner_epoch(),ledger)?;loan.material_estimate()
        }).transpose()?.unwrap_or((0,0));
        let sleep=sleep.map(|loan|{
            loan.validate_binding(scope,source.owner_epoch(),ledger)?;loan.material_estimate()
        }).transpose()?.unwrap_or((0,0));
        let resource=prepared.work_resource_material.as_ref().map(|loan|loan.validate_accounting(scope,ledger))
            .transpose()?.unwrap_or((0,0));
        let current=prepared.current_read_source.as_ref().map(|current|{
            anyhow::ensure!(current.scope()==scope && current.uses_ledger(ledger),
                "Workspace Current source differs from original scope/ledger");
            current.material_estimate()
        }).transpose()?.unwrap_or((0,0));
        let (_,roles)=prepared.current_event_request(ledger)?;
        let current_prior=(current.0.checked_add(roles.0).context("Workspace Current prior bytes overflow")?,
            current.1.checked_add(roles.1).context("Workspace Current prior nodes overflow")?);
        let prior=(workspace.0.checked_add(sleep.0).and_then(|n|n.checked_add(child.0)).and_then(|n|n.checked_add(resource.0)).and_then(|n|n.checked_add(current_prior.0)).context("first Input prior bytes overflow")?,
            workspace.1.checked_add(sleep.1).and_then(|n|n.checked_add(child.1)).and_then(|n|n.checked_add(resource.1)).and_then(|n|n.checked_add(current_prior.1)).context("first Input prior nodes overflow")?);
        // Original Task/Result/Timer receipts retain the stricter Task policy.
        // The full raw/native gate below replaces their independent receipt view.
        let task_receipt=matches!(prepared.command.as_ref(),InputTransitionCommand::Task(_)|InputTransitionCommand::TaskResult(_)|InputTransitionCommand::Timer(_));
        let limits=if task_receipt {(request.byte_limit.min(tasks.byte_limit),request.record_limit.min(tasks.record_limit))}
            else {(request.byte_limit,request.record_limit)};
        let policy=InputEvidenceLoadRequest {windows:Vec::new(),mandatory_roots:BTreeSet::new(),
            interrupted_sources:BTreeSet::new(),selected_completion_brief_ids:BTreeSet::new(),
            byte_limit:limits.0,record_limit:limits.1};
        // Host-owned request guard stays in its dedicated original loan; it
        // cannot be relabeled or inserted into agent-scoped Input guards.
        let input=InputReadMaterial::new_with_memory_and_query_prior(scope,&policy,ledger,prior,None,
            Some((source.roles(),source.owner_epoch())))?;
        Ok(Some(Self {input,current_prior}))
    }
    pub(super) fn current_limits(&self,
        memory:Option<&crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<(usize,usize)> {
        let mut remaining=self.input.remaining_material_limits()?;
        if let Some(memory)=memory {
            memory.original_input_phase_usage(&self.input)?;
            let actual=memory.remaining_material_limits()?;
            remaining.0=remaining.0.min(actual.0);remaining.1=remaining.1.min(actual.1);
        }
        Ok((remaining.0.checked_add(self.current_prior.0)
                .context(InputEvidenceSnapshotError::ResourceExhausted("Workspace Current remaining"))?,
            remaining.1.checked_add(self.current_prior.1)
                .context(InputEvidenceSnapshotError::ResourceExhausted("Workspace Current remaining"))?))
    }
}

/// New request copies and later decoders borrow this same live allowance.
/// The actual family proof owns each body guard; Input only borrows its usage.
pub(super) fn task_request(
    material:&mut InputReadMaterial<'_>,
    original:&crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
) -> Result<crate::runtime_db::task_snapshot::TaskDomainLoadRequest> {
    request_copy(material,&(&original.query,original.capture.include_active,original.capture.include_unsettled,
        original.capture.terminal_limit,&original.capture.required_keys))?;
    let remaining=material.remaining_material_limits()?;
    let mut request=original.clone();request.byte_limit=request.byte_limit.min(remaining.0);
    request.record_limit=request.record_limit.min(remaining.1);Ok(request)
}
pub(super) fn work_request(
    material:&mut InputReadMaterial<'_>, scope:&InputEvidenceScope,
    original:&crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
    admitted:(usize,usize),
) -> Result<crate::runtime_db::work_snapshot::WorkDomainLoadRequest> {
    request_copy(material,&(scope,original.capture.include_live,original.capture.nonopen_limit,
        &original.capture.mandatory_roots,&original.capture.latest_result_work_ids,&original.capture.required_turn_ids,
        &original.capture.required_task_ids,&original.capture.required_trigger_message_ids))?;
    let remaining=material.remaining_material_limits()?;
    let mut request=original.clone();
    request.byte_limit=request.byte_limit.min(remaining.0.checked_add(admitted.0).context("Work original allowance overflow")?);
    request.record_limit=request.record_limit.min(remaining.1.checked_add(admitted.1).context("Work original nodes overflow")?);
    Ok(request)
}
pub(super) fn scheduling_request(
    material:&mut InputReadMaterial<'_>,
    original:&crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest,
    admitted:(usize,usize),
) -> Result<crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest> {
    request_copy(material,&(original.capture.include_live,original.capture.history_limit,&original.capture.required_keys))?;
    let remaining=material.remaining_material_limits()?;
    let mut request=original.clone();
    request.byte_limit=request.byte_limit.min(remaining.0.checked_add(admitted.0).context("Scheduling original allowance overflow")?);
    request.record_limit=request.record_limit.min(remaining.1.checked_add(admitted.1).context("Scheduling original nodes overflow")?);
    Ok(request)
}
fn request_copy<T:serde::Serialize>(material:&mut InputReadMaterial<'_>,value:&T)->Result<()> {
    let (encoded,nodes)=crate::runtime::input_evidence_budget::borrowed_json_material(value)?;
    let nodes=nodes.checked_add(16).and_then(|n|n.checked_mul(8)).context("original request copy nodes overflow")?;
    let bytes=encoded.checked_mul(8)
        .and_then(|n|u64::try_from(nodes).ok().and_then(|m|m.checked_mul(256)).and_then(|m|n.checked_add(m)))
        .context("original request copy bytes overflow")?;
    material.observe_projected(bytes,nodes)
}
pub(super) fn increment(actual:(usize,usize),admitted:(usize,usize))->Result<(usize,usize)> {
    Ok((actual.0.checked_sub(admitted.0).context("original writer bytes retired outside its phase")?,
        actual.1.checked_sub(admitted.1).context("original writer nodes retired outside its phase")?))
}
