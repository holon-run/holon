use super::*;
use crate::runtime::tests::support::{context_config, tempdir};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "compact-wire-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}

#[tokio::test]
async fn actual_loaded_compact_http_keeps_original_material_through_last_encoded_slice(
) -> Result<()> {
    use futures_util::StreamExt;
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let projection = runtime.lightweight_agent_state_projection().await?;
    let expected = serde_json::to_vec(&crate::http_dto::SlimAgentDto::from(&projection))?;
    let source = Arc::downgrade(&projection._ordinary_material.as_ref().unwrap().output);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let strong = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO compact_wire_forbidden_states;
         ALTER TABLE tasks RENAME TO compact_wire_forbidden_tasks;
         ALTER TABLE messages RENAME TO compact_wire_forbidden_messages;
         ALTER TABLE audit_events RENAME TO compact_wire_forbidden_events;",
    )?;
    // The same helper is called by the loaded status/get_agent Compact branch.
    let response = crate::http::traced_compact_json(
        "/agents/{agent_id}/status",
        std::time::Instant::now(),
        projection,
    )
    .map_err(|(status, body)| anyhow!("compact HTTP {status}: {}", body.0))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "application/json"
    );
    assert!(source.upgrade().is_some());
    assert_eq!(Arc::strong_count(&root), strong);
    let mut body = response.into_body().into_data_stream();
    let bytes = body.next().await.context("compact body empty")??;
    assert_eq!(bytes.as_ref(), expected.as_slice());
    assert!(body.next().await.is_none());
    let clone = bytes.clone();
    let last = clone.slice(0..1);
    drop((bytes, clone, body));
    assert!(source.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(last);
    assert!(source.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn actual_compact_encoder_shares_source_cap_and_validates_ledger_without_double_registration(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    for kind in ["byte", "nodes", "exact", "foreign", "successor"] {
        let mut projection = runtime.lightweight_agent_state_projection().await?;
        let source = Arc::downgrade(&projection._ordinary_material.as_ref().unwrap().output);
        let plan = projection.compact_json_plan()?;
        let exact = (usize::try_from(plan.total.0)?, plan.total.1);
        assert!(plan.own.0 < plan.total.0 && plan.own.1 < plan.total.1);
        if kind == "foreign" || kind == "successor" {
            let other = InputEvidenceBudgetLedger::new(Default::default())?;
            if kind == "foreign" {
                projection._ordinary_material.as_mut().unwrap().ledger = other.clone();
            } else {
                projection.identity.incarnation += 1;
            }
            let error = projection
                .into_compact_json_wire_with_limits(exact)
                .err()
                .context("foreign compact source accepted")?;
            assert!(
                matches!(
                    error.downcast_ref::<CompactAgentReadError>(),
                    Some(CompactAgentReadError::ForeignMaterial)
                ),
                "{error:#}"
            );
            assert_eq!(other.report()?.observed_bytes, 0);
        } else if kind == "exact" {
            let before = ledger.report()?.observed_bytes;
            let wire = projection.into_compact_json_wire_with_limits(exact)?;
            assert_eq!(wire.bytes.len(), plan.encoded_len);
            assert_eq!(ledger.report()?.observed_bytes - before, plan.own.0);
            let encoded = Arc::downgrade(&wire._encoded);
            let bytes = axum::body::Bytes::from_owner(wire);
            let last = bytes.slice(0..1);
            drop(bytes);
            assert!(source.upgrade().is_some() && encoded.upgrade().is_some());
            drop(last);
            assert!(encoded.upgrade().is_none());
        } else {
            let limits = if kind == "byte" {
                (exact.0 - 1, exact.1)
            } else {
                (exact.0, exact.1 - 1)
            };
            let error = projection
                .into_compact_json_wire_with_limits(limits)
                .err()
                .context("compact encoder received an independent full cap")?;
            assert!(
                matches!(
                    error.downcast_ref::<CompactAgentReadError>(),
                    Some(CompactAgentReadError::ResourceExhausted)
                ),
                "{error:#}"
            );
        }
        assert!(source.upgrade().is_none(), "{kind}");
        assert_eq!(ledger.report()?.observed_bytes, baseline, "{kind}");
    }
    Ok(())
}

#[tokio::test]
async fn borrowed_compact_formatter_preserves_exact_original_slim_schema_and_display_paths(
) -> Result<()> {
    use crate::system::{WorkspaceAccessMode, WorkspaceProjectionKind};
    use crate::types::{
        ActiveWorkspaceEntry, ChildAgentObservabilitySnapshot, ChildAgentPhase, ChildAgentSummary,
    };
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let mut projection = runtime.lightweight_agent_state_projection().await?;
    for rich in [false, true] {
        if rich {
            // Formatter-only cases exercise the original DTO contract. They
            // do not install a Root or grant these values a runtime source.
            projection.agent.current_run_id = Some("run\0\"中文".into());
            projection.agent.current_turn_id = Some("turn".into());
            projection.agent.current_turn_work_item_id = Some("turn-work".into());
            projection.agent.current_work_item_id = Some("current-work".into());
            projection.agent.attached_workspaces = vec!["workspace".into()];
            projection.agent.active_workspace_entry = Some(ActiveWorkspaceEntry {
                workspace_id: "workspace".into(),
                workspace_anchor: "/tmp/中文\"workspace".into(),
                execution_root_id: "root".into(),
                execution_root: "/tmp/root".into(),
                projection_kind: WorkspaceProjectionKind::CanonicalRoot,
                access_mode: WorkspaceAccessMode::SharedRead,
                cwd: "/tmp/root/child".into(),
                occupancy_id: Some("not-in-slim".into()),
                projection_metadata: None,
            });
            projection.model.requested_model = Some(projection.model.effective_model.clone());
            projection.model.active_model = Some(projection.model.effective_model.clone());
            projection.model.override_model = Some(projection.model.effective_model.clone());
            projection.model.override_reasoning_effort = Some("high".into());
            projection.model.effective_fallback_models =
                vec![projection.model.effective_model.clone()];
            projection.active_children = vec![ChildAgentSummary {
                identity: projection.identity.clone(),
                status: AgentStatus::AwakeIdle,
                current_run_id: Some("child-run".into()),
                pending: 1,
                active_task_count: 2,
                observability: ChildAgentObservabilitySnapshot {
                    phase: ChildAgentPhase::Running,
                    blocked_reason: None,
                    waiting_reason: None,
                    current_work_item_id: Some("child-work".into()),
                    work_summary: Some("summary".into()),
                    last_progress_brief: Some("progress\0\"中文".into()),
                    last_result_brief: Some("result".into()),
                },
            }];
        }
        let old = serde_json::to_vec(&crate::http_dto::SlimAgentDto::from(&projection))?;
        let borrowed = serde_json::to_vec(&SlimAgentDtoRef(&projection))?;
        assert_eq!(borrowed, old, "rich={rich}");
        assert_eq!(
            serialized_bytes(&SlimAgentDtoRef(&projection))?,
            u64::try_from(old.len())?
        );
    }
    Ok(())
}
