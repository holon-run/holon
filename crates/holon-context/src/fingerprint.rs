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
mod tests {
    use super::*;
    use crate::section;
    use std::path::PathBuf;

    #[test]
    fn borrowed_sections_preserve_canonical_json_bytes_and_stability_filter() {
        let sections = [
            PromptStability::Stable,
            PromptStability::AgentScoped,
            PromptStability::TurnScoped,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, stability)| PromptSection {
            content: "\"\\\n\t\u{0000}中文🙂".into(),
            id: "id".into(),
            layout: if index == 0 {
                PromptLayout::SharedPrefix
            } else {
                PromptLayout::ScopedSuffix
            },
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

    fn sample_execution_snapshot() -> ExecutionSnapshot {
        ExecutionSnapshot {
            profile: holon_system::ExecutionProfile::default(),
            policy: holon_system::ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: Some("workspace-1".into()),
            workspace_anchor: PathBuf::from("/repo"),
            execution_root: PathBuf::from("/repo"),
            cwd: PathBuf::from("/repo/src"),
            execution_root_id: Some("canonical_root:workspace-1".into()),
            projection_kind: Some(holon_system::types::WorkspaceProjectionKind::CanonicalRoot),
            access_mode: Some(holon_system::types::WorkspaceAccessMode::SharedRead),
            worktree_root: None,
            execution_roots: Vec::new(),
        }
    }

    #[test]
    fn streamed_fingerprints_match_canonical_payloads() {
        use sha2::{Digest, Sha256};

        for (index, text) in [
            String::new(),
            "\"\\\n\t\r\u{0000}".into(),
            "中文🙂\u{2028}".into(),
            "long prompt section ".repeat(4096),
        ]
        .into_iter()
        .enumerate()
        {
            let mut session = AgentState::new("agent-\"中文");
            session.working_memory.compression_epoch = index as u64;
            let mut execution = sample_execution_snapshot();
            execution.cwd.push(format!("round-{index}"));
            let system = vec![
                section("stable", PromptStability::Stable, text.clone()),
                section("turn", PromptStability::TurnScoped, text.clone()),
            ];
            let context = vec![
                section("agent", PromptStability::AgentScoped, text.clone()),
                section("turn", PromptStability::TurnScoped, text),
            ];
            let tools = if index == 0 {
                vec![]
            } else {
                vec![ToolSpec {
                    name: "tool-\"中文".into(),
                    description: "description\n".into(),
                    input_schema: json!({"z": [null, true, 1.5], "a": {"z": 2, "a": "中"}}),
                    freeform_grammar: Some(holon_domain::ToolFreeformGrammar {
                        syntax: "lark".into(),
                        definition: "start: \"test\"".into(),
                    }),
                }]
            };
            let legacy_context = json!({
                "agent_id": session.id,
                "compression_epoch": session.working_memory.compression_epoch,
                "execution_semantics": execution_semantic_cache_payload(&execution),
                "system_sections": system,
                "context_sections": context,
                "tools": tools,
            });
            let expected = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&legacy_context).unwrap())
            );
            assert_eq!(
                prompt_context_fingerprint(&session, &execution, &system, &context, &tools),
                expected
            );
            let identity = PromptCacheIdentity {
                agent_id: session.id.clone(),
                compression_epoch: session.working_memory.compression_epoch,
                prompt_cache_key: String::new(),
                context_fingerprint: String::new(),
            };
            assert_eq!(
                reprojected_context_fingerprint(&identity, &execution, &system, &context, &tools),
                expected
            );
            let stable_system = system
                .iter()
                .filter(|s| s.stability != PromptStability::TurnScoped)
                .collect::<Vec<_>>();
            let stable_context = context
                .iter()
                .filter(|s| s.stability != PromptStability::TurnScoped)
                .collect::<Vec<_>>();
            let legacy_scope = json!({
                "agent_id": session.id,
                "execution_semantics": execution_semantic_cache_payload(&execution),
                "stable_system_sections": stable_system,
                "stable_context_sections": stable_context,
                "tools": tools,
            });
            assert_eq!(
                prompt_cache_scope_fingerprint(&session, &execution, &system, &context, &tools),
                format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&legacy_scope).unwrap())
                )
            );
            let sections = system.iter().chain(&context).collect::<Vec<_>>();
            assert_eq!(
                section_fingerprint(&sections),
                format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&sections).unwrap())
                )
            );
        }
    }

    #[test]
    fn layout_and_tool_schema_changes_invalidate_full_and_scoped_identity() {
        let execution = sample_execution_snapshot();
        let system = vec![
            crate::shared_section(
                "contract",
                PromptStability::Stable,
                "same rendered bytes".into(),
            ),
            crate::section(
                "agent",
                PromptStability::AgentScoped,
                "same scoped suffix".into(),
            ),
        ];
        let tool = ToolSpec {
            name: "ExecCommand".into(),
            description: "Run a command".into(),
            input_schema: json!({"type": "object"}),
            freeform_grammar: None,
        };
        let tools = vec![tool.clone()];
        let original_context = context("agent-one", 0, &execution, &system, &[], &tools);
        let original_scope = scope("agent-one", &execution, &system, &[], &tools);
        let mut scoped_layout = system.clone();
        scoped_layout[0].layout = PromptLayout::ScopedSuffix;
        assert_eq!(render_sections(&system), render_sections(&scoped_layout));
        assert_ne!(
            original_context,
            context("agent-one", 0, &execution, &scoped_layout, &[], &tools)
        );
        assert_ne!(
            original_scope,
            scope("agent-one", &execution, &scoped_layout, &[], &tools)
        );
        let mut changed_tool = tool;
        changed_tool.input_schema = json!({"type": "object", "required": ["cmd"]});
        let changed_tools = vec![changed_tool];
        assert_ne!(
            original_context,
            context("agent-one", 0, &execution, &system, &[], &changed_tools)
        );
        assert_ne!(
            original_scope,
            scope("agent-one", &execution, &system, &[], &changed_tools)
        );
        assert_ne!(
            original_scope,
            scope("agent-two", &execution, &system, &[], &tools)
        );
    }

    #[test]
    fn prompt_cache_key_ignores_turn_scoped_context_changes() {
        let mut session = AgentState::new("default");
        session.turn_index = 1;
        let execution = sample_execution_snapshot();
        let first_system_sections = vec![
            section("identity", PromptStability::Stable, "stable system".into()),
            section(
                "constrained_repair",
                PromptStability::TurnScoped,
                "fix only file: src/lib.rs".into(),
            ),
        ];
        let first_context_sections = vec![
            section(
                "working_memory",
                PromptStability::AgentScoped,
                "durable plan".into(),
            ),
            section(
                "current_input",
                PromptStability::TurnScoped,
                "first operator prompt".into(),
            ),
        ];
        let tools = vec![ToolSpec {
            name: "ExecCommand".into(),
            description: "Run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            freeform_grammar: None,
        }];
        let first_scope = prompt_cache_scope_fingerprint(
            &session,
            &execution,
            &first_system_sections,
            &first_context_sections,
            &tools,
        );
        let first_context = prompt_context_fingerprint(
            &session,
            &execution,
            &first_system_sections,
            &first_context_sections,
            &tools,
        );

        session.turn_index = 2;
        let second_system_sections = vec![
            section("identity", PromptStability::Stable, "stable system".into()),
            section(
                "constrained_repair",
                PromptStability::TurnScoped,
                "fix only file: src/main.rs".into(),
            ),
        ];
        let second_context_sections = vec![
            section(
                "working_memory",
                PromptStability::AgentScoped,
                "durable plan".into(),
            ),
            section(
                "current_input",
                PromptStability::TurnScoped,
                "second operator prompt".into(),
            ),
        ];
        let second_scope = prompt_cache_scope_fingerprint(
            &session,
            &execution,
            &second_system_sections,
            &second_context_sections,
            &tools,
        );
        let second_context = prompt_context_fingerprint(
            &session,
            &execution,
            &second_system_sections,
            &second_context_sections,
            &tools,
        );

        assert_ne!(first_context, second_context);
        assert_eq!(first_scope, second_scope);
        assert_eq!(
            prompt_cache_key(&session.id, &first_scope),
            prompt_cache_key(&session.id, &second_scope)
        );
    }

    #[test]
    fn prompt_cache_scope_diagnostics_document_stable_and_turn_scoped_boundary() {
        let session = AgentState::new("default");
        let execution = sample_execution_snapshot();
        let tools = vec![ToolSpec {
            name: "ExecCommand".into(),
            description: "Run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            freeform_grammar: None,
        }];
        let first_system_sections = vec![
            section("identity", PromptStability::Stable, "stable system".into()),
            section(
                "event_turn",
                PromptStability::TurnScoped,
                "first event metadata".into(),
            ),
        ];
        let first_context_sections = vec![
            section(
                "working_memory",
                PromptStability::AgentScoped,
                "durable work summary".into(),
            ),
            section(
                "current_input",
                PromptStability::TurnScoped,
                "first operator prompt".into(),
            ),
        ];
        let second_system_sections = vec![
            section("identity", PromptStability::Stable, "stable system".into()),
            section(
                "event_turn",
                PromptStability::TurnScoped,
                "second event metadata".into(),
            ),
        ];
        let second_context_sections = vec![
            section(
                "working_memory",
                PromptStability::AgentScoped,
                "durable work summary".into(),
            ),
            section(
                "current_input",
                PromptStability::TurnScoped,
                "second operator prompt".into(),
            ),
        ];

        let first_scope = prompt_cache_scope_fingerprint(
            &session,
            &execution,
            &first_system_sections,
            &first_context_sections,
            &tools,
        );
        let second_scope = prompt_cache_scope_fingerprint(
            &session,
            &execution,
            &second_system_sections,
            &second_context_sections,
            &tools,
        );
        let first_diagnostics =
            prompt_cache_scope_diagnostics(&first_system_sections, &first_context_sections);
        let second_diagnostics =
            prompt_cache_scope_diagnostics(&second_system_sections, &second_context_sections);

        assert_eq!(first_scope, second_scope);
        assert_eq!(
            prompt_cache_key(&session.id, &first_scope),
            prompt_cache_key(&session.id, &second_scope)
        );
        assert_eq!(first_diagnostics.cacheable_system_sections, 1);
        assert_eq!(first_diagnostics.cacheable_context_sections, 1);
        assert_eq!(first_diagnostics.turn_scoped_system_sections, 1);
        assert_eq!(first_diagnostics.turn_scoped_context_sections, 1);
        assert_eq!(
            first_diagnostics.cacheable_system_fingerprint,
            second_diagnostics.cacheable_system_fingerprint
        );
        assert_eq!(
            first_diagnostics.cacheable_context_fingerprint,
            second_diagnostics.cacheable_context_fingerprint
        );
        assert_ne!(
            first_diagnostics.turn_scoped_fingerprint,
            second_diagnostics.turn_scoped_fingerprint
        );

        let changed_system_sections = vec![
            section(
                "identity",
                PromptStability::Stable,
                "changed stable system".into(),
            ),
            section(
                "event_turn",
                PromptStability::TurnScoped,
                "second event metadata".into(),
            ),
        ];
        let changed_scope = prompt_cache_scope_fingerprint(
            &session,
            &execution,
            &changed_system_sections,
            &second_context_sections,
            &tools,
        );

        assert_ne!(second_scope, changed_scope);
        assert_ne!(
            prompt_cache_key(&session.id, &second_scope),
            prompt_cache_key(&session.id, &changed_scope)
        );
    }

    #[test]
    fn prompt_cache_key_changes_when_agent_scoped_context_changes() {
        let session = AgentState::new("default");
        let execution = sample_execution_snapshot();
        let system_sections = vec![section(
            "identity",
            PromptStability::Stable,
            "stable system".into(),
        )];
        let first_context_sections = vec![section(
            "working_memory",
            PromptStability::AgentScoped,
            "durable plan one".into(),
        )];
        let second_context_sections = vec![section(
            "working_memory",
            PromptStability::AgentScoped,
            "durable plan two".into(),
        )];
        let tools = Vec::new();

        let first_scope = prompt_cache_scope_fingerprint(
            &session,
            &execution,
            &system_sections,
            &first_context_sections,
            &tools,
        );
        let second_scope = prompt_cache_scope_fingerprint(
            &session,
            &execution,
            &system_sections,
            &second_context_sections,
            &tools,
        );

        assert_ne!(
            prompt_cache_key(&session.id, &first_scope),
            prompt_cache_key(&session.id, &second_scope)
        );
    }

    #[test]
    fn prompt_cache_key_ignores_execution_root_bookkeeping_id_changes() {
        let session = AgentState::new("default");
        let mut first_execution = sample_execution_snapshot();
        let mut second_execution = sample_execution_snapshot();
        first_execution.execution_root_id = Some("canonical_root:workspace-1".into());
        second_execution.execution_root_id = Some("canonical_root:workspace-2".into());
        let system_sections = vec![section(
            "identity",
            PromptStability::Stable,
            "stable system".into(),
        )];
        let context_sections = vec![section(
            "working_memory",
            PromptStability::AgentScoped,
            "durable plan".into(),
        )];
        let tools = Vec::new();

        let first_scope = prompt_cache_scope_fingerprint(
            &session,
            &first_execution,
            &system_sections,
            &context_sections,
            &tools,
        );
        let second_scope = prompt_cache_scope_fingerprint(
            &session,
            &second_execution,
            &system_sections,
            &context_sections,
            &tools,
        );

        assert_eq!(first_scope, second_scope);
    }

    #[test]
    fn prompt_cache_key_changes_when_workspace_path_semantics_change() {
        let session = AgentState::new("default");
        let first_execution = sample_execution_snapshot();
        let mut second_execution = sample_execution_snapshot();
        second_execution.workspace_anchor = PathBuf::from("/other-repo");
        second_execution.execution_root = PathBuf::from("/other-repo");
        second_execution.cwd = PathBuf::from("/other-repo/src");
        let system_sections = vec![section(
            "identity",
            PromptStability::Stable,
            "stable system".into(),
        )];
        let context_sections = vec![section(
            "working_memory",
            PromptStability::AgentScoped,
            "durable plan".into(),
        )];
        let tools = Vec::new();

        let first_scope = prompt_cache_scope_fingerprint(
            &session,
            &first_execution,
            &system_sections,
            &context_sections,
            &tools,
        );
        let second_scope = prompt_cache_scope_fingerprint(
            &session,
            &second_execution,
            &system_sections,
            &context_sections,
            &tools,
        );

        assert_ne!(first_scope, second_scope);
    }
}

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
