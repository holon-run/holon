//! Original Evidence's actual Work footprint. Full members stay in SQL.
use super::*;
use crate::runtime_db::episode_snapshot::work_checkpoint::imports::ImportOrigin;

/// Only the original actual writer TX may create and consume this footprint.
/// No generic caller can supply a footprint or complete-marker body.
pub(super) struct OriginalWorkImport<'tx, 'conn, 'source> {
    tx: &'tx Transaction<'conn>,
    source: &'source OriginalEpisodeImport<'source>,
}
impl<'tx, 'conn, 'source> OriginalWorkImport<'tx, 'conn, 'source> {
    pub(super) fn begin(
        tx: &'tx Transaction<'conn>,
        source: &'source OriginalEpisodeImport<'source>,
    ) -> Result<Self> {
        tx.execute_batch("CREATE TEMP TABLE original_work_import_members(family TEXT NOT NULL,record_id TEXT NOT NULL,agent_id TEXT NOT NULL,raw_digest TEXT,origin TEXT NOT NULL,work_item_id TEXT,PRIMARY KEY(family,record_id));")?;
        Ok(Self { tx, source })
    }
    pub(super) fn record(
        &self,
        family: Family,
        id: &str,
        origin: ImportOrigin,
        _material: &mut RowMaterial<'_>,
    ) -> Result<()> {
        ensure!(
            matches!(family, Family::Message | Family::Transcript | Family::Brief),
            "invalid original Work footprint family"
        );
        ensure!(
            !id.is_empty() && !self.source.agent_id().is_empty(),
            "empty legacy import footprint key"
        );
        // The original map records only the key. Readback must remain after
        // every writer: a later Brief can refine an earlier Transcript row.
        // No String/body copy occurs in this SQL key insertion.
        let agent = self.source.agent_id();
        let origin = match origin {
            ImportOrigin::ActualWrite => "actual_write",
            ImportOrigin::LegacyImportEqual => "legacy_import_equal",
        };
        let same_agent:bool=self.tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM original_work_import_members WHERE family=?1 AND record_id=?2 AND agent_id!=?3)",params![family.name(),id,agent],|r|r.get(0))?;
        ensure!(same_agent, "legacy import footprint ID changed agent");
        self.tx.execute("INSERT INTO original_work_import_members(family,record_id,agent_id,origin) VALUES(?1,?2,?3,?4) ON CONFLICT(family,record_id) DO UPDATE SET origin=CASE WHEN original_work_import_members.origin='actual_write' THEN 'actual_write' ELSE excluded.origin END",params![family.name(),id,agent,origin])?;
        Ok(())
    }
    pub(super) fn complete(
        self,
        checkpoint: &serde_json::Value,
        header: &mut RowMaterial<'_>,
    ) -> Result<u64> {
        let mut peak = header.scratch.report()?.observed_bytes;
        // Source and exact actual row projections were validated within this
        // same original TX. Complete cannot accept arbitrary metadata SQL.
        let source_valid:bool=self.tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=?1 AND agent_id=?2 AND sealed=1)",params![COHORT,self.source.agent_id()],|r|r.get(0))?;
        ensure!(source_valid, "original Work checkpoint lost its source");
        let foreign: bool = self.tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM original_work_import_members WHERE agent_id!=?1)",
            [self.source.agent_id()],
            |r| r.get(0),
        )?;
        ensure!(!foreign, "original Work checkpoint changed agent");
        {
            let mut statement=self.tx.prepare("SELECT rowid FROM original_work_import_members ORDER BY family,record_id COLLATE BINARY")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let rowid: i64 = row.get(0)?;
                let mut material = RowMaterial::new(header.scratch);
                material.records = received_joint_records(material.records, header.records)?;
                let raw:u64=self.tx.query_row("SELECT 2048+length(CAST(family AS BLOB))+length(CAST(record_id AS BLOB))+length(CAST(agent_id AS BLOB)) FROM original_work_import_members WHERE rowid=?1",[rowid],|r|r.get(0))?;
                material.charge(
                    raw.checked_mul(8)
                        .context("original Work key raw overflow")?,
                    32,
                )?;
                let (family,id,agent):(String,String,String)=self.tx.query_row("SELECT family,record_id,agent_id FROM original_work_import_members WHERE rowid=?1",[rowid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                ensure!(
                    agent == self.source.agent_id(),
                    "legacy import actual record changed scope"
                );
                let family = match family.as_str() {
                    "message" => Family::Message,
                    "transcript" => Family::Transcript,
                    "brief" => Family::Brief,
                    _ => anyhow::bail!("invalid original Work footprint family"),
                };
                let actual_row: i64 = self.tx.query_row(
                    &format!("SELECT rowid FROM {} WHERE evidence_id=?1", family.table()),
                    [&id],
                    |r| r.get(0),
                )?;
                let (actual_id, actual_agent, payload) =
                    material.payload(self.tx, family, actual_row)?;
                ensure!(
                    actual_id == id && actual_agent == agent,
                    "legacy import actual record changed scope"
                );
                let work = match family {
                    Family::Message => {
                        let record: crate::types::MessageEnvelope = serde_json::from_str(&payload)?;
                        ensure!(
                            record.id == id && record.agent_id == agent,
                            "legacy Message import projection mismatch"
                        );
                        None
                    }
                    Family::Transcript => {
                        let record: TranscriptEntry = serde_json::from_str(&payload)?;
                        ensure!(
                            record.id == id && record.agent_id == agent,
                            "legacy Transcript import projection mismatch"
                        );
                        None
                    }
                    Family::Brief => {
                        let record: BriefRecord = serde_json::from_str(&payload)?;
                        ensure!(
                            record.id == id && record.agent_id == agent,
                            "legacy Brief import projection mismatch"
                        );
                        record.work_item_id
                    }
                    _ => unreachable!(),
                };
                let hash = format!("{:x}", Sha256::digest(payload.as_bytes()));
                self.tx.execute("UPDATE original_work_import_members SET raw_digest=?2,work_item_id=?3 WHERE rowid=?1",params![rowid,hash,work])?;
                peak = peak.max(header.scratch.report()?.observed_bytes);
                drop(payload);
                drop(actual_agent);
                drop(actual_id);
                drop(id);
                drop(agent);
                drop(material);
            }
        }
        let cost = received_material(checkpoint)?;
        header.charge(cost.0, cost.1)?;
        let checkpoint = serde_json::to_string(checkpoint)?;
        // Keep the original v1 object and complete-marker semantics. The full
        // global records JSON is built and installed only in SQL, never read
        // back into a Rust String, Value or BTreeMap.
        self.tx.execute_batch(
            "CREATE TEMP TABLE original_work_import_checkpoint(payload_json TEXT NOT NULL);",
        )?;
        self.tx.execute("INSERT INTO original_work_import_checkpoint(payload_json) SELECT json_set(?1,'$.work_domain_actual_writes',json_object('policy_version',1,'records',json('{'||COALESCE((SELECT group_concat(json_quote(record_key)||':'||record_json,',') FROM (SELECT family||':'||record_id AS record_key,json_object('agent_id',agent_id,'origin',origin,'raw_digest',raw_digest,'work_item_id',work_item_id) AS record_json FROM original_work_import_members ORDER BY family||':'||record_id COLLATE BINARY)),'')||'}')))",[checkpoint])?;
        let bytes:u64=self.tx.query_row("SELECT length(CAST(json_extract(payload_json,'$.work_domain_actual_writes') AS BLOB)) FROM original_work_import_checkpoint",[],|r|r.get(0))?;
        ensure!(
            bytes <= 64 * 1024 * 1024,
            "legacy Work import footprint metadata exceeds original bound"
        );
        let now = crate::runtime_db::migrations::timestamp(chrono::Utc::now());
        self.tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,source_checkpoint_json,imported_at,updated_at) SELECT 'evidence',?1,'complete','db',payload_json,?2,?2 FROM original_work_import_checkpoint WHERE true ON CONFLICT(domain) DO UPDATE SET schema_version=excluded.schema_version,import_status=excluded.import_status,canonical_source=excluded.canonical_source,source_checkpoint_json=excluded.source_checkpoint_json,imported_at=excluded.imported_at,updated_at=excluded.updated_at",params![crate::runtime_db::migrations::max_known_migration_version(),now])?;
        self.tx.execute_batch(
            "DROP TABLE original_work_import_checkpoint;DROP TABLE original_work_import_members;",
        )?;
        Ok(peak.max(header.scratch.report()?.observed_bytes))
    }
}

impl RuntimeDb {
    pub(super) fn begin_original_work_evidence_import(&self) -> Result<()> {
        self.transaction(|tx| {
            let now=crate::runtime_db::migrations::timestamp(chrono::Utc::now());
            // Preserve the existing importing checkpoint in SQL rather than
            // reading a potentially global v1 records object into Rust.
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,source_checkpoint_json,imported_at,updated_at) VALUES('evidence',?1,'importing','jsonl',(SELECT source_checkpoint_json FROM storage_domains WHERE domain='evidence'),NULL,?2) ON CONFLICT(domain) DO UPDATE SET schema_version=excluded.schema_version,import_status=excluded.import_status,canonical_source=excluded.canonical_source,source_checkpoint_json=excluded.source_checkpoint_json,imported_at=excluded.imported_at,updated_at=excluded.updated_at",params![crate::runtime_db::migrations::max_known_migration_version(),now])?;
            Ok(())
        })
    }
    pub(super) fn fail_original_work_evidence_import(&self, error: &anyhow::Error) -> Result<()> {
        let checkpoint = serde_json::json!({"error":error.to_string(),"retry":"restart runtime to retry legacy import"});
        self.transaction_once(|tx| {
            crate::runtime_db::storage_domain::upsert_storage_domain(
                tx,
                "evidence",
                "failed",
                "jsonl",
                Some(checkpoint),
            )
        })
    }
}
