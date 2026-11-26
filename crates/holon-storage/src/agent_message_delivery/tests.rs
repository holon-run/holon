use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use holon_domain::{
    AdmissionContext, AgentMessageAdmissionEvidence, AgentMessageCallerContext,
    AgentMessageDeliveryOutcome, AgentMessagePrincipalKind, AuthorityClass, MessageDeliverySurface,
    MessageOrigin,
};
use rusqlite::{ErrorCode, TransactionBehavior};

use super::*;
use crate::migrations::MIGRATIONS;

fn install_ledger(connection: &Connection) -> Result<()> {
    for name in [
        "agent_message_delivery_ledger",
        "agent_message_chain_budgets",
    ] {
        connection.execute_batch(
            MIGRATIONS
                .iter()
                .find(|migration| migration.name == name)
                .unwrap()
                .sql,
        )?;
    }
    Ok(())
}

fn database() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    install_ledger(&connection)?;
    Ok(connection)
}

fn accepted(id: &str) -> AgentMessageDeliveryRecord {
    let now = "2026-11-26T00:00:00Z".parse().unwrap();
    AgentMessageDeliveryRecord {
        delivery_id: id.into(),
        target_agent_id: "recipient".into(),
        message_chain_id: None,
        expected_incarnation: None,
        message_id: Some(format!("message-{id}")),
        activation_id: None,
        turn_id: None,
        correlation_id: None,
        causation_id: None,
        routing_context: None,
        idempotency_scope: "sender-to-recipient".into(),
        idempotency_key_digest: format!("key-{id}"),
        request_digest: format!("request-{id}"),
        caller: AgentMessageCallerContext {
            caller_principal: "agent:sender".into(),
            caller_agent_id: Some("sender".into()),
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            route: "agent_message".into(),
            origin: MessageOrigin::Task {
                task_id: "task".into(),
            },
            authority_class: AuthorityClass::ExternalEvidence,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: None,
            current_task_id: None,
            current_work_item_id: None,
            inherited_routing_context: None,
            routing_activation_id: None,
        },
        message_authority_version: None,
        outcome: AgentMessageDeliveryOutcome::Accepted,
        state: AgentMessageDeliveryState::Queued,
        state_version: 1,
        admission_evidence: AgentMessageAdmissionEvidence {
            identity_revision: None,
            identity_status: None,
            runtime_status: None,
            message_policy_revision: None,
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            principal_id: Some("agent:sender".into()),
            route: "agent_message".into(),
            matched_rule_index: None,
            derived_grant: None,
        },
        rejection_code: None,
        retryable: false,
        diagnostic: None,
        accepted_at: Some(now),
        terminal_at: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn idempotency_key_is_unique_within_its_scope() -> Result<()> {
    let mut connection = database()?;
    let tx = connection.transaction()?;
    let original = accepted("original");
    insert_delivery_tx(&tx, &original)?;
    let mut conflict = accepted("conflict");
    conflict
        .idempotency_key_digest
        .clone_from(&original.idempotency_key_digest);

    let error = insert_delivery_tx(&tx, &conflict).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(ErrorCode::ConstraintViolation)
    );
    conflict.idempotency_scope = "different-scope".into();
    insert_delivery_tx(&tx, &conflict)?;
    assert_eq!(
        delivery_by_idempotency_scope_tx(
            &tx,
            &original.idempotency_scope,
            &original.idempotency_key_digest
        )?,
        Some(original)
    );
    assert_eq!(
        delivery_by_idempotency_scope_tx(
            &tx,
            &conflict.idempotency_scope,
            &conflict.idempotency_key_digest
        )?,
        Some(conflict)
    );
    tx.commit()?;
    Ok(())
}

#[test]
fn compare_and_set_checks_both_state_and_version() -> Result<()> {
    use AgentMessageDeliveryState::{Dispatched, Failed, Queued};

    let mut connection = database()?;
    let tx = connection.transaction()?;
    let original = accepted("source");
    insert_delivery_tx(&tx, &original)?;
    let mut next = original.clone();
    assert!(next.advance_state(Dispatched, next.created_at, None));
    assert!(!compare_and_set_delivery_state_tx(
        &tx,
        &original.delivery_id,
        Dispatched,
        &next
    )?);

    // An intervening write keeps the state and changes the version.
    let mut refreshed = original.clone();
    refreshed.state_version += 1;
    assert!(compare_and_set_delivery_state_tx(
        &tx,
        &original.delivery_id,
        Queued,
        &refreshed
    )?);
    assert!(!compare_and_set_delivery_state_tx(
        &tx,
        &original.delivery_id,
        Queued,
        &next
    )?);
    let mut expected = refreshed;
    assert!(expected.advance_state(Failed, expected.created_at, Some("delivery failed")));
    assert!(compare_and_set_delivery_state_tx(
        &tx,
        &original.delivery_id,
        Queued,
        &expected
    )?);
    assert_eq!(delivery_by_id(&tx, &original.delivery_id)?, Some(expected));
    tx.commit()?;
    Ok(())
}

#[test]
fn failed_delivery_diagnostic_is_bounded() -> Result<()> {
    use AgentMessageDeliveryState::{Dispatched, Failed};

    let mut connection = database()?;
    let tx = connection.transaction()?;
    let record = accepted("source");
    insert_delivery_tx(&tx, &record)?;
    let diagnostic = "界".repeat(612);
    let message_id = record.message_id.as_deref().unwrap();
    assert!(advance_delivery_state_for_message_tx(
        &tx,
        message_id,
        Failed,
        Some(&diagnostic)
    )?);
    assert!(!advance_delivery_state_for_message_tx(
        &tx, message_id, Dispatched, None
    )?);
    let failed = delivery_by_message_id_on(&tx, message_id)?.unwrap();
    assert_eq!(failed.state, Failed);
    assert_eq!(failed.state_version, 2);
    assert_eq!(failed.terminal_at, Some(failed.updated_at));
    assert_eq!(failed.diagnostic.unwrap(), "界".repeat(512));
    Ok(())
}

#[test]
fn accepted_scan_stops_after_first_match_in_rowid_order() -> Result<()> {
    let mut connection = database()?;
    let tx = connection.transaction()?;
    let first = accepted("first");
    let second = accepted("second");
    let later = accepted("later");
    for delivery in [&first, &second, &later] {
        insert_delivery_tx(&tx, delivery)?;
    }
    let boundary = delivery_rowid_on(&tx, &first.delivery_id)?;
    tx.execute(
        "UPDATE agent_message_deliveries SET payload_json = 'broken' WHERE delivery_id = ?1",
        [&later.delivery_id],
    )?;
    assert_eq!(
        first_accepted_delivery_after_on(&tx, &first.target_agent_id, boundary, |_| true)?,
        Some(second)
    );
    Ok(())
}

#[test]
fn chain_compatibility_anchor_costs_one_delivery_and_rolls_back() -> Result<()> {
    let mut connection = database()?;
    let tx = connection.transaction()?;
    let legacy = accepted("legacy");
    insert_delivery_tx(&tx, &legacy)?;
    tx.commit()?;

    let tx = connection.transaction()?;
    for _ in 1..MESSAGE_CHAIN_MAX_DELIVERIES {
        assert_eq!(exhausted_chain_limit_tx(&tx, &legacy.delivery_id)?, None);
        charge_chain_budget_tx(&tx, &legacy.delivery_id)?;
    }
    assert_eq!(
        exhausted_chain_limit_tx(&tx, &legacy.delivery_id)?,
        Some(MESSAGE_CHAIN_MAX_DELIVERIES)
    );
    tx.rollback()?;
    let tx = connection.transaction()?;
    assert!(!chain_budget_exists_tx(&tx, &legacy.delivery_id)?);
    assert_eq!(delivery_by_id(&tx, &legacy.delivery_id)?, Some(legacy));
    Ok(())
}

#[test]
fn concurrent_writers_share_the_final_chain_allowance() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("ledger.sqlite");
    let mut connection = Connection::open(&path)?;
    install_ledger(&connection)?;
    let chain = "chain";
    let tx = connection.transaction()?;
    for _ in 1..MESSAGE_CHAIN_MAX_DELIVERIES {
        charge_chain_budget_tx(&tx, chain)?;
    }
    tx.commit()?;

    let start = Arc::new(Barrier::new(2));
    let writers: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|id| {
            let start = Arc::clone(&start);
            let path = path.clone();
            thread::spawn(move || -> Result<bool> {
                // Meet before opening the write transactions so SQLite serializes the checks.
                start.wait();
                let mut connection = Connection::open(path)?;
                connection.busy_timeout(Duration::from_secs(5))?;
                let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if exhausted_chain_limit_tx(&tx, chain)?.is_some() {
                    return Ok(false);
                }
                let mut delivery = accepted(id);
                delivery.message_chain_id = Some(chain.into());
                insert_delivery_tx(&tx, &delivery)?;
                charge_chain_budget_tx(&tx, chain)?;
                tx.commit()?;
                Ok(true)
            })
        })
        .collect();
    let winners = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(winners.into_iter().filter(|won| *won).count(), 1);
    let tx = connection.transaction()?;
    assert_eq!(
        exhausted_chain_limit_tx(&tx, chain)?,
        Some(MESSAGE_CHAIN_MAX_DELIVERIES)
    );
    assert_eq!(active_deliveries_for_target_tx(&tx, "recipient")?.len(), 1);
    Ok(())
}
