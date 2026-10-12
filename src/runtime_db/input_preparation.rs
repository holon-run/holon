//! Explicit owner-held preparation, never a context reader fallback.

use anyhow::{ensure, Result};
use rusqlite::Transaction;

use super::{
    bootstrap_inputs::{pending_metadata_tx, pending_queue_inputs_tx, PreparedPendingQueue},
    context_inputs::{ContextDurableRead, ContextScopeFacts},
    episode_snapshot::{load_episode_memory_snapshot_in, EpisodeMemoryLoadScope},
    input_evidence_snapshot::{
        prepare_input_evidence_snapshot_tx, InputEvidenceBootstrapSnapshot,
        InputEvidenceLoadRequest, InputEvidenceSnapshotError,
    },
};
use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceScope,
            ResidentInputEvidence,
        },
        resident_state::ResidentEpisodes,
    },
    types::{AgentRegistryStatus, AgentState},
};

pub(crate) struct PreparedInputRead {
    pub facts: ContextScopeFacts,
    pub episodes: ResidentEpisodes,
    pub inputs: ResidentInputEvidence,
    pub pending: PreparedPendingQueue,
}

/// The caller holds the original Ready owner reservation throughout this
/// transaction and installation. Only qualification sidecars may be written.
pub(crate) fn prepare_input_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        false,
    )
}

/// Recovery requires an original positive receipt and all original owner permits.
/// Normal bootstrap/context preparation continues to require an Active identity.
pub(crate) fn prepare_original_input_recovery_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        true,
    )
}
/// Explicit preparation for an already admitted cleanup, after owner identity
/// validation. Deleting reads only this scope's fixed/current qualification.
pub(crate) fn prepare_existing_admission_cleanup_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        true,
    )
}

fn prepare_input_domains_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    original_recovery: bool,
) -> Result<PreparedInputRead> {
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    if !facts.identity.as_ref().is_some_and(|identity| {
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && (identity.status == AgentRegistryStatus::Active
                || (original_recovery && identity.status == AgentRegistryStatus::Deleting))
    }) {
        return Err(InputEvidenceSnapshotError::RetiredScope.into());
    }
    ensure!(
        facts.state.as_ref() == Some(published_state) && facts.control_revision.is_some(),
        "input preparation canonical State differs from its published root"
    );
    // Count and conservative encoded bytes reject before Episode or input body
    // materialization. The later queue read is from this same SQL snapshot.
    let metadata = if original_recovery {
        super::bootstrap_inputs::pending_metadata_for_input_recovery_tx(tx, scope, budget)?
    } else {
        pending_metadata_tx(tx, scope, budget)?
    };
    ensure!(
        metadata.count == published_state.pending,
        "input preparation canonical pending queue differs from State"
    );
    let episode_scope = EpisodeMemoryLoadScope {
        agent_id: scope.agent_id.clone(),
        identity_incarnation: scope.identity_incarnation,
    };
    let current = if original_recovery {
        super::episode_snapshot::load_episode_memory_for_input_recovery_in(
            tx,
            &episode_scope,
            recent_episode_limit,
        )?
    } else {
        load_episode_memory_snapshot_in(tx, &episode_scope, recent_episode_limit, None)?
    }
    .require_current()?;
    let pending = if original_recovery {
        super::bootstrap_inputs::pending_queue_inputs_for_input_recovery_tx(tx, scope, budget)?
    } else {
        pending_queue_inputs_tx(tx, scope, budget)?
    };
    let mut request = request.clone();
    for message in &pending.messages {
        request
            .mandatory_roots
            .insert(Key::new(Family::Message, &message.id));
        // Snapshot preparation validates asserted recovery sources on its
        // same read view, including focused canonical absence observations.
    }
    let prepared = if original_recovery {
        super::input_evidence_snapshot::prepare_original_input_recovery_snapshot_tx(
            tx, scope, &request,
        )?
    } else {
        prepare_input_evidence_snapshot_tx(tx, scope, &request)?
    };
    let input = match prepared {
        InputEvidenceBootstrapSnapshot::Current(input) => input,
        InputEvidenceBootstrapSnapshot::Retired {
            requested_scope,
            current_identity,
        } => {
            return Err(
                anyhow::Error::from(InputEvidenceSnapshotError::RetiredScope).context(format!(
                    "input preparation scope {:?} retired; current identity {:?}",
                    requested_scope, current_identity
                )),
            );
        }
    };
    ensure!(
        facts.identity.as_ref() == Some(&current.identity)
            && facts.state == current.state
            && facts.control_revision == current.control_revision
            && input.facts.identity == facts.identity
            && input.facts.state == facts.state
            && input.facts.control_revision == facts.control_revision
            && pending.metadata.facts.identity == facts.identity
            && pending.metadata.facts.state == facts.state
            && pending.metadata.facts.control_revision == facts.control_revision,
        "prepared domains do not share their canonical read view"
    );
    Ok(PreparedInputRead {
        facts,
        episodes: ResidentEpisodes::bootstrap(
            &scope.agent_id,
            scope.identity_incarnation,
            recent_episode_limit,
            !current.has_more,
            current.episodes,
        )?,
        inputs: input.resident,
        pending,
    })
}
