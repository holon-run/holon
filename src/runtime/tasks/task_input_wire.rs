//! The encoded input reply keeps its actual material through the last Bytes.
use super::*;
use crate::runtime::input_evidence_budget::serialized_bytes;
pub(crate) struct TaskInputJson {
    bytes: Vec<u8>,
    _read: OwnedTaskInputResult,
    _encoded: Arc<BudgetViewGuard>,
}
impl AsRef<[u8]> for TaskInputJson {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl OwnedTaskInputResult {
    pub(crate) fn into_json_wire(self) -> Result<TaskInputJson> {
        self.into_json_wire_with_limits((usize::MAX, usize::MAX))
    }
    pub(super) fn into_json_wire_with_limits(
        self,
        limits: (usize, usize),
    ) -> Result<TaskInputJson> {
        anyhow::ensure!(
            self.material.matches_resource_ledger(&self.ledger)
                && self.material.owned_material_bytes()? == self.usage.0,
            "TaskInput result material belongs to another actual ledger"
        );
        let encoded = serialized_bytes(&self.result)?;
        let own = encoded
            .checked_add(4096)
            .context("TaskInput wire bytes overflow")?;
        let total = (
            self.usage
                .0
                .checked_add(own)
                .context("TaskInput wire total overflow")?,
            self.usage
                .1
                .checked_add(16)
                .context("TaskInput wire nodes overflow")?,
        );
        let configured = self.ledger.limits();
        let cap = (
            configured
                .history_per_agent_bytes
                .checked_add(configured.pending_per_agent_bytes)
                .context("TaskInput wire cap overflow")?
                .min(64 * 1024 * 1024)
                .min(u64::try_from(limits.0)?),
            usize::try_from(configured.record_limit)?
                .min(65536)
                .min(limits.1),
        );
        anyhow::ensure!(
            total.0 <= cap.0 && total.1 <= cap.1,
            TaskReadError::ResourceExhausted("TaskInput result and wire")
        );
        let guard = self.ledger.register_host_owned_bytes(
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: encoded,
                metadata_estimate_bytes: 4096,
            },
        )?;
        struct ExactJson {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl std::io::Write for ExactJson {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                if self
                    .bytes
                    .len()
                    .checked_add(b.len())
                    .is_none_or(|n| n > self.limit)
                {
                    return Err(std::io::Error::other(
                        "TaskInput JSON exceeded admitted length",
                    ));
                }
                self.bytes.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let size = usize::try_from(encoded)?;
        let mut writer = ExactJson {
            bytes: Vec::with_capacity(size),
            limit: size,
        };
        serde_json::to_writer(&mut writer, &self.result)?;
        anyhow::ensure!(
            writer.bytes.len() == size,
            "TaskInput JSON changed admitted length"
        );
        Ok(TaskInputJson {
            bytes: writer.bytes,
            _read: self,
            _encoded: guard,
        })
    }
}
