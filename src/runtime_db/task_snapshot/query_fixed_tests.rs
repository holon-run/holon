use super::super::episode_snapshot::task_checkpoint::{self, queries};
use super::tests::{identity, legacy};
use super::*;
use crate::types::{TaskKind, TaskRecord, TaskStatus};

fn task(id: &str) -> TaskRecord {
    let at = "2026-10-01T00:00:00.123456789Z".parse().unwrap();
    TaskRecord {
        id: id.into(),
        agent_id: "task-agent".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some(id.into()),
        detail: None,
        recovery: None,
    }
}
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "task-agent".into(),
        identity_incarnation: 1,
    }
}
fn selected(id: &str) -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: BTreeSet::from([TaskKey::new(TaskFamily::Task, id)]),
        },
        record_limit: 32,
        byte_limit: 1024 * 1024,
    }
}
fn fixed(tx: &Transaction<'_>, phase: &str) -> Result<queries::TaskQueryInventory> {
    let seed:String=tx.query_row("SELECT seed_id FROM agent_episode_continuity WHERE agent_id='task-agent' AND identity_incarnation=1",[],|row|row.get(0))?;
    task_checkpoint::load(tx, &seed, phase)?
        .query_inventory
        .context("fixed query inventory absent")
}

#[test]
fn original_query_inventory_qualifies_only_fixed_old_task_without_expanding_resident_window(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        for i in 0..200 {
            repositories::upsert_task_tx(tx, &task(&format!("old-task-{i:03}")))?;
        }
        Ok(())
    })?;
    db.transaction(|tx| {
        let fixed=fixed(tx,"upgrade")?;
        assert_eq!(fixed.task_count,200);
        let current=prepare_task_domain_snapshot_tx(tx,&scope(),&selected("old-task-199"))?.require_current()?;
        assert_eq!(current.resident.coverage().task_metadata.len(),1);
        assert!(current.resident.capture(&selected("old-task-199").capture)?.lookup(&TaskKey::new(TaskFamily::Task,"old-task-199"))?.is_some());
        repositories::upsert_task_tx(tx,&task("late-unreceived"))?;
        assert!(prepare_task_domain_snapshot_tx(tx,&scope(),&selected("late-unreceived")).is_err());
        let mut changed=task("old-task-198");changed.summary=Some("valid canonical drift".into());
        tx.execute("UPDATE tasks SET summary=?1,payload_json=?2 WHERE task_id=?3",params![changed.summary,serde_json::to_string(&changed)?,changed.id])?;
        assert!(matches!(prepare_task_domain_snapshot_tx(tx,&scope(),&selected("old-task-198")).err().unwrap().downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::CanonicalChanged(_))));
        assert!(tx.execute("UPDATE task_query_fixed_tasks SET content_digest=?1 WHERE inventory_id=?2 AND task_id='old-task-197'",params!["0".repeat(64),fixed.id]).is_err());
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut successor = identity();
        successor.incarnation = 2;
        super::super::evidence::upsert_agent_identity_tx(tx, &successor)?;
        let scope = InputEvidenceScope {
            identity_incarnation: 2,
            ..scope()
        };
        assert!(prepare_task_domain_snapshot_tx(tx, &scope, &selected("old-task-196")).is_err());
        Ok(())
    })
}

#[test]
fn first_query_import_advances_only_actual_received_task_versions_and_never_a_raw_tail(
) -> Result<()> {
    let (_home, db) = legacy(false, |tx| {
        let mut original = task("fixed-before-import");
        original.status = TaskStatus::Running;
        repositories::upsert_task_tx(tx, &original)?;
        Ok(())
    })?;
    db.transaction(|tx| {
        repositories::upsert_task_tx(tx, &task("raw-unreceived-tail"))?;
        Ok(())
    })?;
    let mut received = task("fixed-before-import");
    received.summary = Some("actual new imported version".into());
    received.updated_at += chrono::Duration::seconds(1);
    db.tasks()
        .import_legacy(vec![received.clone(), task("actual-import-only")])?;
    db.context_episodes().import_legacy(Vec::new())?;
    db.transaction(|tx| {
        let old = fixed(tx, "upgrade")?;
        let new = fixed(tx, "first_import")?;
        assert_ne!(old.id, new.id);
        assert_eq!(old.task_count, 1);
        assert_eq!(new.task_count, 2);
        assert_ne!(
            queries::task_digest(tx, &old, &received.id)?,
            queries::task_digest(tx, &new, &received.id)?
        );
        assert!(queries::task_digest(tx, &new, "raw-unreceived-tail")?.is_none());
        let current = prepare_task_domain_snapshot_tx(tx, &scope(), &selected(&received.id))?
            .require_current()?;
        let capture = current.resident.capture(&selected(&received.id).capture)?;
        let actual = capture
            .lookup(&TaskKey::new(TaskFamily::Task, &received.id))?
            .unwrap();
        let CanonicalTaskFact::Task(actual) = &actual.fact else {
            unreachable!()
        };
        assert_eq!(actual.summary, received.summary);
        assert!(
            prepare_task_domain_snapshot_tx(tx, &scope(), &selected("raw-unreceived-tail"))
                .is_err()
        );
        Ok(())
    })
}

#[test]
fn fixed_message_inventory_preserves_actual_source_predicate_assignment_and_header_without_alias_mint(
) -> Result<()> {
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    };
    let (_home, db) = legacy(true, |tx| {
        let mut source = MessageEnvelope::new(
            "task-agent",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "origin-id-is-not-selector".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original source".into(),
            },
        );
        source.id = "fixed-task-output-source".into();
        source.metadata = Some(serde_json::json!({"task_id":"metadata-selector"}));
        super::super::evidence::append_message_tx(tx, &source)?;
        let mut state = crate::types::AgentState::new("task-agent");
        state.total_message_count = 1;
        super::super::evidence::upsert_agent_state_tx(tx, &state)?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let fixed=fixed(tx,"upgrade")?;assert_eq!(fixed.message_count,1);
        let source:(String,String,bool,String)=tx.query_row("SELECT source_task_id,content_digest,is_result,fingerprint_json FROM task_query_fixed_messages WHERE inventory_id=?1 AND evidence_id='fixed-task-output-source'",[&fixed.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        assert_eq!(source.0,"metadata-selector");assert!(source.2);assert_eq!(source.1.len(),64);
        let fingerprint:serde_json::Value=serde_json::from_str(&source.3)?;
        assert_eq!(fingerprint["content_hash"],format!("sha256:{}",source.1));
        assert!(fingerprint.get("frozen_payload_digest").is_none());
        assert!(fingerprint["sequence"].as_u64().is_some());
        let aliases:u64=tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='task-agent'",[],|row|row.get(0))?;
        assert_eq!(aliases,0,"fixed query metadata never creates current body authority");
        Ok(())
    })
}

#[test]
fn explicit_fixed_old_task_keeps_its_complete_settled_result_reverse_index() -> Result<()> {
    use crate::{
        runtime_db::task_result_settlement::{
            upsert_pending_tx, TaskResultSettlementDisposition, TaskResultSettlementRecord,
            TaskResultSettlementState,
        },
        types::{
            AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        },
    };
    let (_home, db) = legacy(true, |tx| {
        for i in 0..200 {
            repositories::upsert_task_tx(tx, &task(&format!("old-completed-{i:03}")))?;
        }
        let mut target = task("z-old-with-result");
        target.detail = Some(
            serde_json::json!({"rejoin_obligation_id":target.id,"rejoin_generation":1,"parent_turn_id":"historical-parent-address"}),
        );
        repositories::upsert_task_tx(tx, &target)?;
        let mut source = MessageEnvelope::new(
            "task-agent",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: target.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original owner missing result".into(),
            },
        );
        source.task_id = Some(target.id.clone());
        let (source, _) = super::super::evidence::append_message_tx(tx, &source)?;
        let mut result = TaskResultSettlementRecord::pending(&target, &source, target.updated_at)?
            .context("legacy result missing")?;
        result.state = TaskResultSettlementState::Settled;
        result.disposition = Some(TaskResultSettlementDisposition::OwnerMissing);
        result.settled_at = Some(target.updated_at);
        result.next_recheck_at = None;
        upsert_pending_tx(tx, &result)?;
        let mut state = crate::types::AgentState::new("task-agent");
        state.total_message_count = 1;
        super::super::evidence::upsert_agent_state_tx(tx, &state)?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let fixed = fixed(tx, "upgrade")?;
        assert_eq!(fixed.result_count, 1);
        let loaded = prepare_task_domain_snapshot_tx(tx, &scope(), &selected("z-old-with-result"))?
            .require_current()?;
        assert_eq!(
            loaded.resident.coverage().task_result_ids["z-old-with-result"].len(),
            1
        );
        let capture = loaded
            .resident
            .capture(&selected("z-old-with-result").capture)?;
        assert_eq!(
            capture
                .records
                .iter()
                .filter(|record| record.locator.key.family == TaskFamily::ResultSettlement)
                .count(),
            1
        );
        let result = capture
            .records
            .iter()
            .find(|record| record.locator.key.family == TaskFamily::ResultSettlement)
            .unwrap();
        assert_eq!(
            loaded.resident.coverage().task_result_ids["z-old-with-result"],
            BTreeSet::from([result.locator.key.id.clone()])
        );
        let weak = Arc::downgrade(result);
        let empty = loaded.resident.capture(&TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: BTreeSet::new(),
        })?;
        assert!(
            empty.records.is_empty(),
            "unselected reverse metadata cannot pin historical bodies"
        );
        drop((capture, loaded));
        assert!(
            weak.upgrade().is_none(),
            "empty capture must not retain the domain's historical Result Arc"
        );
        drop(empty);
        Ok(())
    })
}

#[test]
fn original_query_message_scalar_budget_precedes_a_damaged_large_header_string() -> Result<()> {
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    };
    let error = legacy(true, |tx| {
        let mut message = MessageEnvelope::new(
            "task-agent",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: "original-task".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "small original source".into(),
            },
        );
        message.id = "large-query-header".into();
        message.metadata = Some(serde_json::json!({"task_id":"original-task"}));
        super::super::evidence::append_message_tx(tx, &message)?;
        tx.execute(
            "UPDATE messages SET content_hash=CAST(zeroblob(9437184) AS TEXT) WHERE evidence_id=?1",
            [&message.id],
        )?;
        Ok(())
    })
    .err()
    .context("large fixed query header was decoded")?;
    assert!(
        format!("{error:#}").contains("fixed Task query Message material"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn unsealed_query_member_cannot_move_into_any_original_sealed_family() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &task("original-fixed-task"))?;
        Ok(())
    })?;
    db.transaction(|tx|{
        let original=fixed(tx,"upgrade")?;
        let before:(u64,u64,u64,String)=tx.query_row("SELECT task_count,message_count,result_count,metadata_digest FROM task_query_fixed_manifests WHERE inventory_id=?1",[&original.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        tx.execute("INSERT INTO task_query_fixed_manifests(inventory_id,agent_id,task_observed,message_observed,result_observed) VALUES('unsealed-candidate','task-agent',1,1,1)",[])?;
        let hash="a".repeat(64);
        tx.execute("INSERT INTO task_query_fixed_tasks(inventory_id,task_id,content_digest,projection_digest,created_at,generation_created_at,generation_digest) VALUES('unsealed-candidate','late-task',?1,?1,?2,?2,?1)",params![hash,repositories::timestamp(task("late-task").created_at)])?;
        tx.execute("INSERT INTO task_query_fixed_results(inventory_id,result_identity,task_id,content_digest,projection_digest) VALUES('unsealed-candidate','late-result','late-task',?1,?1)",[&hash])?;
        tx.execute("INSERT INTO task_query_fixed_messages(inventory_id,evidence_id,message_id,created_at,source_task_id,is_result,content_digest,fingerprint_json) VALUES('unsealed-candidate','late-message','late-message',?1,'late-task',1,?2,'{}')",params![repositories::timestamp(task("late-task").created_at),hash])?;
        for table in ["task_query_fixed_tasks","task_query_fixed_results","task_query_fixed_messages"] {
            let error=tx.execute(&format!("UPDATE {table} SET inventory_id=?1 WHERE inventory_id='unsealed-candidate'"),[&original.id]).err().context("unreceived staging member entered fixed inventory")?;
            assert!(error.to_string().contains("fixed Task query members are immutable"));
            let staging:u64=tx.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE inventory_id='unsealed-candidate'"),[],|row|row.get(0))?;
            assert_eq!(staging,1,"failed UPDATE retains its original staging row");
        }
        let after:(u64,u64,u64,String)=tx.query_row("SELECT task_count,message_count,result_count,metadata_digest FROM task_query_fixed_manifests WHERE inventory_id=?1",[&original.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        assert_eq!(before,after);
        let actual:(u64,u64,u64)=tx.query_row("SELECT (SELECT COUNT(*) FROM task_query_fixed_tasks WHERE inventory_id=?1),(SELECT COUNT(*) FROM task_query_fixed_messages WHERE inventory_id=?1),(SELECT COUNT(*) FROM task_query_fixed_results WHERE inventory_id=?1)",[&original.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        assert_eq!(actual,(original.task_count,original.message_count,original.result_count));
        queries::validate(tx,&original)?;
        Ok(())
    })
}

#[test]
fn fixed_query_manifest_replace_cannot_reopen_or_replace_original_qualification() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &task("fixed-replace-original"))?;
        Ok(())
    })?;
    db.transaction(|tx|{
        let original=fixed(tx,"upgrade")?;
        let header_sql="SELECT json_array(inventory_id,agent_id,task_count,message_count,result_count,task_observed,message_observed,result_observed,metadata_digest,sealed) FROM task_query_fixed_manifests WHERE inventory_id=?1";
        let before:String=tx.query_row(header_sql,[&original.id],|row|row.get(0))?;
        repositories::upsert_task_tx(tx,&task("raw-unreceived-replace-tail"))?;
        assert!(prepare_task_domain_snapshot_tx(tx,&scope(),&selected("raw-unreceived-replace-tail")).is_err());
        tx.execute("INSERT INTO task_query_fixed_manifests(inventory_id,agent_id,task_observed,message_observed,result_observed) VALUES('replace-staging','task-agent',1,1,1)",[])?;
        let error=tx.execute("INSERT OR REPLACE INTO task_query_fixed_manifests SELECT inventory_id,agent_id,task_count,message_count,result_count,task_observed,message_observed,result_observed,metadata_digest,0 FROM task_query_fixed_manifests WHERE inventory_id=?1",[&original.id]).err().context("sealed header replaced by INSERT")?;
        assert!(error.to_string().contains("fixed Task query inventory is immutable"));
        let error=tx.execute("UPDATE OR REPLACE task_query_fixed_manifests SET inventory_id=?1 WHERE inventory_id='replace-staging'",[&original.id]).err().context("sealed header replaced by UPDATE")?;
        assert!(error.to_string().contains("fixed Task query inventory is immutable"));
        assert_eq!(tx.query_row(header_sql,[&original.id],|row|row.get::<_,String>(0))?,before);
        let actual:u64=tx.query_row("SELECT COUNT(*) FROM task_query_fixed_tasks WHERE inventory_id=?1",[&original.id],|row|row.get(0))?;
        assert_eq!(actual,original.task_count);
        queries::validate(tx,&original)?;
        assert!(prepare_task_domain_snapshot_tx(tx,&scope(),&selected("raw-unreceived-replace-tail")).is_err());
        let current=prepare_task_domain_snapshot_tx(tx,&scope(),&selected("fixed-replace-original"))?.require_current()?;
        assert!(current.resident.capture(&selected("fixed-replace-original").capture)?.lookup(&TaskKey::new(TaskFamily::Task,"fixed-replace-original"))?.is_some());
        // A new staging inventory can still perform its one legal seal.
        tx.execute("UPDATE task_query_fixed_manifests SET sealed=1 WHERE inventory_id='replace-staging'",[])?;
        assert!(tx.query_row("SELECT sealed=1 FROM task_query_fixed_manifests WHERE inventory_id='replace-staging'",[],|row|row.get::<_,bool>(0))?);
        Ok(())
    })
}

fn historical_output_turns(tx: &Transaction<'_>) -> Result<()> {
    repositories::upsert_task_tx(tx, &task("focused-historical-output"))?;
    for index in 1..=514 {
        let mut turn =
            crate::types::TurnRecord::new("task-agent", format!("focused-turn-{index:03}"), index);
        turn.created_at =
            task("time").created_at + chrono::Duration::seconds(i64::try_from(index)?);
        turn.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: turn.created_at + chrono::Duration::seconds(1),
            duration_ms: 1000,
        });
        repositories::upsert_turn_record_tx(tx, &turn)?;
    }
    let mut state = crate::types::AgentState::new("task-agent");
    state.turn_index = 514;
    super::super::evidence::upsert_agent_state_tx(tx, &state)?;
    Ok(())
}

#[test]
fn fixed_output_input_inventory_freezes_owning_versions_outside_the_resident_window_without_body_aliases(
) -> Result<()> {
    let (_home, db) = legacy(true, historical_output_turns)?;
    db.transaction(|tx| {
        let inventory = fixed(tx, "upgrade")?;
        let raw: String = tx.query_row("SELECT fingerprint_json FROM task_query_fixed_input_members WHERE inventory_id=?1 AND family='turn' AND record_id='focused-turn-001'", [&inventory.id], |row| row.get(0))?;
        let frozen: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(frozen["turn_index"], 1);
        assert_eq!(frozen["frozen_payload_digest"].as_str().unwrap().len(), 64);
        let current_alias: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id='task-agent' AND family='turn' AND record_id='focused-turn-001')", [], |row| row.get(0))?;
        assert!(!current_alias, "fixed output metadata cannot mint generic Turn body authority");
        let count: u64 = tx.query_row("SELECT COUNT(*) FROM task_query_fixed_input_members WHERE inventory_id=?1 AND family='turn'", [&inventory.id], |row| row.get(0))?;
        assert_eq!(count, 514, "the durable focused inventory is independent of the resident history window");
        let original = raw.clone();
        assert!(tx.execute("UPDATE task_query_fixed_input_members SET fingerprint_json='{}' WHERE inventory_id=?1 AND family='turn'", [&inventory.id]).is_err());
        assert!(tx.execute("INSERT OR REPLACE INTO task_query_fixed_input_members(inventory_id,family,record_id,fingerprint_json) VALUES(?1,'turn','focused-turn-001','{}')", [&inventory.id]).is_err());
        tx.execute("INSERT INTO task_query_fixed_manifests(inventory_id,agent_id,task_observed,message_observed,result_observed) VALUES('focused-unsealed','task-agent',1,1,1)", [])?;
        tx.execute("INSERT INTO task_query_fixed_input_members(inventory_id,family,record_id,fingerprint_json) VALUES('focused-unsealed','turn','raw-tail','{}')", [])?;
        assert!(tx.execute("UPDATE task_query_fixed_input_members SET inventory_id=?1 WHERE inventory_id='focused-unsealed'", [&inventory.id]).is_err());
        let actual: String = tx.query_row("SELECT fingerprint_json FROM task_query_fixed_input_members WHERE inventory_id=?1 AND family='turn' AND record_id='focused-turn-001'", [&inventory.id], |row| row.get(0))?;
        assert_eq!(actual, original);
        queries::validate(tx, &inventory)?;
        Ok(())
    })
}

#[test]
fn first_query_import_copies_focused_input_versions_without_adopting_late_or_foreign_turns(
) -> Result<()> {
    let (_home, db) = legacy(false, historical_output_turns)?;
    db.transaction(|tx| {
        repositories::upsert_turn_record_tx(
            tx,
            &crate::types::TurnRecord::new("task-agent", "unreceived-output-turn", 515),
        )?;
        repositories::upsert_turn_record_tx(
            tx,
            &crate::types::TurnRecord::new("foreign-agent", "foreign-output-turn", 1),
        )?;
        Ok(())
    })?;
    db.context_episodes().import_legacy(Vec::new())?;
    db.transaction(|tx| {
        let original = fixed(tx, "upgrade")?;
        let imported = fixed(tx, "first_import")?;
        assert_ne!(original.id, imported.id);
        let count: u64 = tx.query_row("SELECT COUNT(*) FROM task_query_fixed_input_members WHERE inventory_id=?1 AND family='turn'", [&imported.id], |row| row.get(0))?;
        assert_eq!(count, 514);
        let old: String = tx.query_row("SELECT fingerprint_json FROM task_query_fixed_input_members WHERE inventory_id=?1 AND family='turn' AND record_id='focused-turn-001'", [&original.id], |row| row.get(0))?;
        let new: String = tx.query_row("SELECT fingerprint_json FROM task_query_fixed_input_members WHERE inventory_id=?1 AND family='turn' AND record_id='focused-turn-001'", [&imported.id], |row| row.get(0))?;
        assert_eq!(old, new, "first JSON import retains the original accepted version");
        let late: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM task_query_fixed_input_members WHERE inventory_id=?1 AND record_id IN ('unreceived-output-turn','foreign-output-turn'))", [&imported.id], |row| row.get(0))?;
        assert!(!late);
        queries::validate(tx, &imported)?;
        Ok(())
    })
}

#[test]
fn focused_fixed_input_scalar_budget_precedes_an_oversized_unselected_turn_header() -> Result<()> {
    let error = legacy(true, |tx| {
        historical_output_turns(tx)?;
        tx.execute("UPDATE turn_records SET created_at=CAST(zeroblob(9437184) AS TEXT) WHERE turn_id='focused-turn-001'", [])?;
        Ok(())
    }).err().context("oversized focused Turn was acquired before its scalar budget")?;
    assert!(
        format!("{error:#}").contains("fixed Task output Input material"),
        "{error:#}"
    );
    Ok(())
}
