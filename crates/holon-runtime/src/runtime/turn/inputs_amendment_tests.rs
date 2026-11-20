use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

#[test]
fn terminal_amendments_reenter_original_material_before_copying_ids() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "terminal-amendment".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let header = ((scope.agent_id.len() + 256) * 8) as u64;
    type Include = fn(&mut BuiltTurnRecord, &str) -> Result<()>;
    for include in [
        BuiltTurnRecord::include_brief as Include,
        BuiltTurnRecord::include_completed_work_item,
        BuiltTurnRecord::include_tool_execution,
        BuiltTurnRecord::include_wait_condition,
    ] {
        let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, header, 64)?;
        let original = TurnRecord::new("terminal-amendment", "turn", 1);
        let mut record = BuiltTurnRecord {
            record: original.clone(),
            material: Some(material.clone()),
            _source: None,
        };
        // The caller supplies no ambient phase.
        assert!(include(&mut record, "rejected-before-owning").is_err());
        assert_eq!(record.record, original);
        assert!(material.finish().is_err());
        drop((record, material));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let material = ResourceReadMaterial::new(&ledger, &scope)?;
    let mut record = BuiltTurnRecord {
        record: TurnRecord::new("terminal-amendment", "turn", 1),
        material: Some(material.clone()),
        _source: None,
    };
    record.include_brief("brief")?;
    record.include_completed_work_item("work")?;
    record.include_tool_execution("tool")?;
    record.include_wait_condition("wait")?;
    let before = material.finish()?;
    record.include_brief("brief")?;
    assert_eq!(before, material.finish()?);
    let last = record.try_clone()?;
    let weak = Arc::downgrade(&material.guard()?);
    drop((record, material));
    assert!(weak.upgrade().is_some());
    assert_eq!(last.produced_brief_ids, ["brief"]);
    assert_eq!(last.completed_work_item_ids, ["work"]);
    assert_eq!(last.tool_execution_ids, ["tool"]);
    assert_eq!(last.waiting_condition_ids, ["wait"]);
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
