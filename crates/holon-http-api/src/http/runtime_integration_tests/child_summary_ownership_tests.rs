use super::*;
use crate::types::{AgentOwnership, AgentProfilePreset};

#[tokio::test]
async fn compact_parent_summary_child_history_cost() -> Result<()> {
    use crate::diagnostics::io_probe::IoProbe;
    use crate::runtime_db::cold_fixture::{ColdFixtureEvidence, ColdFixtureWriter};
    use crate::types::{
        BriefKind, BriefRecord, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        ToolExecutionRecord, ToolExecutionStatus, TurnRecord,
    };
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let mut probe = IoProbe::start();
    let mut expected_reads = None;
    let isolated = std::env::var_os("HOLON_4123_ISOLATED_MEASUREMENT").is_some();
    for children in [1, 10, 20] {
        for tools_per_turn in [0, 32] {
            let (_home, host) = test_host();
            let parent = host.ensure_default_agent_identity()?;
            for child_index in 0..children {
                let selected = child(
                    &host,
                    &format!("compact-cold-{child_index}"),
                    &parent.agent_id,
                )?;
                let id = &selected.agent_id;
                let mut state = AgentState::new(id);
                state.status = AgentStatus::Stopped;
                host.runtime_db()
                    .agent_states()
                    .initialize_cold_for_fixture(&state, selected.incarnation)?;
                let writer = ColdFixtureWriter::new(host.runtime_db(), id)?;
                let mut message = MessageEnvelope::new(
                    id,
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: "fixed Brief source".into(),
                    },
                );
                message.message_seq = Some(1);
                let turn_id = format!("{id}-history-turn");
                message.turn_id = Some(turn_id.clone());
                writer.insert(ColdFixtureEvidence::Message(&message))?;
                let mut turn = TurnRecord::new(id, turn_id, 1);
                turn.input_message_ids.push(message.id.clone());
                for tool_index in 0..tools_per_turn {
                    let tool = ToolExecutionRecord {
                        id: format!("{id}-tool-{tool_index}"),
                        agent_id: id.clone(),
                        work_item_id: None,
                        turn_index: 1,
                        turn_id: Some(turn.turn_id.clone()),
                        tool_name: "GetAgent".into(),
                        created_at: message.created_at,
                        completed_at: Some(message.created_at),
                        duration_ms: 0,
                        authority_class: AuthorityClass::OperatorInstruction,
                        status: ToolExecutionStatus::Success,
                        input: json!({}),
                        output: json!({}),
                        summary: String::new(),
                        call_binding: None,
                        invocation_surface: None,
                    };
                    turn.tool_execution_ids.push(tool.id.clone());
                    writer.insert(ColdFixtureEvidence::Tool(&tool))?;
                }
                writer.insert(ColdFixtureEvidence::Turn(&turn))?;
                let mut brief =
                    BriefRecord::new(id, BriefKind::Ack, "fixed progress", Some(message.id), None);
                brief.turn_id = Some(turn.turn_id);
                writer.insert(ColdFixtureEvidence::Brief(&brief))?;
            }
            crate::runtime_db::cold_fixture::warm_readers(host.runtime_db())?;
            probe.reset();
            let start = std::time::Instant::now();
            let summary = host
                .local_agent_lightweight_projection(&parent.agent_id)
                .await
                .map_err(anyhow::Error::new)?;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            let counts = probe.snapshot().counters;
            eprintln!(
                "children={children} tools_per_turn={tools_per_turn} sql_reads={} sql_writes={} elapsed_ms={elapsed_ms:.2}",
                counts.sql_read_statements, counts.sql_write_statements
            );
            assert_eq!(summary.identity.agent_id, parent.agent_id);
            let wire = serde_json::to_value(crate::http_dto::SlimAgentDto::from(&summary))?;
            assert!(wire.get("active_children").is_none());
            if isolated {
                assert_eq!(counts.sql_write_statements, 0);
                let expected = expected_reads.get_or_insert(counts.sql_read_statements);
                assert_eq!(counts.sql_read_statements, *expected);
            }
            assert!(crate::runtime::http_test_support::has_no_loaded_agents(&host).await);
            let response = crate::http::router(crate::http::AppState::for_unix(host.clone()))
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/agents/{}/status", parent.agent_id))
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await?)?;
            assert_eq!(body, wire);
            assert!(crate::runtime::http_test_support::has_no_loaded_agents(&host).await);
            host.shutdown().await?;
        }
    }
    Ok(())
}

fn child(host: &RuntimeHost, id: &str, parent: &str) -> Result<AgentIdentityRecord> {
    let mut identity = AgentIdentityRecord::new(
        id,
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(parent.into()),
        Some(format!("task-{id}")),
    )
    .with_lineage_parent_agent_id(Some(parent.into()));
    identity.name = Some(format!("子代理 {id} 🦀"));
    host.append_agent_identity(&identity)?;
    Ok(identity)
}
