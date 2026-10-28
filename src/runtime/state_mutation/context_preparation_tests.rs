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
async fn scheduling_context_request_rejects_binding_before_private_candidate_check() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    let before = mutation.capture_published_root()?;
    // A private candidate must not be published. Its large borrowed binding
    // distinguishes constructor admission from the later publication check.
    mutation.state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
        source_message_id: "x".repeat(4096),
        turn_id: "turn".into(),
        work_item_id: None,
        activation_id: None,
        admission_provenance: None,
        owner: None,
        claimed_work_revision: None,
    });
    let scope = mutation.input_evidence_scope();
    let material = ResourceReadMaterial::with_test_limits(
        &runtime.inner.input_evidence_budget,
        &scope,
        20_000,
        65536,
    )?;
    let capture = super::super::resident_scheduling::SchedulingCaptureRequest::current();
    let error = mutation
        .prepare_owner_scheduling_request_with_material(&capture, &material)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    assert!(material.finish().is_err());
    let after = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("published root missing after constructor rejection")?;
    assert_eq!(after.token, before.token);
    assert_eq!(after.state, before.state);
    drop(material);

    // No material remains active; the independent entry still reaches the
    // existing private-candidate rejection, not a leaked TLS budget.
    let error = mutation
        .prepare_owner_scheduling_request(&capture)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot publish a private candidate"),
        "{error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn scheduling_context_request_shares_constructor_and_downstream_remainder() -> Result<()> {
    use crate::diagnostics::resource_io::{admit_cloned_value, ResourceReadMaterial};
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    let scope = mutation.input_evidence_scope();
    let ledger = &runtime.inner.input_evidence_budget;
    let capture = super::super::resident_scheduling::SchedulingCaptureRequest::current();
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let version = runtime.inner.config_snapshot.load_full();
    assert!(version.provider_reconfig.is_none());
    let config = &version.base_context_config;
    let model = version
        .model_catalog
        .effective_model(mutation.state.model_override.as_ref());
    let history = super::super::turn::context_history_preparation(&mutation.state.id, &model);
    let constructor = ResourceReadMaterial::new(ledger, &scope)?;
    {
        let _active = constructor.enter();
        super::super::context_capture::context_input_evidence_request(
            &mutation.state,
            None,
            config,
            history,
            Default::default(),
            record_limit,
            byte_limit,
        )?;
    }
    let (constructor_bytes, constructor_records) = constructor.finish()?;
    drop(constructor);
    let before = mutation.capture_published_root()?;
    let limited = ResourceReadMaterial::with_test_limits(
        ledger,
        &scope,
        constructor_bytes,
        constructor_records,
    )?;
    let error = mutation
        .prepare_owner_scheduling_request_with_material(&capture, &limited)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    assert!(limited.finish().is_err());
    assert_eq!(mutation.capture_published_root()?.token, before.token);
    assert_eq!(mutation.state, *before.state);
    drop(limited);

    // Warm the original preparation, then measure its repeatable prepared path.
    let material = ResourceReadMaterial::new(ledger, &scope)?;
    mutation
        .prepare_owner_scheduling_request_with_material(&capture, &material)
        .await?;
    assert!(mutation.capture_scheduling_read(&capture).is_ok());
    assert!(material.finish()?.0 > constructor_bytes);
    drop(material);
    let measure = ResourceReadMaterial::new(ledger, &scope)?;
    mutation
        .prepare_owner_scheduling_request_with_material(&capture, &measure)
        .await?;
    let (bytes, records) = measure.finish()?;
    drop(measure);
    let before = mutation.capture_published_root()?;
    let pressure = ResourceReadMaterial::with_test_limits(ledger, &scope, bytes, records)?;
    {
        let _active = pressure.enter();
        admit_cloned_value(&"earlier Context material")?;
    }
    let error = mutation
        .prepare_owner_scheduling_request_with_material(&capture, &pressure)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    assert!(pressure.finish().is_err());
    assert_eq!(mutation.capture_published_root()?.token, before.token);
    drop(pressure);
    let exact = ResourceReadMaterial::with_test_limits(ledger, &scope, bytes, records)?;
    mutation
        .prepare_owner_scheduling_request_with_material(&capture, &exact)
        .await?;
    assert_eq!(exact.finish()?, (bytes, records));
    assert!(mutation.capture_scheduling_read(&capture).is_ok());
    let observed = ledger.report()?.observed_bytes;
    admit_cloned_value(&"outside Scheduling preparation".repeat(100_000))?;
    assert_eq!(ledger.report()?.observed_bytes, observed);
    let holder = exact.guard()?;
    drop(exact);
    assert_eq!(ledger.report()?.observed_bytes, observed);
    drop(holder);
    assert!(ledger.report()?.observed_bytes < observed);
    Ok(())
}

#[tokio::test]
async fn preparation_request_input_copies_share_remainder_and_preserve_publication_on_rejection(
) -> Result<()> {
    use crate::{
        diagnostics::resource_io::{admit_cloned_value, ResourceReadMaterial},
        runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    };
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    let scope = mutation.input_evidence_scope();
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let request = InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit,
        byte_limit,
    };
    let ledger = runtime.inner.input_evidence_budget.clone();
    let initial_coverage = mutation
        .capture_published_root()?
        .scheduling
        .as_ref()
        .context("initial Scheduling domain missing")?
        .coverage()
        .clone();
    let measure = ResourceReadMaterial::new(&ledger, &scope)?;
    mutation
        .prepare_owner_input_request_with_material(&request, 3, &measure)
        .await?;
    let (bytes, records) = measure.finish()?;
    drop(measure);
    let before = mutation.capture_published_root()?;
    assert_eq!(
        before.scheduling.as_ref().unwrap().coverage(),
        &initial_coverage
    );
    // The installed resident now retains the first preparation's phase.
    // Its next capture adds one source-guard Arc slot to the same views Vec.
    // The JSON projections are unchanged; count only this independent delta.
    let bytes = bytes
        + u64::try_from(std::mem::size_of::<
            Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
        >())?
            * 8;
    let records = records + 8;
    let pressure = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
    {
        let _active = pressure.enter();
        admit_cloned_value(&request)?;
    }
    let error = mutation
        .prepare_owner_input_request_with_material(&request, 4, &pressure)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    assert!(pressure.finish().is_err());
    assert_eq!(mutation.capture_published_root()?.token, before.token);
    assert_eq!(mutation.state, *before.state);
    drop(pressure);

    // The same limits without the earlier copy permit the actual preparation.
    let exact = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
    mutation
        .prepare_owner_input_request_with_material(&request, 4, &exact)
        .await?;
    assert!(mutation.capture_context_with_inputs(4, &request).is_ok());
    assert!(exact.finish()?.0 > 0);
    let observed = ledger.report()?.observed_bytes;
    // No synchronous TLS entry may leak out of the async preparation.
    admit_cloned_value(&"outside preparation".repeat(100_000))?;
    assert_eq!(ledger.report()?.observed_bytes, observed);
    let holder = exact.guard()?;
    let weak = Arc::downgrade(&holder);
    drop(exact);
    assert_eq!(ledger.report()?.observed_bytes, observed);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, observed);
    assert!(weak.upgrade().is_some());
    drop(before);
    drop(mutation);
    // The installed resident still owns the phase after the mutation releases it.
    assert!(weak.upgrade().is_some());
    drop(runtime);
    assert!(weak.upgrade().is_none());
    assert!(ledger.report()?.observed_bytes < observed);
    Ok(())
}

#[test]
fn preparation_request_root_insertion_admits_duplicate_temporary_keys() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
        runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: "request-roots".into(),
        identity_incarnation: 1,
    };
    let mut request = InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 512,
        byte_limit: 1024 * 1024,
    };
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let before = phase.finish()?;
    {
        let _active = phase.enter();
        request.insert_admitted_root(Family::Turn, "same-turn")?;
    }
    let first = phase.finish()?;
    {
        let _active = phase.enter();
        request.insert_admitted_root(Family::Turn, "same-turn")?;
    }
    let second = phase.finish()?;
    assert_eq!(request.mandatory_roots.len(), 1);
    assert_eq!(second.0 - first.0, first.0 - before.0);
    assert_eq!(second.1 - first.1, first.1 - before.1);
    drop(phase);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, first.0, first.1)?;
    {
        let _active = phase.enter();
        assert!(request
            .insert_admitted_root(Family::Message, &"large-id".repeat(4096))
            .unwrap_err()
            .to_string()
            .contains("ResourceExhausted"));
    }
    assert_eq!(request.mandatory_roots.len(), 1);
    drop(phase);
    // Independent owner paths without a material retain their original behavior.
    request.insert_admitted_root(Family::Message, &"large-id".repeat(4096))?;
    assert_eq!(request.mandatory_roots.len(), 2);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
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
        query: None,
        windows: Vec::new(),
        mandatory_roots: std::collections::BTreeSet::from([Key::new(
            Family::Message,
            &retained.id,
        )]),
        selected_completion_brief_ids: Default::default(),
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
