use super::*;
use holon_model_client::{chat_completions, Role};
use serde_json::json;

#[test]
fn request_body_preserves_holon_wire_controls() {
    let provider = OpenAiChatCompletionsProvider::new(
        Client::new(),
        ChatCompletionsConfig {
            base_url: "https://model.example/v1".into(),
            api_key: None,
            provider: None,
            model: "kimi-k3".into(),
            max_output_tokens: 512,
            max_output_tokens_parameter: output_limit_parameter(
                "tencent-tokenhub",
                "default",
                "kimi-k3",
            ),
            context_window_tokens: None,
            reasoning_effort: Some("none".into()),
        },
    )
    .with_service_tier(Some(ServiceTier::Fast));
    let assistant = chat_completions::message(
        Role::Assistant,
        Value::Null,
        vec![chat_completions::function_call("call-1", "lookup", &json!({"q": "needle"})).unwrap()],
        None,
    );
    let result = chat_completions::message(Role::Tool, json!("match"), vec![], Some("call-1"));
    let tool = chat_completions::function_tool(
        "lookup",
        Some("Find a match"),
        &json!({"type":"object"}),
        false,
    );

    let body = provider.request_body(vec![assistant, result], vec![tool], Some("cache-key"));
    assert_eq!(
        body,
        json!({
            "model":"kimi-k3",
            "messages":[
                {"role":"assistant", "content":null, "tool_calls":[
                    {"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"needle\"}"}}
                ]},
                {"role":"tool","tool_call_id":"call-1","content":"match"}
            ],
            "max_completion_tokens":512,
            "stream":false,
            "tools":[{"type":"function","function":{"name":"lookup","description":"Find a match","parameters":{"type":"object"},"strict":false}}],
            "tool_choice":"auto",
            "prompt_cache_key":"cache-key",
            "reasoning_effort":"none",
            "service_tier":"priority"
        })
    );
}

#[test]
fn chat_completions_url_accepts_openai_compatible_base_urls() {
    assert_eq!(
        chat_completions_url("https://api.deepseek.com"),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://openrouter.ai/api/v1"),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://qianfan.baidubce.com/v2"),
        "https://qianfan.baidubce.com/v2/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://ark.ap-southeast.bytepluses.com/api/v3"),
        "https://ark.ap-southeast.bytepluses.com/api/v3/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.z.ai/api/paas/v4"),
        "https://api.z.ai/api/paas/v4/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://proxy.example/chat/completions"),
        "https://proxy.example/chat/completions"
    );
}

#[test]
fn github_copilot_chat_url_omits_the_generic_v1_prefix() {
    let provider = OpenAiChatCompletionsProvider::new(
        reqwest::Client::new(),
        ChatCompletionsConfig {
            base_url: "https://api.githubcopilot.com".into(),
            api_key: None,
            provider: Some(crate::config::GITHUB_COPILOT_PROVIDER_ID.to_string()),
            model: "gpt-5".into(),
            max_output_tokens: 1024,
            max_output_tokens_parameter: "max_tokens",
            context_window_tokens: None,
            reasoning_effort: None,
        },
    );
    assert_eq!(
        provider.chat_completions_url(),
        "https://api.githubcopilot.com/chat/completions"
    );

    // Other providers keep the generic OpenAI-compatible shape.
    let generic = OpenAiChatCompletionsProvider::new(
        reqwest::Client::new(),
        ChatCompletionsConfig {
            base_url: "https://api.example.com".into(),
            api_key: None,
            provider: Some("example".into()),
            model: "m".into(),
            max_output_tokens: 16,
            max_output_tokens_parameter: "max_tokens",
            context_window_tokens: None,
            reasoning_effort: None,
        },
    );
    assert_eq!(
        generic.chat_completions_url(),
        "https://api.example.com/v1/chat/completions"
    );
}
