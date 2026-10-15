//! Explicit by-ID inspection only; no Work alias, window, State or Ready grant.
use super::RuntimeDb;
use crate::runtime::{
    input_evidence_budget::{
        BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
};
use crate::types::WorkItemRecord;
use anyhow::{ensure, Context, Result};
use rusqlite::OptionalExtension;
use std::sync::Arc;

#[path = "work_item_report_inspection.rs"]
mod report_inspection;
pub(crate) use report_inspection::{ExplicitWorkReport, ExplicitWorkReportSource};

pub(crate) struct ExplicitWorkItemRecall {
    record: WorkItemRecord,
    material: ExplicitWorkItemRecallMaterial,
}
pub(crate) struct ExplicitWorkItemRecallMaterial {
    bytes: u64,
    records: usize,
    _raw: Arc<BudgetViewGuard>,
    _native: Arc<BudgetViewGuard>,
    report: Option<ExplicitWorkReport>,
    _report_guards: Vec<Arc<BudgetViewGuard>>,
}
impl ExplicitWorkItemRecallMaterial {
    pub(crate) fn completion_report(&self) -> Option<&ExplicitWorkReport> {
        self.report.as_ref()
    }
}
impl ExplicitWorkItemRecall {
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        (self.material.bytes, self.material.records)
    }
    pub(crate) fn into_parts(self) -> (WorkItemRecord, ExplicitWorkItemRecallMaterial) {
        (self.record, self.material)
    }
}
impl RuntimeDb {
    /// Accounting uses the actual current caller scope, including foreign-row
    /// inspection. It authorizes no foreign Home/identity or resident adoption.
    pub(crate) fn inspect_work_item_for_explicit_recall(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        id: &str,
        byte_limit: u64,
        record_limit: usize,
    ) -> Result<Option<ExplicitWorkItemRecall>> {
        let exhausted = |label| super::work_snapshot::WorkSnapshotError::ResourceExhausted(label);
        ensure!(
            scope.identity_incarnation > 0 && !id.is_empty(),
            "explicit Work recall lacks its original caller scope/ID"
        );
        let byte_limit = byte_limit.min(64 * 1024 * 1024);
        let record_limit = record_limit.min(65_536);
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        // Every stored variable column is measured by SQLite before any String
        // or JSON projection. Keep this raw guard before native JSON traversal.
        let raw:Option<u64>=tx.query_row(
            "SELECT 512+length(CAST(payload_json AS BLOB))+length(CAST(work_item_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(state AS BLOB))+length(CAST(objective AS BLOB))+length(CAST(revision AS BLOB))+length(CAST(current_focus AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+COALESCE(length(CAST(plan_status AS BLOB)),0)+COALESCE(length(CAST(completed_at AS BLOB)),0)+COALESCE(length(CAST(plan_artifact_path AS BLOB)),0)+COALESCE(length(CAST(last_turn_id AS BLOB)),0)+COALESCE(length(CAST(last_message_id AS BLOB)),0)+COALESCE(length(CAST(causation_id AS BLOB)),0)+COALESCE(length(CAST(correlation_id AS BLOB)),0)+COALESCE(length(CAST(blocked_by AS BLOB)),0)+COALESCE(length(CAST(recheck_at AS BLOB)),0)+COALESCE(length(CAST(recheck_consumed_at AS BLOB)),0) FROM work_items WHERE work_item_id=?1",[id],|row|row.get(0)).optional()?;
        let Some(raw) = raw else {
            tx.commit()?;
            return Ok(None);
        };
        let raw_bytes = raw
            .checked_mul(8)
            .context(exhausted("explicit Work recall raw overflow"))?;
        ensure!(
            raw_bytes <= byte_limit && 8 <= record_limit,
            exhausted("explicit Work recall raw")
        );
        let raw_guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: raw_bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let nodes:u64=tx.query_row("SELECT COUNT(*)+COALESCE(SUM(CASE WHEN typeof(j.key)='text' THEN 1 ELSE 0 END),0)+20 FROM work_items w,json_tree(w.payload_json) j WHERE w.work_item_id=?1",[id],|row|row.get(0))?;
        let records = usize::try_from(nodes)?
            .checked_mul(8)
            .context(exhausted("explicit Work recall native overflow"))?;
        let native_bytes = u64::try_from(records)?
            .checked_mul(256)
            .context(exhausted("explicit Work recall native bytes overflow"))?;
        let bytes = raw_bytes
            .checked_add(native_bytes)
            .context(exhausted("explicit Work recall combined overflow"))?;
        let total_records = records
            .checked_add(8)
            .context(exhausted("explicit Work recall count overflow"))?;
        ensure!(
            bytes <= byte_limit && total_records <= record_limit,
            exhausted("explicit Work recall native")
        );
        let native_guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: native_bytes,
            },
        )?;
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id=?1",
            [id],
            |row| row.get(0),
        )?;
        let record = super::repositories::decode_work_item_payload(&payload)?;
        ensure!(
            record.id == id,
            "explicit Work recall canonical row ID differs"
        );
        let inspected = report_inspection::inspect_tx(
            &tx,
            scope,
            ledger,
            &record,
            (bytes, total_records),
            (byte_limit, record_limit),
        )?;
        tx.commit()?;
        Ok(Some(ExplicitWorkItemRecall {
            record,
            material: ExplicitWorkItemRecallMaterial {
                bytes: inspected.cost.0,
                records: inspected.cost.1,
                _raw: raw_guard,
                _native: native_guard,
                report: inspected.report,
                _report_guards: inspected.guards,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        WorkItemState,
    };
    use std::collections::BTreeSet;

    #[test]
    fn explicit_recall_measures_actual_migrated_work_columns_before_bad_body_decode() -> Result<()>
    {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let identity = AgentIdentityRecord::new(
            "actual-recall-scope",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        let record = WorkItemRecord::new(
            &identity.agent_id,
            "original recalled body",
            WorkItemState::Open,
        );
        db.transaction(|tx| {
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)?;
            super::super::repositories::insert_new_work_item_tx(tx, &record)?;
            Ok(())
        })?;
        let columns = {
            let connection = db.connection()?;
            let mut statement = connection.prepare("PRAGMA table_info(work_items)")?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            columns
        };
        assert_eq!(
            columns,
            [
                "work_item_id",
                "agent_id",
                "state",
                "objective",
                "plan_status",
                "revision",
                "current_focus",
                "created_at",
                "updated_at",
                "completed_at",
                "plan_artifact_path",
                "last_turn_id",
                "last_message_id",
                "causation_id",
                "correlation_id",
                "payload_json",
                "blocked_by",
                "recheck_at",
                "recheck_consumed_at",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        // Actual migrations have 19 fields: readiness was removed, and these
        // three scalar blocker/recheck columns replaced the old projection.
        let scope = InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let baseline = ledger.report()?.observed_bytes;
        for column in ["blocked_by", "recheck_at", "recheck_consumed_at"] {
            db.transaction(|tx| {
                tx.execute(&format!("UPDATE work_items SET {column}=?2,payload_json='{{invalid}}' WHERE work_item_id=?1"), rusqlite::params![record.id, "x".repeat(65 * 1024)])?;
                Ok(())
            })?;
            let error = db
                .inspect_work_item_for_explicit_recall(&scope, &ledger, &record.id, 4096, 256)
                .err()
                .context("normalized field was decoded before the raw cap")?;
            assert!(
                matches!(
                    error.downcast_ref::<super::super::work_snapshot::WorkSnapshotError>(),
                    Some(
                        super::super::work_snapshot::WorkSnapshotError::ResourceExhausted(
                            "explicit Work recall raw"
                        )
                    )
                ),
                "{error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            db.transaction(|tx| {
                tx.execute(
                    &format!(
                        "UPDATE work_items SET {column}=NULL,payload_json=?2 WHERE work_item_id=?1"
                    ),
                    rusqlite::params![record.id, serde_json::to_string(&record)?],
                )?;
                Ok(())
            })?;
        }
        let read = db
            .inspect_work_item_for_explicit_recall(
                &scope,
                &ledger,
                &record.id,
                64 * 1024 * 1024,
                65_536,
            )?
            .context("actual migrated recall row absent")?;
        let (actual, material) = read.into_parts();
        assert_eq!(actual, record);
        assert!(ledger.report()?.observed_bytes > baseline);
        drop(actual);
        drop(material);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        Ok(())
    }
}
