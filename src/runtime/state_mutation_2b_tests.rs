use super::*;
use crate::runtime::tests::support::{context_config, CountingProvider};
use tempfile::{tempdir, TempDir};

fn runtime() -> (RuntimeHandle, TempDir, TempDir) {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_owned(),
        workspace.path().to_owned(),
        "http://127.0.0.1:7878".into(),
        Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "unused",
        }),
        "default".into(),
        context_config(),
    )
    .unwrap();
    (runtime, data, workspace)
}

#[tokio::test]
async fn discarded_private_candidate_does_not_change_live_or_durable_state() {
    let (runtime, _data, _workspace) = runtime();
    let before = runtime.agent_state().await.unwrap();
    {
        let mut candidate = runtime.begin_mutation().await.unwrap();
        candidate.state.total_input_tokens = 991;
        candidate.state.status = AgentStatus::Stopped;
        candidate.queue.clear();
    }
    assert_eq!(runtime.agent_state().await.unwrap(), before);
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_whole_row_updates_preserve_each_committed_increment() {
    let (runtime, _data, _workspace) = runtime();
    let before = runtime.agent_state().await.unwrap();
    let mut writers = Vec::new();
    for _ in 0..16 {
        let runtime = runtime.clone();
        writers.push(tokio::spawn(async move {
            runtime
                .update_agent_state(|state| {
                    state.total_input_tokens += 1;
                    Ok(())
                })
                .await
                .unwrap();
        }));
    }
    for writer in writers {
        writer.await.unwrap();
    }
    let current = runtime.agent_state().await.unwrap();
    assert_eq!(current.total_input_tokens, before.total_input_tokens + 16);
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        current
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_install_retains_known_candidate_for_recovery_without_second_sql() {
    let (runtime, _data, _workspace) = runtime();
    let mut candidate = runtime.begin_mutation().await.unwrap();
    candidate.state.total_output_tokens += 37;
    let expected = candidate.state.clone();
    let live = runtime.inner.agent.lock().await;
    let caller = tokio::spawn({
        let storage = runtime.storage().clone();
        async move { candidate.persist_state(&storage).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if runtime
                .runtime_db()
                .agent_states()
                .latest("default")
                .unwrap()
                .as_ref()
                == Some(&expected)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("SQL must complete without holding the live agent mutex");
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_ne!(live.state, expected);
    drop(live);
    let events = runtime.storage().read_recent_events(100).unwrap();
    let recovered = runtime.begin_mutation().await.unwrap();
    assert_eq!(recovered.state, expected);
    drop(recovered);
    assert_eq!(runtime.agent_state().await.unwrap(), expected);
    assert_eq!(runtime.storage().read_recent_events(100).unwrap(), events);
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest("default")
            .unwrap()
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn rejected_compound_candidate_does_not_install_transient_queue_changes() {
    let (runtime, _data, _workspace) = runtime();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "candidate only".into(),
        },
    );
    let mut candidate = runtime.begin_mutation().await.unwrap();
    candidate.queue.push(message);
    candidate.state.pending = 1;
    let command = crate::runtime_db::transitions::AgentPostureTransitionCommand {
        agent_id: "default".into(),
        expected_incarnation: None,
        agent_state: crate::runtime_db::transitions::AgentStateMutation {
            expected: Some(Box::new(candidate.last_persisted_state.clone())),
            record: Box::new(candidate.state.clone()),
        },
        audit_events: vec![AuditEvent::legacy(
            "candidate_rejection",
            serde_json::json!({}),
        )],
        fault: Some(TransitionFaultPoint::BeforeCommit),
    };
    let error = candidate
        .commit_transition(crate::ids::audit_event_id(), || {
            runtime
                .runtime_db()
                .transitions()
                .commit_agent_posture(&command)
        })
        .await
        .unwrap_err();
    assert_eq!(
        crate::runtime_db::persistence_certainty(&error),
        Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
    );
    drop(candidate);
    assert!(runtime.inner.agent.lock().await.queue.is_empty());
    assert!(runtime.inner.mutation_owner.capture().is_ok());
}
