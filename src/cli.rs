//! CLI command tree definition for `holon`.
//!
//! This module owns the clap-derived types so that integration tests can
//! introspect the full command tree for snapshot / contract testing.

use std::path::PathBuf;

use clap::{ArgGroup, Args, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::types::AuthorityClass;
use crate::types::MediaOperation;

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(parsed) if parsed > 0 => Ok(parsed),
        Ok(_) => Err("value must be greater than zero".to_string()),
        Err(err) => Err(err.to_string()),
    }
}

/// Sentinel accepted by `holon agent model set` override flags to remove the
/// existing override instead of replacing it.
pub const MODEL_OVERRIDE_CLEAR: &str = "clear";

/// Parse a `--service-tier` flag value into a [`crate::config::ServiceTier`].
pub fn parse_service_tier_flag(value: &str) -> Result<crate::config::ServiceTier, String> {
    match value {
        "default" => Ok(crate::config::ServiceTier::Default),
        "fast" | "priority" => Ok(crate::config::ServiceTier::Fast),
        "ultrafast" => Ok(crate::config::ServiceTier::Ultrafast),
        _ => Err(format!(
            "invalid service_tier '{value}'; supported values: default, fast (alias: priority), \
             ultrafast, or '{MODEL_OVERRIDE_CLEAR}' to remove the override"
        )),
    }
}

/// Resolve the tri-state meaning of `--reasoning-effort` against the current
/// override: omitted keeps the current value, `clear` removes it, and any
/// other value replaces it. Returns the value to submit and whether an
/// explicit clear happened (for the user-visible confirmation).
pub fn resolve_effort_override(
    flag: Option<&str>,
    current: Option<&str>,
) -> (Option<String>, bool) {
    match flag {
        None => (current.map(str::to_string), false),
        Some(MODEL_OVERRIDE_CLEAR) => (None, true),
        Some(value) => (Some(value.to_string()), false),
    }
}

/// Resolve the tri-state meaning of `--service-tier` against the current
/// override, with the same semantics as [`resolve_effort_override`].
pub fn resolve_tier_override(
    flag: Option<&str>,
    current: Option<crate::config::ServiceTier>,
) -> Result<(Option<crate::config::ServiceTier>, bool), String> {
    match flag {
        None => Ok((current, false)),
        Some(MODEL_OVERRIDE_CLEAR) => Ok((None, true)),
        Some(value) => Ok((Some(parse_service_tier_flag(value)?), false)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentDetailArg {
    Compact,
    Full,
}

/// Explicit operator decision for a stopped agent-deletion job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DeletionResolveDecision {
    /// Give up on the stopped deletion and return the identity to active.
    Abandon,
}

#[derive(Debug, Subcommand)]
pub enum TimerCommands {
    Create {
        #[command(flatten)]
        options: TimerCreateArgs,
    },
    List {
        #[arg(long, default_value_t = 50, value_parser = parse_positive_usize)]
        limit: usize,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "Pause a timer")]
    Pause {
        timer_id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "Resume a paused timer")]
    Resume {
        timer_id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "Edit a timer's title or instruction without changing its schedule")]
    Edit {
        timer_id: String,
        #[arg(long)]
        agent: Option<String>,
        #[command(flatten)]
        content: TimerEditArgs,
    },
    Cancel {
        timer_id: String,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("timer_content")
        .required(true)
        .multiple(true)
        .args(["title", "clear_title", "instruction", "clear_instruction"])
))]
pub struct TimerEditArgs {
    #[arg(long, conflicts_with = "clear_title")]
    pub title: Option<String>,
    #[arg(long)]
    pub clear_title: bool,
    #[arg(long, visible_alias = "summary", conflicts_with = "clear_instruction")]
    pub instruction: Option<String>,
    #[arg(long, visible_alias = "clear-summary")]
    pub clear_instruction: bool,
}

impl TimerEditArgs {
    pub fn into_patch(self) -> crate::types::TimerContentPatch {
        crate::types::TimerContentPatch {
            title: if self.clear_title {
                Some(None)
            } else {
                self.title.map(Some)
            },
            instruction: if self.clear_instruction {
                Some(None)
            } else {
                self.instruction.map(Some)
            },
        }
    }
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("schedule")
        .required(true)
        .args(["after_ms", "calendar_expression"])
))]
pub struct TimerCreateArgs {
    #[arg(long)]
    pub after_ms: Option<u64>,
    #[arg(long)]
    pub every_ms: Option<u64>,
    #[arg(long)]
    pub calendar_expression: Option<String>,
    #[arg(long)]
    pub timezone: Option<String>,
    #[arg(long, help = "Optional display title")]
    pub title: Option<String>,
    #[arg(
        long,
        conflicts_with = "summary",
        help = "Text for each new timer wake"
    )]
    pub instruction: Option<String>,
    #[arg(long, help = "Legacy alias for --instruction")]
    pub summary: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
}

#[derive(Debug, Args)]
pub struct LegacyTimerCreateArgs {
    #[arg(long)]
    pub after_ms: Option<u64>,
    #[arg(long)]
    pub every_ms: Option<u64>,
    #[arg(long)]
    pub calendar_expression: Option<String>,
    #[arg(long)]
    pub timezone: Option<String>,
    #[arg(long)]
    pub summary: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
}

// ---------------------------------------------------------------------------
// Top-level CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(name = "holon")]
#[command(about = "A headless, event-driven runtime for long-lived agents")]
#[command(version = env!("HOLON_VERSION"))]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

// ---------------------------------------------------------------------------
// Top-level commands
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(about = "Show the declared caller context for this CLI invocation")]
    Context,
    #[command(about = "Show machine-readable CLI command metadata")]
    Commands,
    #[command(about = "Read versioned embedded Holon operational manuals offline")]
    Docs {
        /// Topic slug (cli, skills, apps, agents, workspace, agent-home, models). Omit to list topics.
        topic: Option<String>,
        /// Output structured JSON
        #[arg(long)]
        json: bool,
        /// Output format (`text` or `json`)
        #[arg(long, value_enum, default_value_t = DocsOutputFormat::Text)]
        output: DocsOutputFormat,
    },
    #[command(about = "Interactively set up Holon or print secret-safe onboarding diagnostics")]
    Onboard {
        #[arg(long)]
        json: bool,
    },
    Serve {
        #[command(flatten)]
        options: ServeOptions,
    },
    Daemon {
        #[arg(
            long,
            global = true,
            help = "Write one machine-readable JSON envelope per command"
        )]
        json: bool,
        #[command(subcommand)]
        command: DaemonCommands,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    Prompt {
        text: String,
        #[arg(long)]
        agent: Option<String>,
    },
    Tail {
        #[command(flatten)]
        options: crate::tail::TailOptions,
        #[arg(long)]
        agent: Option<String>,
    },
    Transcript {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "Read stable runtime event envelopes")]
    Events {
        #[command(subcommand)]
        command: EventsCommands,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },
    #[command(name = "work-item")]
    WorkItem {
        #[command(subcommand)]
        command: WorkItemCommands,
    },
    #[command(name = "memory-index", about = "Manage the local memory search index")]
    MemoryIndex {
        #[command(subcommand)]
        command: MemoryIndexCommands,
    },
    #[command(
        about = "Create, list, edit, pause, resume, or cancel timers",
        args_conflicts_with_subcommands = true
    )]
    Timer {
        #[command(subcommand)]
        command: Option<TimerCommands>,
        #[command(flatten)]
        legacy: LegacyTimerCreateArgs,
    },
    #[command(about = "Deprecated: use `holon agent start|stop|abort [agent-id]`")]
    Control {
        action: ControlCommandAction,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(alias = "agents")]
    Agent {
        #[command(subcommand)]
        command: Option<AgentCommands>,
    },
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
    },
    #[command(about = "Read-only Agent template catalog queries")]
    Template {
        #[command(subcommand)]
        command: TemplateCommands,
    },
    Run {
        text: String,
        #[arg(
            long = "authority-class",
            alias = "trust",
            default_value = "operator-instruction"
        )]
        authority_class: AuthorityClass,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, requires = "agent")]
        create_agent: bool,
        #[arg(long, requires = "create_agent")]
        template: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max_turns: Option<u64>,
        #[arg(long)]
        no_wait_for_tasks: bool,
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        workspace_root: Option<PathBuf>,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    Solve {
        #[arg(value_name = "REF")]
        target_ref: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        template: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max_turns: Option<u64>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout_seconds: Option<u64>,
        #[arg(
            long = "authority-class",
            alias = "trust",
            default_value = "operator-instruction"
        )]
        authority_class: AuthorityClass,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        workspace_root: Option<PathBuf>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommands,
    },
    Tui {
        /// Select an agent for this session only (including later switches); do not update remembered selection.
        #[arg(long, value_name = "AGENT_ID")]
        agent: Option<String>,
        #[arg(long)]
        no_alt_screen: bool,
        #[arg(long)]
        connect: Option<String>,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        token_profile: Option<String>,
    },
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
    },
    #[command(
        name = "models-dev",
        about = "models.dev snapshot refresh, validation, and audit"
    )]
    ModelsDev {
        #[command(subcommand)]
        command: ModelsDevCommands,
    },
}

// ---------------------------------------------------------------------------
// Shared enums / args structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DocsOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum ControlCommandAction {
    /// Start the agent.
    Start,
    /// Stop the agent.
    Stop,
    /// Abort the agent.
    Abort,
}

impl ControlCommandAction {
    /// Convert to a `ControlAction`, returning `None` for `Abort`.
    pub fn as_control_action(&self) -> Option<crate::types::ControlAction> {
        match self {
            Self::Start => Some(crate::types::ControlAction::Start),
            Self::Stop => Some(crate::types::ControlAction::Stop),
            Self::Abort => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServeAccess {
    Local,
    Tunnel,
    /// Run a preconfigured Cloudflare Named Tunnel (not Quick Tunnels).
    Cloudflare,
    Lan,
    Tailnet,
}

#[derive(Debug, Clone, Args)]
pub struct ServeOptions {
    /// Discard all inherited serve options before applying explicit inputs.
    #[arg(long)]
    pub reset_serve_options: bool,
    #[arg(long, value_enum)]
    pub access: Option<ServeAccess>,
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long)]
    pub listen: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,
    #[arg(long)]
    pub advertise: Option<String>,
    /// Connector token file for a preconfigured Cloudflare Named Tunnel.
    #[arg(long)]
    pub cloudflare_token_file: Option<PathBuf>,
    #[arg(long)]
    pub token: Option<String>,
    #[arg(long)]
    pub token_file: Option<PathBuf>,
    #[arg(long)]
    pub web_dist: Option<PathBuf>,
    /// Enable authorized Finder operations on the macOS daemon host.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub desktop_integration: Option<bool>,
    /// Enable desktop integration only when no saved or legacy serve options exist.
    #[arg(long, conflicts_with = "reset_serve_options")]
    pub desktop_integration_if_unconfigured: bool,
}

// ---------------------------------------------------------------------------
// Subcommand enums
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum DaemonCommands {
    Start {
        #[command(flatten)]
        options: ServeOptions,
        #[arg(long, env = "HOLON_DAEMON_START_TIMEOUT_SECS", value_parser = clap::value_parser!(u64).range(1..=86400))]
        start_timeout: Option<u64>,
    },
    Stop,
    #[command(hide = true)]
    PrepareUpdate,
    Status,
    /// Read the last recorded startup quarantine report without contacting the daemon.
    Quarantine,
    /// Validate that this binary can start against the existing runtime library.
    ///
    /// Read-only by default: it parses configuration, resolves effective paths,
    /// opens the existing runtime database with `SQLITE_OPEN_READ_ONLY` and
    /// `PRAGMA query_only=1`, and verifies the schema ledger, storage domains,
    /// agent registry, quarantine report, and persisted input-evidence snapshot
    /// metadata. It never creates a missing database, migrates, initializes
    /// storage domains, materializes workspace/identity rows, normalizes wakes
    /// or queues, or appends audit events.
    ///
    /// `--migrate` performs the real schema migration and backfills on an
    /// explicitly selected copy or maintenance window. It refuses before
    /// opening the database unless it holds the maintenance lease and the
    /// control socket is not live, then runs the same read-only validation.
    ValidateStartup {
        /// Apply the real migration and backfills (requires the daemon stopped).
        #[arg(long)]
        migrate: bool,
    },
    Restart {
        #[command(flatten)]
        options: ServeOptions,
        #[arg(long, env = "HOLON_DAEMON_START_TIMEOUT_SECS", value_parser = clap::value_parser!(u64).range(1..=86400))]
        start_timeout: Option<u64>,
    },
    Logs {
        #[arg(long, default_value_t = 80)]
        tail: usize,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    Unset {
        key: String,
    },
    Providers {
        #[command(subcommand)]
        command: ConfigProviderCommands,
    },
    Credentials {
        #[command(subcommand)]
        command: ConfigCredentialCommands,
    },
    Models {
        #[command(subcommand)]
        command: ConfigModelCommands,
    },
    #[command(
        name = "model-providers",
        about = "List resolved model providers with availability and media capability"
    )]
    ModelProviders {
        #[command(subcommand)]
        command: ConfigModelProviderCommands,
    },
    #[command(about = "Inspect or rewrite legacy model selections as canonical route refs")]
    MigrateModelRoutes {
        #[arg(
            long,
            help = "Persist the migration after a complete successful preflight"
        )]
        write: bool,
    },
    List,
    Schema,
    #[command(
        name = "web-providers",
        about = "List effective WebSearch and WebFetch providers as JSON"
    )]
    WebProviders,
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum ConfigProviderCommands {
    /// Manage non-sensitive static headers without replacing provider settings.
    Headers {
        #[command(subcommand)]
        command: ConfigProviderHeaderCommands,
    },
    Set {
        provider: String,
        #[arg(long)]
        transport: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long, default_value = "none")]
        credential_source: String,
        #[arg(long, default_value = "none")]
        credential_kind: String,
        #[arg(long)]
        credential_env: Option<String>,
        #[arg(long)]
        credential_profile: Option<String>,
        #[arg(long)]
        credential_external: Option<String>,
    },
    Get {
        provider: String,
    },
    List,
    Remove {
        provider: String,
    },
    Doctor {
        provider: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigProviderHeaderCommands {
    Set {
        provider: String,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(
            long = "header",
            required = true,
            help = "Non-sensitive NAME=VALUE (repeatable; visible in shell history/process args)"
        )]
        headers: Vec<String>,
    },
    Unset {
        provider: String,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long = "name", required = true)]
        names: Vec<String>,
    },
}

impl std::fmt::Debug for ConfigProviderHeaderCommands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Set {
                provider,
                endpoint,
                headers,
            } => f
                .debug_struct("SetProviderHeaders")
                .field("provider", provider)
                .field("endpoint", endpoint)
                .field("header_count", &headers.len())
                .finish(),
            Self::Unset {
                provider,
                endpoint,
                names,
            } => f
                .debug_struct("UnsetProviderHeaders")
                .field("provider", provider)
                .field("endpoint", endpoint)
                .field("header_count", &names.len())
                .finish(),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum ConfigCredentialCommands {
    Set {
        profile: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        stdin: bool,
        #[arg(
            long,
            help = "Raw credential material (not recommended for secrets; prefer --stdin to avoid shell history/process args leakage)."
        )]
        material: Option<String>,
    },
    List,
    Remove {
        profile: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigModelCommands {
    /// List resolved models (JSON array of ProviderModelEntry)
    List {
        /// Restrict to one provider id from `config model-providers list`
        #[arg(long)]
        provider: Option<String>,
        /// Restrict to models callable for this media operation
        #[arg(long, value_name = "view_image|generate_image")]
        operation: Option<ModelOperationArg>,
        /// Include models that are not selectable (diagnostic view)
        #[arg(
            long,
            help = "Include non-selectable models (equivalent to include_unavailable=true)"
        )]
        all: bool,
    },
    Refresh {
        #[arg(long)]
        provider: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigModelProviderCommands {
    /// List resolved model providers (mirrors the ListModelProviders tool)
    List {
        /// Restrict to providers with a selectable model for this media operation
        #[arg(long, value_name = "view_image|generate_image")]
        operation: Option<ModelOperationArg>,
        /// Include unavailable providers (equivalent to include_unavailable=true)
        #[arg(
            long,
            help = "Include unavailable providers (equivalent to include_unavailable=true)"
        )]
        all: bool,
    },
}

/// `--operation` values shared by the model listing commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ModelOperationArg {
    #[value(name = "view_image")]
    ViewImage,
    #[value(name = "generate_image")]
    GenerateImage,
}

impl ModelOperationArg {
    pub fn into_media_operation(self) -> MediaOperation {
        match self {
            Self::ViewImage => MediaOperation::ViewImage,
            Self::GenerateImage => MediaOperation::GenerateImage,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommands {
    Attach {
        #[arg(long)]
        agent: Option<String>,
        path: PathBuf,
    },
    Exit {
        #[arg(long)]
        agent: Option<String>,
    },
    Detach {
        #[arg(long)]
        agent: Option<String>,
        workspace_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TaskCommands {
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        agent: Option<String>,
    },
    Run {
        summary: String,
        #[arg(long)]
        cmd: String,
        #[arg(long)]
        workdir: Option<String>,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long)]
        login: Option<bool>,
        #[arg(long)]
        tty: bool,
        #[arg(long)]
        yield_time_ms: Option<u64>,
        #[arg(long)]
        max_output_tokens: Option<u64>,
        #[arg(long)]
        agent: Option<String>,
    },
    Status {
        task_id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    Input {
        task_id: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        agent: Option<String>,
    },
    Stop {
        task_id: String,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EventPageOrderCli {
    Asc,
    Desc,
}

impl std::fmt::Display for EventPageOrderCli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Asc => f.write_str("asc"),
            Self::Desc => f.write_str("desc"),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum EventsCommands {
    #[command(
        about = "Fetch a bounded page of stable event envelopes",
        long_about = "Fetch a bounded page of stable runtime event envelopes.\n\nThe stable fields are the event envelope fields emitted by the API, including sequence, identity, timestamps, provenance metadata, and full event payloads. Display-level filtering belongs in clients; this command returns raw event envelopes."
    )]
    Tail {
        #[arg(long)]
        before_seq: Option<u64>,
        #[arg(long)]
        after_seq: Option<u64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum, default_value_t = EventPageOrderCli::Desc)]
        order: EventPageOrderCli,
        #[arg(long)]
        agent: Option<String>,
        #[arg(
            long,
            help = "Read directly from the local runtime database without requiring a running daemon"
        )]
        offline: bool,
    },
    #[command(
        about = "Stream stable event envelopes as newline-delimited JSON",
        long_about = "Stream stable runtime event envelopes as newline-delimited JSON.\n\nThe stream is the raw event feed. Filtering is supported on `holon events tail`, not on the live stream."
    )]
    Stream {
        #[arg(long)]
        after_seq: Option<u64>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, value_parser = parse_positive_usize)]
        max_events: Option<usize>,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorkItemCommands {
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        agent: Option<String>,
    },
    Get {
        work_item_id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    Create {
        objective: String,
        #[arg(long)]
        agent: Option<String>,
    },
    Pick {
        work_item_id: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        clear_blocker: bool,
        #[arg(long)]
        agent: Option<String>,
    },
    Update {
        work_item_id: String,
        #[arg(long)]
        objective: Option<String>,
        #[arg(long)]
        plan_status: Option<String>,
        #[arg(long)]
        todo_json: Option<String>,
        #[arg(long)]
        blocked_by_json: Option<String>,
        #[arg(long)]
        recheck_after: Option<u64>,
        #[arg(long)]
        agent: Option<String>,
    },
    Complete {
        work_item_id: String,
        #[arg(long, conflicts_with = "report_file")]
        report: Option<String>,
        #[arg(long, conflicts_with = "report")]
        report_file: Option<PathBuf>,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MemoryIndexCommands {
    #[command(about = "Request a full rebuild of the local memory search index")]
    Rebuild {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(
            long,
            help = "Run the rebuild directly instead of submitting it to the background indexer"
        )]
        offline: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum DebugCommands {
    Prompt {
        text: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(
            long = "authority-class",
            alias = "trust",
            default_value = "operator-instruction"
        )]
        authority_class: AuthorityClass,
        #[arg(
            long,
            help = "Output canonical ProjectionManifest JSON instead of the text dump"
        )]
        manifest: bool,
        #[arg(
            long,
            help = "Override the prompt budget (estimated tokens) for this debug prompt"
        )]
        budget: Option<usize>,
    },
    Latency {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long, default_value_t = 5000)]
        events_limit: usize,
    },
    Performance {
        #[arg(long)]
        json: bool,
    },
    Trace {
        #[arg(conflicts_with = "search")]
        trace_id: Option<String>,
        #[arg(long, conflicts_with = "trace_id")]
        search: Option<String>,
        #[arg(long)]
        json: bool,
    },
    RuntimeDb {
        #[command(subcommand)]
        command: RuntimeDbDebugCommands,
    },
    SchedulerFixture {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        output: PathBuf,
    },
    #[command(about = "Inspect scheduler bootstrap recovery candidates without applying repairs")]
    SchedulerRecovery {
        #[arg(long, conflicts_with = "all_affected")]
        agent: Option<String>,
        #[arg(long, conflicts_with = "all_affected")]
        message_id: Option<String>,
        #[arg(long)]
        all_affected: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        apply: bool,
        #[arg(
            long,
            requires_all = ["apply", "agent"],
            conflicts_with_all = ["all_affected", "message_id"]
        )]
        rearm_task_result: Option<String>,
        #[arg(long, requires = "apply")]
        no_backup: bool,
    },
    #[command(hide = true)]
    SchedulerRecoveryFixture {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        objective: String,
        #[arg(long)]
        json: bool,
    },
    #[command(hide = true)]
    SchedulerRestartFixture {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        checkpoint: String,
        #[arg(long, default_value = "prepare")]
        stage: String,
        #[arg(long)]
        objective: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ModelsDevCommands {
    #[command(
        about = "Fetch the models.dev snapshot, generate artifact, and write to models.dev/"
    )]
    Refresh {
        #[arg(long, default_value = "https://models.dev/api.json")]
        url: String,
        #[arg(long, default_value = "models.dev")]
        output_dir: PathBuf,
        /// Read and write the supplement at this path instead of under the output directory.
        #[arg(long)]
        supplement_path: Option<PathBuf>,
    },
    #[command(about = "Validate the checked-in models.dev snapshot and artifact")]
    Validate {
        #[arg(long, default_value = "models.dev")]
        input_dir: PathBuf,
        /// Validate a supplement outside the input directory.
        #[arg(long)]
        supplement_path: Option<PathBuf>,
    },
    #[command(about = "Audit provider mappings against a snapshot")]
    Audit {
        #[arg(long, default_value = "models.dev/snapshot.json")]
        snapshot: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Report checked-in route metadata and exact-endpoint upstream review candidates (offline)"
    )]
    Coverage {
        #[arg(long, default_value = "models.dev/snapshot.json")]
        snapshot: PathBuf,
        #[arg(long, default_value = "models.dev/metadata-review.json")]
        baseline: PathBuf,
        #[arg(long)]
        check: bool,
        #[arg(long)]
        json: bool,
    },
}

/// `holon debug runtime-db work-item-source-revision` arguments.
///
/// The `apply_target` group keeps `--batch` and `--work-item` mutually
/// exclusive while `--apply` must pick one of them. The single-item path
/// still needs `--agent`, which clap cannot express while `--batch`
/// legitimately omits it; [`Self::validate`] enforces that before locks
/// are taken.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("apply_target").args(["work_item", "batch"])
))]
pub struct WorkItemSourceRevisionArgs {
    #[arg(long, requires = "apply_target")]
    pub apply: bool,
    #[arg(long, requires = "apply")]
    pub no_backup: bool,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub work_item: Option<String>,
    #[arg(long, requires = "apply")]
    pub batch: bool,
    #[arg(long)]
    pub json: bool,
}

impl WorkItemSourceRevisionArgs {
    /// Parse-level rules cover the batch/single split; this enforces the
    /// remaining single-apply requirements.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.apply && !self.batch {
            if self.agent.is_none() {
                anyhow::bail!("--apply without --batch requires --agent");
            }
            if self.work_item.is_none() {
                anyhow::bail!("--apply without --batch requires --work-item");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Subcommand)]
pub enum RuntimeDbDebugCommands {
    #[command(
        about = "Report or backfill canonical agent relation and policy records from durable legacy evidence"
    )]
    AgentRelations {
        #[arg(long)]
        apply: bool,
        #[arg(long, requires = "apply")]
        no_backup: bool,
        #[arg(long, default_value_t = 20, value_parser = parse_positive_usize)]
        diagnostic_sample_limit: usize,
        #[arg(long)]
        json: bool,
    },
    Audit {
        #[arg(long, value_enum, default_value_t = RuntimeDbAuditCheckArg::All)]
        check: RuntimeDbAuditCheckArg,
        #[arg(long, value_name = "RFC3339")]
        baseline_through: Option<String>,
        #[arg(long, default_value_t = 5, value_parser = parse_positive_usize)]
        sample_limit: usize,
        #[arg(long)]
        json: bool,
    },
    Retention {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    Compact {
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Preflight or remove the data-only v66 conversation input assignment repair marker"
    )]
    ConversationInputAssignmentRollback {
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Prepare or apply a resumable repair for WaitFor final Brief publication linkage"
    )]
    WaitFinalBriefPublication {
        #[arg(long, requires = "plan", conflicts_with = "resume")]
        apply: bool,
        #[arg(long, requires = "apply")]
        no_backup: bool,
        #[arg(long, value_name = "PATH")]
        plan: Option<PathBuf>,
        #[arg(long, requires = "plan", conflicts_with = "apply")]
        resume: bool,
        #[arg(long, conflicts_with_all = ["apply", "resume"])]
        agent: Option<String>,
        #[arg(long, default_value_t = 20, value_parser = parse_positive_usize)]
        diagnostic_sample_limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Audit or apply an offline, fingerprint-fenced repair for orphaned terminal Turn settlements"
    )]
    TurnSettlement {
        #[arg(long, requires = "plan", conflicts_with_all = ["agent", "turn"])]
        apply: bool,
        #[arg(long, requires = "apply")]
        no_backup: bool,
        #[arg(long, value_name = "PATH")]
        plan: Option<PathBuf>,
        #[arg(long, conflicts_with = "apply")]
        agent: Option<String>,
        #[arg(long, conflicts_with = "apply")]
        turn: Option<String>,
        #[arg(long, default_value_t = 20, value_parser = parse_positive_usize)]
        diagnostic_sample_limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Audit or apply an offline, evidence-fenced repair for WorkItem execution source revision drift"
    )]
    WorkItemSourceRevision(WorkItemSourceRevisionArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RuntimeDbAuditCheckArg {
    ProjectionEffects,
    BriefIntegrity,
    All,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommands {
    List {
        /// Filter to direct children of this parent agent; `self` means the
        /// current caller in agent mode.
        #[arg(long)]
        parent: Option<String>,
    },
    Get {
        agent_id: Option<String>,
        #[arg(long, value_enum, default_value_t = AgentDetailArg::Compact)]
        detail: AgentDetailArg,
    },
    Status {
        agent_id: Option<String>,
        #[arg(long, value_enum, default_value_t = AgentDetailArg::Compact)]
        detail: AgentDetailArg,
    },
    Create {
        agent_id: String,
        #[arg(long)]
        template: Option<String>,
    },
    /// Rename a public self-owned agent (display name only; the agent id is permanent)
    #[command(name = "rename")]
    Rename {
        agent_id: String,
        /// New display name (trimmed, 1-64 chars, no control characters or `/ \ :`)
        #[arg(long = "name", value_name = "NAME")]
        name: String,
    },
    /// Retry incomplete post-create bootstrap steps
    Repair {
        agent_id: String,
    },
    Start {
        agent_id: Option<String>,
    },
    Stop {
        agent_id: Option<String>,
    },
    /// Permanently delete an agent and its associated data
    #[command(name = "delete")]
    Delete {
        agent_id: String,
        /// Skip interactive confirmation (required in non-interactive mode)
        #[arg(short = 'y', long)]
        yes: bool,
        /// Also delete private supervised child agents
        #[arg(long)]
        cascade_private_children: bool,
        /// Export private history and AgentHome under the server's archives directory before cleanup
        #[arg(long)]
        archive: bool,
        /// Wait for the deletion job to complete
        #[arg(long)]
        wait: bool,
        /// Timeout in seconds for --wait (default: 300)
        #[arg(long)]
        timeout: Option<u64>,
        /// Explicitly resolve a stopped deletion instead of starting a new one
        #[arg(long, value_enum, value_name = "DECISION")]
        resolve: Option<DeletionResolveDecision>,
        /// Output structured JSON
        #[arg(long)]
        json: bool,
    },
    Abort {
        agent_id: Option<String>,
    },
    Model {
        #[command(subcommand)]
        command: AgentModelCommands,
    },
    /// Manage an agent's timezone override (operator mode only)
    Timezone {
        #[command(subcommand)]
        command: AgentTimezoneCommands,
    },
    /// Reset the external trigger callback for an agent (revoke old token + generate new)
    ResetCallback {
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentTimezoneCommands {
    Get {
        agent_id: Option<String>,
    },
    Set {
        timezone: String,
        agent_id: Option<String>,
    },
    Clear {
        agent_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentModelCommands {
    Get {
        agent_id: Option<String>,
    },
    /// Set the model override for an agent.
    ///
    /// Omitted override flags keep the agent's current reasoning-effort and
    /// service-tier overrides, a passed value replaces them, and `clear`
    /// removes them.
    Set {
        model: String,
        agent_id: Option<String>,
        /// Reasoning effort for the new override (model-supported values,
        /// e.g. `low`, `medium`, `high`, `xhigh`). Omitted: keep the existing
        /// override. `clear`: remove it.
        #[arg(long, value_name = "VALUE|clear")]
        reasoning_effort: Option<String>,
        /// Service tier for the new override (`default`, `fast` alias
        /// `priority`, `ultrafast`). Omitted: keep the existing override.
        /// `clear`: remove it.
        #[arg(long, value_name = "VALUE|clear")]
        service_tier: Option<String>,
    },
    Clear {
        agent_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommands {
    #[command(about = "List skills enabled/effective for an agent")]
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "List the local Skill Library catalog")]
    Catalog,
    #[command(about = "Add or import a skill into the local Skill Library")]
    Add {
        source: String,
        #[arg(long)]
        remote: bool,
        #[arg(long)]
        skill: Option<String>,
        #[arg(long)]
        copy: bool,
    },
    #[command(about = "Remove a skill from the local Skill Library")]
    Remove { name: String },
    #[command(about = "Reconcile local Skill Library contents with .skill-lock.json")]
    Reconcile { name: Option<String> },
    #[command(about = "Refresh runtime catalog by rescanning local skill roots")]
    Refresh,
    #[command(about = "Update remote Skill Library entries using .skill-lock.json metadata")]
    Update { name: Option<String> },
    #[command(about = "Check Skill Library and lock-file consistency")]
    Check { name: Option<String> },
    #[command(about = "Enable a locally known skill for an agent")]
    Enable {
        name: String,
        #[arg(long)]
        copy: bool,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "Disable a skill for an agent")]
    Disable {
        name: String,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "Compatibility alias for enabling or importing a skill for an agent")]
    Install {
        name_or_path: String,
        #[arg(long)]
        remote: bool,
        #[arg(long)]
        skill: Option<String>,
        #[arg(long)]
        copy: bool,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(about = "Compatibility alias for disabling a skill for an agent")]
    Uninstall {
        name: String,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TemplateCommands {
    #[command(about = "List the visible Agent template catalog")]
    Catalog,
    #[command(about = "Show one Agent template catalog entry with full detail")]
    Show {
        #[arg(value_name = "CATALOG_ID")]
        catalog_id: String,
    },
}

// ---------------------------------------------------------------------------
// Snapshot helpers
// ---------------------------------------------------------------------------

/// Return the full `clap::Command` tree for snapshot / contract testing.
pub fn build_command_tree() -> clap::Command {
    Cli::command()
}

/// Normalized command-tree entry used for deterministic snapshot output.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandSnapshotEntry {
    /// Dot-separated command path (e.g. "daemon.start")
    pub path: String,
    /// Positional arguments (name: value_name)
    pub positionals: Vec<PositionalInfo>,
    /// Long options with optional default / possible-values
    pub flags: Vec<FlagInfo>,
    /// Visible aliases (clap aliases)
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PositionalInfo {
    pub name: String,
    pub value_name: String,
    /// Clap positional index (preserves user-facing argument order).
    pub index: Option<usize>,
    pub required: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FlagInfo {
    pub long: String,
    pub short: Option<String>,
    pub default_value: Option<String>,
    pub possible_values: Option<Vec<String>>,
    pub required: bool,
}

/// Collect the full command tree as normalized snapshot entries.
pub fn collect_snapshot() -> Vec<CommandSnapshotEntry> {
    let cmd = build_command_tree();
    let mut entries: Vec<CommandSnapshotEntry> = Vec::new();
    collect_snapshot_recursive(&cmd, String::new(), &mut entries);
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    for entry in &mut entries {
        entry.positionals.sort_by(|a, b| a.index.cmp(&b.index));
        entry.flags.sort_by(|a, b| a.long.cmp(&b.long));
        entry.aliases.sort();
    }
    entries
}

fn collect_snapshot_recursive(
    cmd: &clap::Command,
    parent_path: String,
    entries: &mut Vec<CommandSnapshotEntry>,
) {
    // Skip the root node (empty path) from top-level collection, then recurse.
    if !parent_path.is_empty() {
        let entry = snapshot_entry_for_command(cmd, &parent_path);
        entries.push(entry);
    }

    for sub in cmd.get_subcommands() {
        let child_path = if parent_path.is_empty() {
            sub.get_name().to_string()
        } else {
            format!("{}.{}", parent_path, sub.get_name())
        };
        collect_snapshot_recursive(sub, child_path, entries);
    }
}

fn snapshot_entry_for_command(cmd: &clap::Command, path: &str) -> CommandSnapshotEntry {
    let positionals: Vec<PositionalInfo> = cmd
        .get_positionals()
        .filter(|p| p.get_id() != "help" && p.get_id() != "version")
        .map(|p| PositionalInfo {
            name: p.get_id().to_string(),
            value_name: p
                .get_value_names()
                .map(|names| {
                    names
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default(),
            required: p.is_required_set(),
            index: p.get_index(),
        })
        .collect();

    let flags: Vec<FlagInfo> = cmd
        .get_opts()
        .map(|o| {
            let default_value = o
                .get_default_values()
                .first()
                .map(|v| v.to_string_lossy().into_owned());
            let possible_values: Option<Vec<String>> = o
                .get_value_parser()
                .possible_values()
                .map(|pvs| pvs.map(|pv| pv.get_name().to_string()).collect());
            FlagInfo {
                long: o.get_long().unwrap_or("").to_string(),
                short: o.get_short().map(|c| format!("-{}", c)),
                default_value,
                possible_values: possible_values.filter(|v| !v.is_empty()),
                required: o.is_required_set(),
            }
        })
        .collect();

    let aliases: Vec<String> = cmd.get_visible_aliases().map(|a| a.to_string()).collect();

    CommandSnapshotEntry {
        path: path.to_string(),
        positionals,
        flags,
        aliases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_service_tier_flag_accepts_supported_values_and_lists_them_on_error() {
        assert!(matches!(
            parse_service_tier_flag("default"),
            Ok(crate::config::ServiceTier::Default)
        ));
        assert!(matches!(
            parse_service_tier_flag("fast"),
            Ok(crate::config::ServiceTier::Fast)
        ));
        assert!(matches!(
            parse_service_tier_flag("priority"),
            Ok(crate::config::ServiceTier::Fast)
        ));
        assert!(matches!(
            parse_service_tier_flag("ultrafast"),
            Ok(crate::config::ServiceTier::Ultrafast)
        ));
        let error = parse_service_tier_flag("bogus").expect_err("invalid tier must fail");
        assert!(error.contains("default"), "error lists 'default': {error}");
        assert!(error.contains("fast"), "error lists 'fast': {error}");
        assert!(
            error.contains("ultrafast"),
            "error lists 'ultrafast': {error}"
        );
        assert!(error.contains("clear"), "error mentions 'clear': {error}");
    }

    #[test]
    fn resolve_effort_override_tri_state_semantics() {
        // Omitted flag keeps the current override.
        assert_eq!(
            resolve_effort_override(None, Some("xhigh")),
            (Some("xhigh".to_string()), false)
        );
        assert_eq!(resolve_effort_override(None, None), (None, false));
        // `clear` removes the override.
        assert_eq!(
            resolve_effort_override(Some("clear"), Some("xhigh")),
            (None, true)
        );
        // Any other value replaces it.
        assert_eq!(
            resolve_effort_override(Some("low"), Some("xhigh")),
            (Some("low".to_string()), false)
        );
        assert_eq!(
            resolve_effort_override(Some("medium"), None),
            (Some("medium".to_string()), false)
        );
    }

    #[test]
    fn resolve_tier_override_tri_state_semantics() {
        use crate::config::ServiceTier;
        // Omitted flag keeps the current override.
        assert_eq!(
            resolve_tier_override(None, Some(ServiceTier::Fast)),
            Ok((Some(ServiceTier::Fast), false))
        );
        assert_eq!(resolve_tier_override(None, None), Ok((None, false)));
        // `clear` removes the override.
        assert_eq!(
            resolve_tier_override(Some("clear"), Some(ServiceTier::Fast)),
            Ok((None, true))
        );
        // Any other value replaces it (with parsing and alias support).
        assert_eq!(
            resolve_tier_override(Some("default"), Some(ServiceTier::Fast)),
            Ok((Some(ServiceTier::Default), false))
        );
        assert_eq!(
            resolve_tier_override(Some("priority"), None),
            Ok((Some(ServiceTier::Fast), false))
        );
        // Invalid values surface the parse error.
        assert!(resolve_tier_override(Some("bogus"), None).is_err());
    }

    #[test]
    fn agent_model_set_parses_override_flags() {
        let cli = Cli::try_parse_from([
            "holon",
            "agent",
            "model",
            "set",
            "openai/gpt-5.1",
            "--reasoning-effort",
            "xhigh",
            "--service-tier",
            "fast",
        ])
        .unwrap();
        let Commands::Agent {
            command:
                Some(AgentCommands::Model {
                    command:
                        AgentModelCommands::Set {
                            reasoning_effort,
                            service_tier,
                            ..
                        },
                }),
        } = cli.command
        else {
            panic!("expected agent model set");
        };
        assert_eq!(reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(service_tier.as_deref(), Some("fast"));

        let cli =
            Cli::try_parse_from(["holon", "agent", "model", "set", "openai/gpt-5.1"]).unwrap();
        let Commands::Agent {
            command:
                Some(AgentCommands::Model {
                    command:
                        AgentModelCommands::Set {
                            reasoning_effort,
                            service_tier,
                            ..
                        },
                }),
        } = cli.command
        else {
            panic!("expected agent model set");
        };
        assert_eq!(reasoning_effort, None);
        assert_eq!(service_tier, None);
    }

    #[test]
    fn provider_header_commands_parse_repeated_entries_and_redact_debug() {
        let cli = Cli::try_parse_from([
            "holon",
            "config",
            "providers",
            "headers",
            "set",
            "proxy",
            "--endpoint",
            "coding",
            "--header",
            "X-Example=private-payload=tail",
            "--header",
            "X-Empty=",
        ])
        .unwrap();
        assert!(!format!("{cli:?}").contains("private-payload"));
        let Commands::Config {
            command:
                ConfigCommands::Providers {
                    command:
                        ConfigProviderCommands::Headers {
                            command:
                                ConfigProviderHeaderCommands::Set {
                                    endpoint, headers, ..
                                },
                        },
                },
        } = cli.command
        else {
            panic!("expected provider headers set");
        };
        assert_eq!(endpoint.as_deref(), Some("coding"));
        assert_eq!(headers, ["X-Example=private-payload=tail", "X-Empty="]);
        assert!(
            Cli::try_parse_from(["holon", "config", "providers", "headers", "set", "proxy"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["holon", "config", "providers", "headers", "unset", "proxy"])
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "holon",
            "config",
            "providers",
            "headers",
            "unset",
            "proxy",
            "--name",
            "X-Example",
            "--name",
            "X-Other",
        ])
        .is_ok());
    }

    #[test]
    fn work_item_source_revision_audit_accepts_optional_filters() {
        for (agent, work_item) in [
            (None, None),
            (Some("agent-a"), None),
            (None, Some("work-a")),
            (Some("agent-a"), Some("work-a")),
        ] {
            let mut args = vec!["holon", "debug", "runtime-db", "work-item-source-revision"];
            if let Some(agent) = agent {
                args.extend(["--agent", agent]);
            }
            if let Some(work_item) = work_item {
                args.extend(["--work-item", work_item]);
            }
            let cli = Cli::try_parse_from(args).unwrap();
            let Commands::Debug {
                command:
                    DebugCommands::RuntimeDb {
                        command:
                            RuntimeDbDebugCommands::WorkItemSourceRevision(WorkItemSourceRevisionArgs {
                                apply,
                                no_backup,
                                agent: parsed_agent,
                                work_item: parsed_work_item,
                                ..
                            }),
                    },
            } = cli.command
            else {
                panic!("expected WorkItem source revision repair command");
            };
            assert!(!apply);
            assert!(!no_backup);
            assert_eq!(parsed_agent.as_deref(), agent);
            assert_eq!(parsed_work_item.as_deref(), work_item);
        }
    }

    #[test]
    fn work_item_source_revision_apply_accepts_required_filters() {
        for no_backup in [false, true] {
            let mut args = vec![
                "holon",
                "debug",
                "runtime-db",
                "work-item-source-revision",
                "--apply",
                "--agent",
                "agent-a",
                "--work-item",
                "work-a",
            ];
            if no_backup {
                args.push("--no-backup");
            }
            let cli = Cli::try_parse_from(args).unwrap();
            let Commands::Debug {
                command:
                    DebugCommands::RuntimeDb {
                        command:
                            RuntimeDbDebugCommands::WorkItemSourceRevision(WorkItemSourceRevisionArgs {
                                apply,
                                no_backup: parsed_no_backup,
                                agent,
                                work_item,
                                ..
                            }),
                    },
            } = cli.command
            else {
                panic!("expected WorkItem source revision repair command");
            };
            assert!(apply);
            assert_eq!(parsed_no_backup, no_backup);
            assert_eq!(agent.as_deref(), Some("agent-a"));
            assert_eq!(work_item.as_deref(), Some("work-a"));
        }
    }

    #[test]
    fn work_item_source_revision_rejects_incomplete_apply_or_audit_no_backup() {
        for options in [
            vec!["--apply"],
            vec!["--apply", "--agent", "agent-a"],
            vec!["--no-backup"],
            vec!["--no-backup", "--agent", "agent-a", "--work-item", "work-a"],
        ] {
            let mut args = vec!["holon", "debug", "runtime-db", "work-item-source-revision"];
            args.extend(options);
            let error = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument
            );
        }
    }

    #[test]
    fn work_item_source_revision_batch_flag_rules() {
        for options in [
            vec!["--apply", "--batch"],
            vec!["--apply", "--batch", "--agent", "agent-a"],
            vec!["--apply", "--batch", "--no-backup"],
            vec!["--apply", "--agent", "agent-a", "--work-item", "work-a"],
        ] {
            let mut args = vec!["holon", "debug", "runtime-db", "work-item-source-revision"];
            args.extend(options);
            let cli = Cli::try_parse_from(args).unwrap();
            let Commands::Debug {
                command:
                    DebugCommands::RuntimeDb {
                        command: RuntimeDbDebugCommands::WorkItemSourceRevision(args),
                    },
            } = cli.command
            else {
                panic!("expected WorkItem source revision repair command");
            };
            assert!(args.validate().is_ok());
        }
        // --batch and --work-item are mutually exclusive apply targets.
        let error = Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "work-item-source-revision",
            "--apply",
            "--batch",
            "--work-item",
            "work-a",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        // --batch only writes together with an explicit --apply.
        let error = Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "work-item-source-revision",
            "--batch",
        ])
        .unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn work_item_source_revision_single_apply_agent_rule_is_handler_level() {
        // `--apply --work-item work-a` parses: the apply-target group is
        // satisfied, and the missing --agent is reported by validate()
        // before any lock is taken.
        let cli = Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "work-item-source-revision",
            "--apply",
            "--work-item",
            "work-a",
        ])
        .unwrap();
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command: RuntimeDbDebugCommands::WorkItemSourceRevision(args),
                },
        } = cli.command
        else {
            panic!("expected WorkItem source revision repair command");
        };
        let error = args.validate().expect_err("single apply needs --agent");
        assert!(error.to_string().contains("requires --agent"));

        let single = WorkItemSourceRevisionArgs {
            apply: true,
            batch: false,
            agent: Some("agent-a".into()),
            work_item: Some("work-a".into()),
            no_backup: false,
            json: false,
        };
        assert!(single.validate().is_ok());
        let batch = WorkItemSourceRevisionArgs {
            apply: true,
            batch: true,
            agent: None,
            work_item: None,
            no_backup: false,
            json: false,
        };
        assert!(batch.validate().is_ok());
        let audit = WorkItemSourceRevisionArgs {
            apply: false,
            batch: false,
            agent: None,
            work_item: None,
            no_backup: false,
            json: false,
        };
        assert!(audit.validate().is_ok());
    }

    #[test]
    fn agent_delete_archive_is_explicit() {
        for (args, expected) in [
            (vec!["holon", "agent", "delete", "retired", "--yes"], false),
            (
                vec!["holon", "agent", "delete", "retired", "--yes", "--archive"],
                true,
            ),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            let Commands::Agent {
                command: Some(AgentCommands::Delete { archive, .. }),
            } = cli.command
            else {
                panic!("expected agent delete");
            };
            assert_eq!(archive, expected);
        }
    }

    /// Sanity: the command tree must be buildable without panicking.
    #[test]
    fn command_tree_builds() {
        let _cmd = build_command_tree();
    }

    #[test]
    fn tui_accepts_explicit_agent_locally_and_remotely() {
        for args in [
            vec!["holon", "tui", "--agent", "beta"],
            vec![
                "holon",
                "tui",
                "--connect",
                "https://example.test",
                "--agent",
                "beta",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.command, Commands::Tui { agent: Some(id), .. } if id == "beta"));
        }
        assert!(Cli::try_parse_from(["holon", "tui", "--agent"]).is_err());
        let cli = Cli::try_parse_from(["holon", "tui"]).unwrap();
        assert!(matches!(cli.command, Commands::Tui { agent: None, .. }));
    }

    /// Sanity: snapshot collection must produce entries for all commands.
    #[test]
    fn snapshot_collection_is_not_empty() {
        let entries = collect_snapshot();
        assert!(!entries.is_empty(), "snapshot entries should not be empty");

        // Check top-level commands are present.
        let top_level: Vec<&str> = entries
            .iter()
            .filter(|e| !e.path.contains('.'))
            .map(|e| e.path.as_str())
            .collect();
        assert!(
            top_level.contains(&"context"),
            "context should be in snapshot"
        );
        assert!(
            top_level.contains(&"commands"),
            "commands should be in snapshot"
        );
        assert!(top_level.contains(&"serve"), "serve should be in snapshot");
        assert!(top_level.contains(&"run"), "run should be in snapshot");
        assert!(top_level.contains(&"agent"), "agent should be in snapshot");
    }

    #[test]
    fn task_output_is_absent_from_metadata_and_help() {
        let entries = collect_snapshot();
        assert!(!entries.iter().any(|entry| entry.path == "task.output"));
        assert!(entries.iter().any(|entry| entry.path == "task.status"));
        let mut tree = build_command_tree();
        let task = tree.find_subcommand_mut("task").expect("task command");
        assert!(task.find_subcommand("output").is_none());
        let help = task.render_long_help().to_string();
        assert!(help.contains("status"));
        assert!(!help.contains("output"));
        assert!(Cli::try_parse_from(["holon", "task", "output", "task-old"]).is_err());
    }

    #[test]
    fn template_catalog_and_show_parse_as_read_only_subcommands() {
        let cli = Cli::try_parse_from(["holon", "template", "catalog"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Template {
                command: TemplateCommands::Catalog
            }
        ));

        let cli = Cli::try_parse_from(["holon", "template", "show", "user_global:code-reviewer"])
            .unwrap();
        let Commands::Template {
            command: TemplateCommands::Show { catalog_id },
        } = cli.command
        else {
            panic!("expected template show command");
        };
        assert_eq!(catalog_id, "user_global:code-reviewer");

        // `show` requires exactly one catalog id.
        assert!(Cli::try_parse_from(["holon", "template", "show"]).is_err());
        assert!(Cli::try_parse_from(["holon", "template", "show", "a", "b"]).is_err());

        let entries = collect_snapshot();
        assert!(entries.iter().any(|entry| entry.path == "template.catalog"));
        assert!(entries.iter().any(|entry| entry.path == "template.show"));
    }
}

#[cfg(test)]
mod prompt_tests;
