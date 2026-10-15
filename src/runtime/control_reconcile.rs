//! Focused durable handoff for original Control cancellation obligations.
//!
//! Missing process memory is not completion evidence. Startup proof covers
//! cooperative runtime producers, never an unobserved surviving OS process.

use super::{control_cancellation::ControlJoinLease, RuntimeHandle};
use crate::{
    runtime_db::{
        control_transition::{
            ControlCancellationKind, ControlCancellationTarget, ControlKey, ControlObligationState,
        },
        RuntimeDb, RuntimeDbLock,
    },
    types::{TaskKind, TaskRecord, TaskStatus, TimerStatus},
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, Weak},
};
use tokio::sync::{MutexGuard, Notify};

/// Only Host's pre-activation recovery path may mint this. Its caller has
/// checked the fleet has never activated and has no loaded/temporary/draining
/// runtimes, while holding this handoff guard and its real serve lease.
/// Borrowing both prevents the proof escaping the protected startup window.
pub(crate) struct ControlStartupProof<'a> {
    database: PathBuf,
    observation_id: String,
    _lease: &'a RuntimeDbLock,
    _handoff: &'a MutexGuard<'a, ()>,
}
impl<'a> ControlStartupProof<'a> {
    pub(crate) fn for_pre_activation_host(
        db: &RuntimeDb,
        lease: &'a RuntimeDbLock,
        expected_maintenance_path: &Path,
        handoff: &'a MutexGuard<'a, ()>,
    ) -> Result<Self> {
        ensure!(
            lease.path() == expected_maintenance_path,
            "Control startup serve lease path mismatch"
        );
        Ok(Self {
            database: db.path().to_owned(),
            observation_id: uuid::Uuid::new_v4().to_string(),
            _lease: lease,
            _handoff: handoff,
        })
    }
    fn evidence(&self, db: &RuntimeDb) -> Result<Value> {
        ensure!(
            db.path() == self.database,
            "Control startup proof database mismatch"
        );
        Ok(
            json!({"proof":"exclusive_pre_activation_process_recovery", "observation_id":self.observation_id,
            "original_runtime_producer_absent":true,"os_process_joined":false}),
        )
    }
}

#[derive(Clone)]
enum RegistrationState {
    Preparing(usize),
    NeverStarted,
    Attached(Weak<ControlJoinLease>),
    Joined { lease_id: String },
    UnprovenExternal,
}
struct RegistrationIdentity {
    id: String,
    agent_id: String,
    kind: TaskKind,
    created_at: DateTime<Utc>,
    rejoin: Option<crate::domain::execution_protocol::RejoinFence>,
    raw_rejoin: Option<super::task_generation::TaskRejoinGeneration>,
}
impl From<&TaskRecord> for RegistrationIdentity {
    fn from(task: &TaskRecord) -> Self {
        Self {
            id: task.id.clone(),
            agent_id: task.agent_id.clone(),
            kind: task.kind,
            created_at: task.created_at,
            rejoin: task.rejoin_fence().ok(),
            raw_rejoin: super::task_generation::TaskRejoinGeneration::of(task),
        }
    }
}
pub(super) struct ControlRegistrationRecord {
    identity: RegistrationIdentity,
    incarnation: u64,
    owner_epoch: u64,
    may_prove_never_started: bool,
    state: StdMutex<RegistrationState>,
    changed: Notify,
}
impl ControlRegistrationRecord {
    fn matches_task(&self, task: &TaskRecord, incarnation: u64, owner_epoch: u64) -> bool {
        self.identity.id == task.id
            && self.identity.agent_id == task.agent_id
            && self.identity.kind == task.kind
            && self.identity.created_at == task.created_at
            && self.identity.raw_rejoin.is_some_and(|original| {
                Some(original) == super::task_generation::TaskRejoinGeneration::of(task)
            })
            && self.incarnation == incarnation
            && self.owner_epoch == owner_epoch
    }
    fn matches_target(&self, key: &ControlKey, target: &ControlCancellationTarget) -> bool {
        target.target_kind == ControlCancellationKind::Task
            && self.identity.id == target.target_identity
            && self.identity.agent_id == key.agent_id
            && self.incarnation == key.identity_incarnation
            && target.target_generation.get("created_at") == Some(&json!(self.identity.created_at))
            && target.target_generation.get("rejoin") == Some(&json!(self.identity.rejoin))
            && self.identity.raw_rejoin.is_some_and(|original| {
                original.matches_control_original(&target.target_generation)
            })
            && target.target_generation.get("owner_epoch") == Some(&json!(self.owner_epoch))
    }
    pub(super) fn attach(&self, lease: &Arc<ControlJoinLease>) {
        *self.state.lock().expect("Control registration poisoned") =
            RegistrationState::Attached(Arc::downgrade(lease));
        self.changed.notify_waiters();
    }
    pub(super) fn joined(&self, lease_id: &str) {
        *self.state.lock().expect("Control registration poisoned") = RegistrationState::Joined {
            lease_id: lease_id.into(),
        };
        self.changed.notify_waiters();
    }
    async fn proof(
        &self,
        key: &ControlKey,
        target: &ControlCancellationTarget,
    ) -> Result<Option<Value>> {
        ensure!(
            self.matches_target(key, target),
            "Control registration belongs to a different original generation"
        );
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let state = self
                .state
                .lock()
                .expect("Control registration poisoned")
                .clone();
            match state {
                RegistrationState::Preparing(_) => notified.await,
                RegistrationState::NeverStarted => {
                    return Ok(Some(json!({"proof":"registered_original_never_spawned",
                    "created_at":self.identity.created_at,"rejoin":self.identity.rejoin,"owner_epoch":self.owner_epoch})))
                }
                RegistrationState::Joined { lease_id } => {
                    return Ok(Some(
                        json!({"proof":"original_registered_monitor_joined","runtime_lease_id":lease_id}),
                    ))
                }
                RegistrationState::Attached(lease) => {
                    let Some(lease) = lease.upgrade() else {
                        return Ok(None);
                    };
                    lease.join().await?;
                    self.joined(lease.lease_id());
                }
                RegistrationState::UnprovenExternal => return Ok(None),
            }
        }
    }
    fn reaped(&self) -> bool {
        matches!(
            *self.state.lock().expect("Control registration poisoned"),
            RegistrationState::Joined { .. }
        )
    }
}

/// This ticket is declared before the corresponding active Task SQL. Dropping
/// a pure runtime registration before spawn is proof only together with a
/// fresh canonical terminal Task check. Command/child producers never get that
/// inference because an independent external producer may already exist.
pub(super) struct ControlRegistrationTicket(Arc<ControlRegistrationRecord>);
impl Drop for ControlRegistrationTicket {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("Control registration poisoned");
        if let RegistrationState::Preparing(count) = &mut *state {
            *count -= 1;
            if *count == 0 {
                *state = if self.0.may_prove_never_started {
                    RegistrationState::NeverStarted
                } else {
                    RegistrationState::UnprovenExternal
                };
            }
        }
        self.0.changed.notify_waiters();
    }
}
#[derive(Default)]
pub(super) struct ControlRegistrationLedger(
    StdMutex<BTreeMap<String, Arc<ControlRegistrationRecord>>>,
);
impl ControlRegistrationLedger {
    pub(super) fn before_active_task_sql(
        &self,
        task: &TaskRecord,
        incarnation: u64,
        owner_epoch: u64,
    ) -> ControlRegistrationTicket {
        let mut records = self.0.lock().expect("Control registration ledger poisoned");
        records.retain(|_, record| !record.reaped() || Arc::strong_count(record) > 1);
        let record = records.entry(task.id.clone()).or_insert_with(|| {
            Arc::new(ControlRegistrationRecord {
                identity: RegistrationIdentity::from(task),
                incarnation,
                owner_epoch,
                may_prove_never_started: matches!(
                    task.kind,
                    TaskKind::AgentMessageWait | TaskKind::SleepJob
                ),
                state: StdMutex::new(RegistrationState::Preparing(0)),
                changed: Notify::new(),
            })
        });
        if !record.matches_task(task, incarnation, owner_epoch) {
            *record = Arc::new(ControlRegistrationRecord {
                identity: RegistrationIdentity::from(task),
                incarnation,
                owner_epoch,
                may_prove_never_started: matches!(
                    task.kind,
                    TaskKind::AgentMessageWait | TaskKind::SleepJob
                ),
                state: StdMutex::new(RegistrationState::Preparing(0)),
                changed: Notify::new(),
            });
        }
        {
            let mut state = record.state.lock().expect("Control registration poisoned");
            match &mut *state {
                RegistrationState::Preparing(count) => *count += 1,
                RegistrationState::NeverStarted => *state = RegistrationState::Preparing(1),
                _ => {}
            }
        }
        ControlRegistrationTicket(record.clone())
    }
    pub(super) fn for_task(&self, task: &TaskRecord) -> Option<Arc<ControlRegistrationRecord>> {
        self.0
            .lock()
            .expect("Control registration ledger poisoned")
            .get(&task.id)
            .filter(|record| {
                record.identity.agent_id == task.agent_id
                    && record.identity.created_at == task.created_at
                    && record.identity.raw_rejoin.is_some_and(|original| {
                        Some(original) == super::task_generation::TaskRejoinGeneration::of(task)
                    })
            })
            .cloned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlCanonicalDisposition {
    Completed,
    DurablyHandedOff,
}

fn task_matches_target(
    task: &TaskRecord,
    key: &ControlKey,
    target: &ControlCancellationTarget,
) -> bool {
    task.id == target.target_identity
        && task.agent_id == key.agent_id
        && target.target_generation.get("created_at") == Some(&json!(task.created_at))
        && super::task_generation::control_terminal_rejoin_matches(task, &target.target_generation)
        && target.target.get("kind") == Some(&json!(task.kind))
        && target.target.get("recovery") == Some(&json!(task.recovery))
        && target.target.get("work_item_id") == Some(&json!(task.effective_work_item_id()))
        && matches!(
            task.status,
            TaskStatus::Interrupted
                | TaskStatus::Cancelled
                | TaskStatus::Completed
                | TaskStatus::Failed
        )
}
fn handoff(
    db: &RuntimeDb,
    key: &ControlKey,
    target: &ControlCancellationTarget,
    at: DateTime<Utc>,
    reason: &str,
) -> Result<ControlCanonicalDisposition> {
    db.mark_control_obligation_needs_attention(
        key,
        target,
        at,
        json!({"disposition":"durable_handoff",
        "reason":reason,"physical_completion_proven":false,"artifact_deleted":false}),
    )?;
    let saved = db
        .control_obligation(key, target)?
        .context("Control durable handoff disappeared")?;
    ensure!(
        matches!(
            saved.state,
            ControlObligationState::NeedsAttention | ControlObligationState::Completed
        ),
        "Control handoff not durable"
    );
    Ok(if saved.state == ControlObligationState::Completed {
        ControlCanonicalDisposition::Completed
    } else {
        ControlCanonicalDisposition::DurablyHandedOff
    })
}

/// Call only under the exact owner reservation (or the exclusive startup proof).
/// It never installs State, replays Control SQL, changes Task/Timer, or cleans FS.
fn settle_proven_target(
    db: &RuntimeDb,
    key: &ControlKey,
    target: &ControlCancellationTarget,
    proof: Option<Value>,
    at: DateTime<Utc>,
) -> Result<ControlCanonicalDisposition> {
    let saved = db
        .control_obligation(key, target)?
        .context("Control original obligation missing")?;
    if saved.state == ControlObligationState::Completed {
        return Ok(ControlCanonicalDisposition::Completed);
    }
    let canonical_terminal = match target.target_kind {
        ControlCancellationKind::Task => db
            .tasks()
            .latest(&target.target_identity)?
            .as_ref()
            .is_some_and(|task| task_matches_target(task, key, target)),
        ControlCancellationKind::TimerLoop => db
            .timers()
            .latest(&target.target_identity)?
            .is_some_and(|timer| {
                timer.agent_id == key.agent_id
                    && target.target_generation.get("created_at") == Some(&json!(timer.created_at))
                    && target.target_generation.get("fire_count") == Some(&json!(timer.fire_count))
                    && target.target_generation.get("next_fire_at")
                        == Some(&json!(timer.next_fire_at))
                    && matches!(
                        timer.status,
                        TimerStatus::Cancelled | TimerStatus::Completed
                    )
            }),
        ControlCancellationKind::Run => true, // original run is an in-process processing future
        ControlCancellationKind::Worktree => false, // separate dependency/artifact check
    };
    if canonical_terminal {
        if let Some(proof) = proof {
            db.complete_control_obligation(key, target, at, proof)?;
            return Ok(ControlCanonicalDisposition::Completed);
        }
    }
    handoff(
        db,
        key,
        target,
        at,
        if canonical_terminal {
            "original_producer_join_unproven"
        } else {
            "original_domain_replaced_missing_or_not_terminal"
        },
    )
}

/// Before any activation; caller holds all relevant owner permits as well as
/// the actual startup proof. Retired scopes are allowed, never successor State.
pub(crate) fn reconcile_control_scope_at_startup(
    db: &RuntimeDb,
    agent_id: &str,
    incarnation: u64,
    proof: &ControlStartupProof<'_>,
    at: DateTime<Utc>,
) -> Result<
    Vec<(
        ControlKey,
        ControlCancellationTarget,
        ControlCanonicalDisposition,
    )>,
> {
    let startup = proof.evidence(db)?;
    db.pending_control_obligations(agent_id, incarnation)?
        .into_iter()
        .map(|(key, target)| {
            let eligible = match target.target_kind {
                ControlCancellationKind::Run | ControlCancellationKind::TimerLoop => true,
                ControlCancellationKind::Task => matches!(
                    target.target.get("kind").and_then(Value::as_str),
                    Some("agent_message_wait" | "sleep_job")
                ),
                ControlCancellationKind::Worktree => false,
            };
            let disposition =
                settle_proven_target(db, &key, &target, eligible.then(|| startup.clone()), at)?;
            Ok((key, target, disposition))
        })
        .collect()
}

impl RuntimeHandle {
    /// Original obligation checkpoints do not need authority to mutate a current
    /// Active State. Retired metadata is allowed, but unknown/replaced owners are not.
    pub(super) async fn reserve_control_obligation_scope(
        &self,
        key: &ControlKey,
    ) -> Result<Vec<super::mutation_owner::MutationReservation>> {
        ensure!(
            key.agent_id == self.inner.mutation_owner.agent_id()
                && key.identity_incarnation == self.inner.identity_incarnation,
            "Control original obligation belongs to another identity incarnation"
        );
        let mut reservations = super::mutation_owner::MutationOwner::reserve_all(vec![self
            .inner
            .mutation_owner
            .clone()])
        .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        // Ready Metadata(None) is valid. Do not require or install an AgentState.
        self.inner.mutation_owner.capture()?;
        Ok(reservations)
    }

    /// One bootstrap/replay pass for this loaded identity only. Historical
    /// incarnations belong to Host's explicit pre-activation scope actor.
    pub(super) async fn reconcile_loaded_control_obligations(&self) -> Result<()> {
        let pending = self.inner.runtime_db.pending_control_obligations(
            self.inner.mutation_owner.agent_id(),
            self.inner.identity_incarnation,
        )?;
        let Some((first_key, _)) = pending.first() else {
            return Ok(());
        };
        let reservations = self.reserve_control_obligation_scope(first_key).await?;
        // A known ticket can have installed its saved plan while reserving.
        // Re-observe the unfinished set under that same owner before selecting.
        let pending = self.inner.runtime_db.pending_control_obligations(
            self.inner.mutation_owner.agent_id(),
            self.inner.identity_incarnation,
        )?;
        let mut commands = BTreeMap::<String, (ControlKey, Vec<ControlCancellationTarget>)>::new();
        for (key, target) in pending {
            commands
                .entry(key.command_id.clone())
                .or_insert_with(|| (key, Vec::new()))
                .1
                .push(target);
        }
        let mut original = Vec::new();
        let mut absent = Vec::new();
        for (_, (key, targets)) in commands {
            let receipt = self
                .inner
                .runtime_db
                .control_receipt_for_recovery(&key)?
                .context("unfinished Control obligation has no valid original receipt")?;
            ensure!(
                targets
                    .iter()
                    .all(|target| receipt.obligation_ids.contains(&target.obligation_id)),
                "unfinished Control target differs from its original receipt"
            );
            if let Some((owner_epoch, facts)) = self
                .inner
                .control_cancellations
                .prepared_for_receipt(&key, &receipt.command_digest)?
            {
                original.push(super::state_mutation::SavedControlCancellation {
                    key,
                    digest: receipt.command_digest,
                    owner_epoch,
                    facts,
                });
            } else {
                absent.extend(targets.into_iter().map(|target| (key.clone(), target)));
            }
        }
        // Original completion workers may acquire the owner. Missing-plan
        // targets also acquire it themselves; never recurse while holding it.
        drop(reservations);
        for saved in original {
            self.resume_saved_control_cancellation(&saved)?;
        }
        for (key, target) in absent {
            self.reconcile_absent_control_target(&key, &target, None)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn reconcile_absent_control_target(
        &self,
        key: &ControlKey,
        target: &ControlCancellationTarget,
        registration: Option<&Arc<ControlRegistrationRecord>>,
    ) -> Result<ControlCanonicalDisposition> {
        // Never hold an owner/AgentState mutex while waiting for a registration
        // or joining its exact original monitor.
        let proof = match registration {
            Some(record) => record.proof(key, target).await?,
            None => None,
        };
        let _reservations = self.reserve_control_obligation_scope(key).await?;
        settle_proven_target(&self.inner.runtime_db, key, target, proof, self.now())
    }
    pub(super) fn handoff_control_worktree(
        &self,
        key: &ControlKey,
        target: &ControlCancellationTarget,
        reason: &str,
    ) -> Result<ControlCanonicalDisposition> {
        handoff(&self.inner.runtime_db, key, target, self.now(), reason)
    }
}

/// Read overlay only. Old completion evidence can never rewrite a same-ID
/// replacement Task or revive historical mutable state.
#[cfg(test)]
fn overlay_control_worktree_evidence(
    db: &RuntimeDb,
    task: &TaskRecord,
    incarnation: u64,
) -> Result<TaskRecord> {
    if !matches!(
        task.status,
        TaskStatus::Interrupted
            | TaskStatus::Cancelled
            | TaskStatus::Completed
            | TaskStatus::Failed
    ) {
        return Ok(task.clone());
    }
    let mut connection = db.connection()?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
    let candidates = {
        let mut statement=tx.prepare("SELECT target_json,completion_evidence_json FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND target_kind='worktree' AND target_identity=?3 AND state='completed' ORDER BY updated_at,command_id")?;
        let rows = statement.query_map(
            rusqlite::params![task.agent_id, i64::try_from(incarnation)?, task.id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut observed = task.clone();
    for (target, evidence) in candidates {
        let target: ControlCancellationTarget = serde_json::from_str(&target)?;
        if target.target_kind != ControlCancellationKind::Worktree
            || target.target_identity != task.id
            || target.target_generation.get("created_at") != Some(&json!(task.created_at))
            || !super::task_generation::control_terminal_rejoin_matches(
                task,
                &target.target_generation,
            )
        {
            continue;
        }
        let original = target.target_generation.get("artifact");
        if original
            != task
                .detail
                .as_ref()
                .and_then(|detail| detail.get("worktree"))
        {
            continue;
        }
        let evidence: Value = serde_json::from_str(&evidence)?;
        let Some(worktree) = evidence
            .get("detail")
            .and_then(|detail| detail.get("worktree"))
        else {
            continue;
        };
        // Only expose cleanup's own status evidence, never arbitrary old Task
        // detail/lineage or another worktree path/branch.
        let Some(detail) = observed.detail.as_mut().and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(current) = detail.get_mut("worktree").and_then(Value::as_object_mut) else {
            continue;
        };
        if let Some(saved) = worktree.as_object() {
            for name in [
                "cleanup_status",
                "cleanup_reason",
                "cleanup_error",
                "auto_cleaned_up",
                "changed_files",
                "retained_for_review",
                "actual_branch",
                "branch_cleanup_status",
                "branch_cleanup_error",
            ] {
                if let Some(value) = saved.get(name) {
                    current.insert(name.into(), value.clone());
                }
            }
        }
    }
    tx.commit()?;
    Ok(observed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{tasks::prepare_lifecycle_stop_task_result, tests::support::context_config},
        runtime_db::control_transition::{
            prepare_control, ControlCommit, ControlInvocation, ControlPlanInput, ControlSource,
        },
        types::{AuthorityClass, ControlAction},
    };
    use tempfile::{tempdir, TempDir};

    struct Fixture {
        _home: TempDir,
        _workspace: TempDir,
        runtime: RuntimeHandle,
        task: TaskRecord,
        key: ControlKey,
        target: ControlCancellationTarget,
        ledger: ControlRegistrationLedger,
        time: DateTime<Utc>,
    }
    impl Fixture {
        fn before_stop(kind: TaskKind) -> Self {
            let home = tempdir().unwrap();
            let workspace = tempdir().unwrap();
            let runtime = RuntimeHandle::new(
                "default",
                home.path().into(),
                workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),
                Arc::new(crate::provider::StubProvider::new("unused")),
                "default".into(),
                context_config(),
            )
            .unwrap();
            let time = runtime.now();
            let task = TaskRecord {
                id: "original-task".into(),
                agent_id: "default".into(),
                kind,
                status: TaskStatus::Running,
                created_at: time,
                updated_at: time,
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: Some(if kind == TaskKind::WorktreeSubagentTask {
                    json!({"child_agent_id":"original-child","worktree":{"worktree_path":"/original/task-tree","worktree_branch":"original-branch"}})
                } else {
                    json!({})
                }),
                recovery: None,
            };
            let ledger = ControlRegistrationLedger::default();
            let key = ControlKey {
                agent_id: "default".into(),
                identity_incarnation: runtime.inner.identity_incarnation,
                command_id: "original-stop".into(),
            };
            let target = ControlCancellationTarget {
                obligation_id: "original-task-obligation".into(),
                target_kind: ControlCancellationKind::Task,
                target_identity: task.id.clone(),
                target_generation: json!({"created_at":task.created_at,"rejoin":task.rejoin_fence().ok(),"owner_epoch":1,"runtime_lease_id":null}),
                target: json!({"kind":task.kind,"recovery":task.recovery,"work_item_id":task.effective_work_item_id()}),
            };
            Self {
                _home: home,
                _workspace: workspace,
                runtime,
                task,
                key,
                target,
                ledger,
                time,
            }
        }
        fn stop_command(&self) -> crate::runtime_db::control_transition::PreparedControl {
            let mut cancellation_targets = vec![self.target.clone()];
            if self.task.is_worktree_child_agent_task() {
                let mut generation = self.target.target_generation.clone();
                generation["artifact"] = self.task.detail.as_ref().unwrap()["worktree"].clone();
                cancellation_targets.push(ControlCancellationTarget {
                    obligation_id: "original-worktree-obligation".into(),
                    target_kind: ControlCancellationKind::Worktree,
                    target_identity: self.task.id.clone(),
                    target_generation: generation,
                    target: json!({"reason":"agent_lifecycle_stop"}),
                });
            }
            let snapshot = self
                .runtime
                .inner
                .runtime_db
                .load_control_snapshot(&self.key)
                .unwrap();
            prepare_control(ControlPlanInput {
                key: self.key.clone(),
                invocation: ControlInvocation {
                    action: ControlAction::Stop,
                    source: ControlSource::DirectApi,
                    authority_class: AuthorityClass::OperatorInstruction,
                },
                snapshot,
                prepared_at: self.time,
                task_results: vec![
                    prepare_lifecycle_stop_task_result(&self.task, self.time).unwrap()
                ],
                healthy_running: None,
                protected_control_attempt: None,
                protected_control_run_id: None,
                cancellation_targets,
                audit_events: vec![],
                index_changes: vec![],
            })
            .unwrap()
        }
        fn stop(kind: TaskKind) -> Self {
            let fixture = Self::before_stop(kind);
            let registration = fixture.ledger.before_active_task_sql(
                &fixture.task,
                fixture.runtime.inner.identity_incarnation,
                1,
            );
            fixture.db().tasks().upsert(&fixture.task).unwrap();
            drop(registration);
            let command = fixture.stop_command();
            assert!(matches!(
                fixture.db().commit_control(&command).unwrap(),
                ControlCommit::Applied { .. }
            ));
            fixture
        }
        async fn stop_published(kind: TaskKind) -> Self {
            let fixture = Self::before_stop(kind);
            fixture
                .runtime
                .persist_task_transition(&fixture.task, "original_overlay_task_fixture")
                .await
                .unwrap();
            let mut mutation = fixture.runtime.begin_mutation().await.unwrap();
            let command = fixture.stop_command();
            let work_request = mutation.owner_work_load_request().unwrap();
            let (task_request, _selectors) = mutation.owner_task_load_request().unwrap();
            let db = fixture.db().clone();
            let ledger = fixture.runtime.inner.input_evidence_budget.clone();
            // This storage/publication fixture supplies a retained observation;
            // no actual cancellation or Git worker is registered by its setup.
            let commit = mutation
                .commit_transition(
                    "original-stop-publication-fixture".into(),
                    move |_| match db.commit_control_with_domain_requests(
                        &command,
                        &work_request,
                        &task_request,
                        &ledger,
                    )? {
                        ControlCommit::Applied { delta, .. } => Ok(delta.transition),
                        ControlCommit::Replayed { .. } => {
                            anyhow::bail!("first fixture Stop unexpectedly replayed")
                        }
                    },
                )
                .await
                .unwrap();
            fixture.runtime.apply_transition_commit(commit).await;
            drop(mutation);
            fixture
        }
        fn db(&self) -> &RuntimeDb {
            &self.runtime.inner.runtime_db
        }
    }

    #[tokio::test]
    async fn explicit_never_spawned_registration_and_exact_terminal_task_complete_original_only() {
        let f = Fixture::stop(TaskKind::AgentMessageWait);
        let record = f.ledger.for_task(&f.task).unwrap();
        let proof = record.proof(&f.key, &f.target).await.unwrap();
        assert!(proof.is_some());
        let before = f.db().agent_states().latest(&f.key.agent_id).unwrap();
        assert_eq!(
            settle_proven_target(f.db(), &f.key, &f.target, proof, f.time).unwrap(),
            ControlCanonicalDisposition::Completed
        );
        assert_eq!(
            f.db().agent_states().latest(&f.key.agent_id).unwrap(),
            before
        );
    }
    #[tokio::test]
    async fn missing_lease_is_durable_attention_not_joined() {
        let f = Fixture::stop(TaskKind::AgentMessageWait);
        assert_eq!(
            settle_proven_target(f.db(), &f.key, &f.target, None, f.time).unwrap(),
            ControlCanonicalDisposition::DurablyHandedOff
        );
        let saved = f
            .db()
            .control_obligation(&f.key, &f.target)
            .unwrap()
            .unwrap();
        assert_eq!(saved.state, ControlObligationState::NeedsAttention);
        assert_eq!(
            saved.completion_evidence.as_ref().unwrap()["physical_completion_proven"],
            false
        );
        assert_eq!(
            settle_proven_target(f.db(), &f.key, &f.target, None, f.time).unwrap(),
            ControlCanonicalDisposition::DurablyHandedOff
        );
        assert_eq!(
            f.db().control_obligation(&f.key, &f.target).unwrap(),
            Some(saved)
        );
    }
    #[tokio::test]
    async fn same_id_replacement_cannot_use_old_registration_proof() {
        let f = Fixture::stop(TaskKind::AgentMessageWait);
        let record = f.ledger.for_task(&f.task).unwrap();
        let mut replacement = f.task.clone();
        replacement.created_at += chrono::Duration::seconds(1);
        replacement.updated_at = replacement.created_at;
        // Deliberately replace the generation as a corruption fixture; the
        // production repository correctly rejects Interrupted -> Running.
        f.db().connection().unwrap().execute(
            "UPDATE tasks SET status='running', revision=revision+1, created_at=?1, updated_at=?1, completed_at=NULL, payload_json=?2 WHERE task_id=?3",
            rusqlite::params![replacement.created_at.to_rfc3339(), serde_json::to_string(&crate::runtime_db::repositories::slim_task_record_for_payload(&replacement)).unwrap(), replacement.id],
        ).unwrap();
        assert_eq!(
            settle_proven_target(
                f.db(),
                &f.key,
                &f.target,
                record.proof(&f.key, &f.target).await.unwrap(),
                f.time
            )
            .unwrap(),
            ControlCanonicalDisposition::DurablyHandedOff
        );
        assert_eq!(
            f.db().tasks().latest(&f.task.id).unwrap(),
            Some(replacement)
        );
    }
    #[tokio::test]
    async fn preparing_registration_is_not_missing_and_cannot_be_joined_early() {
        let f = Fixture::stop(TaskKind::AgentMessageWait);
        let ledger = ControlRegistrationLedger::default();
        let ticket = ledger.before_active_task_sql(&f.task, f.key.identity_incarnation, 1);
        let record = ledger.for_task(&f.task).unwrap();
        let mut proof = Box::pin(record.proof(&f.key, &f.target));
        assert!(futures_util::poll!(proof.as_mut()).is_pending());
        drop(ticket);
        assert!(proof.await.unwrap().is_some());
    }
    #[tokio::test]
    async fn startup_process_proof_does_not_claim_an_unknown_command_os_process_joined() {
        let f = Fixture::stop(TaskKind::CommandTask);
        let lock = RuntimeDbLock::try_lock(f._home.path().join("serve.lock")).unwrap();
        let handoff = tokio::sync::Mutex::new(());
        let guard = handoff.lock().await;
        let proof =
            ControlStartupProof::for_pre_activation_host(f.db(), &lock, lock.path(), &guard)
                .unwrap();
        let observed = reconcile_control_scope_at_startup(
            f.db(),
            &f.key.agent_id,
            f.key.identity_incarnation,
            &proof,
            f.time,
        )
        .unwrap();
        assert_eq!(observed[0].2, ControlCanonicalDisposition::DurablyHandedOff);
        assert_eq!(
            f.db()
                .control_obligation(&f.key, &f.target)
                .unwrap()
                .unwrap()
                .state,
            ControlObligationState::NeedsAttention
        );
    }
    #[tokio::test]
    async fn startup_process_proof_can_settle_an_original_pure_runtime_monitor() {
        let f = Fixture::stop(TaskKind::AgentMessageWait);
        let lock = RuntimeDbLock::try_lock(f._home.path().join("serve.lock")).unwrap();
        let handoff = tokio::sync::Mutex::new(());
        let guard = handoff.lock().await;
        let proof =
            ControlStartupProof::for_pre_activation_host(f.db(), &lock, lock.path(), &guard)
                .unwrap();
        let before = f.db().tasks().latest(&f.task.id).unwrap();
        let observed = reconcile_control_scope_at_startup(
            f.db(),
            &f.key.agent_id,
            f.key.identity_incarnation,
            &proof,
            f.time,
        )
        .unwrap();
        assert_eq!(observed[0].2, ControlCanonicalDisposition::Completed);
        assert_eq!(f.db().tasks().latest(&f.task.id).unwrap(), before);
    }
    #[tokio::test]
    async fn retired_original_scope_can_handoff_without_installing_state() {
        let f = Fixture::stop(TaskKind::CommandTask);
        let before_state = f.db().agent_states().latest("default").unwrap();
        let identity = f
            .db()
            .agent_identities()
            .tombstone_with_closed_supervision("default")
            .unwrap();
        let mut reservations = super::super::mutation_owner::MutationOwner::reserve_all(vec![f
            .runtime
            .inner
            .mutation_owner
            .clone()])
        .await
        .unwrap();
        super::super::mutation_owner::MutationReservation::begin_all(
            &mut reservations,
            "retired-metadata-publication".into(),
        )
        .unwrap();
        super::super::mutation_owner::MutationReservation::committed_all(
            &mut reservations,
            vec![super::super::mutation_owner::KnownCommit::Metadata(
                Arc::new(super::super::mutation_owner::IdentityMutationFacts {
                    _identity_budget: None,
                    identity: Some(Arc::new(identity.clone())),
                    state: None,
                }),
            )],
        )
        .unwrap();
        super::super::mutation_owner::MutationReservation::publish_all_known(&mut reservations)
            .unwrap();
        drop(reservations);
        assert!(f.runtime.inner.mutation_owner.capture().unwrap().is_none());
        assert!(f.runtime.begin_mutation().await.is_err());
        assert_eq!(
            f.runtime
                .reconcile_absent_control_target(&f.key, &f.target, None)
                .await
                .unwrap(),
            ControlCanonicalDisposition::DurablyHandedOff
        );
        assert!(f.runtime.inner.mutation_owner.capture().unwrap().is_none());
        assert_eq!(
            f.db().agent_states().latest("default").unwrap(),
            before_state
        );
        assert_eq!(
            f.db().agent_identities().latest("default").unwrap(),
            Some(identity)
        );
        assert_eq!(
            f.db()
                .control_obligation(&f.key, &f.target)
                .unwrap()
                .unwrap()
                .state,
            ControlObligationState::NeedsAttention
        );
    }

    #[tokio::test]
    async fn original_obligation_cas_does_not_open_an_unknown_owner() {
        let f = Fixture::stop(TaskKind::CommandTask);
        let mut reservation = f.runtime.inner.mutation_owner.reserve().await.unwrap();
        let original = reservation
            .begin("unresolved-later-state-operation".into())
            .unwrap();
        drop(reservation);
        assert!(f
            .runtime
            .reconcile_absent_control_target(&f.key, &f.target, None)
            .await
            .is_err());
        assert_eq!(
            f.db()
                .control_obligation(&f.key, &f.target)
                .unwrap()
                .unwrap()
                .state,
            ControlObligationState::Pending
        );
        assert!(matches!(f.runtime.inner.mutation_owner.capture(),
            Err(super::super::mutation_owner::MutationOwnerError::NeedsReconcile(key)) if key == original));
    }

    #[tokio::test]
    async fn loaded_without_original_plan_hands_off_exact_targets_without_replaying_state() {
        let f = Fixture::stop(TaskKind::CommandTask);
        let durable = f.db().agent_states().latest("default").unwrap();
        let live = f.runtime.inner.agent.lock().await.state.clone();
        let task = f.db().tasks().latest(&f.task.id).unwrap();
        let receipt = f.db().control_receipt_for_recovery(&f.key).unwrap();
        f.runtime
            .reconcile_loaded_control_obligations()
            .await
            .unwrap();
        let saved = f
            .db()
            .control_obligation(&f.key, &f.target)
            .unwrap()
            .unwrap();
        assert_eq!(saved.state, ControlObligationState::NeedsAttention);
        assert_eq!(
            saved.completion_evidence.as_ref().unwrap()["physical_completion_proven"],
            false
        );
        assert_eq!(f.db().agent_states().latest("default").unwrap(), durable);
        assert_eq!(f.runtime.inner.agent.lock().await.state, live);
        assert_eq!(f.db().tasks().latest(&f.task.id).unwrap(), task);
        assert_eq!(
            f.db().control_receipt_for_recovery(&f.key).unwrap(),
            receipt
        );
        f.runtime
            .reconcile_loaded_control_obligations()
            .await
            .unwrap();
        assert_eq!(
            f.db().control_obligation(&f.key, &f.target).unwrap(),
            Some(saved)
        );
    }

    #[tokio::test]
    async fn loaded_recovery_resumes_the_original_run_plan_instead_of_claiming_absence() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().into(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        runtime
            .enqueue(crate::types::MessageEnvelope::new(
                "default",
                crate::types::MessageKind::OperatorPrompt,
                crate::types::MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                crate::types::Priority::Normal,
                crate::types::MessageBody::Text {
                    text: "original run".into(),
                },
            ))
            .await
            .unwrap();
        let super::super::scheduler_executor::RunLoopPoll::Message(scheduled) =
            super::super::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
                .unwrap()
        else {
            panic!("expected original canonical claim")
        };
        let (key, invocation) =
            runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::OperatorInstruction);
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let prepared = runtime
            .prepare_runtime_control(&mutation, key.clone(), invocation)
            .await
            .unwrap();
        let target = prepared
            .command
            .cancellation_targets()
            .iter()
            .find(|target| target.target_kind == ControlCancellationKind::Run)
            .unwrap()
            .clone();
        let digest = prepared.command.command_digest().to_string();
        mutation.commit_control(&prepared).await.unwrap();
        drop(mutation);
        let original = runtime
            .inner
            .control_cancellations
            .prepared_for_receipt(&key, &digest)
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            runtime.reconcile_loaded_control_obligations(),
        )
        .await
        .unwrap()
        .unwrap();
        let resumed = runtime
            .inner
            .control_cancellations
            .prepared_for_receipt(&key, &digest)
            .unwrap()
            .unwrap();
        assert_eq!(original.0, resumed.0);
        assert!(Arc::ptr_eq(&original.1, &resumed.1));
        let pending = runtime
            .inner
            .runtime_db
            .control_obligation(&key, &target)
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, ControlObligationState::Pending);
        assert!(pending.completion_evidence.is_none());
        // Actual processing completion, not clearing State.current_run_id,
        // is the original Run lease's completion proof.
        drop(scheduled);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            runtime.inner.control_cancellations.drain(),
        )
        .await
        .unwrap()
        .unwrap();
        let completed = runtime
            .inner
            .runtime_db
            .control_obligation(&key, &target)
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, ControlObligationState::Completed);
        assert_eq!(
            completed.completion_evidence.as_ref().unwrap()["joined"],
            true
        );
    }

    #[tokio::test]
    async fn loaded_obligation_recovery_keeps_an_unrelated_unknown_attempt_fenced() {
        let f = Fixture::stop(TaskKind::CommandTask);
        let mut reservation = f.runtime.inner.mutation_owner.reserve().await.unwrap();
        let original = reservation
            .begin("later-unknown-before-loaded-recovery".into())
            .unwrap();
        drop(reservation);
        assert!(f
            .runtime
            .reconcile_loaded_control_obligations()
            .await
            .is_err());
        assert_eq!(
            f.db()
                .control_obligation(&f.key, &f.target)
                .unwrap()
                .unwrap()
                .state,
            ControlObligationState::Pending
        );
        assert!(matches!(f.runtime.inner.mutation_owner.capture(),
            Err(super::super::mutation_owner::MutationOwnerError::NeedsReconcile(key)) if key == original));
    }

    #[tokio::test]
    async fn worktree_cleanup_evidence_overlays_only_the_exact_current_terminal_generation() {
        let f = Fixture::stop_published(TaskKind::WorktreeSubagentTask).await;
        let target = f
            .db()
            .pending_control_obligations(&f.key.agent_id, f.key.identity_incarnation)
            .unwrap()
            .into_iter()
            .find(|(_, target)| target.target_kind == ControlCancellationKind::Worktree)
            .unwrap()
            .1;
        let terminal = f.db().tasks().latest(&f.task.id).unwrap().unwrap();
        let mut detail = terminal.detail.clone().unwrap();
        detail["worktree"]["cleanup_status"] = json!("retained");
        detail["worktree"]["retained_for_review"] = json!(true);
        // This is exact completed cleanup evidence, not a synthetic Task write.
        let ledger = &f.runtime.inner.input_evidence_budget;
        let limits = ledger.limits();
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit).unwrap().min(65_536),
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .unwrap(),
            )
            .unwrap()
            .min(64 * 1024 * 1024),
        };
        let scope = super::super::resident_input_evidence::InputEvidenceScope {
            agent_id: f.key.agent_id.clone(),
            identity_incarnation: f.key.identity_incarnation,
        };
        let completion = Arc::new(
            ledger
                .reserve_owned_completion(
                    &scope,
                    super::super::input_evidence_budget::InputMemoryCategory::Candidate,
                    super::super::input_evidence_budget::OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: 1024,
                    },
                )
                .unwrap(),
        );
        let mut capsule =
            super::super::task_status_metadata::OriginalControlMetadataCapsule::completed(
                scope,
                &f.key,
                &target,
                json!({"cleanup":"retained","detail":detail}),
                f.time,
                completion,
            );
        f.runtime
            .commit_control_worktree_metadata(&mut capsule, &request)
            .await
            .unwrap();
        let overlay =
            overlay_control_worktree_evidence(f.db(), &terminal, f.key.identity_incarnation)
                .unwrap();
        assert_eq!(
            overlay.detail.as_ref().unwrap()["worktree"]["cleanup_status"],
            "retained"
        );
        assert_eq!(
            f.db().tasks().latest(&f.task.id).unwrap(),
            Some(terminal.clone())
        );
        let status = f.runtime.task_status_snapshot(&f.task.id).await.unwrap();
        assert_eq!(
            status.child_supervision.unwrap().cleanup_status.as_deref(),
            Some("retained")
        );
        let output = f.runtime.task_output(&f.task.id, false, 0).await.unwrap();
        assert_eq!(
            output
                .task
                .child_supervision
                .unwrap()
                .cleanup_status
                .as_deref(),
            Some("retained")
        );
        assert_eq!(
            f.runtime.task_record(&f.task.id).await.unwrap(),
            Some(terminal.clone())
        );
        let mut successor = terminal.clone();
        successor.created_at += chrono::Duration::seconds(1);
        assert_eq!(
            overlay_control_worktree_evidence(f.db(), &successor, f.key.identity_incarnation)
                .unwrap(),
            successor
        );
        let mut active = terminal;
        active.status = TaskStatus::Running;
        assert_eq!(
            overlay_control_worktree_evidence(f.db(), &active, f.key.identity_incarnation).unwrap(),
            active
        );
    }

    #[tokio::test]
    async fn original_worktree_completion_records_only_cleanup_headers_and_replay_preserves_them() {
        let f = Fixture::stop(TaskKind::WorktreeSubagentTask);
        let target = f
            .db()
            .pending_control_obligations(&f.key.agent_id, f.key.identity_incarnation)
            .unwrap()
            .into_iter()
            .find(|(_, target)| target.target_kind == ControlCancellationKind::Worktree)
            .unwrap()
            .1;
        let original = f.db().tasks().latest(&f.task.id).unwrap().unwrap();
        let evidence = json!({"detail":{"worktree":{"path":"/cannot-overlay","branch":"cannot-overlay",
            "cleanup_status":"retained","retained_for_review":true,"auto_cleaned_up":false,
            "changed_files":["src/a.rs"],"cleanup_error":null}}});
        f.db()
            .complete_control_obligation(&f.key, &target, f.time, evidence)
            .unwrap();
        let read = || {
            f.db().connection().unwrap().query_row(
            "SELECT target_generation_json,fields_json FROM task_status_control_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3 AND obligation_id=?4",
            rusqlite::params![f.key.agent_id,f.key.identity_incarnation,f.key.command_id,target.obligation_id],
            |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).unwrap()
        };
        let saved = read();
        assert_eq!(
            serde_json::from_str::<Value>(&saved.0).unwrap(),
            target.target_generation
        );
        let fields: Value = serde_json::from_str(&saved.1).unwrap();
        assert_eq!(
            fields,
            json!({"cleanup_status":"retained","retained_for_review":true,
            "auto_cleaned_up":false,"changed_files":["src/a.rs"],"cleanup_error":null})
        );
        f.db()
            .complete_control_obligation(
                &f.key,
                &target,
                f.time + chrono::Duration::seconds(1),
                json!({"detail":{"worktree":{"cleanup_status":"replacement"}}}),
            )
            .unwrap();
        assert_eq!(
            read(),
            saved,
            "completed replay cannot replace accepted source headers"
        );
        let count: u64 = f
            .db()
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM task_status_control_sources",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            f.db().tasks().latest(&f.task.id).unwrap().unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn cleanup_header_failure_rolls_back_the_actual_control_completion() {
        let f = Fixture::stop(TaskKind::WorktreeSubagentTask);
        let target = f
            .db()
            .pending_control_obligations(&f.key.agent_id, f.key.identity_incarnation)
            .unwrap()
            .into_iter()
            .find(|(_, target)| target.target_kind == ControlCancellationKind::Worktree)
            .unwrap()
            .1;
        f.db().connection().unwrap().execute_batch(
            "CREATE TRIGGER fail_control_cleanup_header BEFORE INSERT ON task_status_control_sources BEGIN SELECT RAISE(ABORT,'cleanup metadata failure'); END;").unwrap();
        let error = f
            .db()
            .complete_control_obligation(
                &f.key,
                &target,
                f.time,
                json!({"detail":{"worktree":{"cleanup_status":"retained"}}}),
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("cleanup metadata failure"));
        let saved = f.db().control_obligation(&f.key, &target).unwrap().unwrap();
        assert_eq!(saved.state, ControlObligationState::Pending);
        assert!(saved.completion_evidence.is_none());
        let count: u64 = f
            .db()
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM task_status_control_sources",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
