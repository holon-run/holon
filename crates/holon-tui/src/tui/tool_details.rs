//! Bounded, read-only tool details from the persisted execution DTO.

use serde_json::Value;

use crate::types::{ToolExecutionRecord, ToolExecutionStatus};

const PREVIEW_LINES: usize = 20;
const PREVIEW_BYTES: usize = 4096;
const EXPANDED_LINES: usize = 200;
const EXPANDED_BYTES: usize = 32_768;
const TRUNCATED: &str = "[details truncated]";

pub(super) fn structured_result_text(value: &Value) -> String {
    let mut text = DetailText::new(true, 0);
    text.value("result", value, 0);
    text.finish("")
}

pub(super) fn tool_detail_text(record: &ToolExecutionRecord, expanded: bool) -> String {
    let result = result_value(&record.output);
    let artifacts = artifact_footer(&record.output, expanded);
    let mut text = DetailText::new(expanded, artifacts.len());
    let status = match record.status {
        ToolExecutionStatus::Deferred => "deferred",
        ToolExecutionStatus::Success => "success",
        ToolExecutionStatus::Error => "error",
        ToolExecutionStatus::Interrupted => "interrupted",
        ToolExecutionStatus::Pending => "pending",
    };
    text.field(&record.tool_name, &format!("tool {status}"));
    text.field("duration", &format!("{}ms", record.duration_ms));
    if let Some(error) = tool_error(&record.output) {
        text.value("error", error, 0);
    }

    match record.tool_name.as_str() {
        "ExecCommand" => command(&mut text, &record.input, result, None, None),
        "ExecCommandBatch" => command_batch(&mut text, &record.input, result),
        "ApplyPatch" => patch(&mut text, &record.input, result),
        _ => {
            text.value("input", &record.input, 0);
            text.value("output", result, 0);
        }
    }
    text.finish(&artifacts)
}

fn result_value(output: &Value) -> &Value {
    if let Some(envelope) = output.get("envelope") {
        envelope.get("result").unwrap_or(&Value::Null)
    } else {
        output.get("result").unwrap_or(output)
    }
}

fn tool_error(output: &Value) -> Option<&Value> {
    output
        .pointer("/envelope/error")
        .filter(|value| !value.is_null())
        .or_else(|| output.get("tool_error").filter(|value| !value.is_null()))
        .or_else(|| output.get("error").filter(|value| !value.is_null()))
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn command(
    text: &mut DetailText,
    input: &Value,
    result: &Value,
    inherited_cwd: Option<&Value>,
    supplied_cmd: Option<&str>,
) {
    let snapshot = result.get("command").unwrap_or(&Value::Null);
    let cmd = supplied_cmd
        .or_else(|| string(input, "cmd"))
        .or_else(|| string(snapshot, "cmd"));
    text.field("cmd", cmd.unwrap_or("(not recorded)"));
    let cwd = input
        .get("workdir")
        .filter(|value| !value.is_null())
        .or(inherited_cwd)
        .or_else(|| snapshot.get("workdir"));
    text.field(
        "cwd",
        cwd.and_then(Value::as_str)
            .unwrap_or("(default; not recorded)"),
    );

    let disposition = string(result, "disposition");
    let pending = matches!(disposition, Some("promoted_to_task" | "already_running"));
    match disposition {
        Some("promoted_to_task") => {
            text.line("command: background task started; completion pending")
        }
        Some("already_running") => text.line("command: already running; completion pending"),
        Some("completed") => text.line("command: completed"),
        Some(other) => text.field("command disposition", other),
        None => text.line("command: completion unknown"),
    }
    if let Some(handle) = result.get("task_handle") {
        if let Some(id) = string(handle, "task_id") {
            text.field("task", id);
        }
        if let Some(status) = string(handle, "status") {
            text.field("task status", status);
        }
    }
    if pending {
        text.line("exit: pending");
    } else if let Some(exit) = result.get("exit_status").and_then(Value::as_i64) {
        text.field("exit", &exit.to_string());
    } else {
        text.line("exit: unknown (not reported)");
    }
    if let Some(error) = tool_error(result) {
        text.value("error", error, 0);
    }
    if result.get("truncated").and_then(Value::as_bool) == Some(true)
        || result
            .get("initial_output_truncated")
            .and_then(Value::as_bool)
            == Some(true)
    {
        text.line("[command output truncated upstream]");
    }
    if pending {
        stream(text, result, "initial_output_preview", "initial output");
    } else {
        stream(text, result, "stdout_preview", "stdout");
        stream(text, result, "stderr_preview", "stderr");
    }
}

fn stream(text: &mut DetailText, result: &Value, key: &str, label: &str) {
    match result.get(key).and_then(Value::as_str) {
        Some("") => text.field(label, "(empty)"),
        Some(value) => text.field(label, value),
        None => text.field(label, "(not captured)"),
    }
}

fn command_batch(text: &mut DetailText, input: &Value, result: &Value) {
    let count = |key| {
        result
            .get(key)
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into())
    };
    text.line(&format!(
        "items: {}; completed: {}; failed: {}; rejected: {}; skipped: {}",
        count("item_count"),
        count("completed_count"),
        count("failed_count"),
        count("rejected_count"),
        count("skipped_count")
    ));
    let Some(items) = result.get("items").and_then(Value::as_array) else {
        text.value("input", input, 0);
        text.value("output", result, 0);
        return;
    };
    // Summarize every item before showing streams; a noisy success must not
    // conceal a later failed/rejected item.
    for item in items {
        let index = item.get("index").and_then(Value::as_u64).unwrap_or(0);
        let status = string(item, "status").unwrap_or("unknown");
        let exit = item
            .pointer("/result/exit_status")
            .and_then(Value::as_i64)
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".into());
        let cmd = string(item, "cmd").unwrap_or("(not recorded)");
        let first_line = cmd.split('\n').next().unwrap_or("");
        let (preview, clipped) = prefix(first_line, 120);
        text.field(
            &format!("[{index}] {status}; exit {exit}"),
            &format!(
                "{preview}{}",
                if clipped || first_line.len() < cmd.len() {
                    " …"
                } else {
                    ""
                }
            ),
        );
        if text.full() {
            return;
        }
    }
    for failed in [true, false] {
        for (offset, item) in items.iter().enumerate() {
            let status = string(item, "status").unwrap_or("unknown");
            if (status != "completed") != failed {
                continue;
            }
            let index = item.get("index").and_then(Value::as_u64).unwrap_or(0);
            text.field("item", &format!("[{index}] {status}"));
            if let Some(kind) = string(item, "error_kind") {
                text.field("error kind", kind);
            }
            if let Some(message) = string(item, "error_message") {
                text.field("error", message);
            }
            let item_input = input
                .get("items")
                .and_then(|value| value.get(offset))
                .unwrap_or(&Value::Null);
            if let Some(item_result) = item.get("result").filter(|value| !value.is_null()) {
                command(
                    text,
                    item_input,
                    item_result,
                    input.get("workdir").filter(|value| !value.is_null()),
                    string(item, "cmd"),
                );
            } else if matches!(status, "failed" | "rejected") {
                text.line("command result: not available");
            }
            if text.full() {
                return;
            }
        }
    }
}

fn patch(text: &mut DetailText, input: &Value, result: &Value) {
    if let Some(count) = result.get("changed_file_count").and_then(Value::as_u64) {
        text.field("changed files", &count.to_string());
    }
    let files = result.get("changed_files").and_then(Value::as_array);
    if let Some(files) = files.filter(|files| !files.is_empty()) {
        for file in files {
            let action = match string(file, "action") {
                Some("add") => "A",
                Some("modify") => "M",
                Some("delete") => "D",
                Some("move") => "R",
                _ => "?",
            };
            if let Some(from) = string(file, "from_path") {
                text.field(&format!("{action} from"), from);
            }
            text.field(action, string(file, "path").unwrap_or("(unknown path)"));
            if let Some(hunks) = file.get("hunks").and_then(Value::as_array) {
                let added: u64 = hunks
                    .iter()
                    .filter_map(|hunk| hunk.get("added_count").and_then(Value::as_u64))
                    .sum();
                let removed: u64 = hunks
                    .iter()
                    .filter_map(|hunk| hunk.get("removed_count").and_then(Value::as_u64))
                    .sum();
                text.field("lines", &format!("+{added} -{removed}"));
            }
            if text.full() {
                return;
            }
        }
    } else if let Some(paths) = result.get("changed_paths") {
        text.value("changed paths", paths, 0);
    }
    for key in ["diagnostics", "ignored_metadata"] {
        if let Some(value) = result
            .get(key)
            .filter(|value| value.as_array().is_some_and(|values| !values.is_empty()))
        {
            text.value(key, value, 0);
        }
    }
    if let Some(files) = files {
        for file in files {
            if let Some(diff) = string(file, "diff_preview") {
                text.field("diff", string(file, "path").unwrap_or("(unknown path)"));
                text.limited_lines(diff, if text.expanded { 80 } else { 8 });
            } else if let Some(hunks) = file
                .get("hunks")
                .filter(|value| value.as_array().is_some_and(|values| !values.is_empty()))
            {
                text.value("hunks", hunks, 0);
            }
            if file.get("diff_truncated").and_then(Value::as_bool) == Some(true) {
                text.line("[diff truncated upstream]");
            }
            if text.full() {
                return;
            }
        }
    } else {
        text.value("input patch", input, 0);
        text.value("output", result, 0);
    }
}

struct DetailText {
    body: String,
    lines: usize,
    max_lines: usize,
    max_bytes: usize,
    nodes: usize,
    truncated: bool,
    expanded: bool,
}

impl DetailText {
    fn new(expanded: bool, artifact_bytes: usize) -> Self {
        let (lines, bytes) = if expanded {
            (EXPANDED_LINES, EXPANDED_BYTES)
        } else {
            (PREVIEW_LINES, PREVIEW_BYTES)
        };
        Self {
            body: String::new(),
            lines: 0,
            // Reserve room for the truncation notice and artifact footer.
            max_lines: lines - 2,
            max_bytes: bytes - artifact_bytes - TRUNCATED.len() - 2,
            nodes: 0,
            truncated: false,
            expanded,
        }
    }

    fn full(&self) -> bool {
        self.lines >= self.max_lines || self.body.len() >= self.max_bytes
    }

    fn line(&mut self, value: &str) {
        if self.full() {
            self.truncated = true;
            return;
        }
        let (value, clipped) = prefix(value, self.max_bytes);
        let safe = safe_text(value);
        let separator = usize::from(!self.body.is_empty());
        let available = self.max_bytes.saturating_sub(self.body.len() + separator);
        let (safe, byte_clipped) = prefix(&safe, available);
        if separator != 0 {
            self.body.push('\n');
        }
        self.body.push_str(safe);
        self.lines += 1;
        self.truncated |= clipped || byte_clipped;
    }

    fn field(&mut self, label: &str, value: &str) {
        let (label, label_clipped) = prefix(label, 256);
        // A shell continuation or multiline header can put the secret on the
        // next line, so hide the bounded field rather than only its marker.
        if sensitive_text(prefix(value, self.max_bytes).0) {
            self.line(&format!("{label}: [redacted sensitive text]"));
            self.truncated |= label_clipped;
            return;
        }
        let mut lines = value.split('\n').peekable();
        let first = lines.next().unwrap_or("");
        let (first, first_clipped) = prefix(first, self.max_bytes);
        self.line(&format!("{label}: {first}"));
        self.truncated |= label_clipped || first_clipped;
        while let Some(line) = lines.next() {
            self.line(line);
            if self.full() {
                self.truncated |= lines.peek().is_some();
                break;
            }
        }
    }

    fn limited_lines(&mut self, value: &str, limit: usize) {
        if sensitive_text(prefix(value, self.max_bytes).0) {
            self.line("[redacted sensitive diff]");
            return;
        }
        for (index, line) in value.split('\n').enumerate() {
            if index >= limit {
                self.line("[diff preview truncated]");
                return;
            }
            self.line(line);
            if self.full() {
                self.truncated = true;
                return;
            }
        }
    }

    fn value(&mut self, label: &str, value: &Value, depth: usize) {
        self.nodes += 1;
        if depth > 6 || self.nodes > 256 {
            self.line("[structured details truncated]");
            return;
        }
        if hidden_key(label) {
            return;
        }
        if sensitive_key(label) {
            self.field("sensitive field", "[redacted]");
            return;
        }
        if self.full() {
            self.truncated = true;
            return;
        }
        match value {
            Value::Null => self.field(label, "(none)"),
            Value::String(value) => self.field(label, value),
            Value::Bool(_) | Value::Number(_) => self.field(label, &value.to_string()),
            Value::Array(values) => {
                self.field(label, if values.is_empty() { "[]" } else { "" });
                for (index, value) in values.iter().enumerate() {
                    self.value(&format!("  [{index}]"), value, depth + 1);
                    if self.full() || self.nodes > 256 {
                        self.truncated = true;
                        break;
                    }
                }
            }
            Value::Object(values) => {
                if string(value, "type").is_some_and(hidden_key) {
                    self.field(label, "[hidden internal content]");
                    return;
                }
                self.field(label, if values.is_empty() { "{}" } else { "" });
                for (key, value) in values {
                    if hidden_key(key) || artifact_key(key) {
                        continue;
                    }
                    if sensitive_key(key) {
                        self.field("sensitive field", "[redacted]");
                    } else {
                        self.value(&format!("  {key}"), value, depth + 1);
                    }
                    if self.full() || self.nodes > 256 {
                        self.truncated = true;
                        break;
                    }
                }
            }
        }
    }

    fn finish(mut self, artifacts: &str) -> String {
        if self.truncated {
            self.body.push('\n');
            self.body.push_str(TRUNCATED);
        }
        if !artifacts.is_empty() {
            self.body.push('\n');
            self.body.push_str(artifacts);
        }
        self.body
    }
}

fn prefix(value: &str, bytes: usize) -> (&str, bool) {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (&value[..end], end < value.len())
}

fn sensitive_key(key: &str) -> bool {
    let key = compact_key(key);
    matches!(
        key.as_str(),
        "token"
            | "apikey"
            | "secret"
            | "secrets"
            | "password"
            | "passwd"
            | "authorization"
            | "proxyauthorization"
            | "bearer"
            | "cookie"
            | "setcookie"
            | "credentials"
            | "credential"
            | "privatekey"
            | "callback"
            | "callbackurl"
            | "capability"
            | "capabilityurl"
            | "capabilitysecret"
    ) || [
        "token",
        "secret",
        "secretkey",
        "password",
        "apikey",
        "credentials",
        "privatekey",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix))
        || [
            "secret",
            "password",
            "credential",
            "authorization",
            "callback",
            "capability",
            "apikey",
            "accesskey",
            "privatekey",
            "cookie",
        ]
        .iter()
        .any(|part| key.contains(part))
}

fn hidden_key(key: &str) -> bool {
    matches!(
        compact_key(key).as_str(),
        "envelope"
            | "providerenvelope"
            | "providerresponse"
            | "rawresponse"
            | "rawrequest"
            | "reasoning"
            | "reasoningcontent"
            | "encryptedcontent"
            | "thinking"
            | "analysis"
            | "chainofthought"
            | "internalreasoning"
            | "summarytext"
    )
}

fn compact_key(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn safe_text(value: &str) -> String {
    // Opaque strings (commands, streams, diffs, and JSON encoded in strings)
    // cannot be traversed by key. Conservatively hide a sensitive line rather
    // than risk exposing a shell assignment, auth header, or wake capability.
    if sensitive_text(value) {
        return "[redacted sensitive text]".into();
    }
    value
        .chars()
        .map(|ch| {
            if ch.is_control() && ch != '\t' {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

fn sensitive_text(value: &str) -> bool {
    value.contains("/api/callbacks/")
        || value.contains("/callbacks/wake/")
        || value
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
            .any(|word| {
                sensitive_key(word.trim_start_matches('-'))
                    || hidden_key(word)
                    || ["sk-", "ghp_", "github_pat_", "cb_"]
                        .iter()
                        .any(|prefix| word.starts_with(prefix))
            })
        || value.split_whitespace().any(|word| {
            word.split_once("://")
                .is_some_and(|(_, rest)| rest.split('/').next().is_some_and(|s| s.contains('@')))
        })
}

fn artifact_key(key: &str) -> bool {
    matches!(
        key,
        "artifacts"
            | "artifact"
            | "recovery_artifact"
            | "source_artifact"
            | "output_ref"
            | "stdout_ref"
            | "stderr_ref"
            | "cmd_ref"
    )
}

fn artifact_footer(output: &Value, expanded: bool) -> String {
    fn collect(value: &Value, refs: &mut Vec<String>, visited: &mut usize, depth: usize) {
        *visited += 1;
        if depth > 8 || *visited > 512 || refs.len() >= 8 {
            return;
        }
        match value {
            Value::Object(values) => {
                if string(value, "type").is_some_and(hidden_key) {
                    return;
                }
                for (key, value) in values {
                    if sensitive_key(key) || (hidden_key(key) && key != "envelope") {
                        continue;
                    }
                    if artifact_key(key) {
                        if let Some(reference) = value.as_str().or_else(|| string(value, "path")) {
                            let (reference, clipped) = prefix(reference, 256);
                            let mut reference = safe_text(reference);
                            if clipped {
                                reference.push_str("…");
                            }
                            if !refs.contains(&reference) {
                                refs.push(reference);
                            }
                        } else if value.get("complete").and_then(Value::as_bool) == Some(false) {
                            refs.push("(recovery artifact unavailable)".into());
                        } else if let Some(artifacts) = value.as_array() {
                            for artifact in artifacts.iter().take(8) {
                                if let Some(path) = string(artifact, "path") {
                                    let (path, clipped) = prefix(path, 256);
                                    let mut path = safe_text(path);
                                    if clipped {
                                        path.push_str("…");
                                    }
                                    if !refs.contains(&path) {
                                        refs.push(path);
                                    }
                                }
                                if refs.len() >= 8 {
                                    break;
                                }
                            }
                        }
                    }
                    if refs.len() >= 8 || *visited > 512 {
                        break;
                    }
                    if value.is_object() || value.is_array() {
                        collect(value, refs, visited, depth + 1);
                    }
                }
            }
            Value::Array(values) => {
                for value in values.iter().take(64) {
                    collect(value, refs, visited, depth + 1);
                    if refs.len() >= 8 || *visited > 512 {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let mut refs = Vec::new();
    collect(output, &mut refs, &mut 0, 0);
    if refs.is_empty() {
        return String::new();
    }
    let joined = refs.join("; ");
    let (joined, clipped) = prefix(&joined, if expanded { 2048 } else { 512 });
    format!(
        "Artifacts: {joined}{}",
        if clipped || refs.len() >= 8 {
            " … [more omitted]"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests;
