//! Opt-in comparison through the same adapters used by runtime inference.
//! No tool is executed: all tool results and source fixtures are synthetic.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::{provider::ProviderSessionId, tool::ToolSpec};

/// The comparison harness opts into a test-local endpoint; Go ships Messages,
/// not a built-in Responses endpoint or compatibility alias.
pub(super) fn enable_responses_comparison(
    config: &mut crate::config::AppConfig,
) -> anyhow::Result<()> {
    let id = crate::config::ProviderId::parse("go-responses-comparison")?;
    let mut runtime = config
        .providers
        .get(&crate::config::ProviderId::parse("opencode-go")?)
        .ok_or_else(|| anyhow::anyhow!("missing Go provider"))?
        .clone();
    runtime.id = id.clone();
    runtime.route_endpoint = crate::config::ProviderEndpointId::parse("responses")?;
    runtime.transport = crate::config::ProviderTransportKind::OpenAiResponses;
    config.providers.insert(id, runtime);
    Ok(())
}

#[test]
fn comparison_endpoint_is_test_local_and_not_builtin() {
    use crate::config::{ModelRouteCapability, ModelRouteRef, RuntimeModelCatalog};
    let mut fixture = super::support::test_config(
        "opencode-go/glm-5.3-flash",
        &[],
        Some("test-key"),
        None,
        false,
    );
    let go = crate::config::ProviderId::parse("opencode-go").unwrap();
    let mut runtime = fixture.config.providers[&crate::config::ProviderId::openai()].clone();
    runtime.id = go.clone();
    runtime.route_provider = go.clone();
    runtime.transport = crate::config::ProviderTransportKind::OpenAiChatCompletions;
    runtime.base_url = "https://opencode.ai/inference/go/v1".into();
    runtime.builtin_web_search = None;
    fixture.config.providers.insert(go, runtime);
    let route = ModelRouteRef::parse("opencode-go@responses/glm-5.3-flash").unwrap();
    let context = crate::context::ContextConfig::default();
    assert!(RuntimeModelCatalog::from_config(&fixture.config)
        .resolve_explicit_model_route(&context, &route, ModelRouteCapability::Turn)
        .is_none());
    enable_responses_comparison(&mut fixture.config).unwrap();
    assert!(RuntimeModelCatalog::from_config(&fixture.config)
        .resolve_explicit_model_route(&context, &route, ModelRouteCapability::Turn)
        .is_some());
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Repair,
    ToolError,
    LongContext,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::Repair => "repair",
            Self::ToolError => "tool_error",
            Self::LongContext => "long_context",
        }
    }

    fn rounds(self) -> usize {
        if matches!(self, Self::Repair) {
            2
        } else {
            3
        }
    }

    fn request(self) -> ProviderTurnRequest {
        let mut tools = vec![ToolSpec {
            name: "InspectCode".into(),
            description: "Inspect a synthetic Rust source and test failure; no filesystem access."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
            freeform_grammar: None,
        }];
        let instructions = match self {
            Self::Repair => "First call InspectCode exactly once with path src/add.rs. After its result, answer with the corrected function only.",
            Self::ToolError => "First call InspectCode with path src/add.rs. If it reports an error, retry InspectCode exactly once with the same path. After a successful result, answer with the corrected function only.",
            Self::LongContext => {
                tools.push(ToolSpec {
                    name: "InspectPolicy".into(),
                    description: "Read the synthetic output policy; no external access.".into(),
                    input_schema: json!({
                        "type": "object", "properties": {}, "additionalProperties": false
                    }),
                    freeform_grammar: None,
                });
                "First call InspectCode with path src/add.rs. After its result, call InspectPolicy with empty arguments. After its result, answer with the corrected function and the policy's required marker comment. Ignore unrelated inventory."
            }
        };
        let mut conversation = Vec::new();
        if matches!(self, Self::LongContext) {
            conversation.push(ConversationMessage::UserText(
                "We are fixing src/add.rs. Preserve the function signature. Other files are unrelated."
                    .into(),
            ));
            conversation.push(ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
                text: "I will inspect the synthetic source and policy before proposing a correction."
                    .into(),
            }]));
            let inventory = (0..256)
                .map(|index| {
                    format!(
                        "Unrelated inventory {index:03}: module fixture_{index:03} has no changes, status stable, checksum fixed.\n"
                    )
                })
                .collect::<String>();
            conversation.push(ConversationMessage::UserText(inventory));
            conversation.push(ConversationMessage::AssistantBlocks(vec![
                ModelBlock::Text {
                    text: "The inventory is unrelated; src/add.rs remains the only repair target."
                        .into(),
                },
            ]));
        }
        conversation.push(ConversationMessage::UserText(
            "Review this synthetic Rust source: fn add(a: i32, b: i32) -> i32 { a - b }. It should add. Use the supplied tools before answering.".into(),
        ));
        ProviderTurnRequest::plain(
            format!("You are a coding agent reviewing a tiny synthetic Rust function. {instructions} Do not use any other tools. Return only the corrected Rust source, optionally in one Rust code fence."),
            conversation,
            tools,
        )
    }

    fn accept(
        self,
        round: usize,
        response: &ProviderTurnResponse,
    ) -> Result<Vec<ToolResultBlock>, &'static str> {
        let calls = response
            .blocks
            .iter()
            .filter_map(|block| match block {
                ModelBlock::ToolUse {
                    id, name, input, ..
                } => Some((id, name, input)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if round + 1 == self.rounds() {
            let text = response
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ModelBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            let source = text.trim();
            let source = if let Some(fenced) = source.strip_prefix("```") {
                let Some((language, body)) = fenced.split_once('\n') else {
                    return Err("incorrect_final_source");
                };
                if !matches!(language.trim(), "" | "rust") {
                    return Err("incorrect_final_source");
                }
                let Some(body) = body.trim_end().strip_suffix("```") else {
                    return Err("incorrect_final_source");
                };
                body
            } else {
                source
            };
            let compact = source
                .lines()
                .filter(|line| line.trim() != "// PROTOCOL_MATRIX_OK")
                .flat_map(str::chars)
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>();
            if !calls.is_empty() {
                return Err("unexpected_final_tool_call");
            }
            if ![
                "fnadd(a:i32,b:i32)->i32{a+b}",
                "fnadd(a:i32,b:i32)->i32{returna+b;}",
            ]
            .iter()
            .any(|source| compact == *source)
            {
                return Err("incorrect_final_source");
            }
            if matches!(self, Self::LongContext) && !text.contains("PROTOCOL_MATRIX_OK") {
                return Err("missing_policy_marker");
            }
            return Ok(Vec::new());
        }
        if calls.len() != 1 {
            return Err("incorrect_tool_count");
        }
        let mut saw_code = false;
        let mut saw_policy = false;
        let mut results = Vec::new();
        for (id, name, input) in calls {
            let (content, is_error) = match name.as_str() {
                "InspectCode"
                    if !saw_code
                        && (!matches!(self, Self::LongContext) || round == 0)
                        && *input == json!({"path": "src/add.rs"}) =>
                {
                    saw_code = true;
                    if matches!(self, Self::ToolError) && round == 0 {
                        ("Synthetic temporary read failure. Retry InspectCode once with the same path.", true)
                    } else {
                        ("Synthetic test: add(2, 3) returned -1; expected 5. Replace subtraction with addition.", false)
                    }
                }
                "InspectPolicy"
                    if matches!(self, Self::LongContext)
                        && round == 1
                        && !saw_policy
                        && *input == json!({}) =>
                {
                    saw_policy = true;
                    ("Synthetic policy: include the comment // PROTOCOL_MATRIX_OK with the corrected function.", false)
                }
                _ => return Err("incorrect_tool_name_or_arguments"),
            };
            results.push(ToolResultBlock {
                tool_use_id: id.clone(),
                content: content.into(),
                is_error,
                error: None,
            });
        }
        Ok(results)
    }
}

fn emit(record: Value) {
    // Only explicit synthetic metadata, never credentials, headers, or error bodies.
    eprintln!("go_matrix {record}");
}

/// Default: two models × three scenarios × three repetitions × two protocols,
/// at most 96 adapter calls. Serial calls, 120s per call, no harness retries;
/// normal adapter retries and continuation fallback remain enabled.
#[tokio::test]
#[ignore = "live synthetic traffic; requires HOLON_LIVE_OPENCODE_GO=1"]
async fn messages_responses_comparison_matrix() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var("HOLON_LIVE_OPENCODE_GO").as_deref() == Ok("1"),
        "explicit live-traffic opt-in required"
    );
    let repetitions: usize = std::env::var("HOLON_LIVE_OPENCODE_GO_REPETITIONS")
        .unwrap_or_else(|_| "3".into())
        .parse()?;
    anyhow::ensure!(
        (1..=10).contains(&repetitions),
        "repetitions must be 1..=10"
    );
    let selected_model = std::env::var("HOLON_LIVE_OPENCODE_GO_MODEL").ok();
    let free_models = ["beam-free", "iceman-contributor-free", "longcat-2.5-free"];
    let models = match selected_model.as_deref() {
        None => vec!["glm-5.3-flash", "kimi-k3"],
        Some("free") => free_models.to_vec(),
        Some(model)
            if ["glm-5.3-flash", "kimi-k3", "deepseek-v4.1-flash"].contains(&model)
                || free_models.contains(&model) =>
        {
            vec![model]
        }
        Some(_) => anyhow::bail!("unknown comparison model"),
    };
    let selected_scenario = std::env::var("HOLON_LIVE_OPENCODE_GO_SCENARIO").ok();
    let scenarios = [Scenario::Repair, Scenario::ToolError, Scenario::LongContext]
        .into_iter()
        .filter(|scenario| {
            selected_scenario
                .as_deref()
                .is_none_or(|selected| selected == scenario.name())
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(!scenarios.is_empty(), "unknown scenario");
    let mut config = crate::config::AppConfig::load()?;
    config.runtime_max_output_tokens = 4096;
    enable_responses_comparison(&mut config)?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut failed = 0;
    let mut completed = 0;
    let mut adapter_calls = 0;
    let mut system_errors = 0;
    for repetition in 0..repetitions {
        for model in &models {
            for &scenario in &scenarios {
                let endpoints = if repetition % 2 == 0 {
                    ["messages", "responses"]
                } else {
                    ["responses", "messages"]
                };
                for endpoint in endpoints {
                    let route = crate::config::ModelRouteRef::parse_compatible(&format!(
                        "opencode-go@{endpoint}/{model}"
                    ))?;
                    let candidate = build_candidate(&config, &route)?;
                    let mut request = scenario.request();
                    request.continuation_scope_id =
                        crate::provider::ContinuationScopeId::new(format!(
                            "{run_id}/{model}/{}/{repetition}/{endpoint}",
                            scenario.name()
                        ));
                    request.provider_session_id = Some(ProviderSessionId::from_runtime_identity(
                        &run_id,
                        &format!("{model}/{}/{repetition}/{endpoint}", scenario.name()),
                        1,
                    ));
                    let conversation_started = Instant::now();
                    let mut failure = None;
                    for round in 0..scenario.rounds() {
                        adapter_calls += 1;
                        let started = Instant::now();
                        let result = tokio::time::timeout(
                            Duration::from_secs(120),
                            candidate.provider.complete_turn(request.clone()),
                        )
                        .await;
                        let mut record = json!({
                            "kind": "request", "run": run_id, "model": model,
                            "endpoint": endpoint, "scenario": scenario.name(),
                            "repetition": repetition, "round": round,
                            "elapsed_ms": started.elapsed().as_millis(),
                        });
                        let response = match result {
                            Ok(Ok(response)) => response,
                            Ok(Err(error)) => {
                                let status = error
                                    .downcast_ref::<crate::provider::retry::ProviderTransportError>(
                                    )
                                    .and_then(|error| error.status);
                                record["outcome"] = json!("provider_error");
                                record["status"] = json!(status);
                                record["error_class"] = json!(format!(
                                    "{:?}",
                                    crate::provider::retry::classify_provider_error(&error).kind
                                ));
                                emit(record);
                                if matches!(status, Some(401 | 403 | 429)) {
                                    system_errors += 1;
                                }
                                failure = Some("provider_error");
                                break;
                            }
                            Err(_) => {
                                record["outcome"] = json!("timeout");
                                emit(record);
                                failure = Some("timeout");
                                break;
                            }
                        };
                        record["input_tokens"] = json!(response.input_tokens);
                        record["output_tokens"] = json!(response.output_tokens);
                        record["cache_usage"] = json!(response.cache_usage);
                        record["streaming"] = json!(response
                            .request_diagnostics
                            .as_ref()
                            .and_then(|d| d.streaming));
                        if let Some(diagnostics) = &response.request_diagnostics {
                            record["lowering_mode"] = json!(diagnostics.request_lowering_mode);
                            record["continuation"] = json!(diagnostics.incremental_continuation);
                            if let Some(timeline) = &diagnostics.transport_timeline {
                                record["response_headers_ms"] = json!((timeline
                                    .response_headers_at
                                    - timeline.request_started_at)
                                    .num_milliseconds());
                            }
                        }
                        record["stop_reason"] = json!(response.stop_reason);
                        record["tool_calls"] = json!(response
                            .blocks
                            .iter()
                            .filter(|block| matches!(block, ModelBlock::ToolUse { .. }))
                            .count());
                        match scenario.accept(round, &response) {
                            Ok(results) => {
                                record["outcome"] = json!("accepted");
                                emit(record);
                                if !results.is_empty() {
                                    request.conversation.push(
                                        ConversationMessage::AssistantBlocks(response.blocks),
                                    );
                                    request
                                        .conversation
                                        .push(ConversationMessage::UserToolResults(results));
                                }
                            }
                            Err(reason) => {
                                record["outcome"] = json!(reason);
                                emit(record);
                                failure = Some(reason);
                                break;
                            }
                        }
                    }
                    completed += 1;
                    failed += usize::from(failure.is_some());
                    emit(json!({
                        "kind": "conversation", "run": run_id, "model": model,
                        "endpoint": endpoint, "scenario": scenario.name(),
                        "repetition": repetition, "outcome": failure.unwrap_or("passed"),
                        "elapsed_ms": conversation_started.elapsed().as_millis(),
                    }));
                    // Do not hammer a broken credential or a rate-limited subscription.
                    anyhow::ensure!(
                        system_errors < 2,
                        "stopped after two auth/quota errors; inspect sanitized records"
                    );
                }
            }
        }
    }
    emit(
        json!({"kind": "summary", "run": run_id, "adapter_calls": adapter_calls, "conversations": completed, "failed": failed}),
    );
    anyhow::ensure!(
        failed == 0,
        "{failed}/{completed} synthetic conversations failed; inspect sanitized records"
    );
    Ok(())
}

#[test]
fn comparison_fixture_validates_repair_errors_and_policy() {
    fn response(blocks: Vec<ModelBlock>) -> ProviderTurnResponse {
        ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        }
    }
    let code = ModelBlock::ToolUse {
        id: "code".into(),
        name: "InspectCode".into(),
        input: json!({"path": "src/add.rs"}),
        kind: Default::default(),
        provider_data: None,
    };
    let first = response(vec![code.clone()]);
    assert!(!Scenario::Repair.accept(0, &first).unwrap()[0].is_error);
    assert!(Scenario::ToolError.accept(0, &first).unwrap()[0].is_error);
    assert!(!Scenario::ToolError.accept(1, &first).unwrap()[0].is_error);
    assert_eq!(Scenario::LongContext.accept(0, &first).unwrap().len(), 1);
    assert!(Scenario::LongContext.accept(1, &first).is_err());
    let policy = ModelBlock::ToolUse {
        id: "policy".into(),
        name: "InspectPolicy".into(),
        input: json!({}),
        kind: Default::default(),
        provider_data: None,
    };
    let policy_response = response(vec![policy.clone()]);
    assert!(Scenario::LongContext.accept(0, &policy_response).is_err());
    assert_eq!(
        Scenario::LongContext
            .accept(1, &policy_response)
            .unwrap()
            .len(),
        1
    );
    assert!(Scenario::LongContext
        .accept(0, &response(vec![code, policy]))
        .is_err());
    let final_source = response(vec![ModelBlock::Text {
        text: "fn add(a: i32, b: i32) -> i32 { a + b }".into(),
    }]);
    assert!(Scenario::Repair.accept(1, &final_source).is_ok());
    assert!(Scenario::LongContext.accept(2, &final_source).is_err());
    assert!(Scenario::Repair.accept(1, &response(Vec::new())).is_err());
    let explicit_return = response(vec![ModelBlock::Text {
        text: "// PROTOCOL_MATRIX_OK\nfn add(a: i32, b: i32) -> i32 { return a + b; }".into(),
    }]);
    assert!(Scenario::LongContext.accept(2, &explicit_return).is_ok());
    let incorrect_source = response(vec![ModelBlock::Text {
        text: "fn add(a: i32, b: i32) -> i32 { a - b }".into(),
    }]);
    assert!(Scenario::Repair.accept(1, &incorrect_source).is_err());
    for text in [
        "/* fn add(a: i32, b: i32) -> i32 { a + b } */ fn add(a: i32, b: i32) -> i32 { a - b }",
        "// fn add(a: i32, b: i32) -> i32 { a + b }",
        "```rust\nfn add(a: i32, b: i32) -> i32 { a + b }\n```\nfn add(a: i32, b: i32) -> i32 { a - b }",
        "fn add(a: i32, b: i32) -> i32 { a + b } fn main() {}",
    ] {
        assert!(Scenario::Repair
            .accept(1, &response(vec![ModelBlock::Text { text: text.into() }]))
            .is_err());
    }
    let fenced = response(vec![ModelBlock::Text {
        text: "```rust\n// PROTOCOL_MATRIX_OK\nfn add(a: i32, b: i32) -> i32 { a + b }\n```".into(),
    }]);
    assert!(Scenario::LongContext.accept(2, &fenced).is_ok());
}
