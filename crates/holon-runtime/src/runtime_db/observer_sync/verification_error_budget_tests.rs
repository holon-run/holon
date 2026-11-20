use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::{atomic::AtomicUsize, Arc};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "verification-error-detail".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn legacy_detail(error: &anyhow::Error, source: Option<&str>) -> String {
    match source {
        Some(source) => serde_json::json!({"error": format!("{error:#}"), "source": source}),
        None => serde_json::json!({"error": format!("{error:#}")}),
    }
    .to_string()
}

fn cost(wire: &str, source: Option<&str>) -> (u64, usize) {
    // Independent formula: object braces, three tokens per string field,
    // and a separator between the two fields. String contents add no slots.
    let fields = 1 + usize::from(source.is_some());
    let slots = 2 + fields * 3 + fields - 1;
    (
        (wire.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

fn fixture() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE observer_sync_capability_verifications (
           capability TEXT PRIMARY KEY CHECK (capability != 'reject'),
           verified INTEGER, verified_at TEXT, detail TEXT
         )",
    )?;
    Ok(connection)
}

#[derive(Debug)]
struct CountedError {
    formats: Arc<AtomicUsize>,
}

impl std::fmt::Display for CountedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.formats.fetch_add(1, Ordering::Relaxed);
        f.write_str("root 路\\\"\n{}[]:,")
    }
}

impl std::error::Error for CountedError {}

fn counted() -> (anyhow::Error, Arc<AtomicUsize>) {
    let formats = Arc::new(AtomicUsize::new(0));
    let error = anyhow::Error::new(CountedError {
        formats: formats.clone(),
    })
    .context("outer context");
    (error, formats)
}

#[test]
fn streamed_error_preserves_exact_legacy_json_and_alternate_chain() -> Result<()> {
    for text in [
        "".into(),
        "路\\\"\n\r\t\u{0}{}[]:,".into(),
        "long".repeat(8192),
    ] {
        let error = anyhow::anyhow!(text).context("middle").context("outer");
        for source in [None, Some("full_inventory"), Some("source 路\\\"\n")] {
            assert_eq!(
                verification_error_detail(&error, source)?,
                legacy_detail(&error, source)
            );
        }
    }
    let (error, formats) = counted();
    let detail = verification_error_detail(&error, None)?;
    assert_eq!(formats.load(Ordering::Relaxed), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&detail)?["error"],
        "outer context: root 路\\\"\n{}[]:,"
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn error_details_share_original_allowance_tls_and_whole_last_holder() -> Result<()> {
    let error = anyhow::anyhow!("inner 路\\\"\n").context("outer");
    let wires = [
        legacy_detail(&error, None),
        legacy_detail(&error, Some("full_inventory")),
    ];
    let details = add(
        cost(&wires[0], None),
        cost(&wires[1], Some("full_inventory")),
    );
    let prior = (23 * 8, 2 * 8);
    let exact = add(add(add(header(), header()), prior), details);
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    {
        let _active = phase.enter();
        charge_owned_metadata(23, 2)?;
    }
    let transaction = phase.transaction()?;
    let connection = fixture()?;
    {
        let _outer = phase.enter();
        {
            let _active = transaction.enter();
            for (i, source) in [None, Some("full_inventory")].into_iter().enumerate() {
                let detail = verification_error_detail(&error, source)?;
                assert_eq!(detail, wires[i]);
                persist_verification(&connection, &i.to_string(), false, "now", &detail)?;
            }
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &transaction.guard()?,
            ));
        }
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &phase.guard()?,
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    charge_owned_metadata(1, 1)?;
    assert_eq!(phase.finish()?, exact);
    assert_eq!(transaction.finish()?, add(header(), details));
    let first = Arc::new(vec![phase.guard()?, transaction.guard()?]);
    let last = first.clone();
    drop((phase, transaction));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(first);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    assert_eq!(
        count_rows(
            &connection,
            "SELECT COUNT(*) FROM observer_sync_capability_verifications"
        )?,
        2
    );
    Ok(())
}

#[test]
fn each_detail_rejects_bytes_and_records_before_owning_format_and_sql_write() -> Result<()> {
    let (error, formats) = counted();
    let wires = [
        legacy_detail(&error, None),
        legacy_detail(&error, Some("full_inventory")),
    ];
    let costs = [
        cost(&wires[0], None),
        cost(&wires[1], Some("full_inventory")),
    ];
    let prior = (31 * 8, 3 * 8);
    for gate in 0..2 {
        let before = costs[..gate]
            .iter()
            .copied()
            .fold(add(header(), prior), add);
        let through = add(before, costs[gate]);
        for records in [false, true] {
            formats.store(0, Ordering::Relaxed);
            let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                through.0 - u64::from(!records),
                through.1 - usize::from(records),
            )?;
            let holder = phase.guard()?;
            let mut connection = fixture()?;
            {
                let _active = phase.enter();
                charge_owned_metadata(31, 3)?;
                let transaction = connection.transaction()?;
                let result = (|| -> Result<()> {
                    for (i, source) in [None, Some("full_inventory")].into_iter().enumerate() {
                        let detail = verification_error_detail(&error, source)?;
                        persist_verification(&transaction, &i.to_string(), false, "now", &detail)?;
                    }
                    Ok(())
                })();
                let error = result.unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                // The rejected detail only streams to the counter, never the
                // owning JSON buffer. Earlier admitted details format twice.
                assert_eq!(formats.load(Ordering::Relaxed), gate * 2 + 1);
                assert!(ResourceReadMaterial::active_guard().is_err());
                drop(transaction);
            }
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert_eq!(budget.report()?.observed_bytes, before.0);
            assert!(connection.is_autocommit());
            assert_eq!(
                count_rows(
                    &connection,
                    "SELECT COUNT(*) FROM observer_sync_capability_verifications"
                )?,
                0
            );
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn persistence_failure_rolls_back_without_detaching_detail_material() -> Result<()> {
    let error = anyhow::anyhow!("storage error").context("verification failed");
    let wire = legacy_detail(&error, None);
    let exact = add(header(), cost(&wire, None));
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    let mut connection = fixture()?;
    {
        let _active = phase.enter();
        let transaction = connection.transaction()?;
        let detail = verification_error_detail(&error, None)?;
        persist_verification(&transaction, "earlier", false, "now", &detail)?;
        assert!(persist_verification(&transaction, "reject", false, "now", &detail).is_err());
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    assert!(connection.is_autocommit());
    assert_eq!(
        count_rows(
            &connection,
            "SELECT COUNT(*) FROM observer_sync_capability_verifications"
        )?,
        0
    );
    assert_eq!(phase.finish()?, exact);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(holder);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn actual_foundation_verifier_persists_all_five_sql_error_branches_as_false() -> Result<()> {
    for (alter, capabilities) in [
        (
            "ALTER TABLE audit_events RENAME COLUMN data_json TO saved_data_json",
            vec![EVENT_PROJECTION_EFFECT_COMPLETE, BRIEF_ATOMIC_LINKAGE_VERIFIED],
        ),
        (
            "ALTER TABLE briefs RENAME COLUMN preview TO saved_preview",
            vec![ROSTER_SNAPSHOT_VERIFIED, PROJECTION_SNAPSHOT_VERIFIED],
        ),
        (
            "ALTER TABLE conversation_source_revisions RENAME COLUMN source_kind TO saved_source_kind",
            vec![CONVERSATION_READ_VERIFIED],
        ),
    ] {
        let home = tempfile::tempdir()?;
        let db = crate::runtime_db::RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let mut connection = db.connection()?;
        connection.execute(
            "UPDATE observer_sync_capability_verifications SET verification_version = 0
             WHERE capability = ?1",
            [EVENT_PROJECTION_EFFECT_COMPLETE],
        )?;
        connection.execute_batch(alter)?;
        verify_observer_sync_foundations(&mut connection)?;
        assert!(connection.is_autocommit());
        for capability in capabilities {
            let (verified, detail): (bool, String) = connection.query_row(
                "SELECT verified, detail FROM observer_sync_capability_verifications WHERE capability = ?1",
                [capability],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert!(!verified, "{capability}");
            let detail: serde_json::Value = serde_json::from_str(&detail)?;
            assert!(detail["error"].as_str().is_some_and(|error| !error.is_empty()), "{capability}: {detail}");
            if capability == EVENT_PROJECTION_EFFECT_COMPLETE {
                assert_eq!(detail["source"], "full_inventory");
                assert_eq!(detail.as_object().unwrap().len(), 2);
            } else {
                assert_eq!(detail.as_object().unwrap().len(), 1);
            }
        }
    }
    Ok(())
}
