use super::*;

#[path = "context_scope_read_tests.rs"]
mod scope_read_tests;

use chrono::{TimeZone, Utc};
use rusqlite::params;
use serde::Serialize;
use serde_json::json;
use tempfile::{tempdir, TempDir};

use crate::{
    runtime_db::{
        evidence::{
            upsert_agent_state_tx, upsert_execution_root_entry_tx, upsert_workspace_entry_tx,
        },
        repositories::{update_expected_work_item_tx, upsert_task_tx, upsert_wait_condition_tx},
        task_result_settlement::{upsert_pending_tx, TaskResultSettlementState},
        RuntimeDb,
    },
    storage::RuntimeReadModels,
    system::WorkspaceProjectionKind,
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility, AuthorityClass, BriefKind,
        CallbackDeliveryMode, ExternalTriggerScope, ExternalTriggerStatus, MessageBody,
        MessageKind, MessageOrigin, Priority, TaskKind, TaskStatus, ToolExecutionStatus,
        TranscriptEntryKind, WaitConditionKind, WaitConditionStatus, WorkItemContinuationFrame,
        WorkItemState,
    },
};

fn at(index: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + index, 0)
        .single()
        .unwrap()
}

#[test]
fn completion_legacy_selection_matches_original_order_and_ignores_unselected_bodies() -> Result<()>
{
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };
    let (_dir, db) = fixture()?;
    for (agent, work, id, text, time) in [
        ("agent-a", "selected", "tie-a", "lower tie", 20),
        ("agent-a", "selected", "tie-z", "canonical winner", 20),
        ("agent-a", "selected", "newest-empty", "", 30),
        ("agent-b", "selected", "foreign", "foreign", 40),
        ("agent-a", "other", "unselected", "unselected", 50),
    ] {
        let mut record = DeliverySummaryRecord::new(agent, work, text, None, None);
        record.id = id.into();
        record.created_at = at(time);
        if work == "other" {
            record.text = "unselected".repeat(128 * 1024);
        }
        db.evidence().append_delivery_summary(&record)?;
    }
    let mut connection = db.connection()?;
    let tx = connection.transaction()?;
    let read = ContextDurableRead::new(&tx, "agent-a");
    let ids = BTreeSet::from(["selected".into(), "absent".into()]);
    let mut original = std::collections::BTreeMap::new();
    for summary in read.delivery_summaries(usize::MAX)?.into_iter().rev() {
        if ids.contains(&summary.work_item_id) && !summary.text.is_empty() {
            original
                .entry(summary.work_item_id.clone())
                .or_insert(summary);
        }
    }
    // Selection uses the canonical indexed column, not JSON extraction over
    // unrelated history. Even SQLite need not parse that history's body.
    tx.execute(
        "UPDATE delivery_summaries SET payload_json='not-json' WHERE evidence_id='unselected'",
        [],
    )?;
    let old_selector_error = tx
        .query_row(
            "SELECT evidence_id FROM delivery_summaries
             WHERE agent_id=?1 AND json_extract(payload_json,'$.work_item_id')=?2
             ORDER BY created_at DESC,evidence_id DESC LIMIT 1",
            params!["agent-a", "selected"],
            |row| row.get::<_, String>(0),
        )
        .unwrap_err();
    assert!(
        old_selector_error.to_string().contains("malformed JSON"),
        "{old_selector_error}"
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 1,
        },
        256 * 1024,
        8192,
    )?;
    let selected = {
        let _active = phase.enter();
        read.completion_delivery_summaries(ids.iter().map(String::as_str))?
    };
    phase.finish()?;
    assert_eq!(selected.len(), 1);
    same(selected[0].clone(), original.remove("selected").unwrap());
    assert_eq!(selected[0].text, "canonical winner");
    Ok(())
}

#[test]
fn completion_legacy_borrows_selector_and_preserves_empty_failure_latch() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };
    let (_dir, db) = fixture()?;
    let mut connection = db.connection()?;
    let tx = connection.transaction()?;
    let read = ContextDurableRead::new(&tx, "agent-a");
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let baseline = ledger.report()?.observed_bytes;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 1,
        },
        4096,
        64,
    )?;
    let header = phase.finish()?;
    let id = "absent".repeat(32 * 1024);
    {
        let _active = phase.enter();
        assert!(read
            .completion_delivery_summaries(std::iter::once(id.as_str()))?
            .is_empty());
        assert!(read
            .completion_delivery_summaries(std::iter::empty())?
            .is_empty());
        assert_eq!(phase.finish()?, header, "no owned selector or row");
        assert!(charge_owned_metadata(0, 1).is_err());
        assert!(read
            .completion_delivery_summaries(std::iter::empty())
            .is_err());
    }
    assert!(phase.finish().is_err());
    let last = phase.clone();
    drop(phase);
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[test]
fn completion_legacy_rejects_borrowed_payload_before_json_decode_and_uses_remaining() -> Result<()>
{
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };
    let (_dir, db) = fixture()?;
    let record = DeliverySummaryRecord::new("agent-a", "selected", "body", None, None);
    db.evidence().append_delivery_summary(&record)?;
    let mut connection = db.connection()?;
    connection.execute(
        "UPDATE delivery_summaries SET payload_json=json_set(payload_json,'$.text',?1)
         WHERE evidence_id=?2",
        params!["x".repeat(32 * 1024), record.id],
    )?;
    let tx = connection.transaction()?;
    let read = ContextDurableRead::new(&tx, "agent-a");
    let ids = BTreeSet::from(["selected".into()]);
    let scope = InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 2 * 1024 * 1024, 8192)?;
    {
        let _active = phase.enter();
        charge_owned_metadata(160 * 1024, 0)?;
        let error = read
            .completion_delivery_summaries(ids.iter().map(String::as_str))
            .unwrap_err();
        assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    }
    assert!(phase.finish().is_err());
    // A malformed selected field would fail deserialization if the reader
    // decoded first. Low admission must still be the first error.
    tx.execute(
        "UPDATE delivery_summaries SET payload_json=json_set(payload_json,'$.text',json(?1))
         WHERE evidence_id=?2",
        params![format!("[\"{}\"]", "x".repeat(32 * 1024)), record.id],
    )?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 64 * 1024, 8192)?;
    {
        let _active = phase.enter();
        let error = read
            .completion_delivery_summaries(ids.iter().map(String::as_str))
            .unwrap_err();
        assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    }
    drop(phase);
    assert!(read
        .completion_delivery_summaries(ids.iter().map(String::as_str))
        .unwrap_err()
        .to_string()
        .contains("invalid type"));
    Ok(())
}

fn fixture() -> Result<(TempDir, RuntimeDb)> {
    let dir = tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    for agent in ["agent-a", "agent-b"] {
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            agent,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let mut state = AgentState::new(agent);
        state.current_work_item_id = Some(format!("{agent}-work-0"));
        for index in 0..3 {
            let mut work = WorkItemRecord::new(
                agent,
                format!("work {index}"),
                if index == 1 {
                    WorkItemState::Completed
                } else {
                    WorkItemState::Open
                },
            );
            work.id = format!("{agent}-work-{index}");
            work.created_at = at(index);
            work.updated_at = at(index);
            db.work_items().insert_new(&work)?;
        }
        db.agent_states().upsert_for_fixture(&state)?;
        for index in 0..2 {
            let turn_id = format!("{agent}-turn-{index}");
            let mut message = MessageEnvelope::new(
                agent,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("input {index}"),
                },
            );
            message.id = format!("{agent}-message-{index}");
            message.created_at = at(index);
            message.message_seq = Some(index as u64 + 1);
            message.turn_id = Some(turn_id.clone());
            db.messages().upsert(&message)?;
            let mut turn = TurnRecord::new(agent, &turn_id, index as u64 + 1);
            turn.created_at = at(index);
            turn.owner = Some(TurnOwner::Command);
            turn.input_message_ids.push(message.id.clone());
            db.turn_records().upsert(&turn)?;
            let mut transcript = TranscriptEntry::new(
                agent,
                TranscriptEntryKind::AssistantRound,
                Some(1),
                Some(message.id.clone()),
                json!({"turn_id":turn_id,"text":format!("body {index}")}),
            );
            transcript.id = format!("{agent}-transcript-{index}");
            transcript.created_at = at(index);
            transcript.transcript_seq = Some(index as u64 + 1);
            db.transcript_entries().upsert(&transcript)?;
            let mut brief = BriefRecord::new(
                agent,
                BriefKind::Result,
                format!("result {index}"),
                Some(message.id),
                None,
            );
            brief.id = format!("{agent}-brief-{index}");
            brief.created_at = at(index);
            brief.turn_id = Some(turn_id.clone());
            db.evidence().append_brief(&brief)?;
            db.evidence().append_tool_execution(&ToolExecutionRecord {
                id: format!("{agent}-tool-{index}"),
                agent_id: agent.into(),
                work_item_id: None,
                turn_index: index as u64 + 1,
                turn_id: Some(turn_id),
                tool_name: "GetAgent".into(),
                created_at: at(index),
                completed_at: Some(at(index)),
                duration_ms: 0,
                authority_class: AuthorityClass::RuntimeInstruction,
                status: ToolExecutionStatus::Success,
                input: json!({}),
                output: json!({"ok":true}),
                summary: "ok".into(),
                invocation_surface: None,
            })?;
            let mut delivery = DeliverySummaryRecord::new(
                agent,
                format!("{agent}-work-0"),
                format!("delivery {index}"),
                Some(index as u64 + 1),
                None,
            );
            delivery.id = format!("{agent}-delivery-{index}");
            delivery.created_at = at(index);
            db.evidence().append_delivery_summary(&delivery)?;
            db.tasks().upsert(&TaskRecord {
                id: format!("{agent}-task-{index}"),
                agent_id: agent.into(),
                kind: TaskKind::CommandTask,
                status: if index == 0 {
                    TaskStatus::Running
                } else {
                    TaskStatus::Queued
                },
                created_at: at(index),
                updated_at: at(index),
                parent_message_id: None,
                work_item_id: Some(format!("{agent}-work-0")),
                summary: None,
                detail: None,
                recovery: None,
            })?;
        }
        for (suffix, work_item_id) in [
            ("live", Some(format!("{agent}-work-0"))),
            ("closed", Some(format!("{agent}-work-1"))),
            ("unbound", None),
        ] {
            db.wait_conditions().upsert(&WaitConditionRecord {
                id: format!("{agent}-wait-{suffix}"),
                agent_id: agent.into(),
                work_item_id,
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::Operator,
                source: None,
                subject_ref: None,
                waiting_for: suffix.into(),
                wake_sources: Vec::new(),
                continuation: None,
                created_at: at(0),
                updated_at: at(0),
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: None,
                trigger_message_id: None,
                triggered_at: None,
            })?;
        }
        db.work_item_continuations()
            .upsert(&WorkItemContinuationFrame::new_on_completed(
                agent,
                format!("{agent}-work-2"),
                format!("{agent}-work-0"),
                Some(format!("{agent}-turn-1")),
            ))?;
        db.external_triggers().upsert(&ExternalTriggerRecord {
            external_trigger_id: format!("{agent}-trigger"),
            target_agent_id: agent.into(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            token: None,
            token_hash: "fixture-only".into(),
            status: ExternalTriggerStatus::Active,
            created_at: at(0),
            revoked_at: None,
            last_delivered_at: Some(at(1)),
            delivery_count: 1,
        })?;
        let settlement = TaskResultSettlementRecord {
            result_identity: format!("{agent}-settlement"),
            agent_id: agent.into(),
            task_id: format!("{agent}-completed-task"),
            message_id: format!("{agent}-message-0"),
            work_item_id: Some(format!("{agent}-work-0")),
            rejoin_generation: 1,
            parent_turn_id: format!("{agent}-turn-0"),
            task_status: "completed".into(),
            state: TaskResultSettlementState::CallerAdmitted,
            activation_id: Some("activation".into()),
            disposition: None,
            created_at: at(0),
            updated_at: at(0),
            admitted_at: Some(at(0)),
            settled_at: None,
            deferred_reason: None,
            deferred_at: None,
            next_recheck_at: None,
        };
        db.transaction(|tx| {
            upsert_pending_tx(tx, &settlement)?;
            tx.execute(
                "INSERT INTO execution_protocol_partitions(agent_id, updated_at) VALUES (?1, ?2)",
                params![agent, at(0).to_rfc3339()],
            )?;
            Ok(())
        })?;
    }
    for index in 0..2 {
        let id = format!("workspace-{index}");
        let mut workspace = WorkspaceEntry::new(&id, dir.path().join(&id), None);
        workspace.created_at = at(index);
        workspace.updated_at = at(index);
        db.workspace_entries().upsert(&workspace)?;
        for ordinal in 0..3 {
            db.execution_root_entries().upsert(&ExecutionRootEntry {
                execution_root_id: format!("{id}-root-{ordinal}"),
                workspace_id: id.clone(),
                filesystem_path: dir.path().join(format!("root-{index}-{ordinal}")),
                root_kind: WorkspaceProjectionKind::CanonicalRoot,
                worktree: None,
                created_at: at(ordinal),
                removed_at: if ordinal == 2 { Some(at(3)) } else { None },
            })?;
        }
    }
    Ok((dir, db))
}

fn same<T: Serialize>(actual: T, expected: T) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

fn queue_signature(queue: WorkQueueReadModel) -> serde_json::Value {
    json!({
        "current":queue.current, "queued_blocked":queue.queued_blocked, "items":queue.items,
        "current_runnable":queue.current_runnable, "triggered_blocked":queue.triggered_blocked,
        "queued_runnable":queue.queued_runnable, "yielded":queue.yielded,
        "waiting_for_operator":queue.waiting_for_operator,"blocked":queue.blocked,
        "completing":queue.completing,"completed_recent":queue.completed_recent,
    })
}

#[test]
fn transaction_inputs_preserve_existing_query_order_selection_and_wait_filter() -> Result<()> {
    let (_dir, db) = fixture()?;
    let mut connection = db.connection()?;
    let tx = connection.transaction()?;
    let read = ContextDurableRead::new(&tx, "agent-a");
    let facts = read.scope_facts()?;
    same(facts.identity, db.agent_identities().latest("agent-a")?);
    same(
        facts.state.as_deref(),
        db.agent_states().latest("agent-a")?.as_ref(),
    );
    assert!(facts.control_revision.is_some());
    same(
        read.workspace_entries()?,
        db.workspace_entries().latest_all()?,
    );
    let workspace_ids: Vec<String> = vec!["workspace-1".into(), "workspace-0".into()];
    let expected_roots = workspace_ids
        .iter()
        .map(|id| db.execution_root_entries().active_for_workspace(id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    same(
        read.execution_roots_for_workspaces(&workspace_ids)?,
        expected_roots,
    );
    let anchors = read.anchor_messages()?;
    same(
        anchors.clone(),
        db.messages()
            .latest_trusted_operator(Some("agent-a"))?
            .into_iter()
            .collect(),
    );
    same(
        anchors,
        vec![db
            .messages()
            .by_id(Some("agent-a"), "agent-a-message-1")?
            .unwrap()],
    );
    same(
        read.message_by_id("agent-a-message-0")?,
        db.messages().by_id(Some("agent-a"), "agent-a-message-0")?,
    );
    same(read.briefs(1)?, db.evidence().recent_briefs("agent-a", 1)?);
    same(
        read.brief_by_id("agent-a-brief-0")?,
        db.evidence().brief_by_id("agent-a", "agent-a-brief-0")?,
    );
    same(
        read.tools(1)?,
        db.evidence().recent_tool_executions("agent-a", 1)?,
    );
    same(
        read.tool_by_id("agent-a-tool-0")?,
        db.evidence()
            .tool_execution_by_id("agent-a", "agent-a-tool-0")?,
    );
    same(
        read.turns(1)?,
        db.turn_records().recent_for_agent("agent-a", 1)?,
    );
    same(
        read.turns_for_owner(&TurnOwner::Command, 1)?,
        db.turn_records()
            .recent_for_owner("agent-a", &TurnOwner::Command, 1)?,
    );
    same(
        read.turn_by_id("agent-a-turn-0")?,
        db.turn_records().by_id(Some("agent-a"), "agent-a-turn-0")?,
    );
    same(
        read.transcript(1)?,
        db.transcript_entries().recent(Some("agent-a"), 1)?,
    );
    let turns = vec!["agent-a-turn-0".into()];
    same(
        read.transcript_for_turns(&turns)?,
        db.transcript_entries().for_turn_ids("agent-a", &turns)?,
    );
    same(
        read.transcript_by_id("agent-a-transcript-0")?,
        db.transcript_entries()
            .by_id(Some("agent-a"), "agent-a-transcript-0")?,
    );
    same(
        read.execution_protocol_state("agent-a")?,
        db.transitions()
            .load_execution_protocol_state_if_initialized("agent-a")?,
    );
    let legacy = RuntimeReadModels::new(db.clone(), Some("agent-a".into()));
    same(
        read.active_waits("agent-a")?,
        legacy.active_wait_conditions_for_agent("agent-a")?,
    );
    assert_eq!(
        read.active_waits("agent-a")?.len(),
        2,
        "closed owner wait must be filtered, unbound remains"
    );
    assert_eq!(
        queue_signature(read.work_queue()?),
        queue_signature(legacy.work_queue()?)
    );
    same(
        read.work_item("agent-a-work-0")?,
        db.work_items().latest("agent-a-work-0")?,
    );
    same(
        read.external_triggers()?,
        db.external_triggers().latest_for_agent("agent-a")?,
    );
    let tasks = read.active_tasks("agent-a")?;
    assert_eq!(
        tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["agent-a-task-1", "agent-a-task-0"]
    );
    same(tasks, db.tasks().active_for_agent("agent-a", usize::MAX)?);
    same(
        read.delivery_summaries(1)?,
        db.evidence().recent_delivery_summaries("agent-a", 1)?,
    );
    same(
        read.admitted_settlements("agent-a", "activation")?,
        db.task_result_settlements()
            .admitted_for_activation("agent-a", "activation")?,
    );
    assert!(read.briefs(0)?.is_empty());
    assert!(read.tools(0)?.is_empty());
    assert!(read.turns(0)?.is_empty());
    assert!(read.transcript(0)?.is_empty());
    Ok(())
}

#[test]
fn transaction_inputs_keep_all_families_on_the_first_read_view() -> Result<()> {
    let (_dir, db) = fixture()?;
    let mut state = db.agent_states().latest("agent-a")?.unwrap();
    let mut task = db.tasks().latest("agent-a-task-0")?.unwrap();
    let mut work = db.work_items().latest("agent-a-work-0")?.unwrap();
    let mut wait = db
        .wait_conditions()
        .latest_all()?
        .into_iter()
        .find(|wait| wait.id == "agent-a-wait-live")
        .unwrap();
    let mut connection = db.connection()?;
    let tx = connection.transaction()?;
    let read = ContextDurableRead::new(&tx, "agent-a");
    let original = read.scope_facts()?; // fixes the SQLite view before the second writer
    state.current_work_item_id = None;
    task.status = TaskStatus::Completed;
    task.updated_at = at(10);
    let old_revision = work.revision;
    work.revision += 1;
    work.state = WorkItemState::Completed;
    work.updated_at = at(10);
    wait.status = WaitConditionStatus::Resolved;
    wait.resolved_at = Some(at(10));
    wait.updated_at = at(10);
    let mut late_brief = BriefRecord::new(
        "agent-a",
        BriefKind::Result,
        "later committed evidence",
        None,
        None,
    );
    late_brief.id = "late-brief".into();
    late_brief.created_at = at(10);
    let mut late_workspace =
        WorkspaceEntry::new("late-workspace", _dir.path().join("late-workspace"), None);
    late_workspace.created_at = at(10);
    late_workspace.updated_at = at(10);
    let late_root = ExecutionRootEntry {
        execution_root_id: "late-root".into(),
        workspace_id: "workspace-0".into(),
        filesystem_path: _dir.path().join("late-root"),
        root_kind: WorkspaceProjectionKind::CanonicalRoot,
        worktree: None,
        created_at: at(10),
        removed_at: None,
    };
    db.transaction(|writer| {
        upsert_agent_state_tx(writer, &state)?;
        upsert_task_tx(writer, &task)?;
        update_expected_work_item_tx(writer, &work, old_revision)?;
        upsert_wait_condition_tx(writer, &wait)?;
        crate::runtime_db::evidence::insert_brief_evidence_tx(writer, &late_brief)?;
        upsert_workspace_entry_tx(writer, &late_workspace)?;
        upsert_execution_root_entry_tx(writer, &late_root)?;
        Ok(())
    })?;
    same(read.scope_facts()?.state, original.state);
    assert_eq!(read.active_tasks("agent-a")?.len(), 2);
    assert_eq!(
        read.work_item(&work.id)?.unwrap().state,
        WorkItemState::Open
    );
    assert!(read
        .active_waits("agent-a")?
        .iter()
        .any(|wait| wait.id == "agent-a-wait-live"));
    assert_eq!(read.work_queue()?.current.unwrap().id, work.id);
    assert!(read.brief_by_id("late-brief")?.is_none());
    assert!(!read
        .workspace_entries()?
        .iter()
        .any(|workspace| workspace.workspace_id == "late-workspace"));
    assert!(!read
        .execution_roots_for_workspaces(&["workspace-0".into()])?
        .iter()
        .any(|root| root.execution_root_id == "late-root"));
    drop(read);
    tx.commit()?;
    let tx = connection.transaction()?;
    let next = ContextDurableRead::new(&tx, "agent-a");
    same(next.scope_facts()?.into_owned_state()?, Some(state));
    assert_eq!(next.active_tasks("agent-a")?.len(), 1);
    assert_eq!(
        next.work_item(&work.id)?.unwrap().state,
        WorkItemState::Completed
    );
    assert!(!next
        .active_waits("agent-a")?
        .iter()
        .any(|wait| wait.id == "agent-a-wait-live"));
    assert!(next.work_queue()?.current.is_none());
    same(next.brief_by_id("late-brief")?, Some(late_brief));
    assert!(next
        .workspace_entries()?
        .iter()
        .any(|workspace| workspace.workspace_id == "late-workspace"));
    assert!(next
        .execution_roots_for_workspaces(&["workspace-0".into()])?
        .iter()
        .any(|root| root.execution_root_id == "late-root"));
    Ok(())
}

#[test]
fn transaction_inputs_reject_foreign_scope_and_do_not_leak_by_id_records() -> Result<()> {
    let (_dir, db) = fixture()?;
    let mut connection = db.connection()?;
    let tx = connection.transaction()?;
    let read = ContextDurableRead::new(&tx, "agent-a");
    assert!(read.execution_protocol_state("agent-b").is_err());
    assert!(read.active_waits("agent-b").is_err());
    assert!(read.active_tasks("agent-b").is_err());
    assert!(read.admitted_settlements("agent-b", "activation").is_err());
    assert!(read
        .turns_for_owner(
            &TurnOwner::AgentLifecycle {
                agent_id: "agent-b".into()
            },
            10
        )
        .is_err());
    assert!(read.message_by_id("agent-b-message-0")?.is_none());
    assert!(read.turn_by_id("agent-b-turn-0")?.is_none());
    assert!(read.brief_by_id("agent-b-brief-0")?.is_none());
    assert!(read.tool_by_id("agent-b-tool-0")?.is_none());
    assert!(read.transcript_by_id("agent-b-transcript-0")?.is_none());
    assert!(read.work_item("agent-b-work-0")?.is_none());
    assert!(read
        .transcript_for_turns(&["agent-b-turn-0".into()])?
        .is_empty());
    assert!(read
        .admitted_settlements("agent-a", "another-activation")?
        .is_empty());
    Ok(())
}
