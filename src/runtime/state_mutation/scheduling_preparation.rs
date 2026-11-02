//! Explicit Scheduling/Input preparation under the existing owner reservation.
use super::*;
use crate::runtime::resident_scheduling::{
    CapturedScheduling, CommittedSchedulingDelta, SchedulingCaptureRequest, SchedulingReadError,
};

impl RuntimeMutation {
    pub(super) fn install_committed_scheduling_delta(
        &mut self,
        delta: &CommittedSchedulingDelta,
    ) -> Result<()> {
        let reservation = &self.reservations[self.primary_index];
        anyhow::ensure!(
            delta.scope.agent_id == reservation.agent_id()
                && delta.scope.identity_incarnation == reservation.identity_incarnation(),
            "committed Scheduling scope differs"
        );
        // An ACK cannot turn an unprepared domain into a complete publication.
        if let Some(domain) = &self.scheduling {
            self.scheduling = Some(Arc::new(domain.with_committed(delta)?));
        }
        Ok(())
    }

    pub(in crate::runtime) fn capture_scheduling_with_state(
        &self,
        request: &SchedulingCaptureRequest,
    ) -> Result<(
        super::super::mutation_owner::PublicationToken,
        Arc<AgentState>,
        CapturedScheduling,
    )> {
        let root = self.capture_published_root()?;
        let domain = root
            .scheduling
            .as_ref()
            .ok_or(SchedulingReadError::IncompleteLive)?;
        let reservation = &self.reservations[self.primary_index];
        anyhow::ensure!(
            domain.scope().agent_id == reservation.agent_id()
                && domain.scope().identity_incarnation == reservation.identity_incarnation()
                && root.state.id == domain.scope().agent_id,
            "Scheduling publication scope differs"
        );
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
            &self.runtime.inner.input_evidence_budget,
            domain.scope(),
        )?;
        let _resource_io = material.enter();
        Ok((root.token, root.state.clone(), domain.capture(request)?))
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_owner_scheduling_request<'a>(
        &'a mut self,
        capture: &'a SchedulingCaptureRequest,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let reservation = &self.reservations[self.primary_index];
            let material =
                crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new_for_identity(
                    &self.runtime.inner.input_evidence_budget,
                    reservation.agent_id(),
                    reservation.identity_incarnation(),
                )?;
            self.prepare_owner_scheduling_request_inner(capture, &material)
                .await
        })
    }

    #[inline(never)]
    pub(in crate::runtime) fn prepare_owner_scheduling_request_with_material<'a>(
        &'a mut self,
        capture: &'a SchedulingCaptureRequest,
        material: &'a crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.prepare_owner_scheduling_request_inner(capture, material))
    }

    async fn prepare_owner_scheduling_request_inner(
        &mut self,
        capture: &SchedulingCaptureRequest,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<()> {
        let version = self.runtime.inner.config_snapshot.load_full();
        let config = if version.provider_reconfig.is_some() {
            version.model_catalog.resolved_context_config(
                &version.base_context_config,
                self.candidate.state.model_override.as_ref(),
            )
        } else {
            version.base_context_config.clone()
        };
        let model = version
            .model_catalog
            .effective_model(self.candidate.state.model_override.as_ref());
        let history =
            super::super::turn::context_history_preparation(&self.candidate.state.id, &model);
        let (roots, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let mut request = {
            let _resource_io = material.enter();
            super::super::context_capture::context_input_evidence_request(
                &self.candidate.state,
                None,
                &config,
                history,
                roots,
                record_limit,
                byte_limit,
            )?
        };
        super::super::scheduler::include_scheduler_brief_window(&mut request.windows);
        self.prepare_owner_input_request_inner(
            &request,
            config.recent_episode_candidates,
            Some(capture),
            Some(material),
        )
        .await
        .map(|_| ())
    }
}

impl RuntimeMutation {
    pub(in crate::runtime) fn capture_timer_producer_read(
        &self,
        previous: &crate::types::TimerRecord,
    ) -> Result<super::super::timer_producer_reads::OwnedTimerProducerRead> {
        let request = SchedulingCaptureRequest::current();
        let captured = self.capture_scheduling_read(&request)?;
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        super::super::timer_producer_reads::OwnedTimerProducerRead::from_selected(
            &captured.scheduling,
            &previous.id,
            Some(previous),
            &self.runtime.inner.input_evidence_budget,
            record_limit,
            byte_limit,
        )
    }
}
