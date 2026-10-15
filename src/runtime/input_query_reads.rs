//! Selected Input query metadata; no ordinary read prepares SQL or raw rows.
use super::*;
use crate::runtime::input_queries::{InputQueryCoverage, InputQueryError, InputQueryLease};

fn remaining_query_material(
    limits: (usize, usize),
    prior: (u64, usize),
    selected: (u64, usize),
) -> Result<(usize, usize)> {
    let byte_limit = limits.0.min(64 * 1024 * 1024);
    let record_limit = limits.1.min(65_536);
    let used_bytes =
        prior
            .0
            .checked_add(selected.0)
            .context(InputQueryError::ResourceExhausted(
                "selected metadata bytes",
            ))?;
    let used_records =
        prior
            .1
            .checked_add(selected.1)
            .context(InputQueryError::ResourceExhausted(
                "selected metadata records",
            ))?;
    ensure!(
        used_records < record_limit && used_bytes < u64::try_from(byte_limit)?,
        InputQueryError::ResourceExhausted("held lease and selected metadata")
    );
    Ok((
        byte_limit - usize::try_from(used_bytes)?,
        record_limit - used_records,
    ))
}

impl ResidentInputEvidence {
    pub(in crate::runtime) fn capture_query_selection(
        &self,
        lease: &InputQueryLease,
        prior: (u64, usize),
    ) -> Result<(
        CapturedInputEvidence,
        InputQueryCoverage,
        Arc<BudgetViewGuard>,
    )> {
        let (input, query, guard, _) =
            self.capture_query_selection_with_material(lease, (usize::MAX, usize::MAX), prior)?;
        Ok((input, query, guard))
    }
    pub(in crate::runtime) fn capture_query_selection_with_material(
        &self,
        lease: &InputQueryLease,
        limits: (usize, usize),
        caller_prior: (u64, usize),
    ) -> Result<(
        CapturedInputEvidence,
        InputQueryCoverage,
        Arc<BudgetViewGuard>,
        (u64, usize),
    )> {
        let selection = lease.selection();
        self.coverage.query.validate_selection(selection)?;
        let ledger = self
            .budget
            .as_ref()
            .context("Input query actual ledger absent")?;
        lease.validate_material_binding(&self.scope, ledger)?;
        let held = lease.material_estimate();
        let prior = (
            caller_prior
                .0
                .checked_add(held.0)
                .context(InputQueryError::ResourceExhausted("held query bytes"))?,
            caller_prior
                .1
                .checked_add(held.1)
                .context(InputQueryError::ResourceExhausted("held query records"))?,
        );
        // Only borrowed sizes precede this observation. It covers the new
        // selector set, metadata maps/Strings and requested Arc slots.
        let (encoded, nodes) = super::super::input_evidence_budget::borrowed_json_material(&(
            &self.scope,
            selection,
            &self.coverage.query,
        ))?;
        // Count every original coverage key, including other roles' required
        // keys that the original clone retains until the subsequent narrowing.
        let count = nodes
            .checked_add(16)
            .and_then(|n| n.checked_mul(8))
            .context(InputQueryError::ResourceExhausted("selected native nodes"))?;
        let bytes = encoded
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(u64::try_from(count).ok()?.checked_mul(256)?))
            .context(InputQueryError::ResourceExhausted("selected native bytes"))?;
        let actual = ledger.limits();
        let limits = (
            // The Source's logical selected-body policy stays in the request;
            // only the explicit phase limit and actual ledger bound metadata.
            limits.0.min(usize::try_from(
                actual
                    .history_per_agent_bytes
                    .checked_add(actual.pending_per_agent_bytes)
                    .context(InputQueryError::ResourceExhausted("actual byte limit"))?,
            )?),
            limits.1.min(usize::try_from(actual.record_limit)?),
        );
        remaining_query_material(limits, prior, (bytes, count))?;
        let guard = ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let mut roots = selection.required_keys.clone();
        if let Some(after) = selection.birth_after {
            roots.extend(
                self.coverage
                    .query
                    .born_key_seqs
                    .iter()
                    .filter(|(_, seq)| **seq > after)
                    .map(|(key, _)| key.clone()),
            );
        }
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: roots,
            interrupted_sources: BTreeSet::new(),
            selected_completion_brief_ids: BTreeSet::new(),
            record_limit: self.coverage.record_limit,
            byte_limit: self.coverage.byte_limit,
        };
        // The Source already retained these qualified roots and their explicit
        // tombstones. This report does not upgrade them to business mandatory.
        let phase_prior = super::super::run_birth_sessions::add_material(prior, (bytes, count))?;
        let (capture, material) =
            self.capture_for_request_with_material(&request, true, limits, phase_prior, ledger)?;
        let mut query = self.coverage.query.clone();
        query.required_keys = selection.required_keys.clone();
        query.birth_after = selection.birth_after;
        query
            .born_key_seqs
            .retain(|_, seq| selection.birth_after.is_some_and(|after| *seq > after));
        let own = super::super::run_birth_sessions::add_material(
            super::super::run_birth_sessions::add_material(held, (bytes, count))?,
            material.material_estimate()?,
        )?;
        Ok((capture, query, guard, own))
    }
}

#[cfg(test)]
mod lease_material_tests {
    use super::*;
    use crate::runtime::input_queries::{InputQueryRegistry, InputQuerySpec};

    #[test]
    fn actual_input_lease_is_part_of_the_first_selected_metadata_allowance() -> Result<()> {
        let scope = InputEvidenceScope {
            agent_id: "actual-query-material".into(),
            identity_incarnation: 1,
        };
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let registry = InputQueryRegistry::default();
        let lease = registry.register(
            &scope,
            1,
            InputQuerySpec::default(),
            &ledger,
            (1024 * 1024, 4096),
        )?;
        lease.validate_material_binding(&scope, &ledger)?;
        let original = lease.material_estimate();
        let new_metadata = (original.0, original.1);
        let cap = usize::try_from(original.0 + new_metadata.0 - 1)?;
        assert!(original.0 < u64::try_from(cap)? && new_metadata.0 < u64::try_from(cap)?);
        let baseline = ledger.report()?.observed_bytes;
        let error = remaining_query_material((cap, 4096), original, new_metadata)
            .err()
            .context("actual held lease was given an independent full allowance")?;
        assert!(matches!(
            error.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::ResourceExhausted(_))
        ));
        assert_eq!(
            ledger.report()?.observed_bytes,
            baseline,
            "borrowed prior cannot register its original guard again"
        );
        let full = (64 * 1024 * 1024, 65536);
        let remaining = remaining_query_material(full, original, new_metadata)?;
        assert_eq!(
            remaining.0 + usize::try_from(original.0 + new_metadata.0)?,
            full.0
        );
        assert!(
            remaining_query_material((80 * 1024 * 1024, 100000), (64 * 1024 * 1024, 1), (0, 0))
                .is_err(),
            "clamp precedes prior subtraction"
        );
        let other = InputEvidenceBudgetLedger::new(Default::default())?;
        assert!(lease.validate_material_binding(&scope, &other).is_err());
        assert_eq!(other.report()?.observed_bytes, 0);
        drop(lease);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
