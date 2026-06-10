#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeRef {
    AgentMemory {
        name: String,
    },
    WorkspaceProfile {
        workspace_id: String,
    },
    Brief {
        id: String,
    },
    Turn {
        id: String,
    },
    Episode {
        id: String,
    },
    WorkItem {
        id: String,
    },
    Task {
        id: String,
    },
    ToolExecution {
        id: String,
        batch_item_index: Option<usize>,
        selector: ToolExecutionRefSelector,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolExecutionRefSelector {
    Cmd,
    Output(ToolOutputSelector),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolOutputSelector {
    Stdout,
    Stderr,
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeRefParseError {
    validation_error: &'static str,
}

impl RuntimeRefParseError {
    pub(crate) fn validation_error(&self) -> &'static str {
        self.validation_error
    }
}

pub(crate) const ALLOWED_SOURCE_REF_PREFIXES: &[&str] = &[
    "agent_memory:",
    "workspace_profile:",
    "brief:",
    "turn:",
    "episode:",
    "work_item:",
    "tool_execution:",
    "task:",
];

impl RuntimeRef {
    pub(crate) fn parse(source_ref: &str) -> Result<Self, RuntimeRefParseError> {
        if source_ref.chars().any(char::is_whitespace) {
            return Err(RuntimeRefParseError {
                validation_error: "must not contain whitespace",
            });
        }
        let Some((prefix, suffix)) = source_ref.split_once(':') else {
            return Err(RuntimeRefParseError {
                validation_error: "missing source_ref prefix",
            });
        };
        if prefix == "tool_execution" {
            return parse_tool_execution_ref(suffix);
        }
        if suffix.is_empty() {
            return Err(RuntimeRefParseError {
                validation_error: "missing source_ref identifier",
            });
        }
        if !valid_source_ref_segment(suffix) {
            return Err(RuntimeRefParseError {
                validation_error:
                    "source_ref identifier must be an opaque id, not a path, URL, or query",
            });
        }
        match prefix {
            "agent_memory" => Ok(Self::AgentMemory {
                name: suffix.to_string(),
            }),
            "workspace_profile" => Ok(Self::WorkspaceProfile {
                workspace_id: suffix.to_string(),
            }),
            "brief" => Ok(Self::Brief {
                id: suffix.to_string(),
            }),
            "turn" => Ok(Self::Turn {
                id: suffix.to_string(),
            }),
            "episode" => Ok(Self::Episode {
                id: suffix.to_string(),
            }),
            "work_item" => Ok(Self::WorkItem {
                id: suffix.to_string(),
            }),
            "task" => Ok(Self::Task {
                id: suffix.to_string(),
            }),
            _ => Err(RuntimeRefParseError {
                validation_error: "unsupported source_ref prefix",
            }),
        }
    }

    pub(crate) fn source_ref(&self) -> String {
        match self {
            Self::AgentMemory { name } => format!("agent_memory:{name}"),
            Self::WorkspaceProfile { workspace_id } => {
                format!("workspace_profile:{workspace_id}")
            }
            Self::Brief { id } => format!("brief:{id}"),
            Self::Turn { id } => format!("turn:{id}"),
            Self::Episode { id } => format!("episode:{id}"),
            Self::WorkItem { id } => format!("work_item:{id}"),
            Self::Task { id } => format!("task:{id}"),
            Self::ToolExecution {
                id,
                batch_item_index,
                selector,
            } => {
                let selector = selector.as_ref_selector();
                if let Some(index) = batch_item_index {
                    format!("tool_execution:{id}:batch_item:{index}:{selector}")
                } else {
                    format!("tool_execution:{id}:{selector}")
                }
            }
        }
    }
}

impl ToolExecutionRefSelector {
    fn as_ref_selector(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Output(ToolOutputSelector::Stdout) => "stdout",
            Self::Output(ToolOutputSelector::Stderr) => "stderr",
            Self::Output(ToolOutputSelector::Output) => "output",
        }
    }
}

impl ToolOutputSelector {
    pub(crate) fn as_ref_selector(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Output => "output",
        }
    }
}

fn parse_tool_execution_ref(suffix: &str) -> Result<RuntimeRef, RuntimeRefParseError> {
    let parts = suffix.split(':').collect::<Vec<_>>();
    match parts.as_slice() {
        [id, selector] if valid_source_ref_segment(id) => Ok(RuntimeRef::ToolExecution {
            id: (*id).to_string(),
            batch_item_index: None,
            selector: parse_tool_execution_selector(selector)?,
        }),
        [id, "batch_item", index, selector] if valid_source_ref_segment(id) => {
            Ok(RuntimeRef::ToolExecution {
                id: (*id).to_string(),
                batch_item_index: Some(parse_batch_item_index(index)?),
                selector: parse_tool_execution_selector(selector)?,
            })
        }
        _ => Err(RuntimeRefParseError {
            validation_error: "tool_execution source_ref must match tool_execution:<id>:{cmd,stdout,stderr,output} or tool_execution:<id>:batch_item:<index>:{cmd,stdout,stderr,output}",
        }),
    }
}

fn parse_tool_execution_selector(
    selector: &str,
) -> Result<ToolExecutionRefSelector, RuntimeRefParseError> {
    match selector {
        "cmd" => Ok(ToolExecutionRefSelector::Cmd),
        "stdout" => Ok(ToolExecutionRefSelector::Output(ToolOutputSelector::Stdout)),
        "stderr" => Ok(ToolExecutionRefSelector::Output(ToolOutputSelector::Stderr)),
        "output" => Ok(ToolExecutionRefSelector::Output(ToolOutputSelector::Output)),
        _ => Err(RuntimeRefParseError {
            validation_error: "unsupported tool_execution selector",
        }),
    }
}

fn parse_batch_item_index(index: &str) -> Result<usize, RuntimeRefParseError> {
    if index.starts_with('0') {
        return Err(RuntimeRefParseError {
            validation_error: "batch item index must be a one-based integer without leading zeroes",
        });
    }
    index
        .parse::<usize>()
        .ok()
        .filter(|index| *index >= 1)
        .ok_or(RuntimeRefParseError {
            validation_error: "batch item index must be a one-based integer",
        })
}

fn valid_source_ref_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_ref_accepts_supported_namespaces() {
        for source_ref in [
            "agent_memory:self",
            "workspace_profile:ws-123",
            "brief:abc",
            "turn:turn_123",
            "episode:ep_123",
            "work_item:work_123",
            "task:task_123",
            "tool_execution:tool-123:cmd",
            "tool_execution:tool-123:stdout",
            "tool_execution:tool-123:stderr",
            "tool_execution:tool-123:output",
            "tool_execution:tool-123:batch_item:2:cmd",
            "tool_execution:tool-123:batch_item:2:stdout",
            "tool_execution:tool-123:batch_item:2:stderr",
            "tool_execution:tool-123:batch_item:2:output",
        ] {
            assert_eq!(
                RuntimeRef::parse(source_ref).unwrap().source_ref(),
                source_ref
            );
        }
    }

    #[test]
    fn runtime_ref_rejects_paths_urls_and_unknown_prefixes() {
        for source_ref in [
            "/Users/jolestar/.agents/skills/agentinbox/SKILL.md",
            "skill:/Users/jolestar/.agents/skills/agentinbox/SKILL.md",
            "skill.md:/Users/jolestar/.agents/skills/agentinbox/SKILL.md",
            "agentinbox:///SKILL.md",
            "memory:invalid-ref-123",
            "brief:/Users/jolestar/project/README.md",
            "brief:https://example.com/memory",
            "turn:../ledger/turn-1",
            "episode:../ledger/episode-1",
            "work_item:work_123?raw=true",
            "tool_execution:tool-123",
            "tool_execution:tool-123:batch_item:0:cmd",
            "tool_execution:tool-123:batch_item:02:cmd",
            "tool_execution:tool-123:batch_item:abc:cmd",
            "tool_execution:tool-123:batch_item:2",
            "tool_execution:tool-123:batch_item:2:artifact",
            "tool_execution:tool-123:artifact",
        ] {
            assert!(
                RuntimeRef::parse(source_ref).is_err(),
                "{source_ref} should be invalid"
            );
        }
    }
}
