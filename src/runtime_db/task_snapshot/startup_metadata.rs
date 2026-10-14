//! Exclusive cold startup may invalidate derived metadata, never grant a body.
use super::*;
use crate::{
    domain::execution_protocol::ExecutionProtocolCommand,
    runtime::input_evidence_budget::{
        BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
    },
    runtime_db::{
        input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery,
        transitions::StartupInputRecoveryScope,
    },
};

const AFFECTED: &str = "lifecycle_state='open' AND EXISTS(SELECT 1 FROM task_result_settlements r WHERE r.agent_id=execution_protocol_attempts.agent_id AND r.activation_id=execution_protocol_attempts.attempt_id AND r.state='caller_admitted')";
const ATTEMPT: &str = "json_array(agent_id,attempt_id,lifecycle_state,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.attempt_id'),json_extract(payload_json,'$.state'),json_extract(payload_json,'$.source_message_id'),json_extract(payload_json,'$.source.generation'))";
struct ScopeMetadata {
    scope: InputEvidenceScope,
    revision: u64,
    before: BTreeMap<String, serde_json::Value>,
    bytes: usize,
    records: usize,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
/// Borrows the real global startup transaction and its exclusive lease/handoff.
/// It carries no State/body/write-intent and cannot authorize any Task row.
pub(crate) struct StartupTaskMetadataProof<'tx, 'connection, 'capability> {
    tx: &'tx Transaction<'connection>,
    capability: &'tx ExclusiveStartupInputRecovery<'capability>,
    ledger: &'tx InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
    scopes: Vec<ScopeMetadata>,
}
fn charge(
    scope: &mut ScopeMetadata,
    bytes: usize,
    records: usize,
    ledger: &InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
) -> Result<()> {
    let bytes = bytes
        .checked_mul(4)
        .context("startup Task metadata byte overflow")?;
    let records = records
        .checked_mul(4)
        .context("startup Task metadata count overflow")?;
    scope.bytes = scope
        .bytes
        .checked_add(bytes)
        .context("startup Task metadata byte overflow")?;
    scope.records = scope
        .records
        .checked_add(records)
        .context("startup Task metadata count overflow")?;
    bound(
        scope.records,
        scope.bytes,
        Some(&TaskDomainLoadRequest {
            query: Default::default(),
            capture: TaskCaptureRequest {
                include_active: false,
                include_unsettled: false,
                terminal_limit: 0,
                required_keys: BTreeSet::new(),
            },
            record_limit,
            byte_limit,
        }),
    )?;
    scope._guards.push(ledger.register_owned_bytes(
        &scope.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(bytes)?,
        },
    )?);
    Ok(())
}
fn validate_identity(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<()> {
    let (agent, status, payload_agent, payload_status, incarnation): (String,String,String,String,Option<u64>) = tx.query_row(
        "SELECT agent_id,status,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.status'),CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') ELSE NULL END FROM agent_identities WHERE agent_id=?1",
        [&scope.agent_id], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
    ensure!(
        agent == scope.agent_id
            && payload_agent == agent
            && status == payload_status
            && matches!(status.as_str(), "active" | "deleting")
            && incarnation == Some(scope.identity_incarnation)
            && scope.identity_incarnation > 0,
        "exclusive Task metadata identity/incarnation changed"
    );
    Ok(())
}
fn revision(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<Option<u64>> {
    let row:Option<(u64,u64)>=tx.query_row("SELECT revision,json_extract(coverage_json,'$.revision') FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    row.map(|(normalized, payload)| {
        ensure!(
            normalized > 0 && normalized == payload,
            "startup Task coverage revision differs"
        );
        Ok(normalized)
    })
    .transpose()
}
fn validate_attempt(scope: &InputEvidenceScope, id: &str, value: &serde_json::Value) -> Result<()> {
    let fields = value
        .as_array()
        .context("startup Task attempt tuple is not an array")?;
    ensure!(
        fields.len() == 8
            && fields[0].as_str() == Some(scope.agent_id.as_str())
            && fields[1].as_str() == Some(id)
            && fields[3] == fields[0]
            && fields[4] == fields[1]
            && fields[2] == fields[5]
            && fields[7].as_u64().is_some_and(|n| n > 0),
        "startup Task actual Attempt projection differs"
    );
    Ok(())
}

pub(crate) fn prepare_exclusive_startup_task_metadata_tx<'tx, 'connection, 'capability>(
    tx: &'tx Transaction<'connection>,
    capability: &'tx ExclusiveStartupInputRecovery<'capability>,
    authorities: &[StartupInputRecoveryScope],
    ledger: &'tx InputEvidenceBudgetLedger,
) -> Result<StartupTaskMetadataProof<'tx, 'connection, 'capability>> {
    capability.validate_transaction(tx)?;
    let limits = ledger.limits();
    let byte_limit = usize::try_from(
        limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("startup Task metadata limits overflow")?,
    )?
    .min(MAX_BYTES);
    let record_limit = usize::try_from(limits.record_limit)?.min(MAX_RECORDS);
    let mut scopes = Vec::new();
    for authority in authorities {
        let scope = match authority {
            StartupInputRecoveryScope::Current(scope) => scope.clone(),
            StartupInputRecoveryScope::Retired {
                agent_id,
                identity_incarnation,
                status: AgentRegistryStatus::Deleting,
            } => InputEvidenceScope {
                agent_id: agent_id.clone(),
                identity_incarnation: *identity_incarnation,
            },
            // Unbound has no Task current namespace; unchanged Task/Result bytes
            // require no recovery overlay. Deleted never accesses a successor.
            _ => continue,
        };
        let coverage_bytes:Option<usize>=tx.query_row("SELECT length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation],|r|r.get(0)).optional()?;
        let Some(coverage_bytes) = coverage_bytes else {
            continue;
        };
        // The old coverage is never read into Rust. Enforce its original hard
        // structural ceiling, without reserving copies that do not exist.
        bound(1, coverage_bytes, None)?;
        let mut observed = ScopeMetadata {
            scope,
            revision: 0,
            before: BTreeMap::new(),
            bytes: 0,
            records: 0,
            _guards: Vec::new(),
        };
        let identity_bytes:usize=tx.query_row("SELECT COALESCE(length(CAST(agent_id AS BLOB)),0)+COALESCE(length(CAST(status AS BLOB)),0)+COALESCE(length(CAST(json_extract(payload_json,'$.agent_id') AS BLOB)),0)+COALESCE(length(CAST(json_extract(payload_json,'$.status') AS BLOB)),0) FROM agent_identities WHERE agent_id=?1",
            [&observed.scope.agent_id],|r|r.get(0))?;
        let address_bytes = identity_bytes
            .checked_add(observed.scope.agent_id.len())
            .and_then(|n| n.checked_add(512))
            .context("startup Task metadata overflow")?;
        charge(
            &mut observed,
            address_bytes,
            2,
            ledger,
            byte_limit,
            record_limit,
        )?;
        validate_identity(tx, &observed.scope)?;
        observed.revision =
            revision(tx, &observed.scope)?.context("startup Task coverage disappeared")?;
        let (count,bytes):(usize,usize)=tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(length(CAST({ATTEMPT} AS BLOB))+128),0) FROM execution_protocol_attempts WHERE agent_id=?1 AND {AFFECTED}"),
            [&observed.scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        charge(
            &mut observed,
            bytes,
            count,
            ledger,
            byte_limit,
            record_limit,
        )?;
        let mut query=tx.prepare(&format!("SELECT attempt_id,{ATTEMPT} FROM execution_protocol_attempts WHERE agent_id=?1 AND {AFFECTED} ORDER BY attempt_id"))?;
        for row in query.query_map([&observed.scope.agent_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (id, raw) = row?;
            let value = serde_json::from_str::<serde_json::Value>(&raw)?;
            validate_attempt(&observed.scope, &id, &value)?;
            ensure!(
                value[2].as_str() == Some("open"),
                "startup Task original Attempt is not open"
            );
            ensure!(
                observed.before.insert(id, value).is_none(),
                "duplicate original startup Attempt"
            );
        }
        if !observed.before.is_empty() {
            scopes.push(observed);
        }
    }
    Ok(StartupTaskMetadataProof {
        tx,
        capability,
        ledger,
        byte_limit,
        record_limit,
        scopes,
    })
}
impl StartupTaskMetadataProof<'_, '_, '_> {
    /// Consume only actual canonical recovery commands already ACKed in this Tx.
    /// Sparse invalidation is not a Task write key or a complete empty domain.
    pub(crate) fn finish(
        mut self,
        tx: &Transaction<'_>,
        actual: &BTreeMap<String, Vec<ExecutionProtocolCommand>>,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "startup Task metadata proof crossed transaction"
        );
        self.capability.validate_transaction(tx)?;
        for scope in &mut self.scopes {
            let mut changed = false;
            if let Some(commands) = actual.get(&scope.scope.agent_id) {
                for command in commands {
                    let id = match command {
                        ExecutionProtocolCommand::Interrupt(c) => c.attempt_id.as_str(),
                        ExecutionProtocolCommand::Settle(c) => c.outcome.attempt_id.as_str(),
                        ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(c) => {
                            c.attempt_id.as_str()
                        }
                        ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(c) => {
                            c.attempt_id.as_str()
                        }
                        _ => continue,
                    };
                    // The global canonical ACK may also contain unrelated
                    // attempts. Only the original observed Result frontier is affected.
                    if !scope.before.contains_key(id) {
                        continue;
                    }
                    let bytes:usize=tx.query_row(&format!("SELECT length(CAST({ATTEMPT} AS BLOB))+128 FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2"),params![scope.scope.agent_id,id],|r|r.get(0))?;
                    charge(
                        scope,
                        bytes,
                        1,
                        self.ledger,
                        self.byte_limit,
                        self.record_limit,
                    )?;
                    let original = &scope.before[id];
                    let raw:String=tx.query_row(&format!("SELECT {ATTEMPT} FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2"),params![scope.scope.agent_id,id],|r|r.get(0))?;
                    let after: serde_json::Value = serde_json::from_str(&raw)?;
                    validate_attempt(&scope.scope, id, &after)?;
                    ensure!(
                        matches!(after[2].as_str(), Some("settled" | "interrupted"))
                            && original[6] == after[6]
                            && original[7] == after[7],
                        "startup Task recovery did not terminate its original source Attempt"
                    );
                    changed |= original != &after;
                }
            }
            if changed {
                validate_identity(tx, &scope.scope)?;
                ensure!(
                    revision(tx, &scope.scope)? == Some(scope.revision),
                    "startup Task coverage changed after preparation"
                );
                let next = scope
                    .revision
                    .checked_add(1)
                    .context("startup Task revision overflow")?;
                loader::persist_coverage(tx, &scope.scope, &writes::empty_coverage(next))?;
            }
        }
        Ok(())
    }
}
