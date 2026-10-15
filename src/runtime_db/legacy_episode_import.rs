//! The original pre-Host episode importer freezes its SQL source once. This
//! private cohort is not an ordinary Input source or a native-insert grant.
use super::*;
use crate::runtime::input_evidence_budget::BudgetViewGuard;
use crate::runtime_db::{evidence::*, RuntimeDb};
use crate::storage::OriginalEpisodeImport;
use crate::types::{BriefRecord, ToolExecutionRecord, TranscriptEntry};
use serde::Serialize;

const COHORT: &str = "original_episode_refs/v1";

/// Count the serialized shape without retaining a Value or an encoded body.
/// Object keys count as additional nodes, conservatively covering map slots.
#[derive(Default)]
struct BorrowedShape {
    bytes: u64,
    nodes: usize,
    depth: usize,
    quoted: bool,
    escaped: bool,
    scalar: bool,
    exhausted: bool,
}
impl std::io::Write for BorrowedShape {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("original import size overflow"))?;
        if self.bytes > MAX_METADATA_BYTES as u64 {
            self.exhausted = true;
            return Err(std::io::Error::other("original import byte limit"));
        }
        for byte in bytes {
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if *byte == b'\\' {
                    self.escaped = true;
                } else if *byte == b'"' {
                    self.quoted = false;
                }
                continue;
            }
            match *byte {
                b'"' => {
                    self.nodes += 1;
                    self.quoted = true;
                    self.scalar = false;
                }
                b'{' | b'[' => {
                    self.nodes += 1;
                    self.depth += 1;
                    self.scalar = false;
                }
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    self.scalar = false;
                }
                b',' | b':' | b' ' | b'\n' | b'\t' | b'\r' => {
                    self.scalar = false;
                }
                _ => {
                    if !self.scalar {
                        self.nodes += 1;
                        self.scalar = true;
                    }
                }
            }
            if self.nodes > MAX_RECORDS || self.depth > MAX_DEPTH {
                self.exhausted = true;
                return Err(std::io::Error::other("original import shape limit"));
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(in crate::runtime_db) fn received_material(value: &impl Serialize) -> Result<(u64, usize)> {
    let mut shape = BorrowedShape::default();
    if let Err(error) = serde_json::to_writer(&mut shape, value) {
        if shape.exhausted {
            return Err(InputEvidenceSnapshotError::ResourceExhausted(
                "original Evidence import received source",
            )
            .into());
        }
        return Err(error.into());
    }
    let records = shape
        .nodes
        .checked_add(32)
        .and_then(|n| n.checked_mul(8))
        .context("original import received records overflow")?;
    let bytes = shape
        .bytes
        .checked_add(2048)
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add((records as u64).checked_mul(256)?))
        .context("original import received bytes overflow")?;
    ensure!(
        records <= MAX_RECORDS && bytes <= MAX_METADATA_BYTES as u64,
        InputEvidenceSnapshotError::ResourceExhausted("original Evidence import received source")
    );
    Ok((bytes, records))
}
pub(in crate::runtime_db) fn received_evidence_material(
    messages: &Vec<serde_json::Value>,
    transcripts: &Vec<TranscriptEntry>,
    tools: &Vec<ToolExecutionRecord>,
    briefs: &Vec<BriefRecord>,
    deliveries: &Vec<DeliverySummaryRecord>,
) -> Result<(u64, usize)> {
    let (mut bytes, mut records) =
        received_material(&(messages, transcripts, tools, briefs, deliveries))?;
    for extra in [
        received_vec_slack(messages)?,
        received_vec_slack(transcripts)?,
        received_vec_slack(tools)?,
        received_vec_slack(briefs)?,
        received_vec_slack(deliveries)?,
    ] {
        bytes = bytes
            .checked_add(extra.0)
            .context("original source Vec material overflow")?;
        records = received_joint_records(records, extra.1)?;
    }
    ensure!(
        bytes <= MAX_METADATA_BYTES as u64,
        InputEvidenceSnapshotError::ResourceExhausted("original Evidence import received source")
    );
    Ok((bytes, records))
}
fn received_vec_slack<T>(values: &Vec<T>) -> Result<(u64, usize)> {
    let slots = values
        .capacity()
        .checked_sub(values.len())
        .context("original source Vec capacity changed")?;
    let bytes = slots
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| n.checked_mul(8))
        .context("original source Vec capacity overflow")?;
    Ok((
        u64::try_from(bytes)?,
        slots
            .checked_mul(8)
            .context("original source Vec slots overflow")?,
    ))
}
pub(in crate::runtime_db) fn received_joint_records(left: usize, right: usize) -> Result<usize> {
    let total = left
        .checked_add(right)
        .context("original import received records overflow")?;
    ensure!(
        total <= MAX_RECORDS,
        InputEvidenceSnapshotError::ResourceExhausted("original Evidence import received source")
    );
    Ok(total)
}

pub(in crate::runtime_db) struct RowMaterial<'a> {
    scratch: &'a InputEvidenceBudgetLedger,
    guards: Vec<Arc<BudgetViewGuard>>,
    records: usize,
}
impl<'a> RowMaterial<'a> {
    fn new(scratch: &'a InputEvidenceBudgetLedger) -> Self {
        Self {
            scratch,
            guards: Vec::new(),
            records: 32,
        }
    }
    fn charge(&mut self, bytes: u64, records: usize) -> Result<()> {
        self.records = self
            .records
            .checked_add(records)
            .context("original source records overflow")?;
        ensure!(
            self.records <= MAX_RECORDS
                && bytes
                    .checked_add(self.scratch.report()?.observed_bytes)
                    .is_some_and(|n| n <= MAX_METADATA_BYTES as u64),
            InputEvidenceSnapshotError::ResourceExhausted("original episode source scratch")
        );
        self.guards.push(self.scratch.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?);
        Ok(())
    }
    fn payload(
        &mut self,
        tx: &Transaction<'_>,
        family: Family,
        rowid: i64,
    ) -> Result<(String, String, String)> {
        let table = family.table();
        let column = family.id_column();
        let raw:u64=tx.query_row(&format!("SELECT 2048+length(CAST(payload_json AS BLOB))+length(CAST({column} AS BLOB))+length(CAST(agent_id AS BLOB)) FROM {table} WHERE rowid=?1"),[rowid],|r|r.get(0))?;
        self.charge(
            raw.checked_mul(8).context("original source raw overflow")?,
            32,
        )?;
        let nodes: usize = tx.query_row(
            &format!(
                "SELECT COUNT(*)+32 FROM {table} r,json_tree(r.payload_json) WHERE r.rowid=?1"
            ),
            [rowid],
            |r| r.get(0),
        )?;
        let nodes = nodes
            .checked_mul(8)
            .context("original source native overflow")?;
        self.charge(
            (nodes as u64)
                .checked_mul(256)
                .context("original source native overflow")?,
            nodes,
        )?;
        tx.query_row(
            &format!("SELECT {column},agent_id,payload_json FROM {table} WHERE rowid=?1"),
            [rowid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(Into::into)
    }
}
#[derive(Clone, Copy)]
enum Family {
    Message,
    Transcript,
    Tool,
    Brief,
    Delivery,
}
impl Family {
    const ALL: [Self; 5] = [
        Self::Message,
        Self::Transcript,
        Self::Tool,
        Self::Brief,
        Self::Delivery,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Transcript => "transcript",
            Self::Tool => "tool",
            Self::Brief => "brief",
            Self::Delivery => "delivery",
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::Message => "messages",
            Self::Transcript => "transcript_entries",
            Self::Tool => "tool_executions",
            Self::Brief => "briefs",
            Self::Delivery => "delivery_summaries",
        }
    }
    fn id_column(self) -> &'static str {
        "evidence_id"
    }
    fn order(self) -> &'static str {
        match self {
            Self::Message => {
                "message_seq IS NULL DESC,message_seq ASC,created_at ASC,message_id ASC"
            }
            Self::Transcript => {
                "COALESCE(transcript_seq,9223372036854775807) ASC,created_at ASC,evidence_id ASC"
            }
            _ => "created_at ASC,evidence_id ASC",
        }
    }
}

fn identity_tx(
    tx: &Transaction<'_>,
    agent: &str,
    scratch: &InputEvidenceBudgetLedger,
) -> Result<Option<u64>> {
    let raw:Option<u64>=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+1024 FROM agent_identities WHERE agent_id=?1",[agent],|r|r.get(0)).optional()?;
    let Some(raw) = raw else { return Ok(None) };
    let mut material = RowMaterial::new(scratch);
    material.charge(
        raw.checked_mul(8)
            .context("original source identity raw overflow")?,
        16,
    )?;
    let nodes: usize = tx.query_row(
        "SELECT COUNT(*)+16 FROM agent_identities r,json_tree(r.payload_json) WHERE r.agent_id=?1",
        [agent],
        |r| r.get(0),
    )?;
    let records = nodes
        .checked_mul(8)
        .context("original source identity native overflow")?;
    material.charge(
        (records as u64)
            .checked_mul(256)
            .context("original source identity native overflow")?,
        records,
    )?;
    let payload: String = tx.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [agent],
        |r| r.get(0),
    )?;
    let identity = crate::runtime_db::repositories::decode_agent_identity_payload(&payload)?;
    ensure!(
        identity.agent_id == agent && identity.incarnation > 0,
        "original episode source identity differs"
    );
    let incarnation = identity.incarnation;
    drop(identity);
    drop(payload);
    Ok(Some(incarnation))
}
fn unbound_seed_tx(tx: &Transaction<'_>, agent: &str) -> Result<Option<String>> {
    // This address is bounded by the source header before its first String.
    let bytes:Option<u64>=tx.query_row("SELECT length(CAST(seed_id AS BLOB)) FROM episode_continuity_seeds WHERE agent_id=?1 AND identity_incarnation IS NULL",[agent],|r|r.get(0)).optional()?;
    ensure!(
        bytes.is_none_or(|n| n <= 4096),
        InputEvidenceSnapshotError::ResourceExhausted("original episode unbound seed")
    );
    tx.query_row("SELECT seed_id FROM episode_continuity_seeds WHERE agent_id=?1 AND identity_incarnation IS NULL",[agent],|r|r.get(0)).optional().map_err(Into::into)
}
fn validate_capture_tx(
    tx: &Transaction<'_>,
    source: &OriginalEpisodeImport<'_>,
    scratch: &InputEvidenceBudgetLedger,
) -> Result<()> {
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=?1 AND agent_id=?2 AND sealed=1 AND length(CAST(metadata_digest AS BLOB))=64 AND metadata_digest NOT GLOB '*[^0-9a-f]*')",params![COHORT,source.agent_id()],|r|r.get(0))?;
    ensure!(
        valid,
        "original episode source capture unavailable or foreign"
    );
    let empty: bool = tx.query_row(
        "SELECT empty_original FROM legacy_episode_import_captures WHERE capture_id=?1",
        [COHORT],
        |r| r.get(0),
    )?;
    if empty {
        // Empty carries no identity/seed grant. Later materialization or raw
        // writes cannot add a source member or change this original empty fact.
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures c WHERE c.capture_id=?1 AND c.identity_incarnation IS NULL AND c.seed_id IS NULL AND c.member_count=0 AND c.metadata_digest=?2 AND NOT EXISTS(SELECT 1 FROM legacy_episode_import_members m WHERE m.capture_id=c.capture_id))",
            params![COHORT, format!("{:x}", Sha256::digest(b""))],
            |r| r.get(0),
        )?;
        ensure!(valid, "original empty episode source changed");
        return Ok(());
    }
    let expected: Option<u64> = tx.query_row(
        "SELECT identity_incarnation FROM legacy_episode_import_captures WHERE capture_id=?1",
        [COHORT],
        |r| r.get(0),
    )?;
    ensure!(
        expected == identity_tx(tx, source.agent_id(), scratch)?,
        "original episode source incarnation changed"
    );
    if expected.is_none() {
        let same_seed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id WHERE c.capture_id=?1 AND s.agent_id=?2 AND s.identity_incarnation IS NULL)",params![COHORT,source.agent_id()],|r|r.get(0))?;
        ensure!(same_seed, "original episode unbound seed changed");
    }
    Ok(())
}
fn original_cohort_row_tx(
    tx: &Transaction<'_>,
    source: &OriginalEpisodeImport<'_>,
    family: Family,
    ordinal: u64,
    material: &mut RowMaterial<'_>,
) -> Result<(String, String, String)> {
    let source_raw:u64=tx.query_row("SELECT 1024+length(CAST(record_id AS BLOB))+length(CAST(raw_digest AS BLOB)) FROM legacy_episode_import_members WHERE capture_id=?1 AND family=?2 AND ordinal=?3",params![COHORT,family.name(),ordinal],|r|r.get(0))?;
    material.charge(
        source_raw
            .checked_mul(8)
            .context("original source locator overflow")?,
        32,
    )?;
    let (id,expected):(String,String)=tx.query_row("SELECT record_id,raw_digest FROM legacy_episode_import_members WHERE capture_id=?1 AND family=?2 AND ordinal=?3",params![COHORT,family.name(),ordinal],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let rowid: i64 = tx.query_row(
        &format!(
            "SELECT rowid FROM {} WHERE {}=?1 AND agent_id=?2",
            family.table(),
            family.id_column()
        ),
        params![id, source.agent_id()],
        |r| r.get(0),
    )?;
    let (actual_id, agent, payload) = material.payload(tx, family, rowid)?;
    ensure!(
        actual_id == id
            && agent == source.agent_id()
            && expected == format!("{:x}", Sha256::digest(payload.as_bytes())),
        "original episode source version changed"
    );
    Ok((actual_id, agent, payload))
}

impl RuntimeDb {
    #[cfg(test)]
    pub(crate) fn original_received_material_for_test(
        value: &impl Serialize,
    ) -> Result<(u64, usize)> {
        received_material(value)
    }
    /// Only StorageLegacyImporter can construct this original source witness.
    pub(crate) fn freeze_original_episode_import(
        &self,
        source: &OriginalEpisodeImport<'_>,
    ) -> Result<()> {
        let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
        self.transaction_once(|tx| {
            ensure!(!source.agent_id().is_empty(),"empty original episode source scope");
            let mut header=RowMaterial::new(&scratch);
            header.charge((source.agent_id().len() as u64+8192).checked_mul(8).context("original episode scope overflow")?,32)?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=?1)",[COHORT],|r|r.get(0))?;
            if exists {return validate_capture_tx(tx,source,&scratch)}
            // This capture is committed before the original imports. Failed
            // import retries reuse the same cohort and cannot acquire raw tails.
            let inc=identity_tx(tx,source.agent_id(),&scratch)?;
            let seed=if inc.is_none(){unbound_seed_tx(tx,source.agent_id())?}else{None};
            let empty_original=inc.is_none() && seed.is_none();
            if empty_original {
                // Fresh standalone bootstrap imports before it materializes an
                // identity. Only its actual empty source can cross that seam.
                for family in Family::ALL {
                    let present:bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {} WHERE agent_id=?1)",family.table()),[source.agent_id()],|r|r.get(0))?;
                    ensure!(!present,"original episode import has no original unbound seed");
                }
                // Wait is an independent input to the original derived Turn
                // actor; five empty evidence tables cannot authorize its rows.
                let waits:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wait_conditions WHERE agent_id=?1)",[source.agent_id()],|r|r.get(0))?;
                ensure!(!waits,"original episode import has no original unbound seed");
            }
            tx.execute("INSERT INTO legacy_episode_import_captures(capture_id,agent_id,identity_incarnation,seed_id,member_count,metadata_digest,sealed,empty_original) VALUES(?1,?2,?3,?4,0,'',0,?5)",params![COHORT,source.agent_id(),inc,seed,empty_original])?;
            let mut digest=Sha256::new();let mut total=0u64;
            for family in Family::ALL {
                if empty_original {break;}
                let mut statement=tx.prepare(&format!("SELECT rowid FROM {} WHERE agent_id=?1 ORDER BY {}",family.table(),family.order()))?;
                let mut rows=statement.query([source.agent_id()])?;
                let mut ordinal=0u64;
                while let Some(row)=rows.next()? {
                    let rowid:i64=row.get(0)?;
                    let mut material=RowMaterial::new(&scratch);
                    let (id,agent,payload)=material.payload(tx,family,rowid)?;
                    ensure!(agent==source.agent_id(),"original episode cohort changed scope");
                    let hash=format!("{:x}",Sha256::digest(payload.as_bytes()));
                    serde_json::to_writer(StreamDigest(&mut digest),&(family.name(),ordinal,&id,&agent,&hash))?;
                    tx.execute("INSERT INTO legacy_episode_import_members(capture_id,family,ordinal,record_id,raw_digest) VALUES(?1,?2,?3,?4,?5)",params![COHORT,family.name(),ordinal,id,hash])?;
                    total=total.checked_add(1).context("original episode source count overflow")?;
                    ordinal=ordinal.checked_add(1).context("original episode ordinal overflow")?;
                    drop(hash);drop(payload);drop(agent);drop(id);drop(material);
                }
            }
            let hash=format!("{:x}",digest.finalize());
            ensure!(tx.execute("UPDATE legacy_episode_import_captures SET member_count=?2,metadata_digest=?3,sealed=1 WHERE capture_id=?1 AND sealed=0",params![COHORT,total,hash])?==1,"original episode cohort did not seal");
            Ok(())
        })
    }
    #[cfg(test)]
    pub(crate) fn original_episode_import_is_empty(
        &self,
        source: &OriginalEpisodeImport<'_>,
    ) -> Result<bool> {
        let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
        self.transaction_once(|tx| {
            let mut header = RowMaterial::new(&scratch);
            header.charge(
                (source.agent_id().len() as u64 + 8192)
                    .checked_mul(8)
                    .context("original episode scope overflow")?,
                32,
            )?;
            validate_capture_tx(tx, source, &scratch)?;
            tx.query_row(
                "SELECT empty_original FROM legacy_episode_import_captures WHERE capture_id=?1",
                [COHORT],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
    }
    pub(crate) fn import_original_episode_evidence(
        &self,
        source: &OriginalEpisodeImport<'_>,
    ) -> Result<()> {
        if self.storage_domain_is_complete("evidence", "db")? {
            return Ok(());
        }
        let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
        self.import_original_episode_evidence_using_scratch(source, &scratch)
            .map(|_| ())
    }
    #[cfg(test)]
    pub(crate) fn import_original_episode_evidence_with_scratch_for_test(
        &self,
        source: &OriginalEpisodeImport<'_>,
        scratch: &InputEvidenceBudgetLedger,
    ) -> Result<u64> {
        self.import_original_episode_evidence_using_scratch(source, scratch)
    }
    fn import_original_episode_evidence_using_scratch(
        &self,
        source: &OriginalEpisodeImport<'_>,
        scratch: &InputEvidenceBudgetLedger,
    ) -> Result<u64> {
        if self.storage_domain_is_complete("evidence", "db")? {
            return Ok(0);
        }
        let mut header = RowMaterial::new(scratch);
        header.charge(
            (source.agent_id().len() as u64 + 8192)
                .checked_mul(8)
                .context("original import header overflow")?,
            32,
        )?;
        let mut peak = scratch.report()?.observed_bytes;
        self.begin_original_work_evidence_import()?;
        let result=self.transaction_once(|tx| {
            validate_capture_tx(tx,source,&scratch)?;
            let work=work_footprint::OriginalWorkImport::begin(tx,source)?;
            let mut counts=[0u64;5];let mut dropped=0u64;
            let mut delivery=Some(imports::LegacyDeliveryImport::begin_if_installed(tx,&scratch,header.records)?
                .context("original streaming Evidence requires installed source schema")?);
            for (index,family) in Family::ALL.into_iter().enumerate() {
                let mut statement=tx.prepare("SELECT ordinal FROM legacy_episode_import_members WHERE capture_id=?1 AND family=?2 ORDER BY ordinal")?;
                let mut rows=statement.query(params![COHORT,family.name()])?;
                while let Some(row)=rows.next()? {
                    let ordinal:u64=row.get(0)?;
                    let mut material=RowMaterial::new(&scratch);
                    material.records=received_joint_records(material.records,header.records)?;
                    let (actual_id,agent,payload)=original_cohort_row_tx(tx,source,family,ordinal,&mut material)?;
                    peak=peak.max(scratch.report()?.observed_bytes);
                    match family {
                        Family::Message=> {
                            let raw:serde_json::Value=serde_json::from_str(&payload)?;
                            if let Some(message)=normalize_legacy_message_value(raw)? {
                                ensure!(message.id==actual_id && message.agent_id==agent,"original Message projection differs");
                                insert_message_evidence_tx(tx,&message)?;
                                work.record(family,&message.id,crate::runtime_db::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite,&mut material)?;
                                counts[index]+=1;
                            } else {dropped+=1;}
                        }
                        Family::Transcript=> {
                            let record:TranscriptEntry=serde_json::from_str(&payload)?;
                            ensure!(record.id==actual_id && record.agent_id==agent,"original Transcript projection differs");
                            insert_transcript_evidence_tx(tx,&record)?;
                            work.record(family,&record.id,crate::runtime_db::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite,&mut material)?;
                            counts[index]+=1;
                        }
                        Family::Tool=> {
                            let record:ToolExecutionRecord=serde_json::from_str(&payload)?;
                            ensure!(record.id==actual_id && record.agent_id==agent,"original Tool projection differs");
                            insert_tool_evidence_tx(tx,&record)?;counts[index]+=1;
                        }
                        Family::Brief=> {
                            let record:BriefRecord=serde_json::from_str(&payload)?;
                            ensure!(record.id==actual_id && record.agent_id==agent,"original Brief projection differs");
                            insert_brief_evidence_tx(tx,&record)?;
                            work.record(family,&record.id,crate::runtime_db::episode_snapshot::work_checkpoint::imports::ImportOrigin::LegacyImportEqual,&mut material)?;
                            counts[index]+=1;
                        }
                        Family::Delivery=> {
                            let record:DeliverySummaryRecord=serde_json::from_str(&payload)?;
                            ensure!(record.id==actual_id && record.agent_id==agent,"original Delivery projection differs");
                            // Source row/locator/map and original kernel/readback
                            // remain under this same scratch allowance.
                            let original=delivery.take().context("original Delivery phase absent")?;
                            let original=original.with_prior_records(material.records)?;
                            original.append_original(&record)?;
                            #[cfg(test)] { peak=peak.max(original.peak_owned_bytes_for_test()); }
                            delivery=Some(original.with_prior_records(received_joint_records(32,header.records)?)?);counts[index]+=1;
                        }
                    }
                    drop(payload);drop(agent);drop(actual_id);drop(material);
                }
            }
            let legacy_delivery_source=delivery.context("original Delivery phase absent")?.finish()?;
            header.charge(2048*8+128*256,128)?;
            let checkpoint=serde_json::json!({"imported_messages":counts[0],"dropped_messages":dropped,
                "imported_transcript_entries":counts[1],"imported_tool_executions":counts[2],
                "imported_briefs":counts[3],"imported_delivery_summaries":counts[4],
                "legacy_delivery_source":legacy_delivery_source,"original_episode_cohort":COHORT});
            peak=peak.max(work.complete(&checkpoint,&mut header)?);
            Ok(())
        });
        if let Err(error) = result {
            self.fail_original_work_evidence_import(&error)?;
            return Err(error).with_context(|| "importing legacy storage domain evidence");
        }
        Ok(peak)
    }
}

#[path = "original_turn_import.rs"]
mod original_turns;

#[path = "original_work_import.rs"]
mod work_footprint;

#[path = "received_turn_import.rs"]
pub(in crate::runtime_db) mod received_turns;
