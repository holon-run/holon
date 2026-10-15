//! Bounded cold discovery of an actual identity scope, before body acquisition.
use super::*;

pub(crate) struct ColdResourceIdentityLocator {
    pub scope: InputEvidenceScope,
    pub status: AgentRegistryStatus,
    pub usage: (usize, usize),
    pub guards: Vec<Arc<BudgetViewGuard>>,
}

pub(crate) fn read_cold_resource_locator_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<ColdResourceIdentityLocator>> {
    let byte_limit = request.byte_limit.min(MAX_BYTES);
    let record_limit = request.record_limit.min(MAX_RECORDS);
    let mut bytes = 0usize;
    let mut records = 0usize;
    let mut guards = Vec::new();
    let mut charge = |raw: usize, nodes: usize| -> Result<()> {
        let added = raw
            .checked_mul(8)
            .context("resource locator byte overflow")?;
        let count = nodes
            .checked_mul(8)
            .context("resource locator record overflow")?;
        let next_bytes = bytes
            .checked_add(added)
            .context("resource locator byte overflow")?;
        let next_records = records
            .checked_add(count)
            .context("resource locator record overflow")?;
        ensure!(
            next_bytes <= byte_limit && next_records <= record_limit,
            SchedulingSnapshotError::ResourceExhausted("cold resource identity locator")
        );
        guards.push(ledger.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(added)?,
            },
        )?);
        bytes = next_bytes;
        records = next_records;
        Ok(())
    };
    // Covers the actual scope/owner selector copies, guard slots and a possible
    // default display State. It asserts no State or domain readiness.
    charge(
        usize::try_from(serialized_bytes(&agent_id)?)?
            .checked_add(8192)
            .context("resource locator selector overflow")?,
        128,
    )?;
    let raw: Option<usize> = tx
        .query_row(
            "SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+
                length(CAST(payload_json AS BLOB))+512
         FROM agent_identities WHERE agent_id=?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else { return Ok(None) };
    charge(raw, 1)?;
    // Raw budget precedes SQLite JSON traversal; native nodes precede either
    // a scope String or the later canonical identity DTO decoder.
    let nodes: usize = tx.query_row(
        "SELECT COUNT(*) FROM json_tree((SELECT payload_json FROM agent_identities WHERE agent_id=?1))",
        [agent_id], |row| row.get(0),
    )?;
    charge(
        nodes
            .checked_mul(256)
            .context("resource locator native bytes overflow")?,
        nodes,
    )?;
    let (same_id, status, incarnation): (bool, u8, Option<u64>) = tx.query_row(
        "SELECT agent_id IS ?1,
                CASE status WHEN 'active' THEN 0 WHEN 'deleting' THEN 1
                     WHEN 'deleted' THEN 2 WHEN 'archived' THEN 2 ELSE 3 END,
                CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1
                     WHEN json_type(payload_json,'$.incarnation')='integer'
                          AND json_extract(payload_json,'$.incarnation')>0
                     THEN json_extract(payload_json,'$.incarnation') END
         FROM agent_identities WHERE agent_id=?1",
        [agent_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    ensure!(
        same_id && incarnation.is_some(),
        "cold resource identity locator differs"
    );
    let status = match status {
        0 => AgentRegistryStatus::Active,
        1 => AgentRegistryStatus::Deleting,
        2 => AgentRegistryStatus::Deleted,
        _ => anyhow::bail!("cold resource normalized identity status is invalid"),
    };
    drop(charge);
    Ok(Some(ColdResourceIdentityLocator {
        scope: InputEvidenceScope {
            agent_id: agent_id.to_owned(),
            identity_incarnation: incarnation.context("resource identity incarnation missing")?,
        },
        status,
        usage: (bytes, records),
        guards,
    }))
}
