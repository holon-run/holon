//! A focused progress carries only its authentic frozen Worktree Audit list.
use super::business_writer::CurrentReadBusinessTransaction;
use super::*;
use crate::runtime_db::task_stop::{
    ActualTaskStopProgress, OriginalTaskStopAuditView, TaskStopPhaseInstallMode,
};

pub(in crate::runtime_db) struct OriginalStopCurrentEvents<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    source: OriginalTaskStopAuditView<'tx, 'conn>,
    appended: bool,
    completed: bool,
    projection: StopEventProjection<'tx, 'conn>,
}
enum StopEventProjection<'tx, 'conn> {
    State(CurrentReadBusinessTransaction<'tx, 'conn>, AgentState),
    Metadata(
        writes::CurrentReadWriteProof<'tx, 'conn>,
        Vec<writes::CurrentReadActualWrite<'tx, 'conn>>,
    ),
}
struct EventIds<'a>(&'a [AuditEvent]);
impl serde::Serialize for EventIds<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for event in self.0 {
            seq.serialize_element(&event.id)?
        }
        seq.end()
    }
}
impl<'tx, 'conn> OriginalStopCurrentEvents<'tx, 'conn> {
    pub(in crate::runtime_db) fn prepare_tx(
        tx: &'tx Transaction<'conn>,
        source: OriginalTaskStopAuditView<'tx, 'conn>,
        mode: TaskStopPhaseInstallMode,
        ledger: &InputEvidenceBudgetLedger,
        event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    ) -> Result<Self> {
        source.validate_tx(tx)?;
        let scope = source.scope();
        let limits = ledger.limits();
        let mut request = CurrentReadRequest::current(
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Stop Audit Current allowance overflow")?,
            )?,
            usize::try_from(limits.record_limit)?,
        );
        // Borrowed sizing precedes the first owned key/set construction.
        let role_prior = if let Some(roles) = event_roles {
            let prior = roles.prepare_request(scope, ledger, &mut request)?;
            // Metadata-only original Stop modes keep their prior permissions;
            // a live consumer cannot manufacture State or Event body coverage.
            if mode != TaskStopPhaseInstallMode::Current {
                request.event_birth_after = None;
            }
            prior
        } else {
            (0, 0)
        };
        let mut material = Material::new(scope, &request, ledger);
        material.share_existing(role_prior.0, role_prior.1)?;
        material.charge(
            usize::try_from(serialized_bytes(&EventIds(source.events()))?)?,
            source.events().len() + 8,
        )?;
        let keys = source
            .events()
            .iter()
            .map(|event| CurrentReadKey::event(&event.id))
            .collect::<BTreeSet<_>>();
        ensure!(
            keys.len() == source.events().len(),
            "Stop Audit source duplicates an original event"
        );
        let prior = (material.bytes, material.records);
        let guards = material.guards;
        let projection = match mode {
            TaskStopPhaseInstallMode::Current => {
                let (current, state) =
                    CurrentReadBusinessTransaction::prepare_actual_original_maintenance_tx(
                        tx, scope, &keys, &request, ledger, prior, guards,
                    )?;
                StopEventProjection::State(current, state)
            }
            TaskStopPhaseInstallMode::NoState
            | TaskStopPhaseInstallMode::Retired
            | TaskStopPhaseInstallMode::Incomplete => {
                let proof = writes::prepare_metadata_stop_events_tx(
                    tx, &source, &keys, &request, ledger, prior, guards,
                )?;
                StopEventProjection::Metadata(proof, Vec::new())
            }
        };
        Ok(Self {
            tx,
            source,
            projection,
            appended: false,
            completed: false,
        })
    }
    pub(in crate::runtime_db) fn state(&self) -> Option<&AgentState> {
        match &self.projection {
            StopEventProjection::State(_, state) => Some(state),
            _ => None,
        }
    }
    pub(in crate::runtime_db) fn append_actual_tx(
        &mut self,
        actual: &ActualTaskStopProgress<'_, '_>,
    ) -> Result<Vec<AuditEvent>> {
        self.source.validate_actual_tx(self.tx, actual)?;
        ensure!(
            !self.appended,
            "Stop Audit source already consumed its append"
        );
        self.appended = true;
        if !actual.changed() {
            self.completed = true;
            return Ok(Vec::new());
        }
        let mut events = Vec::with_capacity(self.source.events().len());
        for event in self.source.events() {
            match &mut self.projection {
                StopEventProjection::State(current, _) => {
                    events.push(
                        current
                            .append_event_tx(self.tx, Some(&self.source.scope().agent_id), event)?
                            .0,
                    );
                }
                StopEventProjection::Metadata(proof, writes) => {
                    let (event, write) = writes::append_current_read_event_tx(proof, event)?;
                    ensure!(
                        !writes.iter().any(|existing| existing.key() == write.key()),
                        "Stop Audit repeated actual append"
                    );
                    writes.push(write);
                    events.push(event);
                }
            }
        }
        self.completed = true;
        Ok(events)
    }
    pub(in crate::runtime_db) fn finish(self) -> Result<writes::CommittedCurrentReadMaintenance> {
        ensure!(
            self.completed,
            "Stop Audit finalizer requires its actual progress observation"
        );
        match self.projection {
            StopEventProjection::State(current, state) => {
                current.finish_original_maintenance(&state)
            }
            StopEventProjection::Metadata(proof, actual) => {
                writes::register_metadata_stop_events_tx(proof, &actual)
            }
        }
    }
}
