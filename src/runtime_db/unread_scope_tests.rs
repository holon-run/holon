use super::*;
use crate::{
    runtime_db::RuntimeDb,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        BriefKind, BriefRecord,
    },
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::{Arc, Mutex};

#[test]
fn observer_scope_executes_one_metadata_statement() -> Result<()> {
    thread_local! {
        static STATEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    fn count_statement(_: &str) {
        STATEMENTS.with(|count| count.set(count.get() + 1));
    }
    let (_root, db) = fixture()?;
    let mut connection = db.connection()?;
    connection.trace(Some(count_statement));
    ReadScope::Observer("control").resolve(&connection, "user-a")?;
    assert_eq!(STATEMENTS.with(std::cell::Cell::get), 1);
    Ok(())
}

fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let root = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        root.path().join("runtime.sqlite"),
        root.path().join("runtime.lock"),
    )?;
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        "agent-a",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))?;
    Ok((root, db))
}

fn append(db: &RuntimeDb, text: &str) -> Result<u64> {
    let brief = BriefRecord::new("agent-a", BriefKind::Result, text, None, None);
    let event = crate::types::brief_created_event_for(&brief)?;
    Ok(db
        .evidence()
        .append_brief_with_created_event(Some("agent-a"), &brief, &event, &[])?
        .event
        .event_seq)
}

#[test]
fn observer_scope_reads_only_authority_metadata_from_one_committed_view() -> Result<()> {
    let (_root, db) = fixture()?;
    let mut connection = db.connection()?;
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
    let original = ReadScope::Observer("control").resolve(&tx, "user-a")?;
    assert_eq!(
        original,
        crate::ids::visibility_scope_id(&db.runtime_id()?, "user-a", "control", 0)
    );
    db.transaction(|writer| {
        writer.execute(
            "UPDATE runtime_metadata SET value = '1' WHERE key = 'visibility_policy_generation'",
            [],
        )?;
        Ok(())
    })?;
    // A policy change committed elsewhere cannot split this operation's view.
    assert_eq!(
        original,
        ReadScope::Observer("control").resolve(&tx, "user-a")?
    );
    tx.commit()?;
    assert_ne!(
        original,
        ReadScope::Observer("control").resolve(&connection, "user-a")?
    );
    let reads = reads.lock().unwrap();
    assert!(!reads.is_empty());
    assert!(reads
        .iter()
        .all(|(table, column)| table == "runtime_metadata"
            && ["key", "value"].contains(&column.as_str())));
    Ok(())
}

#[test]
fn observer_brief_scope_isolates_principal_entitlement_runtime_and_policy() -> Result<()> {
    let (_root, db) = fixture()?;
    let initial = db
        .observer_brief_read_state("user-a", "control", "agent-a")?
        .unwrap();
    let first = append(&db, "one")?;
    let second = append(&db, "two")?;
    let unread = db.observer_brief_read_states("user-a", "control")?;
    assert_eq!(unread[0].unread_count, 2);
    assert_eq!(unread[0].visibility_scope_id, initial.visibility_scope_id);
    let marked = db
        .observer_mark_brief_read("user-a", "control", "agent-a", first)?
        .unwrap();
    assert_eq!(marked.state.unread_count, 1);
    let stale = db
        .observer_mark_brief_read("user-a", "control", "agent-a", 0)?
        .unwrap();
    assert_eq!(stale, marked);
    for (principal, entitlement) in [("user-b", "control"), ("user-a", "anonymous")] {
        let state = db
            .observer_brief_read_states(principal, entitlement)?
            .remove(0);
        assert_ne!(state.visibility_scope_id, initial.visibility_scope_id);
        assert_eq!(state.read_through_event_seq, second);
        assert_eq!(state.unread_count, 0);
    }
    for (key, value) in [
        ("visibility_policy_generation", "1"),
        ("runtime_id", "replacement-runtime"),
    ] {
        let old_scope = db
            .observer_brief_read_states("user-a", "control")?
            .remove(0)
            .visibility_scope_id;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE runtime_metadata SET value = ?2 WHERE key = ?1",
                params![key, value],
            )?;
            Ok(())
        })?;
        let state = db
            .observer_brief_read_states("user-a", "control")?
            .remove(0);
        assert_ne!(state.visibility_scope_id, old_scope);
        assert_eq!(state.read_through_event_seq, second);
        assert_eq!(state.unread_count, 0);
    }
    // Only the original single read initialized a cursor. Batch defaults stay read-only.
    let count: i64 =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM agent_brief_read_cursors", [], |row| {
                row.get(0)
            })?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn observer_batch_remains_read_only_while_writer_is_locked() -> Result<()> {
    let (_root, db) = fixture()?;
    let mut lock = db.connection()?;
    let _writer = lock.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state = db
        .observer_brief_read_states("user-a", "control")?
        .remove(0);
    assert_eq!(state.read_through_event_seq, state.event_head_seq);
    assert_eq!(state.revision, 0);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM agent_brief_read_cursors", [], |row| {
                row.get::<_, i64>(0)
            })?,
        0
    );
    Ok(())
}

#[test]
fn observer_reads_preserve_membership_and_mark_read_atomicity() -> Result<()> {
    let (_root, db) = fixture()?;
    db.observer_brief_read_state("user-a", "control", "agent-a")?;
    let head = append(&db, "one")?;
    for (status, visibility) in [("active", "private_child"), ("deleted", "public")] {
        db.transaction(|tx| { tx.execute("UPDATE agent_identities SET status = ?1, visibility = ?2 WHERE agent_id = 'agent-a'", params![status, visibility])?; Ok(()) })?;
        assert!(db
            .observer_brief_read_states("user-a", "control")?
            .is_empty());
        assert!(db
            .observer_brief_read_state("user-a", "control", "agent-a")?
            .is_none());
        assert!(db
            .observer_mark_brief_read("user-a", "control", "agent-a", head)?
            .is_none());
    }
    db.transaction(|tx| {
        tx.execute("UPDATE agent_identities SET status = 'active', visibility = 'public' WHERE agent_id = 'agent-a'", [])?;
        tx.execute_batch("CREATE TRIGGER reject_cursor_update BEFORE UPDATE ON agent_brief_read_cursors BEGIN SELECT RAISE(ABORT, 'fixture rollback'); END;")?;
        Ok(())
    })?;
    assert!(db
        .observer_mark_brief_read("user-a", "control", "agent-a", head)
        .is_err());
    let state = db
        .observer_brief_read_states("user-a", "control")?
        .remove(0);
    assert_eq!(state.read_through_event_seq, 0);
    assert_eq!(state.unread_count, 1);
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_cursor_update")?;
        Ok(())
    })?;
    let marked = db
        .observer_mark_brief_read("user-a", "control", "agent-a", u64::MAX)?
        .unwrap();
    assert_eq!(marked.applied_read_through_event_seq, head);
    assert_eq!(marked.state.unread_count, 0);
    Ok(())
}

#[test]
fn observer_brief_reads_preserve_retention_gap_and_epoch_reset() -> Result<()> {
    let (_root, db) = fixture()?;
    db.observer_brief_read_state("user-a", "control", "agent-a")?;
    append(&db, "one")?;
    let oldest = append(&db, "two")?;
    let head = append(&db, "three")?;
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO audit_event_retention_watermarks (scope_key, oldest_retained_seq) VALUES (?1, ?2)",
            params![audit_event_sequence_scope(Some("agent-a")), oldest],
        )?;
        tx.execute("DELETE FROM briefs WHERE agent_id = 'agent-a' AND created_event_seq < ?1", [oldest])?;
        Ok(())
    })?;
    let gap = db
        .observer_brief_read_states("user-a", "control")?
        .remove(0);
    assert!(gap.retention_gap && gap.reset_required);
    assert_eq!(gap.unread_count, 2);
    let stale = db
        .observer_mark_brief_read("user-a", "control", "agent-a", 0)?
        .unwrap();
    assert_eq!(stale.state, gap);
    let marked = db
        .observer_mark_brief_read("user-a", "control", "agent-a", u64::MAX)?
        .unwrap();
    assert_eq!(marked.applied_read_through_event_seq, head);
    assert!(!marked.state.retention_gap && !marked.state.reset_required);
    assert_eq!(marked.state.unread_count, 0);
    db.transaction(|tx| {
        tx.execute(
            "UPDATE runtime_metadata SET value = 'replacement-epoch' WHERE key = 'event_log_epoch'",
            [],
        )?;
        Ok(())
    })?;
    let reset = db
        .observer_brief_read_states("user-a", "control")?
        .remove(0);
    assert_ne!(reset.event_log_epoch, gap.event_log_epoch);
    assert_eq!(reset.read_through_event_seq, head);
    assert_eq!(reset.revision, 0);
    assert_eq!(reset.unread_count, 0);
    Ok(())
}

#[test]
fn observer_scope_fails_closed_on_invalid_or_missing_authority_metadata() -> Result<()> {
    for (key, value) in [
        ("runtime_id", ""),
        ("visibility_policy_generation", "invalid"),
    ] {
        let (_root, db) = fixture()?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE runtime_metadata SET value = ?2 WHERE key = ?1",
                params![key, value],
            )?;
            Ok(())
        })?;
        assert!(db.observer_brief_read_states("user-a", "control").is_err());
        assert!(db
            .observer_brief_read_state("user-a", "control", "agent-a")
            .is_err());
        assert!(db
            .observer_mark_brief_read("user-a", "control", "agent-a", 0)
            .is_err());
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM agent_brief_read_cursors",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
    }
    Ok(())
}
