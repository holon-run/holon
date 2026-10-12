use super::*;
use crate::{
    context::ContextHistoryPreparation,
    runtime::{
        resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, InputWindowSelector,
        },
        tests::support::{context_config, tempdir, StubProvider},
    },
    runtime_db::{
        evidence::append_message_tx,
        input_evidence_snapshot::{
            register_committed_input_evidence_tx, CanonicalInputEvidence, InputWindowRequest,
        },
        repositories::upsert_turn_record_tx,
    },
    types::TurnOwner,
};

fn fixture() -> (tempfile::TempDir, RuntimeHandle) {
    let home = tempdir().unwrap();
    let mut config = context_config();
    config.recent_messages = 2;
    config.recent_briefs = 1;
    config.recent_episode_candidates = 2;
    let runtime = RuntimeHandle::new(
        "exact-context-preparation",
        home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "exact-context-preparation".into(),
        config,
    )
    .unwrap();
    (home, runtime)
}

#[tokio::test]
async fn frozen_context_request_prepares_expanded_history_and_episode_limit() -> Result<()> {
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    let scope = mutation.input_evidence_scope();
    let owner = TurnOwner::Conversation {
        interaction_id: "fallback-owner-window".into(),
    };
    runtime.inner.runtime_db.transaction(|tx| {
        for index in 1..=6 {
            let mut turn = TurnRecord::new(
                &scope.agent_id,
                format!("turn:frozen-context:{index}"),
                index,
            );
            if index <= 3 {
                turn.owner = Some(owner.clone());
            }
            upsert_turn_record_tx(tx, &turn)?;
            register_committed_input_evidence_tx(
                tx,
                &scope,
                &[CanonicalInputEvidence::Turn(turn)],
            )?;
        }
        Ok(())
    })?;
    mutation
        .prepare_owner_inputs(Default::default(), Default::default())
        .await?;
    let before = mutation.state.clone();
    let mut frozen_config = context_config();
    frozen_config.recent_messages = 6;
    frozen_config.recent_briefs = 4;
    frozen_config.recent_episode_candidates = 5;
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let mut request = super::super::context_capture::context_input_evidence_request(
        &mutation.state,
        None,
        &frozen_config,
        ContextHistoryPreparation::default(),
        Default::default(),
        record_limit,
        byte_limit,
    )?;
    request.windows.push(InputWindowRequest {
        selector: InputWindowSelector::OwnerTurns {
            owner: owner.clone(),
        },
        limit: 4,
    });
    assert!(
        mutation
            .input_evidence
            .as_ref()
            .unwrap()
            .capture_for_request(&request)
            .is_err(),
        "the default model's smaller window must not certify this frozen request"
    );
    assert!(mutation
        .capture_context(frozen_config.recent_episode_candidates)
        .is_err());

    // A later config publication cannot replace this explicit request. The loaded
    // caller separately retries if its frozen version changes before returning.
    let mut replacement = (*runtime.inner.config_snapshot.load_full()).clone();
    replacement.base_context_config.recent_messages = 1;
    replacement.base_context_config.recent_episode_candidates = 1;
    runtime.inner.config_snapshot.store(Arc::new(replacement));
    mutation
        .prepare_owner_input_request(&request, frozen_config.recent_episode_candidates)
        .await?;
    let captured =
        mutation.capture_context_with_inputs(frozen_config.recent_episode_candidates, &request)?;
    assert_eq!(captured.state.as_ref(), &before);
    assert_eq!(captured.episode_candidate_limit, 5);
    let inputs = captured.input_evidence.as_ref().unwrap();
    assert_eq!(
        inputs.window(&InputWindowSelector::GlobalTurns, 7)?.len(),
        6
    );
    assert_eq!(
        inputs
            .window(&InputWindowSelector::OwnerTurns { owner }, 4)?
            .len(),
        3
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest(&before.id)?
            .as_ref(),
        Some(&before)
    );
    Ok(())
}

#[tokio::test]
async fn exact_request_keeps_accepted_and_existing_owner_roots_without_mutating_request(
) -> Result<()> {
    let (_home, runtime) = fixture();
    let accepted = runtime
        .enqueue(MessageEnvelope::new(
            "exact-context-preparation",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "accepted body remains mandatory".into(),
            },
        ))
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let scope = mutation.input_evidence_scope();
    let retained = runtime.inner.runtime_db.transaction(|tx| {
        let message = MessageEnvelope::new(
            &scope.agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original owner dependency".into(),
            },
        );
        let (message, _) = append_message_tx(tx, &message)?;
        register_committed_input_evidence_tx(
            tx,
            &scope,
            &[CanonicalInputEvidence::Message(message.clone())],
        )?;
        Ok(message)
    })?;
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let mut request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: std::collections::BTreeSet::from([Key::new(
            Family::Message,
            &retained.id,
        )]),
        interrupted_sources: Default::default(),
        record_limit,
        byte_limit,
    };
    mutation.prepare_owner_input_request(&request, 2).await?;
    request.mandatory_roots.clear();
    request.windows.push(InputWindowRequest {
        selector: InputWindowSelector::GlobalTurns,
        limit: 7,
    });
    mutation.prepare_owner_input_request(&request, 3).await?;
    assert!(
        request.mandatory_roots.is_empty(),
        "the caller's frozen selection is immutable"
    );
    let domain = mutation.input_evidence.as_ref().unwrap();
    let captured = domain.capture()?;
    for message in [&accepted, &retained] {
        let key = Key::new(Family::Message, &message.id);
        assert!(domain.mandatory_roots().contains(&key));
        assert!(captured.lookup(&key)?.is_some());
    }
    assert_eq!(mutation.state.pending, 1);
    assert_eq!(mutation.queue.len(), 1);
    Ok(())
}
