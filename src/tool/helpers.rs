//! Helper functions for tool implementation
//!
//! This module contains shared utility functions used by tool implementations.

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde_json::json;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::tool::ToolError;
use crate::types::CommandCostDiagnostics;

pub(crate) const DEFAULT_TOOL_OUTPUT_TOKENS: u64 = 8_000;
pub(crate) const MAX_TOOL_OUTPUT_TOKENS: u64 = 64_000;
pub(crate) const COMMAND_COST_SOFT_THRESHOLD_CHARS: usize = 4_000;
pub(crate) const COMMAND_PREVIEW_CHARS: usize = 240;

pub(crate) fn parse_tool_args<T>(tool_name: &str, input: &Value) -> Result<T>
where
    T: DeserializeOwned,
{
    parse_tool_args_with_recovery_hint(tool_name, input, || {
        format!("provide input for {tool_name} that matches the published tool schema")
    })
}

pub(crate) fn parse_tool_args_with_recovery_hint<T, F>(
    tool_name: &str,
    input: &Value,
    recovery_hint: F,
) -> Result<T>
where
    T: DeserializeOwned,
    F: FnOnce() -> String,
{
    let coerced = coerce_string_scalars(input);
    let input = coerced.as_ref().unwrap_or(input);
    serde_json::from_value(input.clone()).map_err(|error| {
        anyhow::Error::from(
            ToolError::new(
                "invalid_tool_input",
                format!("input for {tool_name} does not match the tool schema"),
            )
            .with_details(serde_json::json!({
                "tool_name": tool_name,
                "parse_error": error.to_string(),
            }))
            .with_recovery_hint(recovery_hint()),
        )
    })
}

/// Recursively coerces string scalars to their typed JSON equivalents so that
/// minor LLM mistakes (e.g. `"42"` instead of `42`, `"true"` instead of `true`)
/// do not cause hard schema failures during `serde_json::from_value`.
///
/// Only pure numeric strings and the exact strings `"true"` / `"false"` are
/// converted. Mixed strings like `"10px"` or `"hello"` are left untouched.
/// Returns `None` when no changes were made so the caller can avoid a
/// needless clone.
fn coerce_string_scalars(value: &Value) -> Option<Value> {
    coerce_value(value)
}

fn coerce_value(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            let mut new_map = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                match coerce_value(v) {
                    Some(coerced) => {
                        new_map.insert(k.clone(), coerced);
                        changed = true;
                    }
                    None => {
                        new_map.insert(k.clone(), v.clone());
                    }
                }
            }
            if changed {
                Some(Value::Object(new_map))
            } else {
                None
            }
        }
        Value::Array(arr) => {
            let mut changed = false;
            let mut new_arr = Vec::with_capacity(arr.len());
            for v in arr {
                match coerce_value(v) {
                    Some(coerced) => {
                        new_arr.push(coerced);
                        changed = true;
                    }
                    None => {
                        new_arr.push(v.clone());
                    }
                }
            }
            if changed {
                Some(Value::Array(new_arr))
            } else {
                None
            }
        }
        Value::String(s) => coerce_string(s),
        _ => None,
    }
}

fn coerce_string(s: &str) -> Option<Value> {
    if s.eq_ignore_ascii_case("true") {
        return Some(Value::Bool(true));
    }
    if s.eq_ignore_ascii_case("false") {
        return Some(Value::Bool(false));
    }
    // Try integer first to preserve precision, then float.
    if let Ok(i) = s.parse::<i64>() {
        return Some(Value::Number(i.into()));
    }
    if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() {
            return Some(Value::Number(serde_json::Number::from_f64(f)?));
        }
    }
    None
}

pub(crate) fn invalid_tool_input(
    tool_name: &str,
    message: impl Into<String>,
    mut details: Value,
    recovery_hint: impl Into<String>,
) -> anyhow::Error {
    if let Some(details_object) = details.as_object_mut() {
        details_object
            .entry("tool_name".to_string())
            .or_insert_with(|| Value::String(tool_name.to_string()));
    }
    anyhow::Error::from(
        ToolError::new("invalid_tool_input", message)
            .with_details(details)
            .with_recovery_hint(recovery_hint),
    )
}

pub(crate) fn validate_non_empty(value: String, tool_name: &str, field: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid_tool_input(
            tool_name,
            format!("{tool_name} requires a non-empty `{field}`"),
            json!({
                "tool_name": tool_name,
                "field": field,
                "validation_error": "must not be empty",
            }),
            format!(
                "provide a non-empty value for `{field}` that matches the published tool schema"
            ),
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn normalize_optional_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
}

/// Normalize a path by resolving . and .. components.
pub(crate) fn normalize_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let can_pop = matches!(
                    normalized.components().next_back(),
                    Some(std::path::Component::Normal(_))
                );
                if can_pop {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Truncate text to a maximum character count.
pub(crate) fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(max).collect::<String>())
    }
}

pub(crate) fn output_char_budget(max_output_tokens: Option<usize>) -> usize {
    max_output_tokens
        .and_then(|tokens| tokens.checked_mul(4))
        .unwrap_or((DEFAULT_TOOL_OUTPUT_TOKENS as usize).saturating_mul(4))
}

pub(crate) fn effective_tool_output_tokens(
    requested: Option<u64>,
    default_tokens: u64,
    max_tokens: u64,
) -> u64 {
    let default_tokens = default_tokens.max(1);
    let max_tokens = max_tokens.max(1);
    requested
        .filter(|value| *value > 0)
        .unwrap_or(default_tokens)
        .min(max_tokens)
}

pub(crate) fn command_preview(cmd: &str) -> String {
    if command_contains_heredoc(cmd) || command_contains_inline_script(cmd) {
        return "[omitted: command contains heredoc or inline script]".to_string();
    }
    truncate_text(&redact_command_secrets(cmd), COMMAND_PREVIEW_CHARS)
}

pub(crate) fn command_display(cmd: &str) -> String {
    let redacted = redact_command_secrets(cmd);
    if command_contains_heredoc(cmd) || command_contains_inline_script(cmd) {
        return redacted.lines().take(2).collect::<Vec<_>>().join("\n");
    }
    redacted
}

pub(crate) fn command_digest(cmd: &str) -> String {
    let digest = Sha256::digest(cmd.as_bytes());
    format!("{digest:x}")
}

pub(crate) fn command_receipt_source_ref(
    tool_execution_id: &str,
    batch_item_index: Option<usize>,
) -> String {
    match batch_item_index {
        Some(index) => format!("tool_execution:{tool_execution_id}:batch_item:{index}:cmd"),
        None => format!("tool_execution:{tool_execution_id}:cmd"),
    }
}

pub(crate) fn command_output_source_ref(
    tool_execution_id: &str,
    batch_item_index: Option<usize>,
    stream: &str,
) -> String {
    match batch_item_index {
        Some(index) => {
            format!("tool_execution:{tool_execution_id}:batch_item:{index}:{stream}")
        }
        None => format!("tool_execution:{tool_execution_id}:{stream}"),
    }
}

pub(crate) fn command_cost_diagnostics(
    cmd: &str,
    effective_max_output_tokens: u64,
) -> CommandCostDiagnostics {
    let cmd_char_count = cmd.chars().count();
    let contains_heredoc = command_contains_heredoc(cmd);
    let contains_inline_script = command_contains_inline_script(cmd);
    CommandCostDiagnostics {
        cmd_preview: command_preview(cmd),
        cmd_char_count,
        cmd_estimated_tokens: (cmd_char_count + 3) / 4,
        contains_heredoc,
        contains_inline_script,
        exceeds_soft_threshold: cmd_char_count > COMMAND_COST_SOFT_THRESHOLD_CHARS,
        effective_max_output_tokens,
        output_char_budget: output_char_budget(Some(effective_max_output_tokens as usize)),
    }
}

fn command_contains_heredoc(cmd: &str) -> bool {
    cmd.contains("<<")
}

fn command_contains_inline_script(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    lower.contains("python -")
        || lower.contains("python3 -")
        || lower.contains("node -")
        || lower.contains("ruby -")
        || lower.contains("perl -")
        || lower.contains("bash -c")
        || lower.contains("sh -c")
        || lower.contains("zsh -c")
}

fn redact_command_secrets(cmd: &str) -> String {
    let mut changed = false;
    let mut redact_next = false;
    let mut parts = Vec::new();

    for token in cmd.split_whitespace() {
        if redact_next {
            parts.push("[redacted]".to_string());
            redact_next = false;
            changed = true;
            continue;
        }

        let (redacted, should_redact_next) = redact_command_token(token);
        if redacted != token {
            changed = true;
        }
        redact_next = should_redact_next;
        parts.push(redacted);
    }

    if changed {
        parts.join(" ")
    } else {
        cmd.to_string()
    }
}

fn redact_command_token(token: &str) -> (String, bool) {
    let token = redact_url_credentials(token);

    if let Some((key, _value)) = token.split_once('=') {
        if is_sensitive_command_key(key) {
            return (format!("{key}=[redacted]"), false);
        }
    }

    if is_sensitive_command_flag(&token) {
        return (token, true);
    }

    (token, false)
}

fn redact_url_credentials(token: &str) -> String {
    let Some(scheme_index) = token.find("://") else {
        return token.to_string();
    };
    let authority_start = scheme_index + 3;
    let Some(at_relative) = token[authority_start..].find('@') else {
        return token.to_string();
    };
    let at_index = authority_start + at_relative;
    let authority = &token[authority_start..at_index];
    if !authority.contains(':') {
        return token.to_string();
    }
    format!(
        "{}[redacted]{}",
        &token[..authority_start],
        &token[at_index..]
    )
}

fn is_sensitive_command_flag(token: &str) -> bool {
    let normalized = token
        .trim_start_matches('-')
        .replace('-', "_")
        .to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "TOKEN"
            | "ACCESS_TOKEN"
            | "AUTH_TOKEN"
            | "PASSWORD"
            | "PASS"
            | "SECRET"
            | "API_KEY"
            | "ACCESS_KEY"
            | "PRIVATE_KEY"
            | "CREDENTIAL"
            | "CREDENTIALS"
    )
}

fn is_sensitive_command_key(key: &str) -> bool {
    let normalized = key
        .trim_start_matches('-')
        .replace('-', "_")
        .to_ascii_uppercase();
    normalized.contains("TOKEN")
        || normalized.contains("SECRET")
        || normalized.contains("PASSWORD")
        || normalized == "PASS"
        || normalized.contains("API_KEY")
        || normalized.contains("ACCESS_KEY")
        || normalized.contains("PRIVATE_KEY")
        || normalized.contains("CREDENTIAL")
}

pub(crate) fn truncate_output_to_char_budget(text: &str, char_budget: usize) -> (String, bool) {
    const MARKER: &str = "\n...\n[output truncated: showing leading and trailing context]\n...\n";

    if text.chars().count() <= char_budget {
        return (text.to_string(), false);
    }

    let marker_len = MARKER.chars().count();
    if char_budget <= marker_len {
        return (text.chars().take(char_budget).collect(), true);
    }

    let remaining = char_budget - marker_len;
    let prefix_len = remaining / 2;
    let suffix_len = remaining - prefix_len;
    let prefix = text.chars().take(prefix_len).collect::<String>();
    let total_chars = text.chars().count();
    let suffix = text
        .chars()
        .skip(total_chars.saturating_sub(suffix_len))
        .collect::<String>();
    (format!("{prefix}{MARKER}{suffix}"), true)
}

/// Approximate a max-output-token limit with a conservative character budget.
pub(crate) fn truncate_output_for_tokens(text: &str, max_output_tokens: Option<usize>) -> String {
    truncate_output_with_flag(text, max_output_tokens).0
}

pub(crate) fn truncate_output_with_flag(
    text: &str,
    max_output_tokens: Option<usize>,
) -> (String, bool) {
    truncate_output_to_char_budget(text, output_char_budget(max_output_tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_budget_defaults_to_command_tool_default() {
        assert_eq!(output_char_budget(None), 32_000);
    }

    #[test]
    fn effective_tool_output_tokens_defaults_and_clamps() {
        assert_eq!(effective_tool_output_tokens(None, 8_000, 64_000), 8_000);
        assert_eq!(effective_tool_output_tokens(Some(0), 8_000, 64_000), 8_000);
        assert_eq!(
            effective_tool_output_tokens(Some(100_000), 8_000, 64_000),
            64_000
        );
    }

    #[test]
    fn command_cost_diagnostics_reports_long_inline_commands_without_full_echo() {
        let cmd = format!(
            "python - <<'PY'\n{}FINAL_SECRET_MARKER\nPY",
            "print('secret')\n".repeat(400)
        );
        let diagnostics = command_cost_diagnostics(&cmd, 2_000);

        assert!(diagnostics.contains_heredoc);
        assert!(diagnostics.contains_inline_script);
        assert!(diagnostics.exceeds_soft_threshold);
        assert_eq!(diagnostics.effective_max_output_tokens, 2_000);
        assert_eq!(diagnostics.output_char_budget, 8_000);
        assert_eq!(
            diagnostics.cmd_preview,
            "[omitted: command contains heredoc or inline script]"
        );
        assert!(!diagnostics.cmd_preview.contains("FINAL_SECRET_MARKER"));
        assert_eq!(command_display(&cmd), "python - <<'PY'\nprint('secret')");
    }

    #[test]
    fn command_preview_redacts_common_secret_shapes() {
        let preview = command_preview(
            "TOKEN=abc123 curl --password hunter2 https://user:pass@example.com/path",
        );

        assert!(preview.contains("TOKEN=[redacted]"));
        assert!(preview.contains("--password [redacted]"));
        assert!(preview.contains("https://[redacted]@example.com/path"));
        assert!(!preview.contains("abc123"));
        assert!(!preview.contains("hunter2"));
        assert!(!preview.contains("user:pass"));
    }

    #[test]
    fn command_display_keeps_full_non_script_command_with_redaction() {
        let display = command_display(
            "TOKEN=abc123 cargo test --all-targets -- --exact some_really_long_test_name",
        );

        assert!(display.contains("TOKEN=[redacted]"));
        assert!(display.contains("cargo test --all-targets -- --exact some_really_long_test_name"));
        assert!(!display.contains("abc123"));
    }

    #[test]
    fn coerce_string_to_number_integer() {
        assert_eq!(coerce_string("42"), Some(Value::Number(42i64.into())));
        assert_eq!(coerce_string("0"), Some(Value::Number(0i64.into())));
        assert_eq!(coerce_string("-7"), Some(Value::Number((-7i64).into())));
    }

    #[test]
    fn coerce_string_to_number_float() {
        let result = coerce_string("2.718");
        assert_eq!(
            result,
            Some(Value::Number(serde_json::Number::from_f64(2.718).unwrap()))
        );
    }

    #[test]
    fn coerce_string_to_bool() {
        assert_eq!(coerce_string("true"), Some(Value::Bool(true)));
        assert_eq!(coerce_string("false"), Some(Value::Bool(false)));
        assert_eq!(coerce_string("TRUE"), Some(Value::Bool(true)));
        assert_eq!(coerce_string("False"), Some(Value::Bool(false)));
    }

    #[test]
    fn coerce_string_leaves_non_numeric_strings() {
        assert_eq!(coerce_string("10px"), None);
        assert_eq!(coerce_string("hello"), None);
        assert_eq!(coerce_string(""), None);
        assert_eq!(coerce_string("123abc"), None);
        assert_eq!(coerce_string("null"), None);
    }

    #[test]
    fn coerce_string_scalars_recursive_object() {
        let input = json!({
            "name": "test",
            "count": "42",
            "enabled": "true",
            "nested": {
                "ratio": "2.718",
                "flag": "false"
            }
        });
        let result = coerce_string_scalars(&input).expect("should have coerced");
        assert_eq!(
            result,
            json!({
                "name": "test",
                "count": 42,
                "enabled": true,
                "nested": {
                    "ratio": 2.718,
                    "flag": false
                }
            })
        );
    }

    #[test]
    fn coerce_string_scalars_recursive_array() {
        let input = json!({
            "items": [
                {"yield_time_ms": "10000", "cmd": "echo hi"},
                {"yield_time_ms": "5000", "cmd": "echo bye"}
            ]
        });
        let result = coerce_string_scalars(&input).expect("should have coerced");
        assert_eq!(
            result,
            json!({
                "items": [
                    {"yield_time_ms": 10000, "cmd": "echo hi"},
                    {"yield_time_ms": 5000, "cmd": "echo bye"}
                ]
            })
        );
    }

    #[test]
    fn coerce_string_scalars_returns_none_when_no_change() {
        let input = json!({"name": "test", "cmd": "echo"});
        assert!(coerce_string_scalars(&input).is_none());
    }
}
