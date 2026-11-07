use super::*;
use crate::{
    runtime::mutation_owner::{MutationOwner, MutationOwnerError},
    runtime::mutation_witness::MutationWitness,
    runtime::resident_input_evidence::InputEvidenceScope,
    runtime_db::{
        input_mutation_receipts::OriginalInputMutationAttempt,
        transitions::{
            AgentPostureTransitionCommand, AgentStateMutation, PreparedInputTransition,
            TransitionFaultPoint,
        },
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentStatus, AgentVisibility,
    },
};

struct Fixture {
    _directory: tempfile::TempDir,
    db: RuntimeDb,
    owner: MutationOwner,
    scope: InputEvidenceScope,
    state: AgentState,
    budget: InputEvidenceBudgetLedger,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "input-recovery".into(),
            identity_incarnation: 1,
        };
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            &scope.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let state = AgentState::new(&scope.agent_id);
        db.agent_states().upsert_for_fixture(&state)?;
        let owner = MutationOwner::standalone(&scope.agent_id, 1);
        let mut reservation = owner.try_reserve()?;
        reservation.begin("initial".into())?;
        let initial = Arc::new(state.clone());
        reservation.committed_prepared(initial.clone(), None, None)?;
        reservation.publish(initial)?;
        drop(reservation);
        Ok(Self {
            _directory: directory,
            db,
            owner,
            scope,
            state,
            budget: InputEvidenceBudgetLedger::new(Default::default())?,
        })
    }
    fn prepared(
        &self,
        fault: Option<TransitionFaultPoint>,
        noop: bool,
    ) -> Result<PreparedInputTransition> {
        let mut candidate = self.state.clone();
        if !noop {
            candidate.status = AgentStatus::Stopped;
        }
        PreparedInputTransition::posture(
            self.scope.clone(),
            "original-input-command".into(),
            "2026-10-13T00:00:00Z".parse()?,
            AgentPostureTransitionCommand {
                agent_id: self.scope.agent_id.clone(),
                expected_incarnation: Some(1),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(self.state.clone())),
                    record: Box::new(candidate),
                },
                audit_events: Vec::new(),
                fault,
            },
            None,
        )
    }
    fn begin(
        &self,
        command: &PreparedInputTransition,
    ) -> Result<(MutationReservation, MutationWitness)> {
        let witness = MutationWitness::input_mutation(
            command.descriptor().command_nonce().into(),
            OriginalInputMutationAttempt::new(command.descriptor().clone()),
            Vec::new(),
        )?;
        let mut reservation = self.owner.try_reserve()?;
        MutationReservation::begin_all_with_witness(
            std::slice::from_mut(&mut reservation),
            witness.operation_id().into(),
            witness.clone(),
        )?;
        Ok((reservation, witness))
    }
    fn resolve(&self, reservation: &mut MutationReservation) -> Result<bool> {
        resolve_input_recovery(
            &self.db,
            std::slice::from_mut(reservation),
            &self.budget,
            &ContextConfig::default(),
            &RuntimeModelCatalog::default(),
        )
    }
}

#[test]
fn positive_original_receipt_prepares_current_root_after_reacquiring_owner() -> Result<()> {
    let f = Fixture::new()?;
    let command = f.prepared(None, false)?;
    let (reservation, witness) = f.begin(&command)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&command)?;
    witness.finish_original_attempt();
    drop(reservation);
    let mut later = f.state.clone();
    later.turn_index = 9;
    f.db.agent_states().upsert_for_fixture(&later)?;
    let mut reservation = f.owner.try_reserve()?;
    assert!(f.resolve(&mut reservation)?);
    assert!(matches!(
        f.owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let known = MutationReservation::validate_group_known(std::slice::from_ref(&reservation))?;
    let Some(KnownCommit::State {
        state,
        episodes: Some(_),
        input_evidence: Some(_),
        runtime: Some(_),
        ..
    }) = &known[0]
    else {
        panic!("receipt must prepare every current input domain");
    };
    assert_eq!(state.as_ref(), &later);
    MutationReservation::reconcile_all(std::slice::from_mut(&mut reservation), known)?;
    let root = f.owner.capture()?.unwrap();
    assert_eq!(root.state.as_ref(), &later);
    root.input_evidence.as_ref().unwrap().capture()?.window(
        &crate::runtime::resident_input_evidence::InputWindowSelector::RecentBriefs,
        crate::runtime::scheduler::SCHEDULER_BRIEF_LIMIT,
    )?;
    Ok(())
}

#[test]
fn original_rollback_absence_preserves_published_root() -> Result<()> {
    let f = Fixture::new()?;
    let old = f.owner.capture()?.unwrap();
    let command = f.prepared(Some(TransitionFaultPoint::AfterCanonicalWrites), false)?;
    let (reservation, witness) = f.begin(&command)?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&command)
        .is_err());
    witness.finish_original_attempt();
    drop(reservation);
    let mut reservation = f.owner.try_reserve()?;
    assert!(f.resolve(&mut reservation)?);
    assert!(reservation.recovery()?.is_none());
    assert!(Arc::ptr_eq(&old, &f.owner.capture()?.unwrap()));
    Ok(())
}

#[test]
fn unfinished_original_attempt_cannot_use_positive_receipt() -> Result<()> {
    let f = Fixture::new()?;
    let command = f.prepared(None, true)?;
    let (mut reservation, witness) = f.begin(&command)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&command)?;
    assert!(!f.resolve(&mut reservation)?);
    assert!(reservation.recovery()?.unwrap().known.is_none());
    witness.finish_original_attempt();
    assert!(f.resolve(&mut reservation)?);
    assert!(reservation.recovery()?.unwrap().known.is_some());
    Ok(())
}

#[test]
fn corrupt_original_receipt_leaves_gate_unknown() -> Result<()> {
    let f = Fixture::new()?;
    let command = f.prepared(None, false)?;
    let (mut reservation, witness) = f.begin(&command)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&command)?;
    witness.finish_original_attempt();
    f.db.connection()?.execute(
        "UPDATE input_mutation_receipts SET result_digest=?1",
        ["f".repeat(64)],
    )?;
    assert!(f.resolve(&mut reservation).is_err());
    assert!(reservation.recovery()?.unwrap().known.is_none());
    assert!(matches!(
        f.owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    Ok(())
}

#[test]
fn lost_ack_receipt_in_same_deleting_incarnation_restores_state_and_all_domains() -> Result<()> {
    let f = Fixture::new()?;
    let command = f.prepared(None, false)?;
    let (reservation, witness) = f.begin(&command)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&command)?;
    witness.finish_original_attempt();
    drop(reservation);
    let mut identity = f.db.agent_identities().latest(&f.scope.agent_id)?.unwrap();
    identity.status = crate::types::AgentRegistryStatus::Deleting;
    f.db.agent_identities().upsert(&identity)?;
    let current = f.db.agent_states().latest(&f.scope.agent_id)?.unwrap();
    let before =
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| {
                r.get::<_, i64>(0)
            })?;
    let mut reservation = f.owner.try_reserve()?;
    assert!(f.resolve(&mut reservation)?);
    let known = MutationReservation::validate_group_known(std::slice::from_ref(&reservation))?;
    assert!(
        matches!(&known[0], Some(KnownCommit::State { state, episodes:Some(_), input_evidence:Some(_), runtime:Some(_), .. }) if state.as_ref() == &current)
    );
    MutationReservation::reconcile_all(std::slice::from_mut(&mut reservation), known)?;
    assert_eq!(f.owner.capture()?.unwrap().state.as_ref(), &current);
    assert_eq!(
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| r
                .get::<_, i64>(0))?,
        before
    );
    let snapshot = f.db.load_episode_memory_snapshot(
        &crate::runtime_db::episode_snapshot::EpisodeMemoryLoadScope {
            agent_id: f.scope.agent_id.clone(),
            identity_incarnation: 1,
        },
        8,
        None,
    )?;
    assert!(matches!(
        snapshot,
        crate::runtime_db::episode_snapshot::EpisodeMemoryBootstrapSnapshot::Retired { .. }
    ));
    Ok(())
}

#[path = "input_recovery_scheduling_tests.rs"]
mod scheduling_tests;
