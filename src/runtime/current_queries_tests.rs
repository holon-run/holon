use super::*;
use crate::{
    runtime::tests::support::LifecycleHarness, runtime_db::current_query::CurrentSelection,
    types::AuditEvent,
};

#[tokio::test]
async fn qualified_stream_loaded_install_preserves_single_record_notification_without_root(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let bus = crate::storage::EventBus::new(8);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut rx = bus.subscribe();
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "loaded-qualified-stream",
            serde_json::json!({"original": true}),
        ))
        .await?;
    let published = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await??;
    let record = published
        .qualified
        .as_ref()
        .context("installed Current proof was stripped")?;
    assert!(published.event.current_record().is_some());
    record.validate(
        &super::resident_input_evidence::InputEvidenceScope {
            agent_id: runtime.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: runtime.inner.identity_incarnation,
        },
        &runtime.inner.input_evidence_budget,
    )?;
    assert!(std::ptr::eq(record.event(), &*published.event));
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let weak = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "next-qualified-stream",
            serde_json::json!({}),
        ))
        .await?;
    assert!(
        weak.upgrade().is_none(),
        "notification may retain its fact, never the original Root"
    );
    assert_eq!(record.event().kind, "loaded-qualified-stream");
    drop((published, rx, session));
    Ok(())
}

#[tokio::test]
async fn current_preparation_keeps_live_union_and_recovers_missing_domain() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    runtime
        .append_original_audit_event(AuditEvent::legacy("retained-role", serde_json::json!({})))
        .await?;
    let cursor = runtime.capture_run_once(&session)?.current.event_cursor;
    let request = CurrentSelection {
        events_after: Some(cursor),
        ..Default::default()
    };
    let mut mutation = runtime.begin_mutation().await?;
    mutation.prepare_current_query(Some(&request)).await?;
    assert_eq!(runtime.capture_run_once(&session)?.events().count(), 1);

    // Expanding Input must prepare the live Current union in the same cut.
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let input = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        query: Some(super::resident_input_evidence::InputQueryRead {
            selection: Default::default(),
            ledger: runtime.inner.input_evidence_budget.clone(),
        }),
        windows: vec![
            crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                selector: super::resident_input_evidence::InputWindowSelector::RecentTranscript,
                limit: 1,
            },
        ],
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit,
        byte_limit,
    };
    assert!(runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .input_evidence
        .as_ref()
        .unwrap()
        .capture_for_request(&input)
        .is_err());
    mutation.prepare_owner_input_request(&input, 0).await?;
    assert_eq!(runtime.capture_run_once(&session)?.events().count(), 1);
    let prepared_root = runtime.inner.mutation_owner.capture()?.unwrap();
    let prepared_query = prepared_root
        .current
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap();
    assert!(prepared_query.selection.covers(&request));
    let generation = prepared_root.token.generation;
    mutation.prepare_current_query(None).await?;
    assert!(
        runtime
            .inner
            .mutation_owner
            .capture()?
            .unwrap()
            .token
            .generation
            > generation
    );
    drop(mutation);
    assert_eq!(
        runtime
            .capture_run_once(&session)?
            .events()
            .next()
            .unwrap()
            .kind,
        "retained-role"
    );
    Ok(())
}

#[tokio::test]
async fn current_frozen_roles_reject_selector_drift() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let query = root.current.as_ref().unwrap().query.as_ref().unwrap();
    let source = runtime
        .inner
        .current_queries
        .source(
            &query.scope,
            root.token.owner_epoch,
            Some(query),
            &runtime.inner.input_evidence_budget,
        )?
        .unwrap();
    source
        .roles
        .validate(&query.scope, root.token.owner_epoch, &source.selection)?;
    let mut different = source.selection.as_ref().clone();
    different.queue_ids.insert("unaccepted-address".into());
    let error = source
        .roles
        .validate(&query.scope, root.token.owner_epoch, &different)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("selection differs"),
        "{error:#}"
    );
    drop(session);
    Ok(())
}

#[tokio::test]
async fn current_install_fence_rejection_retains_known_without_readback() -> Result<()> {
    use crate::runtime::mutation_owner::{KnownCommit, MutationReservation};
    use crate::runtime_db::{
        current_query::CurrentQueryPublication,
        current_read_writes::CommittedCurrentWrites,
        transitions::{PostCommitEffects, TransitionCommit},
    };
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, _session) = runtime.begin_task_birth_session().await?;
    let prior = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = prior
        .current
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .clone();
    let mut mutation = runtime.begin_mutation().await?;
    mutation.prepare_current_query(None).await?;
    let next = runtime.inner.mutation_owner.capture()?.unwrap();
    let query = next
        .current
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .clone();
    assert!(!Arc::ptr_eq(&baseline, &query));
    let mut current = CommittedCurrentWrites::prepared_query(query.clone());
    current.query_publication = Some(Arc::new(CurrentQueryPublication {
        baseline,
        next: query,
    }));
    let event = AuditEvent::legacy("committed-before-install-fence", serde_json::json!({}));
    let storage = runtime.storage().clone();
    let error = mutation
        .commit_transition(event.id.clone(), |_| {
            storage.append_event(&event)?;
            Ok(TransitionCommit {
                applied: true,
                effects: Box::new(PostCommitEffects {
                    current_read_writes: Some(current),
                    audit_events: vec![crate::runtime_db::transitions::AuditEventAck::new(
                        event.clone(),
                    )?],
                    ..Default::default()
                }),
                ..Default::default()
            })
        })
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("another baseline"),
        "{error:#}"
    );
    assert!(runtime
        .runtime_db()
        .audit_events()
        .has_event_by_id(&event.id)?);
    drop(mutation);
    let mut reservations = vec![runtime.inner.mutation_owner.reserve().await?];
    let known = MutationReservation::validate_group_known(&reservations)?;
    let saved = known[0].as_ref().context("known rejected projection")?;
    let KnownCommit::State {
        runtime: Some(facts),
        ..
    } = saved
    else {
        panic!("acknowledged commit lost its Known facts");
    };
    assert!(facts.current.is_none());
    assert_eq!(facts.external_effects().audit_events[0].id, event.id);
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE audit_events RENAME TO current_fence_readback_forbidden;")?;
        Ok(())
    })?;
    runtime
        .install_saved_mutation(&reservations[0], saved)
        .await?;
    MutationReservation::reconcile_all(&mut reservations, known)?;
    assert!(runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .current
        .is_none());
    Ok(())
}

#[tokio::test]
async fn current_event_interval_is_same_cut_pure_and_does_not_retain_old_root() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    runtime
        .append_original_audit_event(AuditEvent::legacy("before", serde_json::json!({})))
        .await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let initial = runtime.capture_run_once(&session)?;
    assert_eq!(initial.events().count(), 0);
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "first",
            serde_json::json!({"text":"原文"}),
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let weak = Arc::downgrade(&root);
    let read = runtime.capture_run_once(&session)?;
    assert!(Arc::ptr_eq(&read.state, &root.state));
    assert_eq!(
        read.events()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
    runtime
        .append_original_audit_event(AuditEvent::legacy("second", serde_json::json!({})))
        .await?;
    drop(root);
    assert!(weak.upgrade().is_none());
    assert_eq!(read.events().count(), 1);
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE audit_events RENAME TO events_current_forbidden;
             ALTER TABLE current_read_qualification RENAME TO qualification_current_forbidden;
             ALTER TABLE current_read_revisions RENAME TO revisions_current_forbidden;",
        )?;
        Ok(())
    })?;
    for _ in 0..4 {
        let captured = runtime.capture_run_once(&session)?;
        assert_eq!(
            captured
                .events()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }
    Ok(())
}

#[tokio::test]
async fn current_event_role_exit_trims_query_and_last_holder_releases_body() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, early) = runtime.begin_task_birth_session().await?;
    runtime
        .append_original_audit_event(AuditEvent::legacy("early", serde_json::json!({})))
        .await?;
    let read = runtime.capture_run_once(&early)?;
    let guard = read.current.events[0].weak_material();
    let (_, late) = runtime.begin_task_birth_session().await?;
    drop(early);
    runtime
        .append_original_audit_event(AuditEvent::legacy("late", serde_json::json!({})))
        .await?;
    assert_eq!(
        runtime
            .capture_run_once(&late)?
            .events()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["late"]
    );
    assert!(guard.upgrade().is_some());
    drop(read);
    assert!(
        guard.upgrade().is_none(),
        "Current query retained an exited role or installation baseline"
    );
    Ok(())
}

#[tokio::test]
async fn current_original_source_rejects_raw_or_changed_body_before_business_sql() -> Result<()> {
    for raw_insert in [false, true] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let (_, session) = runtime.begin_task_birth_session().await?;
        let first = AuditEvent::legacy("accepted", serde_json::json!({}));
        runtime.append_original_audit_event(first.clone()).await?;
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        if raw_insert {
            runtime
                .storage()
                .append_event(&AuditEvent::legacy("raw", serde_json::json!({})))?;
        } else {
            runtime.runtime_db().transaction(|tx| {
                tx.execute("UPDATE audit_events SET data_json=json_set(data_json,'$.kind','tampered') WHERE audit_event_id=?1", [&first.id])?;
                Ok(())
            })?;
        }
        let next = AuditEvent::legacy("must-not-commit", serde_json::json!({}));
        let error = runtime
            .append_original_audit_event(next.clone())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("Current"), "{error:#}");
        assert!(!runtime
            .runtime_db()
            .audit_events()
            .has_event_by_id(&next.id)?);
        assert_eq!(
            runtime.inner.mutation_owner.capture()?.unwrap().state,
            before.state
        );
        assert_eq!(
            runtime
                .capture_run_once(&session)?
                .events()
                .next()
                .unwrap()
                .kind,
            "accepted"
        );
        let original = runtime.inner.mutation_owner.capture()?.unwrap();
        let mut mutation = runtime.begin_mutation().await?;
        let error = mutation
            .prepare_owner_inputs(Default::default(), Default::default())
            .await
            .err()
            .context("Input preparation accepted unqualified Current history")?;
        assert!(format!("{error:#}").contains("Current"), "{error:#}");
        assert!(
            Arc::ptr_eq(&original, &runtime.inner.mutation_owner.capture()?.unwrap()),
            "failed preparation published a partial Root"
        );
    }
    Ok(())
}

#[tokio::test]
async fn current_explicit_interval_source_checks_every_body_and_foreign_ledger() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let frontier = runtime.capture_run_once(&session)?.current.event_cursor;
    runtime
        .append_original_audit_event(AuditEvent::legacy("qualified", serde_json::json!({})))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let query = root.current.as_ref().unwrap().query.as_ref().unwrap();
    let scope = query.scope.clone();
    let selection = CurrentSelection {
        events_after: Some(frontier),
        ..Default::default()
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::new(ledger, &scope)?;
    let prepared = material.read(|| {
        runtime.runtime_db().transaction(|tx| {
            crate::runtime_db::current_query::prepare(
                tx,
                &scope,
                Some(&selection),
                ledger,
                (16 * 1024 * 1024, 65_536),
            )
        })
    })?;
    assert_eq!(
        prepared
            .capture(&selection, ledger)?
            .events()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["qualified"]
    );
    let foreign = super::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(prepared.capture(&selection, &foreign).is_err());
    runtime
        .storage()
        .append_event(&AuditEvent::legacy("raw", serde_json::json!({})))?;
    let rejected = material.read(|| {
        runtime.runtime_db().transaction(|tx| {
            crate::runtime_db::current_query::prepare(
                tx,
                &scope,
                Some(&selection),
                ledger,
                (16 * 1024 * 1024, 65_536),
            )
        })
    });
    assert!(format!("{:#}", rejected.unwrap_err()).contains("original writer preparation"));
    drop(session);
    Ok(())
}
