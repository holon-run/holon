//! Actual metadata which the original Input publication will copy, without a Root backref.
use super::*;
use crate::runtime::input_evidence_budget::{borrowed_json_material, OwnedBytesEstimate};
use crate::runtime::input_queries::{actual_query_material_limits, InputQueryError};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub(in crate::runtime) struct OriginalInputPublicationMaterial {
    scope: InputEvidenceScope,
    bytes: u64,
    nodes: usize,
    digest: [u8; 32],
    publication: (u64, usize),
    guard: Arc<BudgetViewGuard>,
}
impl OriginalInputPublicationMaterial {
    pub(in crate::runtime) fn material_estimate(&self) -> (u64, usize) {
        (self.bytes, self.nodes)
    }
    pub(in crate::runtime) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope,
            "Input publication material belongs to another scope"
        );
        self.guard
            .validate_owned_material_binding(scope, ledger, self.bytes)?;
        let cost = self.publication_copy_estimate();
        ensure!(cost.0 > 0 && cost.1 > 0, "Input publication forecast is incomplete");
        Ok(())
    }
    fn publication_copy_estimate(&self) -> (u64, usize) {
        self.publication
    }
    // This actual-copy fixture exercises the future consumer boundary without
    // enabling the original writer installer or inventing a Ready frontier.
    #[cfg(test)]
    fn copy_for_test(&self, inputs:&ResidentInputEvidence,
        ledger:&InputEvidenceBudgetLedger, limits:(usize,usize), prior:(u64,usize),
    ) -> Result<(ResidentInputEvidence,Arc<BudgetViewGuard>)> {
        ensure!(self.matches_original(inputs,ledger)?,"Input publication source changed before copy");
        let limits=actual_query_material_limits(ledger,limits)?;
        let cost=self.publication_copy_estimate();
        ensure!(prior.0.checked_add(cost.0).is_some_and(|n|n<=limits.0 as u64)
            && prior.1.checked_add(cost.1).is_some_and(|n|n<=limits.1),
            InputQueryError::ResourceExhausted("publication metadata"));
        let guard=ledger.register_owned_bytes(&self.scope,InputMemoryCategory::Candidate,
            OwnedBytesEstimate{payload_bytes:0,metadata_estimate_bytes:cost.0})?;
        Ok((inputs.clone(),guard))
    }
    pub(in crate::runtime) fn visit_material(
        &self,
        visit: &mut impl FnMut(&Arc<BudgetViewGuard>, u64, usize) -> Result<()>,
    ) -> Result<()> {
        visit(&self.guard, self.bytes, self.nodes)
    }
    pub(in crate::runtime) fn matches_original(
        &self,
        inputs: &ResidentInputEvidence,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<bool> {
        self.validate_binding(inputs.scope(), ledger)?;
        Ok(self.digest == inputs.publication_digest()?)
    }
}
struct BorrowedPublicationKeys<'a>(&'a ResidentInputEvidence);
impl Serialize for BorrowedPublicationKeys<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.0
                .records
                .iter()
                .map(|(key, record)| (key, &record.locator)),
        )
    }
}
struct HashWriter(Sha256);
impl std::io::Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl ResidentInputEvidence {
    fn publication_metadata(&self) -> impl Serialize + '_ {
        (
            &self.scope,
            &self.coverage,
            &self.output_recall_roots,
            &self.output_recall_task,
            BorrowedPublicationKeys(self),
        )
    }
    fn publication_digest(&self) -> Result<[u8; 32]> {
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, &self.publication_metadata())?;
        // Typed owning edges are borrowed; neither canonical body nor stdout is
        // serialized/rehydrated. Length framing keeps arbitrary/NUL IDs exact.
        for record in self.records.values() {
            record.visit_references(|family, id| {
                writer.0.update(family.as_str().as_bytes());
                writer.0.update((id.len() as u64).to_le_bytes());
                writer.0.update(id.as_bytes());
            });
        }
        Ok(writer.0.finalize().into())
    }
    pub(in crate::runtime) fn original_publication_material(
        &self,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<OriginalInputPublicationMaterial> {
        ensure!(
            self.shares_query_ledger(ledger),
            "Input publication material ledger differs"
        );
        let limits = actual_query_material_limits(ledger, limits)?;
        let (encoded, native) = borrowed_json_material(&self.publication_metadata())?;
        let mut edge_bytes = 0u64;
        let mut edge_nodes = 0usize;
        let mut overflow = false;
        for record in self.records.values() {
            record.visit_references(|_, id| {
                match edge_bytes
                    .checked_add(id.len() as u64)
                    .and_then(|n| n.checked_add(128))
                {
                    Some(value) => edge_bytes = value,
                    None => overflow = true,
                }
                match edge_nodes.checked_add(6) {
                    Some(value) => edge_nodes = value,
                    None => overflow = true,
                }
            });
        }
        ensure!(
            !overflow,
            InputQueryError::ResourceExhausted("publication metadata")
        );
        // One metadata clone and the original bounded selector/closure working
        // sets coexist. All object keys, slots and actual owning edges count.
        let nodes = native
            .checked_add(edge_nodes)
            .and_then(|n| n.checked_add(32))
            .and_then(|n| n.checked_mul(3))
            .context(InputQueryError::ResourceExhausted("publication metadata"))?;
        let bytes = encoded
            .checked_add(edge_bytes)
            .and_then(|n| n.checked_add(4096))
            .and_then(|n| n.checked_mul(3))
            .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
            .context(InputQueryError::ResourceExhausted("publication metadata"))?;
        let publication = (bytes, nodes);

        // Only this finite descriptor exists during Source capture. The future
        // publication copy must be admitted at its actual final-phase lifetime.
        let (encoded, nodes) = borrowed_json_material(&(
            &self.scope,
            &publication,
            &[0u8; 32],
        ))?;
        let nodes = nodes.checked_add(8)
            .context(InputQueryError::ResourceExhausted("publication metadata"))?;
        let bytes = encoded
            .checked_add(std::mem::size_of::<OriginalInputPublicationMaterial>() as u64)
            .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
            .context(InputQueryError::ResourceExhausted("publication metadata"))?;
        ensure!(
            prior.0.checked_add(bytes).is_some_and(|n| n <= limits.0 as u64)
                && prior.1.checked_add(nodes).is_some_and(|n| n <= limits.1),
            InputQueryError::ResourceExhausted("publication metadata")
        );
        let guard = ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        // The guard precedes the sole actual scope copy; hash state/output is fixed.
        Ok(OriginalInputPublicationMaterial {
            scope: self.scope.clone(),
            bytes,
            nodes,
            digest: self.publication_digest()?,
            publication,
            guard,
        })
    }
}

#[cfg(test)]
#[path = "input_publication_material_tests.rs"]
mod tests;
