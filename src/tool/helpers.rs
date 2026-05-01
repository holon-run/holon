//! Helper functions for tool implementation
//!
//! This module contains shared utility functions used by tool implementations.

use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use serde_json::json;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::tool::ToolError;
use crate::types::CommandCostDiagnostics;

pub(crate) const DEFAULT_TOOL_OUTPUT_TOKENS: u64 = 2_000;
pub(crate) const MAX_TOOL_OUTPUT_TOKENS: u64 = 10_000;
pub(crate) const COMMAND_COST_SOFT_THRESHOLD_CHARS: usize = 4_000;
pub(crate) const COMMAND_PREVIEW_CHARS: usize = 240;

pub(crate) fn parse_tool_args<T>(tool_name: &str, input: &Value) -> Result<T>
where
    T: DeserializeOwned,
{
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
            .with_recovery_hint(format!(
                "provide input for {tool_name} that matches the published tool schema"
            )),
        )
    })
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

/// Resolve a path relative to the workspace root, ensuring it doesn't escape.
pub(crate) fn resolve_workspace_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let candidate = if Path::new(relative).is_absolute() {
        PathBuf::from(relative)
    } else {
        root.join(relative)
    };
    let normalized_root = normalize_path(root)?;
    let normalized_candidate = normalize_path(&candidate)?;
    if !normalized_candidate.starts_with(&normalized_root) {
        return Err(anyhow!("path escapes workspace root"));
    }
    Ok(candidate)
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

/// Extract the sleep reason from input, handling structured payloads.
pub(crate) fn extract_sleep_reason(input: &Value) -> String {
    let reason = input
        .get("reason")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let object = match input {
        Value::Object(map) => map,
        _ => return reason.unwrap_or("sleep requested").to_string(),
    };

    let has_extra_fields = object
        .keys()
        .any(|key| key != "reason" && key != "duration_ms");
    if !has_extra_fields {
        return reason.unwrap_or("sleep requested").to_string();
    }

    let pretty = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    match reason {
        Some(reason) if reason.len() >= 160 => reason.to_string(),
        Some(reason) => {
            format!("{reason}\n\nAdditional structured summary from Sleep input:\n{pretty}")
        }
        None => format!("Sleep requested with structured summary:\n{pretty}"),
    }
}

pub(crate) fn extract_sleep_duration_ms(input: &Value) -> Result<Option<u64>> {
    let object = match input {
        Value::Object(map) => map,
        _ => return Ok(None),
    };
    let Some(duration) = object.get("duration_ms") else {
        return Ok(None);
    };
    if duration.is_null() {
        return Ok(None);
    }
    let Some(duration_ms) = duration.as_u64() else {
        return Err(invalid_tool_input(
            "Sleep",
            "Sleep `duration_ms` must be an integer when provided",
            json!({
                "field": "duration_ms",
                "validation_error": "expected integer",
            }),
            "omit `duration_ms` for ordinary terminal rest, or provide a positive integer millisecond delay",
        ));
    };
    if duration_ms == 0 {
        return Ok(None);
    }
    Ok(Some(duration_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_budget_defaults_to_command_tool_default() {
        assert_eq!(output_char_budget(None), 8_000);
    }

    #[test]
    fn effective_tool_output_tokens_defaults_and_clamps() {
        assert_eq!(effective_tool_output_tokens(None, 2_000, 10_000), 2_000);
        assert_eq!(effective_tool_output_tokens(Some(0), 2_000, 10_000), 2_000);
        assert_eq!(
            effective_tool_output_tokens(Some(50_000), 2_000, 10_000),
            10_000
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
    fn sleep_reason_uses_plain_reason_when_input_is_simple() {
        let reason = extract_sleep_reason(&json!({ "reason": "done with verification" }));
        assert_eq!(reason, "done with verification");
    }

    #[test]
    fn sleep_reason_preserves_structured_payload_when_input_is_malformed() {
        let reason = extract_sleep_reason(&json!({
            "reason": "## Analysis Complete",
            "Recommendation": "Add a stronger final delivery contract in src/runtime.rs.",
            "Citations": "docs/benchmark-results.md and src/prompt.rs"
        }));
        assert!(reason.contains("## Analysis Complete"));
        assert!(reason.contains("Additional structured summary from Sleep input"));
        assert!(reason.contains("Recommendation"));
        assert!(reason.contains("src/runtime.rs"));
    }

    #[test]
    fn sleep_reason_ignores_supported_duration_field() {
        let reason = extract_sleep_reason(&json!({
            "reason": "wait briefly for a filesystem settle",
            "duration_ms": 250,
        }));
        assert_eq!(reason, "wait briefly for a filesystem settle");
    }

    #[test]
    fn sleep_duration_reads_positive_integer_delay() {
        let duration_ms = extract_sleep_duration_ms(&json!({
            "reason": "pause",
            "duration_ms": 250,
        }))
        .unwrap();
        assert_eq!(duration_ms, Some(250));
    }

    #[test]
    fn sleep_duration_treats_null_as_omitted() {
        let duration_ms = extract_sleep_duration_ms(&json!({
            "reason": "pause",
            "duration_ms": null,
        }))
        .unwrap();
        assert_eq!(duration_ms, None);
    }

    #[test]
    fn sleep_duration_treats_zero_as_omitted() {
        let duration_ms = extract_sleep_duration_ms(&json!({
            "reason": "pause",
            "duration_ms": 0,
        }))
        .unwrap();
        assert_eq!(duration_ms, None);
    }
}
