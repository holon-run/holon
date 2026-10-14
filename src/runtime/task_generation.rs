//! Borrowed original rejoin shapes, shared by local leases and durable targets.
use crate::types::TaskRecord;
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write;

pub(crate) const CONTROL_ORIGINAL_REJOIN: &str = "task_rejoin_v1";
pub(crate) const CONTROL_TERMINAL_REJOIN: &str = "terminal_task_rejoin_v1";

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RejoinShape<'a> {
    Unbound,
    CreatorPartial {
        obligation_id: &'a str,
        generation: u64,
    },
    Bound {
        obligation_id: &'a str,
        generation: u64,
        parent_turn_id: &'a str,
    },
}
fn observe(task: &TaskRecord) -> Option<RejoinShape<'_>> {
    let field = |key: &str| task.detail.as_ref().and_then(|detail| detail.get(key));
    if field("rejoin_obligation_id").is_none() && field("rejoin_generation").is_none() {
        // Parent-only provenance is not a rejoin generation or Result grant.
        return Some(RejoinShape::Unbound);
    }
    let obligation_id = field("rejoin_obligation_id")?.as_str()?;
    let generation = field("rejoin_generation")?.as_u64()?;
    if obligation_id.is_empty() || obligation_id != task.id || generation == 0 {
        return None;
    }
    match field("parent_turn_id") {
        None => Some(RejoinShape::CreatorPartial {
            obligation_id,
            generation,
        }),
        Some(value) => {
            let parent_turn_id = value.as_str().filter(|value| !value.is_empty())?;
            Some(RejoinShape::Bound {
                obligation_id,
                generation,
                parent_turn_id,
            })
        }
    }
}

/// Fixed-size private observation. No Task body, Root, lease or storage back-edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TaskRejoinGeneration {
    digest: [u8; 32],
    legacy_compatible: bool,
}
impl TaskRejoinGeneration {
    pub(crate) fn of(task: &TaskRecord) -> Option<Self> {
        struct HashWriter(Sha256);
        impl Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let shape = observe(task)?;
        let legacy_compatible = !matches!(shape, RejoinShape::CreatorPartial { .. });
        let mut writer = HashWriter(Sha256::new());
        writer.0.update(b"holon/original-task-rejoin/v1\0");
        serde_json::to_writer(&mut writer, &shape).ok()?;
        Some(Self {
            digest: writer.0.finalize().into(),
            legacy_compatible,
        })
    }
    fn hex(&self) -> [u8; 64] {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = [0; 64];
        for (index, byte) in self.digest.iter().enumerate() {
            out[index * 2] = DIGITS[usize::from(*byte >> 4)];
            out[index * 2 + 1] = DIGITS[usize::from(*byte & 15)];
        }
        out
    }
    pub(crate) fn matches_value(&self, value: &Value) -> bool {
        value
            .as_str()
            .is_some_and(|value| value.as_bytes() == self.hex().as_slice())
    }
    /// A legacy null partial marker cannot authorize a new association.
    pub(crate) fn matches_control_original(&self, generation: &Value) -> bool {
        if has_explicit_generation(generation) {
            generation.get(CONTROL_TERMINAL_REJOIN).is_some()
                && generation
                    .get(CONTROL_ORIGINAL_REJOIN)
                    .is_some_and(|v| self.matches_value(v))
        } else {
            self.legacy_compatible
        }
    }
}
impl Serialize for TaskRejoinGeneration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex = self.hex();
        serializer.serialize_str(std::str::from_utf8(&hex).expect("hex is ASCII"))
    }
}
pub(crate) fn same_task_rejoin_generation(before: &TaskRecord, actual: &TaskRecord) -> bool {
    TaskRejoinGeneration::of(before)
        .is_some_and(|before| Some(before) == TaskRejoinGeneration::of(actual))
}
fn has_explicit_generation(generation: &Value) -> bool {
    generation.get(CONTROL_ORIGINAL_REJOIN).is_some()
        || generation.get(CONTROL_TERMINAL_REJOIN).is_some()
}
fn legacy_rejoin_matches(task: &TaskRecord, generation: &Value) -> bool {
    match observe(task) {
        Some(RejoinShape::Unbound) => generation.get("rejoin") == Some(&Value::Null),
        Some(RejoinShape::Bound {
            obligation_id,
            generation: number,
            parent_turn_id,
        }) => generation
            .get("rejoin")
            .and_then(Value::as_object)
            .is_some_and(|saved| {
                saved.len() == 3
                    && saved.get("obligation_id").and_then(Value::as_str) == Some(obligation_id)
                    && saved.get("generation").and_then(Value::as_u64) == Some(number)
                    && saved.get("parent_turn_id").and_then(Value::as_str) == Some(parent_turn_id)
            }),
        Some(RejoinShape::CreatorPartial { .. }) | None => false,
    }
}
pub(crate) fn control_original_rejoin_matches(task: &TaskRecord, generation: &Value) -> bool {
    let Some(original) = TaskRejoinGeneration::of(task) else {
        return false;
    };
    if has_explicit_generation(generation) {
        original.matches_control_original(generation)
    } else {
        legacy_rejoin_matches(task, generation)
    }
}
pub(crate) fn control_terminal_rejoin_matches(task: &TaskRecord, generation: &Value) -> bool {
    if has_explicit_generation(generation) {
        generation.get(CONTROL_ORIGINAL_REJOIN).is_some()
            && TaskRejoinGeneration::of(task).is_some_and(|actual| {
                generation
                    .get(CONTROL_TERMINAL_REJOIN)
                    .is_some_and(|value| actual.matches_value(value))
            })
    } else {
        legacy_rejoin_matches(task, generation)
    }
}
