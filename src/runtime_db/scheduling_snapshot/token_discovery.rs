//! Capability routing discovery; the scoped producer still performs admission.
use super::*;
use crate::types::{CallbackDeliveryMode, ExternalTriggerRecord, ExternalTriggerStatus};
use serde_json::Value;

pub(crate) struct ExternalTriggerTokenLocator {
    pub(crate) target_agent_id: String,
    pub(crate) external_trigger_id: String,
    pub(crate) delivery_mode: CallbackDeliveryMode,
    _material: Vec<Arc<BudgetViewGuard>>,
}
pub(crate) struct ExternalTriggerTokenRecordRead {
    record: ExternalTriggerRecord,
    _locator: ExternalTriggerTokenLocator,
}
impl ExternalTriggerTokenRecordRead {
    pub(crate) fn record(&self) -> &ExternalTriggerRecord {
        &self.record
    }
    // Compatibility DTO return boundary; production ingress uses only locator.
    pub(crate) fn into_record(self) -> ExternalTriggerRecord {
        self.record
    }
}

const SELECTED: &str =
    "SELECT * FROM external_triggers WHERE token_hash=?1 AND status='active' LIMIT 1";
struct TokenMaterial<'a> {
    request: &'a SchedulingDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl TokenMaterial<'_> {
    fn charge(&mut self, bytes: usize, records: usize) -> Result<()> {
        let exhausted =
            || SchedulingSnapshotError::ResourceExhausted("callback token discovery material");
        let bytes = bytes.checked_mul(8).context(exhausted())?;
        let records = records.checked_mul(8).context(exhausted())?;
        self.bytes = self.bytes.checked_add(bytes).context(exhausted())?;
        self.records = self.records.checked_add(records).context(exhausted())?;
        bound(self.records, self.bytes, self.request)?;
        self.guards.push(self.ledger.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(bytes)?,
                metadata_estimate_bytes: 0,
            },
        )?);
        Ok(())
    }
}
fn prepare_locator_tx(
    tx: &Transaction<'_>,
    token_hash: &str,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<ExternalTriggerTokenLocator>> {
    ensure!(
        token_hash.len() == 64 && token_hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "callback token hash malformed"
    );
    let mut material = TokenMaterial {
        request,
        ledger,
        bytes: 0,
        records: 0,
        guards: Vec::new(),
    };
    // Integer-only lengths precede JSON parsing, String and locator allocation.
    let (count, bytes):(usize,usize)=tx.query_row(&format!(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(external_trigger_id AS BLOB))+length(CAST(target_agent_id AS BLOB))+COALESCE(length(CAST(trigger_url AS BLOB)),0)+length(CAST(token_hash AS BLOB))+length(CAST(status AS BLOB))+length(CAST(created_at AS BLOB))+COALESCE(length(CAST(revoked_at AS BLOB)),0)+COALESCE(length(CAST(last_delivered_at AS BLOB)),0)+512),0) FROM ({SELECTED})"),
        [token_hash],|row|Ok((row.get(0)?,row.get(1)?)))?;
    ensure!(count <= 1, "callback token selected cardinality changed");
    if count == 0 {
        return Ok(None);
    }
    material.charge(bytes, 1)?;
    // SELECTED preserves the original LIMIT 1 branch, including duplicate hashes.
    let nodes: usize = tx.query_row(
        &format!("SELECT COUNT(*) FROM ({SELECTED}) c,json_tree(c.payload_json)"),
        [token_hash],
        |row| row.get(0),
    )?;
    material.charge(
        nodes
            .checked_mul(std::mem::size_of::<Value>())
            .context("callback node bytes overflow")?,
        nodes,
    )?;
    let (id,agent,status,hash,body_id,body_agent,body_status,body_hash,mode):
        (String,String,String,String,String,String,String,String,String)=tx.query_row(&format!(
        "SELECT external_trigger_id,target_agent_id,status,token_hash,json_extract(payload_json,'$.external_trigger_id'),json_extract(payload_json,'$.target_agent_id'),json_extract(payload_json,'$.status'),json_extract(payload_json,'$.token_hash'),json_extract(payload_json,'$.delivery_mode') FROM ({SELECTED})"),
        [token_hash],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)))?;
    ensure!(
        !id.is_empty()
            && !agent.is_empty()
            && status == "active"
            && hash == token_hash
            && body_id == id
            && body_agent == agent
            && body_status == status
            && body_hash == hash,
        "callback discovery normalized/body address differs"
    );
    let delivery_mode = match mode.as_str() {
        "enqueue_message" => CallbackDeliveryMode::EnqueueMessage,
        "wake_hint" => CallbackDeliveryMode::WakeHint,
        _ => anyhow::bail!("callback discovery delivery mode malformed"),
    };
    Ok(Some(ExternalTriggerTokenLocator {
        target_agent_id: agent,
        external_trigger_id: id,
        delivery_mode,
        _material: material.guards,
    }))
}
pub(crate) fn discover_external_trigger_token_tx(
    tx: &Transaction<'_>,
    token_hash: &str,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<ExternalTriggerTokenLocator>> {
    prepare_locator_tx(tx, token_hash, request, ledger)
}
/// Existing public Record API remains a bounded, explicitly named DTO adapter.
pub(crate) fn read_external_trigger_token_compatibility_tx(
    tx: &Transaction<'_>,
    token_hash: &str,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<ExternalTriggerTokenRecordRead>> {
    let Some(locator) = prepare_locator_tx(tx, token_hash, request, ledger)? else {
        return Ok(None);
    };
    let (raw,id,agent,token,hash,status,created,revoked,delivered,count):
        (String,String,String,Option<String>,String,String,String,Option<String>,Option<String>,u64)=tx.query_row(
        "SELECT payload_json,external_trigger_id,target_agent_id,trigger_url,token_hash,status,created_at,revoked_at,last_delivered_at,delivery_count FROM external_triggers WHERE external_trigger_id=?1",
        [&locator.external_trigger_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)))?;
    let record = repositories::decode_external_trigger_payload(&raw)?;
    ensure!(
        record.external_trigger_id == id
            && id == locator.external_trigger_id
            && record.target_agent_id == agent
            && agent == locator.target_agent_id
            && record.token == token
            && record.token_hash == hash
            && hash == token_hash
            && status == "active"
            && record.status == ExternalTriggerStatus::Active
            && record.delivery_mode == locator.delivery_mode
            && record.delivery_count == count
            && repositories::timestamp(record.created_at) == created
            && record.revoked_at.map(repositories::timestamp) == revoked
            && record.last_delivered_at.map(repositories::timestamp) == delivered,
        "callback compatibility normalized/body record differs"
    );
    Ok(Some(ExternalTriggerTokenRecordRead {
        record,
        _locator: locator,
    }))
}
