//! Global latest-for-child headers accepted by the actual delegation writer.
use super::*;
use crate::runtime_db::repositories::OriginalDelegationWrite;

pub(in crate::runtime_db) fn register_delegation_write_tx(
    tx: &Transaction<'_>,
    observed: &OriginalDelegationWrite<'_, '_>,
) -> Result<()> {
    let id = observed.source_in(tx)?;
    let changed = tx.execute(
        "INSERT INTO task_status_delegation_sources(delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at)
         SELECT delegation_id,child_agent_id,parent_work_item_id,child_work_item_id,created_at,updated_at
         FROM work_item_delegations WHERE delegation_id=?1
         ON CONFLICT(delegation_id) DO UPDATE SET child_agent_id=excluded.child_agent_id,
          parent_work_item_id=excluded.parent_work_item_id,child_work_item_id=excluded.child_work_item_id,
          created_at=excluded.created_at,updated_at=excluded.updated_at",
        [id],
    )?;
    ensure!(changed == 1, "Delegation ACK omitted its stored metadata");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WorkItemDelegationRecord, WorkItemDelegationState};

    fn delegation() -> WorkItemDelegationRecord {
        let at = super::super::tests::at();
        WorkItemDelegationRecord {
            delegation_id: "original-delegation".into(),
            parent_agent_id: "another-parent".into(),
            parent_work_item_id: "original-parent-work".into(),
            child_agent_id: "shared-child".into(),
            child_work_item_id: "original-child-work".into(),
            state: WorkItemDelegationState::Open,
            result_summary: Some("not status metadata".repeat(1024)),
            created_at: at,
            updated_at: at,
        }
    }

    fn header(db: &super::super::super::RuntimeDb) -> Result<(String, String, String)> {
        Ok(db.connection()?.query_row(
            "SELECT child_agent_id,parent_work_item_id,child_work_item_id FROM task_status_delegation_sources WHERE delegation_id='original-delegation'",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        )?)
    }

    #[test]
    fn actual_delegation_ack_keeps_global_child_metadata_and_noop_does_not_adopt_raw_drift(
    ) -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let mut original = delegation();
        original.updated_at += chrono::Duration::seconds(1);
        db.work_item_delegations().upsert(&original)?;
        assert_eq!(
            header(&db)?,
            (
                "shared-child".into(),
                "original-parent-work".into(),
                "original-child-work".into()
            )
        );
        db.transaction(|tx| {
            tx.execute("UPDATE work_item_delegations SET parent_work_item_id='unreceived-raw-work' WHERE delegation_id=?1", [&original.delegation_id])?;
            Ok(())
        })?;
        db.work_item_delegations().upsert(&delegation())?;
        assert_eq!(header(&db)?.1, "original-parent-work");
        let mut next = original.clone();
        next.updated_at += chrono::Duration::seconds(1);
        next.parent_work_item_id = "next-parent-work".into();
        next.child_agent_id = "next-child".into();
        db.work_item_delegations().upsert(&next)?;
        assert_eq!(
            header(&db)?,
            (
                "next-child".into(),
                "next-parent-work".into(),
                "original-child-work".into()
            )
        );
        Ok(())
    }

    #[test]
    fn delegation_header_failure_rolls_back_the_original_business_upsert() -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        db.connection()?.execute_batch("CREATE TRIGGER deny_delegation_metadata BEFORE INSERT ON task_status_delegation_sources BEGIN SELECT RAISE(ABORT,'delegation metadata fault'); END;")?;
        let value = delegation();
        let error = db.work_item_delegations().upsert(&value).unwrap_err();
        assert!(format!("{error:#}").contains("delegation metadata fault"));
        let count: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM work_item_delegations WHERE delegation_id=?1",
            [&value.delegation_id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM task_status_delegation_sources",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        Ok(())
    }
}
