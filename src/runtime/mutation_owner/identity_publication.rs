//! One actual identity allocation and its owned material follow the original ACK.
use super::super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use super::*;
use anyhow::{ensure, Context, Result};

impl PartialEq for IdentityMutationFacts {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity && self.state == other.state
    }
}
impl IdentityMutationFacts {
    pub(crate) fn absent() -> Self {
        Self {
            identity: None,
            state: None,
            _identity_budget: None,
        }
    }
    pub(crate) fn budgeted(
        identity: Option<&AgentIdentityRecord>,
        state: Option<Arc<AgentState>>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let guard = identity
            .map(|id| identity_material(id, ledger))
            .transpose()?;
        Ok(Self {
            identity: identity.cloned().map(Arc::new),
            state,
            _identity_budget: guard,
        })
    }
    pub(crate) fn budgeted_owned(
        identity: Option<AgentIdentityRecord>,
        state: Option<Arc<AgentState>>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let guard = identity
            .as_ref()
            .map(|id| identity_material(id, ledger))
            .transpose()?;
        Ok(Self {
            identity: identity.map(Arc::new),
            state,
            _identity_budget: guard,
        })
    }
    pub(crate) fn observed(
        identity: Arc<AgentIdentityRecord>,
        state: Option<Arc<AgentState>>,
        guard: Arc<BudgetViewGuard>,
    ) -> Self {
        Self {
            identity: Some(identity),
            state,
            _identity_budget: Some(guard),
        }
    }
}
fn identity_material(
    identity: &AgentIdentityRecord,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<BudgetViewGuard>> {
    // This schema has only scalar fields and six optional/required strings.
    // Streaming encoding measures borrowed strings before any clone or Arc.
    let bytes = serialized_bytes(identity)?
        .checked_mul(8)
        .and_then(|n| n.checked_add(8192))
        .context("identity publication material overflow")?;
    let limits = ledger.limits();
    ensure!(
        bytes
            <= limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("identity publication limit overflow")?
                .min(64 * 1024 * 1024)
            && 24 * 8 <= limits.record_limit.min(65_536),
        "identity publication material budget exhausted"
    );
    ensure!(
        !identity.agent_id.is_empty() && identity.incarnation > 0,
        "identity publication has no actual durable scope"
    );
    // No domain may exist yet. This small actual Host view covers the address
    // and ledger-key clones; it grants neither identity nor Ready authority.
    let address = ledger.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(identity.agent_id.len())?
                .checked_mul(4)
                .and_then(|n| n.checked_add(512))
                .context("identity address material overflow")?,
        },
    )?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let guard = ledger.register_owned_bytes(
        &scope,
        InputMemoryCategory::Current,
        OwnedBytesEstimate {
            payload_bytes: bytes,
            metadata_estimate_bytes: 0,
        },
    )?;
    drop((scope, address));
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> AgentIdentityRecord {
        AgentIdentityRecord::new(
            "identity-owner",
            crate::types::AgentKind::Default,
            crate::types::AgentVisibility::Public,
            crate::types::AgentOwnership::SelfOwned,
            crate::types::AgentProfilePreset::PublicNamed,
            None,
            None,
        )
    }
    async fn publish(
        owner: &MutationOwner,
        facts: IdentityMutationFacts,
    ) -> Result<Option<Arc<PostureObservation>>> {
        let mut reservations = vec![owner.reserve().await?];
        MutationReservation::begin_all(&mut reservations, "actual-metadata-ack".into())?;
        MutationReservation::committed_all(
            &mut reservations,
            vec![KnownCommit::Metadata(Arc::new(facts))],
        )?;
        Ok(MutationReservation::publish_all_known(&mut reservations)?.remove(0))
    }
    #[tokio::test]
    async fn metadata_no_state_then_state_ack_shares_identity_and_last_guard() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let owner = MutationOwner::standalone("identity-owner", 1);
        let facts = IdentityMutationFacts::budgeted_owned(Some(identity()), None, &ledger)?;
        let selected = facts.clone();
        let weak = Arc::downgrade(facts.identity.as_ref().unwrap());
        let guard = Arc::downgrade(facts._identity_budget.as_ref().unwrap());
        let bytes = ledger.report()?.observed_bytes;
        ensure!(bytes > 0);
        ensure!(publish(&owner, facts).await?.is_none());
        ensure!(owner.capture()?.is_none());
        let state = Arc::new(AgentState::new("identity-owner"));
        let root = {
            let mut reservation = owner.reserve().await?;
            reservation.begin("actual-state-ack".into())?;
            reservation.committed(state.clone(), None)?;
            reservation.publish(state)?
        };
        ensure!(Arc::ptr_eq(
            root.active_identity()?,
            selected.identity.as_ref().unwrap()
        ));
        ensure!(ledger.report()?.observed_bytes == bytes);
        drop(owner);
        ensure!(weak.upgrade().is_some() && guard.upgrade().is_some());
        drop(root);
        ensure!(weak.upgrade().is_some() && guard.upgrade().is_some());
        drop(selected);
        ensure!(weak.upgrade().is_none() && guard.upgrade().is_none());
        ensure!(ledger.report()?.observed_bytes == 0);
        Ok(())
    }
    #[tokio::test]
    async fn metadata_update_keeps_original_capture_only_until_its_last_holder() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let owner = MutationOwner::standalone("identity-owner", 1);
        let state = Arc::new(AgentState::new("identity-owner"));
        let old = publish(
            &owner,
            IdentityMutationFacts::budgeted_owned(Some(identity()), Some(state.clone()), &ledger)?,
        )
        .await?
        .unwrap();
        let old_identity = Arc::downgrade(old.identity.as_ref().unwrap());
        let old_guard = Arc::downgrade(old._identity_budget.as_ref().unwrap());
        let mut updated = identity();
        updated.name = Some("actual second ACK".into());
        updated.revision += 1;
        let new = publish(
            &owner,
            IdentityMutationFacts::budgeted_owned(Some(updated), Some(state), &ledger)?,
        )
        .await?
        .unwrap();
        ensure!(old.active_identity()?.name.is_none());
        ensure!(new.active_identity()?.name.as_deref() == Some("actual second ACK"));
        ensure!(!Arc::ptr_eq(
            old.identity.as_ref().unwrap(),
            new.identity.as_ref().unwrap()
        ));
        drop(old);
        ensure!(old_identity.upgrade().is_none() && old_guard.upgrade().is_none());
        drop(owner);
        drop(new);
        ensure!(ledger.report()?.observed_bytes == 0);
        Ok(())
    }
    #[tokio::test]
    async fn retired_or_foreign_metadata_never_attests_ready_identity() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let owner = MutationOwner::standalone("identity-owner", 1);
        let mut retired = identity();
        retired.incarnation = 2;
        let facts = IdentityMutationFacts::budgeted_owned(Some(retired), None, &ledger)?;
        let body = Arc::downgrade(facts.identity.as_ref().unwrap());
        ensure!(publish(&owner, facts).await?.is_none());
        ensure!(owner.capture()?.is_none());
        drop(owner);
        ensure!(body.upgrade().is_none());
        let foreign_owner = MutationOwner::standalone("another-owner", 1);
        let mut reservation = foreign_owner.reserve().await?;
        reservation.begin("foreign-ack".into())?;
        let facts = IdentityMutationFacts::budgeted_owned(Some(identity()), None, &ledger)?;
        let error = MutationReservation::committed_all(
            std::slice::from_mut(&mut reservation),
            vec![KnownCommit::Metadata(Arc::new(facts))],
        )
        .err()
        .context("foreign must reject")?;
        ensure!(matches!(error, MutationOwnerError::TicketMismatch));
        ensure!(ledger.report()?.observed_bytes == 0);
        Ok(())
    }
    #[test]
    fn borrowed_identity_budget_precedes_clone_and_releases_on_rejection() -> Result<()> {
        let mut limits = crate::config::StateMemoryLimits::default();
        limits.history_per_agent_bytes = 64 * 1024;
        limits.pending_per_agent_bytes = 4096;
        let ledger = InputEvidenceBudgetLedger::new(limits)?;
        let mut record = identity();
        record.name = Some("x".repeat(512 * 1024));
        ensure!(IdentityMutationFacts::budgeted(Some(&record), None, &ledger).is_err());
        ensure!(ledger.report()?.observed_bytes == 0);
        Ok(())
    }
}
