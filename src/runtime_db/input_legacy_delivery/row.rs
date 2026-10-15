//! One bounded original row, shared by the actual cutover and import actors.
use super::*;
use crate::runtime::input_evidence_budget::BudgetViewGuard;

pub(super) struct OriginalDeliveryRow {
    pub(super) id: String,
    pub(super) agent: String,
    pub(super) work: String,
    pub(super) at: String,
    pub(super) inc: Option<u64>,
    pub(super) seed: Option<String>,
    pub(super) nonempty: bool,
    pub(super) fingerprint_json: String,
    pub(super) content: String,
    // Fields above drop before their actual owned observations.
    _raw: Arc<BudgetViewGuard>,
    _native: Arc<BudgetViewGuard>,
}
pub(super) fn read_original_row_tx(
    tx: &Transaction<'_>,
    rowid: i64,
    scratch: &InputEvidenceBudgetLedger,
    prior_records: usize,
) -> Result<OriginalDeliveryRow> {
    let identities: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_identities')",
        [],
        |row| row.get(0),
    )?;
    let identity_payload = if identities {
        "(SELECT i.payload_json FROM agent_identities i WHERE i.agent_id=r.agent_id)"
    } else {
        "NULL"
    };
    let identity_bytes = if identities {
        "COALESCE((SELECT length(CAST(i.payload_json AS BLOB))+length(CAST(i.agent_id AS BLOB)) FROM agent_identities i WHERE i.agent_id=r.agent_id),0)"
    } else {
        "0"
    };
    let identity_nodes = if identities {
        "(SELECT COUNT(*) FROM agent_identities i,json_tree(i.payload_json) WHERE i.agent_id=r.agent_id)"
    } else {
        "0"
    };
    let raw:u64=tx.query_row(&format!("SELECT 2048+{identity_bytes}+length(CAST(payload_json AS BLOB))+length(CAST(evidence_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(created_at AS BLOB))+COALESCE(length(CAST(turn_id AS BLOB)),0)+COALESCE(length(CAST(message_id AS BLOB)),0)+COALESCE(length(CAST(task_id AS BLOB)),0)+COALESCE(length(CAST(work_item_id AS BLOB)),0)+COALESCE(length(CAST(kind AS BLOB)),0)+COALESCE(length(CAST(preview AS BLOB)),0)+COALESCE(length(CAST(content_hash AS BLOB)),0)+COALESCE((SELECT length(CAST(s.seed_id AS BLOB)) FROM episode_continuity_seeds s WHERE s.agent_id=r.agent_id),0) FROM delivery_summaries r WHERE rowid=?1"),[rowid],|r|r.get(0))?;
    let raw_bytes = raw
        .checked_mul(8)
        .context("LegacyDelivery scratch raw overflow")?;
    ensure!(
        raw_bytes
            .checked_add(scratch.report()?.observed_bytes)
            .is_some_and(|n| n <= MAX_METADATA_BYTES as u64),
        InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery fixed raw")
    );
    let raw_guard = scratch.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: raw_bytes,
            metadata_estimate_bytes: 0,
        },
    )?;
    let nodes:u64=tx.query_row(&format!("SELECT COUNT(*)+64+{identity_nodes} FROM delivery_summaries r,json_tree(r.payload_json) WHERE r.rowid=?1"),[rowid],|r|r.get(0))?;
    let records = nodes
        .checked_mul(8)
        .context("LegacyDelivery scratch node overflow")?;
    let native = records
        .checked_mul(256)
        .context("LegacyDelivery scratch bytes overflow")?;
    ensure!(
        records
            .checked_add(u64::try_from(prior_records)?)
            .is_some_and(|n| n <= MAX_RECORDS as u64)
            && native
                .checked_add(scratch.report()?.observed_bytes)
                .is_some_and(|n| n <= MAX_METADATA_BYTES as u64),
        InputEvidenceSnapshotError::ResourceExhausted("LegacyDelivery fixed native")
    );
    let native_guard = scratch.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: native,
        },
    )?;
    let (id,agent,work,at,kind,message,task,payload,identity_raw,seed):(String,String,String,String,String,Option<String>,Option<String>,String,Option<String>,Option<String>)=tx.query_row(
        &format!("SELECT r.evidence_id,r.agent_id,r.work_item_id,r.created_at,r.kind,r.message_id,r.task_id,r.payload_json,{identity_payload},(SELECT s.seed_id FROM episode_continuity_seeds s WHERE s.agent_id=r.agent_id AND s.identity_incarnation IS NULL) FROM delivery_summaries r WHERE rowid=?1"),
        [rowid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
    let inc = identity_raw
        .as_deref()
        .map(|raw| -> Result<u64> {
            let identity = crate::runtime_db::repositories::decode_agent_identity_payload(raw)?;
            ensure!(
                identity.agent_id == agent && identity.incarnation > 0,
                "LegacyDelivery original identity projection differs"
            );
            Ok(identity.incarnation)
        })
        .transpose()?;
    ensure!(
        inc.is_none_or(|n| n > 0)
            && kind == "delivery_summary"
            && message.is_none()
            && task.is_none(),
        "LegacyDelivery original normalized projection differs"
    );
    let actual: DeliverySummaryRecord = serde_json::from_str(&payload)?;
    ensure!(
        actual.id == id
            && actual.agent_id == agent
            && actual.work_item_id == work
            && crate::runtime_db::repositories::timestamp(actual.created_at) == at,
        "LegacyDelivery original payload address/time differs"
    );
    let key = Key::new(Family::LegacyDelivery, &id);
    let mut fingerprint =
        fingerprint_on(tx, &agent, &key)?.context("LegacyDelivery fixed row disappeared")?;
    ensure!(
        actual.turn_id.as_deref() == fingerprint.related_turn_id.as_deref(),
        "LegacyDelivery original Turn projection differs"
    );
    let content = bytes_digest(payload.as_bytes());
    if let Some(hash) = fingerprint.content_hash.as_deref() {
        ensure!(
            normalized_payload_digest(hash)? == content,
            "LegacyDelivery original body/hash differs"
        );
    }
    fingerprint.frozen_payload_digest = Some(content.clone());
    let fingerprint_json = serde_json::to_string(&fingerprint)?;
    let seed = if inc.is_none() { seed } else { None };
    let nonempty = !actual.text.is_empty();
    drop(actual);
    drop(payload);
    drop(identity_raw);
    drop(fingerprint);
    drop(kind);
    drop(message);
    drop(task);
    Ok(OriginalDeliveryRow {
        id,
        agent,
        work,
        at,
        inc,
        seed,
        nonempty,
        fingerprint_json,
        content,
        _raw: raw_guard,
        _native: native_guard,
    })
}
