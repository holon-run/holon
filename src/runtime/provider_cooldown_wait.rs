//! Turn-local provider cooldown wait sub-state (#3772, option A).
//!
//! The wait is an in-memory sub-state of the active execution: it is
//! queryable through agent summaries and announced through
//! `agent_state_changed` events, but it is not a terminal state, creates no
//! scheduling wake, never calls the model, and is never persisted, so a
//! restart cannot surface a stale wait.

use super::RuntimeHandle;
use crate::runtime_event::RuntimeEventKind;
use crate::types::{
    AgentStateChangedEvent, AuditEvent, ProviderCooldownCauseSummary, ProviderCooldownRetryAfter,
    ProviderCooldownRetryAfterSource, ProviderCooldownWait, ProviderCooldownWaitEndOutcome,
    ProviderCooldownWaitEnded,
};
use anyhow::Result;
use chrono::{DateTime, Utc};

impl From<crate::runtime_db::provider_admission::ProviderCooldownCause>
    for ProviderCooldownCauseSummary
{
    fn from(cause: crate::runtime_db::provider_admission::ProviderCooldownCause) -> Self {
        let details_available = cause.details_available;
        let retry_after = cause.server_not_before.map(|deadline| {
            let seconds = (deadline - cause.occurred_at)
                .num_seconds()
                .try_into()
                .ok()
                .filter(|seconds| *seconds > 0);
            ProviderCooldownRetryAfter {
                source: ProviderCooldownRetryAfterSource::ServerHeader,
                seconds,
                policy: None,
            }
        });
        Self {
            failure_kind: Some(cause.failure_kind),
            http_status: cause.http_status,
            occurred_at: Some(cause.occurred_at),
            retry_after,
            incident_id: cause.incident_id,
            // Stable reason code; user-facing text is localized by clients.
            detail_unavailable_reason: (!details_available)
                .then(|| "cause_details_unavailable".to_string()),
        }
    }
}

impl RuntimeHandle {
    /// Record that the active execution began locally waiting for a shared
    /// provider cooldown deadline. Emits one `agent_state_changed` event when
    /// the visible state changes; identical repeats emit nothing.
    pub async fn begin_provider_cooldown_wait(&self, wait: ProviderCooldownWait) -> Result<()> {
        let event = {
            let mut guard = self.inner.agent.lock().await;
            if guard.state.provider_cooldown_wait.as_ref() == Some(&wait) {
                return Ok(());
            }
            guard.state.provider_cooldown_wait = Some(wait);
            AgentStateChangedEvent::from_state(&guard.state)
        };
        self.emit_provider_cooldown_state_changed(event).await
    }

    /// Update the authoritative deadline of the active wait (cooldown
    /// extension) and optionally refresh its cause summary. Returns false
    /// when no wait is active; unchanged updates emit no event.
    pub async fn extend_provider_cooldown_wait(
        &self,
        not_before: DateTime<Utc>,
        cause: Option<ProviderCooldownCauseSummary>,
    ) -> Result<bool> {
        let event = {
            let mut guard = self.inner.agent.lock().await;
            let Some(wait) = guard.state.provider_cooldown_wait.as_mut() else {
                return Ok(false);
            };
            if wait.not_before == not_before
                && cause.as_ref().is_none_or(|cause| cause == &wait.cause)
            {
                return Ok(true);
            }
            wait.not_before = not_before;
            if let Some(cause) = cause {
                wait.cause = cause;
            }
            AgentStateChangedEvent::from_state(&guard.state)
        };
        self.emit_provider_cooldown_state_changed(event).await?;
        Ok(true)
    }

    /// Clear the active wait and emit its final transition with `outcome`.
    /// Returns false when no wait was active.
    pub async fn end_provider_cooldown_wait(
        &self,
        outcome: ProviderCooldownWaitEndOutcome,
    ) -> Result<bool> {
        let event = {
            let mut guard = self.inner.agent.lock().await;
            let Some(wait) = guard.state.provider_cooldown_wait.take() else {
                return Ok(false);
            };
            let mut event = AgentStateChangedEvent::from_state(&guard.state);
            event.provider_cooldown_wait_ended = Some(ProviderCooldownWaitEnded {
                outcome,
                turn_id: wait.turn_id,
                ended_at: self.now(),
            });
            event
        };
        self.emit_provider_cooldown_state_changed(event).await?;
        Ok(true)
    }

    async fn emit_provider_cooldown_state_changed(
        &self,
        event: AgentStateChangedEvent,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::typed(
            RuntimeEventKind::AgentStateChanged,
            &event,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelRouteRef;
    use crate::runtime::tests::support::LifecycleHarness;
    use crate::types::AgentStatus;

    fn sample_wait(not_before: DateTime<Utc>) -> ProviderCooldownWait {
        ProviderCooldownWait {
            turn_id: Some("turn-1".into()),
            turn_index: 3,
            attempt_id: Some("attempt-1".into()),
            model: Some(ModelRouteRef::parse_compatible("openai/gpt-4o").unwrap()),
            started_at: Utc::now(),
            not_before,
            cause: ProviderCooldownCauseSummary {
                failure_kind: Some("rate_limited".into()),
                http_status: Some(429),
                occurred_at: Some(Utc::now()),
                retry_after: Some(ProviderCooldownRetryAfter {
                    source: ProviderCooldownRetryAfterSource::ServerHeader,
                    seconds: Some(30),
                    policy: None,
                }),
                incident_id: Some("incident-1".into()),
                detail_unavailable_reason: None,
            },
        }
    }

    fn agent_state_changed_payloads(runtime: &LifecycleHarness) -> Vec<serde_json::Value> {
        runtime
            .runtime()
            .storage()
            .read_recent_events(50)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "agent_state_changed")
            .map(|event| event.data)
            .collect()
    }

    #[tokio::test]
    async fn begin_extend_end_surface_queryable_state_and_events() {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let deadline = Utc::now() + chrono::Duration::seconds(11);

        runtime
            .begin_provider_cooldown_wait(sample_wait(deadline))
            .await
            .unwrap();
        let state = runtime.agent_state().await.unwrap();
        let wait = state.provider_cooldown_wait.expect("wait visible in state");
        assert_eq!(wait.not_before, deadline);
        assert_eq!(wait.cause.failure_kind.as_deref(), Some("rate_limited"));

        let extended_deadline = deadline + chrono::Duration::seconds(600);
        assert!(runtime
            .extend_provider_cooldown_wait(extended_deadline, None)
            .await
            .unwrap());
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(
            state
                .provider_cooldown_wait
                .expect("wait retained")
                .not_before,
            extended_deadline
        );

        assert!(runtime
            .end_provider_cooldown_wait(ProviderCooldownWaitEndOutcome::Resumed)
            .await
            .unwrap());
        let state = runtime.agent_state().await.unwrap();
        assert!(state.provider_cooldown_wait.is_none());

        let events = agent_state_changed_payloads(&harness);
        assert_eq!(events.len(), 3, "begin, extend, end each emit one event");
        let parsed_deadline = DateTime::parse_from_rfc3339(
            events[0]["provider_cooldown_wait"]["not_before"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed_deadline.with_timezone(&Utc), deadline);
        let parsed_extended = DateTime::parse_from_rfc3339(
            events[1]["provider_cooldown_wait"]["not_before"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed_extended.with_timezone(&Utc), extended_deadline);
        assert_eq!(
            events[1]["provider_cooldown_wait"]["cause"]["failure_kind"],
            "rate_limited"
        );
        assert!(events[2]["provider_cooldown_wait"].is_null());
        assert_eq!(
            events[2]["provider_cooldown_wait_ended"]["outcome"],
            serde_json::json!("resumed")
        );
        assert_eq!(
            events[2]["provider_cooldown_wait_ended"]["turn_id"],
            serde_json::json!("turn-1")
        );

        // Ending again is a no-op that emits nothing.
        assert!(!runtime
            .end_provider_cooldown_wait(ProviderCooldownWaitEndOutcome::Cancelled)
            .await
            .unwrap());
        assert_eq!(agent_state_changed_payloads(&harness).len(), 3);
    }

    #[tokio::test]
    async fn identical_begin_and_unchanged_extend_emit_no_events() {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let deadline = Utc::now() + chrono::Duration::seconds(11);
        let wait = sample_wait(deadline);

        runtime
            .begin_provider_cooldown_wait(wait.clone())
            .await
            .unwrap();
        runtime
            .begin_provider_cooldown_wait(wait.clone())
            .await
            .unwrap();
        assert!(runtime
            .extend_provider_cooldown_wait(deadline, None)
            .await
            .unwrap());
        assert!(runtime
            .extend_provider_cooldown_wait(deadline, Some(wait.cause))
            .await
            .unwrap());
        assert_eq!(agent_state_changed_payloads(&harness).len(), 1);
    }

    #[tokio::test]
    async fn wait_is_not_persisted_across_runtime_restart() {
        let mut harness = LifecycleHarness::new();
        let deadline = Utc::now() + chrono::Duration::seconds(600);
        harness
            .runtime()
            .begin_provider_cooldown_wait(sample_wait(deadline))
            .await
            .unwrap();
        // Force a state persist while waiting; the wait must not leak into
        // the persisted payload.
        {
            let mut guard = harness.runtime().inner.agent.lock().await;
            guard.state.status = AgentStatus::AwakeRunning;
            guard
                .persist_state(&harness.runtime().inner.storage)
                .unwrap();
        }
        harness.restart();
        let state = harness.runtime().agent_state().await.unwrap();
        assert!(state.provider_cooldown_wait.is_none());
    }

    #[tokio::test]
    async fn abort_and_validation_failure_clear_summary_and_emit_outcome() {
        for outcome in [
            ProviderCooldownWaitEndOutcome::Cancelled,
            ProviderCooldownWaitEndOutcome::ValidationFailed,
        ] {
            let harness = LifecycleHarness::new();
            let runtime = harness.runtime();
            let deadline = Utc::now() + chrono::Duration::seconds(11);
            runtime
                .begin_provider_cooldown_wait(sample_wait(deadline))
                .await
                .unwrap();
            let summary = runtime.agent_summary().await.unwrap();
            assert!(summary.provider_cooldown_wait.is_some());
            let slim = crate::http_dto::SlimAgentDto::from(&summary);
            assert!(slim.agent.provider_cooldown_wait.is_some());
            assert!(slim.into_agent_summary().provider_cooldown_wait.is_some());

            assert!(runtime.end_provider_cooldown_wait(outcome).await.unwrap());
            assert!(runtime
                .agent_summary()
                .await
                .unwrap()
                .provider_cooldown_wait
                .is_none());
            assert!(!runtime.end_provider_cooldown_wait(outcome).await.unwrap());
            let events = agent_state_changed_payloads(&harness);
            assert_eq!(events.len(), 2);
            assert!(events[1]["provider_cooldown_wait"].is_null());
            assert_eq!(
                events[1]["provider_cooldown_wait_ended"]["outcome"],
                serde_json::to_value(outcome).unwrap()
            );
        }
    }

    #[test]
    fn cause_summary_converts_sanitized_cooldown_cause() {
        let cause = crate::runtime_db::provider_admission::ProviderCooldownCause {
            failure_kind: "rate_limited".into(),
            http_status: Some(429),
            occurred_at: Utc::now(),
            server_not_before: None,
            incident_id: Some("incident-9".into()),
            details_available: false,
        };
        let summary = ProviderCooldownCauseSummary::from(cause);
        assert_eq!(summary.failure_kind.as_deref(), Some("rate_limited"));
        assert_eq!(summary.http_status, Some(429));
        assert_eq!(summary.incident_id.as_deref(), Some("incident-9"));
        assert_eq!(
            summary.detail_unavailable_reason.as_deref(),
            Some("cause_details_unavailable")
        );
        assert!(summary.retry_after.is_none());
    }

    #[test]
    fn cause_summary_marks_server_retry_after_origin() {
        let occurred_at = Utc::now();
        let cause = crate::runtime_db::provider_admission::ProviderCooldownCause {
            failure_kind: "server_error".into(),
            http_status: Some(503),
            occurred_at,
            server_not_before: Some(occurred_at + chrono::Duration::seconds(120)),
            incident_id: None,
            details_available: true,
        };
        let summary = ProviderCooldownCauseSummary::from(cause);
        let retry_after = summary.retry_after.expect("server deadline mapped");
        assert_eq!(
            retry_after.source,
            ProviderCooldownRetryAfterSource::ServerHeader
        );
        assert_eq!(retry_after.seconds, Some(120));
        assert!(summary.detail_unavailable_reason.is_none());
    }
}
