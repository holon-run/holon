//! Host and Task fixtures shared by runtime and HTTP tests.
use std::sync::Arc;

use tempfile::{tempdir, TempDir};

use super::{resident_input_evidence::InputEvidenceScope, RuntimeHandle};
use crate::{
    config::AppConfig,
    host::RuntimeHost,
    provider::StubProvider,
    runtime_db::current_query::append_qualified_event_for_fixture,
    types::{AgentState, AuditEvent, TaskRecord},
};
use anyhow::Result;

pub async fn persist_task_transition(
    runtime: &RuntimeHandle,
    record: &TaskRecord,
    event_type: &'static str,
) -> anyhow::Result<()> {
    runtime.persist_task_transition(record, event_type).await
}

pub async fn host_backed_test_runtime() -> (TempDir, RuntimeHost, RuntimeHandle) {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let mut config =
        crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    config.user_home_dir = None;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
    let runtime = host.default_runtime().await.unwrap();
    (home, host, runtime)
}

pub async fn qualified_event_history_fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = Some(home.path().to_path_buf());
    config.default_agent_id = "default".into();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    let identity = host.ensure_default_agent_identity()?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id,
        identity_incarnation: identity.incarnation,
    };
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&AgentState::new(&scope.agent_id))?;
    for index in 0..4 {
        let event = AuditEvent::legacy("qualified-history", serde_json::json!({"index": index}));
        let ledger = input_evidence_budget(&host);
        append_qualified_event_for_fixture(host.runtime_db(), &scope, ledger, &event)?;
    }
    Ok((home, host))
}

pub fn test_host() -> (TempDir, RuntimeHost) {
    test_host_with_provider(Arc::new(StubProvider::new("done")))
}

pub fn test_host_with_provider(
    provider: Arc<dyn crate::provider::AgentProvider>,
) -> (TempDir, RuntimeHost) {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    config.user_home_dir = Some(home.path().to_path_buf());
    let host = RuntimeHost::new_with_provider(config, provider).unwrap();
    host.ensure_default_agent_identity().unwrap();
    (home, host)
}

pub fn mutation_owner(runtime: &RuntimeHandle) -> &super::mutation_owner::MutationOwner {
    &runtime.inner.mutation_owner
}

pub fn swap_scheduler_audit_frontier(runtime: &RuntimeHandle, value: u64) -> u64 {
    runtime
        .inner
        .scheduler_audit_frontier
        .swap(value, std::sync::atomic::Ordering::AcqRel)
}

pub fn bump_scheduler_audit_frontier(runtime: &RuntimeHandle) -> u64 {
    runtime
        .inner
        .scheduler_audit_frontier
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
}

pub use crate::host::http_fixture::{active_identity, has_no_loaded_agents, input_evidence_budget};

pub use super::state_mutation::work_planning::test_support::publish_unprepared_scheduler_frontier;

pub use crate::runtime_db::temp_fixture::TempRuntimeDb;

pub fn seed_qualified_timers(
    db: &crate::runtime_db::RuntimeDb,
    agent: &AgentState,
    incarnation: u64,
    timers: &[crate::types::TimerRecord],
) -> Result<()> {
    use crate::runtime::resident_scheduling::{SchedulingFamily, SchedulingKey};
    use crate::runtime_db::scheduling_snapshot::{
        prepare_scheduling_write_tx, register_scheduling_writes_tx,
    };
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: agent.id.clone(),
        identity_incarnation: incarnation,
    };
    let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        crate::config::StateMemoryLimits::default(),
    )?;
    let scheduling_request = crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
        capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    let keys = timers
        .iter()
        .map(|timer| SchedulingKey::new(SchedulingFamily::Timer, &timer.id))
        .collect();
    db.transaction(|tx| {
        let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &scheduling_request, &ledger)?;
        for timer in timers {
            crate::runtime_db::repositories::upsert_timer_tx(tx, timer)?;
        }
        register_scheduling_writes_tx(tx, &proof, &keys, agent, &scheduling_request).map(|_| ())
    })
}

pub async fn restart_fixture_host(host: RuntimeHost) -> RuntimeHost {
    let config = host.config().as_ref().clone();
    host.shutdown().await.unwrap();
    drop(host);
    RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap()
}

#[cfg(feature = "http-test-support")]
pub use crate::host::http_fixture::inject_constructor_error;

#[cfg(feature = "http-test-support")]
pub fn reject_quarantined_constructor(host: &RuntimeHost, agent_id: &str) {
    let incarnation = active_identity(host, agent_id).unwrap().incarnation;
    inject_constructor_error(
        host,
        agent_id,
        super::AgentConstructorQuarantine {
            agent_id: agent_id.to_owned(),
            incarnation,
        }
        .into(),
    );
}

#[cfg(feature = "http-test-support")]
pub fn seed_callback_fixture(
    db: &crate::runtime_db::RuntimeDb,
    mut trigger: crate::types::ExternalTriggerRecord,
) -> Result<String> {
    let token = crate::callbacks::generate_callback_token();
    trigger.token_hash = crate::callbacks::hash_callback_token(&token);
    trigger.token = Some(token.clone());
    db.transaction(|tx| crate::runtime_db::repositories::upsert_external_trigger_tx(tx, &trigger))?;
    Ok(token)
}

pub fn queue_mutation_witness(
    ticket: &str,
    scope: InputEvidenceScope,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<super::mutation_witness::MutationWitness> {
    use crate::runtime_db::input_mutation_receipts::{
        InputMutationFamily, OriginalInputMutationAttempt, PreparedInputMutation,
    };
    super::mutation_witness::MutationWitness::input_mutation(
        ticket.into(),
        OriginalInputMutationAttempt::new(PreparedInputMutation::new(
            InputMutationFamily::Queue,
            ticket.into(),
            vec![scope],
            "a".repeat(64),
            now,
        )?),
        Vec::new(),
    )
}

pub fn identity_incarnation(runtime: &RuntimeHandle) -> u64 {
    runtime.inner.identity_incarnation
}

#[cfg(feature = "http-test-support")]
pub fn observe_known_commit(record: &super::mutation_owner::RecoveryRecord) -> bool {
    record.known.is_some()
}

#[cfg(feature = "http-test-support")]
pub fn reconcile_known_commit(
    reservation: &mut super::mutation_owner::MutationReservation,
    record: super::mutation_owner::RecoveryRecord,
) -> Result<()> {
    super::mutation_owner::MutationReservation::reconcile_all(
        std::slice::from_mut(reservation),
        vec![record.known],
    )?;
    Ok(())
}

#[cfg(feature = "http-test-support")]
pub fn resolve_uncommitted_witness(
    reservations: &mut [super::mutation_owner::MutationReservation],
    key: &super::mutation_owner::RecoveryKey,
    witness: &super::mutation_witness::MutationWitness,
) -> Result<super::mutation_owner::WitnessUnknownResolution> {
    Ok(
        super::mutation_owner::MutationReservation::resolve_input_unknown(
            reservations,
            key,
            witness,
            None,
        )?,
    )
}

#[cfg(feature = "http-test-support")]
pub fn commit_snapshot_for_test(
    reservation: &super::mutation_owner::MutationReservation,
    root: &super::resident_state::AgentSnapshot,
) -> Result<()> {
    reservation.committed_prepared_domains(
        root.state.clone(),
        root.episodes.clone(),
        root.input_evidence.clone(),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        root.queue_posture,
        root.scheduler_events,
    )?;
    Ok(())
}

#[cfg(feature = "http-test-support")]
pub fn execution_protocol_state(
    db: &crate::runtime_db::RuntimeDb,
    agent_id: &str,
) -> Result<Option<crate::domain::execution_protocol::ExecutionProtocolState>> {
    db.transitions()
        .load_execution_protocol_state_if_initialized(agent_id)
}

#[cfg(feature = "http-test-support")]
pub fn publish_event_fixture(
    storage: &crate::storage::AppStorage,
    effects: &crate::runtime_db::transitions::PostCommitEffects,
) -> usize {
    storage.publish_transition_events(effects).len()
}

#[cfg(feature = "http-test-support")]
pub fn host_mutation_owner(
    host: &RuntimeHost,
    agent_id: &str,
    incarnation: u64,
) -> Result<super::mutation_owner::MutationOwner> {
    host.bridge().mutation_owner_for(agent_id, incarnation)
}

#[cfg(feature = "http-test-support")]
pub async fn reserve_agent_mutations(
    host: &RuntimeHost,
    agent_ids: &[String],
) -> Result<Vec<super::mutation_owner::MutationReservation>> {
    host.reserve_agent_mutations(agent_ids).await
}

#[cfg(feature = "http-test-support")]
pub async fn reserve_mutation_scopes(
    host: &RuntimeHost,
    scopes: &[(String, u64)],
) -> Result<Vec<super::mutation_owner::MutationReservation>> {
    host.reserve_mutation_scopes(scopes).await
}
