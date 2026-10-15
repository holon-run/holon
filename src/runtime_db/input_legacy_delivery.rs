//! Original LegacyDelivery cutover. No ordinary read fallback or native birth.
use super::*;
use crate::runtime::input_evidence_budget::{
    InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};

pub(in crate::runtime_db) const LEGACY_DELIVERY_SCHEMA: &str =
    include_str!("input_legacy_delivery/schema.sql");
#[path = "input_legacy_delivery/heads.rs"]
pub(in crate::runtime_db) mod heads;
#[path = "input_legacy_delivery/import.rs"]
pub(in crate::runtime_db) mod imports;
#[path = "input_legacy_delivery/row.rs"]
mod row;

const UPGRADE: &str = "legacy_delivery/v1/upgrade";

#[derive(Debug, Default)]
pub(super) struct LegacyDeliveryScratchReport {
    pub(super) members: u64,
    pub(super) peak_owned_estimate_bytes: u64,
}
struct StreamDigest<'a>(&'a mut Sha256);
impl std::io::Write for StreamDigest<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(in crate::runtime_db) fn capture_legacy_delivery_upgrade_tx(
    tx: &Transaction<'_>,
) -> Result<()> {
    // This one actual pre-Host migration owns its scratch accounting. It never
    // invents an identity scope or lends its ledger to a runtime publication.
    let migration_scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    capture_upgrade_with_scratch_tx(tx, &migration_scratch).map(|_| ())
}
fn capture_upgrade_with_scratch_tx(
    tx: &Transaction<'_>,
    scratch: &InputEvidenceBudgetLedger,
) -> Result<LegacyDeliveryScratchReport> {
    let previous: Option<(bool,u64,usize,bool)> = tx.query_row(
        "SELECT sealed,member_count,length(CAST(metadata_digest AS BLOB)),metadata_digest NOT GLOB '*[^0-9a-f]*' FROM input_legacy_delivery_captures WHERE capture_id=?1",
        [UPGRADE], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?;
    if let Some((sealed, members, digest_bytes, hex)) = previous {
        ensure!(
            sealed && digest_bytes == 64 && hex,
            "LegacyDelivery fixed capture is incomplete"
        );
        return Ok(LegacyDeliveryScratchReport {
            members,
            peak_owned_estimate_bytes: 0,
        });
    }
    let header = scratch.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: 4096,
        },
    )?;
    let available:bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='delivery_summaries')",
        [], |row|row.get(0))?;
    tx.execute("INSERT INTO input_legacy_delivery_captures(capture_id,table_available,member_count,metadata_digest,sealed) VALUES(?1,?2,0,'',0)",params![UPGRADE,available])?;
    let mut report = LegacyDeliveryScratchReport {
        members: 0,
        peak_owned_estimate_bytes: scratch.report()?.observed_bytes,
    };
    let mut digest = Sha256::new();
    if available {
        // The durable inventory is streamed, including >65536 old rows. Only
        // one row and this digest coexist in Rust; the SQL primary-key scan is
        // not a resident history window or a request to retain all old bodies.
        let mut statement =
            tx.prepare("SELECT rowid FROM delivery_summaries ORDER BY evidence_id")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let original = row::read_original_row_tx(tx, rowid, scratch, 0)?;
            report.peak_owned_estimate_bytes = report
                .peak_owned_estimate_bytes
                .max(scratch.report()?.observed_bytes);
            tx.execute("INSERT INTO input_legacy_delivery_fixed_members(capture_id,record_id,agent_id,identity_incarnation,seed_id,work_item_id,created_at,text_nonempty,fingerprint_json,content_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![UPGRADE,original.id,original.agent,original.inc,original.seed,original.work,
                    original.at,original.nonempty,original.fingerprint_json,original.content])?;
            serde_json::to_writer(
                StreamDigest(&mut digest),
                &(
                    &original.id,
                    &original.agent,
                    original.inc,
                    &original.seed,
                    &original.work,
                    &original.at,
                    original.nonempty,
                    &original.fingerprint_json,
                    &original.content,
                ),
            )?;
            report.members = report
                .members
                .checked_add(1)
                .context("LegacyDelivery member count overflow")?;
            drop(original);
        }
    }
    let sealed = format!("{:x}", digest.finalize());
    let changed=tx.execute("UPDATE input_legacy_delivery_captures SET member_count=?2,metadata_digest=?3,sealed=1 WHERE capture_id=?1 AND sealed=0",params![UPGRADE,report.members,sealed])?;
    ensure!(
        changed == 1,
        "LegacyDelivery fixed capture did not seal once"
    );
    drop(header);
    Ok(report)
}

#[cfg(test)]
#[path = "input_legacy_delivery/tests.rs"]
mod tests;

#[path = "legacy_episode_import.rs"]
pub(in crate::runtime_db) mod original_episode;
