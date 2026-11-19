use super::*;
use crate::provider::{
    PromptContentBlock, ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest,
    ProviderPromptFrame, ToolResultBlock,
};
use holon_domain::PromptStability;
use serde_json::json;

#[test]
fn ensure_placeholder_user_text_appends_to_tool_only_user_turn() {
    let mut messages = vec![ApiMessage {
        role: "user",
        content: json!([{ "type": "tool_result", "tool_use_id": "toolu_1" }]),
    }];
    ensure_placeholder_user_text(&mut messages);
    let blocks = messages[0].content.as_array().unwrap();
    let placeholder = blocks.last().unwrap();
    assert_eq!(placeholder["type"], json!("text"));
    assert!(!placeholder["text"].as_str().unwrap().trim().is_empty());
}

#[test]
fn ensure_placeholder_user_text_checks_latest_user_turn() {
    let mut messages = vec![
        ApiMessage {
            role: "user",
            content: json!([{ "type": "text", "text": "original query" }]),
        },
        ApiMessage {
            role: "assistant",
            content: json!([{ "type": "tool_use", "id": "toolu_1" }]),
        },
        ApiMessage {
            role: "user",
            content: json!([{ "type": "tool_result", "tool_use_id": "toolu_1" }]),
        },
    ];
    ensure_placeholder_user_text(&mut messages);
    ensure_placeholder_user_text(&mut messages);

    assert_eq!(messages.len(), 3);
    let blocks = messages[2].content.as_array().unwrap();
    assert_eq!(
        blocks,
        &vec![
            json!({ "type": "tool_result", "tool_use_id": "toolu_1" }),
            json!({ "type": "text", "text": "(continue)" }),
        ]
    );
    assert_eq!(messages[0].content[0]["text"], json!("original query"));
}

#[test]
fn ensure_placeholder_user_text_fills_empty_latest_user_turn() {
    for content in [json!([]), json!([{ "type": "text", "text": " \n" }])] {
        let mut messages = vec![
            ApiMessage {
                role: "user",
                content: json!("original query"),
            },
            ApiMessage {
                role: "user",
                content,
            },
        ];
        ensure_placeholder_user_text(&mut messages);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].content.as_array().unwrap().last(),
            Some(&json!({ "type": "text", "text": "(continue)" }))
        );
    }
}

#[test]
fn ensure_placeholder_user_text_preserves_latest_user_text() {
    for content in [
        json!("latest query"),
        json!([
            { "type": "tool_result", "tool_use_id": "toolu_1" },
            { "type": "text", "text": "latest query" },
        ]),
    ] {
        let mut messages = vec![ApiMessage {
            role: "user",
            content: content.clone(),
        }];
        ensure_placeholder_user_text(&mut messages);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, content);
    }
}

#[test]
fn ensure_placeholder_user_text_pushes_user_message_when_no_user_turn() {
    let mut messages = vec![ApiMessage {
        role: "assistant",
        content: json!([{ "type": "text", "text": "working" }]),
    }];
    ensure_placeholder_user_text(&mut messages);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, "user");
    assert!(!messages[1].content[0]["text"]
        .as_str()
        .unwrap()
        .trim()
        .is_empty());
}

#[test]
fn build_anthropic_messages_marks_latest_tool_result_as_rolling_cache_tail() {
    let conversation = vec![
        ConversationMessage::UserText("inspect".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "ExecCommand".to_string(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            content: "ok".to_string(),
            is_error: false,
            error: None,
        }]),
    ];

    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        true,
    );

    assert_eq!(
        messages[2].content[0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert!(messages[1].content[0].get("cache_control").is_none());
}

#[test]
fn claude_code_prompt_cache_marks_latest_tool_result_as_rolling_cache_tail() {
    let conversation = vec![
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "ExecCommand".to_string(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            content: "ok".to_string(),
            is_error: false,
            error: None,
        }]),
    ];

    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::ClaudeCodePromptCache,
            true,
        ),
        true,
    );

    assert_eq!(
        messages[1].content[0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
}

#[test]
fn rolling_marker_diagnostics_reports_tail_distance() {
    let conversation = vec![
        ConversationMessage::UserText("inspect".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "ExecCommand".to_string(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            content: "ok".to_string(),
            is_error: false,
            error: None,
        }]),
    ];

    assert_eq!(
        rolling_marker_diagnostics(&conversation, Some((0, 0))),
        (2, false)
    );
    assert_eq!(
        rolling_marker_diagnostics(&conversation, Some((2, 0))),
        (0, true)
    );
    assert_eq!(
        rolling_marker_diagnostics(&conversation, None),
        (conversation.len(), false)
    );
}

#[test]
fn claude_code_prompt_cache_keeps_turn_scoped_context_out_of_system_prefix() {
    let request = ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame {
            system_prompt: "unused plain system".to_string(),
            system_blocks: vec![
                PromptContentBlock {
                    text: "stable system".to_string(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: true,
                },
                PromptContentBlock {
                    text: "turn system".to_string(),
                    stability: PromptStability::TurnScoped,
                    cache_breakpoint: false,
                },
            ],
            context_blocks: vec![
                PromptContentBlock {
                    text: "agent context".to_string(),
                    stability: PromptStability::AgentScoped,
                    cache_breakpoint: true,
                },
                PromptContentBlock {
                    text: "current turn".to_string(),
                    stability: PromptStability::TurnScoped,
                    cache_breakpoint: false,
                },
            ],
            cache: None,
        },
        conversation: vec![ConversationMessage::UserBlocks(vec![
            PromptContentBlock {
                text: "agent context".to_string(),
                stability: PromptStability::AgentScoped,
                cache_breakpoint: true,
            },
            PromptContentBlock {
                text: "current turn".to_string(),
                stability: PromptStability::TurnScoped,
                cache_breakpoint: false,
            },
        ])],
        tools: vec![],
        native_web_search: None,
        response_format: None,
    };

    let system = build_anthropic_system(
        &request,
        AnthropicCacheStrategy::ClaudeCodePromptCache,
        true,
    );
    let system_text = system
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(system_text.contains("stable system"));
    assert!(system_text.contains("agent context"));
    assert!(!system_text.contains("current turn"));
    assert_eq!(
        system
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block.get("text").and_then(Value::as_str) == Some("turn system"))
            .and_then(|block| block.get("cache_control")),
        None
    );

    // The materialized context head is stripped entirely; TurnScoped
    // context is re-attached at the message tail instead of the head.
    let conversation = strip_initial_context_message(&request);
    assert!(matches!(
        conversation.as_slice(),
        [ConversationMessage::UserText(text)] if text == "Continue using the context above."
    ));
    assert_eq!(
        turn_scoped_context_blocks(&request)
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        vec!["current turn"]
    );
}

fn turn_scoped_tail_request() -> ProviderTurnRequest {
    let context_blocks = vec![
        PromptContentBlock {
            text: "agent context".to_string(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: true,
        },
        PromptContentBlock {
            text: "current turn".to_string(),
            stability: PromptStability::TurnScoped,
            cache_breakpoint: false,
        },
    ];
    ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame::structured(
            "stable system".to_string(),
            vec![PromptContentBlock {
                text: "stable system".to_string(),
                stability: PromptStability::Stable,
                cache_breakpoint: true,
            }],
            context_blocks.clone(),
            None,
        ),
        conversation: vec![
            ConversationMessage::UserBlocks(context_blocks),
            ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "ExecCommand".to_string(),
                input: json!({ "cmd": "printf ok" }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }]),
            ConversationMessage::UserToolResults(vec![ToolResultBlock {
                tool_use_id: "toolu_1".to_string(),
                content: "ok".to_string(),
                is_error: false,
                error: None,
            }]),
        ],
        tools: vec![],
        native_web_search: None,
        response_format: None,
    }
}

#[test]
fn shared_system_boundary_survives_both_cache_strategies_and_context_layouts() {
    let shared = "## identity\nfixed role\n\n## tool_contract\nfixed tools\n\n";
    for strategy in [
        AnthropicCacheStrategy::MessagesNative,
        AnthropicCacheStrategy::ClaudeCodePromptCache,
    ] {
        for enabled in [false, true] {
            for materialized in [false, true] {
                for agent in ["agent-a", "agent-b"] {
                    let mut request = turn_scoped_tail_request();
                    let suffix = format!("## workspace\n/{agent}/task");
                    request.prompt_frame.system_prompt = format!("{shared}{suffix}");
                    request.prompt_frame.system_blocks = vec![
                        PromptContentBlock {
                            text: shared.into(),
                            stability: PromptStability::Stable,
                            cache_breakpoint: true,
                        },
                        PromptContentBlock {
                            text: suffix.clone(),
                            stability: PromptStability::Stable,
                            cache_breakpoint: false,
                        },
                    ];
                    if !materialized {
                        request.conversation.remove(0);
                    }
                    let system = build_anthropic_system(&request, strategy, enabled);
                    let system_blocks = system.as_array().unwrap();
                    let offset = usize::from(
                        enabled && strategy == AnthropicCacheStrategy::ClaudeCodePromptCache,
                    );
                    assert_eq!(system_blocks[offset]["text"], shared);
                    assert_eq!(system_blocks[offset + 1]["text"], suffix);
                    assert_eq!(
                        system_blocks[offset].get("cache_control").is_some(),
                        enabled,
                    );
                    assert!(system_blocks[offset + 1].get("cache_control").is_none());
                    // No instance-dependent text precedes the shared content.
                    if offset == 1 {
                        assert_eq!(
                            system_blocks[0]["text"],
                            "x-anthropic-billing-header: holon"
                        );
                    }
                    let conversation = build_anthropic_wire_conversation(
                        &request,
                        can_mark_turn_scoped_context(&system, enabled),
                    );
                    let marker =
                        rolling_conversation_cache_marker(&conversation, strategy, enabled);
                    let messages = build_anthropic_messages(&conversation, marker, enabled);
                    let payload = json!({ "system": system, "messages": messages });
                    assert_eq!(
                        count_payload_cache_controls(&payload),
                        if enabled { (2, 2) } else { (0, 0) },
                    );
                    assert_eq!(payload.to_string().matches("current turn").count(), 1);
                }
            }
        }
    }
}

#[test]
fn inference_time_is_after_history_and_outside_cache_boundary() {
    for strategy in [
        AnthropicCacheStrategy::MessagesNative,
        AnthropicCacheStrategy::ClaudeCodePromptCache,
    ] {
        for cache_control in [false, true] {
            let mut request = turn_scoped_tail_request();
            request
                .conversation
                .push(ConversationMessage::UserBlocks(vec![PromptContentBlock {
                    text: "current_time: 2026-10-08T23:59:59+08:00\ntimezone: Asia/Shanghai".into(),
                    stability: PromptStability::TurnScoped,
                    cache_breakpoint: false,
                }]));
            let system = build_anthropic_system(&request, strategy, cache_control);
            let conversation = build_anthropic_wire_conversation(
                &request,
                can_mark_turn_scoped_context(&system, cache_control),
            );
            let marker = rolling_conversation_cache_marker(&conversation, strategy, cache_control);
            if let Some((message_index, _)) = marker {
                assert!(message_index < conversation.len() - 1);
            }
            let messages = build_anthropic_messages(&conversation, marker, cache_control);
            let time = &messages.last().unwrap().content[0];
            assert!(time["text"].as_str().unwrap().contains("23:59:59+08:00"));
            assert!(time.get("cache_control").is_none());
            assert!(!system.to_string().contains("current_time"));
            let payload = json!({ "system": system, "messages": messages });
            let (system_markers, message_markers) = count_payload_cache_controls(&payload);
            assert!(system_markers + message_markers <= 4);

            request
                .conversation
                .push(ConversationMessage::AssistantBlocks(vec![
                    ModelBlock::Text {
                        text: "continue".into(),
                    },
                ]));
            request
                .conversation
                .push(ConversationMessage::UserText("next input".into()));
            request
                .conversation
                .push(ConversationMessage::UserBlocks(vec![PromptContentBlock {
                    text: "current_time: 2026-10-09T00:00:01+08:00\ntimezone: Asia/Shanghai".into(),
                    stability: PromptStability::TurnScoped,
                    cache_breakpoint: false,
                }]));
            let next_system = build_anthropic_system(&request, strategy, cache_control);
            assert_eq!(next_system, system);
            let next = build_anthropic_wire_conversation(
                &request,
                can_mark_turn_scoped_context(&next_system, cache_control),
            );
            assert_eq!(
                serde_json::to_value(build_anthropic_messages(
                    &next[..conversation.len()],
                    None,
                    cache_control,
                ))
                .unwrap(),
                serde_json::to_value(build_anthropic_messages(&conversation, None, cache_control,))
                    .unwrap(),
            );
            let next_marker = rolling_conversation_cache_marker(&next, strategy, cache_control);
            if let Some((message_index, _)) = next_marker {
                assert!(message_index < next.len() - 1);
            }
            let next_messages = build_anthropic_messages(&next, next_marker, cache_control);
            assert!(next_messages.last().unwrap().content[0]
                .get("cache_control")
                .is_none());
        }
    }
}

#[test]
fn turn_scoped_context_precedes_history_for_both_strategies() {
    let request = turn_scoped_tail_request();
    for strategy in [
        AnthropicCacheStrategy::MessagesNative,
        AnthropicCacheStrategy::ClaudeCodePromptCache,
    ] {
        let wire_conversation = build_anthropic_wire_conversation(&request, true);
        let rolling_cache_marker =
            rolling_conversation_cache_marker(&wire_conversation, strategy, true);
        let messages = build_anthropic_messages(&wire_conversation, rolling_cache_marker, true);

        // The stripped history starts with an assistant round, so the
        // transport keeps a leading user placeholder.
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content[0]["text"], json!("current turn"));
        assert_eq!(
            messages[0].content[0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(
            messages[0].content[1]["text"],
            json!("Continue using the context above.")
        );
        // The rolling marker stays on the latest tool result.
        assert_eq!(
            messages[2].content[0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert_eq!(messages[2].content.as_array().unwrap().len(), 1);

        // Both strategies carry non-TurnScoped context in the system
        // prefix, never TurnScoped content.
        let system = build_anthropic_system(&request, strategy, true);
        let system_text = system
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(system_text.contains("agent context"));
        assert!(!system_text.contains("current turn"));
    }
}

#[test]
fn turn_scoped_prefix_is_reusable_across_provider_rounds() {
    for strategy in [
        AnthropicCacheStrategy::MessagesNative,
        AnthropicCacheStrategy::ClaudeCodePromptCache,
    ] {
        let mut request = turn_scoped_tail_request();
        for round in 0..4 {
            let conversation = build_anthropic_wire_conversation(&request, true);
            let marker = rolling_conversation_cache_marker(&conversation, strategy, true);
            let messages = build_anthropic_messages(&conversation, marker, true);
            let payload = json!({
                "system": build_anthropic_system(&request, strategy, true),
                "messages": messages,
            });
            assert_eq!(count_payload_cache_controls(&payload), (2, 2));
            assert_eq!(messages[0].content[0]["text"], "current turn");
            assert_eq!(messages[0].content[0]["cache_control"]["type"], "ephemeral");
            assert_eq!(
                payload["messages"]
                    .to_string()
                    .matches("current turn")
                    .count(),
                1
            );
            let (marker_message, marker_block) = marker.unwrap();
            assert_eq!(
                messages[marker_message].content[marker_block]["cache_control"]["type"],
                "ephemeral"
            );
            request
                .conversation
                .push(ConversationMessage::AssistantBlocks(vec![
                    ModelBlock::Text {
                        text: format!("round {round}"),
                    },
                ]));
            request
                .conversation
                .push(ConversationMessage::UserText(format!("continue {round}")));
            let next = build_anthropic_wire_conversation(&request, true);
            let next_messages = build_anthropic_messages(
                &next,
                rolling_conversation_cache_marker(&next, strategy, true),
                true,
            );
            assert_eq!(messages[0].content, next_messages[0].content);
            // The old history, except its moving tail marker, is a
            // prefix of the next request.
            for (old, new) in messages.iter().zip(&next_messages).take(messages.len() - 1) {
                assert_eq!(old.role, new.role);
                assert_eq!(old.content, new.content);
            }
        }

        request.prompt_frame.context_blocks[1].text = "next turn".into();
        if let ConversationMessage::UserBlocks(blocks) = &mut request.conversation[0] {
            blocks[1].text = "next turn".into();
        }
        let next = build_anthropic_wire_conversation(&request, true);
        let next_messages = build_anthropic_messages(
            &next,
            rolling_conversation_cache_marker(&next, strategy, true),
            true,
        );
        assert_eq!(next_messages[0].content[0]["text"], "next turn");
        assert!(!serde_json::to_string(&next_messages)
            .unwrap()
            .contains("current turn"));
    }
}

#[test]
fn turn_scoped_prefix_handles_unmaterialized_and_cache_disabled() {
    let mut request = turn_scoped_tail_request();
    request.conversation = vec![ConversationMessage::UserText("hello".into())];
    let uncached = build_anthropic_wire_conversation(&request, false);
    let messages = build_anthropic_messages(&uncached, None, false);
    assert_eq!(messages[0].content[0]["text"], "current turn");
    assert!(messages[0].content[0].get("cache_control").is_none());
    assert_eq!(messages[0].content[1]["text"], "hello");
    assert_eq!(messages[0].content.as_array().unwrap().len(), 2);

    request.conversation.clear();
    let empty = build_anthropic_wire_conversation(&request, true);
    let messages = build_anthropic_messages(&empty, None, true);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content[0]["text"], "current turn");
}

#[test]
fn turn_scoped_prefix_reserves_rolling_breakpoint_budget() {
    let request = turn_scoped_tail_request();
    let mut system = build_anthropic_system(&request, AnthropicCacheStrategy::MessagesNative, true);
    assert!(can_mark_turn_scoped_context(&system, true));
    system
        .as_array_mut()
        .unwrap()
        .push(cacheable_text_block("third system marker"));
    assert!(!can_mark_turn_scoped_context(&system, true));
    assert!(!can_mark_turn_scoped_context(&system, false));

    let conversation =
        build_anthropic_wire_conversation(&request, can_mark_turn_scoped_context(&system, true));
    let marker = rolling_conversation_cache_marker(
        &conversation,
        AnthropicCacheStrategy::MessagesNative,
        true,
    );
    let messages = build_anthropic_messages(&conversation, marker, true);
    assert!(messages[0].content[0].get("cache_control").is_none());
    let payload = json!({"system": system, "messages": messages});
    assert_eq!(count_payload_cache_controls(&payload), (3, 1));
}

#[test]
fn messages_native_system_includes_stable_context_without_materialization() {
    let mut request = turn_scoped_tail_request();
    request.prompt_frame.system_blocks.clear();
    request.conversation = vec![ConversationMessage::UserText("hi".to_string())];

    let system = build_anthropic_system(&request, AnthropicCacheStrategy::MessagesNative, true);
    let blocks = system.as_array().unwrap();
    assert_eq!(
        blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        vec!["stable system", "agent context"]
    );

    // An unmaterialized conversation no longer silently drops context on
    // the native path: the stable part rides the system prefix and the
    // TurnScoped part rides the message prefix.
    let conversation = build_anthropic_wire_conversation(&request, true);
    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        true,
    );
    assert_eq!(messages[0].content[0]["text"], json!("current turn"));
    assert_eq!(messages[0].content[1]["text"], json!("hi"));
}

#[test]
fn stable_only_context_keeps_placeholder_and_no_tail() {
    let mut request = turn_scoped_tail_request();
    request.prompt_frame.context_blocks.truncate(1);
    if let Some(ConversationMessage::UserBlocks(blocks)) = request.conversation.first_mut() {
        blocks.truncate(1);
    }

    assert!(turn_scoped_context_blocks(&request).is_empty());
    let wire_conversation = build_anthropic_wire_conversation(&request, true);
    assert!(matches!(
        wire_conversation.first(),
        Some(ConversationMessage::UserText(text)) if text == "Continue using the context above."
    ));
    assert!(wire_conversation.len() >= 3);
}

#[test]
fn api_response_block_preserves_text_form_dsml_tool_calls_as_text() {
    let block = ApiResponseBlock {
        kind: "text".to_string(),
        text: Some(
            r#"<｜｜DSML｜｜tool_calls>
<｜｜DSML｜｜invoke name="WebFetch">
<｜｜DSML｜｜parameter name="url" string="true">https://example.com</｜｜DSML｜｜parameter>
</｜｜DSML｜｜invoke>
</｜｜DSML｜｜tool_calls>"#
                .to_string(),
        ),
        thinking: None,
        signature: None,
        data: None,
        id: None,
        name: None,
        input: None,
        ..Default::default()
    };

    let model_block = api_response_block_to_model(block, None).expect("text block should be kept");
    match model_block {
        ModelBlock::Text { text } => {
            assert!(text.contains("<｜｜DSML｜｜tool_calls>"));
            assert!(text.contains("WebFetch"));
        }
        other => panic!("unexpected block: {other:?}"),
    }
}

#[test]
fn anthropic_response_flags_text_form_tool_call_violation() {
    let blocks = vec![ApiResponseBlock {
        kind: "text".to_string(),
        text: Some(
            r#"<tool_call>
<function=ExecCommand>
{"cmd":"echo ok"}
</function>
</tool_call>"#
                .to_string(),
        ),
        thinking: None,
        signature: None,
        data: None,
        id: None,
        name: None,
        input: None,
        ..Default::default()
    }];

    assert!(anthropic_response_has_text_form_tool_call_violation(
        &blocks,
        Some("tool_use"),
        true,
    ));
    assert!(!anthropic_response_has_text_form_tool_call_violation(
        &blocks,
        Some("end_turn"),
        true,
    ));
    assert!(!anthropic_response_has_text_form_tool_call_violation(
        &blocks,
        Some("tool_use"),
        false,
    ));
}

#[test]
fn anthropic_response_flags_html_escaped_text_form_tool_call_violation() {
    let blocks = vec![ApiResponseBlock {
        kind: "text".to_string(),
        text: Some(
            "&lt;tool_call&gt;&lt;function=ExecCommand&gt;{}&lt;/function&gt;&lt;/tool_call&gt;"
                .to_string(),
        ),
        thinking: None,
        signature: None,
        data: None,
        id: None,
        name: None,
        input: None,
        ..Default::default()
    }];

    assert!(anthropic_response_has_text_form_tool_call_violation(
        &blocks,
        Some("tool_use"),
        true,
    ));
}

#[test]
fn anthropic_response_native_tool_use_is_not_text_form_violation() {
    let blocks = vec![
        ApiResponseBlock {
            kind: "text".to_string(),
            text: Some("<tool_call>ignored text</tool_call>".to_string()),
            thinking: None,
            signature: None,
            data: None,
            id: None,
            name: None,
            input: None,
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "tool_use".to_string(),
            text: None,
            thinking: None,
            signature: None,
            data: None,
            id: Some("toolu_1".to_string()),
            name: Some("ExecCommand".to_string()),
            input: Some(json!({ "cmd": "echo ok" })),
            ..Default::default()
        },
    ];

    assert!(!anthropic_response_has_text_form_tool_call_violation(
        &blocks,
        Some("tool_use"),
        true,
    ));
}

#[test]
fn build_anthropic_messages_marks_latest_assistant_block_as_rolling_cache_tail() {
    let conversation = vec![
        ConversationMessage::UserText("hello".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
            text: "done".to_string(),
        }]),
    ];

    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        true,
    );

    assert_eq!(
        messages[1].content[0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert!(messages[0].content[0].get("cache_control").is_none());
}

#[test]
fn build_anthropic_messages_skips_empty_provider_visible_messages() {
    let conversation = vec![
        ConversationMessage::UserText("inspect".to_string()),
        ConversationMessage::AssistantBlocks(Vec::new()),
        ConversationMessage::UserText("continue".to_string()),
    ];

    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        true,
    );

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content[0]["text"], "inspect");
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content[0]["text"], "continue");
    assert!(messages.iter().all(|message| {
        message
            .content
            .as_array()
            .is_some_and(|content| !content.is_empty())
    }));
}

#[test]
fn build_anthropic_messages_lowers_user_image_content_blocks() {
    let conversation = vec![ConversationMessage::UserImage {
        prompt: "Describe the visible error dialog.".to_string(),
        media_type: "image/png".to_string(),
        data_base64: "iVBORw0KGgo=".to_string().into(),
        optional_history: false,
    }];

    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        true,
    );

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content[0]["type"], "text");
    assert_eq!(
        messages[0].content[0]["text"],
        "Describe the visible error dialog."
    );
    assert_eq!(
        messages[0].content[0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(messages[0].content[1]["type"], "image");
    assert_eq!(messages[0].content[1]["source"]["type"], "base64");
    assert_eq!(messages[0].content[1]["source"]["media_type"], "image/png");
    assert_eq!(messages[0].content[1]["source"]["data"], "iVBORw0KGgo=");
    assert!(messages[0].content[1].get("cache_control").is_none());
}

#[test]
fn build_anthropic_messages_preserves_tool_only_round_before_interjection() {
    let conversation = vec![
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "ExecCommand".to_string(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            content: "ok".to_string(),
            is_error: false,
            error: None,
        }]),
        ConversationMessage::UserText(
            "[Operator message received while this turn was in progress]\ncontinue".to_string(),
        ),
    ];

    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        true,
    );

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].role, "assistant");
    assert_eq!(messages[0].content[0]["type"], "tool_use");
    assert_eq!(messages[0].content[0]["id"], "toolu_1");
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content[0]["type"], "tool_result");
    assert_eq!(messages[1].content[0]["tool_use_id"], "toolu_1");
    assert_eq!(messages[2].role, "user");
    assert!(messages[2].content[0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("Operator message received")));
}

#[test]
fn build_anthropic_messages_synthesizes_tool_result_for_orphaned_tool_use() {
    let conversation = vec![
        ConversationMessage::UserText("start".to_string()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "toolu_orphan".to_string(),
            name: "ExecCommand".to_string(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserText("Continue using the context above.".to_string()),
    ];

    let messages = build_anthropic_messages(&conversation, None, false);

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].content[0]["type"], "tool_use");
    assert_eq!(messages[2].role, "user");
    assert_eq!(messages[2].content[0]["type"], "tool_result");
    assert_eq!(messages[2].content[0]["tool_use_id"], "toolu_orphan");
    assert_eq!(messages[2].content[0]["is_error"], true);
    assert_eq!(
        messages[2].content[0]["content"],
        crate::provider::INCOMPLETE_TOOL_RESULT_SENTINEL
    );
    assert_eq!(messages[2].content[1]["type"], "text");
}

#[test]
fn build_anthropic_messages_appends_tool_results_when_orphan_ends_conversation() {
    let conversation = vec![ConversationMessage::AssistantBlocks(vec![
        ModelBlock::ToolUse {
            id: "toolu_tail".to_string(),
            name: "ExecCommand".to_string(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        },
    ])];

    let messages = build_anthropic_messages(&conversation, None, false);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "assistant");
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content[0]["type"], "tool_result");
    assert_eq!(messages[1].content[0]["tool_use_id"], "toolu_tail");
    assert_eq!(messages[1].content[0]["is_error"], true);
}

#[test]
fn build_anthropic_messages_moves_rolling_cache_marker_to_repaired_tail() {
    let conversation = vec![
        ConversationMessage::UserText("start".to_string()),
        ConversationMessage::AssistantBlocks(vec![
            ModelBlock::Text {
                text: "checking".to_string(),
            },
            ModelBlock::ToolUse {
                id: "toolu_tail".to_string(),
                name: "ExecCommand".to_string(),
                input: json!({ "cmd": "printf ok" }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            },
        ]),
    ];

    for strategy in [
        AnthropicCacheStrategy::ClaudeCodePromptCache,
        AnthropicCacheStrategy::MessagesNative,
    ] {
        let messages = build_anthropic_messages(
            &conversation,
            rolling_conversation_cache_marker(&conversation, strategy, true),
            true,
        );

        assert_eq!(messages.len(), 3, "{strategy:?}");
        assert_eq!(messages[2].role, "user", "{strategy:?}");
        assert_eq!(
            messages[2].content[0]["type"], "tool_result",
            "{strategy:?}"
        );
        assert!(
            messages[1].content.as_array().is_some_and(|blocks| blocks
                .iter()
                .all(|block| block.get("cache_control").is_none())),
            "orphaned assistant must not keep the cache_control marker ({strategy:?})"
        );
        assert_eq!(
            messages[2].content[0]["cache_control"],
            json!({ "type": "ephemeral" }),
            "repaired tail must carry the transferred marker ({strategy:?})"
        );
        let marker_count = messages
            .iter()
            .filter_map(|message| message.content.as_array())
            .flatten()
            .filter(|block| block.get("cache_control").is_some())
            .count();
        assert_eq!(marker_count, 1, "{strategy:?}");
    }
}

#[test]
fn build_anthropic_messages_leaves_completed_tool_round_untouched() {
    let conversation = vec![
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "toolu_done".to_string(),
            name: "ExecCommand".to_string(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "toolu_done".to_string(),
            content: "ok".to_string(),
            is_error: false,
            error: None,
        }]),
    ];

    let messages = build_anthropic_messages(&conversation, None, false);

    assert_eq!(messages.len(), 2);
    let blocks = messages[1].content.as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["tool_use_id"], "toolu_done");
    assert_eq!(blocks[0]["is_error"], false);
}

#[test]
fn anthropic_response_missing_tool_call_blocks_detects_zero_block_tool_use_stop() {
    let text = ApiResponseBlock {
        kind: "text".to_string(),
        text: Some("done".to_string()),
        ..Default::default()
    };
    let thinking = ApiResponseBlock {
        kind: "thinking".to_string(),
        thinking: Some("checking".to_string()),
        ..Default::default()
    };
    let tool_use = ApiResponseBlock {
        kind: "tool_use".to_string(),
        id: Some("toolu_1".to_string()),
        ..Default::default()
    };
    let server_tool_use = ApiResponseBlock {
        kind: "server_tool_use".to_string(),
        ..Default::default()
    };

    assert!(anthropic_response_missing_tool_call_blocks(
        &[text.clone()],
        Some("tool_use"),
        true
    ));
    assert!(anthropic_response_missing_tool_call_blocks(
        &[thinking, text],
        Some("tool_use"),
        true
    ));
    assert!(!anthropic_response_missing_tool_call_blocks(
        &[tool_use],
        Some("tool_use"),
        true
    ));
    assert!(!anthropic_response_missing_tool_call_blocks(
        &[server_tool_use],
        Some("tool_use"),
        true
    ));
    assert!(!anthropic_response_missing_tool_call_blocks(
        &[ApiResponseBlock::default()],
        Some("tool_use"),
        false
    ));
    assert!(!anthropic_response_missing_tool_call_blocks(
        &[ApiResponseBlock::default()],
        Some("end_turn"),
        true
    ));
}

#[test]
fn rolling_cache_marker_does_not_mutate_provider_conversation() {
    let conversation = vec![ConversationMessage::UserBlocks(vec![PromptContentBlock {
        text: "reusable agent context".to_string(),
        stability: PromptStability::AgentScoped,
        cache_breakpoint: false,
    }])];

    let messages = build_anthropic_messages(
        &conversation,
        rolling_conversation_cache_marker(
            &conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        true,
    );

    assert_eq!(
        messages[0].content[0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    let ConversationMessage::UserBlocks(blocks) = &conversation[0] else {
        panic!("conversation should remain user blocks");
    };
    assert!(!blocks[0].cache_breakpoint);
}

#[test]
fn anthropic_request_payload_for_fingerprints_is_wire_object() {
    let request = ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame {
            system_prompt: "unused plain system".to_string(),
            system_blocks: vec![PromptContentBlock {
                text: "System instruction".to_string(),
                stability: PromptStability::Stable,
                cache_breakpoint: true,
            }],
            context_blocks: vec![],
            cache: None,
        },
        conversation: vec![ConversationMessage::UserText("Hello".to_string())],
        tools: vec![],
        native_web_search: None,
        response_format: None,
    };

    let request_payload = anthropic_request_payload_for_test(&request);

    assert!(request_payload.is_object());
    assert!(request_payload.get("system").is_some());
    assert!(request_payload.get("messages").is_some());
    let fingerprint =
        canonical_prefix_fingerprint(&request_payload, CacheBreakpointPath::System(0));
    assert!(!fingerprint.is_empty());
    assert_ne!(
        fingerprint,
        canonical_prefix_fingerprint(
            &Value::String("response body".to_string()),
            CacheBreakpointPath::System(0)
        )
    );
}

#[test]
fn test_collect_anthropic_cache_diagnostics_initial_request() {
    let request = ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame {
            system_prompt: "You are a helpful assistant.".to_string(),
            system_blocks: vec![
                PromptContentBlock {
                    text: "System instruction 1".to_string(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: true,
                },
                PromptContentBlock {
                    text: "System instruction 2".to_string(),
                    stability: PromptStability::AgentScoped,
                    cache_breakpoint: false,
                },
            ],
            context_blocks: vec![PromptContentBlock {
                text: "Context information".to_string(),
                stability: PromptStability::Stable,
                cache_breakpoint: false,
            }],
            cache: None,
        },
        conversation: vec![ConversationMessage::UserText(
            "Hello, how are you?".to_string(),
        )],
        tools: vec![],
        native_web_search: None,
        response_format: None,
    };

    let request_payload = anthropic_request_payload_for_test(&request);
    let diagnostics = collect_anthropic_cache_diagnostics(
        &request,
        &request.conversation,
        rolling_conversation_cache_marker(
            &request.conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        &request_payload,
        "claude-sonnet-4-6",
        AnthropicCacheStrategy::MessagesNative,
        &[],
    );

    assert_eq!(diagnostics.tools_count, 0);
    assert_eq!(diagnostics.system_block_count, 2);
    assert!(diagnostics.estimated_system_tokens > 0);
    assert_eq!(diagnostics.conversation_message_count, 1);
    assert!(!diagnostics.cache_breakpoints.is_empty());
    assert_eq!(diagnostics.cache_breakpoints.len(), 2);
    assert_eq!(
        diagnostics.cache_breakpoints[0].location,
        "system_blocks[0]"
    );
    assert_eq!(diagnostics.cache_breakpoints[0].stability, "stable");
    assert_eq!(
        diagnostics.cache_breakpoints[0].provider_payload_path,
        "system[0]"
    );
    assert_eq!(diagnostics.cache_breakpoints[0].block_kind, "system_text");
    assert!(!diagnostics.cache_breakpoints[0]
        .canonical_prefix_fingerprint
        .is_empty());
    assert_eq!(
        diagnostics.cache_breakpoints[1].location,
        "messages[0].content[0]"
    );
    assert_eq!(
        diagnostics.cache_breakpoints[1].stability,
        "conversation_tail"
    );
    assert!(diagnostics.tokens_before_last_breakpoint > 0);
    assert_eq!(diagnostics.tokens_after_last_breakpoint, 0);
    assert_eq!(diagnostics.rolling_marker_lag_messages, 0);
    assert!(diagnostics.rolling_marker_at_tail);
    let serialized = serde_json::to_value(&diagnostics).unwrap();
    assert_eq!(serialized["turn_scoped_context_tail_blocks"], 0);
    assert_eq!(serialized["turn_scoped_context_prefix_blocks"], 0);

    let mut previous_record = serialized;
    previous_record
        .as_object_mut()
        .unwrap()
        .remove("turn_scoped_context_prefix_blocks");
    previous_record["turn_scoped_context_tail_blocks"] = json!(2);
    let restored: AnthropicPromptCacheDiagnostics =
        serde_json::from_value(previous_record).unwrap();
    assert_eq!(restored.turn_scoped_context_tail_blocks, 2);
    assert_eq!(restored.turn_scoped_context_prefix_blocks, 0);
}

#[test]
fn test_collect_anthropic_cache_diagnostics_continuation_with_breakpoints() {
    let request = ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame {
            system_prompt: "You are a helpful assistant.".to_string(),
            system_blocks: vec![
                PromptContentBlock {
                    text: "System instruction 1".to_string(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: true,
                },
                PromptContentBlock {
                    text: "System instruction 2".to_string(),
                    stability: PromptStability::AgentScoped,
                    cache_breakpoint: true,
                },
            ],
            context_blocks: vec![PromptContentBlock {
                text: "Context information".to_string(),
                stability: PromptStability::Stable,
                cache_breakpoint: false,
            }],
            cache: None,
        },
        conversation: vec![
            ConversationMessage::UserBlocks(vec![PromptContentBlock {
                text: "User message content".to_string(),
                stability: PromptStability::TurnScoped,
                cache_breakpoint: true,
            }]),
            ConversationMessage::AssistantBlocks(vec![ModelBlock::Text {
                text: "I'm doing well, thank you!".to_string(),
            }]),
        ],
        tools: vec![],
        native_web_search: None,
        response_format: None,
    };

    let request_payload = anthropic_request_payload_for_test(&request);
    let diagnostics = collect_anthropic_cache_diagnostics(
        &request,
        &request.conversation,
        rolling_conversation_cache_marker(
            &request.conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        &request_payload,
        "claude-sonnet-4-6",
        AnthropicCacheStrategy::MessagesNative,
        &[],
    );

    assert_eq!(diagnostics.system_block_count, 2);
    assert!(diagnostics.cache_breakpoints.len() <= 10);
    assert!(diagnostics.cache_breakpoints.len() > 0);

    // Check that breakpoints include system blocks
    let system_breakpoints: Vec<_> = diagnostics
        .cache_breakpoints
        .iter()
        .filter(|bp| bp.location.starts_with("system_blocks"))
        .collect();
    assert_eq!(system_breakpoints.len(), 2);

    // Check token distribution
    assert!(diagnostics.tokens_before_last_breakpoint > 0);
    assert!(diagnostics.tokens_after_last_breakpoint < u64::MAX);
    assert_eq!(diagnostics.rolling_marker_lag_messages, 0);
    assert!(diagnostics.rolling_marker_at_tail);
}

fn anthropic_request_payload_for_test(request: &ProviderTurnRequest) -> Value {
    let rolling_cache_marker = rolling_conversation_cache_marker(
        &request.conversation,
        AnthropicCacheStrategy::MessagesNative,
        true,
    );
    let body = MessagesRequest {
        model: "claude-sonnet-4-6",
        max_tokens: 4096,
        thinking: None,
        output_config: None,
        stream: true,
        system: if request.prompt_frame.has_structured_system_blocks() {
            Value::Array(
                request
                    .prompt_frame
                    .system_blocks
                    .iter()
                    .map(|block| prompt_block_to_anthropic_content(block, true))
                    .collect(),
            )
        } else {
            Value::String(request.prompt_frame.system_prompt.clone())
        },
        messages: build_anthropic_messages(&request.conversation, rolling_cache_marker, true),
        tools: build_anthropic_tools(request),
        tool_choice: anthropic_response_format_tool_choice(request),
        betas: Vec::new(),
        metadata: None,
        temperature: None,
        context_management: None,
    };
    serde_json::to_value(body).expect("test request payload should serialize")
}

#[test]
fn anthropic_request_payload_lowers_native_web_search_tool() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("search the web".into())],
        vec![],
    );
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::Anthropic,
        provider_id: "anthropic_native".into(),
        provider_model_ref: "anthropic/claude-test".into(),
        advertised_tool_type: "web_search_20250305".into(),
        backend_kind: "anthropic_web_search".into(),
        max_results: None,
    });

    let payload = anthropic_request_payload_for_test(&request);

    assert!(payload["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .any(|tool| tool == &json!({ "type": "web_search_20250305", "name": "web_search" })));
}

#[test]
fn deepseek_anthropic_request_payload_lowers_native_web_search_tool() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("search the web".into())],
        vec![],
    );
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::DeepSeek,
        provider_id: "deepseek".into(),
        provider_model_ref: "deepseek@default/deepseek-v4-pro".into(),
        advertised_tool_type: "web_search_20250305".into(),
        backend_kind: "deepseek_web_search".into(),
        max_results: None,
    });

    let payload = anthropic_request_payload_for_test(&request);

    assert!(payload["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .any(|tool| tool == &json!({ "type": "web_search_20250305", "name": "web_search" })));
}

#[test]
fn anthropic_request_payload_enables_streaming() {
    let request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("hello".into())],
        vec![],
    );

    let payload = anthropic_request_payload_for_test(&request);

    assert_eq!(payload["stream"], true);
}

#[test]
fn anthropic_sse_parser_accumulates_text_usage_and_tool_json() {
    let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":3}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hel\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"ExecCommand\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"echo ok\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
    let events = SseParser::default()
        .push(stream.as_bytes())
        .expect("stream should parse");
    let mut accumulator = AnthropicStreamAccumulator::default();
    for event in events {
        accumulator
            .apply(event, "anthropic/claude-test", "https://example.test", None)
            .expect("event should accumulate");
    }

    let response = accumulator
        .finish(
            "anthropic",
            "anthropic/claude-test",
            "https://example.test",
            None,
        )
        .expect("stream should finish");

    assert_eq!(response.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        response.usage.as_ref().and_then(|usage| usage.input_tokens),
        Some(3)
    );
    assert_eq!(
        response
            .usage
            .as_ref()
            .and_then(|usage| usage.output_tokens),
        Some(7)
    );
    assert_eq!(response.content[0].text.as_deref(), Some("hello"));
    assert_eq!(response.content[1].kind, "tool_use");
    assert_eq!(
        response.content[1].input.as_ref(),
        Some(&json!({ "cmd": "echo ok" }))
    );
}

#[test]
fn anthropic_sse_parser_stops_at_message_stop_independent_of_chunk_boundaries() {
    let terminal = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"content\":[]}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    for tail in [
            b"data: {not valid json}\n\n".as_slice(),
            b"data: {\"type\":",
            b"data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"late\"}}\n\n",
        ] {
            let mut body = terminal.as_bytes().to_vec();
            body.extend_from_slice(tail);
            for chunk_size in [body.len(), 1, 7] {
                let mut parser = SseParser::default();
                let mut kinds = Vec::new();
                for chunk in body.chunks(chunk_size) {
                    let events = parser.push(chunk).expect("do not parse beyond message_stop");
                    kinds.extend(events.into_iter().map(|event| event.kind));
                    if kinds.last().map(String::as_str) == Some("message_stop") {
                        break;
                    }
                }
                assert_eq!(kinds, ["message_start", "message_stop"]);
                assert!(parser.push(tail).expect("ignore later chunks").is_empty());
                assert!(parser.finish().expect("ignore trailing frame").is_empty());
            }
        }
}

#[test]
fn anthropic_empty_completed_stream_is_retryable_and_preserves_usage() {
    let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":3}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":893}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
    let events = SseParser::default()
        .push(stream.as_bytes())
        .expect("stream should parse");
    let mut accumulator = AnthropicStreamAccumulator::default();
    for event in events {
        accumulator
            .apply(event, "anthropic/claude-test", "https://example.test", None)
            .expect("event should accumulate");
    }
    let parsed = accumulator
        .finish(
            "anthropic",
            "anthropic/claude-test",
            "https://example.test",
            None,
        )
        .expect("stream should finish");
    let request = ProviderTurnRequest::plain("system", Vec::new(), Vec::new());
    let error = anthropic_messages_response_to_turn_response(
        parsed,
        None,
        &request,
        &[],
        None,
        &json!({}),
        "anthropic",
        "claude-test",
        AnthropicCacheStrategy::MessagesNative,
        &[],
        "anthropic/claude-test",
        "https://example.test",
        None,
    )
    .expect_err("empty completed stream should fail");

    let classification = crate::provider::retry::classify_provider_error(&error);
    assert_eq!(classification.kind, ProviderFailureKind::EmptyResponse);
    assert_eq!(classification.disposition, RetryDisposition::Retryable);
    assert_eq!(
        crate::provider::provider_error_token_usage(&error)
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((3, 893))
    );
}

#[test]
fn anthropic_nonempty_unsupported_block_remains_fail_fast() {
    let parsed = MessagesResponse {
        content: vec![ApiResponseBlock {
            kind: "unsupported_provider_block".into(),
            ..Default::default()
        }],
        usage: Some(ApiUsage {
            input_tokens: Some(3),
            output_tokens: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let request = ProviderTurnRequest::plain("system", Vec::new(), Vec::new());
    let error = anthropic_messages_response_to_turn_response(
        parsed,
        None,
        &request,
        &[],
        None,
        &json!({}),
        "anthropic",
        "claude-test",
        AnthropicCacheStrategy::MessagesNative,
        &[],
        "anthropic/claude-test",
        "https://example.test",
        None,
    )
    .expect_err("unsupported block should fail");

    let classification = crate::provider::retry::classify_provider_error(&error);
    assert_eq!(classification.kind, ProviderFailureKind::InvalidResponse);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
}

#[test]
fn normalize_anthropic_session_id_preserves_safe_range() {
    assert_eq!(
        normalize_anthropic_session_id(Some("cache-key_123")),
        "cache-key_123"
    );
}

#[test]
fn normalize_anthropic_session_id_uses_default_for_empty_key() {
    assert_eq!(normalize_anthropic_session_id(None), "holon-default");
    assert_eq!(
        normalize_anthropic_session_id(Some(" \n\t ")),
        "holon-default"
    );
}

#[test]
fn normalize_anthropic_session_id_handles_short_long_unicode_and_escaped_keys() {
    let short = normalize_anthropic_session_id(Some("abc"));
    assert_ne!(short, "abc");
    assert_normalized_anthropic_session_id(&short);
    assert!(short.starts_with("abc-"));

    let long_key = "safe-key-".repeat(12);
    let long = normalize_anthropic_session_id(Some(&long_key));
    assert_ne!(long, long_key);
    assert_normalized_anthropic_session_id(&long);
    assert!(long.starts_with("safe-key-safe-key"));

    let unicode = normalize_anthropic_session_id(Some("会话-缓存-キー"));
    assert_normalized_anthropic_session_id(&unicode);
    assert!(unicode.starts_with("holon-"));

    let escaped = normalize_anthropic_session_id(Some("cache\"key\\with\nnewline"));
    assert_normalized_anthropic_session_id(&escaped);
    assert!(escaped.starts_with("cache-key-with-newline-"));
    assert_eq!(
        escaped,
        normalize_anthropic_session_id(Some("cache\"key\\with\nnewline"))
    );
}

fn assert_normalized_anthropic_session_id(value: &str) {
    assert!(
        value.len() >= ANTHROPIC_SESSION_ID_MIN_LEN && value.len() <= ANTHROPIC_SESSION_ID_MAX_LEN,
        "session id length should be in provider-compatible range"
    );
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
        "session id should be ASCII metadata-safe"
    );
}

#[test]
fn test_hash_tools_includes_schema() {
    use holon_domain::ToolSpec;

    let tool1 = ToolSpec {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
        freeform_grammar: None,
    };

    let tool2 = ToolSpec {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: json!({"type": "object", "properties": {"new_field": {"type": "string"}}}),
        freeform_grammar: None,
    };

    let hash1 = hash_tools(&[tool1]);
    let hash2 = hash_tools(&[tool2]);

    // Different input_schema should produce different hashes
    assert_ne!(
        hash1, hash2,
        "tools_hash should differ when input_schema changes"
    );
}

#[test]
fn test_hash_tools_includes_freeform_grammar() {
    use holon_domain::{ToolFreeformGrammar, ToolSpec};

    let tool1 = ToolSpec {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
        freeform_grammar: Some(ToolFreeformGrammar {
            syntax: "grammar_v1".to_string(),
            definition: "definition_v1".to_string(),
        }),
    };

    let tool2 = ToolSpec {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
        freeform_grammar: Some(ToolFreeformGrammar {
            syntax: "grammar_v2".to_string(),
            definition: "definition_v2".to_string(),
        }),
    };

    let tool3 = ToolSpec {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
        freeform_grammar: None,
    };

    let hash1 = hash_tools(&[tool1.clone()]);
    let hash2 = hash_tools(&[tool2]);
    let hash3 = hash_tools(&[tool3]);

    // Different freeform_grammar syntax should produce different hashes
    assert_ne!(
        hash1, hash2,
        "tools_hash should differ when freeform_grammar syntax changes"
    );

    // Tool with freeform_grammar should differ from tool without
    assert_ne!(
        hash1, hash3,
        "tools_hash should differ when freeform_grammar presence changes"
    );
}

#[test]
fn test_hash_context_by_stability() {
    let blocks = vec![
        PromptContentBlock {
            text: "Stable content".to_string(),
            stability: PromptStability::Stable,
            cache_breakpoint: false,
        },
        PromptContentBlock {
            text: "Agent scoped content".to_string(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: false,
        },
        PromptContentBlock {
            text: "More stable content".to_string(),
            stability: PromptStability::Stable,
            cache_breakpoint: false,
        },
    ];

    let hashes = hash_context_by_stability(&blocks);

    assert!(hashes.contains_key("stable"));
    assert!(hashes.contains_key("agent_scoped"));
    assert!(!hashes.contains_key("turn_scoped"));
}

#[test]
fn test_estimate_token_distribution() {
    let request = ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame {
            system_prompt: "System prompt".to_string(),
            system_blocks: vec![
                PromptContentBlock {
                    text: "Block 1".to_string(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: true,
                },
                PromptContentBlock {
                    text: "Block 2".to_string(),
                    stability: PromptStability::AgentScoped,
                    cache_breakpoint: false,
                },
            ],
            context_blocks: vec![],
            cache: None,
        },
        conversation: vec![ConversationMessage::UserText("User message".to_string())],
        tools: vec![],
        native_web_search: None,
        response_format: None,
    };

    let request_payload = anthropic_request_payload_for_test(&request);
    let breakpoints = collect_cache_breakpoints(
        &request,
        &request.conversation,
        rolling_conversation_cache_marker(
            &request.conversation,
            AnthropicCacheStrategy::MessagesNative,
            true,
        ),
        &request_payload,
        AnthropicCacheStrategy::MessagesNative,
    );
    let (before, after) = estimate_token_distribution_from_payload(&request_payload, &breakpoints);

    let total = estimate_total_payload_tokens(&request_payload);
    assert_eq!(
        before.saturating_add(after),
        total,
        "before + after should equal total"
    );
}

#[test]
fn test_cache_breakpoints_bounded() {
    let mut system_blocks = vec![];
    for i in 0..20 {
        system_blocks.push(PromptContentBlock {
            text: format!("System block {}", i),
            stability: PromptStability::Stable,
            cache_breakpoint: true,
        });
    }

    let request = ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame {
            system_prompt: "System prompt".to_string(),
            system_blocks,
            context_blocks: vec![],
            cache: None,
        },
        conversation: vec![],
        tools: vec![],
        native_web_search: None,
        response_format: None,
    };

    let request_payload = anthropic_request_payload_for_test(&request);
    let breakpoints = collect_cache_breakpoints(
        &request,
        &request.conversation,
        None,
        &request_payload,
        AnthropicCacheStrategy::MessagesNative,
    );

    // Should be bounded by MAX_BREAKPOINTS (10)
    assert!(
        breakpoints.len() <= 10,
        "cache_breakpoints should be bounded to 10 items"
    );
}

#[test]
fn anthropic_request_payload_lowers_json_schema_response_format_as_forced_tool() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("return json".into())],
        vec![],
    );
    request.response_format = Some(ProviderResponseFormatRequest::JsonSchema(
        crate::provider::ProviderJsonSchemaResponseFormat {
            name: "answer_v1".into(),
            strict: true,
            schema: json!({
                "type": "object",
                "required": ["answer"],
                "properties": {
                    "answer": { "type": "string" }
                }
            }),
        },
    ));

    let payload = anthropic_request_payload_for_test(&request);

    assert_eq!(
        payload["tools"][0]["name"],
        json!("structured_response_answer_v1")
    );
    assert_eq!(payload["tools"][0]["strict"], json!(true));
    assert_eq!(
        payload["tools"][0]["input_schema"]["properties"]["answer"]["type"],
        json!("string")
    );
    assert_eq!(
        payload["tool_choice"],
        json!({ "type": "tool", "name": "structured_response_answer_v1" })
    );

    let diagnostics = response_format_diagnostics(&request)
        .expect("response format diagnostics should be recorded");

    assert!(diagnostics.requested);
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.format_type, "json_schema");
    assert_eq!(diagnostics.schema_name.as_deref(), Some("answer_v1"));
    assert!(diagnostics.fallback_reason.is_none());
}

#[test]
fn api_response_block_converts_forced_response_format_tool_input_to_text_json() {
    let block = ApiResponseBlock {
        kind: "tool_use".to_string(),
        text: None,
        thinking: None,
        signature: None,
        data: None,
        id: Some("toolu_1".to_string()),
        name: Some("structured_response_answer_v1".to_string()),
        input: Some(json!({ "answer": "ok" })),
        ..Default::default()
    };

    let model_block = api_response_block_to_model(block, Some("structured_response_answer_v1"))
        .expect("forced response format tool input should become text");

    match model_block {
        ModelBlock::Text { text } => assert_eq!(text, r#"{"answer":"ok"}"#),
        other => panic!("unexpected block: {other:?}"),
    }
}

#[test]
fn reasoning_effort_to_thinking_none_when_absent() {
    assert!(reasoning_effort_to_thinking(None, 16384).is_none());
}

#[test]
fn reasoning_effort_to_thinking_maps_levels() {
    // low = 10% of 16384 = ~1638, clamped to min 1024
    let low = reasoning_effort_to_thinking(Some("low"), 16384).unwrap();
    assert_eq!(low.kind, "enabled");
    assert_eq!(low.budget_tokens, 1638);

    // medium = 25% of 16384 = ~4096
    let medium = reasoning_effort_to_thinking(Some("medium"), 16384).unwrap();
    assert_eq!(medium.budget_tokens, 4096);

    // high = 50% of 16384 = ~8192
    let high = reasoning_effort_to_thinking(Some("high"), 16384).unwrap();
    assert_eq!(high.budget_tokens, 8192);

    // xhigh = 80% of 16384 = ~13107
    let xhigh = reasoning_effort_to_thinking(Some("xhigh"), 16384).unwrap();
    assert_eq!(xhigh.budget_tokens, 13107);
}

#[test]
fn reasoning_effort_to_thinking_case_insensitive() {
    let result = reasoning_effort_to_thinking(Some("HIGH"), 16384).unwrap();
    assert_eq!(result.budget_tokens, 8192);
}

#[test]
fn reasoning_effort_to_thinking_unknown_defaults_medium() {
    let result = reasoning_effort_to_thinking(Some("bogus"), 16384).unwrap();
    assert_eq!(result.budget_tokens, 4096); // 25% = medium
}

#[test]
fn reasoning_effort_to_thinking_clamps_to_min_1024() {
    // 10% of 4096 = ~410, should be clamped to 1024
    let result = reasoning_effort_to_thinking(Some("low"), 4096).unwrap();
    assert_eq!(result.budget_tokens, 1024);
}

#[test]
fn reasoning_effort_to_thinking_capped_below_max_tokens() {
    // budget must be <= max_output_tokens - 1
    let result = reasoning_effort_to_thinking(Some("xhigh"), 2048).unwrap();
    assert!(result.budget_tokens <= 2047);
    assert_eq!(result.budget_tokens, 1638); // 80% of 2048
}

#[test]
fn reasoning_effort_to_thinking_skips_when_too_small() {
    // max_output_tokens = 1024 → max_tokens - 1 = 1023 < 1024 min → skip
    assert!(reasoning_effort_to_thinking(Some("high"), 1024).is_none());
}

#[test]
fn fable_52_rejects_forced_response_format_without_affecting_other_routes() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("return json".into())],
        vec![],
    );
    assert!(anthropic_response_format_tool_choice_for_model(
        &request,
        "anthropic",
        "claude-fable-5-2",
    )
    .unwrap()
    .is_none());
    request.response_format = Some(ProviderResponseFormatRequest::JsonSchema(
        crate::provider::ProviderJsonSchemaResponseFormat {
            name: "answer_v1".into(),
            strict: true,
            schema: json!({"type": "object", "properties": {"answer": {"type": "string"}}}),
        },
    ));
    let error =
        anthropic_response_format_tool_choice_for_model(&request, "anthropic", "claude-fable-5-2")
            .unwrap_err();
    assert!(error.to_string().contains("does not support forced-tool"));
    for (provider, model) in [
        ("opencode-go", "claude-fable-5-2"),
        ("anthropic", "claude-fable-5"),
        ("anthropic", "claude-fable-5-2-other"),
    ] {
        assert_eq!(
            anthropic_response_format_tool_choice_for_model(&request, provider, model).unwrap(),
            anthropic_response_format_tool_choice(&request),
        );
    }
}

#[test]
fn fable_52_prompt_cache_omits_unsupported_temperature() {
    assert_eq!(
        anthropic_cache_temperature(
            "anthropic",
            "claude-fable-5-2",
            AnthropicCacheStrategy::ClaudeCodePromptCache,
            true,
        ),
        None
    );
    for (provider, model) in [
        ("opencode-go", "claude-fable-5-2"),
        ("anthropic", "claude-fable-5"),
        ("anthropic", "claude-fable-5-2-other"),
    ] {
        assert_eq!(
            anthropic_cache_temperature(
                provider,
                model,
                AnthropicCacheStrategy::ClaudeCodePromptCache,
                true,
            ),
            Some(1.0)
        );
        assert_eq!(
            anthropic_cache_temperature(
                provider,
                model,
                AnthropicCacheStrategy::ClaudeCodePromptCache,
                false,
            ),
            None
        );
    }
}

#[test]
fn fable_52_reasoning_uses_exact_effort_without_manual_thinking_budget() {
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        let (thinking, output_config) = anthropic_reasoning_controls(
            "anthropic",
            "claude-fable-5-2",
            true,
            Some(effort),
            16_384,
        )
        .unwrap();
        assert!(thinking.is_none());
        assert_eq!(
            serde_json::to_value(output_config).unwrap(),
            json!({"effort": effort})
        );
    }
    let (thinking, output_config) =
        anthropic_reasoning_controls("anthropic", "claude-fable-5-2", true, None, 16_384).unwrap();
    assert!(thinking.is_none());
    assert!(output_config.is_none());
    for effort in ["none", "off", "disabled", "minimal", "unknown"] {
        assert!(anthropic_reasoning_controls(
            "anthropic",
            "claude-fable-5-2",
            true,
            Some(effort),
            16_384,
        )
        .is_err());
    }
    for (provider, model) in [
        ("opencode-go", "claude-fable-5-2"),
        ("anthropic", "claude-fable-5"),
        ("anthropic", "claude-fable-5-2-other"),
    ] {
        let (thinking, output_config) =
            anthropic_reasoning_controls(provider, model, true, Some("high"), 16_384).unwrap();
        assert!(thinking.is_some());
        assert!(output_config.is_none());
    }
}

#[test]
fn deepseek_anthropic_reasoning_uses_output_config_effort_only() {
    let (thinking, output_config) =
        anthropic_reasoning_controls("deepseek", "deepseek-v4-pro", true, Some("high"), 16384)
            .unwrap();
    assert!(thinking.is_none());
    assert_eq!(
        serde_json::to_value(output_config).unwrap(),
        json!({ "effort": "high" })
    );

    let (thinking, output_config) =
        anthropic_reasoning_controls("deepseek", "deepseek-v4-pro", true, None, 16384).unwrap();
    assert!(thinking.is_none());
    assert!(output_config.is_none());

    let (thinking, output_config) =
        anthropic_reasoning_controls("anthropic", "claude-sonnet-4-6", true, Some("high"), 16384)
            .unwrap();
    assert_eq!(thinking.unwrap().budget_tokens, 8192);
    assert!(output_config.is_none());
}

#[test]
fn dashscope_qwen4_reasoning_uses_exact_effort_without_manual_budget() {
    for (model, effort, expected) in [
        ("qwen4-max", "low", "low"),
        ("qwen4-max", "medium", "high"),
        ("qwen4-max", "high", "high"),
        ("qwen4-max", "xhigh", "xhigh"),
        ("qwen4-max", "max", "xhigh"),
        ("qwen4-max-preview", "low", "low"),
        ("qwen4-max-preview", "medium", "medium"),
        ("qwen4-max-preview", "high", "xhigh"),
        ("qwen4-max-preview", "xhigh", "xhigh"),
        ("qwen4-max-preview", "max", "xhigh"),
    ] {
        let (thinking, output_config) =
            anthropic_reasoning_controls("dashscope", model, true, Some(effort), 8192).unwrap();
        assert!(thinking.is_none(), "{model}/{effort}");
        assert_eq!(
            serde_json::to_value(output_config).unwrap(),
            json!({"effort": expected}),
            "{model}/{effort}"
        );
    }
    for model in ["qwen4-max", "qwen4-max-preview"] {
        let (thinking, output_config) =
            anthropic_reasoning_controls("dashscope", model, true, None, 8192).unwrap();
        assert!(thinking.is_none());
        assert!(output_config.is_none());
        for effort in ["none", "minimal", "no_think", "unknown"] {
            assert!(
                anthropic_reasoning_controls("dashscope", model, true, Some(effort), 8192).is_err(),
                "{model}/{effort}"
            );
        }
    }
    for (provider, model) in [
        ("dashscope", "qwen3.8-max"),
        ("dashscope", "qwen4-max-unknown"),
        ("dashscope", "qwen4-max-preview-unknown"),
        ("anthropic", "qwen4-max"),
        ("opencode-go", "qwen4-max"),
    ] {
        let (thinking, output_config) =
            anthropic_reasoning_controls(provider, model, true, Some("high"), 8192).unwrap();
        assert!(thinking.is_some(), "{provider}/{model}");
        assert!(output_config.is_none());
    }
}

#[test]
fn dashscope_k31_reasoning_sends_model_specific_output_config_effort() {
    for effort in ["low", "medium", "high", "max"] {
        let (thinking, output_config) =
            anthropic_reasoning_controls("dashscope", "kimi-k3.1-pro", true, Some(effort), 16_384)
                .unwrap();
        assert!(thinking.is_none());
        assert_eq!(
            serde_json::to_value(output_config).unwrap(),
            json!({"effort": effort})
        );
    }
    let (thinking, output_config) =
        anthropic_reasoning_controls("dashscope", "kimi-k3.1-pro", true, None, 16_384).unwrap();
    assert!(thinking.is_none());
    assert!(output_config.is_none());
    for effort in ["none", "no_think", "xhigh", "unknown"] {
        assert!(
            anthropic_reasoning_controls("dashscope", "kimi-k3.1-pro", true, Some(effort), 16_384,)
                .is_err(),
            "{effort}"
        );
    }
    for (provider, model) in [
        ("dashscope", "kimi-k3"),
        ("dashscope", "kimi-k3.1-pro-unknown"),
        ("moonshot", "kimi-k3.1-pro"),
        ("tencent-tokenhub", "kimi-k3.1-pro"),
    ] {
        let (thinking, output_config) =
            anthropic_reasoning_controls(provider, model, true, Some("high"), 16_384).unwrap();
        assert!(thinking.is_some());
        assert!(output_config.is_none());
    }
}

#[test]
fn minimax_m31_reasoning_sends_exact_effort_without_budget_tokens() {
    for provider in ["minimax", "minimax-cn"] {
        for model in ["MiniMax-M3.1-Flash-Preview", "MiniMax-M3.1-Flash"] {
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                let (thinking, output_config) =
                    anthropic_reasoning_controls(provider, model, true, Some(effort), 1024)
                        .unwrap();
                assert!(thinking.is_none());
                assert_eq!(
                    serde_json::to_value(output_config).unwrap(),
                    json!({ "effort": effort })
                );
            }
            let (thinking, output_config) =
                anthropic_reasoning_controls(provider, model, true, None, 16384).unwrap();
            assert!(thinking.is_none());
            assert!(output_config.is_none());
            for effort in ["none", "off", "disabled", "minimal", "unknown"] {
                assert!(
                    anthropic_reasoning_controls(provider, model, true, Some(effort), 16384,)
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn minimax_m31_reasoning_controls_are_provider_and_exact_model_scoped() {
    for provider in ["minimax", "minimax-cn"] {
        for model in ["MiniMax-M3", "MiniMax-M2.7", "MiniMax-M3.1-Flash-other"] {
            let (thinking, output_config) =
                anthropic_reasoning_controls(provider, model, true, Some("high"), 16384).unwrap();
            assert_eq!(thinking.unwrap().budget_tokens, 8192);
            assert!(output_config.is_none());
        }
    }
    for provider in ["anthropic", "zai", "minimax-proxy"] {
        for model in ["MiniMax-M3.1-Flash-Preview", "MiniMax-M3.1-Flash"] {
            let (thinking, output_config) =
                anthropic_reasoning_controls(provider, model, true, Some("high"), 16384).unwrap();
            assert_eq!(thinking.unwrap().budget_tokens, 8192);
            assert!(output_config.is_none());
        }
    }
}

#[test]
fn api_response_block_provider_tool_result_preserves_structured_content() {
    let block = ApiResponseBlock {
        kind: "web_search_tool_result".to_string(),
        tool_use_id: Some("srv_001".to_string()),
        content: Some(json!([{
            "type": "web_search_result",
            "url": "https://example.com/rust",
            "title": "Rust"
        }])),
        extra: [("provider_extension".to_string(), json!("kept"))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let model = api_response_block_to_model(block, None);
    match model {
        Some(ModelBlock::ProviderToolResult {
            tool_use_id,
            content,
            provider_data,
            ..
        }) => {
            assert_eq!(tool_use_id.as_deref(), Some("srv_001"));
            assert_eq!(content[0]["type"], json!("web_search_result"));
            let provider_data = provider_data.expect("raw provider block should be preserved");
            assert_eq!(provider_data.format, ANTHROPIC_PROVIDER_BLOCK_FORMAT);
            assert_eq!(
                provider_data.payload["type"],
                json!("web_search_tool_result")
            );
            assert_eq!(provider_data.payload["provider_extension"], json!("kept"));
        }
        other => panic!("unexpected block: {other:?}"),
    }
}

#[test]
fn api_response_block_server_tool_use_preserves_structured_input() {
    let block = ApiResponseBlock {
        kind: "server_tool_use".to_string(),
        id: Some("srv_search_1".to_string()),
        name: Some("web_search".to_string()),
        input: Some(json!({"query": "rust async"})),
        extra: [("provider_extension".to_string(), json!({"version": 2}))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let model = api_response_block_to_model(block, None);
    match model {
        Some(ModelBlock::ProviderToolUse {
            id,
            name,
            input,
            provider_data,
            ..
        }) => {
            assert_eq!(id, "srv_search_1");
            assert_eq!(name, "web_search");
            assert_eq!(input, json!({"query": "rust async"}));
            assert_eq!(
                provider_data
                    .expect("raw provider block should be preserved")
                    .payload["provider_extension"]["version"],
                json!(2)
            );
        }
        other => panic!("unexpected block: {other:?}"),
    }
}

#[test]
fn provider_tool_blocks_round_trip_through_anthropic_assistant_history() {
    let raw_use = json!({
        "type": "server_tool_use",
        "id": "srv_search_1",
        "name": "web_search",
        "input": {"query": "rust async"},
        "provider_extension": {"version": 2}
    });
    let raw_result = json!({
        "type": "web_search_tool_result",
        "tool_use_id": "srv_search_1",
        "content": [{
            "type": "web_search_result",
            "url": "https://example.com/rust",
            "title": "Rust"
        }]
    });
    let message = ConversationMessage::AssistantBlocks(vec![
        ModelBlock::ProviderToolUse {
            id: "srv_search_1".to_string(),
            name: "web_search".to_string(),
            input: json!({"query": "rust async"}),
            status: None,
            provider_data: Some(ProviderBlockData {
                format: ANTHROPIC_PROVIDER_BLOCK_FORMAT.to_string(),
                payload: raw_use.clone(),
            }),
        },
        ModelBlock::ProviderToolResult {
            tool_use_id: Some("srv_search_1".to_string()),
            content: raw_result["content"].clone(),
            is_error: false,
            status: None,
            provider_data: Some(ProviderBlockData {
                format: ANTHROPIC_PROVIDER_BLOCK_FORMAT.to_string(),
                payload: raw_result.clone(),
            }),
        },
    ]);

    let encoded = conversation_message_to_api(&message, None, true);
    assert_eq!(encoded.content, json!([raw_use, raw_result]));
}

#[test]
fn strips_zai_redundant_provider_tool_display_text() {
    let mut blocks = vec![
        ApiResponseBlock {
            kind: "text".to_string(),
            text: Some(
                "**🌐 Z.ai Built-in Tool: web_search_prime**\n\n\
                     **Input:**\n```json\n\
                     {\"content_size\":\"medium\",\"search_query\":\"rust async\"}\n\
                     ```\n*Executing on server...*\n"
                    .to_string(),
            ),
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "server_tool_use".to_string(),
            id: Some("call_1".to_string()),
            name: Some("web_search_prime".to_string()),
            input: Some(json!({
                "content_size": "medium",
                "search_query": "rust async"
            })),
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "text".to_string(),
            text: Some(
                "**Output:**\n**web_search_prime_result_summary:** [{\"title\":\"Rust\"}]"
                    .to_string(),
            ),
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "tool_result".to_string(),
            tool_use_id: Some("call_1".to_string()),
            content: Some(json!([[{"title": "Rust"}]])),
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "text".to_string(),
            text: Some("Rust uses async/await.".to_string()),
            ..Default::default()
        },
    ];

    strip_zai_redundant_provider_tool_text(&mut blocks);

    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0].kind, "server_tool_use");
    assert_eq!(blocks[1].kind, "tool_result");
    assert_eq!(blocks[2].text.as_deref(), Some("Rust uses async/await."));
}

#[test]
fn preserves_zai_like_text_without_matching_structured_sequence() {
    let mut blocks = vec![
        ApiResponseBlock {
            kind: "text".to_string(),
            text: Some(
                "**🌐 Z.ai Built-in Tool: web_search_prime**\n\n\
                     **Input:**\n```json\n{\"search_query\":\"displayed\"}\n```\n\
                     *Executing on server...*\n"
                    .to_string(),
            ),
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "server_tool_use".to_string(),
            id: Some("call_1".to_string()),
            name: Some("web_search_prime".to_string()),
            input: Some(json!({"search_query": "actual"})),
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "text".to_string(),
            text: Some("**Output:**\n**web_search_prime_result_summary:** []".to_string()),
            ..Default::default()
        },
        ApiResponseBlock {
            kind: "tool_result".to_string(),
            tool_use_id: Some("call_1".to_string()),
            content: Some(json!([])),
            ..Default::default()
        },
    ];

    strip_zai_redundant_provider_tool_text(&mut blocks);

    assert_eq!(blocks.len(), 4);
}

#[test]
fn warn_unsupported_silent_for_server_tool_blocks() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tracing_subscriber::{layer::SubscriberExt, Layer, Registry};

    struct WarningCounter(Arc<AtomicUsize>);

    impl Layer<Registry> for WarningCounter {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, Registry>,
        ) {
            if event.metadata().level() == &tracing::Level::WARN {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let warnings = Arc::new(AtomicUsize::new(0));
    let subscriber = Registry::default().with(WarningCounter(warnings.clone()));
    tracing::subscriber::with_default(subscriber, || {
        for kind in ["server_tool_use", "tool_result"] {
            let block = ApiResponseBlock {
                kind: kind.to_string(),
                ..Default::default()
            };
            warn_unsupported_anthropic_response_block(&block, "test", "model", 0, None, false);
            assert_eq!(
                warnings.load(Ordering::Relaxed),
                0,
                "{kind} should not warn"
            );
        }

        let block = ApiResponseBlock {
            kind: "unknown_block".to_string(),
            ..Default::default()
        };
        warn_unsupported_anthropic_response_block(&block, "test", "model", 0, None, false);
        assert_eq!(
            warnings.load(Ordering::Relaxed),
            1,
            "an unsupported block must warn, proving the observer is active"
        );
    });
}

#[test]
fn parse_sse_error_event_dashscope_fallback() {
    // DashScope sends non-standard error events: `message` is a string,
    // wrapped in an SSE `event: error` line instead of the Anthropic
    // `{"type":"error","error":{...}}` shape.
    let raw = b"event: error\ndata: {\"request_id\":\"req_1\",\"code\":\"InvalidParameter\",\"message\":\"Inference engine abort\"}";
    let event = parse_sse_event(raw)
        .expect("parse should succeed via fallback")
        .expect("should produce an event");
    assert_eq!(event.kind, "error");
    let error = event.error.expect("error field should be populated");
    assert_eq!(error.kind.as_deref(), Some("InvalidParameter"));
    assert_eq!(error.message.as_deref(), Some("Inference engine abort"));
}

#[test]
fn parse_sse_error_event_standard_anthropic() {
    // Standard Anthropic error event should still parse normally.
    let raw = b"data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}";
    let event = parse_sse_event(raw)
        .expect("parse should succeed")
        .expect("should produce an event");
    assert_eq!(event.kind, "error");
    let error = event.error.expect("error field should be populated");
    assert_eq!(error.kind.as_deref(), Some("overloaded_error"));
    assert_eq!(error.message.as_deref(), Some("Overloaded"));
}

#[test]
fn sse_accumulator_classifies_dashscope_error_as_retryable() {
    // End-to-end: DashScope error event should flow through SseParser,
    // then AnthropicStreamAccumulator::apply, and be classified as
    // ServerError + Retryable (not InvalidResponse + FailFast).
    let sse = b"event: error\ndata: {\"code\":\"InvalidParameter\",\"message\":\"Inference engine abort\"}\n\n";
    let mut parser = SseParser::default();
    let events = parser.push(sse).expect("push should succeed");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "error");

    let mut acc = AnthropicStreamAccumulator::default();
    let err = acc
        .apply(
            events.into_iter().next().unwrap(),
            "test-model",
            "test-url",
            None,
        )
        .expect_err("error event should fail the accumulator");
    let classification = crate::provider::retry::classify_provider_error(&err);
    assert_eq!(
        classification.kind,
        ProviderFailureKind::ServerError,
        "DashScope error should be ServerError, not InvalidResponse"
    );
    assert_eq!(
        classification.disposition,
        RetryDisposition::Retryable,
        "DashScope error should be Retryable, not FailFast"
    );
}

#[test]
fn parse_sse_non_error_event_with_string_message_still_fails() {
    // A non-error event with a string `message` should still fail to parse,
    // because the fallback only applies when the SSE event type is "error".
    let raw = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":\"oops\"}";
    let result = parse_sse_event(raw);
    assert!(
        result.is_err(),
        "non-error event with string message should still fail"
    );
}
