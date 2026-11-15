use super::recovery_fixture::{legacy_completed_work_fixture, RECOVERY_ID, SOURCE_ID, WORK_ID};
use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        resident_input_evidence::{InputEvidenceKey, InputEvidenceReadError},
        resident_task::TaskCaptureRequest,
        resident_work::{WorkCaptureRequest, WorkFamily, WorkKey},
    },
};
use anyhow::Context;
use std::collections::BTreeSet;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: 1,
    }
}

fn request() -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        record_limit: 65_536,
        byte_limit: 32 * 1024 * 1024,
    }
}

#[test]
fn original_input_recovery_does_not_promote_historical_work_sources() -> Result<()> {
    let (_directory, db) = legacy_completed_work_fixture(128)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let material = ResourceReadMaterial::new(&ledger, &scope())?;
    material.read(|| {
        db.transaction(|tx| {
            let state = AgentState::new("default");
            let key = WorkKey::new(WorkFamily::Work, WORK_ID);
            for with_queries in [false, true] {
                for receipt_only in [false, true] {
                    let mut works = WorkCaptureRequest::current();
                    if receipt_only {
                        works.nonopen_limit = 0;
                        works.mandatory_roots.insert(key.clone());
                    }
                    let inputs = request();
                    let tasks = TaskCaptureRequest::current();
                    let prepared = if with_queries {
                        prepare_original_input_recovery_with_queries_tx(
                            tx,
                            &scope(),
                            &state,
                            &inputs,
                            16,
                            &ledger,
                            &works,
                            &tasks,
                            None,
                        )
                    } else {
                        prepare_original_input_recovery_with_work_tx(
                            tx,
                            &scope(),
                            &state,
                            &inputs,
                            16,
                            &ledger,
                            &works,
                            &tasks,
                        )
                    }
                    .with_context(|| {
                        format!("with_queries={with_queries}, receipt_only={receipt_only}")
                    })?;
                    let work = prepared.work.as_ref().unwrap().capture(&works)?;
                    assert!(
                        work.lookup(&key)?.is_some(),
                        "receipt/display Work body must survive"
                    );
                    assert_eq!(work.nonopen.keys.len(), if receipt_only { 0 } else { 128 });
                    let inputs = prepared.inputs.capture()?;
                    assert!(inputs
                        .lookup(&InputEvidenceKey::new(Family::Message, RECOVERY_ID))
                        .is_err());
                    assert!(!inputs.execution.sources.contains_key(SOURCE_ID));
                }
            }
            Ok(())
        })
    })
}

#[test]
fn original_input_recovery_keeps_requested_provider_sources_strict() -> Result<()> {
    let (_directory, db) = legacy_completed_work_fixture(1)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let material = ResourceReadMaterial::new(&ledger, &scope())?;
    material.read(|| {
        db.transaction(|tx| {
            let state = AgentState::new("default");
            let mut inputs = request();
            inputs
                .mandatory_roots
                .insert(InputEvidenceKey::new(Family::Message, RECOVERY_ID));
            let works = WorkCaptureRequest::current();
            let tasks = TaskCaptureRequest::current();
            for with_queries in [false, true] {
                let error = if with_queries {
                    prepare_original_input_recovery_with_queries_tx(
                        tx,
                        &scope(),
                        &state,
                        &inputs,
                        16,
                        &ledger,
                        &works,
                        &tasks,
                        None,
                    )
                } else {
                    prepare_original_input_recovery_with_work_tx(
                        tx,
                        &scope(),
                        &state,
                        &inputs,
                        16,
                        &ledger,
                        &works,
                        &tasks,
                    )
                }
                .err()
                .context("an explicitly required recovery cannot discard its provider source")?;
                assert!(matches!(error.downcast_ref::<InputEvidenceReadError>(),
                Some(InputEvidenceReadError::NeedsPreparation(key))
                if key == &InputEvidenceKey::new(Family::Message, SOURCE_ID)));
            }
            Ok(())
        })
    })
}
