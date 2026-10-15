//! Live selector loans share the original Input phase's actual ledger/Memory.
use super::*;
use crate::runtime::input_queries::InputQuerySnapshot;

impl InputReadMaterial<'_> {
    pub(in crate::runtime_db) fn observe_query_prior(
        &mut self,
        query: &InputQuerySnapshot,
        owner_epoch: u64,
    ) -> Result<()> {
        query.validate_binding(self.scope, owner_epoch, self.ledger)?;
        query.visit_material(|guard, bytes, records| {
            guard.validate_owned_material_binding(self.scope, self.ledger, bytes)?;
            // Exactly the same guard allocation may be lent twice by nested
            // original adapters. Its original cost is debited only once here.
            if self.guards.iter().any(|held| Arc::ptr_eq(held, guard)) {
                return Ok(());
            }
            let next_bytes = self.bytes.checked_add(usize::try_from(bytes)?).context(
                InputEvidenceSnapshotError::ResourceExhausted("Input query prior"),
            )?;
            let next_records = self.records.checked_add(records).context(
                InputEvidenceSnapshotError::ResourceExhausted("Input query prior"),
            )?;
            ensure!(
                next_bytes <= self.byte_limit && next_records <= self.record_limit,
                InputEvidenceSnapshotError::ResourceExhausted("Input query prior")
            );
            let observed = ObservedInputReadMaterial {
                scope: self.scope,
                ledger: self.ledger,
                guard: guard.clone(),
                bytes: usize::try_from(bytes)?,
                records,
            };
            if let Some(memory) = self.memory.as_deref_mut() {
                memory.additional_observed_input_material(&observed)?;
            }
            self.guards.push(observed.guard);
            self.bytes = next_bytes;
            self.records = next_records;
            Ok(())
        })
    }
}

#[cfg(test)]
mod original_phase_mark_tests {
    use super::*;
    use crate::runtime::{
        input_queries::{source_roles_for_fixture, InputQuerySpec},
        mutation_owner::MutationOwnerRegistry,
    };
    #[test]
    fn shared_actual_query_loans_cannot_identify_another_input_decoder_phase() -> Result<()> {
        let scope = InputEvidenceScope {
            agent_id: "actual-shared-query-phase".into(),
            identity_incarnation: 1,
        };
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let owners = MutationOwnerRegistry::default();
        let owner = owners.claim(&scope.agent_id, scope.identity_incarnation)?;
        let (reservation, lease, roles) = source_roles_for_fixture(
            &owner,
            &scope,
            InputQuerySpec {
                required_keys: &[],
                birth_after: Some(0),
            },
            &ledger,
            (64 * 1024 * 1024, 65536),
        )?;
        drop(roles);
        let source = reservation.input_query_source(&scope, &ledger, (64 * 1024 * 1024, 65536))?;
        let baseline = ledger.report()?.observed_bytes;
        let request = InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            byte_limit: 64 * 1024 * 1024,
            record_limit: 65536,
        };
        let first = InputReadMaterial::new_with_memory_and_query_prior(
            &scope,
            &request,
            &ledger,
            (0, 0),
            None,
            Some((source.roles(), source.owner_epoch())),
        )?;
        let mut second = InputReadMaterial::new_with_memory_and_query_prior(
            &scope,
            &request,
            &ledger,
            (0, 0),
            None,
            Some((source.roles(), source.owner_epoch())),
        )?;
        assert!(
            !Arc::ptr_eq(&first.guards[0], &second.guards[0]),
            "shared selector guard replaced the unique decoder phase anchor"
        );
        let mark = first.tool_scratch_mark();
        let before = second.usage();
        let error = second
            .retire_tool_scratch(mark, None)
            .err()
            .context("foreign decoder mark was accepted through shared selector loan")?;
        assert!(
            format!("{error:#}").contains("another Input phase"),
            "{error:#}"
        );
        assert_eq!(second.usage(), before);
        let own_mark = second.tool_scratch_mark();
        second.retire_tool_scratch(own_mark, None)?;
        assert_eq!(second.usage(), before);
        drop((first, second));
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop((source, lease, reservation));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
