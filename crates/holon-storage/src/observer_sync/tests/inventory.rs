use super::*;

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

#[test]
fn inventory_preserves_grouping_classification_and_diagnostic_text() -> Result<()> {
    use holon_domain::runtime_event::UnsupportedProjectionEvent;
    assert!(!SYNC.verify_event_projection_effect_inventory(&Connection::open_in_memory()?)?);
    let connection = unsupported()?;
    assert!(!SYNC.verify_event_projection_effect_inventory(&connection)?);
    let a = SYNC.inventory_diagnostic(format_args!(
        "a_未知@schema\"路v-1/cv2/reason={:?}/count=2",
        UnsupportedProjectionEvent::InvalidMetadata,
    ))?;
    let b = SYNC.inventory_diagnostic(format_args!(
        "z_future@typed.zv1/cv2/reason={:?}/count=1",
        UnsupportedProjectionEvent::UnknownTypedKind,
    ))?;
    assert_eq!([a.clone(), b.clone()], diagnostics());
    assert_eq!(
        InventoryDiagnostics(&[a, b]).to_string(),
        diagnostics().join(", ")
    );
    assert_eq!(InventoryDiagnostics(&[]).to_string(), "");
    assert_eq!(
        InventoryDiagnostics(&diagnostics()[..1]).to_string(),
        diagnostics()[0],
    );
    let descriptor = holon_domain::runtime_event::ALL_RUNTIME_EVENT_KINDS[0].descriptor();
    for (kind, schema, version, contract, reason) in [
        (
            descriptor.wire_name,
            "wrong",
            1i64,
            2i64,
            UnsupportedProjectionEvent::PayloadSchemaMismatch,
        ),
        (
            descriptor.wire_name,
            descriptor.payload_schema,
            i64::from(descriptor.payload_schema_version) + 1,
            2,
            UnsupportedProjectionEvent::FuturePayloadSchemaVersion,
        ),
        (
            "unknown",
            "typed",
            i64::MIN,
            i64::MAX,
            UnsupportedProjectionEvent::InvalidMetadata,
        ),
    ] {
        connection.execute("DELETE FROM audit_events", [])?;
        insert(
            &connection,
            kind,
            &serde_json::json!({
                "payload_schema": schema,
                "payload_schema_version": version,
                "contract_version": contract,
            })
            .to_string(),
        )?;
        assert!(!SYNC.verify_event_projection_effect_inventory(&connection)?);
        let expected = format!("{kind}@{schema}v{version}/cv{contract}/reason={reason:?}/count=1");
        assert_eq!(
            SYNC.inventory_diagnostic(format_args!(
                "{kind}@{schema}v{version}/cv{contract}/reason={reason:?}/count=1"
            ))?,
            expected,
        );
    }
    assert!(connection.is_autocommit());
    Ok(())
}
