use super::*;
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger,
    resident_input_evidence::{OriginalInputQuerySource, ResidentInputEvidence},
};
use crate::runtime_db::{
    input_evidence_snapshot::{
        prepare_input_evidence_snapshot_tx, InputEvidenceBootstrapSnapshot,
        InputEvidenceLoadRequest,
    },
    task_snapshot::TaskDomainLoadRequest,
    work_snapshot::WorkDomainLoadRequest,
};
use std::collections::BTreeMap;

fn prepared_query(
    f: &Fixture,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(ResidentInputEvidence, InputEvidenceLoadRequest)> {
    let request = native_query_request(ledger, Some(0));
    let InputEvidenceBootstrapSnapshot::Current(snapshot) =
        f.db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &f.scope, &request))?
    else {
        anyhow::bail!("fixture requires a current scope");
    };
    Ok((snapshot.resident.with_budget(ledger.clone())?, request))
}

fn commit(
    f: &Fixture,
    prepared: &PreparedInputEvidence,
    ledger: &InputEvidenceBudgetLedger,
    source: &OriginalInputQuerySource,
) -> Result<InputMutationExecution<InputEvidenceCommit<PreparedEvidenceValue>>> {
    f.db.evidence()
        .commit_prepared_input_evidence_with_task_budget(
            prepared,
            &WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 65_536,
                byte_limit: 64 * 1024 * 1024,
            },
            &TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: 65_536,
                byte_limit: 64 * 1024 * 1024,
            },
            ledger,
            Some(source),
            None,
        )
}

#[test]
fn original_query_publication_advances_complete_frontier_and_preserves_old_capture() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (original, request) = prepared_query(&f, &ledger)?;
    let empty = original.capture_for_request(&request)?;
    let source = original
        .original_query_source()?
        .context("original source")?;
    let record = f.transcript();
    let key = InputEvidenceKey::new(Family::Transcript, &record.id);
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "query-publication".into(),
        f.at,
        record.clone(),
    )?;
    let InputMutationExecution::Committed { value, receipt } =
        commit(&f, &prepared, &ledger, &source)?
    else {
        anyhow::bail!("original publication must commit");
    };
    let weak = value
        .input_delta
        .query_advance
        .as_ref()
        .unwrap()
        .weak_guard_for_fixture();
    let next = original
        .with_committed(&value.input_delta, Default::default())?
        .context("published Input query")?;
    let captured = next.capture_for_request(&request)?;
    let query = captured.query.as_ref().context("published query")?;
    assert_eq!(query.birth_after, Some(0));
    assert_eq!(query.birth_cursor, 1);
    assert_eq!(query.born, BTreeMap::from([(1, key.clone())]));
    assert_eq!(captured.records.len(), 1);
    assert!(captured.lookup(&key)?.is_some());
    assert_eq!(empty.query.as_ref().unwrap().birth_cursor, 0);
    assert!(empty.records.is_empty());
    // Exact replay precedes source validation and does not repeat the birth.
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = commit(&f, &prepared, &ledger, &source)?
    else {
        anyhow::bail!("exact receipt must replay");
    };
    assert_eq!(receipt, replay);
    let next_source = next.original_query_source()?.context("next source")?;
    let mut second_record = record.clone();
    second_record.id.push_str("-second");
    let second_key = InputEvidenceKey::new(Family::Transcript, &second_record.id);
    let second = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "query-publication-second".into(),
        f.at,
        second_record,
    )?;
    let InputMutationExecution::Committed { value: second, .. } =
        commit(&f, &second, &ledger, &next_source)?
    else {
        anyhow::bail!("second birth must commit");
    };
    let next2 = next
        .with_committed(&second.input_delta, Default::default())?
        .context("second query publication")?;
    let captured2 = next2.capture_for_request(&request)?;
    let query2 = captured2.query.as_ref().context("second complete query")?;
    assert_eq!(query2.birth_cursor, 2);
    assert_eq!(
        query2.born,
        BTreeMap::from([(1, key.clone()), (2, second_key)])
    );
    assert_eq!(
        captured2.records.len(),
        2,
        "old query-only body remains rooted"
    );
    assert!(captured2.lookup(&key)?.is_some());
    assert_eq!(query.birth_cursor, 1, "old capture keeps its own cut");
    let source2 = next2.original_query_source()?.context("second source")?;
    let noop = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "query-publication-noop".into(),
        f.at,
        record,
    )?;
    let InputMutationExecution::Committed {
        value: noop,
        receipt,
    } = commit(&f, &noop, &ledger, &source2)?
    else {
        anyhow::bail!("new descriptor preserves original successful no-op");
    };
    assert!(!receipt.outcome.applied);
    let after_noop = next2
        .with_committed(&noop.input_delta, Default::default())?
        .context("no-op query publication")?;
    assert_eq!(
        after_noop.capture_for_request(&request)?.query.as_ref(),
        Some(query2)
    );
    drop((
        original,
        next,
        after_noop,
        source,
        next_source,
        next2,
        captured2,
        source2,
        second,
        empty,
        value,
        noop,
        request,
    ));
    assert!(
        weak.upgrade().is_some(),
        "selected capture owns original writer material"
    );
    assert!(ledger.report()?.observed_bytes > 0);
    drop(f);
    assert!(captured.lookup(&key)?.is_some());
    drop(captured);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_query_publication_rejects_foreign_ledger_stale_source_and_equal_other_base(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let (original, request) = prepared_query(&f, &ledger)?;
    let (equal_other_base, _) = prepared_query(&f, &ledger)?;
    let source = original
        .original_query_source()?
        .context("original source")?;
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "query-source-fences".into(),
        f.at,
        f.transcript(),
    )?;
    let error = commit(&f, &prepared, &foreign, &source).expect_err("foreign ledger");
    assert!(
        format!("{error:#}").contains("original scope/ledger"),
        "{error:#}"
    );
    assert_eq!(f.count("transcript_entries")?, 0);
    assert_eq!(f.count("input_mutation_receipts")?, 0);
    assert_eq!(foreign.report()?.observed_bytes, 0);
    let InputMutationExecution::Committed { value, .. } = commit(&f, &prepared, &ledger, &source)?
    else {
        anyhow::bail!("first original source");
    };
    let error = equal_other_base
        .with_committed(&value.input_delta, Default::default())
        .expect_err("equal metadata does not substitute another accepted base");
    assert!(
        format!("{error:#}").contains("original accepted baseline"),
        "{error:#}"
    );
    let mut record = f.transcript();
    record.id.push_str("-stale");
    let stale = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "query-source-stale".into(),
        f.at,
        record,
    )?;
    let error = commit(&f, &stale, &ledger, &source).expect_err("stale frontier");
    assert!(
        format!("{error:#}").contains("original birth frontier"),
        "{error:#}"
    );
    assert_eq!(f.count("transcript_entries")?, 1);
    assert_eq!(f.count("input_mutation_receipts")?, 1);
    assert_eq!(f.count("input_query_birth_members")?, 1);
    assert_eq!(
        original
            .capture_for_request(&request)?
            .query
            .unwrap()
            .birth_cursor,
        0
    );
    drop((original, equal_other_base, source, value, request));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_query_publication_receipt_fault_rolls_back_and_preserves_prepared_baseline(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (original, request) = prepared_query(&f, &ledger)?;
    let source = original
        .original_query_source()?
        .context("original source")?;
    let before = ledger.report()?.observed_bytes;
    f.db.connection()?.execute_batch(
        "CREATE TRIGGER reject_query_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'query receipt fault'); END;",
    )?;
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "query-publication-fault".into(),
        f.at,
        f.transcript(),
    )?;
    let error = commit(&f, &prepared, &ledger, &source).expect_err("original receipt fault");
    assert!(
        format!("{error:#}").contains("query receipt fault"),
        "{error:#}"
    );
    for table in [
        "transcript_entries",
        "input_evidence_current",
        "input_query_birth_members",
        "input_mutation_receipts",
    ] {
        assert_eq!(f.count(table)?, 0, "{table}");
    }
    assert_eq!(ledger.report()?.observed_bytes, before);
    assert_eq!(
        original
            .capture_for_request(&request)?
            .query
            .unwrap()
            .birth_cursor,
        0
    );
    drop((original, source, request));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
