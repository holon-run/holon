//! The exclusive startup actor may advance only a fixed unbound seed's exact
//! members, plus Outcomes actually produced by its canonical recovery commands.
use super::*;
use crate::domain::execution_protocol::ExecutionProtocolCommand;
use crate::runtime_db::{RuntimeDb, RuntimeDbLock};
use std::path::{Path, PathBuf};
use tokio::sync::MutexGuard;

/// Host checks never-activated/no active, temporary or draining runtimes before
/// minting. Both borrows keep this capability inside the protected handoff.
pub(crate) struct ExclusiveStartupInputRecovery<'a> {
    database: PathBuf,
    _lease: &'a RuntimeDbLock,
    _handoff: &'a MutexGuard<'a, ()>,
}
impl<'a> ExclusiveStartupInputRecovery<'a> {
    pub(crate) fn for_pre_activation_host(
        db: &RuntimeDb,
        lease: &'a RuntimeDbLock,
        expected_maintenance_path: &Path,
        handoff: &'a MutexGuard<'a, ()>,
    ) -> Result<Self> {
        ensure!(
            lease.path() == expected_maintenance_path,
            "input startup maintenance lease mismatch"
        );
        Ok(Self {
            database: db.path().to_owned(),
            _lease: lease,
            _handoff: handoff,
        })
    }
    pub(crate) fn validate_database(&self, db: &RuntimeDb) -> Result<()> {
        ensure!(
            self.database == db.path(),
            "input startup capability database mismatch"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacySeedRecoveryScope {
    pub agent_id: String,
    pub seed_id: String,
    pub boundary_id: String,
    pub source_kind: String,
    pub capture_checkpoint_digest: String,
}
#[derive(Debug, thiserror::Error)]
#[error("legacy input recovery needs explicit fixed-seed preparation for {0}")]
pub(crate) struct NeedsLegacyRecoveryPreparation(pub String);

pub(crate) struct PreparedLegacySeedRecovery {
    pub(super) scope: LegacySeedRecoveryScope,
    existing: BTreeMap<Key, Fingerprint>,
}

/// Durable namespace facts only; the repository must also borrow a genuine
/// ExclusiveStartupInputRecovery for the entire original recovery transaction.
pub(crate) fn legacy_seed_recovery_scope_tx(
    tx: &Transaction<'_>,
    agent: &str,
) -> Result<LegacySeedRecoveryScope> {
    let row:Option<(String,String,String,Option<i64>)>=tx.query_row(
        "SELECT seed_id,boundary_id,source_kind,identity_incarnation FROM episode_continuity_seeds WHERE agent_id=?1 AND policy_version=1",
        [agent],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (seed, boundary, source, inc) =
        row.ok_or_else(|| NeedsLegacyRecoveryPreparation(agent.into()))?;
    ensure!(
        inc.is_none() && matches!(source.as_str(), "upgrade_unbound" | "legacy_state_import"),
        "legacy startup seed is not an explicit unbound source"
    );
    let current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1) OR EXISTS(SELECT 1 FROM agent_episode_continuity WHERE agent_id=?1)",[agent],|r|r.get(0))?;
    ensure!(
        !current,
        "legacy startup seed already has an identity or binding"
    );
    let actual_boundary: String = tx.query_row(
        "SELECT value FROM runtime_metadata WHERE key='episode_continuity/v1/upgrade_boundary'",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        boundary == actual_boundary,
        "legacy startup fixed boundary mismatch"
    );
    let captures = {
        let mut statement=tx.prepare("SELECT capture_phase,policy_version,includes_tails,metadata_digest FROM input_evidence_captures WHERE seed_id=?1 ORDER BY capture_phase")?;
        let rows = statement.query_map([&seed], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    ensure!(
        !captures.is_empty() && captures.iter().all(|x| x.1 == 1),
        "legacy startup has no fixed input capture"
    );
    Ok(LegacySeedRecoveryScope {
        agent_id: agent.into(),
        seed_id: seed,
        boundary_id: boundary,
        source_kind: source,
        capture_checkpoint_digest: bytes_digest(&serde_json::to_vec(&captures)?),
    })
}
fn validate_scope_tx(tx: &Transaction<'_>, scope: &LegacySeedRecoveryScope) -> Result<()> {
    ensure!(
        legacy_seed_recovery_scope_tx(tx, &scope.agent_id)? == *scope,
        "legacy recovery fixed proof changed"
    );
    Ok(())
}

pub(crate) fn prepare_unbound_seed_recovery_tx(
    tx: &Transaction<'_>,
    scope: &LegacySeedRecoveryScope,
    keys: &[Key],
) -> Result<PreparedLegacySeedRecovery> {
    validate_scope_tx(tx, scope)?;
    let mut selected = BTreeSet::new();
    let mut bytes = 0;
    extend_metadata_selection(&mut selected, &mut bytes, keys.iter().cloned())?;
    let mut existing = BTreeMap::new();
    for key in selected {
        let current = fingerprint_on(tx, &scope.agent_id, &key)?
            .ok_or_else(|| NeedsLegacyRecoveryPreparation(key.id.clone()))?;
        require_eligible(
            tx,
            &Binding {
                seed_id: Some(scope.seed_id.clone()),
            },
            &key,
            &current,
        )?;
        existing.insert(key, current);
    }
    Ok(PreparedLegacySeedRecovery {
        scope: scope.clone(),
        existing,
    })
}

/// Called after the original canonical SQL and command-result write, before its
/// COMMIT. It cannot grant unrelated rows or recapture any optional history tail.
pub(crate) fn register_unbound_seed_recovery_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedLegacySeedRecovery,
    records: &[CanonicalInputEvidence],
    commands: &[ExecutionProtocolCommand],
) -> Result<()> {
    validate_scope_tx(tx, &prepared.scope)?;
    let agent = &prepared.scope.agent_id;
    let witnesses = commands
        .iter()
        .map(recovery_witness)
        .collect::<Result<Vec<_>>>()?;
    for supplied in records {
        let key = supplied.key();
        let payload = payload_on(tx, agent, &key)?.context("legacy recovery ACK row missing")?;
        ensure!(
            decode_canonical(&key, &payload)? == *supplied,
            "legacy recovery ACK differs from canonical SQL"
        );
        let mut current =
            fingerprint_on(tx, agent, &key)?.context("legacy recovery ACK metadata missing")?;
        // The validated original actor may advance this fixed member version.
        // A later first materialization must prove this exact acknowledged body.
        let digest = bytes_digest(payload.as_bytes());
        validate_canonical_payload_digest(&key, &current, &digest)?;
        if requires_frozen_body_digest(&key, &current) {
            current.frozen_payload_digest = Some(digest);
        }
        let (origin, attempt, kind, identity, hash) = if prepared.existing.contains_key(&key) {
            ("fixed_member", None, None, None, None)
        } else {
            let CanonicalInputEvidence::Outcome(outcome) = supplied else {
                return Err(NeedsLegacyRecoveryPreparation(key.id).into());
            };
            ensure!(
                prepared
                    .existing
                    .contains_key(&Key::new(Family::Attempt, &outcome.attempt_id)),
                "new legacy Outcome has no fixed predecessor"
            );
            let witness = witnesses
                .iter()
                .find(|w| w.outcome == outcome.outcome_id && w.attempt == outcome.attempt_id)
                .context("new legacy Outcome is not produced by this recovery")?;
            let actual:Option<String>=tx.query_row("SELECT payload_hash FROM execution_protocol_command_results WHERE agent_id=?1 AND command_kind=?2 AND command_identity=?3",params![agent,witness.kind,witness.identity],|r|r.get(0)).optional()?;
            ensure!(
                actual.as_deref() == Some(witness.hash.as_str()),
                "legacy recovery command witness missing or changed"
            );
            (
                "recovery_created",
                Some(witness.attempt.clone()),
                Some(witness.kind.to_string()),
                Some(witness.identity.clone()),
                Some(witness.hash.clone()),
            )
        };
        let old:Option<(i64,String)>=tx.query_row("SELECT revision,fingerprint_json FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![prepared.scope.seed_id,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((_, json)) = old.as_ref() {
            let old_fingerprint: Fingerprint = serde_json::from_str(json)?;
            if old_fingerprint == current {
                continue;
            }
        }
        let revision = old.map_or(Ok(1i64), |(revision, _)| {
            revision
                .checked_add(1)
                .context("legacy recovery revision overflow")
        })?;
        let json = serde_json::to_string(&current)?;
        tx.execute("INSERT INTO input_evidence_seed_recovery_current(seed_id,family,record_id,revision,fingerprint_json,origin_kind,origin_attempt_id,command_kind,command_identity,command_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(seed_id,family,record_id) DO UPDATE SET revision=excluded.revision,fingerprint_json=excluded.fingerprint_json",params![prepared.scope.seed_id,key.family.as_str(),key.id,revision,json,origin,attempt,kind,identity,hash])?;
        let actual:(i64,String)=tx.query_row("SELECT revision,fingerprint_json FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![prepared.scope.seed_id,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            actual == (revision, json),
            "legacy recovery alias exact readback mismatch"
        );
    }
    super::execution::register_seed_recovery_tx(
        tx,
        &prepared.scope.seed_id,
        &prepared.scope.agent_id,
        records,
    )?;
    Ok(())
}
struct RecoveryWitness {
    attempt: String,
    outcome: String,
    kind: &'static str,
    identity: String,
    hash: String,
}
fn recovery_witness(command: &ExecutionProtocolCommand) -> Result<RecoveryWitness> {
    let (attempt, outcome, kind, identity) = match command {
        ExecutionProtocolCommand::Interrupt(c) => (
            &c.attempt_id,
            &c.outcome_id,
            "interrupt_execution",
            &c.outcome_id,
        ),
        ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(c) => (
            &c.attempt_id,
            &c.outcome_id,
            "recover_interrupted_task_result_claim",
            &c.command_id,
        ),
        ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(c) => (
            &c.attempt_id,
            &c.outcome_id,
            "recover_unadvanced_task_result_claim",
            &c.command_id,
        ),
        _ => anyhow::bail!("unbound seed actor accepts only canonical startup recovery commands"),
    };
    Ok(RecoveryWitness {
        attempt: attempt.clone(),
        outcome: outcome.clone(),
        kind,
        identity: identity.clone(),
        hash: bytes_digest(&serde_json::to_vec(command)?),
    })
}

/// Loader validation after later identity materialization. The immutable fixed
/// membership remains the origin; a recovery-created Outcome needs its exact
/// stored canonical command hash and fixed/validated predecessor membership.
pub(super) fn validate_seed_overlay_on(tx: &Transaction<'_>, seed: &str, key: &Key) -> Result<()> {
    let row:Option<(String,Option<String>,Option<String>,Option<String>,Option<String>)>=tx.query_row("SELECT origin_kind,origin_attempt_id,command_kind,command_identity,command_hash FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![seed,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((origin, attempt, kind, identity, hash)) = row else {
        return Ok(());
    };
    let fixed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE seed_id=?1 AND family=?2 AND record_id=?3)",params![seed,key.family.as_str(),key.id],|r|r.get(0))?;
    if origin == "fixed_member" {
        ensure!(fixed, "legacy overlay has no immutable membership");
        return Ok(());
    }
    if origin == "maintenance_created" {
        ensure!(!fixed, "fallback created origin overwrote fixed membership");
        return maintenance::fallback::validate_origin_tx(tx, seed, key);
    }
    ensure!(
        origin == "recovery_created" && key.family == Family::Outcome && !fixed,
        "invalid legacy recovery-created origin"
    );
    let attempt = attempt.context("legacy recovery predecessor missing")?;
    let eligible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE seed_id=?1 AND family='attempt' AND record_id=?2)",params![seed,attempt],|r|r.get(0))?;
    ensure!(
        eligible,
        "legacy recovery predecessor is not fixed eligible"
    );
    let actual:Option<String>=tx.query_row("SELECT c.payload_hash FROM execution_protocol_command_results c JOIN episode_continuity_seeds s ON s.agent_id=c.agent_id WHERE s.seed_id=?1 AND c.command_kind=?2 AND c.command_identity=?3",params![seed,kind.context("legacy recovery command kind missing")?,identity.context("legacy recovery command id missing")?],|r|r.get(0)).optional()?;
    ensure!(
        actual == hash && actual.is_some(),
        "legacy recovery origin witness no longer matches"
    );
    Ok(())
}

#[cfg(test)]
#[path = "input_evidence_legacy_recovery_tests.rs"]
mod tests;
