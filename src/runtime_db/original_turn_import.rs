//! Original Turn import scratch and the separate normalized Wait source.
//! Full relations stay in SQL; only admitted finite metadata enters Rust.
use super::*;
use crate::runtime_db::repositories::{decode_wait_condition_row, enum_string};
use crate::types::{TurnRecord, WaitConditionRecord};

const WAITS: &str = "original_episode_waits/v1";
const WAIT_COLUMNS: &str = "wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,last_turn_id,trigger_message_id,triggered_at,wake_sources_json,continuation_json";

fn normalized_wait_tx(
    tx: &Transaction<'_>,
    rowid: i64,
    material: &mut RowMaterial<'_>,
) -> Result<WaitConditionRecord> {
    // Original storage's Wait reader uses these columns, not payload_json.
    let raw:u64=tx.query_row("SELECT 4096+length(CAST(wait_condition_id AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(work_item_id AS BLOB)),0)+length(CAST(status AS BLOB))+length(CAST(kind AS BLOB))+COALESCE(length(CAST(source AS BLOB)),0)+COALESCE(length(CAST(subject_ref AS BLOB)),0)+length(CAST(waiting_for AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+COALESCE(length(CAST(expires_at AS BLOB)),0)+COALESCE(length(CAST(resolved_at AS BLOB)),0)+COALESCE(length(CAST(cancelled_at AS BLOB)),0)+COALESCE(length(CAST(last_turn_id AS BLOB)),0)+COALESCE(length(CAST(trigger_message_id AS BLOB)),0)+COALESCE(length(CAST(triggered_at AS BLOB)),0)+length(CAST(wake_sources_json AS BLOB))+COALESCE(length(CAST(continuation_json AS BLOB)),0) FROM wait_conditions WHERE rowid=?1",[rowid],|r|r.get(0))?;
    material.charge(
        raw.checked_mul(8).context("original Wait raw overflow")?,
        48,
    )?;
    let nodes:usize=tx.query_row("SELECT 48+(SELECT COUNT(*) FROM json_tree(w.wake_sources_json))+(SELECT COUNT(*) FROM json_tree(w.continuation_json)) FROM wait_conditions w WHERE rowid=?1",[rowid],|r|r.get(0))?;
    let records = nodes
        .checked_mul(8)
        .context("original Wait native overflow")?;
    material.charge(
        (records as u64)
            .checked_mul(256)
            .context("original Wait native overflow")?,
        records,
    )?;
    let mut query = tx.prepare(&format!(
        "SELECT {WAIT_COLUMNS} FROM wait_conditions WHERE rowid=?1"
    ))?;
    let mut rows = query.query([rowid])?;
    decode_wait_condition_row(rows.next()?.context("original Wait row disappeared")?)
}
impl RuntimeDb {
    pub(crate) fn freeze_original_turn_wait_source(
        &self,
        source: &OriginalEpisodeImport<'_>,
    ) -> Result<()> {
        let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
        self.transaction_once(|tx| {
            let mut header=RowMaterial::new(&scratch);
            header.charge((source.agent_id().len() as u64+8192).checked_mul(8).context("original Wait source scope overflow")?,32)?;
            validate_capture_tx(tx,source,&scratch)?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=?1)",[WAITS],|r|r.get(0))?;
            if exists {
                let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=?1 AND source_capture_id=?2 AND sealed=1 AND length(CAST(metadata_digest AS BLOB))=64 AND metadata_digest NOT GLOB '*[^0-9a-f]*')",params![WAITS,COHORT],|r|r.get(0))?;
                ensure!(valid,"original Wait source unavailable or foreign");
                let empty:bool=tx.query_row("SELECT empty_original FROM legacy_episode_import_captures WHERE capture_id=?1",[COHORT],|r|r.get(0))?;
                if empty {
                    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_wait_captures c WHERE c.capture_id=?1 AND c.member_count=0 AND c.metadata_digest=?2 AND NOT EXISTS(SELECT 1 FROM legacy_episode_wait_members m WHERE m.capture_id=c.capture_id))",params![WAITS,format!("{:x}",Sha256::digest(b""))],|r|r.get(0))?;
                    ensure!(valid,"original empty Wait source changed");
                }
                return Ok(())
            }
            tx.execute("INSERT INTO legacy_episode_wait_captures(capture_id,source_capture_id,member_count,metadata_digest,sealed) VALUES(?1,?2,0,'',0)",params![WAITS,COHORT])?;
            let empty:bool=tx.query_row("SELECT empty_original FROM legacy_episode_import_captures WHERE capture_id=?1",[COHORT],|r|r.get(0))?;
            let mut statement=tx.prepare("SELECT rowid FROM wait_conditions WHERE agent_id=?1 AND ?2=0 ORDER BY updated_at ASC,created_at ASC,wait_condition_id DESC")?;
            let mut rows=statement.query(params![source.agent_id(),empty])?;
            let mut digest=Sha256::new();let mut ordinal=0u64;
            while let Some(row)=rows.next()? {
                let mut material=RowMaterial::new(&scratch);
                material.records=received_joint_records(material.records,header.records)?;
                let record=normalized_wait_tx(tx,row.get(0)?,&mut material)?;
                ensure!(record.agent_id==source.agent_id(),"original Wait source changed scope");
                let mut body=Sha256::new();serde_json::to_writer(StreamDigest(&mut body),&record)?;
                let hash=format!("{:x}",body.finalize());
                serde_json::to_writer(StreamDigest(&mut digest),&(ordinal,&record.id,&record.agent_id,&hash))?;
                tx.execute("INSERT INTO legacy_episode_wait_members(capture_id,ordinal,record_id,normalized_digest) VALUES(?1,?2,?3,?4)",params![WAITS,ordinal,record.id,hash])?;
                ordinal=ordinal.checked_add(1).context("original Wait source count overflow")?;
                drop(hash);drop(record);drop(material);
            }
            let hash=format!("{:x}",digest.finalize());
            ensure!(tx.execute("UPDATE legacy_episode_wait_captures SET member_count=?2,metadata_digest=?3,sealed=1 WHERE capture_id=?1 AND sealed=0",params![WAITS,ordinal,hash])?==1,"original Wait source did not seal");
            Ok(())
        })
    }
}

// These tables exist only for the duration of the original Turn business TX.
// Full durable links stay in SQL; finite metadata is the only Rust Turn DTO.
const TURN_SCRATCH_SCHEMA: &str = r#"
CREATE TEMP TABLE original_turn_import_metadata (
 turn_id TEXT PRIMARY KEY,
 metadata_json TEXT NOT NULL,
 before_json TEXT,
 desired_json TEXT
);
CREATE TEMP TABLE original_turn_import_links (
 turn_id TEXT NOT NULL,
 family TEXT NOT NULL CHECK(family IN ('input_message_ids','tool_execution_ids','produced_brief_ids','delivery_summary_ids','completed_work_item_ids','waiting_condition_ids')),
 record_id TEXT NOT NULL,
 PRIMARY KEY(turn_id,family,record_id)
);
"#;

/// Only a consumed original same-TX staged source may authorize this row write.
/// No generic repository/public caller can construct it.
struct OriginalDerivedTurnWrite<'tx, 'conn, 'source, 'ledger> {
    tx: &'tx Transaction<'conn>,
    source: &'source OriginalEpisodeImport<'source>,
    metadata: TurnRecord,
    before_present: bool,
    material: RowMaterial<'ledger>,
}

fn read_staged_metadata_tx(
    tx: &Transaction<'_>,
    turn_id: &str,
    material: &mut RowMaterial<'_>,
) -> Result<Option<TurnRecord>> {
    let raw:Option<u64>=tx.query_row("SELECT 2048+length(CAST(turn_id AS BLOB))+length(CAST(metadata_json AS BLOB)) FROM original_turn_import_metadata WHERE turn_id=?1",[turn_id],|r|r.get(0)).optional()?;
    let Some(raw) = raw else { return Ok(None) };
    material.charge(
        raw.checked_mul(8)
            .context("original Turn metadata raw overflow")?,
        32,
    )?;
    let nodes:usize=tx.query_row("SELECT COUNT(*)+32 FROM original_turn_import_metadata r,json_tree(r.metadata_json) WHERE r.turn_id=?1",[turn_id],|r|r.get(0))?;
    let records = nodes
        .checked_mul(8)
        .context("original Turn metadata native overflow")?;
    material.charge(
        (records as u64)
            .checked_mul(256)
            .context("original Turn metadata native overflow")?,
        records,
    )?;
    let payload: String = tx.query_row(
        "SELECT metadata_json FROM original_turn_import_metadata WHERE turn_id=?1",
        [turn_id],
        |r| r.get(0),
    )?;
    let record: TurnRecord = serde_json::from_str(&payload)?;
    ensure!(
        record.turn_id == turn_id
            && record.input_message_ids.is_empty()
            && record.tool_execution_ids.is_empty()
            && record.produced_brief_ids.is_empty()
            && record.delivery_summary_ids.is_empty()
            && record.completed_work_item_ids.is_empty()
            && record.waiting_condition_ids.is_empty(),
        "original Turn metadata contains an unstaged body relation"
    );
    Ok(Some(record))
}
fn write_staged_metadata_tx(
    tx: &Transaction<'_>,
    record: &TurnRecord,
    material: &mut RowMaterial<'_>,
) -> Result<()> {
    let cost = received_material(record)?;
    material.charge(cost.0, cost.1)?;
    let payload = serde_json::to_string(record)?;
    tx.execute("INSERT INTO original_turn_import_metadata(turn_id,metadata_json,before_json) VALUES(?1,?2,NULL) ON CONFLICT(turn_id) DO UPDATE SET metadata_json=excluded.metadata_json",params![record.turn_id,payload])?;
    Ok(())
}
fn stage_original_source_tx(
    tx: &Transaction<'_>,
    material: &mut RowMaterial<'_>,
    agent: &str,
    turn_id: &str,
    turn_index: u64,
    created_at: chrono::DateTime<chrono::Utc>,
    work: Option<&str>,
    trigger: Option<crate::types::TurnTriggerSummary>,
    links: &[(&str, &str)],
) -> Result<()> {
    let mut record = read_staged_metadata_tx(tx, turn_id, material)?
        .unwrap_or_else(|| TurnRecord::new(agent, turn_id, turn_index));
    ensure!(
        record.agent_id == agent,
        "original Turn source changed agent"
    );
    if record.turn_index == 0 && turn_index != 0 {
        record.turn_index = turn_index;
    }
    record.created_at = record.created_at.min(created_at);
    if record.current_work_item_id.is_none() {
        record.current_work_item_id = work.map(str::to_owned);
    }
    if record.trigger.is_none() {
        record.trigger = trigger;
    }
    write_staged_metadata_tx(tx, &record, material)?;
    for (family, id) in links {
        tx.execute("INSERT OR IGNORE INTO original_turn_import_links(turn_id,family,record_id) VALUES(?1,?2,?3)",params![turn_id,family,id])?;
    }
    Ok(())
}

fn stage_cohort_source_row_tx(
    tx: &Transaction<'_>,
    source: &OriginalEpisodeImport<'_>,
    family: Family,
    ordinal: u64,
    material: &mut RowMaterial<'_>,
) -> Result<()> {
    use crate::runtime_db::repositories::{turn_key_from_message, turn_key_from_optional};
    let (id, agent, payload) = original_cohort_row_tx(tx, source, family, ordinal, material)?;
    match family {
        Family::Message => {
            if let Some(message) = normalize_legacy_message_value(serde_json::from_str(&payload)?)?
            {
                ensure!(
                    message.id == id && message.agent_id == agent,
                    "original Turn Message projection differs"
                );
                let key = turn_key_from_message(&message);
                let (turn, index) = key.parts();
                stage_original_source_tx(
                    tx,
                    material,
                    &agent,
                    turn,
                    index,
                    message.created_at,
                    message.work_item_id.as_deref(),
                    Some(crate::types::TurnTriggerSummary::from_message(&message)),
                    &[("input_message_ids", &message.id)],
                )?;
            }
        }
        Family::Tool => {
            let record: ToolExecutionRecord = serde_json::from_str(&payload)?;
            ensure!(
                record.id == id && record.agent_id == agent,
                "original Turn Tool projection differs"
            );
            if let Some(key) = turn_key_from_optional(record.turn_id.as_deref(), record.turn_index)
            {
                let (turn, index) = key.parts();
                stage_original_source_tx(
                    tx,
                    material,
                    &agent,
                    turn,
                    index,
                    record.created_at,
                    record.work_item_id.as_deref(),
                    None,
                    &[("tool_execution_ids", &record.id)],
                )?;
            }
        }
        Family::Brief => {
            let record: BriefRecord = serde_json::from_str(&payload)?;
            ensure!(
                record.id == id && record.agent_id == agent,
                "original Turn Brief projection differs"
            );
            if let Some(key) = turn_key_from_optional(
                record.turn_id.as_deref(),
                record.turn_index.unwrap_or_default(),
            ) {
                let (turn, index) = key.parts();
                stage_original_source_tx(
                    tx,
                    material,
                    &agent,
                    turn,
                    index,
                    record.created_at,
                    record.work_item_id.as_deref(),
                    None,
                    &[("produced_brief_ids", &record.id)],
                )?;
            }
        }
        Family::Delivery => {
            let record: DeliverySummaryRecord = serde_json::from_str(&payload)?;
            ensure!(
                record.id == id && record.agent_id == agent,
                "original Turn Delivery projection differs"
            );
            if let Some(key) = turn_key_from_optional(
                record.turn_id.as_deref(),
                record.source_turn_index.unwrap_or_default(),
            ) {
                let (turn, index) = key.parts();
                stage_original_source_tx(
                    tx,
                    material,
                    &agent,
                    turn,
                    index,
                    record.created_at,
                    Some(&record.work_item_id),
                    None,
                    &[
                        ("delivery_summary_ids", &record.id),
                        ("completed_work_item_ids", &record.work_item_id),
                    ],
                )?;
            }
        }
        Family::Transcript => anyhow::bail!("Transcript is not an original derived Turn source"),
    }
    Ok(())
}

fn stage_original_wait_tx(
    tx: &Transaction<'_>,
    source: &OriginalEpisodeImport<'_>,
    ordinal: u64,
    material: &mut RowMaterial<'_>,
) -> Result<()> {
    let raw:u64=tx.query_row("SELECT 1024+length(CAST(record_id AS BLOB))+length(CAST(normalized_digest AS BLOB)) FROM legacy_episode_wait_members WHERE capture_id=?1 AND ordinal=?2",params![WAITS,ordinal],|r|r.get(0))?;
    material.charge(
        raw.checked_mul(8)
            .context("original Wait locator overflow")?,
        32,
    )?;
    let (id,expected):(String,String)=tx.query_row("SELECT record_id,normalized_digest FROM legacy_episode_wait_members WHERE capture_id=?1 AND ordinal=?2",params![WAITS,ordinal],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let rowid: i64 = tx.query_row(
        "SELECT rowid FROM wait_conditions WHERE wait_condition_id=?1 AND agent_id=?2",
        params![id, source.agent_id()],
        |r| r.get(0),
    )?;
    let record = normalized_wait_tx(tx, rowid, material)?;
    let mut body = Sha256::new();
    serde_json::to_writer(StreamDigest(&mut body), &record)?;
    ensure!(
        record.id == id
            && record.agent_id == source.agent_id()
            && expected == format!("{:x}", body.finalize()),
        "original Wait source version changed"
    );
    if let Some(turn) = record.turn_id.as_deref().filter(|s| !s.trim().is_empty()) {
        stage_original_source_tx(
            tx,
            material,
            &record.agent_id,
            turn.trim(),
            0,
            record.created_at,
            record.work_item_id.as_deref(),
            None,
            &[("waiting_condition_ids", &record.id)],
        )?;
    }
    Ok(())
}

const ID_FIELDS: [&str; 6] = [
    "input_message_ids",
    "tool_execution_ids",
    "produced_brief_ids",
    "delivery_summary_ids",
    "completed_work_item_ids",
    "waiting_condition_ids",
];
const METADATA_JSON:&str="json_remove(payload_json,'$.input_message_ids','$.tool_execution_ids','$.produced_brief_ids','$.delivery_summary_ids','$.completed_work_item_ids','$.waiting_condition_ids')";

fn merge_existing_turn_tx(
    tx: &Transaction<'_>,
    source: &OriginalEpisodeImport<'_>,
    turn_id: &str,
    material: &mut RowMaterial<'_>,
) -> Result<(TurnRecord, bool)> {
    let derived = read_staged_metadata_tx(tx, turn_id, material)?
        .context("original derived Turn is absent")?;
    ensure!(
        derived.agent_id == source.agent_id(),
        "original derived Turn changed scope"
    );
    let raw:Option<u64>=tx.query_row("SELECT 4096+length(CAST(payload_json AS BLOB))+length(CAST(turn_id AS BLOB))+length(CAST(agent_id AS BLOB)) FROM turn_records WHERE turn_id=?1",[turn_id],|r|r.get(0)).optional()?;
    let Some(raw) = raw else {
        return Ok((derived, false));
    };
    material.charge(
        raw.checked_mul(8)
            .context("original existing Turn raw overflow")?,
        32,
    )?;
    // Each whole array stays in SQL. Invalid shapes retain the original strict
    // Vec<String> decoder behavior without allocating the complete collection.
    for field in ID_FIELDS {
        let valid:bool=tx.query_row(&format!("SELECT (json_type(payload_json,'$.{field}') IS NULL OR json_type(payload_json,'$.{field}')='array') AND NOT EXISTS(SELECT 1 FROM json_each(payload_json,'$.{field}') WHERE type!='text') FROM turn_records WHERE turn_id=?1"),[turn_id],|r|r.get(0))?;
        ensure!(valid, "original existing Turn ID array is not text");
    }
    let metadata_bytes: u64 = tx.query_row(
        &format!(
            "SELECT 2048+length(CAST({METADATA_JSON} AS BLOB)) FROM turn_records WHERE turn_id=?1"
        ),
        [turn_id],
        |r| r.get(0),
    )?;
    material.charge(
        metadata_bytes
            .checked_mul(8)
            .context("original existing Turn metadata raw overflow")?,
        32,
    )?;
    let nodes: usize = tx.query_row(
        &format!(
            "SELECT COUNT(*)+32 FROM turn_records r,json_tree({METADATA_JSON}) WHERE r.turn_id=?1"
        ),
        [turn_id],
        |r| r.get(0),
    )?;
    let records = nodes
        .checked_mul(8)
        .context("original existing Turn metadata native overflow")?;
    material.charge(
        (records as u64)
            .checked_mul(256)
            .context("original existing Turn metadata native overflow")?,
        records,
    )?;
    let payload: String = tx.query_row(
        &format!("SELECT {METADATA_JSON} FROM turn_records WHERE turn_id=?1"),
        [turn_id],
        |r| r.get(0),
    )?;
    let existing: TurnRecord = serde_json::from_str(&payload)?;
    ensure!(
        existing.turn_id == turn_id && existing.agent_id == derived.agent_id,
        "original existing Turn normalized identity differs"
    );
    // The original merge preserves every existing metadata field. Only links
    // are unioned; no incoming clock/trigger/owner replaces canonical metadata.
    for field in ID_FIELDS {
        tx.execute(&format!("INSERT OR IGNORE INTO original_turn_import_links(turn_id,family,record_id) SELECT turn_id,?2,value FROM turn_records,json_each(payload_json,'$.{field}') WHERE turn_id=?1"),params![turn_id,field])?;
    }
    write_staged_metadata_tx(tx, &existing, material)?;
    tx.execute("UPDATE original_turn_import_metadata SET before_json=(SELECT payload_json FROM turn_records WHERE turn_id=?1) WHERE turn_id=?1",[turn_id])?;
    Ok((existing, true))
}

#[derive(serde::Serialize)]
struct TurnPrefix<'a> {
    turn_id: &'a str,
    turn_index: u64,
    agent_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_work_item_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<&'a crate::types::TurnOwner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger: Option<&'a crate::types::TurnTriggerSummary>,
}
#[derive(serde::Serialize)]
struct TurnSuffix<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal: Option<&'a crate::types::TurnTerminalSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replay: Option<&'a crate::types::TurnReplayProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    destructive_operation_id: Option<&'a str>,
    external_side_effect_possible: bool,
    created_at: chrono::DateTime<chrono::Utc>,
}
fn finite_parts(record: &TurnRecord) -> (TurnPrefix<'_>, TurnSuffix<'_>) {
    (
        TurnPrefix {
            turn_id: &record.turn_id,
            turn_index: record.turn_index,
            agent_id: &record.agent_id,
            run_id: record.run_id.as_deref(),
            current_work_item_id: record.current_work_item_id.as_deref(),
            owner: record.owner.as_ref(),
            trigger: record.trigger.as_ref(),
        },
        TurnSuffix {
            terminal: record.terminal.as_ref(),
            replay: record.replay.as_ref(),
            destructive_operation_id: record.destructive_operation_id.as_deref(),
            external_side_effect_possible: record.external_side_effect_possible,
            created_at: record.created_at,
        },
    )
}

impl<'tx, 'conn, 'source, 'ledger> OriginalDerivedTurnWrite<'tx, 'conn, 'source, 'ledger> {
    fn prepare(
        tx: &'tx Transaction<'conn>,
        source: &'source OriginalEpisodeImport<'source>,
        turn: &str,
        scratch: &'ledger InputEvidenceBudgetLedger,
        prior_records: usize,
    ) -> Result<Self> {
        let mut material = RowMaterial::new(scratch);
        material.records = received_joint_records(material.records, prior_records)?;
        material.charge(
            (turn.len() as u64 + source.agent_id().len() as u64 + 4096)
                .checked_mul(8)
                .context("original Turn proof address overflow")?,
            32,
        )?;
        let (metadata, before_present) = merge_existing_turn_tx(tx, source, turn, &mut material)?;
        if let Some(owner) = metadata.owner.as_ref() {
            let expected = owner.work_item_id();
            if metadata.current_work_item_id.as_deref() != expected {
                return Err(crate::runtime_db::RuntimeStateTransitionConflict::new(
                    "turn owner",
                    &metadata.turn_id,
                    expected.unwrap_or("unbound"),
                    metadata
                        .current_work_item_id
                        .as_deref()
                        .unwrap_or("unbound"),
                )
                .into());
            }
        }
        ensure!(
            metadata.agent_id == source.agent_id(),
            "original Turn write proof changed scope"
        );
        if before_present {
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn_records t JOIN original_turn_import_metadata s ON s.turn_id=t.turn_id WHERE t.turn_id=?1 AND t.payload_json=s.before_json AND t.agent_id=?2 AND t.turn_index=?3 AND t.run_id IS ?4 AND t.current_work_item_id IS ?5 AND t.created_at=?6 AND t.trigger_message_id IS ?7)",params![metadata.turn_id,metadata.agent_id,metadata.turn_index as i64,metadata.run_id,metadata.current_work_item_id,crate::runtime_db::migrations::timestamp(metadata.created_at),metadata.trigger.as_ref().and_then(|t|t.message_id.as_deref())],|r|r.get(0))?;
            ensure!(valid, "original Turn before projection differs");
        }
        Ok(Self {
            tx,
            source,
            metadata,
            before_present,
            material,
        })
    }
    fn commit(mut self) -> Result<u64> {
        // The original actor validated incarnation in this same writer TX.
        // Its finite footprint never changes identity; recheck the immutable
        // source address here without allocating another identity view.
        let source_valid:bool=self.tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=?1 AND agent_id=?2 AND sealed=1)",params![COHORT,self.source.agent_id()],|r|r.get(0))?;
        ensure!(source_valid, "original Turn source proof changed scope");
        let (prefix, suffix) = finite_parts(&self.metadata);
        let cost = received_material(&(&prefix, &suffix))?;
        self.material.charge(cost.0, cost.1)?;
        let prefix = serde_json::to_string(&prefix)?;
        let suffix = serde_json::to_string(&suffix)?;
        // Only finite parts enter Rust. Arrays and the final body stay in SQL;
        // fields are inserted at their original Serde position, not json_set's
        // append position. Empty arrays retain skip_serializing_if semantics.
        let mut expression = String::from("substr(?2,1,length(?2)-1)");
        for field in ID_FIELDS {
            expression.push_str(&format!("||CASE WHEN EXISTS(SELECT 1 FROM original_turn_import_links WHERE turn_id=?1 AND family='{field}') THEN ',\"{field}\":'||(SELECT json_group_array(record_id) FROM (SELECT record_id FROM original_turn_import_links WHERE turn_id=?1 AND family='{field}' ORDER BY record_id COLLATE BINARY)) ELSE '' END"));
        }
        expression.push_str("||','||substr(?3,2)");
        self.tx.execute(&format!("UPDATE original_turn_import_metadata SET desired_json={expression} WHERE turn_id=?1"),params![self.metadata.turn_id,prefix,suffix])?;
        let before_matches: bool = if self.before_present {
            self.tx.query_row("SELECT EXISTS(SELECT 1 FROM turn_records t JOIN original_turn_import_metadata s ON s.turn_id=t.turn_id WHERE t.turn_id=?1 AND t.payload_json=s.before_json)",[&self.metadata.turn_id],|r|r.get(0))?
        } else {
            !self.tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM turn_records WHERE turn_id=?1)",
                [&self.metadata.turn_id],
                |r| r.get::<_, bool>(0),
            )?
        };
        ensure!(before_matches, "original Turn exact before changed");
        let changed:bool=self.tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM turn_records t JOIN original_turn_import_metadata s ON s.turn_id=t.turn_id WHERE t.turn_id=?1 AND t.payload_json=s.desired_json)",[&self.metadata.turn_id],|r|r.get(0))?;
        let (owner_kind, owner_id) = match self.metadata.owner.as_ref() {
            Some(owner) => {
                let (kind, id) = owner.index_parts();
                (Some(kind), id)
            }
            None => (None, None),
        };
        let terminal_kind = self
            .metadata
            .terminal
            .as_ref()
            .map(|terminal| enum_string(&terminal.kind))
            .transpose()?;
        let completed_at = self
            .metadata
            .terminal
            .as_ref()
            .map(|terminal| crate::runtime_db::migrations::timestamp(terminal.completed_at));
        let affected=self.tx.execute("INSERT INTO turn_records(turn_id,turn_index,agent_id,run_id,current_work_item_id,owner_kind,owner_id,trigger_message_id,replay_source_turn_id,terminal_kind,created_at,completed_at,payload_json) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,desired_json FROM original_turn_import_metadata WHERE turn_id=?1 ON CONFLICT(turn_id) DO UPDATE SET owner_kind=COALESCE(turn_records.owner_kind,excluded.owner_kind),owner_id=COALESCE(turn_records.owner_id,excluded.owner_id),replay_source_turn_id=excluded.replay_source_turn_id,terminal_kind=excluded.terminal_kind,completed_at=excluded.completed_at,payload_json=excluded.payload_json WHERE COALESCE(excluded.completed_at,excluded.created_at)>=COALESCE(turn_records.completed_at,turn_records.created_at)",params![self.metadata.turn_id,self.metadata.turn_index as i64,self.metadata.agent_id,self.metadata.run_id,self.metadata.current_work_item_id,owner_kind,owner_id,self.metadata.trigger.as_ref().and_then(|t|t.message_id.as_deref()),self.metadata.replay.as_ref().map(|r|r.source_turn_id.as_str()),terminal_kind,crate::runtime_db::migrations::timestamp(self.metadata.created_at),completed_at])?;
        ensure!(affected == 1, "original Turn proved write was not applied");
        crate::runtime_db::repositories::finish_original_turn_revisions_tx(
            self.tx,
            &self.metadata,
            changed,
        )?;
        let mut peak = self.material.scratch.report()?.observed_bytes;
        let mut statement=self.tx.prepare("SELECT rowid FROM original_turn_import_links WHERE turn_id=?1 AND family='input_message_ids' ORDER BY record_id COLLATE BINARY")?;
        let mut rows = statement.query([&self.metadata.turn_id])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let mut id_material = RowMaterial::new(self.material.scratch);
            id_material.records =
                received_joint_records(id_material.records, self.material.records)?;
            let bytes:u64=self.tx.query_row("SELECT 2048+length(CAST(record_id AS BLOB))+COALESCE((SELECT length(CAST(a.message_id AS BLOB))+length(CAST(a.agent_id AS BLOB))+length(CAST(a.turn_id AS BLOB)) FROM conversation_input_assignments a WHERE a.message_id=l.record_id),0)+COALESCE((SELECT length(CAST(s.agent_id AS BLOB))+COALESCE(length(CAST(s.turn_id AS BLOB)),0) FROM conversation_source_revisions s WHERE s.source_kind='operator' AND s.source_id=l.record_id),0) FROM original_turn_import_links l WHERE rowid=?1",[rowid],|r|r.get(0))?;
            id_material.charge(
                bytes
                    .checked_mul(8)
                    .context("original input assignment material overflow")?,
                64,
            )?;
            let id: String = self.tx.query_row(
                "SELECT record_id FROM original_turn_import_links WHERE rowid=?1",
                [rowid],
                |r| r.get(0),
            )?;
            crate::runtime_db::repositories::assign_original_turn_input_tx(
                self.tx,
                &self.metadata,
                &id,
            )?;
            peak = peak.max(self.material.scratch.report()?.observed_bytes);
            drop(id);
            drop(id_material);
        }
        Ok(peak)
    }
}

impl RuntimeDb {
    pub(crate) fn import_original_turn_records(
        &self,
        source: &OriginalEpisodeImport<'_>,
    ) -> Result<()> {
        if self.storage_domain_is_complete("turn_records", "db")? {
            return Ok(());
        }
        let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
        self.import_original_turn_records_using_scratch(source, &scratch)
            .map(|_| ())
    }
    #[cfg(test)]
    pub(crate) fn import_original_turn_records_with_scratch_for_test(
        &self,
        source: &OriginalEpisodeImport<'_>,
        scratch: &InputEvidenceBudgetLedger,
    ) -> Result<u64> {
        self.import_original_turn_records_using_scratch(source, scratch)
    }
    fn import_original_turn_records_using_scratch(
        &self,
        source: &OriginalEpisodeImport<'_>,
        scratch: &InputEvidenceBudgetLedger,
    ) -> Result<u64> {
        if self.storage_domain_is_complete("turn_records", "db")? {
            return Ok(0);
        }
        let mut header = RowMaterial::new(scratch);
        header.charge(
            (source.agent_id().len() as u64 + 8192)
                .checked_mul(8)
                .context("original Turn import scope overflow")?,
            32,
        )?;
        let mut peak = scratch.report()?.observed_bytes;
        let result=self.run_storage_domain_import("turn_records","jsonl-derived","db",|tx| {
            validate_capture_tx(tx,source,scratch)?;
            let waits_valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=?1 AND source_capture_id=?2 AND sealed=1 AND length(CAST(metadata_digest AS BLOB))=64 AND metadata_digest NOT GLOB '*[^0-9a-f]*')",params![WAITS,COHORT],|r|r.get(0))?;
            ensure!(waits_valid,"original Turn Wait cohort unavailable");
            tx.execute_batch(TURN_SCRATCH_SCHEMA)?;
            for family in [Family::Message,Family::Tool,Family::Brief,Family::Delivery] {
                let mut statement=tx.prepare("SELECT ordinal FROM legacy_episode_import_members WHERE capture_id=?1 AND family=?2 ORDER BY ordinal")?;
                let mut rows=statement.query(params![COHORT,family.name()])?;
                while let Some(row)=rows.next()? {
                    let mut material=RowMaterial::new(scratch);
                    material.records=received_joint_records(material.records,header.records)?;
                    stage_cohort_source_row_tx(tx,source,family,row.get(0)?,&mut material)?;
                    peak=peak.max(scratch.report()?.observed_bytes);
                }
            }
            {
                let mut statement=tx.prepare("SELECT ordinal FROM legacy_episode_wait_members WHERE capture_id=?1 ORDER BY ordinal")?;
                let mut rows=statement.query([WAITS])?;
                while let Some(row)=rows.next()? {
                    let mut material=RowMaterial::new(scratch);
                    material.records=received_joint_records(material.records,header.records)?;
                    stage_original_wait_tx(tx,source,row.get(0)?,&mut material)?;
                    peak=peak.max(scratch.report()?.observed_bytes);
                }
            }
            let mut imported=0u64;
            {
                let mut statement=tx.prepare("SELECT rowid FROM original_turn_import_metadata ORDER BY turn_id COLLATE BINARY")?;
                let mut rows=statement.query([])?;
                while let Some(row)=rows.next()? {
                    let rowid:i64=row.get(0)?;let mut id_material=RowMaterial::new(scratch);
                    id_material.records=received_joint_records(id_material.records,header.records)?;
                    let bytes:u64=tx.query_row("SELECT 1024+length(CAST(turn_id AS BLOB)) FROM original_turn_import_metadata WHERE rowid=?1",[rowid],|r|r.get(0))?;
                    id_material.charge(bytes.checked_mul(8).context("original Turn selected ID overflow")?,32)?;
                    let turn:String=tx.query_row("SELECT turn_id FROM original_turn_import_metadata WHERE rowid=?1",[rowid],|r|r.get(0))?;
                    let proof=OriginalDerivedTurnWrite::prepare(tx,source,&turn,scratch,id_material.records)?;
                    peak=peak.max(scratch.report()?.observed_bytes);
                    peak=peak.max(proof.commit()?);
                    imported=imported.checked_add(1).context("original Turn import count overflow")?;
                    drop(turn);drop(id_material);
                }
            }
            tx.execute_batch("DROP TABLE original_turn_import_links;DROP TABLE original_turn_import_metadata;")?;
            header.charge(2048*8+128*256,128)?;
            peak=peak.max(scratch.report()?.observed_bytes);
            Ok(serde_json::json!({"imported_records":imported,"source":"legacy evidence jsonl","ignored":"turns.jsonl"}))
        });
        result.map(|_| peak)
    }
}
