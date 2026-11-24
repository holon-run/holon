use super::*;
use crate::tests::migrated_connection;

#[test]
fn checkpoint_replacement_respects_the_caller_transaction() -> Result<()> {
    let mut connection = migrated_connection()?;
    let tx = connection.transaction()?;
    let domain = "test-domain";
    let checkpoint = serde_json::json!({"version": 1});
    upsert_storage_domain(&tx, domain, "complete", "db", Some(checkpoint.clone()))?;
    tx.commit()?;

    let committed = read_storage_domain_connection(&connection, domain)?.unwrap();
    assert_eq!(committed.schema_version, max_known_migration_version());
    assert_eq!(
        committed.source_checkpoint_json,
        Some(checkpoint.to_string())
    );
    assert_eq!(
        committed.imported_at.as_deref(),
        Some(committed.updated_at.as_str())
    );

    assert!(list_storage_domains(&connection)?.contains(&committed));

    let tx = connection.transaction()?;
    upsert_storage_domain_checkpoint_json(&tx, domain, "pending", "legacy", None)?;
    let pending = read_storage_domain_connection(&tx, domain)?.unwrap();
    assert!(pending.imported_at.is_none());
    assert!(pending.source_checkpoint_json.is_none());
    tx.rollback()?;
    assert_eq!(
        read_storage_domain_connection(&connection, domain)?,
        Some(committed)
    );
    Ok(())
}
