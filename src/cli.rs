//! CLI command tree definition for `holon`.
//!
//! This module owns the clap-derived types so that integration tests can
//! introspect the full command tree for snapshot / contract testing.

use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::types::AuthorityClass;

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(parsed) if parsed > 0 => Ok(parsed),
        Ok(_) => Err("value must be greater than zero".to_string()),
        Err(err) => Err(err.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Top-level CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(name = "holon")]
#[command(about = "A headless, event-driven runtime for long-lived agents")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

// ---------------------------------------------------------------------------
// Top-level commands
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum Commands {
    Serve {
        #[command(flatten)]
        options: ServeOptions,
    },
    Daemon {
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
    Status {
        #[arg(long)]
        agent: Option<String>,
    },
    Tail {
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
    Timer {
        #[arg(long)]
        after_ms: u64,
        #[arg(long)]
        every_ms: Option<u64>,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        agent: Option<String>,
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
}

// ---------------------------------------------------------------------------
// Shared enums / args structs
// ---------------------------------------------------------------------------

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
    Lan,
    Tailnet,
}

#[derive(Debug, Clone, Args)]
pub struct ServeOptions {
    #[arg(long, value_enum, default_value_t = ServeAccess::Local)]
    pub access: ServeAccess,
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long)]
    pub listen: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,
    #[arg(long)]
    pub advertise: Option<String>,
    #[arg(long)]
    pub token: Option<String>,
    #[arg(long)]
    pub token_file: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Subcommand enums
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum DaemonCommands {
    Start {
        #[command(flatten)]
        options: ServeOptions,
    },
    Stop,
    Status,
    Restart {
        #[command(flatten)]
        options: ServeOptions,
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
    List,
    Schema,
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum ConfigProviderCommands {
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
    List,
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
    Output {
        task_id: String,
        #[arg(long)]
        block: bool,
        #[arg(long)]
        timeout_ms: Option<u64>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EventProjectionCli {
    Operator,
    LocalDebug,
}

impl std::fmt::Display for EventProjectionCli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operator => f.write_str("operator"),
            Self::LocalDebug => f.write_str("local_debug"),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum EventsCommands {
    #[command(
        about = "Fetch a bounded page of stable event envelopes",
        long_about = "Fetch a bounded page of stable runtime event envelopes.\n\nThe stable fields are the event envelope fields emitted by the API, including sequence, identity, timestamps, origin/trust/priority metadata, and user-facing brief/data payloads. `holon tail` and `holon transcript` remain summary views over recent operator-facing history. Use `--projection local-debug` only for diagnostic/internal payloads; those fields are not a compatibility contract."
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
        #[arg(long, value_enum, default_value_t = EventProjectionCli::Operator)]
        projection: EventProjectionCli,
        #[arg(long)]
        agent: Option<String>,
    },
    #[command(
        about = "Stream stable event envelopes as newline-delimited JSON",
        long_about = "Stream stable runtime event envelopes as newline-delimited JSON.\n\nThe stable fields are the event envelope fields emitted by the API, including sequence, identity, timestamps, origin/trust/priority metadata, and user-facing brief/data payloads. `holon tail` and `holon transcript` remain summary views over recent operator-facing history. Use `--projection local-debug` only for diagnostic/internal payloads; those fields are not a compatibility contract."
    )]
    Stream {
        #[arg(long)]
        after_seq: Option<u64>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, value_enum, default_value_t = EventProjectionCli::Operator)]
        projection: EventProjectionCli,
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
    },
    Latency {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long, default_value_t = 5000)]
        events_limit: usize,
    },
    SchedulerFixture {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentCommands {
    List,
    Status {
        agent_id: Option<String>,
    },
    Create {
        agent_id: String,
        #[arg(long)]
        template: Option<String>,
    },
    Start {
        agent_id: Option<String>,
    },
    Stop {
        agent_id: Option<String>,
    },
    Abort {
        agent_id: Option<String>,
    },
    Model {
        #[command(subcommand)]
        command: AgentModelCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentModelCommands {
    Get {
        agent_id: Option<String>,
    },
    Set {
        model: String,
        agent_id: Option<String>,
    },
    Clear {
        agent_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommands {
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    Install {
        name_or_path: String,
        #[arg(long)]
        builtin: bool,
        #[arg(long)]
        remote: bool,
        #[arg(long)]
        skill: Option<String>,
        #[arg(long)]
        copy: bool,
        #[arg(long)]
        agent: Option<String>,
    },
    Uninstall {
        name: String,
        #[arg(long)]
        agent: Option<String>,
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

    /// Sanity: the command tree must be buildable without panicking.
    #[test]
    fn command_tree_builds() {
        let _cmd = build_command_tree();
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
        assert!(top_level.contains(&"serve"), "serve should be in snapshot");
        assert!(top_level.contains(&"run"), "run should be in snapshot");
        assert!(top_level.contains(&"agent"), "agent should be in snapshot");
    }
}
