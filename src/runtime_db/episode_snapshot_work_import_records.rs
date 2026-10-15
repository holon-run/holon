//! The original first-import v1 footprint, streamed into indexed SQL scratch.
//! This is not a resident cache or ordinary read/adoption API.
use super::*;
use crate::runtime::input_evidence_budget::{
    BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};

pub(super) struct ImportedRecords {
    scratch: InputEvidenceBudgetLedger,
    agent_id: String,
    connection_address: usize,
    view_id: String,
    #[cfg(test)]
    peak: std::sync::atomic::AtomicU64,
    _header: Arc<BudgetViewGuard>,
}
impl ImportedRecords {
    pub(super) fn load(connection: &Connection, agent_id: &str) -> Result<Self> {
        let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
        let bytes = (agent_id.len() as u64 + 4096)
            .checked_mul(8)
            .context("Work import reader header overflow")?;
        ensure!(
            bytes <= 64 * 1024 * 1024,
            "Work import reader header exceeds material bound"
        );
        let header = scratch.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        connection.execute_batch("DROP TABLE IF EXISTS original_work_import_read_scope;DROP TABLE IF EXISTS original_work_import_read_entries;DROP TABLE IF EXISTS original_work_import_read_records;CREATE TEMP TABLE original_work_import_read_scope(view_id TEXT NOT NULL,agent_id TEXT NOT NULL);CREATE TEMP TABLE original_work_import_read_entries(record_key TEXT NOT NULL,record_json TEXT NOT NULL);CREATE TEMP TABLE original_work_import_read_records(family TEXT NOT NULL,record_id TEXT NOT NULL,agent_id TEXT NOT NULL,raw_digest TEXT NOT NULL,origin TEXT NOT NULL,work_item_id TEXT,PRIMARY KEY(family,record_id));")?;
        let view = Self {
            scratch,
            _header: header,
            agent_id: agent_id.to_owned(),
            connection_address: connection as *const Connection as usize,
            view_id: crate::ids::audit_event_id(),
            #[cfg(test)]
            peak: std::sync::atomic::AtomicU64::new(bytes),
        };
        connection.execute(
            "INSERT INTO original_work_import_read_scope(view_id,agent_id) VALUES(?1,?2)",
            params![view.view_id, view.agent_id],
        )?;
        let mut total = 0usize;
        for (domain, families) in [
            ("work_items", &["work_item"][..]),
            ("wait_conditions", &["wait"][..]),
            ("messages", &["message"][..]),
            ("transcript_entries", &["transcript"][..]),
            ("evidence", &["brief", "message", "transcript"][..]),
        ] {
            let bytes:Option<usize>=connection.query_row("SELECT length(CAST(source_checkpoint_json AS BLOB)) FROM storage_domains WHERE domain=?1 AND import_status='complete' AND canonical_source='db'",[domain],|r|r.get(0)).optional()?.flatten();
            let Some(bytes) = bytes else { continue };
            total = total
                .checked_add(bytes)
                .context("Work import metadata size overflow")?;
            bound(total)?;
            // The original total metadata bound applies to SQL parsing. It is
            // not charged as a Rust body: no global checkpoint String exists.
            let present:bool=connection.query_row("SELECT json_type(source_checkpoint_json,'$.work_domain_actual_writes') IS NOT NULL FROM storage_domains WHERE domain=?1",[domain],|r|r.get(0))?;
            if !present {
                continue;
            }
            let valid:bool=connection.query_row("SELECT json_type(source_checkpoint_json,'$.work_domain_actual_writes')='object' AND json_type(source_checkpoint_json,'$.work_domain_actual_writes.policy_version')='integer' AND json_extract(source_checkpoint_json,'$.work_domain_actual_writes.policy_version')=1 AND json_type(source_checkpoint_json,'$.work_domain_actual_writes.records')='object' FROM storage_domains WHERE domain=?1",[domain],|r|r.get(0))?;
            ensure!(
                valid,
                "legacy Work footprint policy or records shape mismatch"
            );
            connection.execute("INSERT INTO original_work_import_read_entries(record_key,record_json) SELECT j.key,j.value FROM storage_domains d,json_each(d.source_checkpoint_json,'$.work_domain_actual_writes.records') j WHERE d.domain=?1",[domain])?;
            {
                let mut query=connection.prepare("SELECT rowid FROM original_work_import_read_entries ORDER BY record_key COLLATE BINARY")?;
                let mut rows = query.query([])?;
                while let Some(row) = rows.next()? {
                    let rowid: i64 = row.get(0)?;
                    let raw:u64=connection.query_row("SELECT 2048+length(CAST(record_key AS BLOB))+length(CAST(record_json AS BLOB)) FROM original_work_import_read_entries WHERE rowid=?1",[rowid],|r|r.get(0))?;
                    let raw = raw
                        .checked_mul(8)
                        .context("Work import record raw overflow")?;
                    let _raw = view.charge(raw, 32)?;
                    let nodes:usize=connection.query_row("SELECT 32+(SELECT COUNT(*) FROM json_tree(record_json)) FROM original_work_import_read_entries WHERE rowid=?1",[rowid],|r|r.get(0))?;
                    let nodes = nodes
                        .checked_mul(8)
                        .context("Work import record native overflow")?;
                    let _native = view.charge(
                        (nodes as u64)
                            .checked_mul(256)
                            .context("Work import record native overflow")?,
                        nodes
                            .checked_add(32)
                            .context("Work import record combined nodes overflow")?,
                    )?;
                    let valid:bool=connection.query_row("SELECT json_type(record_json)='object' AND json_type(record_json,'$.agent_id')='text' AND json_type(record_json,'$.raw_digest')='text' AND json_extract(record_json,'$.origin') IN ('actual_write','legacy_import_equal') AND (json_type(record_json,'$.work_item_id') IS NULL OR json_type(record_json,'$.work_item_id') IN ('null','text')) FROM original_work_import_read_entries WHERE rowid=?1",[rowid],|r|r.get(0))?;
                    ensure!(valid, "legacy Work footprint record shape mismatch");
                    let (key,agent):(String,String)=connection.query_row("SELECT record_key,json_extract(record_json,'$.agent_id') FROM original_work_import_read_entries WHERE rowid=?1",[rowid],|r|Ok((r.get(0)?,r.get(1)?)))?;
                    let (family, id) = key
                        .split_once(':')
                        .context("invalid legacy Work footprint key")?;
                    ensure!(
                        families.contains(&family),
                        "legacy Work footprint domain mismatch"
                    );
                    if agent == agent_id {
                        // Domain order preserves the old map's final actual
                        // original record; a foreign later row cannot replace it.
                        connection.execute("INSERT INTO original_work_import_read_records(family,record_id,agent_id,raw_digest,origin,work_item_id) SELECT ?2,?3,json_extract(record_json,'$.agent_id'),json_extract(record_json,'$.raw_digest'),json_extract(record_json,'$.origin'),json_extract(record_json,'$.work_item_id') FROM original_work_import_read_entries WHERE rowid=?1 ON CONFLICT(family,record_id) DO UPDATE SET agent_id=excluded.agent_id,raw_digest=excluded.raw_digest,origin=excluded.origin,work_item_id=excluded.work_item_id",params![rowid,family,id])?;
                    }
                    drop(agent);
                    drop(key);
                }
            }
            connection.execute("DELETE FROM original_work_import_read_entries", [])?;
        }
        Ok(view)
    }
    fn charge(&self, bytes: u64, records: usize) -> Result<Arc<BudgetViewGuard>> {
        ensure!(
            records.checked_add(32).is_some_and(|n| n <= 65536)
                && bytes
                    .checked_add(self.scratch.report()?.observed_bytes)
                    .is_some_and(|n| n <= 64 * 1024 * 1024),
            EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "legacy Work import row material"
            }
        );
        let guard = self.scratch.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        #[cfg(test)]
        self.peak.fetch_max(
            self.scratch.report()?.observed_bytes,
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(guard)
    }
    fn validate_connection(&self, connection: &Connection) -> Result<()> {
        ensure!(
            self.connection_address == connection as *const Connection as usize,
            "Work import read view changed its actual connection"
        );
        let same:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM original_work_import_read_scope WHERE view_id=?1 AND agent_id=?2)",params![self.view_id,self.agent_id],|r|r.get(0))?;
        ensure!(same, "Work import read view was replaced");
        Ok(())
    }
    pub(super) fn matches(&self, connection: &Connection, key: &str, digest: &str) -> Result<bool> {
        self.validate_connection(connection)?;
        let (family, id) = key
            .split_once(':')
            .context("invalid Work import lookup key")?;
        connection.query_row("SELECT EXISTS(SELECT 1 FROM original_work_import_read_records WHERE family=?1 AND record_id=?2 AND agent_id=?3 AND raw_digest=?4)",params![family,id,self.agent_id,digest],|r|r.get(0)).map_err(Into::into)
    }
    pub(super) fn has(&self, connection: &Connection, key: &str) -> Result<bool> {
        self.validate_connection(connection)?;
        let (family, id) = key
            .split_once(':')
            .context("invalid Work import lookup key")?;
        connection.query_row("SELECT EXISTS(SELECT 1 FROM original_work_import_read_records WHERE family=?1 AND record_id=?2 AND agent_id=?3)",params![family,id,self.agent_id],|r|r.get(0)).map_err(Into::into)
    }
    pub(super) fn has_brief_for_work(&self, connection: &Connection, work: &str) -> Result<bool> {
        self.validate_connection(connection)?;
        connection.query_row("SELECT EXISTS(SELECT 1 FROM original_work_import_read_records WHERE family='brief' AND work_item_id=?1 AND agent_id=?2)",params![work,self.agent_id],|r|r.get(0)).map_err(Into::into)
    }
    pub(super) fn visit_messages(
        &self,
        connection: &Connection,
        mut visit: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        self.validate_connection(connection)?;
        let mut statement=connection.prepare("SELECT rowid FROM original_work_import_read_records WHERE family='message' AND agent_id=?1 ORDER BY record_id COLLATE BINARY")?;
        let mut rows = statement.query([&self.agent_id])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let raw:u64=connection.query_row("SELECT 1024+length(CAST(record_id AS BLOB))+length(CAST(raw_digest AS BLOB)) FROM original_work_import_read_records WHERE rowid=?1",[rowid],|r|r.get(0))?;
            let _guard = self.charge(
                raw.checked_mul(8)
                    .context("Work imported Message address overflow")?,
                32,
            )?;
            let (id, digest): (String, String) = connection.query_row(
                "SELECT record_id,raw_digest FROM original_work_import_read_records WHERE rowid=?1",
                [rowid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            visit(&id, &digest)?;
        }
        Ok(())
    }
    pub(super) fn select_head(
        &self,
        connection: &Connection,
        agent: &str,
        work: &str,
        fixed: &BTreeSet<String>,
    ) -> Result<(Option<(String, String)>, Vec<Arc<BudgetViewGuard>>)> {
        self.validate_connection(connection)?;
        ensure!(agent == self.agent_id, "Work head reader changed scope");
        let bytes = crate::runtime::input_evidence_budget::serialized_bytes(fixed)?;
        let selector_records = fixed
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(32))
            .context("Work head selector records overflow")?;
        let selector = self.charge(
            bytes
                .checked_add(1024)
                .and_then(|n| n.checked_mul(8))
                .and_then(|n| n.checked_add((selector_records as u64).checked_mul(256)?))
                .context("Work head selector overflow")?,
            selector_records,
        )?;
        let fixed = serde_json::to_string(fixed)?;
        // The ordered candidate cursor avoids allocating every accepted Brief
        // ID, and evaluates text only after this row's actual raw guard.
        let mut query=connection.prepare("SELECT b.rowid,2048+length(CAST(b.evidence_id AS BLOB))+length(CAST(b.payload_json AS BLOB)) FROM briefs b WHERE b.agent_id=?1 AND b.work_item_id=?2 AND b.kind='result' AND (b.evidence_id IN (SELECT record_id FROM original_work_import_read_records WHERE family='brief' AND agent_id=?1 AND work_item_id=?2) OR b.evidence_id IN (SELECT value FROM json_each(?3))) ORDER BY b.created_at DESC,b.evidence_id DESC")?;
        let mut rows = query.query(params![agent, work, fixed])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let raw: u64 = row.get(1)?;
            let raw = self.charge(
                raw.checked_mul(8)
                    .context("Work import head raw overflow")?,
                32,
            )?;
            let nonempty:bool=connection.query_row("SELECT COALESCE(length(json_extract(payload_json,'$.text'))>0,0) FROM briefs WHERE rowid=?1",[rowid],|r|r.get(0))?;
            if !nonempty {
                continue;
            }
            let nodes: usize = connection.query_row(
                "SELECT COUNT(*)+32 FROM briefs b,json_tree(b.payload_json) WHERE b.rowid=?1",
                [rowid],
                |r| r.get(0),
            )?;
            let nodes = nodes
                .checked_mul(8)
                .context("Work import head native overflow")?;
            let native = self.charge(
                (nodes as u64)
                    .checked_mul(256)
                    .context("Work import head native overflow")?,
                nodes
                    .checked_add(selector_records)
                    .and_then(|n| n.checked_add(32))
                    .context("Work import head combined records overflow")?,
            )?;
            let body: (String, String) = connection.query_row(
                "SELECT evidence_id,payload_json FROM briefs WHERE rowid=?1",
                [rowid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            return Ok((Some(body), vec![selector, raw, native]));
        }
        Ok((None, vec![selector]))
    }
    pub(super) fn finish(self, connection: &Connection) -> Result<()> {
        self.validate_connection(connection)?;
        connection.execute_batch("DROP TABLE original_work_import_read_scope;DROP TABLE original_work_import_read_entries;DROP TABLE original_work_import_read_records;")?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "episode_snapshot_work_import_records_tests.rs"]
mod tests;
