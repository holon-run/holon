// Extracted provider implementations from runtime_flow.rs
// This file contains all provider implementations used for testing

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use holon::provider::{
    AgentProvider, ConversationMessage, ModelBlock, ProviderTurnRequest, ProviderTurnResponse,
};
use serde_json::json;
use tokio::sync::Mutex;

use tokio::time::{sleep, Duration};

// Import helper functions from runtime_helpers
use super::runtime_helpers::{delegated_prompt_text, preserves_prior_tool_context};

/// Helper struct to capture turn request details for testing
#[derive(Debug, Clone)]
pub struct CapturedTurnRequest {
    pub prompt_text: String,
    pub compression_epoch: u64,
    pub working_memory_revision: u64,
}

// ============================================================================
// Provider Implementations
// ============================================================================

/// Provider that uses tools in its first turn
pub struct ToolUsingProvider {
    calls: Mutex<usize>,
}

impl ToolUsingProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for ToolUsingProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(!request.tools.is_empty());
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "tool-1".into(),
                    name: "AgentGet".into(),
                    input: json!({}),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        assert!(preserves_prior_tool_context(&request));
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "tool loop complete".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that notifies operator then calls AgentGet
pub struct NotifyThenAgentGetProvider {
    calls: Mutex<usize>,
}

impl NotifyThenAgentGetProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for NotifyThenAgentGetProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request
                .tools
                .iter()
                .any(|tool| tool.name == "NotifyOperator"));
            assert!(request.tools.iter().any(|tool| tool.name == "AgentGet"));
            return Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::ToolUse {
                        id: "notify-1".into(),
                        name: "NotifyOperator".into(),
                        input: json!({
                            "message": "Operator FYI\nContinuing with the default path."
                        }),
                    },
                    ModelBlock::ToolUse {
                        id: "agent-get-1".into(),
                        name: "AgentGet".into(),
                        input: json!({}),
                    },
                ],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        assert!(preserves_prior_tool_context(&request));
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "continued after notifying operator".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that demonstrates file editing operations
pub struct FileEditingProvider {
    calls: Mutex<usize>,
}

impl FileEditingProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for FileEditingProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request.tools.iter().any(|tool| tool.name == "ApplyPatch"));
            assert!(request.tools.iter().any(|tool| tool.name == "ExecCommand"));
            return Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::ToolUse {
                        id: "patch-1".into(),
                        name: "ApplyPatch".into(),
                        input: json!({
                            "patch": "--- /dev/null\n+++ b/notes/result.txt\n@@ -0,0 +1,1 @@\n+hello from holon\n"
                        }),
                    },
                    ModelBlock::ToolUse {
                        id: "read-1".into(),
                        name: "ExecCommand".into(),
                        input: json!({
                            "cmd": "cat notes/result.txt",
                            "workdir": "."
                        }),
                    },
                ],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        assert!(preserves_prior_tool_context(&request));
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "file tools complete".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that demonstrates terminal result brief functionality
pub struct TerminalResultBriefProvider {
    calls: Mutex<usize>,
}

impl TerminalResultBriefProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for TerminalResultBriefProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        match *calls {
            1 => {
                assert!(request.tools.iter().any(|tool| tool.name == "ApplyPatch"));
                assert!(request.tools.iter().any(|tool| tool.name == "ExecCommand"));
                Ok(ProviderTurnResponse {
                    blocks: vec![
                        ModelBlock::Text {
                            text: "Let me create a summary document of what was changed.".into(),
                        },
                        ModelBlock::ToolUse {
                            id: "patch-1".into(),
                            name: "ApplyPatch".into(),
                            input: json!({
                                "patch": "--- /dev/null\n+++ b/notes/result.txt\n@@ -0,0 +1,1 @@\n+hello from holon\n"
                            }),
                        },
                        ModelBlock::ToolUse {
                            id: "verify-1".into(),
                            name: "ExecCommand".into(),
                            input: json!({
                                "cmd": "printf tests_passed",
                                "login": false
                            }),
                        },
                    ],
                    stop_reason: None,
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            2 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Verification is complete. I'll package the final answer now.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "sleep-1".into(),
                        name: "Sleep".into(),
                        input: json!({
                            "reason": "sleep requested"
                        }),
                    },
                ],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => {
                assert!(
                    request.tools.is_empty(),
                    "terminal delivery round should have no tools"
                );
                Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::Text {
                        text: "What changed: notes/result.txt\nWhy: to address the requested task: write and verify a file.\nVerification: successful verification command completed with exit status 0.".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
        }
    }
}

/// Provider that demonstrates sleep-only completion after text
pub struct SleepOnlyCompletionAfterTextProvider {
    calls: Mutex<usize>,
}

impl SleepOnlyCompletionAfterTextProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for SleepOnlyCompletionAfterTextProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        match *calls {
            1 => {
                assert!(request.tools.iter().any(|tool| tool.name == "ApplyPatch"));
                Ok(ProviderTurnResponse {
                    blocks: vec![
                        ModelBlock::Text {
                            text: "Updated notes/result.txt and verified the requested change."
                                .into(),
                        },
                        ModelBlock::ToolUse {
                            id: "patch-1".into(),
                            name: "ApplyPatch".into(),
                            input: json!({
                                "patch": "--- /dev/null\n+++ b/notes/result.txt\n@@ -0,0 +1,1 @@\n+hello from holon\n"
                            }),
                        },
                    ],
                    stop_reason: None,
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            2 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "sleep-1".into(),
                    name: "Sleep".into(),
                    input: json!({
                        "reason": "delivery complete"
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 20,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => anyhow::bail!("unexpected provider call"),
        }
    }
}

/// Provider that demonstrates shell command execution
pub struct ShellProvider {
    calls: Mutex<usize>,
}

impl ShellProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for ShellProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request.tools.iter().any(|tool| tool.name == "ExecCommand"));
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "exec-1".into(),
                    name: "ExecCommand".into(),
                    input: json!({
                        "cmd": "printf shell_ok"
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let tool_result_text = request
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserToolResults(results) => Some(
                    results
                        .iter()
                        .map(|result| result.content.clone())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .unwrap_or_default();
        assert!(tool_result_text.contains("shell_ok"));
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "shell tools complete".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that demonstrates truncated shell output reinjection
pub struct TruncatedShellReinjectionProvider {
    calls: Mutex<usize>,
    payload: String,
}

impl TruncatedShellReinjectionProvider {
    pub fn new(payload: String) -> Self {
        Self {
            calls: Mutex::new(0),
            payload,
        }
    }
}

#[async_trait]
impl AgentProvider for TruncatedShellReinjectionProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request.tools.iter().any(|tool| tool.name == "ExecCommand"));
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "exec-truncated-1".into(),
                    name: "ExecCommand".into(),
                    input: json!({
                        "cmd": format!("printf '%s' '{}'", self.payload),
                        "login": false,
                        "max_output_tokens": 32
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let tool_result_text = request
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserToolResults(results) => Some(
                    results
                        .iter()
                        .map(|result| result.content.clone())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .unwrap_or_default();
        assert!(
            tool_result_text.contains("[output truncated: showing leading and trailing context]")
        );
        assert!(!tool_result_text.contains(&self.payload));
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "truncated shell reinjection observed".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that demonstrates long-running shell commands
pub struct LongShellProvider {
    calls: Mutex<usize>,
}

impl LongShellProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for LongShellProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "exec-long-1".into(),
                    name: "ExecCommand".into(),
                    input: json!({
                        "cmd": "printf start && sleep 1 && printf done",
                        "yield_time_ms": 50
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let tool_result_text = request
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserToolResults(results) => Some(
                    results
                        .iter()
                        .map(|result| result.content.clone())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .unwrap_or_default();
        assert!(tool_result_text.contains("Command promoted to background task"));
        assert!(tool_result_text.contains("Task:"));
        assert!(tool_result_text.contains("Initial output:"));
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "auto promotion observed".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider for delegated child agent execution
pub struct DelegatedBoundaryProvider;

#[async_trait]
impl AgentProvider for DelegatedBoundaryProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let prompt = delegated_prompt_text(&request);
        let (delay_ms, text) = if prompt.contains("slow-child") {
            (200, "slow child result")
        } else if prompt.contains("alpha-child") {
            (160, "alpha child result")
        } else if prompt.contains("beta-child") {
            (20, "beta child result")
        } else if prompt.contains("fail-child") {
            anyhow::bail!("child execution exploded");
        } else {
            anyhow::bail!("unexpected delegated prompt: {prompt}");
        };

        if delay_ms > 0 {
            sleep(Duration::from_millis(delay_ms)).await;
        }

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text { text: text.into() }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that demonstrates wake hint functionality
pub struct WakeHintProvider {
    calls: Mutex<usize>,
}

impl WakeHintProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for WakeHintProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            sleep(Duration::from_millis(250)).await;
            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "first turn complete".into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            })
        } else {
            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "wake follow-up complete".into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }
}

/// Provider that records all turn requests for inspection
pub struct RecordingPromptProvider {
    calls: Mutex<usize>,
    requests: Mutex<Vec<CapturedTurnRequest>>,
    replies: Vec<String>,
    first_delay: Option<Duration>,
}

impl RecordingPromptProvider {
    pub fn new(replies: &[&str]) -> Self {
        Self {
            calls: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
            replies: replies.iter().map(|reply| (*reply).to_string()).collect(),
            first_delay: None,
        }
    }

    pub fn with_first_delay(mut self, delay: Duration) -> Self {
        self.first_delay = Some(delay);
        self
    }

    pub async fn captured_requests(&self) -> Vec<CapturedTurnRequest> {
        self.requests.lock().await.clone()
    }
}

#[async_trait]
impl AgentProvider for RecordingPromptProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let call_index = *calls - 1;
        drop(calls);

        self.requests.lock().await.push(CapturedTurnRequest {
            prompt_text: delegated_prompt_text(&request),
            compression_epoch: request
                .prompt_frame
                .cache
                .as_ref()
                .map(|cache| cache.compression_epoch)
                .unwrap_or_default(),
            working_memory_revision: request
                .prompt_frame
                .cache
                .as_ref()
                .map(|cache| cache.working_memory_revision)
                .unwrap_or_default(),
        });

        if call_index == 0 {
            if let Some(delay) = self.first_delay {
                sleep(delay).await;
            }
        }

        let reply = self
            .replies
            .get(call_index)
            .cloned()
            .or_else(|| self.replies.last().cloned())
            .unwrap_or_else(|| "recorded turn".into());

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text { text: reply }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that demonstrates tool error handling
pub struct ToolErrorProvider {
    calls: Mutex<usize>,
}

impl ToolErrorProvider {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for ToolErrorProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::ToolUse {
                        id: "bad-exec".into(),
                        name: "ExecCommand".into(),
                        input: json!({
                            "yield_time_ms": 10
                        }),
                    },
                    ModelBlock::ToolUse {
                        id: "bad-tool".into(),
                        name: "DefinitelyNotATool".into(),
                        input: json!({}),
                    },
                    ModelBlock::ToolUse {
                        id: "retired-read".into(),
                        name: "Read".into(),
                        input: json!({
                            "file_path": "notes/result.txt"
                        }),
                    },
                ],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let tool_results = request
            .conversation
            .iter()
            .rev()
            .find_map(|message| match message {
                ConversationMessage::UserToolResults(results) => Some(results.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(tool_results.len(), 3);
        assert!(tool_results.iter().all(|result| result.is_error));
        assert!(tool_results.iter().any(|result| {
            result.error.as_ref().is_some_and(|error| {
                error.kind == "invalid_tool_input"
                    && error
                        .details
                        .as_ref()
                        .and_then(|details| details.get("parse_error"))
                        .and_then(|value| value.as_str())
                        .is_some_and(|parse_error| parse_error.contains("missing field `cmd`"))
            })
        }));
        assert!(tool_results.iter().any(|result| result
            .content
            .contains("tool DefinitelyNotATool was not exposed in this round")));
        assert!(tool_results.iter().any(|result| result
            .content
            .contains("tool Read was not exposed in this round")));

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "tool failures handled".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that always fails with runtime error
pub struct RuntimeFailureProvider;

#[async_trait]
impl AgentProvider for RuntimeFailureProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        anyhow::bail!("provider transport broke")
    }
}

/// Provider that always fails with verbose runtime error
pub struct VerboseRuntimeFailureProvider;

#[async_trait]
impl AgentProvider for VerboseRuntimeFailureProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        anyhow::bail!(
            "provider transport broke with a very long first line {}\
\nraw backend body: {{\"detail\":\"unexpected backend body that should stay out of briefs\"}}",
            "x".repeat(260)
        )
    }
}

/// Provider that demonstrates workspace usage
pub struct UseWorkspaceProvider {
    calls: Mutex<usize>,
    workspace_path: PathBuf,
    branch_name: String,
}

impl UseWorkspaceProvider {
    pub fn new(workspace_path: PathBuf, branch_name: impl Into<String>) -> Self {
        Self {
            calls: Mutex::new(0),
            workspace_path,
            branch_name: branch_name.into(),
        }
    }
}

#[async_trait]
impl AgentProvider for UseWorkspaceProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request.tools.iter().any(|tool| tool.name == "UseWorkspace"));
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "workspace-1".into(),
                    name: "UseWorkspace".into(),
                    input: json!({
                        "path": self.workspace_path,
                        "mode": "isolated",
                        "access_mode": "exclusive_write",
                        "isolation_label": self.branch_name,
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let tool_result_text = request
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserToolResults(results) => Some(
                    results
                        .iter()
                        .map(|result| result.content.clone())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .unwrap_or_default();
        assert!(tool_result_text.contains("\"mode\": \"isolated\""));
        assert!(tool_result_text.contains("\"projection_kind\": \"git_worktree_root\""));
        assert!(tool_result_text.contains("\"access_mode\": \"exclusive_write\""));
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "entered worktree successfully".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that demonstrates worktree lifecycle
pub struct WorktreeLifecycleProvider {
    calls: Mutex<usize>,
    workspace_path: PathBuf,
    branch_name: String,
    expected_exit_result: String,
}

impl WorktreeLifecycleProvider {
    pub fn new(
        workspace_path: PathBuf,
        branch_name: impl Into<String>,
        expected_exit_result: impl Into<String>,
    ) -> Self {
        Self {
            calls: Mutex::new(0),
            workspace_path,
            branch_name: branch_name.into(),
            expected_exit_result: expected_exit_result.into(),
        }
    }
}

#[async_trait]
impl AgentProvider for WorktreeLifecycleProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        match *calls {
            1 => {
                assert!(request.tools.iter().any(|tool| tool.name == "UseWorkspace"));
                Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::ToolUse {
                        id: "use-1".into(),
                        name: "UseWorkspace".into(),
                        input: json!({
                            "path": self.workspace_path,
                            "mode": "isolated",
                            "access_mode": "exclusive_write",
                            "isolation_label": self.branch_name,
                        }),
                    }],
                    stop_reason: None,
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            2 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "entered worktree".into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            3 => {
                assert!(request.tools.iter().any(|tool| tool.name == "UseWorkspace"));
                Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::ToolUse {
                        id: "use-home-1".into(),
                        name: "UseWorkspace".into(),
                        input: json!({
                            "workspace_id": "agent_home",
                        }),
                    }],
                    stop_reason: None,
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            4 => {
                let tool_results = request
                    .conversation
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        ConversationMessage::UserToolResults(results) => Some(results.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                assert!(tool_results
                    .iter()
                    .any(|result| result.content.contains(&self.expected_exit_result)));
                Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::Text {
                        text: "exited worktree".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            _ => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "done".into(),
                }],
                stop_reason: None,
                input_tokens: 50,
                output_tokens: 20,
                cache_usage: None,
                request_diagnostics: None,
            }),
        }
    }
}

/// Provider that captures worktree prompts for inspection
pub struct WorktreeCapturingProvider {
    prompts: Mutex<Vec<String>>,
    reply: String,
}

impl WorktreeCapturingProvider {
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            prompts: Mutex::new(Vec::new()),
            reply: reply.into(),
        }
    }

    pub async fn prompts(&self) -> Vec<String> {
        self.prompts.lock().await.clone()
    }
}

#[async_trait]
impl AgentProvider for WorktreeCapturingProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.prompts
            .lock()
            .await
            .push(request.prompt_frame.system_prompt.clone());
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: self.reply.clone(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

/// Provider that delays before responding
pub struct DelayedTextProvider;

#[async_trait]
impl AgentProvider for DelayedTextProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        sleep(Duration::from_millis(250)).await;
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "Made changes in worktree".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

