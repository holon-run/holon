use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result};
use tokio::time::Instant;

use crate::{
    daemon::{
        persist_quarantine_boot, QuarantineBoot, QuarantineEntry, QuarantineReason, QuarantineStage,
    },
    startup::StartupObservation,
};

use super::{RuntimeActivationReason, RuntimeHost};

const COOLDOWN: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
#[error("agent {agent_id} is quarantined this boot: storage constructor rejected agent-local input preparation")]
pub(crate) struct AgentQuarantined {
    pub agent_id: String,
    pub incarnation: u64,
}

#[derive(Default)]
pub(super) struct AgentQuarantines {
    entries: BTreeMap<String, (Instant, QuarantineEntry)>,
    boot: Option<StartupObservation>,
}

impl AgentQuarantines {
    pub(super) fn blocks_activation(
        &self,
        agent_id: &str,
        incarnation: u64,
        reason: RuntimeActivationReason,
        now: Instant,
    ) -> bool {
        self.entries.get(agent_id).is_some_and(|(retry_at, entry)| {
            entry.incarnation == incarnation
                && (now < *retry_at
                    || !matches!(
                        reason,
                        RuntimeActivationReason::OperatorControl
                            | RuntimeActivationReason::ExternalIngress
                    ))
        })
    }

    pub(super) fn contains(&self, agent_id: &str, incarnation: u64) -> bool {
        self.entries
            .get(agent_id)
            .is_some_and(|(_, entry)| entry.incarnation == incarnation)
    }

    pub(super) fn record(
        &mut self,
        agent_id: &str,
        incarnation: u64,
        generation: u64,
        now: Instant,
    ) -> AgentQuarantined {
        self.entries.insert(
            agent_id.to_owned(),
            (
                now + COOLDOWN,
                QuarantineEntry {
                    agent_id: agent_id.to_owned(),
                    incarnation,
                    generation,
                    stage: QuarantineStage::StorageConstructor,
                    reason: QuarantineReason::ProjectionChanged,
                    failed_at: chrono::Utc::now(),
                },
            ),
        );
        AgentQuarantined {
            agent_id: agent_id.to_owned(),
            incarnation,
        }
    }

    pub(super) fn clear_on_install(&mut self, agent_id: &str) -> bool {
        self.entries.remove(agent_id).is_some()
    }

    pub(super) async fn persist(&self, host: &RuntimeHost) {
        let Some(observation) = &self.boot else {
            return;
        };
        let run_dir = host.config().run_dir();
        let boot = QuarantineBoot::new(
            &observation.startup_id,
            observation.started_at,
            self.entries.values().map(|(_, entry)| entry),
        );
        if !matches!(
            tokio::task::spawn_blocking(move || persist_quarantine_boot(&run_dir, boot)).await,
            Ok(Ok(()))
        ) {
            tracing::warn!(startup_id = %observation.startup_id, "could not persist agent quarantine report");
        }
    }
}

impl RuntimeHost {
    /// Track the diagnostic boot; persisted reports are never admission input.
    pub async fn begin_quarantine_report(&self, observation: &StartupObservation) {
        let mut registry = self.inner.runtimes.write().await;
        registry
            .quarantines
            .boot
            .get_or_insert_with(|| observation.clone());
    }

    pub async fn publish_quarantine_report(&self) {
        let registry = self.inner.runtimes.write().await;
        registry.quarantines.persist(self).await;
    }

    pub async fn bind_primary_for_serve(
        &self,
        observation: &StartupObservation,
        http_addr: &str,
    ) -> Result<tokio::net::TcpListener> {
        observation
            .observe("default_activation", async {
                if self.configured_default_agent_id()?.is_some() {
                    self.prepare_default_runtime_for_serve(observation).await?;
                }
                Ok(())
            })
            .await?;
        observation
            .observe("bind", async {
                tokio::net::TcpListener::bind(http_addr)
                    .await
                    .with_context(|| format!("failed to bind {http_addr}"))
            })
            .await
    }

    async fn prepare_default_runtime_for_serve(
        &self,
        observation: &StartupObservation,
    ) -> Result<()> {
        let runtime = match self
            .default_runtime_observed(Some(observation.clone()))
            .await
        {
            Ok(runtime) => runtime,
            Err(error) if error.downcast_ref::<AgentQuarantined>().is_some() => return Ok(()),
            Err(error) => return Err(error),
        };
        // Late bootstrap failures do not carry constructor isolation proof.
        runtime.wait_for_bootstrap().await
    }

    pub(crate) async fn agent_is_quarantined(&self, agent_id: &str, incarnation: u64) -> bool {
        self.inner
            .runtimes
            .read()
            .await
            .quarantines
            .contains(agent_id, incarnation)
    }

    pub(super) async fn cold_lifecycle_hint(
        &self,
        agent_id: &str,
        incarnation: u64,
        status: crate::types::AgentStatus,
    ) -> crate::types::AgentLifecycleHint {
        if self.agent_is_quarantined(agent_id, incarnation).await {
            return crate::types::AgentLifecycleHint {
                accepts_external_messages: false,
                operator_hint: Some(
                    "agent quarantined this boot; see holon daemon quarantine".into(),
                ),
            };
        }
        crate::types::AgentLifecycleHint::from_status(agent_id, status)
    }
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use super::*;

    #[test]
    fn bounded_cooldown_fences_incarnations_and_resets_on_failure() {
        let demand = RuntimeActivationReason::OperatorControl;
        let mut state = AgentQuarantines::default();
        let start = Instant::now();
        state.record("operator", 1, 4, start);
        assert!(state.blocks_activation(
            "operator",
            1,
            demand,
            start + Duration::from_millis(14_999)
        ));
        assert!(!state.blocks_activation("operator", 2, demand, start));
        assert!(!state.blocks_activation("operator", 1, demand, start + COOLDOWN));
        for reason in [
            RuntimeActivationReason::StartupRecovery,
            RuntimeActivationReason::AgentLifecycle,
            RuntimeActivationReason::SchedulerDispatch,
            RuntimeActivationReason::Wake,
            RuntimeActivationReason::ChildSupervision,
        ] {
            assert!(state.blocks_activation("operator", 1, reason, start + COOLDOWN));
        }
        assert!(state.contains("operator", 1));
        let retry = start + COOLDOWN;
        state.record("operator", 1, 5, retry);
        assert!(state.blocks_activation("operator", 1, demand, retry));
        assert!(!state.blocks_activation("operator", 1, demand, retry + COOLDOWN));
        assert!(state.clear_on_install("operator"));
        assert!(!state.contains("operator", 1));
    }
}
