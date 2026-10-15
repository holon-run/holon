//! Only the original Evidence import records this finite actual write footprint.
//! Public raw append does not call this actor and acquires no source membership.
use super::*;
use crate::runtime::input_evidence_budget::{serialized_bytes, BudgetViewGuard};

const IMPORT: &str = "legacy_delivery/v1/actual_evidence_import";
pub(in crate::runtime_db) struct LegacyDeliveryImport<'tx, 'conn, 'ledger> {
    tx: &'tx Transaction<'conn>,
    scratch: &'ledger InputEvidenceBudgetLedger,
    prior_records: usize,
    #[cfg(test)]
    peak_owned: std::cell::Cell<u64>,
}
pub(in crate::runtime_db) fn value_nodes(value: &serde_json::Value, depth: usize) -> Result<usize> {
    ensure!(
        depth <= MAX_DEPTH,
        InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery import depth")
    );
    let mut count = 1usize;
    let mut add = |child: &serde_json::Value| -> Result<()> {
        count = count
            .checked_add(value_nodes(child, depth + 1)?)
            .context("LegacyDelivery import nodes overflow")?;
        ensure!(
            count <= MAX_RECORDS,
            InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery import nodes")
        );
        Ok(())
    };
    match value {
        serde_json::Value::Array(values) => {
            for child in values {
                add(child)?;
            }
        }
        serde_json::Value::Object(values) => {
            for child in values.values() {
                add(child)?;
            }
        }
        _ => {}
    }
    Ok(count)
}
pub(in crate::runtime_db) fn checkpoint_material(
    value: &serde_json::Value,
) -> Result<(u64, usize)> {
    let records = value_nodes(value, 0)?
        .checked_add(32)
        .and_then(|n| n.checked_mul(8))
        .context("Evidence import checkpoint records overflow")?;
    let bytes = serialized_bytes(value)?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(u64::try_from(records).ok()?.checked_mul(256)?))
        .context("Evidence import checkpoint bytes overflow")?;
    Ok((bytes, records))
}
impl<'tx, 'conn, 'ledger> LegacyDeliveryImport<'tx, 'conn, 'ledger> {
    pub(in crate::runtime_db) fn begin_if_installed(
        tx: &'tx Transaction<'conn>,
        scratch: &'ledger InputEvidenceBudgetLedger,
        prior_records: usize,
    ) -> Result<Option<Self>> {
        let schema:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_legacy_delivery_captures')",[],|r|r.get(0))?;
        if !schema {
            let installed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=91)",
                [],
                |r| r.get(0),
            )?;
            ensure!(!installed, "LegacyDelivery installed import schema missing");
            return Ok(None);
        }
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id=?1)",
                [IMPORT],
                |r| r.get::<_, bool>(0)
            )?,
            "LegacyDelivery original import already captured"
        );
        // Same storage-domain TX; a failure rolls back this header and every
        // actual write/member. The original complete receipt is still LAST.
        tx.execute("INSERT INTO input_legacy_delivery_captures(capture_id,table_available,member_count,metadata_digest,sealed) VALUES(?1,1,0,'',0)",[IMPORT])?;
        Ok(Some(Self {
            tx,
            scratch,
            prior_records,
            #[cfg(test)]
            peak_owned: std::cell::Cell::new(scratch.report()?.observed_bytes),
        }))
    }
    pub(in crate::runtime_db) fn with_prior_records(mut self, records: usize) -> Result<Self> {
        ensure!(
            records <= MAX_RECORDS,
            InputEvidenceSnapshotError::ResourceExhausted("original episode source scratch")
        );
        self.prior_records = records;
        Ok(self)
    }
    pub(in crate::runtime_db) fn append_original(
        &self,
        record: &DeliverySummaryRecord,
    ) -> Result<()> {
        let nodes = record
            .evidence
            .as_ref()
            .map(|v| value_nodes(v, 0))
            .transpose()?
            .unwrap_or(0)
            .checked_add(24)
            .context("LegacyDelivery incoming nodes overflow")?;
        let records = nodes
            .checked_mul(8)
            .context("LegacyDelivery incoming records overflow")?;
        let old:u64=self.tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM delivery_summaries WHERE evidence_id=?1),0)",[&record.id],|r|r.get(0))?;
        let bytes = serialized_bytes(record)?
            .checked_add(old)
            .and_then(|n| n.checked_add(1024))
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(u64::try_from(records).ok()?.checked_mul(256)?))
            .context("LegacyDelivery incoming bytes overflow")?;
        ensure!(
            bytes
                .checked_add(self.scratch.report()?.observed_bytes)
                .is_some_and(|n| n <= MAX_METADATA_BYTES as u64)
                && records
                    .checked_add(self.prior_records)
                    .is_some_and(|n| n <= MAX_RECORDS),
            InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery import original write")
        );
        let _incoming = self.scratch.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        // The old payload String, exact incoming serialization and original
        // repository SQL allocation are admitted before the existing kernel.
        super::super::super::evidence::insert_delivery_summary_evidence_tx(self.tx, record)?;
        let rowid: i64 = self.tx.query_row(
            "SELECT rowid FROM delivery_summaries WHERE evidence_id=?1",
            [&record.id],
            |r| r.get(0),
        )?;
        let original = super::row::read_original_row_tx(
            self.tx,
            rowid,
            self.scratch,
            self.prior_records
                .checked_add(records)
                .context("LegacyDelivery row prior overflow")?,
        )?;
        #[cfg(test)]
        self.peak_owned.set(
            self.peak_owned
                .get()
                .max(self.scratch.report()?.observed_bytes),
        );
        let mut digest = Sha256::new();
        serde_json::to_writer(StreamDigest(&mut digest), record)?;
        ensure!(
            original.id == record.id
                && original.agent == record.agent_id
                && original.content == format!("{:x}", digest.finalize()),
            "LegacyDelivery import differs from its actual received write"
        );
        // A repeated ID retains the final actual write in this same TX. This
        // is a bounded SQL footprint; no complete ID map is retained in Rust.
        self.tx.execute("INSERT INTO input_legacy_delivery_fixed_members(capture_id,record_id,agent_id,identity_incarnation,seed_id,work_item_id,created_at,text_nonempty,fingerprint_json,content_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(capture_id,record_id) DO UPDATE SET agent_id=excluded.agent_id,identity_incarnation=excluded.identity_incarnation,seed_id=excluded.seed_id,work_item_id=excluded.work_item_id,created_at=excluded.created_at,text_nonempty=excluded.text_nonempty,fingerprint_json=excluded.fingerprint_json,content_digest=excluded.content_digest",
            params![IMPORT,original.id,original.agent,original.inc,original.seed,original.work,
                original.at,original.nonempty,original.fingerprint_json,original.content])?;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn peak_owned_bytes_for_test(&self) -> u64 {
        self.peak_owned.get()
    }
    pub(in crate::runtime_db) fn finish(self) -> Result<(u64, String)> {
        let mut digest = Sha256::new();
        let mut members = 0u64;
        let mut query=self.tx.prepare("SELECT rowid FROM input_legacy_delivery_fixed_members WHERE capture_id=?1 ORDER BY record_id")?;
        let mut rows = query.query([IMPORT])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let raw:u64=self.tx.query_row("SELECT 2048+length(CAST(record_id AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0)+length(CAST(work_item_id AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(fingerprint_json AS BLOB))+length(CAST(content_digest AS BLOB)) FROM input_legacy_delivery_fixed_members WHERE rowid=?1",[rowid],|r|r.get(0))?;
            let bytes = raw
                .checked_mul(8)
                .context("LegacyDelivery import seal bytes overflow")?;
            ensure!(
                bytes
                    .checked_add(self.scratch.report()?.observed_bytes)
                    .is_some_and(|n| n <= MAX_METADATA_BYTES as u64)
                    && self
                        .prior_records
                        .checked_add(128)
                        .is_some_and(|n| n <= MAX_RECORDS),
                InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery import seal")
            );
            let _row = self.scratch.register_host_owned_bytes(
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: bytes,
                    metadata_estimate_bytes: 0,
                },
            )?;
            let tuple:(String,String,Option<u64>,Option<String>,String,String,bool,String,String)=self.tx.query_row(
                "SELECT record_id,agent_id,identity_incarnation,seed_id,work_item_id,created_at,text_nonempty,fingerprint_json,content_digest FROM input_legacy_delivery_fixed_members WHERE rowid=?1",[rowid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?;
            serde_json::to_writer(StreamDigest(&mut digest), &tuple)?;
            members = members
                .checked_add(1)
                .context("LegacyDelivery import count overflow")?;
            drop(tuple);
        }
        let digest = format!("{:x}", digest.finalize());
        let changed=self.tx.execute("UPDATE input_legacy_delivery_captures SET member_count=?2,metadata_digest=?3,sealed=1 WHERE capture_id=?1 AND sealed=0",params![IMPORT,members,digest])?;
        ensure!(changed == 1, "LegacyDelivery import did not seal once");
        Ok((members, digest))
    }
}

pub(in crate::runtime_db) fn observe_existing_import_metadata(
    scratch: &InputEvidenceBudgetLedger,
    cost: (u64, usize),
) -> Result<Arc<BudgetViewGuard>> {
    ensure!(
        cost.0
            .checked_add(scratch.report()?.observed_bytes)
            .is_some_and(|n| n <= MAX_METADATA_BYTES as u64)
            && cost.1.checked_add(32).is_some_and(|n| n <= MAX_RECORDS),
        InputEvidenceSnapshotError::ResourceExhausted("original Evidence import shared metadata")
    );
    Ok(scratch.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: cost.0,
        },
    )?)
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
