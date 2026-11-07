use super::*;
use crate::{
    runtime::{
        mutation_owner::{MutationOwner, MutationOwnerError},
        mutation_witness::MutationWitness,
        resident_input_evidence::InputEvidenceScope,
        tests::support::{context_config, StubProvider},
        RuntimeHandle,
    },
    runtime_db::{
        input_mutation_receipts::OriginalInputMutationAttempt,
        prepared_input_evidence::InputEvidenceWrite, transitions::InputMutationExecution,
    },
    types::{brief_created_event_for, BriefKind, BriefRecord},
};

fn runtime_fixture() -> Result<(tempfile::TempDir, tempfile::TempDir, RuntimeHandle)> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )?;
    runtime
        .storage()
        .enable_event_bus(crate::storage::EventBus::new(32))?;
    Ok((home, workspace, runtime))
}

#[tokio::test]
async fn successful_brief_replay_returns_original_result_without_republishing_event() -> Result<()>
{
    let (_home, _workspace, runtime) = runtime_fixture()?;
    let mut receiver = runtime
        .storage()
        .subscribe_events()?
        .context("fixture event receiver")?;
    let brief = BriefRecord::new("default", BriefKind::Ack, "original brief", None, None);
    runtime.persist_brief(&brief).await?;
    let mut first = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if event.event.kind == "brief_created" {
            first.push(event.event);
        }
    }
    assert_eq!(first.len(), 1);
    let before = runtime
        .storage()
        .read_brief_by_id(&brief.id)?
        .context("original Brief")?;
    runtime.persist_brief(&brief).await?;
    assert_eq!(
        runtime.storage().read_brief_by_id(&brief.id)?,
        Some(before.clone())
    );
    assert_eq!(
        runtime.agent_state().await?.last_brief_at,
        Some(before.created_at)
    );
    while let Ok(event) = receiver.try_recv() {
        assert_ne!(
            event.event.kind, "brief_created",
            "observed replay cannot resend old audit"
        );
    }
    assert_eq!(
        runtime
            .storage()
            .read_recent_events(32)?
            .iter()
            .filter(|event| event.kind == "brief_created")
            .count(),
        1
    );
    assert_eq!(
        runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [&first[0].id],
            |row| row.get::<_, i64>(0)
        )?,
        1
    );
    assert!(runtime.inner.mutation_owner.capture().is_ok());
    Ok(())
}

#[tokio::test]
async fn real_lost_ack_without_observed_replay_still_publishes_original_brief_event() -> Result<()>
{
    let (_home, _workspace, runtime) = runtime_fixture()?;
    let mut receiver = runtime
        .storage()
        .subscribe_events()?
        .context("fixture event receiver")?;
    let brief = BriefRecord::new("default", BriefKind::Ack, "lost ACK Brief", None, None);
    let event = brief_created_event_for(&brief)?;
    let scope = InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: runtime.instance_incarnation(),
    };
    let prepared = runtime.storage().prepare_input_evidence(
        scope,
        event.id.clone(),
        brief.created_at,
        InputEvidenceWrite::BriefCreated {
            brief: brief.clone(),
            event: event.clone(),
        },
    )?;
    let witness = MutationWitness::input_mutation(
        event.id.clone(),
        OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
        Vec::new(),
    )?;
    {
        let mut permits =
            MutationOwner::reserve_all(vec![runtime.inner.mutation_owner.clone()]).await?;
        MutationReservation::begin_all_with_witness(
            &mut permits,
            event.id.clone(),
            witness.clone(),
        )?;
        assert!(matches!(
            runtime
                .storage()
                .commit_prepared_input_evidence(&prepared)?,
            InputMutationExecution::Committed { .. }
        ));
        // Real SQL committed, but the original ACK never became Known facts.
        // There is deliberately no typed Replayed observation on this attempt.
        witness.finish_original_attempt();
    }
    assert!(!witness.input_storage_replayed());
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(receiver.try_recv().is_err());
    runtime.reconcile_posture_observation().await?;
    let mut published = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if event.event.kind == "brief_created" {
            published.push(event.event);
        }
    }
    let stored = runtime
        .storage()
        .read_recent_events(32)?
        .into_iter()
        .filter(|stored| stored.id == event.id)
        .collect::<Vec<_>>();
    assert_eq!(stored.len(), 1);
    assert_eq!(published, stored);
    runtime.reconcile_posture_observation().await?;
    while let Ok(event) = receiver.try_recv() {
        assert_ne!(
            event.event.kind, "brief_created",
            "completed restoration cannot resend"
        );
    }
    assert_eq!(
        runtime
            .storage()
            .read_brief_by_id(&brief.id)?
            .unwrap()
            .created_event_seq,
        Some(stored[0].event_seq)
    );
    assert!(runtime.inner.mutation_owner.capture().is_ok());
    Ok(())
}
