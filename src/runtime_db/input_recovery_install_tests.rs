use super::*;
use crate::{
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        input_mutation_receipts::{
            canonical_command_digest, insert_input_mutation_receipt_tx,
            lookup_input_mutation_receipt_tx, InputMutationFamily, InputMutationOutcome,
            InputMutationScopeOutcome, PreparedInputMutation,
        },
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, CallbackDeliveryMode, ExternalTriggerScope, ExternalTriggerStatus,
        TaskKind, TaskStatus, TimerStatus, WorkItemRecord, WorkItemState,
    },
};

struct Fixture {
    _dir: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    effects: PostCommitEffects,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "cache-targets".into(),
            identity_incarnation: 1,
        };
        let at = chrono::Utc::now();
        let mut work = WorkItemRecord::new(
            &scope.agent_id,
            "original work objective",
            WorkItemState::Open,
        );
        work.id = "work-cache-original".into();
        let task = TaskRecord {
            id: "task-cache-original".into(),
            agent_id: scope.agent_id.clone(),
            kind: TaskKind::SleepJob,
            status: TaskStatus::Completed,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("original task summary".into()),
            detail: None,
            recovery: None,
        };
        let timer = TimerRecord {
            id: "timer-cache-original".into(),
            agent_id: scope.agent_id.clone(),
            created_at: at,
            duration_ms: 1,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Cancelled,
            summary: None,
            next_fire_at: None,
            last_fired_at: None,
            fire_count: 0,
        };
        let trigger = ExternalTriggerRecord {
            external_trigger_id: "trigger-cache-original".into(),
            target_agent_id: scope.agent_id.clone(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            token: None,
            token_hash: "hash".into(),
            status: ExternalTriggerStatus::Revoked,
            created_at: at,
            revoked_at: Some(at),
            last_delivered_at: None,
            delivery_count: 0,
        };
        db.transaction(|tx| {
            upsert_agent_identity_tx(
                tx,
                &AgentIdentityRecord::new(
                    &scope.agent_id,
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
            )?;
            upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
            repositories::insert_new_work_item_tx(tx, &work)?;
            repositories::upsert_task_tx(tx, &task)?;
            repositories::upsert_timer_tx(tx, &timer)?;
            repositories::upsert_external_trigger_tx(tx, &trigger)
        })?;
        Ok(Self {
            _dir: dir,
            db,
            scope,
            effects: PostCommitEffects {
                work_items: vec![work],
                tasks: vec![task],
                timers: vec![timer],
                external_triggers: vec![trigger],
                ..Default::default()
            },
        })
    }
    fn descriptor(&self, nonce: &str) -> Result<PreparedInputMutation> {
        PreparedInputMutation::new(
            InputMutationFamily::Queue,
            nonce.into(),
            vec![self.scope.clone()],
            canonical_command_digest(&serde_json::json!({"original":nonce}))?,
            chrono::Utc::now(),
        )
    }
    fn outcome(&self, keys: InputMutationInstallKeys) -> InputMutationOutcome {
        InputMutationOutcome {
            result: Default::default(),
            applied: true,
            delivery_receipt: None,
            scopes: vec![InputMutationScopeOutcome {
                scope: self.scope.clone(),
                install_keys: keys,
                audit_events: Vec::new(),
                notify_memory_index: false,
                notify_scheduler: true,
            }],
        }
    }
}

#[test]
fn original_receipt_keeps_typed_ids_and_recovery_reads_current_values() -> Result<()> {
    let f = Fixture::new()?;
    let keys = InputMutationInstallKeys::from_effects(&f.scope, &f.effects)?;
    let descriptor = f.descriptor("cache-original")?;
    let original = f.db.transaction(|tx| {
        insert_input_mutation_receipt_tx(
            tx,
            &descriptor,
            &f.outcome(keys.clone()),
            chrono::Utc::now(),
        )
    })?;
    let encoded = serde_json::to_string(&original)?;
    assert!(
        !encoded.contains("original work objective") && !encoded.contains("original task summary")
    );
    let mut current_work = f.effects.work_items[0].clone();
    current_work.revision += 1;
    current_work.objective = "current work objective after receipt".into();
    assert!(f.db.work_items().update_expected(&current_work, 1)?);
    f.db.transaction(|tx| {
        let replay = lookup_input_mutation_receipt_tx(tx, &descriptor)?
            .context("original receipt missing")?;
        assert_eq!(replay, original);
        let current =
            load_current_install_facts_tx(tx, &f.scope, &replay.outcome.scopes[0].install_keys)?;
        assert_eq!(current.keys.work_items, keys.work_items);
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id=?1",
            [&current_work.id],
            |row| row.get(0),
        )?;
        assert_eq!(
            repositories::decode_work_item_payload(&payload)?,
            current_work
        );
        assert_eq!(current.tasks.len(), 1);
        assert_eq!(current.timers.len(), 1);
        assert_eq!(current.external_triggers.len(), 1);
        Ok(())
    })
}

#[test]
fn missing_current_rows_retain_only_original_keys_for_precise_cache_removal() -> Result<()> {
    let f = Fixture::new()?;
    let keys = InputMutationInstallKeys::from_effects(&f.scope, &f.effects)?;
    let descriptor = f.descriptor("cache-pruned")?;
    let original = f.db.transaction(|tx| {
        insert_input_mutation_receipt_tx(
            tx,
            &descriptor,
            &f.outcome(keys.clone()),
            chrono::Utc::now(),
        )
    })?;
    let mut unrelated = WorkItemRecord::new(
        &f.scope.agent_id,
        "unrelated cache item",
        WorkItemState::Open,
    );
    unrelated.id = "work-cache-unrelated".into();
    f.db.work_items().insert_new(&unrelated)?;
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM work_items WHERE work_item_id=?1",
            [&f.effects.work_items[0].id],
        )?;
        let current = load_current_install_facts_tx(tx, &f.scope, &keys)?;
        // Only finite selectors remain; no Work body is read into the legacy DTO.
        assert!(tx.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM work_items WHERE work_item_id=?1)",
            [&f.effects.work_items[0].id],
            |row| row.get::<_, bool>(0)
        )?);
        assert_eq!(current.keys, keys);
        assert!(!current.keys.work_items.contains(&unrelated.id));
        assert_eq!(
            lookup_input_mutation_receipt_tx(tx, &descriptor)?,
            Some(original.clone())
        );
        Ok(())
    })
}

#[test]
fn original_missing_target_rolls_back_receipt_and_preceding_state_write() -> Result<()> {
    let f = Fixture::new()?;
    let mut keys = InputMutationInstallKeys::from_effects(&f.scope, &f.effects)?;
    keys.work_items.insert("work-never-written".into());
    let descriptor = f.descriptor("cache-invalid-original")?;
    let before = f.db.agent_states().latest(&f.scope.agent_id)?.unwrap();
    assert!(f
        .db
        .transaction(|tx| {
            let mut changed = before.clone();
            changed.total_message_count = 99;
            upsert_agent_state_tx(tx, &changed)?;
            insert_input_mutation_receipt_tx(
                tx,
                &descriptor,
                &f.outcome(keys.clone()),
                chrono::Utc::now(),
            )
        })
        .is_err());
    assert_eq!(f.db.agent_states().latest(&f.scope.agent_id)?, Some(before));
    f.db.transaction(|tx| {
        assert!(lookup_input_mutation_receipt_tx(tx, &descriptor)?.is_none());
        Ok(())
    })
}

#[test]
fn every_family_rejects_foreign_normalized_agent_and_mismatched_payload_id() -> Result<()> {
    let f = Fixture::new()?;
    let keys = InputMutationInstallKeys::from_effects(&f.scope, &f.effects)?;
    for (table, id_column, agent_column, id, payload_id) in [
        (
            "work_items",
            "work_item_id",
            "agent_id",
            &f.effects.work_items[0].id,
            "id",
        ),
        (
            "tasks",
            "task_id",
            "owner_agent_id",
            &f.effects.tasks[0].id,
            "id",
        ),
        (
            "timers",
            "timer_id",
            "agent_id",
            &f.effects.timers[0].id,
            "id",
        ),
        (
            "external_triggers",
            "external_trigger_id",
            "target_agent_id",
            &f.effects.external_triggers[0].external_trigger_id,
            "external_trigger_id",
        ),
    ] {
        f.db.transaction(|tx| {
            let sql = format!("UPDATE {table} SET {agent_column}='foreign-agent' WHERE {id_column}=?1");
            tx.execute(&sql, [id])?;
            assert!(validate_original_install_keys_tx(tx, &f.scope, &keys).is_err());
            tx.execute(&format!("UPDATE {table} SET {agent_column}=?2 WHERE {id_column}=?1"), rusqlite::params![id,f.scope.agent_id])?;
            let sql = format!("UPDATE {table} SET payload_json=json_set(payload_json,'$.{payload_id}','foreign-id') WHERE {id_column}=?1");
            tx.execute(&sql, [id])?;
            assert!(validate_original_install_keys_tx(tx, &f.scope, &keys).is_err());
            tx.execute(&format!("UPDATE {table} SET payload_json=json_set(payload_json,'$.{payload_id}',?2) WHERE {id_column}=?1"), rusqlite::params![id,id])?;
            let payload_agent = if table == "external_triggers" { "target_agent_id" } else { "agent_id" };
            tx.execute(&format!("UPDATE {table} SET payload_json=json_set(payload_json,'$.{payload_agent}','foreign-agent') WHERE {id_column}=?1"), [id])?;
            assert!(validate_original_install_keys_tx(tx, &f.scope, &keys).is_err());
            tx.execute(&format!("UPDATE {table} SET payload_json=json_set(payload_json,'$.{payload_agent}',?2) WHERE {id_column}=?1"), rusqlite::params![id,f.scope.agent_id])?;
            Ok(())
        })?;
    }
    let mut foreign = f.effects.clone();
    foreign.work_items[0].agent_id = "foreign-agent".into();
    assert!(InputMutationInstallKeys::from_effects(&f.scope, &foreign).is_err());
    Ok(())
}
