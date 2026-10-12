//! First typed family on the owner protocol. Only posture observations are
//! published here; legacy readers and writers still use RuntimeAgent.

use super::{
    mutation_owner::{MutationReservation, PublicationToken},
    *,
};
use crate::runtime_db::{persistence_certainty, PersistenceCertainty};

#[derive(Default)]
pub(super) struct OwnedMutationTasks {
    closed: bool,
    drain: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl OwnedMutationTasks {
    pub(super) fn all_finished(&self) -> bool {
        self.handles.iter().all(|task| task.is_finished())
            && self
                .drain
                .try_lock()
                .is_ok_and(|tasks| tasks.iter().all(|task| task.is_finished()))
    }
}

struct PreparedPostureMutation {
    command: crate::runtime_db::transitions::AgentPostureTransitionCommand,
    candidate: Arc<AgentState>,
}

impl PreparedPostureMutation {
    fn new(command: crate::runtime_db::transitions::AgentPostureTransitionCommand) -> Result<Self> {
        anyhow::ensure!(
            command.agent_id == command.agent_state.record.id,
            "posture request identity mismatch"
        );
        anyhow::ensure!(
            command.audit_events.len() == 1,
            "posture request requires one stable audit operation identity"
        );
        Ok(Self {
            candidate: Arc::new(command.agent_state.record.as_ref().clone()),
            command,
        })
    }
}

impl RuntimeHandle {
    pub(crate) fn spawn_owned_mutation(
        &self,
        mutation: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let mut tasks = self
            .inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned");
        if tasks.closed {
            return Err(super::execution_admission::closed_error());
        }
        tasks.handles.retain(|task| !task.is_finished());
        // Registration and closing share one gate. Shutdown cannot miss a task
        // spawned immediately after taking the previous handles to drain.
        tasks.handles.push(tokio::spawn(mutation));
        Ok(())
    }

    pub(super) fn close_mutation_admission(&self) {
        self.inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned")
            .closed = true;
    }

    pub(crate) async fn join_owned_mutations(&self) -> Result<()> {
        let drain = {
            let mut tasks = self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned");
            tasks.closed = true;
            tasks.drain.clone()
        };
        let mut draining = drain.lock().await;
        draining.extend(std::mem::take(
            &mut self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned")
                .handles,
        ));
        let mut failure = None;
        while let Some(task) = draining.last_mut() {
            // The supervisor retains the handle across await. Cancelling this
            // drainer leaves it for the next drainer rather than detaching it.
            let result = task.await;
            draining.pop();
            if let Err(error) = result {
                failure.get_or_insert_with(|| anyhow::Error::new(error));
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn submit_posture(
        &self,
        reservation: &mut MutationReservation,
        agent: &mut RuntimeAgent,
        command: crate::runtime_db::transitions::AgentPostureTransitionCommand,
    ) -> Result<TransitionCommit> {
        let prepared = PreparedPostureMutation::new(command)?;
        // Capture root and gate together. This root describes only this pilot;
        // it must not authorize ordinary context or scheduler decisions yet.
        if let Some(previous) = self.inner.mutation_owner.capture()? {
            tracing::trace!(agent_id = %previous.state.id,
                owner_epoch = previous.token.owner_epoch,
                publication_generation = previous.token.generation,
                "preparing posture observation");
        }
        let key = reservation.begin(prepared.command.audit_events[0].id.clone())?;
        let committed = match self
            .inner
            .runtime_db
            .transitions()
            .commit_agent_posture(&prepared.command)
        {
            Ok(committed) => committed,
            Err(error) => {
                if persistence_certainty(&error)
                    == Some(PersistenceCertainty::DefinitelyNotCommitted)
                {
                    reservation.definitely_not_committed()?;
                }
                // An unclassified/unknown error is conservatively fenced. The
                // original domain error remains available to OCC callers.
                return Err(error);
            }
        };
        reservation.committed(prepared.candidate.clone())?;
        if committed.effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            bail!(
                "agent posture committed but publication requires reconciliation: {}",
                key.operation_id
            );
        }
        // The caller temporarily retains the legacy agent lock across SQL.
        // Removing that compatibility barrier requires all whole-row writers
        // to join the reservation; otherwise Start/Stop could be overwritten.
        agent.state = prepared.candidate.as_ref().clone();
        agent.last_persisted_state = prepared.candidate.as_ref().clone();
        let observation = reservation.publish(prepared.candidate)?;
        trace_publication(observation.token);
        let mut notifications = committed;
        notifications.effects.agent_state = None;
        Ok(notifications)
    }

    pub(super) async fn reconcile_posture_observation(&self) -> Result<()> {
        let mut reservation = self.inner.mutation_owner.reserve().await?;
        let Some(recovery) = reservation.recovery()? else {
            return Ok(());
        };
        anyhow::ensure!(recovery.committed_state.is_some(),
            "posture commit outcome is unknown; no durable receipt proves the original operation {}", recovery.key.operation_id);
        // This is recovery, not a retry of the original business mutation. A
        // latest row can restore observations but cannot prove an unknown ACK.
        let mut agent = self.inner.agent.lock().await;
        let current = self
            .inner
            .runtime_db
            .agent_states()
            .latest(&recovery.key.agent_id)?
            .ok_or_else(|| anyhow!("posture recovery has no durable agent state"))?;
        anyhow::ensure!(
            agent.state == agent.last_persisted_state,
            "posture recovery cannot discard unpersisted runtime state"
        );
        anyhow::ensure!(
            agent
                .current_run_abort
                .as_ref()
                .is_none_or(|handle| current.current_run_id.as_ref() == Some(&handle.run_id)),
            "posture recovery conflicts with a live run handle"
        );
        agent.state = current.clone();
        agent.last_persisted_state = current.clone();
        let observation = reservation.reconcile(&recovery.key, Arc::new(current))?;
        tracing::warn!(agent_id = %recovery.key.agent_id,
            operation_id = %recovery.key.operation_id,
            phase = ?recovery.phase,
            original_outcome_known = recovery.committed_state.is_some(),
            "reconciled posture observation without replaying the original mutation");
        trace_publication(observation.token);
        Ok(())
    }
}

fn trace_publication(token: PublicationToken) {
    tracing::trace!(
        identity_incarnation = token.identity_incarnation,
        owner_epoch = token.owner_epoch,
        publication_generation = token.generation,
        "published committed posture observation"
    );
}

#[cfg(test)]
#[path = "state_mutation_tests.rs"]
mod tests;
