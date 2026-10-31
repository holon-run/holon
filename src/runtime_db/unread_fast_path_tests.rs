//! Reader-pool fast paths for brief read cursors: the polled steady state
//! (repeat GETs and already-covered POST marks) must never enter the single
//! writer channel, while first-touch pinning and real advances keep their
//! exact previous behavior.

use super::*;
use crate::{
    runtime_db::RuntimeDb,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        BriefKind, BriefRecord,
    },
};

fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let root = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        root.path().join("runtime.sqlite"),
        root.path().join("runtime.lock"),
    )?;
    upsert_public_agent(&db, "agent-a")?;
    Ok((root, db))
}

fn upsert_public_agent(db: &RuntimeDb, agent_id: &str) -> Result<()> {
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))?;
    Ok(())
}

fn append(db: &RuntimeDb, agent_id: &str, text: &str) -> Result<u64> {
    let brief = BriefRecord::new(agent_id, BriefKind::Result, text, None, None);
    let event = crate::types::brief_created_event_for(&brief)?;
    Ok(db
        .evidence()
        .append_brief_with_created_event(Some(agent_id), &brief, &event, &[])?
        .event
        .event_seq)
}

fn write_admissions(db: &RuntimeDb) -> u64 {
    db.reader_pool_writer_weak_for_tests()
        .upgrade()
        .expect("writer state alive")
        .write_channel_admissions_for_tests()
        .expect("write queue healthy")
}

/// Wait for fixture-appended background writes to stop taking write-channel
/// turns, then read the counter. Unlike a forced flush this injects no extra
/// ticket of its own, so before/after deltas stay exact.
fn quiesced_admissions(db: &RuntimeDb) -> u64 {
    let mut last = write_admissions(db);
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        let current = write_admissions(db);
        if current == last {
            return current;
        }
        last = current;
    }
    panic!("runtime db background writes did not quiesce within 2s");
}

#[test]
fn mark_repeat_noop_never_enters_writer_channel() -> Result<()> {
    let (_root, db) = fixture()?;
    let first_seq = append(&db, "agent-a", "one")?;
    let second_seq = append(&db, "agent-a", "two")?;

    // First mark pins the baseline cursor through the writer, exactly once.
    let before = quiesced_admissions(&db);
    let first = db
        .mark_brief_read("principal-1", "scope-1", "agent-a", second_seq)?
        .expect("active public agent");
    assert_eq!(first.applied_read_through_event_seq, second_seq);
    assert_eq!(first.state.unread_count, 0);
    assert_eq!(quiesced_admissions(&db), before + 1);

    // Repeat, stale and clamped-above-head requests are covered no-ops with
    // identical results and zero writer-channel admissions.
    for requested in [second_seq, first_seq, 0, u64::MAX] {
        let repeat = db
            .mark_brief_read("principal-1", "scope-1", "agent-a", requested)?
            .expect("active public agent");
        assert_eq!(repeat.applied_read_through_event_seq, second_seq);
        assert_eq!(repeat.state.read_through_event_seq, second_seq);
        assert_eq!(repeat.state.unread_count, 0);
        assert_eq!(repeat.state.revision, first.state.revision);
    }
    assert_eq!(
        quiesced_admissions(&db),
        before + 1,
        "covered no-op marks must not enter the writer channel"
    );
    Ok(())
}

#[test]
fn mark_first_touch_pins_cursor_via_writer_exactly_once() -> Result<()> {
    let (_root, db) = fixture()?;
    let head = append(&db, "agent-a", "one")?;

    // Without a persisted row the request still goes through the writer so
    // first-touch initialization keeps its previous behavior.
    let before = quiesced_admissions(&db);
    let first = db
        .mark_brief_read("principal-1", "scope-1", "agent-a", head)?
        .expect("active public agent");
    assert_eq!(first.applied_read_through_event_seq, head);
    assert_eq!(quiesced_admissions(&db), before + 1);

    let repeat = db
        .mark_brief_read("principal-1", "scope-1", "agent-a", head)?
        .expect("active public agent");
    assert_eq!(repeat.applied_read_through_event_seq, head);
    assert_eq!(quiesced_admissions(&db), before + 1);
    Ok(())
}

#[test]
fn mark_real_advance_still_uses_writer_and_advances_state() -> Result<()> {
    let (_root, db) = fixture()?;
    // Pin the baseline while the head is still at the first brief, then let a
    // new brief make the cursor genuinely stale.
    let first_seq = append(&db, "agent-a", "one")?;
    db.mark_brief_read("principal-1", "scope-1", "agent-a", first_seq)?;
    let second_seq = append(&db, "agent-a", "two")?;
    let stale = db
        .brief_read_state("principal-1", "scope-1", "agent-a")?
        .expect("active public agent");
    assert_eq!(stale.unread_count, 1);

    let before = quiesced_admissions(&db);
    let advanced = db
        .mark_brief_read("principal-1", "scope-1", "agent-a", second_seq)?
        .expect("active public agent");
    assert_eq!(advanced.applied_read_through_event_seq, second_seq);
    assert_eq!(advanced.state.read_through_event_seq, second_seq);
    assert_eq!(advanced.state.unread_count, 0);
    assert_eq!(quiesced_admissions(&db), before + 1);
    Ok(())
}

#[test]
fn mark_and_get_non_public_agent_skip_writer_channel() -> Result<()> {
    let (_root, db) = fixture()?;
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        "agent-private",
        AgentKind::Named,
        AgentVisibility::Private,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))?;

    let before = quiesced_admissions(&db);
    assert!(db
        .mark_brief_read("principal-1", "scope-1", "agent-private", 5)?
        .is_none());
    assert!(db
        .brief_read_state("principal-1", "scope-1", "agent-private")?
        .is_none());
    assert_eq!(
        quiesced_admissions(&db),
        before,
        "non-public agent lookups must not enter the writer channel"
    );
    Ok(())
}

#[test]
fn get_single_agent_repeat_reads_never_enter_writer_channel() -> Result<()> {
    let (_root, db) = fixture()?;
    let baseline = append(&db, "agent-a", "one")?;

    // First read pins the baseline cursor through the writer, exactly once.
    let before = quiesced_admissions(&db);
    let initial = db
        .brief_read_state("principal-1", "scope-1", "agent-a")?
        .expect("active public agent");
    assert_eq!(initial.read_through_event_seq, baseline);
    assert_eq!(quiesced_admissions(&db), before + 1);

    // Briefs arriving after the pinned baseline stay unread, and every repeat
    // read is answered from the reader pool.
    let later = append(&db, "agent-a", "two")?;
    assert!(later > baseline);
    let before_repeats = quiesced_admissions(&db);
    for _ in 0..3 {
        let state = db
            .brief_read_state("principal-1", "scope-1", "agent-a")?
            .expect("active public agent");
        assert_eq!(state.read_through_event_seq, baseline);
        assert_eq!(state.event_head_seq, later);
        assert_eq!(state.unread_count, 1);
    }
    assert_eq!(
        quiesced_admissions(&db),
        before_repeats,
        "repeat single-agent reads must not enter the writer channel"
    );
    Ok(())
}
