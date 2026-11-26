use super::super::{prepare_work_domain_snapshot_tx, WorkSnapshotError};
use super::{
    fresh, insert_qualified_work, legacy, raw_legacy_wait, raw_legacy_work, request, scope,
    task_wait, work,
};
use crate::runtime_db::RuntimeDb;
use crate::types::{WaitConditionStatus, WorkItemRecord, WorkItemState};
use anyhow::{ensure, Context, Result};
use rusqlite::params;
use sha2::{Digest, Sha256};

const BLOCKER: &str = "operator input for blocker provenance";

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn blocked_work(id: &str) -> WorkItemRecord {
    let mut work = work(id, WorkItemState::Open);
    work.blocked_by = Some(BLOCKER.into());
    work
}

fn matching_wait(id: &str, work_id: &str) -> Result<crate::types::WaitConditionRecord> {
    let mut wait = task_wait(id, "unused-provenance-task", "unused-provenance-trigger")?;
    wait.work_item_id = Some(work_id.into());
    wait.waiting_for = BLOCKER.into();
    wait.status = WaitConditionStatus::Active;
    wait.resolved_at = None;
    wait.trigger_message_id = None;
    wait.triggered_at = None;
    Ok(wait)
}

fn reopen(dir: &std::path::Path) -> Result<RuntimeDb> {
    RuntimeDb::open_and_migrate(dir.join("runtime.sqlite"), dir.join("runtime.lock"))
}

fn payload_digest(db: &RuntimeDb, id: &str) -> Result<String> {
    db.transaction(|tx| {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id=?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(sha256_hex(payload.as_bytes()))
    })
}

fn eligibility_digests(db: &RuntimeDb, id: &str) -> Result<Vec<String>> {
    db.transaction(|tx| {
        let mut stmt = tx.prepare(
            "SELECT content_digest FROM work_domain_eligibility
             WHERE family='work' AND record_id=?1 ORDER BY seed_id, capture_phase",
        )?;
        let rows = stmt
            .query_map([id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(rows)
    })
}

fn frozen_bytes(db: &RuntimeDb) -> Result<(String, String)> {
    db.transaction(|tx| {
        let roots: String = tx.query_row(
            "SELECT roots_json FROM episode_continuity_seeds ORDER BY seed_id",
            [],
            |row| row.get(0),
        )?;
        let captures: String = tx.query_row(
            "SELECT group_concat(metadata_digest, ',') FROM (
               SELECT metadata_digest FROM work_domain_captures ORDER BY seed_id, capture_phase
             )",
            [],
            |row| row.get(0),
        )?;
        Ok((roots, captures))
    })
}

fn revision(db: &RuntimeDb, id: &str) -> Result<i64> {
    db.transaction(|tx| {
        tx.query_row(
            "SELECT revision FROM work_items WHERE work_item_id=?1",
            [id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    })
}

#[test]
fn upgrade_refreshes_blocker_eligibility_and_second_start_is_idempotent() -> Result<()> {
    let (dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &blocked_work("provenance-work"))?;
        raw_legacy_wait(tx, &matching_wait("provenance-wait", "provenance-work")?)?;
        Ok(())
    })?;
    let digest = payload_digest(&db, "provenance-work")?;
    let eligibility = eligibility_digests(&db, "provenance-work")?;
    ensure!(
        !eligibility.is_empty(),
        "upgrade did not capture work eligibility"
    );
    ensure!(
        eligibility.iter().all(|item| item == &digest),
        "eligibility {eligibility:?} != payload {digest}"
    );
    ensure!(revision(&db, "provenance-work")? == 1);
    db.transaction(|tx| {
        let loaded =
            prepare_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(loaded.resident.coverage().observations.contains_key(
            &crate::runtime::resident_work::WorkKey::new(
                crate::runtime::resident_work::WorkFamily::Work,
                "provenance-work",
            )
        ));
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id='provenance-work'",
            [],
            |row| row.get(0),
        )?;
        let record: WorkItemRecord = serde_json::from_str(&payload)?;
        assert_eq!(
            record.blocked_by_wait_id.as_deref(),
            Some("provenance-wait")
        );
        assert_eq!(record.revision, 1);
        Ok(())
    })?;
    let frozen = frozen_bytes(&db)?;
    drop(db);
    let db = reopen(dir.path())?;
    ensure!(payload_digest(&db, "provenance-work")? == digest);
    ensure!(eligibility_digests(&db, "provenance-work")? == eligibility);
    ensure!(frozen_bytes(&db)? == frozen);
    ensure!(revision(&db, "provenance-work")? == 1);
    Ok(())
}

#[test]
fn stuck_library_refreshes_only_eligibility() -> Result<()> {
    let (dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &blocked_work("stuck-work"))?;
        raw_legacy_wait(tx, &matching_wait("stuck-wait", "stuck-work")?)?;
        Ok(())
    })?;
    let current = payload_digest(&db, "stuck-work")?;
    let frozen = frozen_bytes(&db)?;
    db.transaction(|tx| {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id='stuck-work'",
            [],
            |row| row.get(0),
        )?;
        let mut record: WorkItemRecord = serde_json::from_str(&payload)?;
        record.blocked_by_wait_id = None;
        let old = sha256_hex(&serde_json::to_vec(&record)?);
        let updated = tx.execute(
            "UPDATE work_domain_eligibility SET content_digest=?1
             WHERE family='work' AND record_id='stuck-work'",
            [old],
        )?;
        ensure!(updated > 0, "stuck fixture found no eligibility row");
        Ok(())
    })?;
    drop(db);
    let db = reopen(dir.path())?;
    ensure!(payload_digest(&db, "stuck-work")? == current);
    ensure!(eligibility_digests(&db, "stuck-work")?
        .iter()
        .all(|item| item == &current));
    ensure!(
        frozen_bytes(&db)? == frozen,
        "captures or roots_json changed"
    );
    ensure!(revision(&db, "stuck-work")? == 1);
    db.transaction(|tx| {
        prepare_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        Ok(())
    })?;
    Ok(())
}

#[test]
fn unproven_blocker_divergence_stays_fail_closed() -> Result<()> {
    let (dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &blocked_work("closed-work"))?;
        raw_legacy_wait(tx, &matching_wait("closed-wait", "closed-work")?)?;
        Ok(())
    })?;
    let (seed, expected) = db.transaction(|tx| {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id='closed-work'",
            [],
            |row| row.get(0),
        )?;
        let mut record: WorkItemRecord = serde_json::from_str(&payload)?;
        record.objective = "diverged outside provenance".into();
        let rewritten = serde_json::to_string(&record)?;
        tx.execute(
            "UPDATE work_items SET payload_json=?1 WHERE work_item_id='closed-work'",
            [&rewritten],
        )?;
        let seed: String = tx.query_row(
            "SELECT seed_id FROM work_domain_eligibility WHERE family='work' AND record_id='closed-work' LIMIT 1",
            [],
            |row| row.get(0),
        )?;
        let expected: String = tx.query_row(
            "SELECT content_digest FROM work_domain_eligibility WHERE family='work' AND record_id='closed-work' LIMIT 1",
            [],
            |row| row.get(0),
        )?;
        Ok((seed, expected))
    })?;
    drop(db);
    let db = reopen(dir.path())?;
    ensure!(eligibility_digests(&db, "closed-work")?
        .iter()
        .all(|item| item == &expected));
    let error = db
        .transaction(|tx| prepare_work_domain_snapshot_tx(tx, &scope(), &request()))
        .unwrap_err();
    let mismatch = error
        .downcast_ref::<WorkSnapshotError>()
        .context("startup did not fail closed")?;
    match mismatch {
        WorkSnapshotError::CanonicalChanged {
            record,
            seed: failed_seed,
            expected: failed_expected,
            actual,
            ..
        } => {
            assert_eq!(record, "closed-work");
            assert_eq!(failed_seed.as_deref(), Some(seed.as_str()));
            assert_eq!(failed_expected, &expected);
            assert_ne!(actual, &expected);
        }
        other => anyhow::bail!("expected CanonicalChanged, got {other:?}"),
    }
    Ok(())
}

#[test]
fn owner_write_alias_mismatch_stays_strict_and_backfill_skips_it() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut item = work("aliased-work", WorkItemState::Open);
    item.blocked_by = Some(BLOCKER.into());
    db.transaction(|tx| insert_qualified_work(tx, &item).map(|_| ()))?;
    let before = eligibility_digests(&db, "aliased-work")?;
    db.transaction(|tx| {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id='aliased-work'",
            [],
            |row| row.get(0),
        )?;
        let mut record: WorkItemRecord = serde_json::from_str(&payload)?;
        record.objective = "owner path tamper".into();
        tx.execute(
            "UPDATE work_items SET payload_json=?1 WHERE work_item_id='aliased-work'",
            params![serde_json::to_string(&record)?],
        )?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| prepare_work_domain_snapshot_tx(tx, &scope(), &request()))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("qualified Work changed outside its original transaction"),
        "{error:#}"
    );
    drop(db);
    let db = reopen(_dir.path())?;
    ensure!(eligibility_digests(&db, "aliased-work")? == before);
    ensure!(revision(&db, "aliased-work")? == 1);
    Ok(())
}
