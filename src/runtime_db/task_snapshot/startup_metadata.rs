//! Exclusive startup invalidates metadata and ACKs its qualified Result writes.
use super::*;
use crate::{
    domain::execution_protocol::ExecutionProtocolCommand,
    runtime::input_evidence_budget::{
        BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
    },
    runtime_db::{
        input_evidence_snapshot::legacy_recovery::{
            ExclusiveStartupInputRecovery, LegacySeedRecoveryScope,
        },
        transitions::StartupInputRecoveryScope,
    },
};
use chrono::{DateTime, Utc};

const AFFECTED: &str = "lifecycle_state IN ('open','interrupted') AND EXISTS(SELECT 1 FROM task_result_settlements r WHERE r.agent_id=execution_protocol_attempts.agent_id AND r.activation_id=execution_protocol_attempts.attempt_id AND r.state='caller_admitted')";
const ATTEMPT: &str = "json_array(agent_id,attempt_id,lifecycle_state,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.attempt_id'),json_extract(payload_json,'$.state'),json_extract(payload_json,'$.source_message_id'),json_extract(payload_json,'$.source.generation'))";
struct ScopeMetadata {
    scope: InputEvidenceScope,
    unbound: Option<LegacySeedRecoveryScope>,
    revision: u64,
    before: BTreeMap<String, serde_json::Value>,
    bytes: usize,
    records: usize,
    canonical_changed: bool,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
/// Borrows the real global startup transaction and its exclusive lease/handoff.
/// Result write authority still requires the qualified original before-version
/// and the actual native writer ACK. Metadata observation alone grants nothing.
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
    reserve(
        scope,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(bytes)?,
        },
        records,
        ledger,
        byte_limit,
        record_limit,
    )
}
fn reserve(
    scope: &mut ScopeMetadata,
    estimate: OwnedBytesEstimate,
    records: usize,
    ledger: &InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
) -> Result<()> {
    let bytes = usize::try_from(
        estimate
            .payload_bytes
            .checked_add(estimate.metadata_estimate_bytes)
            .context("startup Task owned bytes overflow")?,
    )?;
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
            recovery_at: None,
            query: Default::default(),
            capture: TaskCaptureRequest {
                include_active: false,
                include_unsettled: false,
                terminal_limit: 0,
                recovery_source_ids: BTreeSet::new(),
                required_keys: BTreeSet::new(),
            },
            record_limit,
            byte_limit,
        }),
    )?;
    scope._guards.push(ledger.register_owned_bytes(
        &scope.scope,
        InputMemoryCategory::Candidate,
        estimate,
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
    accounting: Option<&[InputEvidenceScope]>,
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
        if let StartupInputRecoveryScope::Unbound(seed) = authority {
            let account = accounting
                .and_then(|scopes| scopes.iter().find(|s| s.agent_id == seed.agent_id))
                .context(
                    "unbound startup Task material requires its held accounting reservation",
                )?;
            // This address charges the original host reservation only. The
            // fixed seed/capability below remains the sole unbound authority.
            scopes.push(ScopeMetadata {
                scope: account.clone(),
                unbound: Some(seed.clone()),
                revision: 0,
                before: BTreeMap::new(),
                bytes: 0,
                records: 0,
                canonical_changed: false,
                _guards: Vec::new(),
            });
            continue;
        }
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
            // Deleted never accesses a successor.
            _ => continue,
        };
        let coverage_bytes:Option<usize>=tx.query_row("SELECT length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation],|r|r.get(0)).optional()?;
        // The old coverage is never read into Rust. Enforce its original hard
        // structural ceiling, without reserving copies that do not exist.
        if let Some(bytes) = coverage_bytes {
            bound(1, bytes, None)?;
        }
        let mut observed = ScopeMetadata {
            scope,
            unbound: None,
            revision: 0,
            before: BTreeMap::new(),
            bytes: 0,
            records: 0,
            canonical_changed: false,
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
        observed.revision = revision(tx, &observed.scope)?.unwrap_or(1);
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
                matches!(value[2].as_str(), Some("open" | "interrupted")),
                "startup Task original Attempt is not recoverable"
            );
            ensure!(
                observed.before.insert(id, value).is_none(),
                "duplicate original startup Attempt"
            );
        }
        scopes.push(observed);
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
    pub(crate) fn settle_finalized_activation(
        &mut self,
        tx: &Transaction<'_>,
        agent: &str,
        activation: &str,
        disposition: super::super::task_result_settlement::TaskResultSettlementDisposition,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        use super::super::task_result_settlement as results;
        ensure!(
            std::ptr::eq(tx, self.tx),
            "startup Result crossed transaction"
        );
        self.capability.validate_transaction(tx)?;
        let Some(scope) = self.scopes.iter_mut().find(|s| s.scope.agent_id == agent) else {
            return results::settle_activation_tx(tx, agent, activation, disposition, now);
        };
        if let Some(seed) = &scope.unbound {
            ensure!(super::super::input_evidence_snapshot::legacy_recovery::legacy_seed_recovery_scope_tx(tx, agent)? == *seed,
                "unbound startup Result seed changed");
        } else {
            validate_identity(tx, &scope.scope)?;
        }
        let retained = (scope.bytes, scope.records, scope._guards.len());
        let result = (|| {
            let limits = results::TaskResultWriteLimits {
                record_limit: self.record_limit,
                byte_limit: self.byte_limit,
            };
            let selection = results::select_settle_activation_tx(
                tx,
                agent,
                activation,
                disposition,
                now,
                limits,
            )?;
            let estimate = selection.material_estimate();
            // The native inventory contains only addresses/lengths. Reserve all
            // original, typed and post-write copies before either body decoder.
            reserve(
                scope,
                OwnedBytesEstimate {
                    payload_bytes: u64::try_from(estimate.payload_bytes)?,
                    metadata_estimate_bytes: u64::try_from(estimate.metadata_estimate_bytes)?,
                },
                estimate
                    .records
                    .checked_mul(4)
                    .context("startup Result count overflow")?,
                self.ledger,
                self.byte_limit,
                self.record_limit,
            )?;
            let prepared = selection.materialize(limits)?;
            let candidates = prepared
                .candidate_result_ids()
                .iter()
                .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
                .collect();
            if let Some(seed) = scope.unbound.clone() {
                let material =
                    seed_maintenance::unbound_task_maintenance_material_tx(tx, &seed, &candidates)?;
                reserve(
                    scope,
                    OwnedBytesEstimate {
                        payload_bytes: u64::try_from(material.bytes)?,
                        metadata_estimate_bytes: 0,
                    },
                    material.records,
                    self.ledger,
                    self.byte_limit,
                    self.record_limit,
                )?;
                let qualified = seed_maintenance::qualified_unbound_task_maintenance_keys_tx(
                    tx,
                    self.capability,
                    &seed,
                    &candidates,
                )?;
                let proof = seed_maintenance::prepare_unbound_task_maintenance_tx(
                    tx,
                    self.capability,
                    &seed,
                    &qualified,
                )?;
                let ack = prepared.execute()?;
                ensure!(
                    ack.actual_result_ids
                        .iter()
                        .all(|id| candidates
                            .contains(&TaskKey::new(TaskFamily::ResultSettlement, id))),
                    "unbound startup Result ACK exceeds original predicate"
                );
                let actual = ack
                    .actual_result_ids
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
                    .filter(|key| qualified.contains(key))
                    .collect();
                let version =
                    seed_maintenance::register_unbound_task_maintenance_tx(tx, proof, &actual)?;
                ensure!(
                    version.seed == seed && version.actual_fixed_keys == actual,
                    "unbound startup Result ACK changed"
                );
                return ack.into_count();
            }
            let qualified = read_preparation::qualified_task_recovery_selectors_tx(
                tx,
                &scope.scope,
                &candidates,
            )?;
            let seed = binding(tx, &scope.scope)?;
            let mut before = BTreeMap::new();
            for key in &qualified {
                let stored =
                    stored_on(tx, agent, key)?.context("startup original Result missing")?;
                qualification::validate_member(tx, &scope.scope, seed.as_deref(), key, &stored)?;
                before.insert(key.clone(), stored);
            }
            let ack = prepared.execute()?;
            ensure!(
                ack.actual_result_ids
                    .iter()
                    .all(|id| candidates.contains(&TaskKey::new(TaskFamily::ResultSettlement, id))),
                "startup Result native ACK exceeds original predicate"
            );
            for id in &ack.actual_result_ids {
                let key = TaskKey::new(TaskFamily::ResultSettlement, id);
                let Some(original) = before.get(&key) else {
                    continue;
                };
                let after =
                    stored_on(tx, agent, &key)?.context("startup Result readback missing")?;
                if original != &after {
                    let next = scope
                        .revision
                        .checked_add(1)
                        .context("startup Task revision overflow")?;
                    upsert_alias(
                        tx,
                        &scope.scope,
                        &key,
                        &Alias {
                            revision: next,
                            availability: TaskAvailability::Present,
                            digest: Some(after.digest()),
                            projection_digest: Some(after.projection_digest),
                        },
                    )?;
                    scope.canonical_changed = true;
                }
            }
            ack.into_count()
        })();
        // Result bodies and readbacks have left scope. Keep only the original
        // Attempt metadata guards; later activations reuse this allowance.
        scope._guards.truncate(retained.2);
        scope.bytes = retained.0;
        scope.records = retained.1;
        result
    }

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
            if scope.unbound.is_some() {
                continue;
            }
            let mut changed = scope.canonical_changed;
            if let Some(commands) = actual.get(&scope.scope.agent_id) {
                for command in commands {
                    let id = match command {
                        ExecutionProtocolCommand::Interrupt(c) => c.attempt_id.as_str(),
                        ExecutionProtocolCommand::Settle(c) => c.outcome.attempt_id.as_str(),
                        ExecutionProtocolCommand::FinalizeInterrupted(c) => c.attempt_id.as_str(),
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
                    revision(tx, &scope.scope)?.unwrap_or(1) == scope.revision,
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
