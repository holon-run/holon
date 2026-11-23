use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::{mem::size_of, sync::Arc};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "inventory-verifier".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch("CREATE TABLE audit_events (kind, data_json)")?;
    Ok(connection)
}

fn insert(connection: &Connection, kind: &str, body: &str) -> Result<()> {
    connection.execute("INSERT INTO audit_events VALUES (?1, ?2)", [kind, body])?;
    Ok(())
}

fn unsupported() -> Result<Connection> {
    let connection = connection()?;
    for _ in 0..2 {
        insert(
            &connection,
            "a_未知",
            r#"{"payload_schema":"schema\"路","payload_schema_version":-1,"contract_version":2}"#,
        )?;
    }
    insert(
        &connection,
        "z_future",
        r#"{"payload_schema":"typed.z","payload_schema_version":1,"contract_version":2}"#,
    )?;
    Ok(connection)
}

fn diagnostics() -> [String; 2] {
    [
        "a_未知@schema\"路v-1/cv2/reason=InvalidMetadata/count=2".into(),
        "z_future@typed.zv1/cv2/reason=UnknownTypedKind/count=1".into(),
    ]
}

fn costs() -> [(u64, usize); 2] {
    // Independent formula: output String plus list cell; warning borrows both.
    let [a, b] = diagnostics();
    [
        ((a.len() + 2 * size_of::<String>()) as u64 * 8, 16),
        ((b.len() + 2 * size_of::<String>()) as u64 * 8, 16),
    ]
}

#[test]
fn supported_inventory_borrows_text_without_consuming_original_remainder() -> Result<()> {
    let connection = connection()?;
    for kind in crate::runtime_event::ALL_RUNTIME_EVENT_KINDS {
        let descriptor = kind.descriptor();
        insert(
            &connection,
            descriptor.wire_name,
            &serde_json::json!({
                "payload_schema": descriptor.payload_schema,
                "payload_schema_version": descriptor.payload_schema_version,
                "contract_version": 2,
            })
            .to_string(),
        )?;
    }
    // Missing metadata still defaults to the original legacy shape.
    insert(&connection, "unknown legacy", "{}")?;
    insert(
        &connection,
        "legacy_schema",
        &serde_json::json!({
            "payload_schema": crate::runtime_event::LEGACY_PAYLOAD_SCHEMA,
            "contract_version": 2,
        })
        .to_string(),
    )?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        assert!(SYNC_TEST.verify_event_projection_effect_inventory(&connection)?);
        assert!(SYNC_TEST.verify_event_projection_effect_inventory(&connection)?);
        assert_eq!(phase.finish()?, header());
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(connection.is_autocommit());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn inventory_uses_original_transaction_allowance_tls_and_group_last_holder() -> Result<()> {
    let connection = unsupported()?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let prior = (37 * 8, 3 * 8);
    let output = costs().into_iter().fold((0, 0), add);
    let exact = add(add(add(header(), header()), prior), output);
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    {
        let _active = phase.enter();
        charge_owned_metadata(37, 3)?;
    }
    let transaction = phase.transaction()?;
    {
        let _outer = phase.enter();
        {
            let _active = transaction.enter();
            assert!(!SYNC_TEST.verify_event_projection_effect_inventory(&connection)?);
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
    assert_eq!(transaction.finish()?, add(header(), output));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    let first = Arc::new(vec![phase.guard()?, transaction.guard()?]);
    let last = first.clone();
    drop((phase, transaction, first));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn inventory_rejects_each_output_gate_before_later_rows_and_releases_on_error() -> Result<()> {
    let costs = costs();
    for gate in 0..costs.len() {
        for records in [false, true] {
            let connection = unsupported()?;
            // BLOB kind sorts after text; its type error must not mask rejection.
            connection.execute(
                "INSERT INTO audit_events VALUES (X'ff', '{\"contract_version\":2}')",
                [],
            )?;
            let before = costs[..gate].iter().copied().fold(header(), add);
            let through = add(before, costs[gate]);
            let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                through.0 - u64::from(!records),
                through.1 - usize::from(records),
            )?;
            let holder = phase.guard()?;
            {
                let _active = phase.enter();
                let error = SYNC_TEST
                    .verify_event_projection_effect_inventory(&connection)
                    .unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(phase.finish().is_err());
            assert_eq!(budget.report()?.observed_bytes, before.0);
            assert!(connection.is_autocommit());
            assert_eq!(
                count_rows(&connection, "SELECT COUNT(*) FROM audit_events")?,
                4,
            );
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    struct FormatPasses<'a>(&'a std::cell::Cell<usize>);
    impl std::fmt::Display for FormatPasses<'_> {
        fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.set(self.0.get() + 1);
            output.write_str("probe")
        }
    }
    let passes = std::cell::Cell::new(0);
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        assert!(SYNC_TEST
            .inventory_diagnostic(format_args!("{}", FormatPasses(&passes)))
            .is_err());
    }
    // The counting pass ran; rejection prevented the owning format pass.
    assert_eq!(passes.get(), 1);
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn inventory_sql_and_text_errors_restore_original_phase_and_release_rows() -> Result<()> {
    for values in [
        "NULL, '{}'",
        "X'ff', '{}'",
        "CAST(X'ff' AS TEXT), '{}'",
        "'bad-schema', '{\"payload_schema\":2}'",
        "'bad-version', '{\"payload_schema_version\":\"two\"}'",
        "'malformed', '{'",
    ] {
        let connection = unsupported()?;
        connection.execute(&format!("INSERT INTO audit_events VALUES ({values})"), [])?;
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), 1_000_000, 10_000)?;
        let transaction = phase.transaction()?;
        {
            let _outer = phase.enter();
            {
                let _active = transaction.enter();
                let error = SYNC_TEST
                    .verify_event_projection_effect_inventory(&connection)
                    .unwrap_err();
                assert!(!format!("{error:#}").contains("ResourceExhausted"));
            }
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &phase.guard()?,
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let used = phase.finish()?;
        assert!(used.0 >= 2 * header().0);
        let last = Arc::new(vec![phase.guard()?, transaction.guard()?]);
        drop((phase, transaction));
        assert_eq!(budget.report()?.observed_bytes, used.0);
        connection.execute("DELETE FROM audit_events", [])?;
        assert!(SYNC_TEST.verify_event_projection_effect_inventory(&connection)?);
        connection.execute("DROP TABLE audit_events", [])?;
        assert!(!SYNC_TEST.verify_event_projection_effect_inventory(&connection)?);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
