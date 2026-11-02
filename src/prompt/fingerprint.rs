//! Borrow section text while preserving the legacy JSON Value key order.

use super::{
    execution_semantic_cache_payload, ExecutionSnapshot, PromptSection, PromptStability, ToolSpec,
};
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct CanonicalSection<'a> {
    content: &'a str,
    id: &'a str,
    name: &'a str,
    stability: PromptStability,
}

struct Sections<'a> {
    sections: &'a [PromptSection],
    stable_only: bool,
}

impl Serialize for Sections<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for section in self.sections {
            if self.stable_only && section.stability == PromptStability::TurnScoped {
                continue;
            }
            sequence.serialize_element(&CanonicalSection {
                content: &section.content,
                id: &section.id,
                name: &section.name,
                stability: section.stability,
            })?;
        }
        sequence.end()
    }
}

// Fields are alphabetical, as in the legacy json! -> Value -> JSON encoding.
#[derive(Serialize)]
struct ContextPayload<'a> {
    agent_id: &'a str,
    compression_epoch: u64,
    context_sections: Sections<'a>,
    execution_semantics: Value,
    system_sections: Sections<'a>,
    tools: Value,
}

#[derive(Serialize)]
struct ScopePayload<'a> {
    agent_id: &'a str,
    execution_semantics: Value,
    stable_context_sections: Sections<'a>,
    stable_system_sections: Sections<'a>,
    tools: Value,
}

pub(super) fn context(
    agent_id: &str,
    compression_epoch: u64,
    execution: &ExecutionSnapshot,
    system: &[PromptSection],
    context: &[PromptSection],
    tools: &[ToolSpec],
) -> String {
    digest(&ContextPayload {
        agent_id,
        compression_epoch,
        context_sections: Sections {
            sections: context,
            stable_only: false,
        },
        execution_semantics: execution_semantic_cache_payload(execution),
        system_sections: Sections {
            sections: system,
            stable_only: false,
        },
        tools: serde_json::to_value(tools).expect("prompt tools should serialize"),
    })
}

pub(super) fn scope(
    agent_id: &str,
    execution: &ExecutionSnapshot,
    system: &[PromptSection],
    context: &[PromptSection],
    tools: &[ToolSpec],
) -> String {
    digest(&ScopePayload {
        agent_id,
        execution_semantics: execution_semantic_cache_payload(execution),
        stable_context_sections: Sections {
            sections: context,
            stable_only: true,
        },
        stable_system_sections: Sections {
            sections: system,
            stable_only: true,
        },
        tools: serde_json::to_value(tools).expect("prompt tools should serialize"),
    })
}

pub(super) fn digest(value: &impl Serialize) -> String {
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
    let mut writer = HashWriter(Sha256::new());
    {
        use std::io::Write;
        // Batch small JSON fragments, without buffering the complete prompt.
        let mut buffered = std::io::BufWriter::with_capacity(8192, &mut writer);
        serde_json::to_writer(&mut buffered, value).expect("prompt fingerprint should serialize");
        buffered.flush().expect("prompt fingerprint should flush");
    }
    format!("{:x}", writer.0.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_sections_preserve_legacy_json_bytes_and_stability_filter() {
        let sections = [
            PromptStability::Stable,
            PromptStability::AgentScoped,
            PromptStability::TurnScoped,
        ]
        .into_iter()
        .map(|stability| PromptSection {
            content: "\"\\\n\t\u{0000}中文🙂".into(),
            id: "id".into(),
            name: "name".into(),
            stability,
        })
        .collect::<Vec<_>>();
        for stable_only in [false, true] {
            let legacy = sections
                .iter()
                .filter(|s| !stable_only || s.stability != PromptStability::TurnScoped)
                .collect::<Vec<_>>();
            let expected = serde_json::to_vec(&serde_json::to_value(legacy).unwrap()).unwrap();
            assert_eq!(
                serde_json::to_vec(&Sections {
                    sections: &sections,
                    stable_only
                })
                .unwrap(),
                expected
            );
        }
        assert_eq!(
            serde_json::to_vec(&Sections {
                sections: &[],
                stable_only: true
            })
            .unwrap(),
            b"[]"
        );
    }
}
