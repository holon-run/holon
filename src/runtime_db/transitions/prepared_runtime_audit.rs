//! Original Runtime Audits carry no State mutation; exceptional modes are sealed.
use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadKey, CurrentReadRequest},
    input_evidence_budget::{
        borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
};
use crate::runtime_db::current_read_snapshot::CurrentReadBusinessTransaction;

#[derive(Debug, serde::Serialize)]
pub(super) struct PreparedRuntimeAuditCommand {
    agent_id: String,
    event: AuditEvent,
    // None preserves the original single-Event command and receipt digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduler_legacy_event: Option<AuditEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_shutdown_abort: Option<AuditEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_sleep_override_state: Option<AuditEvent>,
    #[serde(skip)]
    original_sleep_material: Option<Arc<crate::runtime::OriginalSleepOverrideMaterial>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_lifecycle_noop: Option<crate::runtime::OriginalLifecycleNoopKind>,
    #[serde(skip)]
    original_lifecycle_source: Option<crate::runtime::OriginalLifecycleNoopAuditSource>,
    // An original held-body OCC witness, never business content or Ready authority.
    #[serde(skip)]
    expected: Arc<AgentState>,
    #[serde(skip)]
    fault: Option<TransitionFaultPoint>,
}
impl PreparedRuntimeAuditCommand {
    pub(super) fn agent_id(&self) -> &str {
        &self.agent_id
    }
    pub(super) fn events(&self) -> impl Iterator<Item = &AuditEvent> {
        std::iter::once(&self.event)
            .chain(self.scheduler_legacy_event.as_ref())
            .chain(self.original_shutdown_abort.as_ref())
            .chain(self.original_sleep_override_state.as_ref())
    }
    pub(super) fn validate(&self, scope: &InputEvidenceScope) -> Result<()> {
        anyhow::ensure!(
            self.agent_id == scope.agent_id && self.expected.id == scope.agent_id,
            "Runtime Audit differs from original held State/scope"
        );
        match (
            &self.original_lifecycle_noop,
            &self.original_lifecycle_source,
        ) {
            (None, None) => {
                anyhow::ensure!(
                    self.original_shutdown_abort.is_none(),
                    "shutdown Audit requires its original sealed source"
                );
            }
            (Some(kind), Some(source)) => {
                anyhow::ensure!(
                    *kind == source.kind() && self.scheduler_legacy_event.is_none(),
                    "Lifecycle noop mode differs from its original source"
                );
                source.validate_event(scope, &self.event)?;
                source.validate_shutdown_companion(
                    &self.event,
                    self.original_shutdown_abort.as_ref(),
                )?;
            }
            _ => anyhow::bail!("Lifecycle noop Audit has no sealed original source"),
        }
        match (
            &self.original_sleep_override_state,
            &self.original_sleep_material,
        ) {
            (None, None) => {}
            (Some(state), Some(_)) => anyhow::ensure!(
                self.original_lifecycle_source.is_none()
                    && self.scheduler_legacy_event.is_none()
                    && self.original_shutdown_abort.is_none()
                    && self.event.kind == "scheduler_posture_decision"
                    && state.kind == "agent_state_changed"
                    && self.event.id != state.id
                    && self.event.data["boundary"] == "lifecycle_sleep"
                    && self.event.data["reason"] == "sleep_overridden_runnable_work",
                "Sleep override Audit must retain its original posture/state pair"
            ),
            _ => anyhow::bail!("Sleep override observation has no original material"),
        }
        if let Some(legacy) = &self.scheduler_legacy_event {
            anyhow::ensure!(
                self.event.kind == "scheduler_diagnostic"
                    && legacy.kind == "scheduler_decision"
                    && self.event.id != legacy.id,
                "Scheduler Audit must keep its original ordered diagnostic/legacy pair"
            );
        }
        Ok(())
    }
}
impl PreparedInputTransition {
    pub(crate) fn runtime_audit(
        scope: InputEvidenceScope,
        nonce: String,
        event: AuditEvent,
        expected: Arc<AgentState>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let at = event.created_at;
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: None,
            original_lifecycle_source: None,
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}
impl PreparedInputTransition {
    pub(crate) fn runtime_scheduler_audit(
        scope: InputEvidenceScope,
        nonce: String,
        events: [AuditEvent; 2],
        expected: Arc<AgentState>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let [diagnostic, legacy] = events;
        let at = diagnostic.created_at;
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event: diagnostic,
            scheduler_legacy_event: Some(legacy),
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: None,
            original_lifecycle_source: None,
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}
impl PreparedInputTransition {
    pub(crate) fn runtime_lifecycle_noop_audit(
        scope: InputEvidenceScope,
        event: AuditEvent,
        expected: Arc<AgentState>,
        source: crate::runtime::OriginalLifecycleNoopAuditSource,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.kind().is_model(),
            "Model noop factory cannot change another original Lifecycle family"
        );
        let at = event.created_at;
        let nonce = format!("runtime-lifecycle-noop:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: Some(source.kind()),
            original_lifecycle_source: Some(source),
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}
impl PreparedInputTransition {
    pub(crate) fn runtime_shutdown_audits(
        scope: InputEvidenceScope,
        event: AuditEvent,
        aborted: Option<AuditEvent>,
        expected: Arc<AgentState>,
        source: crate::runtime::OriginalLifecycleNoopAuditSource,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.kind() == crate::runtime::OriginalLifecycleNoopKind::RuntimeShutdown,
            "shutdown Audit requires its original closed source"
        );
        let at = event.created_at;
        let nonce = format!("runtime-shutdown-audit:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: aborted,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: Some(source.kind()),
            original_lifecycle_source: Some(source),
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}
impl PreparedInputTransition {
    pub(crate) fn runtime_operator_abort_audit(
        scope: InputEvidenceScope,
        event: AuditEvent,
        expected: Arc<AgentState>,
        source: crate::runtime::OriginalLifecycleNoopAuditSource,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.kind().is_operator_abort(),
            "operator abort Audit requires its original closed source"
        );
        let at = event.created_at;
        let nonce = format!("runtime-operator-abort-audit:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: Some(source.kind()),
            original_lifecycle_source: Some(source),
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}

impl PreparedInputTransition {
    pub(crate) fn runtime_original_workspace_audit(
        scope: InputEvidenceScope,
        event: AuditEvent,
        expected: Arc<AgentState>,
        source: crate::runtime::OriginalLifecycleNoopAuditSource,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.kind().is_workspace(),
            "Workspace Audit requires its original closed source"
        );
        let at = event.created_at;
        let nonce = format!("runtime-original-workspace-audit:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: Some(source.kind()),
            original_lifecycle_source: Some(source),
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}

impl PreparedInputTransition {
    pub(crate) fn runtime_sleep_override_audits(
        scope: InputEvidenceScope,
        events: [AuditEvent; 2],
        expected: Arc<AgentState>,
        material: Arc<crate::runtime::OriginalSleepOverrideMaterial>,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        material.validate_binding(&scope, epoch, ledger)?;
        let [event, state] = events;
        let at = event.created_at;
        let nonce = format!("runtime-sleep-override:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: Some(state),
            original_sleep_material: Some(material),
            original_lifecycle_noop: None,
            original_lifecycle_source: None,
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}

struct AuditMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    request: &'a CurrentReadRequest,
    usage: (usize, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl AuditMaterial<'_> {
    fn observe(&mut self, bytes: usize, records: usize) -> Result<()> {
        let bytes = bytes
            .checked_add(std::mem::size_of::<BudgetViewGuard>() + 512)
            .context("Audit guard bytes overflow")?;
        let next = (
            self.usage
                .0
                .checked_add(bytes)
                .context("Audit phase bytes overflow")?,
            self.usage
                .1
                .checked_add(records)
                .and_then(|n| n.checked_add(4))
                .context("Audit phase records overflow")?,
        );
        self.request.check_material(next.0, next.1)?;
        let guard = self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(bytes)?,
            },
        )?;
        self.guards.push(guard);
        self.usage = next;
        Ok(())
    }
}
pub(super) fn commit_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    command: &PreparedRuntimeAuditCommand,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let scope = &prepared.primary_scope;
    command.validate(scope)?;
    anyhow::ensure!(
        prepared.descriptor.scopes() == std::slice::from_ref(scope)
            && prepared.memory.is_none()
            && prepared.work_resource_material.is_none(),
        "Runtime Audit has a different original business footprint"
    );
    let (request, role_prior) = prepared.current_event_request(ledger)?;
    let source_prior = command
        .original_lifecycle_source
        .as_ref()
        .map(|source| source.material_prior(scope, ledger, &command.event))
        .transpose()?
        .unwrap_or((0, 0));
    let sleep_prior = command
        .original_sleep_material
        .as_ref()
        .map(|loan| {
            loan.validate_binding(scope, loan.owner_epoch(), ledger)?;
            loan.material_estimate()
        })
        .transpose()?
        .unwrap_or((0, 0));
    let prior = (
        role_prior
            .0
            .checked_add(source_prior.0)
            .and_then(|n| n.checked_add(sleep_prior.0))
            .context("Lifecycle Audit combined source bytes overflow")?,
        role_prior
            .1
            .checked_add(source_prior.1)
            .and_then(|n| n.checked_add(sleep_prior.1))
            .context("Lifecycle Audit combined source records overflow")?,
    );
    request.check_material(prior.0, prior.1)?;
    let mut material = AuditMaterial {
        scope,
        ledger,
        request: &request,
        usage: prior,
        guards: Vec::new(),
    };
    // Frozen original Event and held State are simultaneously alive. Borrowed
    // native sizing precedes the key/receipt containers and any SQL body decode.
    let (encoded, nodes) = borrowed_json_material(&(
        scope,
        &command.event,
        &command.scheduler_legacy_event,
        command.expected.as_ref(),
    ))?;
    let bytes = usize::try_from(encoded)?
        .checked_add(
            nodes
                .checked_mul(256)
                .context("Audit original native bytes overflow")?,
        )
        .and_then(|n| n.checked_add(std::mem::size_of::<PreparedRuntimeAuditCommand>() + 1024))
        .context("Audit original material overflow")?;
    material.observe(
        bytes,
        nodes
            .checked_add(32)
            .context("Audit original nodes overflow")?,
    )?;
    if let Some(state) = command.original_sleep_override_state.as_ref() {
        let (encoded, nodes) = borrowed_json_material(state)?;
        let bytes = usize::try_from(encoded)?
            .checked_add(
                nodes
                    .checked_mul(256)
                    .context("Sleep observation native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(512))
            .context("Sleep observation bytes overflow")?;
        material.observe(
            bytes,
            nodes
                .checked_add(16)
                .context("Sleep observation nodes overflow")?,
        )?;
    }
    if let Some(aborted) = command.original_shutdown_abort.as_ref() {
        let (encoded, nodes) = borrowed_json_material(aborted)?;
        let bytes = usize::try_from(encoded)?
            .checked_add(
                nodes
                    .checked_mul(256)
                    .context("shutdown companion native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(512))
            .context("shutdown companion bytes overflow")?;
        material.observe(
            bytes,
            nodes
                .checked_add(16)
                .context("shutdown companion records overflow")?,
        )?;
    }
    // Full original ten-column receipt: raw guard first, then complete JSON
    // native trees, then the original decoder. No Task/result error disguise.
    let receipt_bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(family AS BLOB))+length(CAST(phase_version AS BLOB))+length(CAST(command_digest AS BLOB))+length(CAST(scope_digest AS BLOB))+length(CAST(scopes_json AS BLOB))+length(CAST(prepared_at AS BLOB))+length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB))+length(CAST(committed_at AS BLOB))+length(CAST(result_digest AS BLOB)) FROM input_mutation_receipts WHERE command_nonce=?1),0)",[prepared.descriptor.command_nonce()],|r|r.get(0))?;
    material.observe(
        receipt_bytes
            .checked_mul(8)
            .context("Audit receipt raw overflow")?,
        16,
    )?;
    let receipt_nodes:usize=tx.query_row("SELECT COALESCE((SELECT (SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(scopes_json))+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(prepared_json))+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(outcome_json)) FROM input_mutation_receipts WHERE command_nonce=?1),0)",[prepared.descriptor.command_nonce()],|r|r.get(0))?;
    let receipt_nodes = receipt_nodes
        .checked_mul(2)
        .context("Audit receipt native count overflow")?;
    material.observe(
        receipt_nodes
            .checked_mul(256)
            .context("Audit receipt native bytes overflow")?,
        receipt_nodes,
    )?;
    if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
        return Ok(InputMutationExecution::Replayed {
            receipt,
            _material_budget_views: material.guards,
        });
    }
    let keys = command
        .events()
        .map(|event| CurrentReadKey::event(&event.id))
        .collect::<BTreeSet<_>>();
    let mut current = if command
        .original_lifecycle_source
        .as_ref()
        .is_some_and(|source| !source.kind().requires_active_observation_scope())
    {
        // Only the sealed original StateSome noop branch preserves Posture's
        // existing same-incarnation Deleting authority. No new NoState path.
        CurrentReadBusinessTransaction::prepare_tx(
            tx,
            scope,
            &command.expected,
            &keys,
            &request,
            ledger,
            material.usage,
            material.guards,
        )?
    } else {
        CurrentReadBusinessTransaction::prepare_active_audit_tx(
            tx,
            scope,
            &command.expected,
            &keys,
            &request,
            ledger,
            material.usage,
            material.guards,
        )?
    };
    inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
    let mut events = Vec::new();
    for event in command.events() {
        let (actual, changed) = current.append_event_tx(tx, Some(&scope.agent_id), event)?;
        if changed {
            events.push(actual);
        }
    }
    let changed = !events.is_empty();
    inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;
    inject_fault(command.fault, TransitionFaultPoint::AfterAuditWrites)?;
    let facts = current.finish(&command.expected)?;
    let effects = PostCommitEffects {
        _original_sleep_override_material: command.original_sleep_material.clone(),
        audit_events: events,
        current_read_facts: Some(Arc::new(facts)),
        fault: command.fault.filter(|point| point.is_post_commit()),
        ..Default::default()
    };
    let install_keys =
        crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
            scope, &effects,
        )?;
    let outcome = InputMutationOutcome {
        result: InputMutationResult::Transition,
        applied: changed,
        delivery_receipt: None,
        scopes: vec![InputMutationScopeOutcome {
            scope: scope.clone(),
            install_keys,
            audit_events: effects.audit_events.clone(),
            notify_memory_index: false,
            notify_scheduler: false,
        }],
    };
    let receipt = insert_input_mutation_receipt_tx(
        tx,
        &prepared.descriptor,
        &outcome,
        prepared.descriptor.prepared_at(),
    )?;
    inject_fault(command.fault, TransitionFaultPoint::BeforeCommit)?;
    Ok(InputMutationExecution::Committed {
        value: TransitionCommit {
            applied: changed,
            effects: Box::new(effects),
            ..Default::default()
        },
        receipt,
    })
}

impl PreparedInputTransition {
    pub(crate) fn runtime_loop_failure_audit(
        scope: InputEvidenceScope,
        event: AuditEvent,
        expected: Arc<AgentState>,
        source: crate::runtime::OriginalLifecycleNoopAuditSource,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.kind() == crate::runtime::OriginalLifecycleNoopKind::RuntimeLoopFailure,
            "Runtime failure Audit requires its original closed source"
        );
        let at = event.created_at;
        let nonce = format!("runtime-loop-failure-audit:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: Some(source.kind()),
            original_lifecycle_source: Some(source),
            expected,
            fault: None,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}

impl PreparedInputTransition {
    pub(crate) fn runtime_task_input_audit(
        scope: InputEvidenceScope,
        event: AuditEvent,
        expected: Arc<AgentState>,
        source: crate::runtime::OriginalLifecycleNoopAuditSource,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.kind().is_task_input(),
            "TaskInput Audit requires its original successful delivery source"
        );
        let at = event.created_at;
        let nonce = format!("runtime-task-input-audit:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: Some(source.kind()),
            original_lifecycle_source: Some(source),
            expected,
            fault: None,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}

impl PreparedInputTransition {
    pub(crate) fn runtime_observation_audit(
        scope: InputEvidenceScope,
        event: AuditEvent,
        expected: Arc<AgentState>,
        source: crate::runtime::OriginalLifecycleNoopAuditSource,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.kind().is_runtime_observation(),
            "Runtime observation Audit requires its original closed family"
        );
        let at = event.created_at;
        let nonce = format!("runtime-observation-audit:{}", event.id);
        let command = PreparedRuntimeAuditCommand {
            agent_id: scope.agent_id.clone(),
            event,
            scheduler_legacy_event: None,
            original_shutdown_abort: None,
            original_sleep_override_state: None,
            original_sleep_material: None,
            original_lifecycle_noop: Some(source.kind()),
            original_lifecycle_source: Some(source),
            expected,
            fault,
        };
        command.validate(&scope)?;
        Self::prepare(
            scope,
            nonce,
            at,
            InputTransitionCommand::RuntimeAudit(command),
        )
    }
}
