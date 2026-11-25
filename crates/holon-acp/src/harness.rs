//! Declarative external harness profiles and their launch/session mapping.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::AcpError;
use crate::protocol::AuthMethod;

/// Sandbox policy requested for a harness launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    /// No confinement is claimed by Holon; the harness runs with its own defaults.
    #[default]
    FullAccess,
}

impl SandboxMode {
    pub fn key(self) -> &'static str {
        match self {
            SandboxMode::ReadOnly => "read_only",
            SandboxMode::WorkspaceWrite => "workspace_write",
            SandboxMode::FullAccess => "full_access",
        }
    }
}

/// Session thinking level advertised by the Antigravity profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Low,
    Medium,
    High,
    Extra,
    Max,
}

impl ThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Low => "Low",
            ThinkingLevel::Medium => "Medium",
            ThinkingLevel::High => "High",
            ThinkingLevel::Extra => "Extra",
            ThinkingLevel::Max => "Max",
        }
    }
}

/// Session-scoped options a harness profile understands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    /// Execution mode label owned by the harness (for example a permission mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl SessionOptions {
    /// Render these options into a `session/new` `_meta` map.
    ///
    /// The key names are profile-declared values, not ACP wire contract; the
    /// profile owns them so a harness rename is a data change, not a code change.
    pub fn to_meta(&self) -> Map<String, Value> {
        self.to_meta_with(&SessionOptionKeys::default())
    }

    pub fn to_meta_with(&self, keys: &SessionOptionKeys) -> Map<String, Value> {
        let mut meta = Map::new();
        if let Some(model) = &self.model {
            meta.insert(keys.model.clone(), Value::String(model.clone()));
        }
        if let Some(level) = self.thinking_level {
            meta.insert(
                keys.thinking_level.clone(),
                Value::String(level.as_str().to_string()),
            );
        }
        if let Some(mode) = &self.mode {
            meta.insert(keys.mode.clone(), Value::String(mode.clone()));
        }
        meta
    }
}

/// Profile-declared `_meta` keys for session options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOptionKeys {
    pub model: String,
    pub thinking_level: String,
    pub mode: String,
}

impl Default for SessionOptionKeys {
    fn default() -> Self {
        Self {
            model: "_meta.model".to_string(),
            thinking_level: "_meta.thinkingLevel".to_string(),
            mode: "_meta.mode".to_string(),
        }
    }
}

/// Inputs that shape a harness process launch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequest {
    /// Bound execution root; the harness process `cwd`.
    pub workspace_dir: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<String>,
    #[serde(default)]
    pub sandbox: SandboxMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
    #[serde(default)]
    pub disable_telemetry: bool,
}

/// Program and arguments for one harness launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl LaunchCommand {
    pub fn command_line(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// Authentication methods this client recognizes by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownAuthMethod {
    OAuthPersonal,
    OAuthBusiness,
    GeminiApiKey,
    AgentPlatform,
    TerminalLogin,
}

impl KnownAuthMethod {
    pub const ALL: [KnownAuthMethod; 5] = [
        KnownAuthMethod::OAuthPersonal,
        KnownAuthMethod::OAuthBusiness,
        KnownAuthMethod::GeminiApiKey,
        KnownAuthMethod::AgentPlatform,
        KnownAuthMethod::TerminalLogin,
    ];

    pub fn id(self) -> &'static str {
        match self {
            KnownAuthMethod::OAuthPersonal => "oauth-personal",
            KnownAuthMethod::OAuthBusiness => "oauth-business",
            KnownAuthMethod::GeminiApiKey => "gemini-api-key",
            KnownAuthMethod::AgentPlatform => "agent-platform",
            KnownAuthMethod::TerminalLogin => "terminal-login",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.id() == id)
    }
}

/// Declarative profile for one external ACP harness.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpHarnessSpec {
    pub name: String,
    pub program: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_dir_arg: Option<String>,
    /// Sandbox mode key -> arguments; absent key means "no flag for this mode".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sandbox_args: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_telemetry_arg: Option<String>,
    pub session_option_keys: SessionOptionKeysMap,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_key_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth_methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_help: Option<String>,
    #[serde(default)]
    pub default_session_options: SessionOptions,
}

/// Serializable form of [`SessionOptionKeys`] inside a profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOptionKeysMap {
    pub model: String,
    pub thinking_level: String,
    pub mode: String,
}

impl Default for SessionOptionKeysMap {
    fn default() -> Self {
        let keys = SessionOptionKeys::default();
        Self {
            model: keys.model,
            thinking_level: keys.thinking_level,
            mode: keys.mode,
        }
    }
}

impl From<&SessionOptionKeysMap> for SessionOptionKeys {
    fn from(map: &SessionOptionKeysMap) -> Self {
        Self {
            model: map.model.clone(),
            thinking_level: map.thinking_level.clone(),
            mode: map.mode.clone(),
        }
    }
}

impl AcpHarnessSpec {
    /// Build the process launch command for a request.
    pub fn build_launch(&self, request: &LaunchRequest) -> LaunchCommand {
        let mut args = self.default_args.clone();
        for directory in &request.additional_directories {
            if let Some(flag) = &self.additional_dir_arg {
                args.push(flag.clone());
                args.push(directory.clone());
            }
        }
        if let Some(flag) = &self.sandbox_args.get(request.sandbox.key()) {
            args.extend(flag.iter().cloned());
        }
        if let (Some(flag), Some(agent)) = (&self.agent_arg, &request.agent) {
            args.push(flag.clone());
            args.push(agent.clone());
        }
        if let (Some(flag), Some(log_file)) = (&self.log_file_arg, &request.log_file) {
            args.push(flag.clone());
            args.push(log_file.clone());
        }
        if request.disable_telemetry {
            if let Some(flag) = &self.disable_telemetry_arg {
                args.push(flag.clone());
            }
        }
        LaunchCommand {
            program: self.program.clone(),
            args,
        }
    }

    /// Keep only allowlisted environment variables plus declared API key vars.
    pub fn filter_env(&self, source: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        source
            .iter()
            .filter(|(name, _)| {
                self.env_allowlist.iter().any(|allowed| allowed == *name)
                    || self.api_key_env.iter().any(|allowed| allowed == *name)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }

    /// Session `_meta` map for the profile's declared option keys.
    pub fn session_meta(&self, options: &SessionOptions) -> Map<String, Value> {
        options.to_meta_with(&SessionOptionKeys::from(&self.session_option_keys))
    }

    /// Authentication methods the peer advertised that this profile recognizes.
    pub fn known_auth_methods(&self, methods: &[AuthMethod]) -> Vec<KnownAuthMethod> {
        methods
            .iter()
            .filter_map(|method| KnownAuthMethod::from_id(&method.id))
            .collect()
    }

    /// Structured `-32000` mapping for this profile.
    pub fn authentication_error(
        &self,
        message: impl Into<String>,
        methods: &[AuthMethod],
    ) -> AcpError {
        AcpError::AuthenticationRequired {
            message: message.into(),
            auth_methods: methods.iter().map(|method| method.id.clone()).collect(),
        }
    }
}

/// Registry of known harness profiles.
#[derive(Clone, Debug, Default)]
pub struct AcpHarnessRegistry {
    profiles: BTreeMap<String, AcpHarnessSpec>,
}

impl AcpHarnessRegistry {
    /// Registry seeded with the built-in `antigravity` profile.
    pub fn with_builtin_profiles() -> Self {
        let mut registry = Self::default();
        registry.register(crate::antigravity::profile());
        registry
    }

    pub fn register(&mut self, spec: AcpHarnessSpec) -> Option<AcpHarnessSpec> {
        self.profiles.insert(spec.name.clone(), spec)
    }

    pub fn get(&self, name: &str) -> Option<&AcpHarnessSpec> {
        self.profiles.get(name)
    }

    pub fn require(&self, name: &str) -> Result<&AcpHarnessSpec, AcpError> {
        self.get(name).ok_or_else(|| AcpError::UnknownHarness {
            name: name.to_string(),
        })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.profiles.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_launch_maps_flags() {
        let spec = crate::antigravity::profile();
        let request = LaunchRequest {
            workspace_dir: "/work/repo".into(),
            additional_directories: vec!["/work/extra".into()],
            sandbox: SandboxMode::WorkspaceWrite,
            agent: Some("reviewer".into()),
            log_file: Some("/tmp/agy.log".into()),
            disable_telemetry: true,
        };
        let command = spec.build_launch(&request);
        assert_eq!(command.program, "agy");
        assert_eq!(
            command.args,
            vec![
                "acp",
                "--add-dir",
                "/work/extra",
                "--sandbox",
                "--agent",
                "reviewer",
                "--log-file",
                "/tmp/agy.log",
                "--disable-telemetry",
            ]
        );
    }

    #[test]
    fn full_access_sandbox_adds_no_flag() {
        let spec = crate::antigravity::profile();
        let request = LaunchRequest {
            workspace_dir: "/work/repo".into(),
            sandbox: SandboxMode::FullAccess,
            ..LaunchRequest::default()
        };
        assert_eq!(spec.build_launch(&request).args, vec!["acp"]);
    }

    #[test]
    fn filters_environment_to_allowlist_and_api_keys() {
        let spec = crate::antigravity::profile();
        let source: BTreeMap<String, String> = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/dev"),
            ("SECRET_TOKEN", "nope"),
            ("GEMINI_API_KEY", "key"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
        let filtered = spec.filter_env(&source);
        assert!(filtered.contains_key("PATH"));
        assert!(filtered.contains_key("GEMINI_API_KEY"));
        assert!(!filtered.contains_key("SECRET_TOKEN"));
    }

    #[test]
    fn maps_session_options_through_profile_keys() {
        let spec = crate::antigravity::profile();
        let options = SessionOptions {
            model: Some("gemini-pro".into()),
            thinking_level: Some(ThinkingLevel::High),
            mode: Some("plan".into()),
        };
        let meta = spec.session_meta(&options);
        assert_eq!(meta["_meta.model"], serde_json::json!("gemini-pro"));
        assert_eq!(meta["_meta.thinkingLevel"], serde_json::json!("High"));
        assert_eq!(meta["_meta.mode"], serde_json::json!("plan"));
    }

    #[test]
    fn registry_resolves_profiles_and_reports_unknown() {
        let registry = AcpHarnessRegistry::with_builtin_profiles();
        assert!(registry.get("antigravity").is_some());
        assert_eq!(registry.names().collect::<Vec<_>>(), vec!["antigravity"]);
        assert!(matches!(
            registry.require("missing"),
            Err(AcpError::UnknownHarness { .. })
        ));
    }
}
