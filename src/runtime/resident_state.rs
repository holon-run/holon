//! Immutable records published with the mutation owner's posture generation.

use std::{collections::BTreeMap, sync::Arc};

use anyhow::{ensure, Result};

use super::mutation_owner::PublicationToken;
use super::resident_input_evidence::{CapturedInputEvidence, ResidentInputEvidence};
use super::resident_scheduling::{CapturedScheduling, ResidentSchedulingDomain};
use super::resident_task::{CapturedTasks, ResidentTaskDomain};
use super::resident_work::{CapturedWork, ResidentWorkDomain};
use crate::types::{AgentIdentityRecord, AgentState, ContextEpisodeRecord};

#[derive(Debug, Clone)]
pub(crate) struct AgentSnapshot {
    pub token: PublicationToken,
    pub identity: Option<Arc<AgentIdentityRecord>>,
    pub(crate) _identity_budget: Option<Arc<super::input_evidence_budget::BudgetViewGuard>>,
    pub state: Arc<AgentState>,
    // None is an unprepared domain, never a ready empty collection.
    pub episodes: Option<Arc<ResidentEpisodes>>,
    pub input_evidence: Option<Arc<ResidentInputEvidence>>,
    pub work: Option<Arc<ResidentWorkDomain>>,
    pub tasks: Option<Arc<ResidentTaskDomain>>,
    pub scheduling: Option<Arc<ResidentSchedulingDomain>>,
    pub current_reads: Option<Arc<super::current_read_facts::ResidentCurrentReadFacts>>,
}

/// Only the selected immutable records survive request preparation. This value
/// never retains the published root or a resident domain's maps.
#[derive(Debug)]
pub(crate) struct CapturedAgentContext {
    pub token: PublicationToken,
    pub state: Arc<AgentState>,
    pub episode_candidate_limit: usize,
    pub episodes: Vec<Arc<ContextEpisodeRecord>>,
    pub input_evidence: Option<CapturedInputEvidence>,
    pub work: Option<CapturedWork>,
    pub tasks: Option<CapturedTasks>,
    pub scheduling: Option<CapturedScheduling>,
}

impl AgentSnapshot {
    pub(crate) fn active_identity(&self) -> Result<&Arc<AgentIdentityRecord>> {
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("actual identity header requires preparation"))?;
        ensure!(
            identity.agent_id == self.state.id
                && identity.incarnation == self.token.identity_incarnation,
            "published identity belongs to another incarnation"
        );
        match identity.status {
            crate::types::AgentRegistryStatus::Active => Ok(identity),
            crate::types::AgentRegistryStatus::Deleting => {
                Err(crate::host::PublicAgentError::Deleting {
                    agent_id: identity.agent_id.clone(),
                }
                .into())
            }
            crate::types::AgentRegistryStatus::Deleted => {
                Err(crate::host::PublicAgentError::Deleted {
                    agent_id: identity.agent_id.clone(),
                }
                .into())
            }
        }
    }
    pub(crate) fn capture_context(&self, episode_limit: usize) -> Result<CapturedAgentContext> {
        let episodes = self
            .episodes
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("episode domain requires preparation"))?
            .selected(episode_limit)?;
        Ok(CapturedAgentContext {
            token: self.token,
            state: self.state.clone(),
            episode_candidate_limit: episode_limit,
            episodes,
            input_evidence: None,
            work: None,
            tasks: None,
            scheduling: None,
        })
    }
}

/// The loader must establish lifecycle/adoption scope before constructing this
/// window. Membership in an agent_id-only repository query is not that proof.
#[derive(Debug, Clone)]
pub(crate) struct ResidentEpisodes {
    agent_id: String,
    identity_incarnation: u64,
    limit: usize,
    is_exhaustive: bool,
    by_id: BTreeMap<String, Arc<ContextEpisodeRecord>>,
    recent_ids: Vec<String>,
}

impl ResidentEpisodes {
    pub(crate) fn belongs_to(&self, agent_id: &str, incarnation: u64) -> bool {
        self.agent_id == agent_id && self.identity_incarnation == incarnation
    }

    pub(crate) fn bootstrap(
        agent_id: &str,
        identity_incarnation: u64,
        limit: usize,
        is_exhaustive: bool,
        qualified_records: Vec<ContextEpisodeRecord>,
    ) -> Result<Self> {
        ensure!(
            identity_incarnation > 0,
            "resident episodes need a durable incarnation"
        );
        let mut resident = Self {
            agent_id: agent_id.to_owned(),
            identity_incarnation,
            limit,
            is_exhaustive,
            by_id: BTreeMap::new(),
            recent_ids: Vec::new(),
        };
        for record in qualified_records {
            resident.insert(record)?;
        }
        resident.order_and_bound();
        Ok(resident)
    }

    /// Exact committed additions only. Existing immutable records are shared;
    /// callers retain the old published root until this candidate is published.
    pub(crate) fn with_committed(
        &self,
        identity_incarnation: u64,
        additions: &[ContextEpisodeRecord],
        removals: &[String],
    ) -> Result<Option<Self>> {
        ensure!(
            self.identity_incarnation == identity_incarnation,
            "resident episode incarnation mismatch"
        );
        let mut next = self.clone();
        if !self.is_exhaustive && removals.iter().any(|id| self.by_id.contains_key(id)) {
            // A discarded tail may now contain a top-K record. Hydration must
            // establish coverage before this domain can be ready again.
            return Ok(None);
        }
        for id in removals {
            next.by_id.remove(id);
        }
        for record in additions {
            next.insert(record.clone())?;
        }
        next.order_and_bound();
        Ok(Some(next))
    }

    pub(crate) fn selected(&self, limit: usize) -> Result<Vec<Arc<ContextEpisodeRecord>>> {
        ensure!(
            limit <= self.limit,
            "episode window expansion needs explicit hydration"
        );
        Ok(self
            .recent_ids
            .iter()
            .take(limit)
            .rev()
            .map(|id| {
                self.get(id)
                    .expect("resident recent index points to a record")
            })
            .collect())
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<ContextEpisodeRecord>> {
        self.by_id.get(id).cloned()
    }

    fn insert(&mut self, record: ContextEpisodeRecord) -> Result<()> {
        ensure!(
            record.agent_id == self.agent_id,
            "resident episode agent mismatch"
        );
        if let Some(existing) = self.by_id.get(&record.id) {
            ensure!(
                existing.as_ref() == &record,
                "immutable resident episode conflict"
            );
        } else {
            self.by_id.insert(record.id.clone(), Arc::new(record));
        }
        Ok(())
    }

    fn order_and_bound(&mut self) {
        if self.by_id.len() > self.limit {
            self.is_exhaustive = false;
        }
        let mut records = self.by_id.values().collect::<Vec<_>>();
        // Match the canonical recent query, including its deterministic ties.
        records.sort_by(|a, b| {
            b.finalized_at
                .cmp(&a.finalized_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        self.recent_ids = records
            .into_iter()
            .take(self.limit)
            .map(|record| record.id.clone())
            .collect();
        self.by_id.retain(|id, _| self.recent_ids.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EpisodeBoundaryReason;

    fn episode(id: &str, second: i64) -> ContextEpisodeRecord {
        let timestamp = chrono::DateTime::from_timestamp(second, 0).unwrap();
        ContextEpisodeRecord {
            id: id.into(),
            agent_id: "agent-a".into(),
            workspace_id: "agent-home".into(),
            created_at: timestamp,
            finalized_at: timestamp,
            start_turn_index: 1,
            end_turn_index: 1,
            start_message_count: 1,
            end_message_count: 1,
            boundary_reason: EpisodeBoundaryReason::HardTurnCap,
            current_work_item_id: None,
            objective: None,
            work_summary: None,
            scope_hints: Vec::new(),
            source_turn_ids: Vec::new(),
            source_refs: Vec::new(),
            generated_by: None,
            working_set_files: Vec::new(),
            decisions: Vec::new(),
            carry_forward: Vec::new(),
            waiting_on: Vec::new(),
        }
    }

    #[test]
    fn selected_record_outlives_root_without_pinning_other_records() {
        let root = ResidentEpisodes::bootstrap(
            "agent-a",
            2,
            2,
            true,
            vec![episode("a", 1), episode("b", 2)],
        )
        .unwrap();
        let other = Arc::downgrade(&root.get("a").unwrap());
        let selected = root.selected(1).unwrap();
        drop(root);
        assert!(other.upgrade().is_none());
        assert_eq!(selected[0].id, "b");
    }

    #[test]
    fn captured_context_does_not_retain_root_or_unselected_domain_allocations() {
        let episodes = Arc::new(
            ResidentEpisodes::bootstrap(
                "agent-a",
                2,
                2,
                true,
                vec![episode("a", 1), episode("b", 2)],
            )
            .unwrap(),
        );
        let old_record = Arc::downgrade(&episodes.get("a").unwrap());
        let domain = Arc::downgrade(&episodes);
        let root = Arc::new(AgentSnapshot {
            identity: None,
            _identity_budget: None,
            input_evidence: None,
            work: None,
            tasks: None,
            scheduling: None,
            current_reads: None,
            token: PublicationToken {
                identity_incarnation: 2,
                owner_epoch: 1,
                generation: 1,
            },
            state: Arc::new(AgentState::new("agent-a")),
            episodes: Some(episodes),
        });
        let old_root = Arc::downgrade(&root);
        let captured = root.capture_context(1).unwrap();
        drop(root);
        assert!(old_root.upgrade().is_none());
        assert!(domain.upgrade().is_none());
        assert!(old_record.upgrade().is_none());
        assert_eq!(captured.episode_candidate_limit, 1);
        assert_eq!(captured.episodes[0].id, "b");
    }

    #[test]
    fn committed_window_shares_unchanged_records_and_rejects_conflicts() {
        let original = ResidentEpisodes::bootstrap(
            "agent-a",
            2,
            2,
            true,
            vec![episode("a", 1), episode("b", 2)],
        )
        .unwrap();
        let next = original
            .with_committed(2, &[episode("c", 3)], &[])
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(
            &original.get("b").unwrap(),
            &next.get("b").unwrap()
        ));
        assert!(original.get("a").is_some());
        assert!(next.get("a").is_none());
        assert!(next.with_committed(2, &[episode("b", 4)], &[]).is_err());
        assert!(next.with_committed(3, &[], &[]).is_err());
        assert!(next.selected(3).is_err());
        assert_eq!(
            next.selected(2)
                .unwrap()
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        assert!(next
            .with_committed(2, &[], &["c".into()])
            .unwrap()
            .is_none());
    }
}
