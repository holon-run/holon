use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    for (id, second) in [("a", 0), ("b", 0), ("c", 1)] {
        db.workspace_occupancies()
            .upsert(&WorkspaceOccupancyRecord {
                occupancy_id: id.into(),
                execution_root_id: "root 汉🦀 \"{}[],:\\ ".into(),
                workspace_id: "workspace".into(),
                holder_agent_id: "occupancy-reader".into(),
                access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
                acquired_at: DateTime::from_timestamp(1_792_540_800 + second, 0).unwrap(),
                released_at: None,
            })?;
    }
    Ok((home, db))
}

#[test]
fn occupancy_source_admits_original_order_and_latches_exact_limits() -> Result<()> {
    let (_home, db) = fixture()?;
    let expected = db.workspace_occupancies().latest_all()?;
    assert_eq!(
        expected
            .iter()
            .map(|record| record.occupancy_id.as_str())
            .collect::<Vec<_>>(),
        ["b", "a", "c"],
    );
    let scope = InputEvidenceScope {
        agent_id: "occupancy-reader".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    assert_eq!(
        phase.read(|| db.workspace_occupancies().latest_all())?,
        expected
    );
    let usage = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for shortage in [None, Some(true), Some(false)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            usage.0 - u64::from(shortage == Some(true)),
            usage.1 - usize::from(shortage == Some(false)),
        )?;
        let result = phase.read(|| db.workspace_occupancies().latest_all());
        if shortage.is_none() {
            assert_eq!(result?, expected);
            assert_eq!(phase.finish()?, usage);
            let holder = phase.guard()?;
            let weak = Arc::downgrade(&holder);
            drop(phase);
            assert!(weak.upgrade().is_some());
            drop(holder);
            assert!(weak.upgrade().is_none());
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                format!("{error:#}"),
                "ResourceExhausted: context resource read material",
            );
            let held = ledger.report()?.observed_bytes;
            let retry = phase
                .read(|| db.workspace_occupancies().latest_all())
                .unwrap_err();
            assert_eq!(
                format!("{retry:#}"),
                "resourceexhausted: context resource read material",
            );
            assert_eq!(ledger.report()?.observed_bytes, held);
            drop(phase);
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn occupancy_source_preserves_column_and_decode_failures() -> Result<()> {
    let (_home, db) = fixture()?;
    for payload in [
        rusqlite::types::Value::Blob(vec![0xff]),
        rusqlite::types::Value::Text("{".into()),
    ] {
        db.connection()?.execute(
            "UPDATE workspace_occupancies SET payload_json=?1 WHERE occupancy_id='c'",
            [&payload],
        )?;
        let expected = (|| -> Result<Vec<WorkspaceOccupancyRecord>> {
            let connection = db.connection()?;
            let mut statement = connection.prepare(
                "SELECT payload_json FROM workspace_occupancies ORDER BY acquired_at DESC, occupancy_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_workspace_occupancy_payload(&row?)).collect()
        })().unwrap_err();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "occupancy-reader".into(),
            identity_incarnation: 1,
        };
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
        let actual = phase
            .read(|| db.workspace_occupancies().latest_all())
            .unwrap_err();
        assert_eq!(format!("{actual:#}"), format!("{expected:#}"));
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
