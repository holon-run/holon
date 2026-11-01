use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor},
        tests::support::{context_config, isolate_test_user_home, tempdir, StubProvider},
    },
    runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError,
    types::{
        GenerateImageParameters, GenerateImageSize, GeneratedImageReference, ToolExecutionRecord,
        ToolExecutionStatus,
    },
};

async fn fixture() -> Result<(RuntimeHandle, tempfile::TempDir, String)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new(
        "image-inputs",
        home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "image-inputs".into(),
        context_config(),
    )?;
    isolate_test_user_home(&runtime);
    runtime
        .enqueue(MessageEnvelope::new(
            "image-inputs",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "generate an image".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime).poll().await?
    else {
        anyhow::bail!("fixture input was not selected");
    };
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await?;
    let turn = runtime
        .agent_state()
        .await?
        .current_turn_id
        .context("fixture Turn missing")?;
    Ok((runtime, home, turn))
}

fn tool(turn: &str, index: usize) -> ToolExecutionRecord {
    let at = Utc::now();
    let image = GeneratedImageReference {
        kind: "generated_image".into(),
        id: format!("image-{index}"),
        workspace_id: crate::types::agent_home_workspace_id("image-inputs"),
        path: format!("media/generated/image-{index}.png").into(),
        uri: format!("workspace://image-inputs/media/generated/image-{index}.png"),
        sha256: "a".repeat(64),
        mime: "image/png".into(),
        byte_count: 128,
        size: GenerateImageSize {
            width: Some(1),
            height: Some(1),
        },
        created_at: at,
    };
    ToolExecutionRecord {
        id: format!("image-tool-{index:03}"),
        agent_id: "image-inputs".into(),
        work_item_id: None,
        turn_id: Some(turn.into()),
        turn_index: 1,
        tool_name: crate::tool::names::GENERATE_IMAGE.into(),
        created_at: at,
        completed_at: Some(at),
        duration_ms: 1,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({}),
        output: serde_json::json!({"result": GenerateImageResult {
            images: vec![image], provider: "test".into(), model: "test/image".into(),
            prompt: "image prompt".into(), summary_text: None,
            parameters: GenerateImageParameters {
                size: None, aspect_ratio: None, resolution: None,
                background: None, output_format: None, name: None,
            },
            selection: None,
            generation_succeeded: Some(true),
            requested_parameters: None,
            provider_parameters: None,
            provider_effective_parameters: None,
            parameter_defaults: None,
            parameter_mapping: None,
            contract: None,
            source_images: Vec::new(),
            conversion: Vec::new(),
        }}),
        summary: "generated".into(),
        invocation_surface: None,
    }
}

fn brief(turn: &str) -> BriefRecord {
    let mut brief = BriefRecord::new("image-inputs", BriefKind::Result, "image", None, None);
    brief.turn_id = Some(turn.into());
    brief
}

#[tokio::test]
async fn image_recent64_full_body_is_sql_free_ordered_and_held_to_last_source() -> Result<()> {
    let (runtime, _home, turn) = fixture().await?;
    let base: chrono::DateTime<Utc> = "2026-10-22T00:00:00.123456789Z".parse()?;
    let duplicate = tool(&turn, 63).output["result"].clone();
    for index in 0..65 {
        let mut record = tool(&turn, index);
        record.created_at = base + chrono::Duration::milliseconds(index as i64);
        if index == 62 {
            record.output["result"] = duplicate.clone();
        } else if index == 64 {
            record.output["result"]["prompt"] = serde_json::Value::String("x".repeat(40_000));
        } else if index == 60 {
            record.status = ToolExecutionStatus::Error;
        } else if index == 61 {
            record.turn_id = None;
        } else if index != 0 && index != 63 {
            record.tool_name = "OtherTool".into();
        }
        runtime.persist_tool_execution_evidence(&record).await?;
    }
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let read = runtime.read_generated_image_inputs(&turn).await?;
    assert_eq!(
        read.tools[0].created_at,
        base + chrono::Duration::milliseconds(64),
        "body loans retain canonical nanoseconds, not the SQL ordering timestamp"
    );
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(before.token, after.token);
    assert!(Arc::ptr_eq(
        before.input_evidence.as_ref().unwrap(),
        after.input_evidence.as_ref().unwrap()
    ));
    assert_eq!(
        read.tools
            .iter()
            .map(|tool| tool.id.as_str())
            .collect::<Vec<_>>(),
        ["image-tool-064", "image-tool-063", "image-tool-062"]
    );
    assert_eq!(
        read.tools[0].output["result"]["prompt"]
            .as_str()
            .unwrap()
            .len(),
        40_000
    );
    drop((before, after));
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(AuditEvent::legacy("image-read-cut", serde_json::json!({})))
        .await?;
    assert!(
        old_root.upgrade().is_none(),
        "finite input must not retain the entire Root"
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE tool_executions RENAME TO image_forbidden_tools;
                          ALTER TABLE input_evidence_current RENAME TO image_forbidden_qualifications;")?;
        Ok(())
    })?;
    let mut output = brief(&turn);
    read.attach_to(&mut output)?;
    let attachments = output
        .attachments
        .as_ref()
        .context("image attachments missing")?;
    assert_eq!(
        attachments
            .iter()
            .map(|attachment| attachment.name.as_str())
            .collect::<Vec<_>>(),
        ["image-64.png", "image-63.png"]
    );
    assert_eq!(
        attachments[0].value.as_ref().unwrap()["uri"],
        attachments[0].uri.as_ref().unwrap().as_str()
    );
    let source = Arc::downgrade(&read._source.records[0]);
    let read = Arc::new(read);
    let last = read.clone();
    drop(read);
    assert!(source.upgrade().is_some());
    drop(last);
    assert!(source.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn image_inputs_refuse_raw_and_changed_tools_without_publication() -> Result<()> {
    let (runtime, _home, turn) = fixture().await?;
    let record = tool(&turn, 1);
    runtime.storage().append_tool_execution(&record)?;
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    let error = runtime
        .read_generated_image_inputs(&turn)
        .await
        .err()
        .context("raw Tool accepted")?;
    assert!(matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::NeedsPreparation(_))
    ));
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM tool_executions WHERE evidence_id=?1",
            [&record.id],
        )?;
        Ok(())
    })?;
    runtime.persist_tool_execution_evidence(&record).await?;
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    runtime.runtime_db().transaction(|tx| {
        tx.execute("UPDATE tool_executions SET payload_json=json_set(payload_json,'$.output.result.images[0].uri','changed')
                    WHERE evidence_id=?1", [&record.id])?;
        Ok(())
    })?;
    let error = runtime
        .read_generated_image_inputs(&turn)
        .await
        .err()
        .context("changed Tool accepted")?;
    assert!(matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ProjectionChanged(_))
    ));
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    Ok(())
}

#[tokio::test]
async fn image_attachment_decode_enters_original_material_before_allocating() -> Result<()> {
    let (runtime, _home, turn) = fixture().await?;
    runtime
        .persist_tool_execution_evidence(&tool(&turn, 1))
        .await?;
    let mut read = runtime.read_generated_image_inputs(&turn).await?;
    let scope = &read._source.scope;
    let header = ((scope.agent_id.len() + 256) * 8) as u64;
    read.material = ResourceReadMaterial::with_test_limits(
        &runtime.inner.input_evidence_budget,
        scope,
        header,
        64,
    )?;
    let mut output = brief(&turn);
    output.attachments = Some(Vec::new());
    let original = output.clone();
    let error = read
        .attach_to(&mut output)
        .expect_err("decode must require admission");
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    assert_eq!(output, original);
    assert!(read.material.finish().is_err());
    Ok(())
}

#[tokio::test]
async fn image_attachment_real_brief_publication_keeps_exact_locator() -> Result<()> {
    let (runtime, _home, turn) = fixture().await?;
    let record = tool(&turn, 1);
    runtime.persist_tool_execution_evidence(&record).await?;
    let output = brief(&turn);
    runtime.persist_brief(&output).await?;
    let stored = runtime
        .storage()
        .read_brief_by_id(&output.id)?
        .context("Brief missing")?;
    let attachments = stored
        .attachments
        .context("persisted attachments missing")?;
    assert_eq!(attachments.len(), 1);
    assert_eq!(
        attachments[0].value,
        Some(record.output["result"]["images"][0].clone())
    );
    assert_eq!(
        attachments[0].uri.as_deref(),
        record.output["result"]["images"][0]["uri"].as_str()
    );
    Ok(())
}
