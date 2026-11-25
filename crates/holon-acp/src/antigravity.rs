//! Google Antigravity (`agy acp`) harness profile.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::harness::{
    AcpHarnessSpec, KnownAuthMethod, SandboxMode, SessionOptionKeysMap, SessionOptions,
};

pub const NAME: &str = "antigravity";
pub const PROGRAM: &str = "agy";
pub const ACP_SUBCOMMAND: &str = "acp";

pub const ADDITIONAL_DIR_ARG: &str = "--add-dir";
pub const SANDBOX_ARG: &str = "--sandbox";
pub const AGENT_ARG: &str = "--agent";
pub const LOG_FILE_ARG: &str = "--log-file";
pub const DISABLE_TELEMETRY_ARG: &str = "--disable-telemetry";

/// Environment variables forwarded to the harness process.
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "LANG",
    "LC_ALL",
    "TERM",
    "SHELL",
    "USER",
    "LOGNAME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// Credential environment variables the profile accepts.
pub const API_KEY_ENV: &[&str] = &["GEMINI_API_KEY", "GOOGLE_API_KEY"];

/// Vertex / Enterprise `agent-platform` Application Default Credentials.
///
/// These are credential inputs, not general environment: they are only
/// forwarded when the host explicitly has them, and `AGY_ADC_AUTH` selects the
/// ADC auth mode without copying credential files into the harness home.
pub const ADC_ENV: &[&str] = &[
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "AGY_ADC_AUTH",
];

/// Auth methods the first profile recognizes by id.
pub const AUTH_METHOD_IDS: &[&str] = &[
    "oauth-personal",
    "oauth-business",
    "gemini-api-key",
    "agent-platform",
    "terminal-login",
];

pub const AUTH_HELP: &str =
    "reuse the host's existing `agy acp --login` state, or provide GEMINI_API_KEY; \
     never copy or scrape private harness session files";

/// Normalized `_meta` keys used by the Antigravity profile.
pub const MODEL_META_KEY: &str = "_meta.model";
pub const THINKING_LEVEL_META_KEY: &str = "_meta.thinkingLevel";
pub const MODE_META_KEY: &str = "_meta.mode";

/// Antigravity profile definition.
pub fn profile() -> AcpHarnessSpec {
    let mut sandbox_args = BTreeMap::new();
    // The profile only claims the flag form. A value-carrying sandbox mode is
    // not asserted here because the CLI contract is not verified; callers that
    // need finer confinement must not claim it.
    sandbox_args.insert(
        SandboxMode::ReadOnly.key().to_string(),
        vec![SANDBOX_ARG.to_string()],
    );
    sandbox_args.insert(
        SandboxMode::WorkspaceWrite.key().to_string(),
        vec![SANDBOX_ARG.to_string()],
    );

    AcpHarnessSpec {
        name: NAME.to_string(),
        program: PROGRAM.to_string(),
        default_args: vec![ACP_SUBCOMMAND.to_string()],
        additional_dir_arg: Some(ADDITIONAL_DIR_ARG.to_string()),
        sandbox_args,
        agent_arg: Some(AGENT_ARG.to_string()),
        log_file_arg: Some(LOG_FILE_ARG.to_string()),
        disable_telemetry_arg: Some(DISABLE_TELEMETRY_ARG.to_string()),
        session_option_keys: SessionOptionKeysMap {
            model: MODEL_META_KEY.to_string(),
            thinking_level: THINKING_LEVEL_META_KEY.to_string(),
            mode: MODE_META_KEY.to_string(),
        },
        env_allowlist: ENV_ALLOWLIST.iter().map(|name| name.to_string()).collect(),
        api_key_env: API_KEY_ENV
            .iter()
            .chain(ADC_ENV.iter())
            .map(|name| name.to_string())
            .collect(),
        auth_methods: AUTH_METHOD_IDS.iter().map(|id| id.to_string()).collect(),
        auth_help: Some(AUTH_HELP.to_string()),
        default_session_options: SessionOptions::default(),
    }
}

/// Default session `_meta` map for the given options.
pub fn session_meta(options: &SessionOptions) -> Map<String, Value> {
    profile().session_meta(options)
}

/// Recognized auth methods from a handshake result.
pub fn known_auth_methods(methods: &[crate::protocol::AuthMethod]) -> Vec<KnownAuthMethod> {
    profile().known_auth_methods(methods)
}
