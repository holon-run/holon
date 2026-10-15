//! Loaded compact JSON owns the original material through the last Bytes slice.
use super::*;
use crate::http_dto::SlimAgentDtoRef;
use crate::runtime::input_evidence_budget::serialized_bytes;

pub(crate) struct CompactAgentJson {
    bytes: Vec<u8>,
    _projection: LightweightAgentStateProjection,
    _encoded: Arc<BudgetViewGuard>,
}
impl AsRef<[u8]> for CompactAgentJson {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
struct CompactJsonPlan {
    encoded_len: usize,
    own: (u64, usize),
    total: (u64, usize),
}
impl LightweightAgentStateProjection {
    fn compact_json_plan(&self) -> Result<CompactJsonPlan> {
        let material = self
            ._ordinary_material
            .as_ref()
            .context("compact JSON requires its actual loaded material")?;
        let source = material.material_estimate()?;
        ensure!(
            self.agent == *material.read.state
                && self.identity.agent_id == material.identity.agent_id
                && self.identity.incarnation == material.identity.incarnation,
            CompactAgentReadError::ForeignMaterial
        );
        // The loaded aggregate already charged the actual DTO before copying.
        // Genuine cold inspection's first-copy source is a separate follow-up;
        // its already-owned output still shares this concrete encoder phase.
        let children = if material.children.is_some() || self.active_children.is_empty() {
            (0, 0)
        } else {
            copies(&self.active_children, 1)?
        };
        let encoded = serialized_bytes(&SlimAgentDtoRef(self))?;
        let own = add(
            children,
            (
                encoded
                    .checked_add(8192)
                    .context(CompactAgentReadError::ResourceExhausted)?,
                16,
            ),
        )?;
        Ok(CompactJsonPlan {
            encoded_len: usize::try_from(encoded)?,
            own,
            total: add(source, own)?,
        })
    }
    pub(crate) fn into_compact_json_wire(self) -> Result<CompactAgentJson> {
        self.into_compact_json_wire_with_limits((usize::MAX, usize::MAX))
    }
    fn into_compact_json_wire_with_limits(
        self,
        limits: (usize, usize),
    ) -> Result<CompactAgentJson> {
        let plan = self.compact_json_plan()?;
        let material = self._ordinary_material.as_ref().unwrap();
        admit(material_limits(&material.ledger, limits)?, plan.total)?;
        let encoded = material.ledger.register_owned_bytes(
            &material.read.work.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(plan.encoded_len)?,
                metadata_estimate_bytes: plan
                    .own
                    .0
                    .checked_sub(u64::try_from(plan.encoded_len)?)
                    .context(CompactAgentReadError::ResourceExhausted)?,
            },
        )?;
        // Same admitted fixed-capacity kernel as the original Full response;
        // the borrowed Slim projection constructs no intermediate DTO or Value.
        struct ExactJson {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl std::io::Write for ExactJson {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self
                    .bytes
                    .len()
                    .checked_add(bytes.len())
                    .is_none_or(|n| n > self.limit)
                {
                    return Err(std::io::Error::other(
                        "compact JSON exceeded its admitted length",
                    ));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = ExactJson {
            bytes: Vec::with_capacity(plan.encoded_len),
            limit: plan.encoded_len,
        };
        serde_json::to_writer(&mut writer, &SlimAgentDtoRef(&self))?;
        ensure!(
            writer.bytes.len() == plan.encoded_len,
            "compact JSON changed its admitted length"
        );
        Ok(CompactAgentJson {
            bytes: writer.bytes,
            _projection: self,
            _encoded: encoded,
        })
    }
}

#[cfg(test)]
#[path = "compact_agent_json_tests.rs"]
mod tests;
