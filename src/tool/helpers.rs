//! Helper functions for tool implementation
//!
//! This module contains shared utility functions used by tool implementations.

use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use serde_json::json;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::tool::ToolError;

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
        .unwrap_or(12_000)
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
        return Err(invalid_tool_input(
            "Sleep",
            "Sleep `duration_ms` must be greater than zero",
            json!({
                "field": "duration_ms",
                "validation_error": "must be greater than zero",
            }),
            "provide a positive integer millisecond delay, or omit `duration_ms` for ordinary terminal rest",
        ));
    }
    Ok(Some(duration_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn sleep_duration_rejects_zero_delay() {
        let err = extract_sleep_duration_ms(&json!({
            "reason": "pause",
            "duration_ms": 0,
        }))
        .unwrap_err();
        let tool_error = err
            .downcast_ref::<crate::tool::ToolError>()
            .expect("tool error");
        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert!(tool_error.message.contains("greater than zero"));
    }
}
