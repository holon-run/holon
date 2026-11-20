use super::*;
use crate::provider::ProviderSessionId;
use serde_json::json;

#[tokio::test]
#[ignore = "requires explicit HOLON_LIVE_OPENCODE_GO=1 and a configured Go subscription"]
async fn live_go_messages_responses_tool_round_trip() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var("HOLON_LIVE_OPENCODE_GO").as_deref() == Ok("1"),
        "set HOLON_LIVE_OPENCODE_GO=1 to authorize up to four paid requests"
    );
    let mut config = crate::config::AppConfig::load()?;
    config.runtime_max_output_tokens = 1024;
    super::live_go_comparison::enable_responses_comparison(&mut config)?;
    let model = "glm-5.3-flash";
    let session = ProviderSessionId::from_runtime_identity(
        "synthetic-protocol-test-installation",
        "synthetic-coding-conversation",
        1,
    );
    let mut failures = Vec::new();
    for endpoint in ["messages", "responses"] {
        let route = crate::config::ModelRouteRef::parse_compatible(&format!(
            "opencode-go@{endpoint}/{model}"
        ))?;
        let candidate = super::super::catalog::build_candidate(&config, &route)?;
        let mut request = ProviderTurnRequest::plain(
            "You are reviewing a tiny synthetic Rust function. First call InspectCode exactly once with path src/add.rs. After its result, answer with the corrected function only. Do not use other tools.",
            vec![ConversationMessage::UserText(
                "Review this synthetic source: fn add(a: i32, b: i32) -> i32 { a - b }. It should add. InspectCode returns the test failure; use it before answering.".into(),
            )],
            vec![crate::tool::ToolSpec {
                name: "InspectCode".into(),
                description: "Read a synthetic test result; no actual filesystem access.".into(),
                input_schema: json!({
                    "type":"object",
                    "properties":{"path":{"type":"string"}},
                    "required":["path"],
                    "additionalProperties":false
                }),
                freeform_grammar: None,
            }],
        );
        request.provider_session_id = Some(session.clone());
        for round in 0..2 {
            let started = std::time::Instant::now();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                candidate.provider.complete_turn(request.clone()),
            )
            .await;
            let response = match result {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let status = error
                        .downcast_ref::<crate::provider::retry::ProviderTransportError>()
                        .and_then(|error| error.status);
                    eprintln!("go_live endpoint={endpoint} model={model} round={round} error_class={:?} status={status:?} elapsed_ms={}", crate::provider::retry::classify_provider_error(&error).kind, started.elapsed().as_millis());
                    failures.push(format!("{endpoint} round {round}: provider error"));
                    break;
                }
                Err(_) => {
                    failures.push(format!("{endpoint} round {round}: 120s timeout"));
                    break;
                }
            };
            let calls = response
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ModelBlock::ToolUse {
                        id, name, input, ..
                    } => Some((id.clone(), name.clone(), input.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>();
            eprintln!("go_live endpoint={endpoint} model={model} round={round} elapsed_ms={} input_tokens={} output_tokens={} tool_calls={} stop_reason={:?} diagnostics={}", started.elapsed().as_millis(), response.input_tokens, response.output_tokens, calls.len(), response.stop_reason, serde_json::to_string(&response.request_diagnostics)?);
            if round == 0 {
                if calls.len() != 1
                    || calls[0].1 != "InspectCode"
                    || calls[0].2["path"] != "src/add.rs"
                {
                    failures.push(format!(
                        "{endpoint}: expected one valid synthetic tool call"
                    ));
                    break;
                }
                request
                    .conversation
                    .push(ConversationMessage::AssistantBlocks(response.blocks));
                request.conversation.push(ConversationMessage::UserToolResults(vec![crate::provider::ToolResultBlock {
                    tool_use_id: calls[0].0.clone(),
                    content: "Synthetic test: add(2, 3) returned -1; expected 5. Replace subtraction with addition.".into(),
                    is_error: false,
                    error: None,
                }]));
            } else if !calls.is_empty()
                || !response.blocks.iter().any(
                    |block| matches!(block, ModelBlock::Text { text } if text.contains("a + b")),
                )
            {
                failures.push(format!(
                    "{endpoint}: expected corrected function after tool result"
                ));
            }
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "bounded Go comparison failed: {}",
        failures.join("; ")
    );
    Ok(())
}
