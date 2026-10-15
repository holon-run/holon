//! Original-transaction binding for actual-dispatch memory only.
use super::*;
use crate::{
    memory::{
        episode_transition::{plan_episode_memory, EpisodeMemoryPlanInput},
        working::{
            derive_working_memory_snapshot_from_observations, refresh_working_memory_from_snapshot,
        },
    },
    runtime::{
        resident_work::{CanonicalWorkRecord, CapturedWork, WorkFamily},
        runtime_error_active, RuntimeHandle, SchedulerAgentSnapshot, SchedulerObservations,
        SchedulerProjection,
    },
    runtime_db::{
        episode_memory::{EpisodeMemoryCommit, EpisodeMemoryTransitionCommand},
        transitions::TransitionCommit,
        work_snapshot::WorkDomainLoadRequest,
    },
    types::{AuditEvent, WaitConditionRecord, WaitConditionStatus},
};

/// Addresses a seed, never a latest State or a caller-invented Turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchMemoryBinding {
    key: EpisodeMemoryKey,
    invocation: EpisodeMemoryInvocation,
    seed_digest: String,
}
impl DispatchMemoryBinding {
    #[cfg(test)]
    pub(crate) fn from_start(start: &DispatchMemoryStart) -> Self {
        Self {
            key: start.key.clone(),
            invocation: start.invocation.clone(),
            seed_digest: start.digest.clone(),
        }
    }
    pub(crate) fn scope(&self) -> InputEvidenceScope {
        scope(&self.key)
    }
    pub(super) fn validate(&self) -> Result<()> {
        validate_key(&self.key, &self.invocation)?;
        ensure!(
            valid_digest(&self.seed_digest),
            "invalid dispatch seed binding"
        );
        Ok(())
    }
    pub(crate) fn key(&self) -> &EpisodeMemoryKey {
        &self.key
    }
    pub(crate) fn invocation(&self) -> &EpisodeMemoryInvocation {
        &self.invocation
    }
    pub(super) fn seed_digest(&self) -> &str {
        &self.seed_digest
    }
    fn matches(&self, record: &DispatchMemoryRecord) -> Result<()> {
        self.validate()?;
        ensure!(
            record.key() == &self.key
                && record.invocation() == &self.invocation
                && record.seed_digest() == self.seed_digest,
            DispatchMemoryError::StartConflict
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchMemoryCommandPolicy {
    None,
    Deferred,
    Finish,
}

/// Explicit command intent. None at the enclosing command preserves legacy
/// behavior; there is no automatic checkpoint on an arbitrary Work write.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DispatchMemoryIntent {
    Started {
        start: Box<DispatchMemoryStart>,
    },
    Deferred {
        binding: DispatchMemoryBinding,
        frozen: FrozenEpisodeMemoryValues,
    },
    Finish {
        binding: DispatchMemoryBinding,
        frozen: FrozenEpisodeMemoryValues,
    },
}
impl DispatchMemoryIntent {
    pub(crate) fn binding_scope(&self) -> InputEvidenceScope {
        match self {
            Self::Started { start } => scope(&start.key),
            Self::Deferred { binding, .. } | Self::Finish { binding, .. } => binding.scope(),
        }
    }
    pub(crate) fn input_roots(&self) -> BTreeSet<InputEvidenceKey> {
        match self {
            Self::Started { start } => start.input_roots(),
            Self::Deferred { binding, .. } | Self::Finish { binding, .. } => BTreeSet::from([
                InputEvidenceKey::new(InputEvidenceFamily::Message, &binding.invocation.message_id),
                InputEvidenceKey::new(
                    InputEvidenceFamily::Turn,
                    &binding.key.actual_dispatch_turn_id,
                ),
            ]),
        }
    }
}

/// Minted before business SQL. The original transaction's own State OCC is
/// still enforced by its reducer. This stores that read's exact memory/phase,
/// never a mutable current State to be installed after a later dispatch.
pub(crate) struct ValidatedDispatchMemoryFinish<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    record: DispatchMemoryRecord,
    expected_state: Box<AgentState>,
    frozen: FrozenEpisodeMemoryValues,
}
impl<'tx, 'connection> ValidatedDispatchMemoryFinish<'tx, 'connection> {
    pub(in crate::runtime_db) fn original_audit_id_tx(&self, tx: &Transaction<'_>) -> Result<&str> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "memory audit footprint belongs to another transaction"
        );
        Ok(&self.frozen.audit_event_id)
    }
    pub(in crate::runtime_db) fn embedded_inputs(
        &self,
    ) -> (
        &Transaction<'connection>,
        &EpisodeMemoryKey,
        &EpisodeMemoryInvocation,
        &AgentState,
    ) {
        (
            self.tx,
            self.record.key(),
            self.record.invocation(),
            &self.expected_state,
        )
    }
}

pub(crate) fn prepare_dispatch_memory_finish_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    binding: &DispatchMemoryBinding,
    frozen: &FrozenEpisodeMemoryValues,
) -> Result<ValidatedDispatchMemoryFinish<'tx, 'connection>> {
    binding.validate()?;
    validate_frozen(frozen)?;
    let record = load_dispatch_memory_tx(tx, &binding.key, &binding.invocation)?
        .context(DispatchMemoryError::MissingStart)?;
    binding.matches(&record)?;
    let start = record
        .start()
        .context(DispatchMemoryError::AlreadyFinalized)?;
    require_identity(tx, &start.key, true)?;
    validate_dispatch_memory_members_tx(tx, &record)?;
    let expected_state = validate_original_dispatch(tx, start)?;
    ensure!(
        expected_state.turn_index == start.invocation.turn_index
            && expected_state.working_memory == start.baseline,
        DispatchMemoryError::BaselineAdvanced
    );
    if episode_memory::load_receipt(tx, &start.key, None, &start.invocation)?.is_some() {
        return Err(episode_memory::EpisodeMemoryError::EmbeddedPhaseCollision.into());
    }
    Ok(ValidatedDispatchMemoryFinish {
        tx,
        frozen: record
            .obligation()
            .map(|o| o.frozen.clone())
            .unwrap_or_else(|| frozen.clone()),
        record,
        expected_state: Box::new(expected_state),
    })
}

pub(crate) fn validate_detached_binding(
    token: &ValidatedDetachedMemory<'_, '_>,
    binding: &DispatchMemoryBinding,
) -> Result<()> {
    binding.matches(&token.record)
}

/// Consume one existing D1 phase after the once-only Work finalizer. It returns
/// only final State/effects; it never publishes or acknowledges the outer TX.
pub(crate) fn finish_dispatch_memory_tx(
    validated: ValidatedDispatchMemoryFinish<'_, '_>,
    final_business_state: &AgentState,
    final_work: &CapturedWork,
    request: &WorkDomainLoadRequest,
    current_reads: Option<
        &mut super::super::current_read_snapshot::CurrentReadBusinessTransaction<'_, '_>,
    >,
) -> Result<TransitionCommit> {
    let tx = validated.tx;
    let start = validated
        .record
        .start()
        .expect("validated unfinished dispatch");
    ensure!(
        final_work.scope == scope(&start.key),
        "memory final Work scope differs"
    );
    ensure!(
        final_business_state.working_memory == start.baseline
            && final_business_state.turn_index == start.invocation.turn_index,
        DispatchMemoryError::BaselineAdvanced
    );
    let embedded = episode_memory::prepare_existing_dispatch_episode_memory_tx(&validated)?;
    let (closure, work_queue, active_waits) = final_closure_tx(
        tx,
        final_business_state,
        final_work,
        request,
        validated.frozen.timestamp,
    )?;
    let snapshot =
        derive_working_memory_snapshot_from_observations(&work_queue, &active_waits, &closure);
    let mut refreshed = final_business_state.clone();
    let refresh = refresh_working_memory_from_snapshot(&mut refreshed, snapshot);
    let source = ContextDurableRead::new(tx, &start.key.agent_id)
        .message_by_id(&start.invocation.message_id)?
        .context("original dispatch source missing")?;
    ensure!(
        source.message_seq == Some(start.source_sequence) && source.kind == start.source_kind,
        "original dispatch source changed before memory finalization"
    );
    let plan = plan_episode_memory(
        EpisodeMemoryPlanInput {
            agent: &refreshed,
            actual_dispatch_turn_id: &start.key.actual_dispatch_turn_id,
            trigger: &source,
            prior_closure: &start.prior_closure,
            current_closure: &closure,
            previous_snapshot: &refresh.previous_snapshot,
            current_snapshot: &refresh.current_snapshot,
        },
        &validated.frozen,
    );
    let command = EpisodeMemoryTransitionCommand::new(
        start.key.clone(),
        start.invocation.clone(),
        (*validated.expected_state).clone(),
        plan,
        validated.frozen.timestamp,
    )?;
    let committed = episode_memory::write_embedded_episode_memory_with_current_read_tx(
        embedded,
        &command,
        final_business_state,
        current_reads,
    )?;
    let EpisodeMemoryCommit::Applied {
        mut transition,
        episode,
        ..
    } = committed
    else {
        return Err(episode_memory::EpisodeMemoryError::EmbeddedPhaseCollision.into());
    };
    // The existing original members are receipts from preceding transactions;
    // the current whole command's receipt is written last by its caller.
    mark_dispatch_memory_done_tx(tx, &start.key, &start.invocation)?;
    transition.effects.context_episodes.extend(episode);
    Ok(transition)
}

fn final_closure_tx(
    tx: &Transaction<'_>,
    state: &AgentState,
    work: &CapturedWork,
    request: &WorkDomainLoadRequest,
    now: DateTime<Utc>,
) -> Result<(
    ClosureDecision,
    crate::storage::WorkQueueReadModel,
    Vec<WaitConditionRecord>,
)> {
    // These remain explicit D5/D6 adapters. Preflight bounds their materialized
    // legacy facts in this same read view, before decoding any payload.
    let budget = request.byte_limit.min(MAX_METADATA_BYTES);
    let mut bytes = 0usize;
    let mut records = 0usize;
    for (table, predicate) in [
        (
            "tasks",
            "owner_agent_id=?1 AND status IN ('queued','running','cancelling')",
        ),
        ("external_triggers", "target_agent_id=?1"),
        ("execution_protocol_work_items", "agent_id=?1"),
        ("execution_protocol_attempts", "agent_id=?1"),
        ("execution_protocol_outcomes", "agent_id=?1"),
    ] {
        let (count, size): (usize, usize) = tx.query_row(&format!(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM {table} WHERE {predicate}"),
            [&state.id], |r| Ok((r.get(0)?,r.get(1)?)))?;
        records = records
            .checked_add(count)
            .context(DispatchMemoryError::ResourceExhausted)?;
        bytes = bytes
            .checked_add(size)
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            records <= request.record_limit.min(MAX_METADATA_RECORDS) && bytes <= budget,
            DispatchMemoryError::ResourceExhausted
        );
    }
    let read = ContextDurableRead::new(tx, &state.id);
    let triggers = read.external_triggers()?;
    let work_queue = work.work_queue(state, triggers)?;
    let works = work
        .records
        .iter()
        .filter_map(|r| match &r.record {
            CanonicalWorkRecord::Work(w) => Some(w.clone()),
            _ => None,
        })
        .collect();
    let waits = work
        .unresolved_wait_ids
        .iter()
        .map(|id| {
            let key = WorkKey::new(WorkFamily::Wait, id);
            match &work
                .lookup(&key)?
                .context("complete unresolved Wait body missing")?
                .record
            {
                CanonicalWorkRecord::Wait(wait) => Ok(wait.clone()),
                _ => anyhow::bail!("unresolved Work key is not a Wait"),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let mut active_waits = crate::storage::filter_context_active_waits(
        waits
            .into_iter()
            .filter(|w| w.status == WaitConditionStatus::Active)
            .collect(),
        works,
    );
    // Preserve the original active-for-agent order.
    active_waits.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let active_tasks = read.active_tasks(&state.id)?;
    let execution_snapshot = read.execution_protocol_state(&state.id)?;
    let (queue_len, has_interrupted_replay): (usize, bool) = tx.query_row(
        "SELECT COUNT(*),COALESCE(MAX(status='interrupted'),0) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted')",
        [&state.id], |r| Ok((r.get(0)?,r.get(1)?)))?;
    let invalid_timer: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM timers WHERE agent_id=?1 AND status='active' AND (json_extract(payload_json,'$.agent_id') IS NOT agent_id OR json_extract(payload_json,'$.status') IS NOT status))",
        [&state.id], |r| r.get(0))?;
    ensure!(
        !invalid_timer,
        "memory timer adapter columns and payload disagree"
    );
    let active_timers: usize = tx.query_row(
        "SELECT COUNT(*) FROM timers WHERE agent_id=?1 AND status='active'",
        [&state.id],
        |r| r.get(0),
    )?;
    let (events, event_bytes) = recent_events_tx(tx, &state.id, budget.saturating_sub(bytes))?;
    bytes = bytes
        .checked_add(event_bytes)
        .context(DispatchMemoryError::ResourceExhausted)?;
    let briefs = super::super::input_evidence_snapshot::qualified_dispatch_briefs_tx(
        tx,
        &work.scope,
        64,
        budget.saturating_sub(bytes),
    )?;
    let runtime_error = runtime_error_active(&events, &briefs);
    let projection = SchedulerProjection::from_observations(
        &SchedulerAgentSnapshot::from_state(state),
        queue_len,
        work_queue.clone(),
        SchedulerObservations {
            active_tasks,
            has_interrupted_replay,
            active_wait_conditions: active_waits.clone(),
            activation_waits: Vec::new(),
            execution_snapshot,
            active_timers,
            runtime_error,
        },
        now,
    );
    let closure = RuntimeHandle::closure_decision_from_projection(state, &projection, None);
    Ok((closure, work_queue, active_waits))
}

fn recent_events_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    byte_limit: usize,
) -> Result<(Vec<AuditEvent>, usize)> {
    let size:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(data_json AS BLOB))),0) FROM (SELECT data_json FROM audit_events WHERE agent_id=?1 ORDER BY event_seq DESC,created_at DESC LIMIT 64)", [agent_id], |r|r.get(0))?;
    ensure!(size <= byte_limit, DispatchMemoryError::ResourceExhausted);
    let mut statement=tx.prepare("SELECT audit_event_id,event_seq,kind,created_at,data_json FROM audit_events WHERE agent_id=?1 ORDER BY event_seq DESC,created_at DESC LIMIT 64")?;
    let mut result = Vec::new();
    for row in statement.query_map([agent_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })? {
        let (id, seq, kind, at, payload) = row?;
        let event: AuditEvent = serde_json::from_str(&payload)?;
        ensure!(
            event.id == id
                && event.event_seq == seq
                && event.kind == kind
                && super::super::repositories::timestamp(event.created_at) == at,
            "memory audit adapter columns and payload disagree"
        );
        result.push(event);
    }
    result.reverse();
    Ok((result, size))
}

/// Shared dry-run/inventory/delete path. Mutable obligation metadata cannot
/// retain arbitrary evidence without original whole-transaction ACK proofs.
pub(crate) fn dispatch_memory_retention_roots_in(
    connection: &Connection,
) -> Result<BTreeSet<InputEvidenceKey>> {
    let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='dispatch_memory_checkpoints')",[],|r|r.get(0))?;
    if !exists {
        return Ok(BTreeSet::new());
    }
    let handoffs_exist: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='dispatch_memory_retired_handoffs')", [], |r| r.get(0))?;
    // Disposed retired checkpoints remain immutable history. Only still-live
    // responsibilities require the old full-record preparation budget; every
    // excluded handoff is validated below before any roots can be released.
    let budget_sql = if handoffs_exist {
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(c.payload_json AS BLOB))),0) FROM dispatch_memory_checkpoints c WHERE c.state!='done' AND NOT EXISTS(SELECT 1 FROM dispatch_memory_retired_handoffs h WHERE h.agent_id=c.agent_id AND h.identity_incarnation=c.identity_incarnation AND h.turn_id=c.turn_id)"
    } else {
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM dispatch_memory_checkpoints WHERE state!='done'"
    };
    let (count, bytes): (usize, usize) =
        connection.query_row(budget_sql, [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    ensure!(
        count <= MAX_METADATA_RECORDS && bytes <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    let mut statement=connection.prepare("SELECT agent_id,identity_incarnation,turn_id,source_message_id,turn_index FROM dispatch_memory_checkpoints WHERE state!='done' ORDER BY agent_id,identity_incarnation,turn_index,turn_id")?;
    let mut roots = BTreeSet::new();
    for row in statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, u64>(4)?,
        ))
    })? {
        let (agent_id, identity_incarnation, actual_dispatch_turn_id, message_id, turn_index) =
            row?;
        let key = EpisodeMemoryKey {
            agent_id,
            identity_incarnation,
            actual_dispatch_turn_id,
        };
        let invocation = EpisodeMemoryInvocation {
            message_id,
            turn_index,
        };
        if super::retired::validated_retired_handoff_in(connection, &key, &invocation)? {
            continue;
        }
        let record = load_dispatch_memory_in(connection, &key, &invocation)?
            .context("unfinished dispatch disappeared")?;
        validate_dispatch_memory_members_in(connection, &record)?;
        for key in record.input_roots() {
            roots.insert(key);
            ensure!(
                roots.len() <= MAX_METADATA_RECORDS,
                DispatchMemoryError::ResourceExhausted
            );
        }
    }
    Ok(roots)
}

/// Small protocol-address read. It does not hydrate the baseline, acknowledge
/// memory, or authorize a missing seed. Deferred/Finish later validate the
/// complete record and original outcome in their own transaction.
pub(crate) fn load_dispatch_memory_binding_tx(
    tx: &Transaction<'_>,
    original_scope: &InputEvidenceScope,
    state: &AgentState,
) -> Result<Option<DispatchMemoryBinding>> {
    ensure!(
        state.id == original_scope.agent_id,
        "memory binding State scope differs"
    );
    let Some(current) = state.current_execution_binding.as_ref() else {
        let Some(turn_id) = state.current_turn_id.as_ref() else {
            return Ok(None);
        };
        let (agent,index,terminal):(String,u64,bool)=tx.query_row(
            "SELECT agent_id,turn_index,COALESCE(json_type(payload_json,'$.terminal')='object',0) FROM turn_records WHERE turn_id=?1",
            [turn_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        ensure!(
            agent == state.id
                && index == state.turn_index
                && terminal
                && state.current_run_id.is_none(),
            "actual started dispatch has no execution binding"
        );
        return Ok(None);
    };
    let Some(turn_id) = state.current_turn_id.as_ref() else {
        return Ok(None);
    };
    ensure!(
        turn_id == &current.turn_id,
        "memory current Turn differs from execution binding"
    );
    let key = EpisodeMemoryKey {
        agent_id: original_scope.agent_id.clone(),
        identity_incarnation: original_scope.identity_incarnation,
        actual_dispatch_turn_id: turn_id.clone(),
    };
    require_identity(tx, &key, true)?;
    let normalized:(u64,Option<String>,Option<String>,Option<String>,Option<String>,u64)=tx.query_row(
        "SELECT turn_index,current_run_id,json_extract(payload_json,'$.current_turn_id'),json_extract(payload_json,'$.current_execution_binding.turn_id'),json_extract(payload_json,'$.current_execution_binding.source_message_id'),json_extract(payload_json,'$.turn_index') FROM agent_states WHERE agent_id=?1",
        [&state.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    ensure!(
        normalized
            == (
                state.turn_index,
                state.current_run_id.clone(),
                Some(turn_id.clone()),
                Some(turn_id.clone()),
                Some(current.source_message_id.clone()),
                state.turn_index
            ),
        "memory binding differs from canonical current State"
    );
    let invocation = EpisodeMemoryInvocation {
        message_id: current.source_message_id.clone(),
        turn_index: state.turn_index,
    };
    load_binding_header_tx(tx, original_scope, &key, &invocation, &state.current_run_id)
}

/// Existing actual Turn address only. This protocol read neither proves a
/// physical producer ended nor grants a new dispatch/checkpoint permission.
pub(crate) fn load_dispatch_memory_binding_by_turn_tx(
    tx: &Transaction<'_>,
    original_scope: &InputEvidenceScope,
    turn_id: &str,
) -> Result<Option<DispatchMemoryBinding>> {
    let key = EpisodeMemoryKey {
        agent_id: original_scope.agent_id.clone(),
        identity_incarnation: original_scope.identity_incarnation,
        actual_dispatch_turn_id: turn_id.into(),
    };
    require_identity(tx, &key, true)?;
    let (agent,index,payload_agent,payload_index,run):(String,u64,String,u64,Option<String>)=tx.query_row(
        "SELECT agent_id,turn_index,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.turn_index'),json_extract(payload_json,'$.run_id') FROM turn_records WHERE turn_id=?1",
        [turn_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    ensure!(
        agent == original_scope.agent_id && payload_agent == agent && payload_index == index,
        "original memory Turn address differs from canonical owner/index"
    );
    let address:Option<(String,u64)>=tx.query_row(
        "SELECT source_message_id,turn_index FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
        params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let address=match address {
        Some(address)=>address,
        None=>tx.query_row(
            "SELECT source_message_id,turn_index FROM dispatch_memory_legacy_dispatches WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
            params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?
            .context(DispatchMemoryError::MissingStart)?,
    };
    ensure!(
        address.1 == index,
        "original memory seed and actual Turn index differ"
    );
    let invocation = EpisodeMemoryInvocation {
        message_id: address.0,
        turn_index: address.1,
    };
    load_binding_header_tx(tx, original_scope, &key, &invocation, &run)
}

pub(super) fn load_binding_header_tx(
    tx: &Transaction<'_>,
    original_scope: &InputEvidenceScope,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
    expected_run: &Option<String>,
) -> Result<Option<DispatchMemoryBinding>> {
    let header_sql="json_object('key',json_extract(payload_json,'$.start.key'),'invocation',json_extract(payload_json,'$.start.invocation'),'source_sequence',json_extract(payload_json,'$.start.source_sequence'),'source_kind',json_extract(payload_json,'$.start.source_kind'),'digest',json_extract(payload_json,'$.start.digest'),'original_start',json_extract(payload_json,'$.original_start'))";
    let found:Option<(String,usize)>=tx.query_row(&format!("SELECT state,CASE WHEN state='done' THEN length(CAST(payload_json AS BLOB)) ELSE length(CAST({header_sql} AS BLOB)) END FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3"),params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((record_state, size)) = found else {
        ensure!(
            is_legacy_dispatch_tx(tx, &key, &invocation)?,
            DispatchMemoryError::MissingStart
        );
        validate_binding_address_tx(tx, expected_run, key, invocation, None, false)?;
        return Ok(None);
    };
    ensure!(size <= 64 * 1024, DispatchMemoryError::ResourceExhausted);
    if record_state == "done" {
        let record = load_dispatch_memory_tx(tx, &key, &invocation)?
            .context(DispatchMemoryError::MissingStart)?;
        let DispatchMemoryRecord::Done { completed } = &record else {
            anyhow::bail!("completed dispatch projection differs");
        };
        let receipt = episode_memory::load_receipt(tx, &key, None, &invocation)?
            .context(DispatchMemoryError::MissingReceipt)?;
        ensure!(
            completed.receipt_digest == canonical_command_digest(&receipt)?
                && completed.committed_at == receipt.committed_at,
            "completed dispatch differs from original D1 receipt"
        );
        validate_binding_address_tx(tx, expected_run, key, invocation, None, true)?;
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Header {
        key: EpisodeMemoryKey,
        invocation: EpisodeMemoryInvocation,
        source_sequence: u64,
        source_kind: MessageKind,
        digest: String,
        original_start: PreparedInputMutation,
    }
    let (source,index,seed,status,record_status,payload):(String,u64,String,String,String,String)=tx.query_row(
        &format!("SELECT source_message_id,turn_index,seed_digest,state,json_extract(payload_json,'$.state'),{header_sql} FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3"),
        params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    let header: Header = serde_json::from_str(&payload)?;
    ensure!(
        header.key == *key
            && header.invocation == *invocation
            && source == invocation.message_id
            && index == invocation.turn_index
            && seed == header.digest
            && valid_digest(&seed)
            && status == record_status
            && matches!(status.as_str(), "started" | "pending"),
        "memory Start header columns and payload disagree"
    );
    validate_descriptor(&header.original_start)?;
    ensure!(
        header.original_start.family() == InputMutationFamily::AgentPosture
            && header.original_start.scopes().contains(original_scope),
        "memory Start header has another original command"
    );
    let receipt_size:usize=tx.query_row("SELECT length(CAST(prepared_json AS BLOB)) FROM input_mutation_receipts WHERE command_nonce=?1",[header.original_start.command_nonce()],|r|r.get(0))?;
    ensure!(
        receipt_size <= 64 * 1024,
        DispatchMemoryError::ResourceExhausted
    );
    let (receipt_digest,scope_digest,prepared):(String,String,String)=tx.query_row("SELECT command_digest,scope_digest,prepared_json FROM input_mutation_receipts WHERE command_nonce=?1",[header.original_start.command_nonce()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    ensure!(
        receipt_digest == header.original_start.command_digest()
            && scope_digest == header.original_start.scope_digest()
            && serde_json::from_str::<PreparedInputMutation>(&prepared)? == header.original_start,
        "memory Start receipt address differs"
    );
    validate_binding_address_tx(
        tx,
        expected_run,
        key,
        invocation,
        Some((header.source_sequence, &header.source_kind)),
        false,
    )?;
    Ok(Some(DispatchMemoryBinding {
        key: key.clone(),
        invocation: invocation.clone(),
        seed_digest: seed,
    }))
}

fn validate_binding_address_tx(
    tx: &Transaction<'_>,
    expected_run: &Option<String>,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
    source: Option<(u64, &MessageKind)>,
    completed: bool,
) -> Result<()> {
    let (source_agent, seq, kind): (String, u64, String) = tx.query_row(
        "SELECT agent_id,message_seq,kind FROM messages WHERE evidence_id=?1",
        [&invocation.message_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        source_agent == key.agent_id && seq > 0,
        "memory binding canonical source differs"
    );
    if let Some((expected_seq, expected_kind)) = source {
        ensure!(
            seq == expected_seq
                && serde_json::to_value(expected_kind)?.as_str() == Some(kind.as_str()),
            "memory binding canonical source changed"
        );
    }
    let (agent,index,run,owns_source):(String,u64,Option<String>,bool)=tx.query_row(
        "SELECT agent_id,turn_index,json_extract(payload_json,'$.run_id'),(EXISTS(SELECT 1 FROM json_each(json_extract(payload_json,'$.input_message_ids')) WHERE value=?2) OR COALESCE(json_extract(payload_json,'$.trigger.message_id')=?2,0)) FROM turn_records WHERE turn_id=?1",
        params![key.actual_dispatch_turn_id,invocation.message_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    ensure!(
        agent == key.agent_id
            && index == invocation.turn_index
            && (completed || &run == expected_run)
            && owns_source,
        "memory binding canonical Turn/source differs"
    );
    Ok(())
}
