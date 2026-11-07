use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::{
        material::tests::response, ProviderImageData, ProviderPromptFrame, ProviderRequestMaterial,
    },
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "anthropic-material".into(),
        identity_incarnation: 1,
    }
}

#[tokio::test]
async fn anthropic_json_and_sse_keep_image_acquisition_through_last_result() -> Result<()> {
    for streaming in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = ResourceReadMaterial::new(&ledger, &scope())?;
        let material = ProviderRequestMaterial::scoped(source.clone());
        let message = json!({
            "id":"message", "type":"message", "role":"assistant",
            "content":[{"type":"text","text":"已完成"}],
            "stop_reason":"end_turn", "usage":{"input_tokens":7,"output_tokens":3}
        });
        let body = if streaming {
            [
                json!({"type":"message_start", "message":{
                    "id":"message", "content":[], "usage":{"input_tokens":7}
                }}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"已完成"}}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
                json!({"type":"message_stop"}),
            ].iter().map(|event| format!("data: {event}\n\n")).collect::<String>()
        } else {
            message.to_string()
        };
        let (url, server) = response(
            body.as_bytes(),
            if streaming {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .await?;
        let home = tempfile::tempdir()?;
        let provider = AnthropicProvider {
            client: Client::new(),
            route_provider: "anthropic".into(),
            route_endpoint: "default".into(),
            base_url: url,
            auth_token: None,
            quota_identity: None,
            model: "test".into(),
            max_output_tokens: 128,
            context_window_tokens: None,
            reasoning_effort: None,
            supports_reasoning: false,
            context_management: Default::default(),
            cache_control_source: None,
            model_route: "anthropic@default/test".into(),
            builtin_web_search: None,
            trace_home_dir: home.path().into(),
            http_trace_override: None,
        };
        let request = ProviderTurnRequest {
            dispatch_guard: None,
            material: material.clone(),
            continuation_scope_id: None,
            prompt_frame: ProviderPromptFrame::plain("test"),
            conversation: vec![ConversationMessage::UserImage {
                prompt: "image".into(),
                media_type: "image/png".into(),
                data_base64: ProviderImageData::encode(b"abc", &source)?,
                optional_history: false,
            }],
            tools: vec![],
            native_web_search: None,
            response_format: None,
        };
        let result = provider.complete_turn(request).await?;
        server.await??;
        assert!(
            matches!(result.blocks.as_slice(), [ModelBlock::Text { text }] if text == "已完成")
        );
        assert_eq!((result.input_tokens, result.output_tokens), (7, 3));
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(
            result
                .request_diagnostics
                .as_ref()
                .unwrap()
                .transport_timeline
                .as_ref()
                .unwrap()
                .streaming,
            streaming
        );
        let weak = Arc::downgrade(&source.guard()?);
        let last = result.clone();
        drop((result, material, source, provider));
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}

#[test]
fn anthropic_sse_pending_and_sparse_cells_admit_exactly_before_growth() -> Result<()> {
    let scope = scope();
    let initial = (scope.agent_id.len() as u64 + 256) * 8;
    for sparse in [false, true] {
        let (bytes, records) = if sparse {
            (
                3 * std::mem::size_of::<ApiResponseBlock>() as u64 * 8,
                3 * 8,
            )
        } else {
            ((3 + 128) * 8, 8)
        };
        for (deny_bytes, deny_records) in [(0, 0), (1, 0), (0, 1)] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let source = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                initial + bytes - deny_bytes,
                64 + records - deny_records,
            )?;
            let material = ProviderRequestMaterial::scoped(source.clone());
            let mut parser = SseParser {
                material: material.clone(),
                ..Default::default()
            };
            let mut accumulator = AnthropicStreamAccumulator {
                material: material.clone(),
                ..Default::default()
            };
            let result = if sparse {
                accumulator.ensure_content_index(2)
            } else {
                parser.push(b"abc").map(|events| assert!(events.is_empty()))
            };
            if deny_bytes == 0 && deny_records == 0 {
                result?;
                assert_eq!(source.finish()?, (initial + bytes, 64 + records));
                assert_eq!(
                    (parser.buffer.len(), accumulator.content.len()),
                    if sparse { (0, 3) } else { (3, 0) }
                );
            } else {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
                let error = classify_anthropic_sse_parse_error(error, |_| {
                    panic!("quota is not a protocol error")
                });
                assert!(error
                    .downcast_ref::<crate::provider::retry::ProviderTransportError>()
                    .is_none());
                assert_eq!((parser.buffer.len(), accumulator.content.len()), (0, 0));
                assert!(source.finish().is_err());
            }
            drop((parser, accumulator, material, source));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn anthropic_sse_split_utf8_and_tool_json_preserve_protocol() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope())?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let mut parser = SseParser {
        material: material.clone(),
        ..Default::default()
    };
    let mut accumulator = AnthropicStreamAccumulator {
        material: material.clone(),
        ..Default::default()
    };
    let events = [
        json!({"type":"message_start","message":{"content":[]}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool","name":"test","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"值\":"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"中文\"}"}}),
        json!({"type":"message_stop"}),
    ];
    let body = events
        .iter()
        .map(|event| format!("data: {event}\r\n\r\n"))
        .collect::<String>();
    for chunk in body.as_bytes().chunks(1) {
        for event in parser.push(chunk)? {
            accumulator.apply(event, "test", "http://localhost", None)?;
        }
    }
    for event in parser.finish()? {
        accumulator.apply(event, "test", "http://localhost", None)?;
    }
    let result = accumulator.finish("test", "test", "http://localhost", None)?;
    assert_eq!(result.content[0].input, Some(json!({"值":"中文"})));
    source.finish()?;
    drop((result, material, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn anthropic_tool_json_delta_does_not_recopy_existing_prefix() -> Result<()> {
    let scope = scope();
    let initial = (scope.agent_id.len() as u64 + 256) * 8;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source =
        ResourceReadMaterial::with_test_limits(&ledger, &scope, initial + (1 + 128) * 8, 64 + 8)?;
    let mut prefix = String::with_capacity(4096);
    prefix.push_str(&"x".repeat(2048));
    let pointer = prefix.as_ptr();
    let mut block = ApiResponseBlock {
        input: Some(Value::String(prefix)),
        ..Default::default()
    };
    let delta: AnthropicStreamDelta =
        serde_json::from_value(json!({"type":"input_json_delta","partial_json":"y"}))?;
    source.read(|| apply_stream_delta(&mut block, Some(delta)))?;
    assert_eq!(
        block.input.as_ref().unwrap().as_str().unwrap().as_ptr(),
        pointer
    );
    assert_eq!(block.input.as_ref().unwrap().as_str().unwrap().len(), 2049);
    assert_eq!(source.finish()?, (initial + (1 + 128) * 8, 64 + 8));
    drop((block, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
