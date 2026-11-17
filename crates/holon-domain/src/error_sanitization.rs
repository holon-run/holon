//! Redaction of error text exposed to operators.

const ERROR_TEXT_MAX_CHARS: usize = 512;

pub fn sanitize_runtime_error_text(raw: &str) -> String {
    let trimmed = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if trimmed.is_empty() {
        return String::new();
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("authorization:")
        || lower.contains("bearer ")
        || lower.contains("api_key")
        || lower.contains("api-key")
        || lower.contains("access_token")
        || lower.contains("access-token")
        || lower.contains("x-api-key")
        || lower.contains("capability_secret")
        || lower.contains("/api/callbacks/wake/")
        || lower.contains("/api/callbacks/enqueue/")
        || lower.contains("/callbacks/wake/")
        || lower.contains("/callbacks/enqueue/")
    {
        return "<redacted-sensitive-error-context>".into();
    }

    let public_prefix = trimmed
        .find('{')
        .map(|index| {
            let prefix = trimmed[..index].trim_end_matches([' ', ':']);
            if prefix.is_empty() {
                "<redacted-structured-error-context>".to_string()
            } else {
                format!("{prefix}: <redacted-structured-error-context>")
            }
        })
        .unwrap_or_else(|| trimmed.to_string());
    let sanitized = public_prefix
        .split_whitespace()
        .map(sanitize_error_token)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&sanitized, ERROR_TEXT_MAX_CHARS)
}

fn sanitize_error_token(raw: &str) -> String {
    let (prefix, token, suffix) = split_token_punctuation(raw);
    let sanitized = if let Ok(mut url) = url::Url::parse(token) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    } else if std::path::Path::new(token).is_absolute() {
        std::path::Path::new(token)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("<redacted-path>/{name}"))
            .unwrap_or_else(|| "<redacted-path>".into())
    } else {
        token.to_string()
    };
    format!("{prefix}{sanitized}{suffix}")
}

fn split_token_punctuation(raw: &str) -> (&str, &str, &str) {
    let start = raw
        .char_indices()
        .find(|(_, ch)| !matches!(ch, '(' | '[' | '{' | '"' | '\''))
        .map(|(index, _)| index)
        .unwrap_or(raw.len());
    let end = raw
        .char_indices()
        .rev()
        .find(|(_, ch)| !matches!(ch, ')' | ']' | '}' | ',' | ';' | '"' | '\''))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(start);
    (&raw[..start], &raw[start..end], &raw[end..])
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}
