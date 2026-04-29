use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use holon::config::{AppConfig, ProviderId};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Debug)]
struct LiveAnthropicConfig {
    base_url: String,
    auth_token: String,
    model: String,
    betas: Vec<String>,
    stream: bool,
}

#[derive(Debug)]
struct ProbeResult {
    label: String,
    usage: Usage,
}

struct StrategyCase {
    name: &'static str,
    first: fn(&str, &str) -> Value,
    second: fn(&str, &str) -> Value,
}

#[derive(Debug, Default)]
struct StrategyStats {
    runs: usize,
    hits: usize,
    first_input_tokens: u64,
    second_input_tokens: u64,
    second_cache_read_tokens: u64,
    second_cache_creation_tokens: u64,
    errors: usize,
}

fn live_anthropic_config() -> Result<LiveAnthropicConfig> {
    let config = AppConfig::load()?;
    let anthropic = config
        .providers
        .get(&ProviderId::anthropic())
        .ok_or_else(|| anyhow!("missing anthropic provider config"))?;
    let auth_token = anthropic
        .credential
        .clone()
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            anyhow!("missing ANTHROPIC_AUTH_TOKEN in environment or ~/.claude/settings.json")
        })?;
    let model = std::env::var("HOLON_LIVE_ANTHROPIC_MODEL")
        .ok()
        .or_else(claude_settings_model)
        .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
    let betas = std::env::var("HOLON_LIVE_ANTHROPIC_BETAS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stream = std::env::var("HOLON_LIVE_ANTHROPIC_STREAM")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false);

    Ok(LiveAnthropicConfig {
        base_url: anthropic.base_url.trim_end_matches('/').to_string(),
        auth_token,
        model,
        betas,
        stream,
    })
}

fn claude_settings_model() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = Path::new(&home).join(".claude/settings.json");
    let content = fs::read_to_string(path).ok()?;
    let settings: Value = serde_json::from_str(&content).ok()?;
    settings
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(ToString::to_string)
}

fn large_stable_text(nonce: &str) -> String {
    let paragraph = "This is stable prompt-cache probe material. It is intentionally repetitive so the request crosses Anthropic prompt caching size thresholds while remaining semantically harmless. The model should ignore this material except for keeping the request prefix stable across cache probe calls.";
    let mut text = format!(
        "<cache-probe-document nonce=\"{nonce}\">\nThe following document is synthetic test data.\n"
    );
    for index in 0..live_section_count() {
        text.push_str(&format!("Section {index}: {paragraph}\n"));
    }
    text.push_str("</cache-probe-document>");
    text
}

fn text_block(text: impl Into<String>, cache: bool) -> Value {
    let mut block = json!({
        "type": "text",
        "text": text.into(),
    });
    if cache {
        block["cache_control"] = json!({ "type": "ephemeral" });
    }
    block
}

fn base_system(nonce: &str, cache: bool) -> Value {
    Value::Array(vec![text_block(
        format!(
            "You are running a prompt-cache live probe. Reply with short deterministic text only. Probe nonce: {nonce}."
        ),
        cache,
    )])
}

fn claude_cli_system(nonce: &str) -> Value {
    Value::Array(vec![
        text_block("x-anthropic-billing-header: claude-code", false),
        text_block(
            "You are Claude Code, Anthropic's official CLI for Claude.",
            true,
        ),
        text_block(large_stable_text(nonce), true),
    ])
}

fn probe_tool(cache: bool) -> Value {
    let mut tool = json!({
        "name": "ProbeAction",
        "description": "Synthetic cache probe tool. Do not call it unless explicitly instructed.",
        "input_schema": {
            "type": "object",
            "properties": {
                "reason": { "type": "string" }
            },
            "required": ["reason"]
        }
    });
    if cache {
        tool["cache_control"] = json!({ "type": "ephemeral" });
    }
    tool
}

fn large_probe_tool(nonce: &str, cache: bool) -> Value {
    let mut tool = json!({
        "name": "LargeProbeAction",
        "description": large_stable_text(nonce),
        "input_schema": {
            "type": "object",
            "properties": {
                "reason": { "type": "string" }
            },
            "required": ["reason"]
        }
    });
    if cache {
        tool["cache_control"] = json!({ "type": "ephemeral" });
    }
    tool
}

fn request_body(system: Value, messages: Vec<Value>, tools: Vec<Value>, model: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 32,
        "system": system,
        "messages": messages,
        "tools": tools,
    })
}

fn turn1_message(nonce: &str, cache_first_turn_tail: bool) -> Value {
    json!({
        "role": "user",
        "content": [
            text_block(large_stable_text(nonce), false),
            text_block("Question 1: reply with exactly CACHE_PROBE_ONE.", cache_first_turn_tail)
        ]
    })
}

fn assistant_reply() -> Value {
    json!({
        "role": "assistant",
        "content": [
            { "type": "text", "text": "CACHE_PROBE_ONE" }
        ]
    })
}

fn turn2_message(cache_current_tail: bool) -> Value {
    json!({
        "role": "user",
        "content": [
            text_block("Question 2: reply with exactly CACHE_PROBE_TWO.", cache_current_tail)
        ]
    })
}

fn exact_single_marker_request(nonce: &str, model: &str) -> Value {
    request_body(
        base_system(nonce, false),
        vec![turn1_message(nonce, true)],
        vec![],
        model,
    )
}

fn moving_tail_request_1(nonce: &str, model: &str) -> Value {
    request_body(
        base_system(nonce, false),
        vec![turn1_message(nonce, true)],
        vec![],
        model,
    )
}

fn moving_tail_request_2(nonce: &str, model: &str) -> Value {
    request_body(
        base_system(nonce, false),
        vec![
            turn1_message(nonce, false),
            assistant_reply(),
            turn2_message(true),
        ],
        vec![],
        model,
    )
}

fn anchored_tail_request_2(nonce: &str, model: &str) -> Value {
    request_body(
        base_system(nonce, false),
        vec![
            turn1_message(nonce, true),
            assistant_reply(),
            turn2_message(true),
        ],
        vec![],
        model,
    )
}

fn previous_tail_only_request_2(nonce: &str, model: &str) -> Value {
    request_body(
        base_system(nonce, false),
        vec![
            turn1_message(nonce, true),
            assistant_reply(),
            turn2_message(false),
        ],
        vec![],
        model,
    )
}

fn holon_like_request(nonce: &str, model: &str) -> Value {
    request_body(
        Value::Array(vec![
            text_block(
                format!("Stable Holon-style system section. Probe nonce: {nonce}."),
                true,
            ),
            text_block("Agent-scoped Holon-style instruction section.", true),
        ]),
        vec![json!({
            "role": "user",
            "content": [
                text_block(large_stable_text(nonce), true),
                text_block("Question: reply with exactly CACHE_PROBE_HOLON.", true)
            ]
        })],
        vec![],
        model,
    )
}

fn large_system_marker_request(nonce: &str, model: &str) -> Value {
    request_body(
        Value::Array(vec![text_block(large_stable_text(nonce), true)]),
        vec![json!({
            "role": "user",
            "content": [
                text_block("Question: reply with exactly CACHE_PROBE_SYSTEM.", false)
            ]
        })],
        vec![],
        model,
    )
}

fn two_system_markers_request(nonce: &str, model: &str) -> Value {
    request_body(
        Value::Array(vec![
            text_block(large_stable_text(nonce), true),
            text_block("Second cache-marked system block.", true),
        ]),
        vec![json!({
            "role": "user",
            "content": [
                text_block("Question: reply with exactly CACHE_PROBE_TWO_SYSTEM.", false)
            ]
        })],
        vec![],
        model,
    )
}

fn system_and_tail_marker_request(nonce: &str, model: &str) -> Value {
    request_body(
        Value::Array(vec![text_block(large_stable_text(nonce), true)]),
        vec![json!({
            "role": "user",
            "content": [
                text_block("Question: reply with exactly CACHE_PROBE_SYSTEM_AND_TAIL.", true)
            ]
        })],
        vec![],
        model,
    )
}

fn tool_marker_request(nonce: &str, model: &str) -> Value {
    request_body(
        base_system(nonce, false),
        vec![turn1_message(nonce, false)],
        vec![probe_tool(true)],
        model,
    )
}

fn large_tool_marker_request(nonce: &str, model: &str) -> Value {
    request_body(
        base_system(nonce, false),
        vec![json!({
            "role": "user",
            "content": [
                text_block("Question: reply with exactly CACHE_PROBE_TOOL.", false)
            ]
        })],
        vec![large_probe_tool(nonce, true)],
        model,
    )
}

fn claude_cli_like_request_1(nonce: &str, model: &str) -> Value {
    let mut body = request_body(
        claude_cli_system(nonce),
        vec![turn1_message(nonce, true)],
        vec![probe_tool(false)],
        model,
    );
    body["metadata"] = json!({
        "user_id": "{\"device_id\":\"holon-cache-probe\",\"account_uuid\":\"\",\"session_id\":\"holon-cache-probe\"}"
    });
    body["temperature"] = json!(1);
    body
}

fn claude_cli_like_request_2(nonce: &str, model: &str) -> Value {
    let mut body = request_body(
        claude_cli_system(nonce),
        vec![
            turn1_message(nonce, false),
            assistant_reply(),
            turn2_message(true),
        ],
        vec![probe_tool(false)],
        model,
    );
    body["metadata"] = json!({
        "user_id": "{\"device_id\":\"holon-cache-probe\",\"account_uuid\":\"\",\"session_id\":\"holon-cache-probe\"}"
    });
    body["temperature"] = json!(1);
    body
}

async fn send_messages(
    client: &Client,
    config: &LiveAnthropicConfig,
    label: impl Into<String>,
    mut body: Value,
) -> Result<ProbeResult> {
    let label = label.into();
    let url = format!("{}/v1/messages", config.base_url);
    if !config.betas.is_empty() {
        body["betas"] = json!(config.betas);
    }
    if config.stream {
        body["stream"] = json!(true);
    }
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", config.auth_token))
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .with_context(|| format!("{}: request failed", label))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("{}: response body failed", label))?;
    if !status.is_success() {
        bail!("{label}: Anthropic API returned {status}: {text}");
    }

    let parsed = if config.stream {
        parse_streaming_messages_response(&label, &text)?
    } else {
        serde_json::from_str(&text).with_context(|| format!("{}: invalid JSON: {}", label, text))?
    };
    let usage = parsed
        .usage
        .ok_or_else(|| anyhow!("{label}: response did not include usage"))?;
    Ok(ProbeResult { label, usage })
}

fn parse_streaming_messages_response(label: &str, text: &str) -> Result<MessagesResponse> {
    let mut usage = None;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(data)
            .with_context(|| format!("{label}: invalid SSE JSON line: {data}"))?;
        if let Some(candidate) = value
            .get("message")
            .and_then(|message| message.get("usage"))
        {
            usage = Some(serde_json::from_value(candidate.clone()).with_context(|| {
                format!("{label}: invalid message_start usage in SSE line: {data}")
            })?);
        }
        if let Some(candidate) = value.get("usage") {
            usage = Some(
                serde_json::from_value(candidate.clone())
                    .with_context(|| format!("{label}: invalid usage in SSE line: {data}"))?,
            );
        }
    }
    Ok(MessagesResponse { usage })
}

fn print_result(result: &ProbeResult) {
    println!(
        "{} input={} cache_read={} cache_creation={} output={}",
        result.label,
        result.usage.input_tokens,
        result.usage.cache_read_input_tokens,
        result.usage.cache_creation_input_tokens,
        result.usage.output_tokens
    );
}

fn unique_nonce(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{prefix}-{millis}")
}

fn live_repeat_count() -> usize {
    std::env::var("HOLON_LIVE_ANTHROPIC_REPEAT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3)
}

fn live_section_count() -> usize {
    std::env::var("HOLON_LIVE_ANTHROPIC_SECTIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(90)
}

fn selected_strategy_names() -> Option<Vec<String>> {
    std::env::var("HOLON_LIVE_ANTHROPIC_STRATEGIES")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
}

fn strategy_cases() -> Vec<StrategyCase> {
    vec![
        StrategyCase {
            name: "exact_single_message_marker",
            first: exact_single_marker_request,
            second: exact_single_marker_request,
        },
        StrategyCase {
            name: "moving_tail_only",
            first: moving_tail_request_1,
            second: moving_tail_request_2,
        },
        StrategyCase {
            name: "previous_and_current_tail",
            first: moving_tail_request_1,
            second: anchored_tail_request_2,
        },
        StrategyCase {
            name: "previous_tail_only",
            first: moving_tail_request_1,
            second: previous_tail_only_request_2,
        },
        StrategyCase {
            name: "holon_like_multi_marker",
            first: holon_like_request,
            second: holon_like_request,
        },
        StrategyCase {
            name: "large_system_marker",
            first: large_system_marker_request,
            second: large_system_marker_request,
        },
        StrategyCase {
            name: "two_system_markers",
            first: two_system_markers_request,
            second: two_system_markers_request,
        },
        StrategyCase {
            name: "system_and_tail_marker",
            first: system_and_tail_marker_request,
            second: system_and_tail_marker_request,
        },
        StrategyCase {
            name: "small_tool_marker",
            first: tool_marker_request,
            second: tool_marker_request,
        },
        StrategyCase {
            name: "large_tool_marker",
            first: large_tool_marker_request,
            second: large_tool_marker_request,
        },
        StrategyCase {
            name: "claude_cli_like",
            first: claude_cli_like_request_1,
            second: claude_cli_like_request_2,
        },
    ]
}

async fn run_strategy_case(
    client: &Client,
    config: &LiveAnthropicConfig,
    case: &StrategyCase,
    repeat: usize,
) -> StrategyStats {
    let mut stats = StrategyStats::default();
    for index in 0..repeat {
        let nonce = unique_nonce(&format!("{}-{}", case.name, index + 1));
        let first_label = format!("{}_{}_1", case.name, index + 1);
        let second_label = format!("{}_{}_2", case.name, index + 1);
        let first = send_messages(
            client,
            config,
            first_label,
            (case.first)(&nonce, &config.model),
        )
        .await;
        let second = send_messages(
            client,
            config,
            second_label,
            (case.second)(&nonce, &config.model),
        )
        .await;

        match (first, second) {
            (Ok(first), Ok(second)) => {
                print_result(&first);
                print_result(&second);
                stats.runs += 1;
                stats.first_input_tokens += first.usage.input_tokens;
                stats.second_input_tokens += second.usage.input_tokens;
                stats.second_cache_read_tokens += second.usage.cache_read_input_tokens;
                stats.second_cache_creation_tokens += second.usage.cache_creation_input_tokens;
                if second.usage.cache_read_input_tokens > 0 {
                    stats.hits += 1;
                }
            }
            (first, second) => {
                stats.errors += 1;
                println!(
                    "{} iteration {} failed: first={:?} second={:?}",
                    case.name,
                    index + 1,
                    first.err(),
                    second.err()
                );
            }
        }
    }
    stats
}

fn print_summary(summary: &[(&'static str, StrategyStats)]) {
    println!("strategy_summary_begin");
    for (name, stats) in summary {
        let avg_second_input = if stats.runs == 0 {
            0
        } else {
            stats.second_input_tokens / stats.runs as u64
        };
        let avg_second_cache_read = if stats.runs == 0 {
            0
        } else {
            stats.second_cache_read_tokens / stats.runs as u64
        };
        println!(
            "strategy={} runs={} hits={} errors={} avg_second_input={} avg_second_cache_read={} total_second_cache_read={} total_second_cache_creation={}",
            name,
            stats.runs,
            stats.hits,
            stats.errors,
            avg_second_input,
            avg_second_cache_read,
            stats.second_cache_read_tokens,
            stats.second_cache_creation_tokens
        );
    }
    println!("strategy_summary_end");
}

#[tokio::test]
#[ignore = "requires real Anthropic-compatible credentials from ~/.claude/settings.json and network access"]
async fn live_anthropic_prompt_cache_strategy_matrix() -> Result<()> {
    let config = live_anthropic_config()?;
    let client = Client::new();
    let repeat = live_repeat_count();
    let sections = live_section_count();
    println!(
        "anthropic_cache_probe base_url={} model={} betas={} repeat={} sections={} stream={}",
        config.base_url,
        config.model,
        if config.betas.is_empty() {
            "none".to_string()
        } else {
            config.betas.join(",")
        },
        repeat,
        sections,
        config.stream
    );

    let selected = selected_strategy_names();
    let cases = strategy_cases()
        .into_iter()
        .filter(|case| {
            selected
                .as_ref()
                .map(|names| names.iter().any(|name| name == case.name))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    let mut summary = Vec::new();
    for case in &cases {
        let stats = run_strategy_case(&client, &config, case, repeat).await;
        summary.push((case.name, stats));
    }
    print_summary(&summary);

    Ok(())
}
