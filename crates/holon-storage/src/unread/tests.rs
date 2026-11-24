use super::*;
use crate::tests::{migrated_connection, seed_runtime_metadata};
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    Connection, TransactionBehavior,
};
use std::sync::{Arc, Mutex};

#[test]
fn observer_scope_executes_one_metadata_statement() -> Result<()> {
    thread_local! {
        static STATEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    fn count_statement(_: &str) {
        STATEMENTS.with(|count| count.set(count.get() + 1));
    }
    let mut connection = migrated_connection()?;
    seed_runtime_metadata(&connection)?;
    connection.trace(Some(count_statement));
    BriefReadScope::Observer("control").resolve(&connection, "user-a")?;
    assert_eq!(STATEMENTS.with(std::cell::Cell::get), 1);
    Ok(())
}

#[test]
fn observer_scope_reads_only_authority_metadata_from_one_committed_view() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("runtime.sqlite");
    let mut connection = Connection::open(&database)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    crate::migrations::migrate(&mut connection, |_, _| Ok(()))?;
    seed_runtime_metadata(&connection)?;
    let writer = Connection::open(&database)?;
    let reads = Arc::new(Mutex::new(Vec::new()));
    let observed = reads.clone();
    connection.authorizer(Some(move |context: AuthContext<'_>| match context.action {
        AuthAction::Read {
            table_name,
            column_name,
        } => {
            observed
                .lock()
                .unwrap()
                .push((table_name.to_string(), column_name.to_string()));
            if table_name == "runtime_metadata" {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        _ => Authorization::Allow,
    }));
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let original = BriefReadScope::Observer("control").resolve(&tx, "user-a")?;
    assert_eq!(
        original,
        holon_domain::ids::visibility_scope_id("runtime-test", "user-a", "control", 0)
    );
    writer.execute(
        "UPDATE runtime_metadata SET value = '1' WHERE key = 'visibility_policy_generation'",
        [],
    )?;
    // A policy change committed elsewhere cannot split this operation's view.
    assert_eq!(
        original,
        BriefReadScope::Observer("control").resolve(&tx, "user-a")?
    );
    tx.commit()?;
    assert_ne!(
        original,
        BriefReadScope::Observer("control").resolve(&connection, "user-a")?
    );
    let reads = reads.lock().unwrap();
    assert!(!reads.is_empty());
    assert!(reads
        .iter()
        .all(|(table, column)| table == "runtime_metadata"
            && ["key", "value"].contains(&column.as_str())));
    Ok(())
}
