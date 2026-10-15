//! Named full/historical Task output acquisition under the existing owner.
use super::*;
use crate::runtime::{
    resident_task::{TaskFamily, TaskKey, TaskReadError},
    task_queries::TaskQueryLease,
};

impl RuntimeMutation {
    #[inline(never)]
    pub(in crate::runtime) fn prepare_task_output_inputs<'a>(
        &'a mut self,
        role: &'a TaskQueryLease,
        id: &'a str,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.prepare_task_output_inputs_inner(role, id))
    }
    async fn prepare_task_output_inputs_inner(
        &mut self,
        role: &TaskQueryLease,
        id: &str,
    ) -> Result<()> {
        let root = self.capture_published_root()?;
        let domain = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            domain.scope() == &self.input_evidence_scope(),
            "Output acquire original Task scope differs"
        );
        self.runtime.inner.task_queries.validate_lease(
            role,
            domain.scope(),
            root.token.owner_epoch,
        )?;
        anyhow::ensure!(
            role.selection().output_task_ids.contains(id),
            "Output acquire requires its actual live query role"
        );
        let capture = domain.capture_query(role.selection())?;
        let task = capture
            .lookup(&TaskKey::new(TaskFamily::Task, id))?
            .context("Output target Task absent")?;
        let source = capture.output_source(id)?;
        let (request, _selectors) = self.owner_task_load_request()?;
        let recall = self.runtime.inner.runtime_db.transaction(|tx| {
            crate::runtime_db::task_snapshot::prepare_task_output_input_recall_tx(
                tx,
                &root.state,
                domain.coverage().revision,
                task,
                source,
                &request,
                &self.runtime.inner.input_evidence_budget,
            )
        })?;
        let inputs =
            super::super::resident_input_evidence::ResidentInputEvidence::with_task_output_recall(
                root.input_evidence.as_deref(),
                recall,
                self.runtime.inner.input_evidence_budget.clone(),
            )?;
        // Read-only publication changes no Task/State/business receipt. Save the
        // exact same-view installation facts before any fallible runtime await.
        self.input_evidence = Some(Arc::new(inputs));
        let facts = Arc::new(self.facts(Default::default())?);
        let reservation = &mut self.reservations[self.primary_index];
        reservation.begin(crate::ids::audit_event_id())?;
        MutationReservation::committed_all(
            std::slice::from_mut(reservation),
            vec![KnownCommit::State {
                current_reads: facts.current_reads.clone(),
                state: facts.state.clone(),
                episodes: facts.episodes.clone(),
                input_evidence: facts.input_evidence.clone(),
                work: facts.work.clone(),
                tasks: facts.tasks.clone(),
                scheduling: facts.scheduling.clone(),
                runtime: Some(facts.clone()),
            }],
        )?;
        drop((root, capture));
        self.runtime.install_mutation_facts(&facts).await?;
        let observations = MutationReservation::publish_all_known(std::slice::from_mut(
            &mut self.reservations[self.primary_index],
        ))?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.base = self.candidate.clone();
        self.base_work = self.work.clone();
        Ok(())
    }
}
