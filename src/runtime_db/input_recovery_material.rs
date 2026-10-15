//! One original report recovery keeps its retained Source and fresh read together.
use super::super::{
    input_evidence_snapshot::read_material::InputReadMaterial,
    input_mutation_receipts::PreparedInputMutation,
};
use super::*;
use anyhow::Context;

/// Accounting only. The original attempt, receipt and owner group still grant
/// all recovery authority; this opaque phase cannot publish or mutate State.
pub(crate) struct OriginalInputRecoveryMaterial<'a> {
    input: InputReadMaterial<'a>,
    source_prior: (usize, usize),
}
impl<'a> OriginalInputRecoveryMaterial<'a> {
    #[cfg(test)]
    pub(crate) fn new(
        scope: &'a InputEvidenceScope,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        source_prior: (usize, usize),
        roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    ) -> Result<Self> {
        Self::new_with_input_source(scope, ledger, limits, source_prior, roles, None)
    }
    pub(crate) fn new_with_input_source(
        scope: &'a InputEvidenceScope,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        source_prior: (usize, usize),
        roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
        input_source: Option<&crate::runtime::mutation_owner::OriginalInputQuerySource>,
    ) -> Result<Self> {
        if let Some(source) = input_source {
            source.remaining_limits(scope, ledger, limits)?;
        }
        let mut current =
            crate::runtime::current_read_facts::CurrentReadRequest::current(limits.0, limits.1);
        let event_prior = roles
            .map(|roles| roles.prepare_request(scope, ledger, &mut current))
            .transpose()?
            .unwrap_or((0, 0));
        let prior = (
            source_prior
                .0
                .checked_add(event_prior.0)
                .context("fresh Source byte prior overflow")?,
            source_prior
                .1
                .checked_add(event_prior.1)
                .context("fresh Source node prior overflow")?,
        );
        let request = InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            byte_limit: limits.0,
            record_limit: limits.1,
        };
        Ok(Self {
            input: InputReadMaterial::new_with_memory_and_query_prior(
                scope,
                &request,
                ledger,
                prior,
                None,
                input_source.map(|source| (source.roles(), source.owner_epoch())),
            )?,
            source_prior,
        })
    }

    /// Complete scalar lengths precede any receipt JSON or owned Row/String.
    pub(crate) fn receipt_tx(
        &mut self,
        tx: &Transaction<'_>,
        prepared: &PreparedInputMutation,
    ) -> Result<()> {
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(family AS BLOB))+length(CAST(phase_version AS BLOB))+length(CAST(command_digest AS BLOB))+length(CAST(scope_digest AS BLOB))+length(CAST(scopes_json AS BLOB))+length(CAST(prepared_at AS BLOB))+length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB))+length(CAST(committed_at AS BLOB))+length(CAST(result_digest AS BLOB))),0),COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [prepared.command_nonce()], |r| Ok((r.get(0)?,r.get(1)?)),
        )?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("fresh original receipt material");
        self.input.observe_projected(
            u64::try_from(
                bytes
                    .checked_mul(8)
                    .and_then(|n| rows.checked_mul(8 * 256).and_then(|m| n.checked_add(m)))
                    .context(exhausted())?,
            )?,
            rows.checked_mul(8).context(exhausted())?,
        )?;
        let nodes: usize = tx.query_row(
            "SELECT (SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_mutation_receipts r,json_tree(r.scopes_json) j WHERE r.command_nonce=?1)+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_mutation_receipts r,json_tree(r.prepared_json) j WHERE r.command_nonce=?1)+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_mutation_receipts r,json_tree(r.outcome_json) j WHERE r.command_nonce=?1)",
            [prepared.command_nonce()], |r|r.get(0),
        )?;
        let nodes = nodes.checked_mul(8).context(exhausted())?;
        self.input.observe_projected(
            u64::try_from(nodes.checked_mul(256).context(exhausted())?)?,
            nodes,
        )
    }

    /// Confirmation may release a dropped consumer's Source. Borrow its actual
    /// post-confirmation cost instead of keeping that released body as prior.
    pub(crate) fn refresh_source_prior(&mut self, actual: (usize, usize)) -> Result<()> {
        self.input
            .replace_borrowed_recovery_prior(self.source_prior, actual)?;
        self.source_prior = actual;
        Ok(())
    }

    pub(crate) fn scope_facts_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<ContextScopeFacts> {
        self.input.validate_accounting(scope, ledger)?;
        // A retired original namespace still resolves to metadata. Do not read
        // a successor State to discover that original retirement.
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(agent_id AS BLOB))+length(CAST(payload_json AS BLOB))),0),COUNT(*) FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id], |r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("fresh original identity material");
        self.input.observe_projected(
            u64::try_from(bytes.checked_mul(8).context(exhausted())?)?,
            rows.checked_mul(8).context(exhausted())?,
        )?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities r,json_tree(r.payload_json) j WHERE r.agent_id=?1",
            [&scope.agent_id], |r|r.get(0),
        )?;
        let nodes = nodes.checked_mul(2).context(exhausted())?;
        self.input.observe_projected(
            u64::try_from(nodes.checked_mul(256).context(exhausted())?)?,
            nodes,
        )?;
        use rusqlite::OptionalExtension;
        let identity = tx
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                [&scope.agent_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|raw| super::super::repositories::decode_agent_identity_payload(&raw))
            .transpose()?;
        let Some(identity) = identity else {
            return Ok(ContextScopeFacts {
                identity: None,
                state: None,
                control_revision: None,
            });
        };
        ensure!(
            identity.agent_id == scope.agent_id,
            "fresh original identity scope differs"
        );
        if identity.incarnation != scope.identity_incarnation
            || !matches!(
                identity.status,
                AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
            )
        {
            return Ok(ContextScopeFacts {
                identity: Some(identity),
                state: None,
                control_revision: None,
            });
        }
        drop(identity);
        self.input.scope_facts_tx(tx)
    }

    pub(crate) fn pending_metadata_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<super::super::bootstrap_inputs::BootstrapPendingMetadata> {
        self.input.pending_metadata_tx(tx, scope, ledger, true)
    }

    /// Borrowed shape counts every object key without allocating JSON/Strings.
    /// Reusing this pure counter does not invoke any legacy import authority.
    pub(crate) fn projected(&mut self, value: &impl serde::Serialize) -> Result<()> {
        let (bytes, nodes) = crate::runtime_db::input_evidence_snapshot::legacy_delivery::original_episode::received_material(value)?;
        self.input.observe_projected(bytes, nodes)
    }

    pub(crate) fn prepare_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        state: &AgentState,
        request: &InputEvidenceLoadRequest,
        recent_episode_limit: usize,
        budget: &InputEvidenceBudgetLedger,
        work: &crate::runtime::resident_work::WorkCaptureRequest,
        tasks: &crate::runtime::resident_task::TaskCaptureRequest,
        query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
        roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    ) -> Result<PreparedInputRead> {
        prepare_original_input_recovery_using_material_tx(
            tx,
            scope,
            state,
            request,
            recent_episode_limit,
            budget,
            work,
            tasks,
            query,
            roles,
            &mut self.input,
        )
    }
}

#[cfg(test)]
#[path = "input_recovery_material_tests.rs"]
mod tests;
