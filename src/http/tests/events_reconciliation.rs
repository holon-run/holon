use super::*;
use crate::{
    runtime::{
        mutation_owner::{
            MutationOwner, MutationOwnerError, MutationReservation, RecoveryPhase,
            WitnessUnknownResolution,
        },
        mutation_witness::MutationWitness,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::{
        current_query::append_qualified_event_for_fixture,
        input_mutation_receipts::{
            InputMutationFamily, OriginalInputMutationAttempt, PreparedInputMutation,
        },
    },
    storage::EventLogPageOrder,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuditEvent,
    },
};
use serde_json::Value;

fn history(host: &RuntimeHost, scope: &InputEvidenceScope) {
    let ledger = host.bridge().input_evidence_budget().unwrap();
    for index in 0..4 {
        append_qualified_event_for_fixture(
            host.runtime_db(),
            scope,
            &ledger,
            &AuditEvent::legacy(
                "reconciliation-history",
                serde_json::json!({"index": index}),
            ),
        )
        .unwrap();
    }
}

async fn page(app: &axum::Router, agent_id: &str) -> axum::response::Response {
    page_query(app, agent_id, "order=asc&limit=2").await
}

async fn page_query(app: &axum::Router, agent_id: &str, query: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/agents/{agent_id}/events?event_kind=reconciliation-history&{query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn indexes(page: &Value) -> Vec<u64> {
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["payload"]["index"].as_u64().unwrap())
        .collect()
}

async fn continuous_pages(app: &axum::Router, agent_id: &str, before: &Value) {
    for (order, expected) in [("asc", vec![0, 1, 2, 3]), ("desc", vec![3, 2, 1, 0])] {
        let response = page_query(app, agent_id, &format!("order={order}&limit=2")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let first = body(response).await;
        assert_eq!(first["event_log_epoch"], before["event_log_epoch"]);
        assert_eq!(first["cursor_seq"], before["cursor_seq"]);
        let (boundary, sequence, remaining, exhausted) = if order == "asc" {
            ("after_seq", &first["newest_seq"], "has_newer", "has_older")
        } else {
            ("before_seq", &first["oldest_seq"], "has_older", "has_newer")
        };
        assert_eq!(first[remaining], true);
        assert_eq!(first[exhausted], false);
        let response = page_query(
            app,
            agent_id,
            &format!("order={order}&limit=2&{boundary}={sequence}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let next = body(response).await;
        assert_eq!(next["event_log_epoch"], first["event_log_epoch"]);
        assert_eq!(next["cursor_seq"], first["cursor_seq"]);
        assert_eq!(next[remaining], false);
        assert_eq!(next[exhausted], false);
        let mut all = indexes(&first);
        all.extend(indexes(&next));
        assert_eq!(
            all, expected,
            "adjacent pages must not lose or repeat events"
        );
    }
}

async fn pending_events(host: &RuntimeHost, scope: InputEvidenceScope) {
    history(host, &scope);
    let other = AgentIdentityRecord::new(
        "events-other",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(scope.agent_id.clone()),
        None,
    );
    host.append_agent_identity(&other).unwrap();
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&AgentState::new(&other.agent_id))
        .unwrap();
    history(
        host,
        &InputEvidenceScope {
            agent_id: other.agent_id.clone(),
            identity_incarnation: other.incarnation,
        },
    );
    let app = router(AppState::for_unix(host.clone()));
    let response = page(&app, &scope.agent_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let before = body(response).await;
    assert_eq!(indexes(&before), vec![0, 1]);
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)
        .unwrap();
    let token = owner.capture_with_token().unwrap().0;
    let witness = MutationWitness::input_mutation(
        "private-events-ticket".into(),
        OriginalInputMutationAttempt::new(
            PreparedInputMutation::new(
                InputMutationFamily::Queue,
                "private-events-ticket".into(),
                vec![scope.clone()],
                "a".repeat(64),
                chrono::Utc::now(),
            )
            .unwrap(),
        ),
        Vec::new(),
    )
    .unwrap();
    let key = {
        let mut reservations = MutationOwner::reserve_all(vec![owner.clone()])
            .await
            .unwrap();
        MutationReservation::begin_all_with_witness(
            &mut reservations,
            "private-events-ticket".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0)
    };
    // The real Host reservation/recovery path must preserve the typed source.
    let error = host
        .operator_event_page(
            &scope.agent_id,
            None,
            None,
            2,
            EventLogPageOrder::Asc,
            |_| Ok(|_: &AuditEvent| true),
        )
        .await
        .err()
        .expect("unfinished original attempt must fence the read");
    let crate::host::PublicAgentError::Runtime(error) = error else {
        panic!("expected a typed runtime error");
    };
    assert!(matches!(
        error.downcast_ref::<MutationOwnerError>(),
        Some(MutationOwnerError::NeedsReconcile(pending)) if pending == &key
    ));
    assert_eq!(page(&app, &other.agent_id).await.status(), StatusCode::OK);

    let response = page(&app, &scope.agent_id).await;
    let status = response.status();
    let retry_after = response.headers().get(header::RETRY_AFTER).cloned();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(retry_after.unwrap(), "1");
    assert_eq!(body["code"], "agent_reconciliation_pending");
    assert_eq!(body["domain"], "runtime");
    assert_eq!(body["retryable"], true);
    assert!(!body.to_string().contains("private-events-ticket"));
    assert!(body.get("events").is_none() && body.get("cursor_seq").is_none());

    let mut reservations = MutationOwner::reserve_all(vec![owner.clone()])
        .await
        .unwrap();
    let recovery = reservations[0].recovery().unwrap().unwrap();
    assert_eq!(recovery.key, key);
    assert_eq!(recovery.phase, RecoveryPhase::NeedsReconcile);
    assert!(recovery.known.is_none() && recovery.committed_state.is_none());
    let original = recovery.witness.unwrap();
    assert!(original.same_attempt(&witness));
    assert!(!original.original_attempt_finished());
    assert!(matches!(
        owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(pending)) if pending == key
    ));
    // Only the original producer ends its no-write attempt, outside the GET.
    witness.finish_original_attempt();
    assert_eq!(
        MutationReservation::resolve_input_unknown(&mut reservations, &key, &witness, None)
            .unwrap(),
        WitnessUnknownResolution::NotCommitted
    );
    drop(reservations);
    assert_eq!(owner.capture_with_token().unwrap().0, token);
    continuous_pages(&app, &scope.agent_id, &before).await;
    assert!(host.try_get_loaded_runtime(&other.agent_id).await.is_none());
}

#[tokio::test]
async fn operator_events_route_cold_pending_reconciliation() {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity().unwrap();
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&AgentState::new(&identity.agent_id))
        .unwrap();
    pending_events(
        &host,
        InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        },
    )
    .await;
    assert!(host
        .try_get_loaded_runtime(&identity.agent_id)
        .await
        .is_none());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn operator_events_route_loaded_pending_reconciliation() {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources_for_test().await;
    let run_loop = runtime.pause_run_loop_for_monitor_test().await;
    pending_events(
        &host,
        InputEvidenceScope {
            agent_id: runtime.instance_agent_id().into(),
            identity_incarnation: runtime.instance_incarnation(),
        },
    )
    .await;
    drop(run_loop);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn operator_events_route_waits_for_original_committing_owner() {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity().unwrap();
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&AgentState::new(&identity.agent_id))
        .unwrap();
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    history(&host, &scope);
    let app = router(AppState::for_unix(host.clone()));
    let response = page(&app, &identity.agent_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let before = body(response).await;
    let owner = host
        .bridge()
        .mutation_owner_for(&identity.agent_id, identity.incarnation)
        .unwrap();
    let mut reservation = owner.reserve().await.unwrap();
    let key = reservation.begin("original-committing".into()).unwrap();
    let agent_id = identity.agent_id.clone();
    let request = tokio::spawn(async move { page(&app, &agent_id).await });
    // A test-only signal is emitted only after the actual lock future is Pending.
    tokio::time::timeout(
        Duration::from_secs(5),
        owner.wait_for_reservation_waiter_for_test(),
    )
    .await
    .expect("the real HTTP read must enter reservation waiting");
    assert!(!request.is_finished());
    let recovery = reservation.recovery().unwrap().unwrap();
    assert_eq!(recovery.key, key);
    assert_eq!(recovery.phase, RecoveryPhase::Committing);
    assert!(recovery.known.is_none());
    // Resolve with the original permit, never a replacement ticket or the GET.
    MutationReservation::definitely_not_committed_all(std::slice::from_mut(&mut reservation))
        .unwrap();
    drop(reservation);
    let response = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, before);
    assert!(host
        .try_get_loaded_runtime(&identity.agent_id)
        .await
        .is_none());
    host.shutdown().await.unwrap();
}

#[test]
fn read_reconciliation_error_preserves_other_errors() {
    use crate::{host::PublicAgentError, http::AgentReadSurface};
    let cases: [(&str, fn() -> anyhow::Error); 5] = [
        ("busy", || MutationOwnerError::Busy.into()),
        ("replaced", || MutationOwnerError::Replaced.into()),
        ("ticket", || MutationOwnerError::TicketMismatch.into()),
        ("unknown", || anyhow::anyhow!("unknown failure")),
        ("similar text", || {
            anyhow::anyhow!("agent mutation requires reconciliation: private-events-ticket")
        }),
    ];
    for (name, error) in cases {
        for surface in [AgentReadSurface::State, AgentReadSurface::EventPage] {
            let expected = crate::http::agent_access_error(PublicAgentError::Runtime(error()));
            let actual = crate::http::agent_read_access_error(
                "requested-agent",
                surface,
                PublicAgentError::Runtime(error()),
            );
            assert_eq!(actual.0, expected.0, "{name}");
            assert_eq!(actual.1 .0, expected.1 .0, "{name}");
        }
    }
}

#[test]
fn read_reconciliation_error_context_and_diagnostics_are_safe() {
    use crate::{
        host::PublicAgentError,
        http::{agent_read_access_error, AgentReadSurface},
        runtime::mutation_owner::RecoveryKey,
    };
    #[derive(Clone)]
    struct Writer(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || Writer(output.clone()))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for surface in [AgentReadSurface::State, AgentReadSurface::EventPage] {
            let error = anyhow::Error::new(MutationOwnerError::NeedsReconcile(RecoveryKey {
                agent_id: "affected-agent".into(),
                identity_incarnation: 42,
                operation_id: "SECRET-RECOVERY-TICKET".into(),
            }))
            .context("SECRET-PAYLOAD-CONTEXT");
            let (status, Json(body)) = agent_read_access_error(
                "requested-agent",
                surface,
                PublicAgentError::Runtime(error),
            );
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["code"], "agent_reconciliation_pending");
            assert_eq!(body["domain"], "runtime");
            assert_eq!(body["retryable"], true);
            assert!(!body.to_string().contains("SECRET"));
        }
    });
    let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
    for field in [
        "requested-agent",
        "affected_agent_id=affected-agent",
        "identity_incarnation=42",
        "read_surface=\"state\"",
        "read_surface=\"events.page\"",
        "error_kind=\"needs_reconcile\"",
    ] {
        assert!(logs.contains(field), "{field}: {logs}");
    }
    assert!(!logs.contains("SECRET"), "{logs}");
}
