//! Helper functions for tool implementation
//!
//! This module contains shared utility functions used by tool implementations.

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::future::Future;
use std::path::{Path, PathBuf};

use crate::tool::{spec::ToolInputCoercion, ToolError};
use crate::types::CommandCostDiagnostics;

pub(crate) const DEFAULT_TOOL_OUTPUT_TOKENS: u64 = 8_000;
pub(crate) const MAX_TOOL_OUTPUT_TOKENS: u64 = 64_000;
pub(crate) const COMMAND_COST_SOFT_THRESHOLD_CHARS: usize = 4_000;
pub(crate) const COMMAND_PREVIEW_CHARS: usize = 240;
const TOOL_INPUT_ENVELOPE_KEYS: [&str; 4] = ["arguments", "parameters", "params", "input"];
const MAX_STRING_JSON_PARSE_ERRORS: usize = 8;
const MAX_STRING_JSON_PARSE_ERROR_CHARS: usize = 240;

tokio::task_local! {
    static TOOL_INPUT_COERCION: RefCell<Option<ToolInputCoercion>>;
}

struct CoercionOutcome {
    value: Option<Value>,
    string_json_parse_errors: Vec<Value>,
}

pub(crate) async fn capture_tool_input_coercion<F>(
    future: F,
) -> (F::Output, Option<ToolInputCoercion>)
where
    F: Future,
{
    TOOL_INPUT_COERCION
        .scope(RefCell::new(None), async move {
            let output = future.await;
            let input_coercion = TOOL_INPUT_COERCION.with(|coercion| coercion.borrow().clone());
            (output, input_coercion)
        })
        .await
}

fn record_tool_input_coercion(input_coercion: ToolInputCoercion) {
    let _ = TOOL_INPUT_COERCION.try_with(|recorded| {
        let mut recorded = recorded.borrow_mut();
        if recorded.is_none() {
            *recorded = Some(input_coercion);
        }
    });
}

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
    let coercion = coerce_string_scalars_with_diagnostics(input);
    let input = coercion.value.as_ref().unwrap_or(input);
    let first_error = match serde_json::from_value(input.clone()) {
        Ok(args) => return Ok(args),
        Err(error) => error,
    };

    let mut parse_error = first_error.to_string();
    if !coercion.string_json_parse_errors.is_empty() {
        parse_error = redact_serde_string_value(&parse_error);
    }
    if let Some(unknown_field) = unknown_field_name(&parse_error) {
        if TOOL_INPUT_ENVELOPE_KEYS.contains(&unknown_field) {
            if let Some((candidate, coercion)) = recover_tool_input_envelope_from_normalized(input)
            {
                let ToolInputCoercion::UnwrapToolInputEnvelope { envelope_key, .. } = &coercion;
                if envelope_key == unknown_field {
                    if let Ok(args) = serde_json::from_value(candidate) {
                        record_tool_input_coercion(coercion);
                        return Ok(args);
                    }
                }
            }
        }
    }

    let mut details = json!({
        "tool_name": tool_name,
        "parse_error": parse_error,
    });
    if !coercion.string_json_parse_errors.is_empty() {
        details
            .as_object_mut()
            .expect("tool error details should be an object")
            .insert(
                "string_json_parse_errors".into(),
                Value::Array(coercion.string_json_parse_errors),
            );
    }
    let default_hint = recovery_hint();
    let hint = unknown_field_recovery_hint(&parse_error)
        .map(|specific| format!("{specific}; {default_hint}"))
        .unwrap_or(default_hint);

    Err(anyhow::Error::from(
        ToolError::new(
            "invalid_tool_input",
            format!("input for {tool_name} does not match the tool schema"),
        )
        .with_details(details)
        .with_recovery_hint(hint),
    ))
}

/// Recursively coerces string scalars to their typed JSON equivalents so that
/// minor LLM mistakes (e.g. `"42"` instead of `42`, `"true"` instead of `true`)
/// do not cause hard schema failures during `serde_json::from_value`.
///
/// Only pure numeric strings and the exact strings `"true"` / `"false"` are
/// converted. Mixed strings like `"10px"` or `"hello"` are left untouched.
/// Returns `None` when no changes were made so the caller can avoid a
/// needless clone.
#[cfg(test)]
fn coerce_string_scalars(value: &Value) -> Option<Value> {
    coerce_string_scalars_with_diagnostics(value).value
}

fn coerce_string_scalars_with_diagnostics(value: &Value) -> CoercionOutcome {
    let mut string_json_parse_errors = Vec::new();
    let value = coerce_value(value, "", &mut string_json_parse_errors);
    CoercionOutcome {
        value,
        string_json_parse_errors,
    }
}

fn coerce_value(
    value: &Value,
    path: &str,
    string_json_parse_errors: &mut Vec<Value>,
) -> Option<Value> {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            let mut new_map = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                let child_path = json_pointer_child(path, k);
                match coerce_value(v, &child_path, string_json_parse_errors) {
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
            for (index, v) in arr.iter().enumerate() {
                let child_path = json_pointer_child(path, &index.to_string());
                match coerce_value(v, &child_path, string_json_parse_errors) {
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
        Value::String(s) => coerce_string_at_path(s, path, string_json_parse_errors),
        _ => None,
    }
}

#[cfg(test)]
fn coerce_string(s: &str) -> Option<Value> {
    coerce_string_at_path(s, "", &mut Vec::new())
}

fn coerce_string_at_path(
    s: &str,
    path: &str,
    string_json_parse_errors: &mut Vec<Value>,
) -> Option<Value> {
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
            if let Some(i) = parse_integral_decimal_i64(s) {
                return Some(Value::Number(i.into()));
            }
            return Some(Value::Number(serde_json::Number::from_f64(f)?));
        }
    }

    // If the string looks like a JSON array or object, try parsing it.
    // This recovers the common LLM mistake of serializing a structured
    // field as a JSON string instead of inline JSON.
    let trimmed = s.trim_start();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        match serde_json::from_str::<Value>(s) {
            Ok(parsed) if parsed.is_array() || parsed.is_object() => {
                return Some(
                    coerce_value(&parsed, path, string_json_parse_errors).unwrap_or(parsed),
                );
            }
            Err(error) => {
                if string_json_parse_errors.len() < MAX_STRING_JSON_PARSE_ERRORS {
                    string_json_parse_errors.push(json!({
                        "path": if path.is_empty() { "/" } else { path },
                        "error": truncate_text(
                            &error.to_string(),
                            MAX_STRING_JSON_PARSE_ERROR_CHARS,
                        ),
                        "line": error.line(),
                        "column": error.column(),
                    }));
                }
            }
            _ => {}
        }
    }
    None
}

fn json_pointer_child(path: &str, component: &str) -> String {
    let escaped = component.replace('~', "~0").replace('/', "~1");
    format!("{path}/{escaped}")
}

fn redact_serde_string_value(parse_error: &str) -> String {
    for prefix in ["invalid type: string ", "invalid value: string "] {
        let Some(rest) = parse_error.strip_prefix(prefix) else {
            continue;
        };
        let Some((_, expected)) = rest.rsplit_once(", expected ") else {
            continue;
        };
        return format!("{}, expected {expected}", prefix.trim_end());
    }
    parse_error.to_owned()
}

fn recover_tool_input_envelope_from_normalized(
    input: &Value,
) -> Option<(Value, ToolInputCoercion)> {
    let outer = input.as_object()?;
    let envelope_keys = TOOL_INPUT_ENVELOPE_KEYS
        .iter()
        .filter(|key| outer.contains_key(**key))
        .copied()
        .collect::<Vec<_>>();
    let [envelope_key] = envelope_keys.as_slice() else {
        return None;
    };
    let inner = outer.get(*envelope_key)?.as_object()?;
    let mut merged = inner.clone();
    for (key, value) in outer {
        if key != envelope_key {
            merged.insert(key.clone(), value.clone());
        }
    }
    let coercion = ToolInputCoercion::UnwrapToolInputEnvelope {
        envelope_key: (*envelope_key).to_string(),
        outer_keys: outer.keys().cloned().collect(),
        inner_keys: inner.keys().cloned().collect(),
    };
    Some((Value::Object(merged), coercion))
}

fn unknown_field_recovery_hint(parse_error: &str) -> Option<String> {
    let field = unknown_field_name(parse_error)?;
    let rest = parse_error.strip_prefix("unknown field `")?;
    let field_end = rest.find('`')?;
    let expected = rest[field_end + 1..]
        .split_once(", expected ")
        .map(|(_, expected)| backtick_values(expected))
        .unwrap_or_default();
    let accepted = if expected.is_empty() {
        String::new()
    } else {
        format!("; accepted top-level fields: {}", expected.join(", "))
    };
    if TOOL_INPUT_ENVELOPE_KEYS.contains(&field) {
        Some(format!(
            "remove the top-level `{field}` envelope and place its object fields at the top level{accepted}"
        ))
    } else {
        Some(format!(
            "remove unsupported top-level field `{field}`{accepted}"
        ))
    }
}

fn unknown_field_name(parse_error: &str) -> Option<&str> {
    let rest = parse_error.strip_prefix("unknown field `")?;
    Some(&rest[..rest.find('`')?])
}

fn backtick_values(value: &str) -> Vec<String> {
    let mut remaining = value;
    let mut values = Vec::new();
    while let Some((_, after_open)) = remaining.split_once('`') {
        let Some((value, after_close)) = after_open.split_once('`') else {
            break;
        };
        values.push(format!("`{value}`"));
        remaining = after_close;
    }
    values
}

fn parse_integral_decimal_i64(s: &str) -> Option<i64> {
    let (negative, unsigned) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let exponent = unsigned.get(index + 1..)?.parse::<i64>().ok()?;
            (&unsigned[..index], exponent)
        }
        None => (unsigned, 0),
    };
    let (integer, fractional) = match mantissa.split_once('.') {
        Some((integer, fractional)) if !fractional.contains('.') => (integer, fractional),
        Some(_) => return None,
        None => (mantissa, ""),
    };
    if integer.is_empty() && fractional.is_empty() {
        return None;
    }
    if !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let mut digits = String::with_capacity(integer.len() + fractional.len());
    digits.push_str(integer);
    digits.push_str(fractional);
    if digits.bytes().all(|byte| byte == b'0') {
        return Some(0);
    }

    let significant_start = digits.bytes().position(|byte| byte != b'0')?;
    let mut normalized = digits[significant_start..].to_string();
    let fractional_len = i64::try_from(fractional.len()).ok()?;
    let decimal_shift = exponent.checked_sub(fractional_len)?;
    if decimal_shift >= 0 {
        let trailing_zeros = usize::try_from(decimal_shift).ok()?;
        if normalized.len().checked_add(trailing_zeros)? > 19 {
            return None;
        }
        normalized.extend(std::iter::repeat_n('0', trailing_zeros));
    } else {
        let removed_digits = usize::try_from(decimal_shift.unsigned_abs()).ok()?;
        if removed_digits >= normalized.len() {
            return None;
        }
        let integer_len = normalized.len() - removed_digits;
        if !normalized.as_bytes()[integer_len..]
            .iter()
            .all(|byte| *byte == b'0')
        {
            return None;
        }
        normalized.truncate(integer_len);
    }

    if negative {
        normalized.insert(0, '-');
    }
    normalized.parse::<i64>().ok()
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

    #[derive(Debug, serde::Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct EnvelopeArgs {
        cmd: String,
        #[serde(default)]
        max_output_tokens: Option<u64>,
        #[serde(default)]
        workdir: Option<String>,
    }

    #[test]
    fn parse_tool_args_recovers_known_object_and_string_envelopes() {
        for envelope_key in TOOL_INPUT_ENVELOPE_KEYS {
            let direct = json!({
                envelope_key: {"cmd": "printf ok"},
                "max_output_tokens": "800"
            });
            let parsed: EnvelopeArgs = parse_tool_args("TestTool", &direct).unwrap();
            assert_eq!(
                parsed,
                EnvelopeArgs {
                    cmd: "printf ok".into(),
                    max_output_tokens: Some(800),
                    workdir: None,
                }
            );

            let string = json!({
                envelope_key: "{\"cmd\":\"printf ok\",\"max_output_tokens\":\"900\"}"
            });
            let parsed: EnvelopeArgs = parse_tool_args("TestTool", &string).unwrap();
            assert_eq!(
                parsed,
                EnvelopeArgs {
                    cmd: "printf ok".into(),
                    max_output_tokens: Some(900),
                    workdir: None,
                }
            );
        }
    }

    #[test]
    fn parse_tool_args_prefers_explicit_outer_fields_when_unwrapping() {
        let parsed: EnvelopeArgs = parse_tool_args(
            "TestTool",
            &json!({
                "arguments": {
                    "cmd": "inner",
                    "workdir": "inner-dir"
                },
                "cmd": "outer",
                "max_output_tokens": "800"
            }),
        )
        .unwrap();

        assert_eq!(parsed.cmd, "outer");
        assert_eq!(parsed.workdir.as_deref(), Some("inner-dir"));
        assert_eq!(parsed.max_output_tokens, Some(800));
    }

    #[test]
    fn parse_tool_args_does_not_recover_multiple_or_non_object_envelopes() {
        for input in [
            json!({
                "arguments": {"cmd": "printf arguments"},
                "params": {"cmd": "printf params"}
            }),
            json!({"arguments": ["printf", "ok"]}),
            json!({"arguments": "printf ok"}),
        ] {
            let error = parse_tool_args::<EnvelopeArgs>("TestTool", &input).unwrap_err();
            let error = error.downcast_ref::<ToolError>().expect("tool error");
            assert_eq!(error.kind, "invalid_tool_input");
        }
    }

    #[test]
    fn parse_tool_args_does_not_unwrap_a_recognized_object_field() {
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegitimateInputArgs {
            #[allow(dead_code)]
            input: Value,
            #[allow(dead_code)]
            required: String,
        }

        let error = parse_tool_args::<LegitimateInputArgs>(
            "TestTool",
            &json!({"input": {"required": "must stay nested"}}),
        )
        .unwrap_err();
        let error = error.downcast_ref::<ToolError>().expect("tool error");

        assert!(error
            .details
            .as_ref()
            .and_then(|details| details["parse_error"].as_str())
            .is_some_and(|message| message.contains("missing field `required`")));
    }

    #[tokio::test]
    async fn failed_envelope_candidate_preserves_first_parse_error_and_hint() {
        let input = json!({"arguments": {"workdir": "."}});
        let (result, input_coercion) = capture_tool_input_coercion(async {
            parse_tool_args::<EnvelopeArgs>("TestTool", &input)
        })
        .await;
        let error = result.unwrap_err();
        let error = error.downcast_ref::<ToolError>().expect("tool error");
        let details = error.details.as_ref().expect("details");

        assert_eq!(input_coercion, None);
        assert!(details["parse_error"]
            .as_str()
            .expect("parse error")
            .contains("unknown field `arguments`"));
        let hint = error.recovery_hint.as_deref().expect("recovery hint");
        assert!(hint.contains("remove the top-level `arguments` envelope"));
        assert!(hint.contains("accepted top-level fields"));
    }

    #[test]
    fn unknown_field_hint_names_unsupported_and_accepted_fields() {
        let error = parse_tool_args::<EnvelopeArgs>(
            "TestTool",
            &json!({"cmd": "printf ok", "work_dir": "."}),
        )
        .unwrap_err();
        let error = error.downcast_ref::<ToolError>().expect("tool error");
        let hint = error.recovery_hint.as_deref().expect("recovery hint");

        assert!(hint.contains("remove unsupported top-level field `work_dir`"));
        assert!(hint.contains("`cmd`"));
        assert!(hint.contains("`workdir`"));
    }

    #[test]
    fn invalid_structured_json_reports_bounded_path_and_location_without_value() {
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StructuredArgs {
            #[allow(dead_code)]
            items: Vec<String>,
        }

        let error = parse_tool_args::<StructuredArgs>(
            "TestTool",
            &json!({"items": "[\"SECRET_MARKER\",]"}),
        )
        .unwrap_err();
        let error = error.downcast_ref::<ToolError>().expect("tool error");
        let details = error.details.as_ref().expect("details");
        let diagnostics = details["string_json_parse_errors"]
            .as_array()
            .expect("string JSON diagnostics");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["path"], "/items");
        assert!(diagnostics[0]["line"].as_u64().unwrap() >= 1);
        assert!(diagnostics[0]["column"].as_u64().unwrap() >= 1);
        assert!(!details.to_string().contains("SECRET_MARKER"));
    }

    #[tokio::test]
    async fn input_coercion_capture_records_actual_recovery_without_values() {
        let input = json!({
            "arguments": "{\"cmd\":\"SECRET_MARKER\"}",
            "max_output_tokens": "800"
        });
        let (parsed, coercion) = capture_tool_input_coercion(async {
            parse_tool_args::<EnvelopeArgs>("TestTool", &input)
        })
        .await;
        assert_eq!(parsed.unwrap().cmd, "SECRET_MARKER");
        let coercion = coercion.expect("input coercion");
        let value = serde_json::to_value(coercion).unwrap();

        assert_eq!(value["kind"], "unwrap_tool_input_envelope");
        assert_eq!(value["envelope_key"], "arguments");
        assert_eq!(
            value["outer_keys"],
            json!(["arguments", "max_output_tokens"])
        );
        assert_eq!(value["inner_keys"], json!(["cmd"]));
        assert!(!value.to_string().contains("SECRET_MARKER"));
    }

    #[tokio::test]
    async fn input_coercion_capture_does_not_infer_from_directly_accepted_input() {
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegitimateInputArgs {
            input: Value,
        }

        let input = json!({"input": {"value": "nested"}});
        let (parsed, input_coercion) = capture_tool_input_coercion(async {
            parse_tool_args::<LegitimateInputArgs>("TestTool", &input)
        })
        .await;

        assert_eq!(parsed.unwrap().input, json!({"value": "nested"}));
        assert_eq!(input_coercion, None);
    }

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
        let result = coerce_string("4.567");
        assert_eq!(
            result,
            Some(Value::Number(serde_json::Number::from_f64(4.567).unwrap()))
        );
    }

    #[test]
    fn coerce_integral_decimal_strings_to_integer() {
        assert_eq!(
            coerce_string("900000.0"),
            Some(Value::Number(900000i64.into()))
        );
        assert_eq!(coerce_string("-3.0"), Some(Value::Number((-3i64).into())));
        assert_eq!(coerce_string("1e3"), Some(Value::Number(1000i64.into())));
        assert_eq!(coerce_string("1.23e3"), Some(Value::Number(1230i64.into())));
        assert_eq!(
            coerce_string("9007199254740993.0"),
            Some(Value::Number(9007199254740993i64.into()))
        );
        assert_eq!(
            coerce_string("9223372036854775807.0"),
            Some(Value::Number(i64::MAX.into()))
        );
        assert_eq!(
            coerce_string("-9223372036854775808.0"),
            Some(Value::Number(i64::MIN.into()))
        );
    }

    #[test]
    fn coerce_non_integral_decimal_string_stays_float() {
        let value = 1e-1f64;
        assert_eq!(
            coerce_string("1e-1"),
            Some(Value::Number(serde_json::Number::from_f64(value).unwrap()))
        );
    }

    #[test]
    fn coerce_out_of_range_integral_decimal_string_stays_float() {
        let value = 9223372036854775808.0f64;
        assert_eq!(
            coerce_string("9223372036854775808.0"),
            Some(Value::Number(serde_json::Number::from_f64(value).unwrap()))
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
                "ratio": "4.567",
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
                    "ratio": 4.567,
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

    #[test]
    fn coerce_string_json_array() {
        let s = r#"[{"text":"do something","state":"pending"}]"#;
        let result = coerce_string(s).expect("should parse JSON array");
        assert_eq!(
            result,
            json!([{"text": "do something", "state": "pending"}])
        );
    }

    #[test]
    fn coerce_string_json_object() {
        let s = r#"{"key":"value","num":42}"#;
        let result = coerce_string(s).expect("should parse JSON object");
        assert_eq!(result, json!({"key": "value", "num": 42}));
    }

    #[test]
    fn coerce_string_invalid_json_left_untouched() {
        // Starts with [ but is not valid JSON — should return None.
        assert_eq!(coerce_string("[invalid"), None);
        assert_eq!(coerce_string("{not json"), None);
    }

    #[test]
    fn coerce_string_plain_text_with_brace_untouched() {
        // Text that merely starts with { or [ but is not JSON should be preserved.
        assert_eq!(coerce_string("{placeholder}"), None);
        assert_eq!(coerce_string("[link]"), None);
    }

    #[test]
    fn coerce_string_scalars_json_string_field() {
        // Simulates the LLM mistake: todo_list serialized as a JSON string.
        let input = json!({
            "todo_list": "[{\"text\":\"step 1\",\"state\":\"pending\"}]"
        });
        let result = coerce_string_scalars(&input).expect("should have coerced");
        assert_eq!(
            result,
            json!({"todo_list": [{"text": "step 1", "state": "pending"}]})
        );
    }
}
