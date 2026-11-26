use holon_domain::input_mutation_receipt::{canonical_result_digest, InputMutationFamily};

use super::*;
use crate::schema::INPUT_MUTATION_RECEIPT_SCHEMA;

const COMMITTED_AT: &str = "2026-11-27T00:00:01.123456789Z";
const OUTCOME: &str = r#"{ "deferred": [], "applied": false }"#;

fn database() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(INPUT_MUTATION_RECEIPT_SCHEMA)?;
    Ok(connection)
}

fn prepared(family: InputMutationFamily, incarnation: u64) -> Result<PreparedInputMutation> {
    PreparedInputMutation::new(
        family,
        "original-nonce".into(),
        vec![InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: incarnation,
        }],
        "a".repeat(64),
        "2026-11-27T00:00:00.012345678Z".parse()?,
    )
}

#[test]
fn receipt_preserves_outcome_bytes_and_rolls_back_with_the_caller() -> Result<()> {
    let mut connection = database()?;
    let prepared = prepared(InputMutationFamily::Queue, 1)?;
    let committed_at = COMMITTED_AT.parse()?;
    let outcome: serde_json::Value = serde_json::from_str(OUTCOME)?;
    let digest = canonical_result_digest(&prepared, &outcome, committed_at)?;
    let tx = connection.transaction()?;

    insert_receipt_tx(&tx, &prepared, OUTCOME, committed_at, &digest)?;
    let stored = lookup_receipt_on(&tx, &prepared)?.unwrap();
    assert_eq!(stored.prepared, prepared);
    assert_eq!(stored.outcome_json, OUTCOME);
    assert_eq!(stored.committed_at, COMMITTED_AT);
    assert_eq!(stored.result_digest, digest);

    let replacement_digest = "b".repeat(64);
    assert!(insert_receipt_tx(&tx, &prepared, "{}", committed_at, &replacement_digest).is_err());
    assert_eq!(
        lookup_receipt_on(&tx, &prepared)?.unwrap().outcome_json,
        OUTCOME
    );
    tx.rollback()?;
    assert!(lookup_receipt_on(&connection, &prepared)?.is_none());
    Ok(())
}

#[test]
fn nonce_lookup_validates_projections_then_scope_then_command() -> Result<()> {
    let mut connection = database()?;
    let original = prepared(InputMutationFamily::Queue, 1)?;
    let wrong_scope = prepared(InputMutationFamily::Terminal, 2)?;
    let wrong_command = prepared(InputMutationFamily::Terminal, 1)?;
    let tx = connection.transaction()?;
    insert_receipt_tx(
        &tx,
        &original,
        OUTCOME,
        COMMITTED_AT.parse()?,
        &"b".repeat(64),
    )?;
    let conflict = lookup_receipt_on(&tx, &wrong_scope).err().unwrap();
    assert!(matches!(
        conflict.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::GroupConflict)
    ));
    let conflict = lookup_receipt_on(&tx, &wrong_command).err().unwrap();
    assert!(matches!(
        conflict.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::CommandConflict)
    ));

    tx.execute(
        "UPDATE input_mutation_receipts SET scope_digest = ?1 WHERE command_nonce = ?2",
        params!["c".repeat(64), original.command_nonce()],
    )?;
    let corrupt = lookup_receipt_on(&tx, &wrong_scope).err().unwrap();
    assert!(matches!(
        corrupt.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::CorruptReceipt(
            "normalized descriptor differs from payload"
        ))
    ));
    Ok(())
}
