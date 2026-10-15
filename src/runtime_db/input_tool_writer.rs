//! The original generic writer lends its actual Input phase to finite Tool ACKs.
use super::*;
use std::cell::{Cell, RefCell};

/// A transaction-local budget adapter, never a Source or body permission.
/// Only the generic Native proof creates its one actual material implementation.
pub(in crate::runtime_db) trait OriginalToolInputWriter {
    fn validate_scope(&self, tx: &Transaction<'_>, scope: &InputEvidenceScope,
        agent: &str, cleanup: Option<&ExistingAdmissionInputProof>) -> Result<()>;
    fn reserve_original_tool(&self, tool: &ToolExecutionRecord) -> Result<()>;
    fn validate_original_tool_key(&self, tx: &Transaction<'_>, scope: &InputEvidenceScope,
        id: &str, cleanup: Option<&ExistingAdmissionInputProof>) -> Result<()>;
    fn mark_original_tool_noop(&self, tx: &Transaction<'_>, id: &str) -> Result<()>;
    fn before_facts(&self, tx: &Transaction<'_>, agent: &str,
        tools: &[ToolExecutionRecord], capacity: usize,
        current: Option<&crate::runtime_db::current_read_snapshot::ObservedCurrentReadMaterial<'_>>) -> Result<()>;
    fn register_tool(&self, tx: &Transaction<'_>, scope: &InputEvidenceScope,
        tool: &ToolExecutionRecord, cleanup: Option<&ExistingAdmissionInputProof>) -> Result<QualifiedInputEvidence>;
}

/// Constructed only by original key validation before business SQL. A kernel
/// no-op consumes these original bytes, never the later row's apparent ID.
pub(super) struct OriginalToolSource {
    fingerprint: Fingerprint,
    digest: String,
    noop: bool,
}
impl OriginalToolSource {
    pub(super) fn fingerprint(&self) -> &Fingerprint { &self.fingerprint }
    pub(super) fn digest(&self) -> &str { &self.digest }
}

/// Only the actual original Focus no-op can produce this exact D3 readback.
/// The concrete TX and original locator survive the short material borrow.
pub(in crate::runtime_db) struct OriginalToolNoopReadback<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    locator: EvidenceLocator,
}
impl OriginalToolNoopReadback<'_, '_> {
    pub(in crate::runtime_db) fn validate<'a>(&'a self, tx: &Transaction<'_>,
        scope: &InputEvidenceScope) -> Result<&'a EvidenceLocator> {
        ensure!(std::ptr::eq(self.tx, tx) && self.locator.scope == *scope
            && self.locator.key.family == Family::ToolExecution,
            "original no-op Tool readback changed transaction/scope");
        Ok(&self.locator)
    }
}

pub(in crate::runtime_db) struct InputToolWriter<'phase, 'accounting, 'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    existing: RefCell<BTreeMap<String, OriginalToolSource>>,
    noop_readbacks: RefCell<Vec<OriginalToolNoopReadback<'tx, 'connection>>>,
    material: RefCell<&'phase mut read_material::InputReadMaterial<'accounting>>,
    // Real original counters only. They bound coexisting prepared/effects Tool
    // bodies, and do not identify a Tool or authorize any canonical record.
    reserved: Cell<(u64, usize)>,
    current_usage: Cell<(usize, usize)>,
}
impl<'phase, 'accounting, 'tx, 'connection> InputToolWriter<'phase, 'accounting, 'tx, 'connection> {
    pub(in crate::runtime_db) fn new(
        tx: &'tx Transaction<'connection>,
        material: &'phase mut read_material::InputReadMaterial<'accounting>,
        current_usage: (usize, usize),
    ) -> Self {
        Self { tx, existing: RefCell::new(BTreeMap::new()), noop_readbacks: RefCell::new(Vec::new()), material: RefCell::new(material), reserved: Cell::new((0, 0)), current_usage: Cell::new(current_usage) }
    }
    pub(in crate::runtime_db) fn admitted_current_material(&self)->(usize,usize) {
        self.current_usage.get()
    }
    pub(in crate::runtime_db) fn take_original_noop_readbacks(self)
        -> Vec<OriginalToolNoopReadback<'tx, 'connection>> {
        self.noop_readbacks.into_inner()
    }
    fn validate_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(std::ptr::eq(self.tx, tx), "original Tool writer transaction differs");
        Ok(())
    }
}
fn original_tool_cost(tool: &ToolExecutionRecord) -> Result<(u64, usize)> {
    let (bytes, nodes) = crate::runtime::input_evidence_budget::borrowed_json_material(tool)?;
    // Original prepared DTO, business Effects clone, canonical serde String
    // and SQLite bind copy. Only the first two own Rust JSON node trees.
    let bytes = bytes.checked_mul(4)
        .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(2 * 256)?))
        .and_then(|n| n.checked_add(4096)).context("original Tool write copies overflow")?;
    Ok((bytes, nodes.checked_mul(2).and_then(|n|n.checked_add(32)).context("original Tool write nodes overflow")?))
}
impl OriginalToolInputWriter for InputToolWriter<'_, '_, '_, '_> {
    fn validate_scope(&self, tx: &Transaction<'_>, scope: &InputEvidenceScope,
        agent: &str, cleanup: Option<&ExistingAdmissionInputProof>) -> Result<()> {
        self.validate_tx(tx)?;
        let mut material = self.material.borrow_mut();
        material.validate_binding(scope)?;
        ensure!(agent == scope.agent_id, "original Tool writer agent differs");
        let facts = material.scope_facts_tx(tx)?;
        let active = facts.identity.as_ref().is_some_and(|identity| identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation && identity.status == AgentRegistryStatus::Active);
        drop(facts);
        if let Some(proof) = cleanup { proof.validate_scope_tx(tx, scope)?; }
        else { ensure!(active, InputEvidenceSnapshotError::RetiredScope); }
        Ok(())
    }
    fn reserve_original_tool(&self, tool: &ToolExecutionRecord) -> Result<()> {
        let mut material = self.material.borrow_mut();
        material.validate_tool_agent(&tool.agent_id)?;
        let cost = original_tool_cost(tool)?;
        material.observe_projected(cost.0, cost.1)?;
        let previous = self.reserved.get();
        self.reserved.set((previous.0.checked_add(cost.0).context("original Tool source bytes overflow")?,
            previous.1.checked_add(cost.1).context("original Tool source nodes overflow")?));
        Ok(())
    }
    fn validate_original_tool_key(&self, tx: &Transaction<'_>, scope: &InputEvidenceScope,
        id: &str, cleanup: Option<&ExistingAdmissionInputProof>) -> Result<()> {
        self.validate_tx(tx)?;
        let mut material = self.material.borrow_mut();
        material.validate_binding(scope)?;
        // The global owner check is scalar; foreign body is never traversed.
        let row: Option<(bool, usize, usize)> = tx.query_row(
            "SELECT agent_id=?1,length(CAST(payload_json AS BLOB)),length(CAST(agent_id AS BLOB)) FROM tool_executions WHERE evidence_id=?2",
            params![scope.agent_id,id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let owner_bytes = row.map_or(0, |r|r.2);
        material.observe_projected(u64::try_from(owner_bytes.checked_add(id.len()).context("original Tool key bytes overflow")?)?
            .checked_mul(8).and_then(|n|n.checked_add(4096)).context("original Tool key copies overflow")?, 32)?;
        let key = Key::new(Family::ToolExecution, id);
        material.binding_metadata_tx(tx)?;
        let binding = binding_on(tx, scope)?;
        material.record_metadata_tx(tx, &binding, &key)?;
        if let Some(proof) = cleanup { proof.validate_key_tx(tx, &key)?; }
        else { validate_existing_input_evidence_tx(tx, scope, &key)?; }
        if !row.is_some_and(|r|r.0) { return Ok(()); }
        if let Some((true, bytes, _)) = row {
            // Count with the existing incremental visitor, then retire that
            // scratch before guarding the old Focus String+DTO allocation.
            let scratch = material.tool_scratch_mark();
            let nodes = tool_run_report::original_tool_body_nodes_on(tx, &scope.agent_id, id,
                |bytes, count| material.observe_projected(bytes, count));
            material.retire_tool_scratch(scratch, None)?;
            let nodes = nodes?;
            material.observe_projected(u64::try_from(bytes)?.checked_mul(2).and_then(|n|n.checked_add(4096))
                .context("original existing Tool raw copies overflow")?, 16)?;
            let nodes = nodes.checked_mul(2).context("original existing Tool native copies overflow")?;
            material.observe_projected(u64::try_from(nodes)?.checked_mul(256).context("original existing Tool native bytes overflow")?, nodes)?;
        }
        let fingerprint = fingerprint_on(tx, &scope.agent_id, &key)?.context("original Tool metadata disappeared")?;
        if let Some(source) = self.existing.borrow().get(id) {
            ensure!(source.fingerprint == fingerprint, InputEvidenceSnapshotError::ProjectionChanged(id.into()));
            return Ok(());
        }
        // An accepted fixed Tool without a canonical hash historically grants
        // its finite projection. Seal its actual original bytes now; do not
        // bind a later post-write row as that original source.
        let alias = alias_on(tx, scope, &key)?;
        let fixed = if alias.is_none() { require_eligible_metadata(tx, &binding, &key, &fingerprint)? }
            else { BTreeSet::new() };
        let scratch = material.tool_scratch_mark();
        let digest = tool_run_report::original_tool_blob_digest_on(tx, &scope.agent_id, &key, &fingerprint,
            |bytes, count| material.observe_projected(bytes, count));
        material.retire_tool_scratch(scratch, None)?;
        let digest = digest?;
        if let Some(alias) = alias {
            ensure!(alias.present && alias.fingerprint == fingerprint && alias.digest.as_deref().is_none_or(|d| d == digest.as_str()),
                InputEvidenceSnapshotError::ProjectionChanged(id.into()));
        } else if !fixed.is_empty() { validate_fixed_body_digest(&key, &fixed, &digest)?; }
        let (bytes,nodes) = crate::runtime::input_evidence_budget::borrowed_json_material(&(&fingerprint, &digest, id))?;
        material.observe_projected(bytes.checked_add(u64::try_from(nodes)?.checked_mul(256).context("original Tool seal native bytes overflow")?)
            .and_then(|n|n.checked_add(4096)).and_then(|n|n.checked_mul(8)).context("original Tool seal copies overflow")?,
            nodes.checked_add(8).and_then(|n|n.checked_mul(8)).context("original Tool seal native copies overflow")?)?;
        self.existing.borrow_mut().insert(id.into(), OriginalToolSource { fingerprint, digest, noop:false });
        Ok(())
    }
    fn mark_original_tool_noop(&self, tx: &Transaction<'_>, id: &str) -> Result<()> {
        self.validate_tx(tx)?;
        let mut existing = self.existing.borrow_mut();
        let source = existing.get_mut(id).context("original no-op Tool has no pre-business qualified source")?;
        source.noop = true;
        Ok(())
    }
    fn before_facts(&self, tx: &Transaction<'_>, agent: &str,
        tools: &[ToolExecutionRecord], capacity: usize,
        current: Option<&crate::runtime_db::current_read_snapshot::ObservedCurrentReadMaterial<'_>>) -> Result<()> {
        self.validate_tx(tx)?;
        let mut material = self.material.borrow_mut();
        material.validate_tool_agent(agent)?;
        if let Some(current) = current {
            let delta = current.after_admitted(self.current_usage.get())?;
            material.observe_current_prior(&delta)?;
            let delta = delta.validate_accounting(material.accounting_scope(), material.accounting_ledger())?;
            self.current_usage.set((self.current_usage.get().0.checked_add(delta.0).context("Current writer bytes overflow")?,
                self.current_usage.get().1.checked_add(delta.1).context("Current writer nodes overflow")?));
        }
        let mut actual = (0u64, 0usize);
        let mut metadata = u64::try_from(capacity.checked_mul(std::mem::size_of::<ToolExecutionRecord>())
            .context("Tool writer container overflow")?)?;
        for tool in tools {
            ensure!(tool.agent_id == agent, "original Tool fact crossed agent");
            let cost = original_tool_cost(tool)?;
            actual.0 = actual.0.checked_add(cost.0).context("actual Tool fact bytes overflow")?;
            actual.1 = actual.1.checked_add(cost.1).context("actual Tool fact nodes overflow")?;
            metadata = metadata.checked_add(u64::try_from(tool.id.len().checked_add(agent.len())
                .and_then(|n|n.checked_mul(8)).and_then(|n|n.checked_add(4096))
                .context("Tool writer locator copies overflow")?)?).context("Tool writer metadata overflow")?;
        }
        // Existing terminal interruption can append extra actual Tools. Their
        // original source query remains a separate required migration; their
        // actual readback cannot escape this phase's remaining allowance.
        let reserved = self.reserved.get();
        material.observe_projected(actual.0.saturating_sub(reserved.0).checked_add(metadata)
            .context("Tool writer combined bytes overflow")?,
            actual.1.saturating_sub(reserved.1).checked_add(tools.len().checked_mul(32)
                .context("Tool writer locator nodes overflow")?).context("Tool writer combined nodes overflow")?)?;
        self.reserved.set((actual.0.max(reserved.0), actual.1.max(reserved.1)));
        // Guard original continuity Strings before the D3 binding decoder.
        material.binding_metadata_tx(tx)?;
        Ok(())
    }
    fn register_tool(&self, tx: &Transaction<'_>, scope: &InputEvidenceScope,
        tool: &ToolExecutionRecord, cleanup: Option<&ExistingAdmissionInputProof>) -> Result<QualifiedInputEvidence> {
        self.validate_tx(tx)?;
        let mut material = self.material.borrow_mut();
        material.validate_binding(scope)?;
        ensure!(tool.agent_id == scope.agent_id, "original Tool readback crossed scope");
        material.binding_metadata_tx(tx)?;
        let binding = binding_on(tx, scope)?;
        let key = Key::new(Family::ToolExecution, &tool.id);
        material.record_metadata_tx(tx, &binding, &key)?;
        let fingerprint = fingerprint_on(tx, &scope.agent_id, &key)?.context("committed Tool metadata disappeared")?;
        let existing = self.existing.borrow();
        let original = existing.get(&tool.id).filter(|source| source.noop);
        let (digest,prompt) = if let Some(source) = original {
            ensure!(fingerprint == source.fingerprint, InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()));
            let prompt = tool_run_report::prepare_original_noop_tool_prompt_on(tx, scope, &key, source, &mut material)?;
            (source.digest.clone(),prompt)
        } else {
            let (digest, len) = original_tool_digest(tool)?;
            ensure!(fingerprint.payload_bytes == len
                && fingerprint.created_at == crate::runtime_db::repositories::timestamp(tool.created_at)
                && normalized_payload_digest(fingerprint.content_hash.as_deref().context("committed Tool canonical hash missing")?)? == digest,
                InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()));
            // Actual rewrites retain the strict current serializer check.
            let prompt = tool_run_report::prepare_authorized_tool_prompt_on(tx, scope, &key,
                &fingerprint, &digest, &mut material)?;
            (digest,prompt)
        };
        if let Some(owner) = cleanup.and_then(|proof| proof.derived_work_owners.get(&key)) {
            ensure!(tool.work_item_id.as_deref() == Some(owner.as_str()),
                "committed derived Tool differs from original Work admission");
        }
        let revision = upsert_alias_tx(tx, scope, &key, &fingerprint, Some(&digest), true,
            None, "committed_writer", binding.seed_id.as_deref())?;
        let has_canonical_hash = fingerprint.content_hash.is_some();
        let mut record = qualified(scope, key, revision, digest, fingerprint,
            PreparedInputEvidence::ToolExecution(prompt), None)?;
        // NoState/Deleting keep the original canonical ACK, never a new Ready
        // report. Active ordinary query publication remains strict elsewhere.
        let facts = material.scope_facts_tx(tx)?;
        if has_canonical_hash && facts.state.is_some() && facts.identity.as_ref().is_some_and(|identity| identity.status == AgentRegistryStatus::Active) {
            record.tool_run_report = Some(tool_run_report::prepare_qualified_tool_run_report_on(tx, &record, &mut material)?);
        }
        if original.is_some() {
            // Exact pre-business seal, actual kernel no-op and post-business
            // Blob SHA above precede this finite, guarded locator copy.
            let (bytes, nodes) = crate::runtime::input_evidence_budget::borrowed_json_material(&record.locator)?;
            material.observe_projected(bytes.checked_add(u64::try_from(nodes)?.checked_mul(256)
                .context("original no-op Tool locator native bytes overflow")?)
                .and_then(|n|n.checked_add(4096)).and_then(|n|n.checked_mul(8))
                .context("original no-op Tool locator copies overflow")?,
                nodes.checked_add(8).and_then(|n|n.checked_mul(8))
                    .context("original no-op Tool locator native copies overflow")?)?;
            let exact: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='tool_execution' AND record_id=?3 AND revision=?4 AND content_digest=?5 AND availability='present' AND qualification IN ('legacy_prepared','committed_writer'))",
                params![scope.agent_id,scope.identity_incarnation,record.locator.key.id,record.locator.revision,record.locator.content_digest],
                |row|row.get(0))?;
            ensure!(exact, "original no-op Tool lacks exact actual D3 readback");
            self.noop_readbacks.borrow_mut().push(OriginalToolNoopReadback {
                tx: self.tx, locator: record.locator.clone(),
            });
        }
        Ok(record)
    }
}

/// Same canonical serde producer as evidence::full_payload_json, streamed to a
/// constant-size SHA state. No complete payload String/Value is constructed.
fn original_tool_digest(tool: &ToolExecutionRecord) -> Result<(String, usize)> {
    use std::io::Write;
    struct Hash { digest: Sha256, bytes: usize }
    impl Write for Hash {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self.bytes.checked_add(bytes.len()).ok_or_else(||std::io::Error::other("Tool source hash bytes overflow"))?;
            self.digest.update(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut hash = Hash { digest: Sha256::new(), bytes: 0 };
    serde_json::to_writer(&mut hash, tool)?;
    Ok((format!("{:x}",hash.digest.finalize()), hash.bytes))
}
