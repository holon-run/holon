//! The Task schema cut freezes metadata, independently of Episode/body rights.
use super::*;

const ROW_MATERIAL_BYTES: usize = 64 * 1024 * 1024;

fn admit_row(bytes: usize) -> Result<()> {
    ensure!(
        bytes
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .is_some_and(|n| n <= ROW_MATERIAL_BYTES),
        "fixed TaskStatus source row material exceeds budget"
    );
    Ok(())
}

fn table_observed(tx: &Transaction<'_>, table: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )?)
}

pub(in crate::runtime_db) fn capture_status_sources_upgrade_tx(tx: &Transaction<'_>) -> Result<()> {
    let initialized: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1)",
        [],
        |row| row.get(0),
    )?;
    if initialized {
        return Ok(());
    }
    let control_observed = table_observed(tx, "control_cancellation_obligations")?;
    let delegation_observed = table_observed(tx, "work_item_delegations")?;
    tx.execute("INSERT INTO task_status_source_inventory(singleton,control_observed,delegation_observed,sealed) VALUES(1,?1,?2,0)", params![control_observed,delegation_observed])?;
    if control_observed {
        let mut query = tx.prepare("SELECT length(CAST(agent_id AS BLOB))+length(CAST(command_id AS BLOB))+length(CAST(obligation_id AS BLOB))+length(CAST(target_identity AS BLOB))+length(CAST(target_json AS BLOB))+length(CAST(target_generation_json AS BLOB))+length(CAST(completion_evidence_json AS BLOB))+length(CAST(updated_at AS BLOB)),agent_id,identity_incarnation,command_id,obligation_id FROM control_cancellation_obligations WHERE target_kind='worktree' AND state='completed' ORDER BY agent_id,identity_incarnation,command_id,obligation_id")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            admit_row(row.get(0)?)?;
            let (agent, incarnation, command, obligation): (String, u64, String, String) =
                (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?);
            let valid: bool = tx.query_row("SELECT json_valid(target_json) AND json_valid(target_generation_json) AND json_valid(completion_evidence_json) FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3 AND obligation_id=?4", params![agent,incarnation,command,obligation], |row| row.get(0))?;
            ensure!(valid, "fixed TaskStatus Control source JSON is invalid");
            // Preserve the original overlay's normalized candidate predicate,
            // then its decoded target-kind/identity checks. Generation JSON is
            // taken from the same target payload the original reader decoded.
            tx.execute("INSERT INTO task_status_fixed_controls(agent_id,identity_incarnation,command_id,obligation_id,task_id,target_generation_json,fields_json,updated_at)
                SELECT c.agent_id,c.identity_incarnation,c.command_id,c.obligation_id,c.target_identity,json_extract(c.target_json,'$.target_generation'),
                 COALESCE((SELECT json_group_object(e.key,CASE e.type WHEN 'object' THEN json(e.value) WHEN 'array' THEN json(e.value) WHEN 'true' THEN json('true') WHEN 'false' THEN json('false') WHEN 'null' THEN json('null') ELSE e.value END)
                   FROM json_each(c.completion_evidence_json,'$.detail.worktree') e WHERE e.key IN ('cleanup_status','cleanup_reason','cleanup_error','auto_cleaned_up','changed_files','retained_for_review','actual_branch','branch_cleanup_status','branch_cleanup_error')),'{}'),c.updated_at
                FROM control_cancellation_obligations c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.command_id=?3 AND c.obligation_id=?4
                 AND json_extract(c.target_json,'$.target_kind')='worktree' AND json_extract(c.target_json,'$.target_identity')=c.target_identity",
                params![agent,incarnation,command,obligation])?;
        }
    }
    if delegation_observed {
        let mut query = tx.prepare("SELECT length(CAST(delegation_id AS BLOB))+length(CAST(child_agent_id AS BLOB))+length(CAST(parent_work_item_id AS BLOB))+length(CAST(child_work_item_id AS BLOB))+length(CAST(payload_json AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB)),delegation_id FROM work_item_delegations ORDER BY delegation_id")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            admit_row(row.get(0)?)?;
            let id: String = row.get(1)?;
            let valid: bool = tx.query_row(
                "SELECT json_valid(payload_json) FROM work_item_delegations WHERE delegation_id=?1",
                [&id],
                |row| row.get(0),
            )?;
            ensure!(valid, "fixed TaskStatus delegation JSON is invalid");
            let agrees: bool = tx.query_row("SELECT COALESCE(json_extract(payload_json,'$.delegation_id')=delegation_id AND json_extract(payload_json,'$.child_agent_id')=child_agent_id AND json_extract(payload_json,'$.parent_work_item_id')=parent_work_item_id AND json_extract(payload_json,'$.child_work_item_id')=child_work_item_id,0) FROM work_item_delegations WHERE delegation_id=?1", [&id], |row| row.get(0))?;
            ensure!(
                agrees,
                "fixed TaskStatus delegation normalized metadata differs"
            );
            tx.execute("INSERT INTO task_status_fixed_delegations(delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at) SELECT delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at FROM work_item_delegations WHERE delegation_id=?1", [&id])?;
        }
    }
    let mut hash = Sha256::new();
    for (kind, sql) in [
        ("control", "SELECT length(CAST(json_array(agent_id,identity_incarnation,command_id,obligation_id,task_id,target_generation_json,fields_json,updated_at) AS BLOB)),json_array(agent_id,identity_incarnation,command_id,obligation_id,task_id,target_generation_json,fields_json,updated_at) FROM task_status_fixed_controls ORDER BY agent_id,identity_incarnation,command_id,obligation_id"),
        ("delegation", "SELECT length(CAST(json_array(delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at) AS BLOB)),json_array(delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at) FROM task_status_fixed_delegations ORDER BY delegation_id"),
    ] {
        hash.update(kind.as_bytes());
        let mut query = tx.prepare(sql)?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let size: usize = row.get(0)?;
            admit_row(size)?;
            let encoded: String = row.get(1)?;
            ensure!(encoded.len() == size, "fixed TaskStatus seal size differs");
            hash.update(u64::try_from(size)?.to_le_bytes());
            hash.update(encoded.as_bytes());
        }
    }
    tx.execute("INSERT INTO task_status_control_sources(agent_id,identity_incarnation,command_id,obligation_id,task_id,target_generation_json,fields_json,updated_at) SELECT agent_id,identity_incarnation,command_id,obligation_id,task_id,target_generation_json,fields_json,updated_at FROM task_status_fixed_controls", [])?;
    tx.execute("INSERT INTO task_status_delegation_sources(delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at) SELECT delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at FROM task_status_fixed_delegations", [])?;
    let changed = tx.execute("UPDATE task_status_source_inventory SET control_count=(SELECT COUNT(*) FROM task_status_fixed_controls),delegation_count=(SELECT COUNT(*) FROM task_status_fixed_delegations),metadata_digest=?1,sealed=1 WHERE singleton=1 AND sealed=0", [format!("{:x}",hash.finalize())])?;
    ensure!(changed == 1, "fixed TaskStatus source seal changed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
    use crate::types::WorkItemDelegationRecord;

    fn old_connection() -> Result<Connection> {
        let mut connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "foreign_keys", true)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS
            .iter()
            .filter(|m| m.version < TASK_DOMAIN_VERSION)
        {
            apply_migration(&mut connection, migration)?;
        }
        Ok(connection)
    }

    fn seed_delegation(connection: &Connection) -> Result<()> {
        let mut record = WorkItemDelegationRecord::new(
            "another-parent",
            "old-parent-work",
            "shared-child",
            "old-child-work",
        );
        record.delegation_id = "fixed-delegation".into();
        record.created_at = super::super::tests::at();
        record.updated_at = record.created_at;
        connection.execute("INSERT INTO work_item_delegations(delegation_id,parent_agent_id,parent_work_item_id,child_agent_id,child_work_item_id,state,created_at,updated_at,payload_json) VALUES(?1,?2,?3,?4,?5,'open',?6,?6,?7)", params![record.delegation_id,record.parent_agent_id,record.parent_work_item_id,record.child_agent_id,record.child_work_item_id,repositories::timestamp(record.created_at),serde_json::to_string(&record)?])?;
        Ok(())
    }

    fn seed_control(connection: &Connection, evidence: &str) -> Result<()> {
        connection.execute("INSERT INTO control_command_receipts VALUES('old-parent',7,'fixed-control',1,'stop','invocation','command','2026-10-01T00:00:00Z','{}','{}')", [])?;
        let generation = serde_json::json!({"created_at":"2026-10-01T00:00:00Z","rejoin":null,"artifact":{"path":"/original"}});
        let target = serde_json::json!({"obligation_id":"fixed-worktree","target_kind":"worktree","target_identity":"original-task","target_generation":generation});
        connection.execute("INSERT INTO control_cancellation_obligations VALUES('old-parent',7,'fixed-control','fixed-worktree','worktree','original-task',?1,'2026-10-01T00:00:00Z','completed','2026-10-01T00:00:01Z',?2,?3)",params![generation.to_string(),target.to_string(),evidence])?;
        Ok(())
    }

    fn migrate_task(connection: &mut Connection) -> Result<()> {
        apply_migration(
            connection,
            MIGRATIONS
                .iter()
                .find(|m| m.version == TASK_DOMAIN_VERSION)
                .unwrap(),
        )
    }

    #[test]
    fn original_task_schema_cut_freezes_global_status_headers_and_never_reads_late_tail(
    ) -> Result<()> {
        let mut connection = old_connection()?;
        seed_delegation(&connection)?;
        seed_control(
            &connection,
            r#"{"detail":{"worktree":{"cleanup_status":"retained","changed_files":["a.rs"],"auto_cleaned_up":false,"path":"/forbidden"}}}"#,
        )?;
        migrate_task(&mut connection)?;
        let fields: String=connection.query_row("SELECT fields_json FROM task_status_control_sources WHERE agent_id='old-parent' AND identity_incarnation=7", [], |row|row.get(0))?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fields)?,
            serde_json::json!({"cleanup_status":"retained","changed_files":["a.rs"],"auto_cleaned_up":false})
        );
        let manifest: (bool,bool,u64,u64,usize)=connection.query_row("SELECT control_observed,delegation_observed,control_count,delegation_count,length(metadata_digest) FROM task_status_source_inventory WHERE singleton=1 AND sealed=1", [], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
        assert_eq!(manifest, (true, true, 1, 1, 64));
        connection.execute_batch("ALTER TABLE work_item_delegations RENAME TO status_raw_delegations_forbidden; ALTER TABLE control_cancellation_obligations RENAME TO status_raw_controls_forbidden;")?;
        let tx = connection.transaction()?;
        capture_status_sources_upgrade_tx(&tx)?;
        let parent:String=tx.query_row("SELECT parent_work_item_id FROM task_status_delegation_sources WHERE child_agent_id='shared-child'", [], |row|row.get(0))?;
        assert_eq!(parent, "old-parent-work");
        assert!(tx
            .execute(
                "UPDATE task_status_fixed_delegations SET parent_work_item_id='raw'",
                []
            )
            .is_err());
        assert!(tx
            .execute("DELETE FROM task_status_fixed_controls", [])
            .is_err());
        tx.commit()?;
        Ok(())
    }

    #[test]
    fn missing_original_status_tables_stay_unavailable_at_the_task_cut() -> Result<()> {
        let mut connection = old_connection()?;
        connection.execute_batch(
            "DROP TABLE control_cancellation_obligations; DROP TABLE work_item_delegations;",
        )?;
        migrate_task(&mut connection)?;
        let flags:(bool,bool)=connection.query_row("SELECT control_observed,delegation_observed FROM task_status_source_inventory WHERE singleton=1 AND sealed=1", [], |row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(flags, (false, false));
        Ok(())
    }

    #[test]
    fn oversized_original_status_material_fails_before_json_decode_and_rolls_back_task_migration(
    ) -> Result<()> {
        let mut connection = old_connection()?;
        seed_control(&connection, &"invalid original JSON ".repeat(500_000))?;
        let error = migrate_task(&mut connection).unwrap_err();
        assert!(format!("{error:#}").contains("source row material exceeds budget"));
        assert!(!connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=?1)",
            [TASK_DOMAIN_VERSION],
            |row| row.get::<_, bool>(0)
        )?);
        assert!(!connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='task_status_source_inventory')", [], |row|row.get::<_,bool>(0))?);
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM control_cancellation_obligations",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            1
        );
        Ok(())
    }
}
