//! Borrow section text while preserving canonical JSON Value key order.

use crate::{
    render_sections, shared_prefix_len, PromptCacheIdentity, PromptCacheScopeDiagnostics,
    PromptLayout, PromptSection, PromptStability,
};
use holon_domain::{execution_types::ExecutionSnapshot, AgentState, ToolSpec};
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct CanonicalSection<'a> {
    content: &'a str,
    id: &'a str,
    layout: PromptLayout,
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
                layout: section.layout,
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

pub fn context(
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

pub fn scope(
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

pub fn digest(value: &impl Serialize) -> String {
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

/// Observability only: this digest never replaces an agent-scoped cache identity.
pub fn shared_prefix_fingerprint(sections: &[PromptSection]) -> String {
    digest(&render_sections(&sections[..shared_prefix_len(sections)]))
}

pub fn prompt_cache_scope_fingerprint(
    session: &AgentState,
    execution: &ExecutionSnapshot,
    system_sections: &[PromptSection],
    context_sections: &[PromptSection],
    available_tools: &[ToolSpec],
) -> String {
    prompt_cache_scope_fingerprint_for_agent(
        &session.id,
        execution,
        system_sections,
        context_sections,
        available_tools,
    )
}

pub fn prompt_cache_scope_fingerprint_for_agent(
    agent_id: &str,
    execution: &ExecutionSnapshot,
    system_sections: &[PromptSection],
    context_sections: &[PromptSection],
    available_tools: &[ToolSpec],
) -> String {
    scope(
        agent_id,
        execution,
        system_sections,
        context_sections,
        available_tools,
    )
}

pub fn prompt_context_fingerprint(
    session: &AgentState,
    execution: &ExecutionSnapshot,
    system_sections: &[PromptSection],
    context_sections: &[PromptSection],
    available_tools: &[ToolSpec],
) -> String {
    context(
        &session.id,
        session.working_memory.compression_epoch,
        execution,
        system_sections,
        context_sections,
        available_tools,
    )
}

pub fn reprojected_context_fingerprint(
    cache_identity: &PromptCacheIdentity,
    execution: &ExecutionSnapshot,
    system_sections: &[PromptSection],
    context_sections: &[PromptSection],
    available_tools: &[ToolSpec],
) -> String {
    context(
        &cache_identity.agent_id,
        cache_identity.compression_epoch,
        execution,
        system_sections,
        context_sections,
        available_tools,
    )
}

#[cfg(test)]
mod tests;

pub fn prompt_cache_key(agent_id: &str, context_fingerprint: &str) -> String {
    let short_fingerprint = context_fingerprint.get(..16).unwrap_or(context_fingerprint);
    format!("{agent_id}:ctx:{short_fingerprint}")
}

pub fn prompt_cache_scope_diagnostics(
    system_sections: &[PromptSection],
    context_sections: &[PromptSection],
) -> PromptCacheScopeDiagnostics {
    let cacheable_system_sections = system_sections
        .iter()
        .filter(|section| section.stability != PromptStability::TurnScoped)
        .collect::<Vec<_>>();
    let cacheable_context_sections = context_sections
        .iter()
        .filter(|section| section.stability != PromptStability::TurnScoped)
        .collect::<Vec<_>>();
    let turn_scoped_system_sections = system_sections
        .iter()
        .filter(|section| section.stability == PromptStability::TurnScoped)
        .collect::<Vec<_>>();
    let turn_scoped_context_sections = context_sections
        .iter()
        .filter(|section| section.stability == PromptStability::TurnScoped)
        .collect::<Vec<_>>();

    PromptCacheScopeDiagnostics {
        cacheable_system_sections: cacheable_system_sections.len(),
        cacheable_context_sections: cacheable_context_sections.len(),
        turn_scoped_system_sections: turn_scoped_system_sections.len(),
        turn_scoped_context_sections: turn_scoped_context_sections.len(),
        cacheable_system_chars: section_chars(&cacheable_system_sections),
        cacheable_context_chars: section_chars(&cacheable_context_sections),
        turn_scoped_system_chars: section_chars(&turn_scoped_system_sections),
        turn_scoped_context_chars: section_chars(&turn_scoped_context_sections),
        cacheable_system_fingerprint: section_fingerprint(&cacheable_system_sections),
        cacheable_context_fingerprint: section_fingerprint(&cacheable_context_sections),
        turn_scoped_fingerprint: section_fingerprint(
            &turn_scoped_system_sections
                .into_iter()
                .chain(turn_scoped_context_sections)
                .collect::<Vec<_>>(),
        ),
    }
}

fn section_chars(sections: &[&PromptSection]) -> usize {
    sections
        .iter()
        .map(|section| section.content.chars().count())
        .sum()
}

fn section_fingerprint(sections: &[&PromptSection]) -> String {
    digest(&sections)
}

fn execution_semantic_cache_payload(execution: &ExecutionSnapshot) -> serde_json::Value {
    json!({
        // `workspace_id` is included because it is rendered in the execution environment summary.
        "rendered_workspace_id": execution.workspace_id,
        // Attached workspace ids and anchors are rendered when multiple workspaces are visible.
        "rendered_attached_workspaces": execution.attached_workspaces,
        // These paths are rendered and define default cwd / relative path tool behavior.
        "workspace_anchor": execution.workspace_anchor,
        "execution_root": execution.execution_root,
        "cwd": execution.cwd,
        "worktree_root": execution.worktree_root,
        // These shape relative path and workspace projection semantics; bookkeeping ids do not.
        "projection_kind": execution.projection_kind,
        "access_mode": execution.access_mode,
    })
}
