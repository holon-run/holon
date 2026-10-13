use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        mutation_owner::{MutationOwner, MutationOwnerError},
        resident_input_evidence::{InputEvidenceFamily as Family, InputEvidenceKey as Key},
    },
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    },
};

struct Fixture {
    _dir: tempfile::TempDir,
    db: RuntimeDb,
    owner: MutationOwner,
    scope: InputEvidenceScope,
    budget: InputEvidenceBudgetLedger,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "preparation-agent".into(),
            identity_incarnation: 1,
        };
        let state = Arc::new(AgentState::new(&scope.agent_id));
        db.transaction(|tx| {
            upsert_agent_identity_tx(
                tx,
                &AgentIdentityRecord::new(
                    &scope.agent_id,
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
            )?;
            upsert_agent_state_tx(tx, &state)
        })?;
        let owner = MutationOwner::standalone(&scope.agent_id, scope.identity_incarnation);
        {
            let mut reservation = owner.reserve().await?;
            reservation.begin("initial-state".into())?;
            reservation.committed(state.clone())?;
            reservation.publish(state)?;
        }
        Ok(Self {
            _dir: dir,
            db,
            owner,
            scope,
            budget: InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
        })
    }

    fn request(&self) -> InputEvidenceLoadRequest {
        InputEvidenceLoadRequest {
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            record_limit: 512,
            byte_limit: 1024 * 1024,
        }
    }

    fn append(&self, qualify: bool) -> Result<MessageEnvelope> {
        self.db.transaction(|tx| {
            let message = MessageEnvelope::new(
                &self.scope.agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "exact requested dependency".into(),
                },
            );
            let (stored, inserted) = append_message_tx(tx, &message)?;
            assert!(inserted);
            if qualify {
                register_committed_input_evidence_tx(
                    tx,
                    &self.scope,
                    &[CanonicalInputEvidence::Message(stored.clone())],
                )?;
            }
            Ok(stored)
        })
    }

    fn prepare(
        &self,
        reservation: &MutationReservation,
        root: &AgentSnapshot,
        request: &InputEvidenceLoadRequest,
    ) -> Result<InputPreparation> {
        prepare_reserved_input_domains(
            &self.db,
            reservation,
            root,
            InputPreparationCause::ValidatedRecovery,
            request,
            12,
            &self.budget,
        )
    }

    fn publish(
        &self,
        reservation: &mut MutationReservation,
        prepared: PreparedInputDomains,
    ) -> Result<Arc<AgentSnapshot>> {
        reservation.begin("prepared-domain-install".into())?;
        reservation.committed_prepared_with_inputs(
            prepared.state.clone(),
            Some(prepared.episodes),
            Some(prepared.inputs),
        )?;
        Ok(reservation.publish(prepared.state)?)
    }
}

#[tokio::test]
async fn input_none_prepares_exact_domains_without_publishing_or_business_ticket() -> Result<()> {
    let fixture = Fixture::new().await?;
    let reservation = fixture.owner.reserve().await?;
    let original = fixture.owner.capture()?.unwrap();
    let InputPreparation::Prepared(prepared) =
        fixture.prepare(&reservation, &original, &fixture.request())?
    else {
        panic!("unprepared root incorrectly treated as ready")
    };
    assert_eq!(prepared.original_token, original.token);
    assert_eq!(prepared.facts.state.as_ref(), Some(original.state.as_ref()));
    assert_eq!(
        prepared.facts.identity.as_ref().unwrap().incarnation,
        fixture.scope.identity_incarnation
    );
    assert!(prepared.facts.control_revision.is_some());
    assert_eq!(prepared.queue.len(), 0);
    assert!(prepared.inputs.belongs_to(&fixture.scope));
    assert!(reservation.recovery()?.is_none());
    assert!(Arc::ptr_eq(&original, &fixture.owner.capture()?.unwrap()));
    assert!(original.input_evidence.is_none());
    Ok(())
}

#[tokio::test]
async fn already_prepared_request_does_not_query_durable_inputs() -> Result<()> {
    let fixture = Fixture::new().await?;
    let mut reservation = fixture.owner.reserve().await?;
    let original = fixture.owner.capture()?.unwrap();
    let InputPreparation::Prepared(prepared) =
        fixture.prepare(&reservation, &original, &fixture.request())?
    else {
        panic!("expected preparation")
    };
    let published = fixture.publish(&mut reservation, prepared)?;
    // Test-only corruption behind the owner proves this fast path performs no
    // SQL acquisition; legitimate canonical writers must hold this reservation.
    fixture.db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_states SET payload_json='invalid' WHERE agent_id=?1",
            [&fixture.scope.agent_id],
        )?;
        Ok(())
    })?;
    assert!(matches!(
        fixture.prepare(&reservation, &published, &fixture.request())?,
        InputPreparation::AlreadyPrepared
    ));
    Ok(())
}

#[tokio::test]
async fn unknown_business_ticket_is_not_reprepared_or_cleared() -> Result<()> {
    let fixture = Fixture::new().await?;
    let mut reservation = fixture.owner.reserve().await?;
    let original = fixture.owner.capture()?.unwrap();
    let key = reservation.begin("original-indeterminate-family".into())?;
    let error = fixture
        .prepare(&reservation, &original, &fixture.request())
        .err()
        .unwrap();
    assert!(matches!(error.downcast_ref::<MutationOwnerError>(),
        Some(MutationOwnerError::NeedsReconcile(actual)) if actual == &key));
    assert_eq!(reservation.recovery()?.unwrap().key, key);
    assert!(reservation.recovery()?.unwrap().committed_state.is_none());
    Ok(())
}

#[tokio::test]
async fn stale_publication_generation_cannot_prepare_even_with_equal_state() -> Result<()> {
    let fixture = Fixture::new().await?;
    let mut reservation = fixture.owner.reserve().await?;
    let old = fixture.owner.capture()?.unwrap();
    reservation.begin("same-state-new-generation".into())?;
    reservation.committed(old.state.clone())?;
    let new = reservation.publish(old.state.clone())?;
    assert_ne!(old.token, new.token);
    assert!(fixture
        .prepare(&reservation, &old, &fixture.request())
        .is_err());
    assert!(Arc::ptr_eq(&new, &fixture.owner.capture()?.unwrap()));
    assert!(reservation.recovery()?.is_none());
    Ok(())
}

#[tokio::test]
async fn exact_dependency_prepares_only_qualified_current_scope_and_failure_preserves_old_root(
) -> Result<()> {
    let fixture = Fixture::new().await?;
    let mut reservation = fixture.owner.reserve().await?;
    let old = fixture.owner.capture()?.unwrap();
    let InputPreparation::Prepared(prepared) =
        fixture.prepare(&reservation, &old, &fixture.request())?
    else {
        panic!("expected preparation")
    };
    let published = fixture.publish(&mut reservation, prepared)?;
    let qualified = fixture.append(true)?;
    let mut request = fixture.request();
    request
        .mandatory_roots
        .insert(Key::new(Family::Message, &qualified.id));
    let InputPreparation::Prepared(prepared) =
        fixture.prepare(&reservation, &published, &request)?
    else {
        panic!("missing exact dependency incorrectly treated as ready")
    };
    let captured = prepared.inputs.capture_for_request(&request)?;
    assert!(captured
        .lookup(&Key::new(Family::Message, &qualified.id))?
        .is_some());
    drop(captured);
    let unqualified = fixture.append(false)?;
    request
        .mandatory_roots
        .insert(Key::new(Family::Message, &unqualified.id));
    assert!(fixture.prepare(&reservation, &published, &request).is_err());
    assert!(Arc::ptr_eq(&published, &fixture.owner.capture()?.unwrap()));
    let count: i64 = fixture.db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id=?3",
        rusqlite::params![fixture.scope.agent_id, fixture.scope.identity_incarnation, unqualified.id], |r| r.get(0))?;
    assert_eq!(count, 0);
    assert!(reservation.recovery()?.is_none());
    Ok(())
}

#[tokio::test]
async fn changed_canonical_state_or_retired_identity_never_rebases_published_state() -> Result<()> {
    let fixture = Fixture::new().await?;
    let reservation = fixture.owner.reserve().await?;
    let old = fixture.owner.capture()?.unwrap();
    let mut changed = old.state.as_ref().clone();
    changed.turn_index += 1;
    fixture
        .db
        .transaction(|tx| upsert_agent_state_tx(tx, &changed))?;
    assert!(fixture
        .prepare(&reservation, &old, &fixture.request())
        .is_err());
    fixture.db.transaction(|tx| {
        let mut identity = AgentIdentityRecord::new(
            &fixture.scope.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.incarnation = 2;
        upsert_agent_identity_tx(tx, &identity)
    })?;
    let error = fixture
        .prepare(&reservation, &old, &fixture.request())
        .err()
        .unwrap();
    assert!(matches!(
        error
            .downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(
            ),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::RetiredScope)
    ));
    assert!(Arc::ptr_eq(&old, &fixture.owner.capture()?.unwrap()));
    assert_eq!(old.token.identity_incarnation, 1);
    assert!(reservation.recovery()?.is_none());
    Ok(())
}

#[tokio::test]
async fn malformed_pending_provider_recovery_survives_restart_without_hydration_grant() -> Result<()>
{
    use crate::{
        runtime::{
            tests::support::{context_config, StubProvider},
            RuntimeHandle,
        },
        types::{AdmissionContext, MessageDeliverySurface},
    };
    let home = tempfile::tempdir()?;
    let create = || {
        RuntimeHandle::new(
            "malformed-pending-restart",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "malformed-pending-restart".into(),
            context_config(),
        )
    };
    let runtime = create()?;
    let mut malformed = MessageEnvelope::new(
        "malformed-pending-restart",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "model_lineage_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "invalid directive must reach audited admission rejection".into(),
        },
    );
    malformed.delivery_surface = Some(MessageDeliverySurface::RuntimeSystem);
    malformed.admission_context = Some(AdmissionContext::RuntimeOwned);
    malformed.causation_id = Some("untrusted-source-message".into());
    malformed.source_refs.insert(
        "source_message_id".into(),
        "untrusted-source-message".into(),
    );
    malformed
        .source_refs
        .insert("source_turn_id".into(), "untrusted-source-turn".into());
    malformed.metadata = None;
    let queued = runtime.enqueue(malformed).await?;
    drop(runtime);
    let restarted = create()?;
    let mut mutation = restarted.begin_mutation().await?;
    mutation
        .prepare_owner_inputs(
            [Key::new(Family::Message, &queued.id)]
                .into_iter()
                .collect(),
            [queued.id.clone()].into_iter().collect(),
        )
        .await?;
    let inputs = mutation.capture_input_evidence()?;
    assert_eq!(mutation.state.pending, 1);
    assert!(mutation
        .queue
        .peek()
        .is_some_and(|message| message.id == queued.id));
    let record = inputs
        .lookup(&Key::new(Family::Message, &queued.id))?
        .expect("canonical malformed queued input missing");
    let crate::runtime::resident_input_evidence::PreparedInputEvidence::Message(stored) =
        &record.record
    else {
        panic!("expected canonical queued Message")
    };
    assert!(crate::runtime::provider_recovery::continuity_sources(stored).is_err());
    assert!(inputs
        .lookup(&Key::new(Family::Message, "untrusted-source-message"))
        .is_err());
    assert!(inputs
        .lookup(&Key::new(Family::Turn, "untrusted-source-turn"))
        .is_err());
    Ok(())
}
