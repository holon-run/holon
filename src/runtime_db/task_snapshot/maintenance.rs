//! Existing-only qualification maintenance. This ACK cannot install a Ready domain.
use super::*;

pub(super) struct ExistingTarget {
    pub(super) before: Option<StoredTask>,
    pub(super) qualified: bool,
}
/// Borrowing the original transaction prevents a proof crossing commit/rollback.
/// It carries neither a State nor permission for a new Task or Result key.
pub(crate) struct ExistingTaskMaintenanceProof<'tx, 'connection> {
    pub(super) tx: &'tx Transaction<'connection>,
    pub(super) scope: InputEvidenceScope,
    pub(super) base_revision: u64,
    pub(super) before: BTreeMap<TaskKey, ExistingTarget>,
}
impl ExistingTaskMaintenanceProof<'_, '_> {
    /// An original producer observes only an already qualified member. This
    /// getter grants neither a new key nor a business transition exception.
    pub(crate) fn qualified_task_before(&self, id: &str) -> Result<&crate::types::TaskRecord> {
        let key = TaskKey::new(TaskFamily::Task, id);
        let row = self
            .before
            .get(&key)
            .context("original Task not reserved")?;
        ensure!(row.qualified, TaskSnapshotError::NeedsPreparation(key));
        let stored = row
            .before
            .as_ref()
            .context("qualified original Task missing")?;
        let CanonicalTaskFact::Task(task) = &stored.fact else {
            anyhow::bail!("original Task family differs")
        };
        Ok(task)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskScopeInvalidation {
    pub base_revision: u64,
    pub revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskMaintenanceAck {
    pub scope: InputEvidenceScope,
    pub qualified_actual_keys: BTreeSet<TaskKey>,
    pub invalidation: Option<TaskScopeInvalidation>,
}

/// Owned preparation/final readback copies, before any body decode. The caller
/// retains its shared-ledger Candidate guard through the original transaction.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskMaintenanceMaterial {
    pub bytes: usize,
    pub records: usize,
}
pub(crate) fn task_maintenance_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<TaskMaintenanceMaterial> {
    let identity_bytes: usize = tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",
        [&scope.agent_id], |r| r.get(0),
    )?;
    let mut bytes = identity_bytes
        .checked_add(scope.agent_id.len() + 256)
        .context("Task maintenance material overflow")?;
    for key in keys {
        let n = checkpoint_material_size_on(tx, key)?.unwrap_or_default();
        bytes = bytes
            .checked_add(n)
            .and_then(|n| n.checked_add(key.id.len() + 512))
            .context("Task maintenance material overflow")?;
        bound(keys.len() + 1, bytes, None)?;
    }
    let material = TaskMaintenanceMaterial {
        bytes: bytes
            .checked_mul(8)
            .context("Task maintenance copies overflow")?,
        records: (keys.len() + 1)
            .checked_mul(8)
            .context("Task maintenance records overflow")?,
    };
    bound(material.records, material.bytes, None)?;
    Ok(material)
}
fn revision_tx(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<u64> {
    let row: Option<(u64,u64)> = tx.query_row(
        "SELECT revision,json_extract(coverage_json,'$.revision') FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    match row {
        Some((normalized, payload)) => {
            ensure!(
                normalized > 0 && normalized == payload,
                "Task maintenance revision projection mismatch"
            );
            Ok(normalized)
        }
        None => Ok(1),
    }
}

/// Identity-only: NoState cleanup must not load a successor or invent empty State.
pub(super) fn require_existing_maintenance_scope_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<()> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid Task maintenance scope"
    );
    let size: Option<usize> = tx
        .query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id],
            |r| r.get(0),
        )
        .optional()?;
    bound(1, size.unwrap_or_default(), None)?;
    let raw: Option<(String, String, String)> = tx
        .query_row(
            "SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (agent, status, payload) = raw.context("Task maintenance identity missing")?;
    let identity = repositories::decode_agent_identity_payload(&payload)?;
    ensure!(
        agent == scope.agent_id
            && identity.agent_id == agent
            && identity.incarnation == scope.identity_incarnation
            && identity.incarnation > 0
            && serde_json::to_value(&identity.status)?.as_str() == Some(status.as_str()),
        "Task maintenance identity projection/incarnation mismatch"
    );
    ensure!(
        matches!(
            identity.status,
            AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
        ),
        "Task maintenance scope retired"
    );
    binding(tx, scope)?;
    Ok(())
}

pub(crate) fn prepare_existing_task_maintenance_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<ExistingTaskMaintenanceProof<'tx, 'connection>> {
    task_maintenance_material_tx(tx, scope, keys)?;
    require_existing_maintenance_scope_tx(tx, scope)?;
    let seed = binding(tx, scope)?;
    lengths(tx, keys, None)?;
    let mut before = BTreeMap::new();
    for key in keys {
        ensure!(!key.id.is_empty(), "empty Task maintenance target");
        let a = alias(tx, scope, key)?;
        ensure!(
            !a.as_ref()
                .is_some_and(|a| a.availability == TaskAvailability::Pruned),
            "Task maintenance cannot revive pruned ID"
        );
        let fixed = qualification::eligible_digest(tx, seed.as_deref(), key)?;
        let qualified = a
            .as_ref()
            .is_some_and(|a| a.availability == TaskAvailability::Present)
            || (a.is_none() && fixed.is_some());
        let stored = stored_on(tx, &scope.agent_id, key)?;
        if qualified {
            let item = stored
                .as_ref()
                .context("qualified Task maintenance body missing")?;
            qualification::validate_member(tx, scope, seed.as_deref(), key, item)?;
        }
        before.insert(
            key.clone(),
            ExistingTarget {
                before: stored,
                qualified,
            },
        );
    }
    Ok(ExistingTaskMaintenanceProof {
        tx,
        scope: scope.clone(),
        base_revision: revision_tx(tx, scope)?,
        before,
    })
}

/// Call only with IDs acknowledged by actual writes, before the original whole
/// receipt. Raw old business cleanup is retained without granting qualification.
pub(crate) fn register_existing_task_maintenance_tx(
    tx: &Transaction<'_>,
    proof: ExistingTaskMaintenanceProof<'_, '_>,
    actual_keys: &BTreeSet<TaskKey>,
) -> Result<TaskMaintenanceAck> {
    register_with_derived_tx(tx, proof, actual_keys, &BTreeSet::new())
}
pub(super) fn register_with_derived_tx(
    tx: &Transaction<'_>,
    proof: ExistingTaskMaintenanceProof<'_, '_>,
    actual_keys: &BTreeSet<TaskKey>,
    derived: &BTreeSet<TaskKey>,
) -> Result<TaskMaintenanceAck> {
    ensure!(
        std::ptr::eq(tx, proof.tx),
        "Task maintenance proof belongs to another transaction"
    );
    require_existing_maintenance_scope_tx(tx, &proof.scope)?;
    ensure!(
        revision_tx(tx, &proof.scope)? == proof.base_revision,
        "Task maintenance base changed"
    );
    ensure!(
        actual_keys.iter().all(|key| proof.before.contains_key(key)),
        "Task maintenance actual writes exceed original footprint"
    );
    lengths(tx, actual_keys, None)?;
    let mut ack = TaskMaintenanceAck {
        scope: proof.scope.clone(),
        qualified_actual_keys: BTreeSet::new(),
        invalidation: None,
    };
    if actual_keys.is_empty() {
        return Ok(ack);
    }
    let revision = proof
        .base_revision
        .checked_add(1)
        .context("Task maintenance revision overflow")?;
    for key in actual_keys {
        let before = &proof.before[key];
        ensure!(
            before.before.is_some() || derived.contains(key),
            "existing Task maintenance cannot create a new ID"
        );
        let after = stored_on(tx, &proof.scope.agent_id, key)?;
        ensure!(
            !derived.contains(key) || after.is_some(),
            "derived Result actual body missing"
        );
        if !before.qualified {
            continue;
        }
        let value = match after.as_ref() {
            Some(after) => Alias {
                revision,
                availability: TaskAvailability::Present,
                digest: Some(after.digest()),
                projection_digest: Some(after.projection_digest.clone()),
            },
            None => Alias {
                revision,
                availability: TaskAvailability::Pruned,
                digest: None,
                projection_digest: None,
            },
        };
        upsert_alias(tx, &proof.scope, key, &value)?;
        if let Some(after) = after.as_ref() {
            if let CanonicalTaskFact::Task(task) = &after.fact {
                query_relations::register_actual_task_tx(
                    tx,
                    &proof.scope,
                    task,
                    &after.digest(),
                    &after.projection_digest,
                    false,
                )?;
            }
        }
        ack.qualified_actual_keys.insert(key.clone());
    }
    // Task-only writes can change a Result predicate, including raw old Tasks.
    // A sparse ACK deliberately discards all completeness/negative observations.
    loader::persist_coverage(tx, &proof.scope, &writes::empty_coverage(revision))?;
    ack.invalidation = Some(TaskScopeInvalidation {
        base_revision: proof.base_revision,
        revision,
    });
    Ok(ack)
}
