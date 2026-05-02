#![allow(dead_code)]

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use holon::provider::{
    AgentProvider, ConversationMessage, ModelBlock, ProviderTurnRequest, ProviderTurnResponse,
};
use serde_json::json;
use tokio::sync::Mutex;

use super::{
    runtime_helpers::{compact_request_snapshot, delegated_prompt_text},
    runtime_providers::CapturedTurnRequest,
};

/// Provider that demonstrates max-output recovery flow
pub struct MaxOutputRecoveryProvider {
    calls: Arc<Mutex<usize>>,
}

impl MaxOutputRecoveryProvider {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
        }
    }
}

#[async_trait]
impl AgentProvider for MaxOutputRecoveryProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let call_count = *calls;

        // First call: generate extensive text that will hit max_tokens
        if call_count == 1 {
            let extensive_text = "COMPREHENSIVE TECHNICAL REPORT\n\n\
                ## System Architecture Patterns\n\n\
                Modern distributed systems require careful architectural planning. \
                Microservices architecture enables independent scaling and deployment. \
                Event-driven patterns facilitate loose coupling between components.\n\n\
                ## Data Flow Strategies\n\n\
                Data pipelines must handle both batch and streaming workloads efficiently. \
                CQRS patterns separate read and write operations for optimal performance. \
                Event sourcing provides audit trails and enables temporal queries.\n\n\
                ## Security Considerations\n\n\
                Zero-trust security models assume no implicit trust within the network perimeter. \
                End-to-end encryption protects data in transit. \
                Identity and access management must be granular and auditable.\n\n\
                ## Performance Optimization\n\n\
                Caching strategies reduce latency and database load. \
                CDN distribution improves content delivery times. \
                Database indexing and query optimization are essential for scale.\n\n\
                ## Monitoring Approaches\n\n\
                Distributed tracing provides visibility across service boundaries. \
                Metrics collection enables performance analysis and alerting. \
                Log aggregation and analysis support debugging and compliance.";

            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: extensive_text.into(),
                }],
                stop_reason: Some("max_tokens".into()),
                input_tokens: 100,
                output_tokens: 1000,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        // Second call: Sleep-only completion (after max-output recovery)
        if call_count == 2 {
            // Verify that conversation history includes the previous extensive text
            let has_previous_text = request
                .conversation
                .iter()
                .any(|msg| matches!(msg, ConversationMessage::AssistantBlocks(_)));

            assert!(
                has_previous_text,
                "conversation should include previous assistant text"
            );

            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "sleep-1".into(),
                    name: "Sleep".into(),
                    input: json!({
                        "reason": "Generated comprehensive technical report covering architecture patterns, data flow strategies, security considerations, performance optimization, and monitoring approaches. All requested sections have been completed."
                    }),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        anyhow::bail!("unexpected provider call: {}", call_count)
    }
}

/// Provider that demonstrates repeated compaction behavior
pub struct RepeatedCompactionProvider {
    calls: Arc<Mutex<usize>>,
    requests: Arc<Mutex<Vec<super::runtime_helpers::CompactionRequestSnapshot>>>,
}

impl RepeatedCompactionProvider {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn captured_requests(&self) -> Vec<super::runtime_helpers::CompactionRequestSnapshot> {
        self.requests.lock().await.clone()
    }
}

#[async_trait]
impl AgentProvider for RepeatedCompactionProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let call_count = *calls;

        let snapshot = compact_request_snapshot(call_count, &request);
        self.requests.lock().await.push(snapshot);

        match call_count {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 1: initial reconnaissance. ".repeat(140),
                    },
                    ModelBlock::ToolUse {
                        id: "probe-round-1-exec".into(),
                        name: "ExecCommand".into(),
                        input: json!({
                            "cmd": "printf 'round-1-output'",
                            "max_output_tokens": 12,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            2 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 2: follow-up probing continues. ".repeat(140),
                    },
                    ModelBlock::ToolUse {
                        id: "probe-round-2-exec".into(),
                        name: "ExecCommand".into(),
                        input: json!({
                            "cmd": "printf 'round-2-output'",
                            "max_output_tokens": 12,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            3 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 3: checkpoint anchor remains unchanged. ".repeat(140),
                    },
                    ModelBlock::ToolUse {
                        id: "probe-round-3-exec".into(),
                        name: "ExecCommand".into(),
                        input: json!({
                            "cmd": "printf 'round-3-output'",
                            "max_output_tokens": 12,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            4 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Round 4: checkpoint-ready continuation with deterministic next step. "
                        .repeat(140),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Checkpointed review complete; carry the current discovery forward."
                        .into(),
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

/// Provider that demonstrates max-output followed by compaction
pub struct MaxOutputThenCompactionProvider {
    calls: Arc<Mutex<usize>>,
    requests: Arc<Mutex<Vec<super::runtime_helpers::CompactionRequestSnapshot>>>,
}

impl MaxOutputThenCompactionProvider {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn captured_requests(&self) -> Vec<super::runtime_helpers::CompactionRequestSnapshot> {
        self.requests.lock().await.clone()
    }
}

#[async_trait]
impl AgentProvider for MaxOutputThenCompactionProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let call_count = *calls;

        let snapshot = compact_request_snapshot(call_count, &request);
        self.requests.lock().await.push(snapshot);

        match call_count {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "[analysis] ".repeat(260),
                }],
                stop_reason: Some("max_tokens".into()),
                input_tokens: 100,
                output_tokens: 1500,
                cache_usage: None,
                request_diagnostics: None,
            }),
            2 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text:
                            "Round 2: recovery continuation introduces structured output evidence. "
                                .repeat(70),
                    },
                    ModelBlock::ToolUse {
                        id: "recovery-round-2-exec".into(),
                        name: "ExecCommand".into(),
                        input: json!({
                            "cmd": "printf 'recovery-round-2-output'",
                            "max_output_tokens": 16,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 100,
                output_tokens: 60,
                cache_usage: None,
                request_diagnostics: None,
            }),
            3 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 3: continued verification without dropping context. "
                            .repeat(70),
                    },
                    ModelBlock::ToolUse {
                        id: "recovery-round-3-exec".into(),
                        name: "ExecCommand".into(),
                        input: json!({
                            "cmd": "printf 'recovery-round-3-output'",
                            "max_output_tokens": 16,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 100,
                output_tokens: 60,
                cache_usage: None,
                request_diagnostics: None,
            }),
            4 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Round 4: follow-up synthesis for compacted checkpoint continuity."
                        .into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 30,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Recovery path complete.".into(),
                }],
                stop_reason: None,
                input_tokens: 100,
                output_tokens: 20,
                cache_usage: None,
                request_diagnostics: None,
            }),
        }
    }
}

/// Provider that demonstrates multi-pass compaction recovery flow
pub struct MultiPassCompactionRecoveryFlowProvider {
    calls: Arc<Mutex<usize>>,
    requests: Arc<Mutex<Vec<CapturedTurnRequest>>>,
    task_id: Arc<Mutex<Option<String>>>,
}

impl MultiPassCompactionRecoveryFlowProvider {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            task_id: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn set_task_id(&self, task_id: String) {
        let mut id = self.task_id.lock().await;
        *id = Some(task_id);
    }

    pub async fn captured_requests(&self) -> Vec<CapturedTurnRequest> {
        self.requests.lock().await.clone()
    }
}

#[async_trait]
impl AgentProvider for MultiPassCompactionRecoveryFlowProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let call_index = {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            *calls
        };

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

        match call_index {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: format!(
                        "Round 1 planning {} {}",
                        "deep-context reconstruction ".repeat(240),
                        "Output token limit hit. Continue exactly where you left off. Do not restart from the top."
                    ),
                }],
                stop_reason: Some("max_tokens".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            }),
            2 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: format!(
                            "Round 2 checkpoint signal {}. [Runtime-generated full progress checkpoint request]",
                            "delta capture ".repeat(220)
                        ),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-2".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "awk 'BEGIN { for (i=0; i<900; i++) printf \"exec_round_2_marker \" }'",
                            "max_output_tokens": 24,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            }),
            3 => {
                let task_id = self
                    .task_id
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_else(|| "missing-task".into());
                Ok(ProviderTurnResponse {
                    blocks: vec![
                        ModelBlock::Text {
                            text: format!(
                                "Round 3 follow-up {}",
                                "continuity anchor ".repeat(220)
                            ),
                        },
                        ModelBlock::ToolUse {
                            id: "task-output-round-3".into(),
                            name: "TaskOutput".into(),
                            input: serde_json::json!({
                                "task_id": task_id,
                                "block": false,
                            }),
                        },
                    ],
                    stop_reason: Some("tool_use".into()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            4 => Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: format!(
                            "Round 4 checkpointing {}",
                            "stable progression ".repeat(220)
                        ),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-4".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "awk 'BEGIN { for (i=0; i<840; i++) printf \"exec_round_4_marker \" }'",
                            "max_output_tokens": 24,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Completed after bounded repeated compaction.".into(),
                }],
                stop_reason: Some("end_turn".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            }),
        }
    }
}
