//! Offline repairs refresh only the finite evidence already qualified before
//! their original SQL transaction. The borrowed lock excludes resident owners.
use super::*;
use crate::domain::execution_protocol::ExecutionProtocolCommand;
use crate::runtime_db::{RuntimeDb, RuntimeDbLock};
use std::path::{Path, PathBuf};

/// Capability for an offline caller that holds the configured maintenance lock.
/// This borrow must enclose every original canonical repair transaction.
pub struct OfflineInputMaintenance<'a> {
    database: PathBuf,
    _lease: &'a RuntimeDbLock,
}
impl<'a> OfflineInputMaintenance<'a> {
    pub fn for_database(
        db: &RuntimeDb,
        lease: &'a RuntimeDbLock,
        expected_maintenance_path: &Path,
    ) -> Result<Self> {
        ensure!(
            lease.path() == expected_maintenance_path,
            "offline input maintenance lease path mismatch"
        );
        Ok(Self {
            database: db.path().to_owned(),
            _lease: lease,
        })
    }
    fn validate_database(&self, db: &RuntimeDb) -> Result<()> {
        ensure!(
            self.database == db.path(),
            "offline input maintenance database mismatch"
        );
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("qualified offline input repair requires the configured maintenance lease for {0}")]
pub struct NeedsOfflineInputMaintenance(pub String);

pub(crate) struct PreparedOfflineInputRepair {
    agent: String,
    role: RepairRole,
    allowed: BTreeSet<Key>,
    commands: Vec<ExecutionProtocolCommand>,
}
enum RepairRole {
    Current(InputEvidenceScope, ExistingAdmissionInputProof),
    Unbound(legacy_recovery::PreparedLegacySeedRecovery),
    Unqualified,
}

/// Called before canonical writes, using only the original finite writer keys.
/// Outcome keys may be new; only an actual recovery command with an already
/// qualified predecessor can authorize those derived records.
pub(crate) fn prepare_offline_input_repair_tx(
    tx: &Transaction<'_>,
    db: &RuntimeDb,
    agent: &str,
    keys: &[Key],
    commands: &[ExecutionProtocolCommand],
    authority: Option<&OfflineInputMaintenance<'_>>,
) -> Result<PreparedOfflineInputRepair> {
    if let Some(authority) = authority {
        authority.validate_database(db)?;
    }
    let mut selected = BTreeSet::new();
    let mut bytes = 0;
    extend_metadata_selection(&mut selected, &mut bytes, keys.iter().cloned())?;
    let absent = || PreparedOfflineInputRepair {
        agent: agent.into(),
        role: RepairRole::Unqualified,
        allowed: BTreeSet::new(),
        commands: Vec::new(),
    };
    let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_evidence_current')", [], |r| r.get(0))?;
    if !exists {
        return Ok(absent());
    }
    let existing_records = selected
        .iter()
        .filter_map(|key| match payload_on(tx, agent, key) {
            Ok(Some(payload)) => Some(decode_canonical(key, &payload)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    let related = affected_projection_keys_for_agent_tx(tx, agent, &existing_records)?;
    extend_metadata_selection(&mut selected, &mut bytes, related)?;
    let commands = commands
        .iter()
        .filter(|command| {
            matches!(
                command,
                ExecutionProtocolCommand::Interrupt(_)
                    | ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(_)
                    | ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let identity: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
            [agent],
            |r| r.get(0),
        )
        .optional()?;
    let (role, mut allowed) = if let Some(payload) = identity {
        let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
        ensure!(
            identity.agent_id == agent && identity.incarnation > 0,
            "offline input identity differs from canonical namespace"
        );
        if identity.status == AgentRegistryStatus::Deleted {
            return Ok(absent());
        }
        let scope = InputEvidenceScope {
            agent_id: agent.into(),
            identity_incarnation: identity.incarnation,
        };
        // This is the original finite cleanup scope, including same-incarnation
        // Deleting. Ordinary writers still require an Active identity.
        require_original_input_recovery_scope(tx, &scope)?;
        let binding = binding_on(tx, &scope)?;
        let mut roots = BTreeSet::new();
        for key in &selected {
            if is_existing_member_on(tx, &scope, &binding, key)? {
                validate_existing_input_evidence_inner_tx(tx, &scope, key)?;
                roots.insert(key.clone());
            }
        }
        if roots.is_empty() {
            return Ok(absent());
        }
        if authority.is_none() {
            return Err(NeedsOfflineInputMaintenance(agent.into()).into());
        }
        let derived = derived_outcomes(&commands, &roots);
        let proof =
            ExistingAdmissionInputProof::prepare_tx(tx, &scope, roots.clone(), derived.clone())?;
        let allowed: BTreeSet<Key> = roots.union(&derived).cloned().collect();
        (RepairRole::Current(scope, proof), allowed)
    } else {
        let seed: Option<String> = tx.query_row("SELECT seed_id FROM episode_continuity_seeds WHERE agent_id=?1 AND identity_incarnation IS NULL AND policy_version=1", [agent], |r| r.get(0)).optional()?;
        let Some(seed) = seed else {
            return Ok(absent());
        };
        let mut roots = BTreeSet::new();
        for key in &selected {
            let member: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_seed_membership WHERE seed_id=?1 AND family=?2 AND record_id=?3)", params![seed, key.family.as_str(), key.id], |r| r.get(0))?;
            if member {
                roots.insert(key.clone());
            }
        }
        if roots.is_empty() {
            return Ok(absent());
        }
        if authority.is_none() {
            return Err(NeedsOfflineInputMaintenance(agent.into()).into());
        }
        let scope = legacy_recovery::legacy_seed_recovery_scope_tx(tx, agent)?;
        let prepared = legacy_recovery::prepare_unbound_seed_recovery_tx(
            tx,
            &scope,
            &roots.iter().cloned().collect::<Vec<_>>(),
        )?;
        let derived = derived_outcomes(&commands, &roots);
        let allowed: BTreeSet<Key> = roots.union(&derived).cloned().collect();
        (RepairRole::Unbound(prepared), allowed)
    };
    // The caller cannot smuggle an unrelated derived ID into the exact readback.
    allowed.retain(|key| selected.contains(key));
    Ok(PreparedOfflineInputRepair {
        agent: agent.into(),
        role,
        allowed,
        commands,
    })
}
fn derived_outcomes(commands: &[ExecutionProtocolCommand], roots: &BTreeSet<Key>) -> BTreeSet<Key> {
    commands
        .iter()
        .filter_map(|command| {
            let (attempt, outcome) = match command {
                ExecutionProtocolCommand::Interrupt(x) => (&x.attempt_id, &x.outcome_id),
                ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(x) => {
                    (&x.attempt_id, &x.outcome_id)
                }
                ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(x) => {
                    (&x.attempt_id, &x.outcome_id)
                }
                _ => return None,
            };
            roots
                .contains(&Key::new(Family::Attempt, attempt))
                .then(|| Key::new(Family::Outcome, outcome))
        })
        .collect()
}
impl PreparedOfflineInputRepair {
    /// Exact readback and qualification share the caller's still-open original TX.
    pub(crate) fn finish_tx(self, tx: &Transaction<'_>) -> Result<CommittedInputEvidenceDelta> {
        let records = self
            .allowed
            .iter()
            .map(|key| {
                let payload = payload_on(tx, &self.agent, key)?
                    .context("offline repair canonical ACK row missing")?;
                decode_canonical(key, &payload)
            })
            .collect::<Result<Vec<_>>>()?;
        match self.role {
            RepairRole::Current(scope, proof) => {
                register_committed_cleanup_input_evidence_tx(tx, &scope, &records, &proof)
            }
            RepairRole::Unbound(prepared) => {
                legacy_recovery::register_unbound_seed_recovery_tx(
                    tx,
                    &prepared,
                    &records,
                    &self.commands,
                )?;
                Ok(Default::default())
            }
            RepairRole::Unqualified => Ok(Default::default()),
        }
    }
}

#[cfg(test)]
#[path = "input_evidence_maintenance_tests.rs"]
mod tests;

#[path = "input_evidence_maintenance_fallback.rs"]
pub(crate) mod fallback;
