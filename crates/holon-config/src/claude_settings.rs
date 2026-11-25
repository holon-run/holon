//! Best-effort loading of the optional Claude-compatible settings env file.
//!
//! `~/.claude/settings.json` is not Holon persisted configuration or provider
//! schema: it is only one optional source of Claude-compatible environment
//! values. A missing, unreadable, malformed, or partially incompatible file
//! must never block provider resolution, runtime config validation, or
//! onboarding, so loading never fails. Recoverable problems become one
//! aggregated diagnostic per high-level load, and diagnostics never carry env
//! values or the settings file content.

use std::{
    collections::HashMap,
    env, fmt, fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;

/// Stable, privacy-preserving label for the optional settings file.
pub const CLAUDE_SETTINGS_SOURCE: &str = "~/.claude/settings.json";

/// Environment values recovered from the Claude-compatible settings file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeSettingsEnv {
    /// Recovered env values. Empty when nothing could be recovered.
    pub env: HashMap<String, String>,
    /// Single aggregated diagnostic for this load, if the file had problems.
    pub diagnostic: Option<ClaudeSettingsDiagnostic>,
}

impl ClaudeSettingsEnv {
    /// Borrow the aggregated diagnostic for this load, if any.
    pub fn diagnostic(&self) -> Option<&ClaudeSettingsDiagnostic> {
        self.diagnostic.as_ref()
    }

    /// Consume the load result and take the recovered env values.
    pub fn into_env(self) -> HashMap<String, String> {
        self.env
    }

    fn damaged(category: ClaudeSettingsIssue, detail: impl Into<String>) -> Self {
        Self {
            env: HashMap::new(),
            diagnostic: Some(ClaudeSettingsDiagnostic {
                category,
                detail: detail.into(),
            }),
        }
    }
}

/// One aggregated, value-free diagnostic for a settings load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSettingsDiagnostic {
    pub category: ClaudeSettingsIssue,
    pub detail: String,
}

impl fmt::Display for ClaudeSettingsDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ignoring {CLAUDE_SETTINGS_SOURCE} ({}): {}",
            self.category.as_str(),
            self.detail
        )
    }
}

/// Recoverable problem categories reported for the optional settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeSettingsIssue {
    Unreadable,
    InvalidJson,
    IncompatibleRoot,
    IncompatibleEnv,
    UnsupportedEnvValues,
}

impl ClaudeSettingsIssue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::InvalidJson => "invalid json",
            Self::IncompatibleRoot => "incompatible root",
            Self::IncompatibleEnv => "incompatible env",
            Self::UnsupportedEnvValues => "unsupported env values",
        }
    }
}

/// Load the optional Claude-compatible env values. Never fails.
pub fn load_settings_env() -> ClaudeSettingsEnv {
    load_settings_env_at(&settings_path())
}

pub(crate) fn settings_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".claude/settings.json")
}

pub(crate) fn load_settings_env_at(path: &Path) -> ClaudeSettingsEnv {
    match fs::read_to_string(path) {
        Ok(content) => parse_settings_env(&content),
        // A missing optional file is the normal case and is not a problem.
        Err(error) if error.kind() == io::ErrorKind::NotFound => ClaudeSettingsEnv::default(),
        Err(error) => {
            ClaudeSettingsEnv::damaged(ClaudeSettingsIssue::Unreadable, error.to_string())
        }
    }
}

fn parse_settings_env(content: &str) -> ClaudeSettingsEnv {
    let document: Value = match serde_json::from_str(content) {
        // serde_json keeps the line/column position in its message.
        Ok(document) => document,
        Err(error) => {
            return ClaudeSettingsEnv::damaged(ClaudeSettingsIssue::InvalidJson, error.to_string())
        }
    };
    let Some(root) = document.as_object() else {
        return ClaudeSettingsEnv::damaged(
            ClaudeSettingsIssue::IncompatibleRoot,
            format!(
                "expected a JSON object at the document root, found {}",
                json_kind(&document)
            ),
        );
    };
    let Some(env_value) = root.get("env") else {
        return ClaudeSettingsEnv::default();
    };
    let Value::Object(env_object) = env_value else {
        return ClaudeSettingsEnv::damaged(
            ClaudeSettingsIssue::IncompatibleEnv,
            format!(
                "expected /env to be a JSON object, found {}",
                json_kind(env_value)
            ),
        );
    };

    let mut env = HashMap::new();
    let mut unsupported = Vec::new();
    for (key, value) in env_object {
        match value {
            Value::String(text) => {
                env.insert(key.clone(), text.clone());
            }
            // Claude Code exports every scalar as a string; use the JSON
            // canonical text so `1` becomes "1" and `true` becomes "true".
            Value::Number(number) => {
                env.insert(key.clone(), number.to_string());
            }
            Value::Bool(flag) => {
                env.insert(key.clone(), flag.to_string());
            }
            // A JSON null means "unset"; it must not become the string "null".
            Value::Null => {}
            Value::Array(_) | Value::Object(_) => {
                unsupported.push(format!("{} ({})", env_pointer(key), json_kind(value)));
            }
        }
    }

    if unsupported.is_empty() {
        ClaudeSettingsEnv {
            env,
            diagnostic: None,
        }
    } else {
        ClaudeSettingsEnv {
            env,
            diagnostic: Some(ClaudeSettingsDiagnostic {
                category: ClaudeSettingsIssue::UnsupportedEnvValues,
                detail: format!(
                    "skipped unsupported env values at {}",
                    unsupported.join(", ")
                ),
            }),
        }
    }
}

fn env_pointer(key: &str) -> String {
    format!("/env/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn load(contents: &str) -> ClaudeSettingsEnv {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, contents).unwrap();
        load_settings_env_at(&path)
    }

    #[test]
    fn missing_file_yields_empty_env_without_diagnostic() {
        let directory = tempfile::tempdir().unwrap();
        let loaded = load_settings_env_at(&directory.path().join("settings.json"));
        assert!(loaded.env.is_empty());
        assert_eq!(loaded.diagnostic, None);
    }

    #[test]
    fn unreadable_file_yields_empty_env_with_one_diagnostic() {
        let directory = tempfile::tempdir().unwrap();
        // A directory is never readable as a settings file.
        let loaded = load_settings_env_at(directory.path());
        assert!(loaded.env.is_empty());
        assert_eq!(
            loaded.diagnostic.map(|diagnostic| diagnostic.category),
            Some(ClaudeSettingsIssue::Unreadable)
        );
    }

    #[test]
    fn malformed_json_keeps_serde_position() {
        let loaded = load("{\"env\": {\"KEY\": 1}");
        assert!(loaded.env.is_empty());
        let diagnostic = loaded.diagnostic.unwrap();
        assert_eq!(diagnostic.category, ClaudeSettingsIssue::InvalidJson);
        assert!(
            diagnostic.detail.contains("line 1 column"),
            "missing serde position: {}",
            diagnostic.detail
        );
        assert!(diagnostic.to_string().contains(CLAUDE_SETTINGS_SOURCE));
    }

    #[test]
    fn incompatible_root_yields_one_diagnostic() {
        let loaded = load("[\"env\"]");
        assert!(loaded.env.is_empty());
        let diagnostic = loaded.diagnostic.unwrap();
        assert_eq!(diagnostic.category, ClaudeSettingsIssue::IncompatibleRoot);
        assert!(diagnostic.detail.contains("array"));
    }

    #[test]
    fn incompatible_env_yields_one_diagnostic() {
        let loaded = load("{\"env\": \"KEY=value\"}");
        assert!(loaded.env.is_empty());
        let diagnostic = loaded.diagnostic.unwrap();
        assert_eq!(diagnostic.category, ClaudeSettingsIssue::IncompatibleEnv);
        assert!(diagnostic.detail.contains("/env"));
    }

    #[test]
    fn scalar_env_values_are_stringified_and_null_is_unset() {
        let loaded = load(
            r#"{"env": {"STRING": "value", "INT": 1, "FLOAT": 1.50, "TRUE": true, "FALSE": false, "UNSET": null}}"#,
        );
        assert_eq!(loaded.diagnostic, None);
        let mut expected = HashMap::new();
        expected.insert("STRING".to_string(), "value".to_string());
        expected.insert("INT".to_string(), "1".to_string());
        expected.insert("FLOAT".to_string(), "1.5".to_string());
        expected.insert("TRUE".to_string(), "true".to_string());
        expected.insert("FALSE".to_string(), "false".to_string());
        assert_eq!(loaded.env, expected);
    }

    #[test]
    fn unsupported_env_values_are_skipped_and_aggregated_once() {
        let loaded =
            load(r#"{"env": {"KEEP": "1", "OBJECT": {"nested": "secret"}, "ARRAY": ["secret"]}}"#);
        assert_eq!(
            loaded.env.get("KEEP").map(String::as_str),
            Some("1"),
            "legal entries must survive"
        );
        assert_eq!(loaded.env.len(), 1);
        let diagnostic = loaded.diagnostic.unwrap();
        assert_eq!(
            diagnostic.category,
            ClaudeSettingsIssue::UnsupportedEnvValues
        );
        assert!(diagnostic.detail.contains("/env/OBJECT (object)"));
        assert!(diagnostic.detail.contains("/env/ARRAY (array)"));
        assert!(!diagnostic.to_string().contains("secret"));
    }

    #[test]
    fn unknown_top_level_fields_are_ignored() {
        let loaded = load(r#"{"env": {"KEY": "value"}, "permissions": {"allow": []}}"#);
        assert_eq!(loaded.diagnostic, None);
        assert_eq!(loaded.env.len(), 1);
    }

    #[test]
    fn pointer_escapes_json_pointer_characters() {
        assert_eq!(env_pointer("A/B~C"), "/env/A~1B~0C");
    }
}
