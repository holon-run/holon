//! Borrowed original rejoin shapes, shared by local leases and durable targets.
use crate::types::{TaskRecord, TaskStatus};
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
    CommandCreation {
        obligation_id: &'a str,
        generation: u64,
        command_creation_id: &'a str,
    },
}
fn observe(task: &TaskRecord) -> Option<RejoinShape<'_>> {
    let field = |key: &str| task.detail.as_ref().and_then(|detail| detail.get(key));
    if field("rejoin_obligation_id").is_none() && field("rejoin_generation").is_none() {
        if field("command_creation_id").is_some() {
            return None;
        }
        // Parent-only provenance is not a rejoin generation or Result grant.
        return Some(RejoinShape::Unbound);
    }
    let obligation_id = field("rejoin_obligation_id")?.as_str()?;
    let generation = field("rejoin_generation")?.as_u64()?;
    if obligation_id.is_empty() || obligation_id != task.id || generation == 0 {
        return None;
    }
    match (field("parent_turn_id"), field("command_creation_id")) {
        (None, None) => Some(RejoinShape::CreatorPartial {
            obligation_id,
            generation,
        }),
        (Some(value), None) => {
            let parent_turn_id = value.as_str().filter(|value| !value.is_empty())?;
            Some(RejoinShape::Bound {
                obligation_id,
                generation,
                parent_turn_id,
            })
        }
        (None, Some(value)) if task.kind == crate::types::TaskKind::CommandTask => {
            let command_creation_id = value.as_str().filter(|value| *value == task.id)?;
            Some(RejoinShape::CommandCreation {
                obligation_id,
                generation,
                command_creation_id,
            })
        }
        _ => None,
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
        let legacy_compatible = matches!(shape, RejoinShape::Unbound | RejoinShape::Bound { .. });
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

/// Terminal publication cannot change creation provenance into a result Turn.
pub(crate) fn command_terminal_rejoin_matches(
    before: &TaskRecord,
    actual: &TaskRecord,
    message: Option<&crate::types::MessageEnvelope>,
) -> bool {
    if !same_task_rejoin_generation(before, actual)
        || before.id != actual.id
        || before.agent_id != actual.agent_id
        || before.kind != actual.kind
        || before.work_item_id != actual.work_item_id
    {
        return false;
    }
    match message {
        Some(message) => {
            message.kind == crate::types::MessageKind::TaskResult
                && message.task_id.as_deref() == Some(before.id.as_str())
                && message.agent_id == before.agent_id
                && message.work_item_id == before.work_item_id
                && actual.parent_message_id.as_deref() == Some(message.id.as_str())
                && matches!(&message.origin, crate::types::MessageOrigin::Task { task_id }
                    if task_id == &before.id)
                && message.turn_id.as_deref().is_some_and(|turn_id| {
                    !turn_id.is_empty()
                        && actual
                            .detail
                            .as_ref()
                            .and_then(|detail| detail.get("result_execution_turn_id"))
                            .and_then(Value::as_str)
                            == Some(turn_id)
                })
        }
        None => before.parent_message_id == actual.parent_message_id,
    }
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
        Some(RejoinShape::CreatorPartial { .. } | RejoinShape::CommandCreation { .. }) | None => {
            false
        }
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
    if !matches!(
        task.status,
        TaskStatus::Interrupted
            | TaskStatus::Cancelled
            | TaskStatus::Completed
            | TaskStatus::Failed
    ) {
        return false;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TaskKind,
    };

    #[test]
    fn turnless_command_preserves_only_its_original_creation_source() {
        let at = chrono::Utc::now();
        let before = TaskRecord {
            id: "command".into(),
            agent_id: "owner".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: Some("work".into()),
            summary: None,
            detail: Some(serde_json::json!({
                "rejoin_obligation_id": "command",
                "rejoin_generation": 1,
                "command_creation_id": "command",
            })),
            recovery: None,
        };
        let mut message = MessageEnvelope::new(
            "owner",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: before.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "completed".into(),
            },
        );
        message.task_id = Some(before.id.clone());
        message.work_item_id = before.work_item_id.clone();
        message.turn_id = Some("result-turn".into());
        let mut terminal = before.clone();
        terminal.status = TaskStatus::Completed;
        terminal.parent_message_id = Some(message.id.clone());
        terminal.detail.as_mut().unwrap()["result_execution_turn_id"] =
            serde_json::json!("result-turn");
        assert!(command_terminal_rejoin_matches(
            &before,
            &terminal,
            Some(&message)
        ));
        assert!(same_task_rejoin_generation(&before, &terminal));
        assert!(!control_original_rejoin_matches(
            &before,
            &serde_json::json!({"rejoin": null}),
        ));
        assert!(!command_terminal_rejoin_matches(&before, &terminal, None));

        for (field, value) in [
            ("rejoin_obligation_id", serde_json::json!("another-command")),
            ("rejoin_generation", serde_json::json!(2)),
            ("command_creation_id", serde_json::json!("another-command")),
            ("parent_turn_id", serde_json::json!("another-turn")),
        ] {
            let mut other = terminal.clone();
            other.detail.as_mut().unwrap()[field] = value;
            assert!(!command_terminal_rejoin_matches(
                &before,
                &other,
                Some(&message)
            ));
        }
        for field in ["parent_message_id", "work_item_id"] {
            let mut other = terminal.clone();
            if field == "parent_message_id" {
                other.parent_message_id = Some("another-result".into());
            } else {
                other.work_item_id = Some("another-work".into());
            }
            assert!(!command_terminal_rejoin_matches(
                &before,
                &other,
                Some(&message)
            ));
        }
        for field in ["task_id", "agent_id", "work_item_id", "origin"] {
            let mut other = message.clone();
            match field {
                "task_id" => other.task_id = Some("another-command".into()),
                "agent_id" => other.agent_id = "another-owner".into(),
                "work_item_id" => other.work_item_id = Some("another-work".into()),
                _ => {
                    other.origin = MessageOrigin::Task {
                        task_id: "another-command".into(),
                    }
                }
            }
            assert!(!command_terminal_rejoin_matches(
                &before,
                &terminal,
                Some(&other)
            ));
        }
        let mut rebound = terminal.clone();
        let mut next = message.clone();
        next.turn_id = Some("next-result-turn".into());
        rebound.detail.as_mut().unwrap()["parent_turn_id"] = serde_json::json!("next-result-turn");
        assert!(!command_terminal_rejoin_matches(
            &terminal,
            &rebound,
            Some(&next)
        ));

        let mut partial = before.clone();
        partial
            .detail
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("command_creation_id");
        let mut synthesized = partial.clone();
        synthesized.detail.as_mut().unwrap()["parent_turn_id"] = serde_json::json!("result-turn");
        assert!(!same_task_rejoin_generation(&partial, &before));
        assert!(!command_terminal_rejoin_matches(
            &partial,
            &synthesized,
            Some(&message)
        ));

        for invalid_source in [
            serde_json::Value::Null,
            serde_json::json!("foreign"),
            serde_json::json!(""),
        ] {
            let mut invalid = before.clone();
            invalid.detail.as_mut().unwrap()["command_creation_id"] = invalid_source;
            assert!(TaskRejoinGeneration::of(&invalid).is_none());
        }
        let mut non_command = before.clone();
        non_command.kind = TaskKind::ActorInvocation;
        assert!(TaskRejoinGeneration::of(&non_command).is_none());

        let mut unbound = before.clone();
        unbound.detail = Some(serde_json::json!({"command_creation_id": "command"}));
        assert!(TaskRejoinGeneration::of(&unbound).is_none());

        let original = TaskRejoinGeneration::of(&before).unwrap();
        let mut control = serde_json::json!({
            (CONTROL_ORIGINAL_REJOIN): original,
            (CONTROL_TERMINAL_REJOIN): TaskRejoinGeneration::of(&terminal).unwrap(),
        });
        assert!(control_original_rejoin_matches(&before, &control));
        assert!(control_terminal_rejoin_matches(&terminal, &control));
        control[CONTROL_ORIGINAL_REJOIN] =
            serde_json::to_value(TaskRejoinGeneration::of(&partial).unwrap()).unwrap();
        assert!(!control_original_rejoin_matches(&before, &control));
    }
}
