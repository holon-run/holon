//! Shared original read-only Task/Input scope checks; it grants no writer authority.
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;

pub(in crate::runtime_db) struct TaskInputReadAuthorization<'a, 'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: &'a InputEvidenceScope,
    pub(super) seed: Option<String>,
    pub(super) fixed_inventory: Option<String>,
    pub(super) material: read_preparation::TaskReadMaterial<'a>,
}

pub(super) fn prepare_task_input_read_base_tx<'a, 'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &'a InputEvidenceScope,
    expected_state: &AgentState,
    revision: u64,
    key: &TaskKey,
    task: Option<&QualifiedTaskFact>,
    source: Option<&TaskOutputSourceHead>,
    request: &'a TaskDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
) -> Result<TaskInputReadAuthorization<'a, 'tx, 'conn>> {
    ensure!(
        key.family == TaskFamily::Task && expected_state.id == scope.agent_id,
        "Task/Input readonly original scope differs"
    );
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(ledger));
    material.charge_ids(
        read_preparation::encoded_metadata_bytes(&(key, task.map(|task| &task.locator), source))?
            + 512,
        8,
    )?;
    let identity_bytes: usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)", [&scope.agent_id], |row| row.get(0))?;
    material.charge_record_address(0, identity_bytes)?;
    let identity_nodes: usize = tx.query_row(
        "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities i,json_tree(i.payload_json) j WHERE i.agent_id=?1",
        [&scope.agent_id], |row| row.get(0),
    )?;
    material.charge_ids(0, identity_nodes)?;
    // Retired identity never authorizes reading a successor State body.
    let current: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND status='active' AND json_extract(payload_json,'$.agent_id')=agent_id AND json_extract(payload_json,'$.status')=status AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END=?2)",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    ensure!(current, TaskSnapshotError::RetiredScope);
    let state_bytes: usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)",[&scope.agent_id],|row|row.get(0))?;
    material.charge_record_address(0, state_bytes)?;
    let state_nodes:usize=tx.query_row("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_states s,json_tree(s.payload_json) j WHERE s.agent_id=?1",[&scope.agent_id],|row|row.get(0))?;
    material.charge_ids(0, state_nodes)?;
    let facts = scope_facts(tx, scope, false)?.ok_or(TaskSnapshotError::RetiredScope)?;
    ensure!(
        facts.state.as_ref() == Some(expected_state),
        "Task output State changed from its original publication"
    );
    let actual: Option<u64> = tx.query_row("SELECT revision FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0)).optional()?;
    ensure!(
        actual == Some(revision),
        TaskSnapshotError::RevisionMismatch {
            expected: revision,
            actual
        }
    );
    let seed_bytes: usize = tx.query_row("SELECT COALESCE(length(CAST(mode AS BLOB)),0)+COALESCE(length(CAST(seed_id AS BLOB)),0) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    material.charge_ids(seed_bytes, 2)?;
    let seed = binding(tx, scope)?;
    if let Some(seed) = seed.as_deref() {
        // Full immutable roots are decoded by manifests(). Their integer byte
        // gate must precede even native JSON node counting.
        let mut query=tx.prepare("SELECT CASE WHEN c.capture_phase='first_import' THEN length(CAST(i.roots_json AS BLOB)) ELSE length(CAST(s.roots_json AS BLOB)) END+length(CAST(c.capture_phase AS BLOB))+length(CAST(c.metadata_digest AS BLOB)) FROM task_domain_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id WHERE c.seed_id=?1 ORDER BY c.capture_phase")?;
        for bytes in query.query_map([seed], |row| row.get::<_, usize>(0))? {
            material.charge_record_address(0, bytes?)?;
        }
        let nodes: usize=tx.query_row("SELECT COALESCE(SUM((SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM json_tree(CASE WHEN c.capture_phase='first_import' THEN i.roots_json ELSE s.roots_json END) j)),0) FROM task_domain_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id WHERE c.seed_id=?1",[seed],|row|row.get(0))?;
        material.charge_ids(0, nodes)?;
    }
    let manifests = qualification::manifests(tx, seed.as_deref())?;
    let fixed = manifests
        .iter()
        .rev()
        .find_map(|checkpoint| checkpoint.query_inventory.as_ref());
    if let Some(fixed) = fixed {
        super::super::episode_snapshot::task_checkpoint::queries::validate(tx, fixed)?;
    }
    if let Some(task) = task {
        ensure!(
            task.locator.scope == *scope && task.locator.key == *key,
            "Task/Input original locator differs"
        );
        let same_owner: Option<bool> = tx
            .query_row(
                "SELECT owner_agent_id=?2 FROM tasks WHERE task_id=?1",
                params![key.id, scope.agent_id],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            same_owner.is_some(),
            TaskSnapshotError::CanonicalChanged(key.clone())
        );
        ensure!(
            same_owner == Some(true),
            TaskSnapshotError::ForeignRecord(key.clone())
        );
        let alias_bytes: usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(availability AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(projection_digest AS BLOB)),0) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3),0)",params![scope.agent_id,scope.identity_incarnation,key.id],|row|row.get(0))?;
        material.charge_relation(alias_bytes)?;
        material
            .admit_stored_tx(tx, &scope.agent_id, key)?
            .context("Task output canonical Task missing")?;
        let stored =
            stored_on(tx, &scope.agent_id, key)?.context("Task output canonical Task missing")?;
        ensure!(
            stored.digest() == task.locator.content_digest
                && stored.projection_digest == task.locator.projection_digest
                && stored.fact == task.fact,
            TaskSnapshotError::CanonicalChanged(key.clone())
        );
        // A fixed qualified Task can have no current alias. Reuse the ordinary
        // exact-member check; current tombstones still take precedence over fixed.
        let member_revision =
            qualification::validate_member(tx, scope, seed.as_deref(), key, &stored)?;
        ensure!(
            member_revision == task.locator.revision,
            "Task output locator differs from its original qualified Task"
        );
    } else {
        // None is the explicit same-view global absence, never a pruned body.
        let canonical: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1)",
            [&key.id],
            |row| row.get(0),
        )?;
        let blocked = canonical || qualification::known_anywhere(tx, key)?;
        ensure!(!blocked, TaskSnapshotError::NeedsPreparation(key.clone()));
    }
    Ok(TaskInputReadAuthorization {
        tx,
        scope,
        seed,
        fixed_inventory: fixed.map(|fixed| fixed.id.clone()),
        material,
    })
}

impl TaskInputReadAuthorization<'_, '_, '_> {
    pub(in crate::runtime_db) fn validates_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "Task/Input read crossed its original transaction"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn scope(&self) -> &InputEvidenceScope {
        self.scope
    }
    pub(in crate::runtime_db) fn seed(&self) -> Option<&str> {
        self.seed.as_deref()
    }
    pub(in crate::runtime_db) fn fixed_inventory(&self) -> Option<&str> {
        self.fixed_inventory.as_deref()
    }
    pub(in crate::runtime_db) fn charge(&mut self, bytes: usize, records: usize) -> Result<()> {
        self.material.charge_final_metadata(
            bytes
                .checked_add(
                    records
                        .checked_mul(256)
                        .context("Task/Input nodes overflow")?,
                )
                .context("Task/Input bytes overflow")?,
            records,
        )
    }
    /// Borrow the original same-scope Task accounting phase; this is no read grant.
    pub(in crate::runtime_db) fn with_input_material<T>(
        &mut self,
        action: impl FnOnce(
            &mut crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        self.material.with_input_material(action)
    }
    pub(in crate::runtime_db) fn charge_body(&mut self, bytes: usize) -> Result<()> {
        self.material.charge_record_address(0, bytes)
    }
}

#[cfg(test)]
#[path = "qualified_tool_read_tests.rs"]
mod tool_read_tests;

#[cfg(test)]
#[path = "qualified_input_material_tests.rs"]
mod scope_material_tests;
