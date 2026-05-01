use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    provider::{
        AgentProvider, ModelBlock, ProviderTurnRequest, ProviderTurnResponse, StubProvider,
    },
    run_once::{run_once_with_host, RunFinalStatus, RunOnceRequest},
    storage::AppStorage,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{ControlAction, FailureArtifactCategory, TaskStatus, TokenUsage, TrustLevel},
};
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

mod support;

use support::{assert_run_once_completed_text, init_git_repo, TestConfigBuilder};

fn test_config(workspace_dir: PathBuf, home_dir: PathBuf) -> AppConfig {
    TestConfigBuilder::new()
        .with_workspace_dir(workspace_dir)
        .with_data_dir(home_dir)
        .with_control_auth_mode(ControlAuthMode::Auto)
        .build()
}

fn run_request(text: impl Into<String>) -> RunOnceRequest {
    RunOnceRequest {
        text: text.into(),
        trust: TrustLevel::TrustedOperator,
        agent_id: None,
        create_agent: false,
        template: None,
        max_turns: None,
        wait_for_tasks: true,
        workspace_root: None,
        cwd: None,
    }
}

#[tokio::test]
async fn run_once_returns_completed_text_for_simple_prompt() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir.clone()),
        Arc::new(StubProvider::new("stub result")),
    )?;

    let response = run_once_with_host(host.clone(), run_request("hello")).await?;

    assert_run_once_completed_text(&response, "stub result");
    assert_eq!(response.token_usage.input_tokens, 0);
    assert_eq!(response.token_usage.output_tokens, 0);
    assert_eq!(response.token_usage.total_tokens, 0);
    assert!(!response.render_text().contains("Token usage:"));
    assert!(response.agent_id.starts_with("tmp_run_"));
    assert!(response.tasks.is_empty());
    let listed_agents = host
        .list_agents()
        .await?
        .into_iter()
        .map(|summary| summary.identity.agent_id)
        .collect::<Vec<_>>();
    assert!(!listed_agents.iter().any(|id| id == &response.agent_id));
    Ok(())
}

struct TokenReportingProvider;

#[async_trait]
impl AgentProvider for TokenReportingProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "token counted result".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn run_once_surfaces_structured_token_usage_when_provider_reports_it() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(TokenReportingProvider),
    )?;

    let response = run_once_with_host(host, run_request("hello")).await?;

    assert_eq!(response.token_usage.input_tokens, 100);
    assert_eq!(response.token_usage.output_tokens, 50);
    assert_eq!(response.token_usage.total_tokens, 150);
    assert!(response
        .render_text()
        .contains("Token usage: input 100, output 50, total 150"));
    Ok(())
}

struct WorkItemDeliverySummaryProvider {
    calls: Mutex<usize>,
    work_item_id: Mutex<Option<String>>,
    complete_next: Mutex<bool>,
    after_completion: Mutex<bool>,
}

impl WorkItemDeliverySummaryProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
            work_item_id: Mutex::new(None),
            complete_next: Mutex::new(false),
            after_completion: Mutex::new(false),
        }
    }

    async fn set_work_item_id(&self, work_item_id: String) {
        *self.work_item_id.lock().await = Some(work_item_id);
    }

    async fn complete_next_turn(&self) {
        *self.complete_next.lock().await = true;
    }
}

#[async_trait]
impl AgentProvider for WorkItemDeliverySummaryProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if request.tools.is_empty() {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Implemented broad prompt/context snapshot coverage, then fixed annotation warnings in a later continuation. Verification: focused tests passed.".into(),
                }],
                stop_reason: None,
                input_tokens: 10,
                output_tokens: 12,
                cache_usage: None,
            request_diagnostics: None,
            });
        }

        let work_item_id = self
            .work_item_id
            .lock()
            .await
            .clone()
            .expect("test should seed a work item id before provider use");
        if *self.after_completion.lock().await {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Fixed two test annotation issues.".into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }
        if *self.complete_next.lock().await {
            *self.complete_next.lock().await = false;
            *self.after_completion.lock().await = true;
            return Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Fixed two test annotation issues.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "work-fixup".into(),
                        name: "CompleteWorkItem".into(),
                        input: json!({
                            "work_item_id": work_item_id.clone(),
                            "result_summary": "Implemented broad prompt/context snapshot coverage"
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
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = match *calls {
            1 => vec![
                ModelBlock::Text {
                    text: "Implemented broad prompt/context snapshot coverage.".into(),
                },
                ModelBlock::ToolUse {
                    id: "work-main".into(),
                    name: "UpdateWorkItem".into(),
                    input: json!({
                        "work_item_id": work_item_id.clone(),
                        "blocked_by": "Main implementation is in place; annotations still need cleanup."
                    }),
                },
            ],
            _ => vec![ModelBlock::Text {
                text: "Main snapshot coverage is implemented; a small annotation cleanup remains."
                    .into(),
            }],
        };

        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn run_once_prefers_completed_work_item_delivery_summary_over_latest_turn_text() -> Result<()>
{
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let provider = Arc::new(WorkItemDeliverySummaryProvider::new());
    let host =
        RuntimeHost::new_with_provider(test_config(workspace_dir, home_dir), provider.clone())?;
    let runtime = host.default_runtime().await?;
    let (work_item, _) = runtime
        .create_work_item("cover prompt/context snapshots".into(), None)
        .await?;
    runtime.pick_work_item(work_item.id.clone()).await?;
    provider.set_work_item_id(work_item.id.clone()).await;
    let mut request = run_request("continue snapshot coverage");
    request.agent_id = Some("default".into());

    let first = run_once_with_host(host.clone(), request.clone()).await?;
    assert!(first.final_text.contains("Main snapshot coverage"));

    request.text = "fix the remaining warning".into();
    provider.complete_next_turn().await;
    let second = run_once_with_host(host.clone(), request).await?;

    assert_eq!(
        second.final_status,
        RunFinalStatus::Completed,
        "unexpected final text: {}",
        second.final_text
    );
    assert!(
        second
            .final_text
            .contains("Implemented broad prompt/context snapshot coverage"),
        "unexpected final text: {}",
        second.final_text
    );
    assert_eq!(
        second.final_text,
        "Implemented broad prompt/context snapshot coverage"
    );
    assert_eq!(
        second.raw_final_text.as_deref(),
        Some("Fixed two test annotation issues.")
    );
    let runtime = host
        .get_public_agent_for_external_ingress("default")
        .await?;
    let summary = runtime
        .storage()
        .latest_delivery_summary(&work_item.id)?
        .expect("completed work item should persist a delivery summary");
    assert_eq!(summary.text, second.final_text);
    assert_eq!(
        runtime.agent_state().await?.last_turn_token_usage,
        Some(TokenUsage::new(100, 50))
    );
    Ok(())
}

struct ErrorProvider;

#[async_trait]
impl AgentProvider for ErrorProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        anyhow::bail!("provider failed");
    }
}

#[tokio::test]
async fn run_once_surfaces_runtime_error_as_failed_delivery() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(ErrorProvider),
    )?;

    let response = run_once_with_host(host, run_request("explode")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Failed);
    assert!(response.waiting_reason.is_none());
    assert!(response
        .final_text
        .contains("Turn failed while processing operator_prompt"));
    assert!(response.final_text.contains("provider failed"));
    let artifact = response
        .failure_artifact
        .expect("failed run should include normalized failure artifact");
    assert_eq!(artifact.category, FailureArtifactCategory::Runtime);
    assert_eq!(artifact.kind, "runtime_error");
    assert!(artifact.summary.contains("provider failed"));
    Ok(())
}

struct FileEditingProvider {
    calls: Mutex<usize>,
}

impl FileEditingProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for FileEditingProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
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

#[tokio::test]
async fn run_once_collects_changed_files_from_mutating_tools() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(FileEditingProvider::new()),
    )?;

    let response = run_once_with_host(host, run_request("write a file")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert!(response
        .changed_files
        .iter()
        .any(|path| path.ends_with("notes/result.txt")));
    Ok(())
}

struct MultiMutatingToolsProvider {
    calls: Mutex<usize>,
}

impl MultiMutatingToolsProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for MultiMutatingToolsProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::ToolUse {
                        id: "patch-1".into(),
                        name: "ApplyPatch".into(),
                        input: json!({
                            "patch": "--- /dev/null\n+++ b/notes/result.txt\n@@ -0,0 +1,1 @@\n+alpha\n"
                        }),
                    },
                    ModelBlock::ToolUse {
                        id: "patch-2".into(),
                        name: "ApplyPatch".into(),
                        input: json!({
                            "patch": "--- a/notes/result.txt\n+++ b/notes/result.txt\n@@ -1,1 +1,1 @@\n-alpha\n+beta\n"
                        }),
                    },
                    ModelBlock::ToolUse {
                        id: "patch-3".into(),
                        name: "ApplyPatch".into(),
                        input: json!({
                            "patch": "--- /dev/null\n+++ b/notes/extra.txt\n@@ -0,0 +1,1 @@\n+gamma\n"
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

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "multiple mutating tools complete".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn run_once_collects_changed_files_from_multiple_mutating_tools() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir.clone(), home_dir),
        Arc::new(MultiMutatingToolsProvider::new()),
    )?;

    let response = run_once_with_host(host, run_request("mutate multiple files")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.changed_files.len(), 2);
    assert!(
        response
            .changed_files
            .iter()
            .any(|path| path.ends_with("notes/result.txt")),
        "changed_files={:?}",
        response.changed_files
    );
    assert!(
        response
            .changed_files
            .iter()
            .any(|path| path.ends_with("notes/extra.txt")),
        "changed_files={:?}",
        response.changed_files
    );
    assert_eq!(
        std::fs::read_to_string(workspace_dir.join("notes").join("result.txt"))?,
        "beta\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_dir.join("notes").join("extra.txt"))?,
        "gamma\n"
    );
    Ok(())
}

struct TerminalDeliveryProvider {
    calls: Mutex<usize>,
}

impl TerminalDeliveryProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for TerminalDeliveryProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        match *calls {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![
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
            }),
            2 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Perfect! All tests pass. Let me create a summary document of what was changed:".into(),
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
            _ => panic!("unexpected extra provider round: {:?}", request.tools),
        }
    }
}

struct EmptyTerminalDeliveryProvider {
    calls: Mutex<usize>,
}

impl EmptyTerminalDeliveryProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

struct SleepOnlyTerminalProvider {
    calls: Mutex<usize>,
}

impl SleepOnlyTerminalProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for SleepOnlyTerminalProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        match *calls {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "patch-1".into(),
                    name: "ApplyPatch".into(),
                    input: json!({
                        "patch": "--- /dev/null\n+++ b/notes/result.txt\n@@ -0,0 +1,1 @@\n+hello from holon\n"
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            2 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "sleep-1".into(),
                    name: "Sleep".into(),
                    input: json!({
                        "reason": "sleep requested"
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => panic!("unexpected extra provider round: {:?}", request.tools),
        }
    }
}

#[async_trait]
impl AgentProvider for EmptyTerminalDeliveryProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        match *calls {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![
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
            }),
            2 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Verification is done. I'll package the final answer now.".into(),
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
            _ => panic!("unexpected extra provider round: {:?}", request.tools),
        }
    }
}

#[tokio::test]
async fn run_once_uses_last_assistant_message_without_terminal_delivery_round() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(TerminalDeliveryProvider::new()),
    )?;

    let response = run_once_with_host(host, run_request("write and verify a file")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(
        response.final_text,
        "Perfect! All tests pass. Let me create a summary document of what was changed:"
    );
    Ok(())
}

#[tokio::test]
async fn run_once_keeps_last_assistant_message_without_structured_fallback() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(EmptyTerminalDeliveryProvider::new()),
    )?;

    let response = run_once_with_host(host, run_request("write and verify a file")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(
        response.final_text,
        "Verification is done. I'll package the final answer now."
    );
    Ok(())
}

#[tokio::test]
async fn run_once_leaves_final_text_empty_without_assistant_text() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(SleepOnlyTerminalProvider::new()),
    )?;

    let response = run_once_with_host(host, run_request("write a file")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.final_text, "");
    Ok(())
}

struct SleepTaskProvider {
    calls: Mutex<usize>,
    duration_ms: u64,
}

impl SleepTaskProvider {
    fn new(duration_ms: u64) -> Self {
        Self {
            calls: Mutex::new(0),
            duration_ms,
        }
    }
}

#[async_trait]
impl AgentProvider for SleepTaskProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request.tools.iter().any(|tool| tool.name == "Sleep"));
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "task-1".into(),
                    name: "Sleep".into(),
                    input: json!({
                        "reason": "background nap",
                        "duration_ms": self.duration_ms
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "sleep finished".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn run_once_waits_for_background_tasks_by_default() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(SleepTaskProvider::new(50)),
    )?;

    let response = run_once_with_host(host, run_request("create a short task")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.final_text, "sleep finished");
    assert_eq!(response.sleep_reason.as_deref(), Some("background nap"));
    assert!(response.tasks.is_empty());
    Ok(())
}

struct CommandTaskProvider {
    calls: Mutex<usize>,
    cmd: String,
}

impl CommandTaskProvider {
    fn new(cmd: impl Into<String>) -> Self {
        Self {
            calls: Mutex::new(0),
            cmd: cmd.into(),
        }
    }
}

#[async_trait]
impl AgentProvider for CommandTaskProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request.tools.iter().any(|tool| tool.name == "ExecCommand"));
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "task-1".into(),
                    name: "ExecCommand".into(),
                    input: json!({
                        "cmd": &self.cmd,
                        "shell": "sh",
                        "yield_time_ms": 10,
                        "login": false,
                        "tty": false,
                        "max_output_tokens": 256
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "command task queued".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn run_once_waits_for_command_tasks_by_default() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(CommandTaskProvider::new("sleep 0.1; printf command_ok")),
    )?;

    let response = run_once_with_host(host, run_request("create a managed command task")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.tasks.len(), 1);
    assert_eq!(response.tasks[0].task.kind, "command_task");
    assert_eq!(response.tasks[0].task.status, TaskStatus::Completed);
    assert_eq!(response.tasks[0].task.output_preview, "command_ok");
    Ok(())
}

#[tokio::test]
async fn run_once_no_wait_does_not_interrupt_session_local_sleep() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(SleepTaskProvider::new(500)),
    )?;

    let response = run_once_with_host(
        host,
        RunOnceRequest {
            wait_for_tasks: false,
            ..run_request("create a long task")
        },
    )
    .await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert!(
        response.final_text.is_empty() || response.final_text == "sleep finished",
        "unexpected final_text: {:?}",
        response.final_text
    );
    assert_eq!(response.sleep_reason.as_deref(), Some("background nap"));
    assert!(response.tasks.is_empty());
    Ok(())
}

#[tokio::test]
async fn run_once_no_wait_stops_unfinished_command_tasks_before_exit() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(CommandTaskProvider::new("sleep 5")),
    )?;

    let response = run_once_with_host(
        host,
        RunOnceRequest {
            wait_for_tasks: false,
            ..run_request("create a long managed command task")
        },
    )
    .await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.tasks.len(), 1);
    assert_eq!(response.tasks[0].task.kind, "command_task");
    assert!(matches!(
        response.tasks[0].task.status,
        TaskStatus::Cancelling | TaskStatus::Cancelled
    ));
    Ok(())
}

#[tokio::test]
async fn run_once_no_wait_allows_short_tasks_to_finish_during_quiescence() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(SleepTaskProvider::new(50)),
    )?;

    let response = run_once_with_host(
        host,
        RunOnceRequest {
            wait_for_tasks: false,
            ..run_request("create a short task")
        },
    )
    .await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.final_text, "sleep finished");
    assert_eq!(response.sleep_reason.as_deref(), Some("background nap"));
    assert!(response.tasks.is_empty());
    Ok(())
}

#[tokio::test]
async fn run_once_no_wait_allows_short_command_tasks_to_finish_during_quiescence() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(CommandTaskProvider::new(
            "sleep 0.1 && printf quick_command_ok",
        )),
    )?;

    let response = run_once_with_host(
        host,
        RunOnceRequest {
            wait_for_tasks: false,
            ..run_request("create a short managed command task")
        },
    )
    .await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.tasks.len(), 1);
    assert_eq!(response.tasks[0].task.kind, "command_task");
    assert_eq!(response.tasks[0].task.status, TaskStatus::Completed);
    assert_eq!(response.tasks[0].task.output_preview, "quick_command_ok");
    Ok(())
}

#[tokio::test]
async fn run_once_prefers_parent_final_result_over_delegated_task_briefs() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(DelegatedRunOnceProvider::new()),
    )?;

    let response = run_once_with_host(host, run_request("delegate bounded work")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.final_text, "parent final result");
    assert_eq!(response.tasks.len(), 1);
    assert_eq!(response.tasks[0].task.kind, "child_agent_task");
    assert_eq!(response.tasks[0].task.status, TaskStatus::Completed);
    assert!(
        !response.final_text.contains("child delegated result"),
        "parent-facing final_text should not be selected from child task output"
    );
    Ok(())
}

struct TwoRoundProvider {
    calls: Mutex<usize>,
}

struct DelegatedRunOnceProvider {
    parent_calls: Mutex<usize>,
}

impl DelegatedRunOnceProvider {
    fn new() -> Self {
        Self {
            parent_calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for DelegatedRunOnceProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if request.tools.is_empty() {
            sleep(Duration::from_millis(100)).await;
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "child delegated result".into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let mut parent_calls = self.parent_calls.lock().await;
        *parent_calls += 1;
        match *parent_calls {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "task-1".into(),
                    name: "SpawnAgent".into(),
                    input: json!({
                        "summary": "delegated boundary task",
                        "prompt": "delegated-child",
                        "workspace_mode": "inherit"
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "parent final result".into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
        }
    }
}

impl TwoRoundProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for TwoRoundProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            assert!(request.tools.iter().any(|tool| tool.name == "AgentGet"));
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
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "two rounds complete".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn run_once_enforces_soft_max_turns_boundary() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(TwoRoundProvider::new()),
    )?;

    let response = run_once_with_host(
        host,
        RunOnceRequest {
            max_turns: Some(1),
            ..run_request("two round prompt")
        },
    )
    .await?;

    assert_eq!(response.final_status, RunFinalStatus::MaxTurnsExceeded);
    assert_eq!(response.model_rounds, 2);
    Ok(())
}

#[tokio::test]
async fn run_once_max_turns_respects_wait_for_command_tasks() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(CommandTaskProvider::new("sleep 0.5; printf command_ok")),
    )?;

    let response = run_once_with_host(
        host,
        RunOnceRequest {
            max_turns: Some(1),
            ..run_request("create a long managed command task")
        },
    )
    .await?;

    assert_eq!(response.final_status, RunFinalStatus::MaxTurnsExceeded);
    assert_eq!(response.tasks.len(), 1);
    assert_eq!(response.tasks[0].task.kind, "command_task");
    assert_eq!(response.tasks[0].task.status, TaskStatus::Completed);
    assert_eq!(response.tasks[0].task.output_preview, "command_ok");
    Ok(())
}

struct WorktreeTaskProvider {
    calls: Mutex<usize>,
}

impl WorktreeTaskProvider {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for WorktreeTaskProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if request.tools.is_empty() {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "worktree subagent finished".into(),
                }],
                stop_reason: None,
                input_tokens: 50,
                output_tokens: 20,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "task-1".into(),
                    name: "SpawnAgent".into(),
                    input: json!({
                        "summary": "try worktree path",
                        "prompt": "inspect this worktree",
                        "workspace_mode": "worktree"
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "worktree task queued".into(),
            }],
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn run_once_includes_worktree_task_metadata() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    init_git_repo(&workspace_dir)?;
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(WorktreeTaskProvider::new()),
    )?;

    let response = run_once_with_host(host, run_request("create a worktree task")).await?;

    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.tasks.len(), 1);
    let worktree = response.tasks[0]
        .worktree
        .as_ref()
        .expect("worktree metadata should be present");
    assert!(!worktree.worktree_path.is_empty());
    assert_eq!(
        worktree.worktree_branch,
        format!("task-{}", response.tasks[0].task.task_id)
    );
    Ok(())
}

#[tokio::test]
async fn run_once_can_target_a_persistent_named_agent_session() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let provider = Arc::new(StubProvider::new("persistent result"));
    let first_host = RuntimeHost::new_with_provider(
        test_config(workspace_dir.clone(), home_dir.clone()),
        provider.clone(),
    )?;

    let first = run_once_with_host(
        first_host,
        RunOnceRequest {
            agent_id: Some("bench-15".into()),
            create_agent: true,
            ..run_request("first turn")
        },
    )
    .await?;
    assert_eq!(first.agent_id, "bench-15");

    let first_state = AppStorage::new(home_dir.join("agents").join("bench-15"))?
        .read_agent()?
        .expect("expected persistent agent state after first run");
    assert!(first_state.total_message_count > 0);

    let second_host =
        RuntimeHost::new_with_provider(test_config(workspace_dir, home_dir.clone()), provider)?;
    let listed_agents = second_host
        .list_agents()
        .await?
        .into_iter()
        .map(|summary| summary.identity.agent_id)
        .collect::<Vec<_>>();
    assert!(listed_agents.iter().any(|id| id == "bench-15"));

    let second = run_once_with_host(
        second_host,
        RunOnceRequest {
            agent_id: Some("bench-15".into()),
            ..run_request("second turn")
        },
    )
    .await?;
    assert_eq!(second.agent_id, "bench-15");

    let second_state = AppStorage::new(home_dir.join("agents").join("bench-15"))?
        .read_agent()?
        .expect("expected persistent agent state after second run");
    assert!(second_state.total_message_count > first_state.total_message_count);
    assert!(home_dir.join("agents").join("bench-15").exists());
    Ok(())
}

#[tokio::test]
async fn run_once_rejects_template_without_create_agent() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(StubProvider::new("ignored")),
    )?;

    let err = run_once_with_host(
        host,
        RunOnceRequest {
            agent_id: Some("bench-15".into()),
            template: Some("holon-reviewer".into()),
            ..run_request("continue existing agent")
        },
    )
    .await
    .expect_err("template without create_agent should be rejected");

    assert!(err
        .to_string()
        .contains("template requires create_agent=true"));
    Ok(())
}

#[tokio::test]
async fn run_once_preserves_existing_workspace_binding_for_persistent_agents() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir.clone(), home_dir.clone()),
        Arc::new(StubProvider::new("continued in existing workspace")),
    )?;

    host.create_named_agent("bench-15", None).await?;
    let runtime = host.get_public_agent("bench-15").await?;
    let workspace = host.ensure_workspace_entry(workspace_dir.clone())?;
    runtime.attach_workspace(&workspace).await?;
    let nested_cwd = workspace_dir.join("nested");
    std::fs::create_dir_all(&nested_cwd)?;
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            Some(nested_cwd.clone()),
            None,
        )
        .await?;
    let initial_state = runtime.agent_state().await?;
    let initial_entry = initial_state
        .active_workspace_entry
        .clone()
        .expect("expected active workspace entry");

    let response = run_once_with_host(
        host,
        RunOnceRequest {
            agent_id: Some("bench-15".into()),
            ..run_request("continue inside existing workspace")
        },
    )
    .await?;
    assert_eq!(response.agent_id, "bench-15");

    let resumed_state = AppStorage::new(home_dir.join("agents").join("bench-15"))?
        .read_agent()?
        .expect("expected resumed persistent agent state");
    let resumed_entry = resumed_state
        .active_workspace_entry
        .clone()
        .expect("expected resumed active workspace entry");

    assert_eq!(
        resumed_state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone()),
        initial_state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
    );
    assert_eq!(
        resumed_state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.cwd.clone()),
        Some(nested_cwd)
    );
    assert_eq!(
        resumed_entry.projection_kind,
        WorkspaceProjectionKind::CanonicalRoot
    );
    assert_eq!(resumed_entry.execution_root, initial_entry.execution_root);
    assert!(resumed_state.worktree_session.is_none());
    Ok(())
}

#[tokio::test]
async fn run_once_rejects_stopped_persistent_agent() -> Result<()> {
    let home_dir = tempdir()?.keep();
    let workspace_dir = tempdir()?.keep();
    let host = RuntimeHost::new_with_provider(
        test_config(workspace_dir, home_dir),
        Arc::new(StubProvider::new("persistent result")),
    )?;

    let runtime = host.default_runtime().await?;
    runtime.control(ControlAction::Stop).await?;

    let error = run_once_with_host(
        host,
        RunOnceRequest {
            agent_id: Some("default".into()),
            ..run_request("hello after stop")
        },
    )
    .await
    .expect_err("run_once should reject stopped persistent agents");
    assert!(error
        .to_string()
        .contains("agent default is stopped; resume first"));
    Ok(())
}
