use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use holon::{
    client::{normalize_control_base_url, LocalClient},
    config::{
        built_in_provider_default_config, config_schema, credential_store_path, default_holon_home,
        get_config_key, list_credential_profiles_at, load_credential_store_at,
        load_persisted_config_at, persisted_config_path, provider_config_view,
        provider_config_views, remove_credential_profile_at, save_persisted_config_at,
        set_config_key, set_credential_profile_at, unset_config_key, validate_provider_config,
        AppConfig, ControlAuthMode, CredentialKind, CredentialSource, ProviderAuthConfig,
        ProviderConfigFile, ProviderId, ProviderTransportKind,
    },
    daemon::{
        daemon_logs, daemon_restart, daemon_start, daemon_status, daemon_stop,
        ensure_serve_preflight, RuntimeServiceHandle,
    },
    host::RuntimeHost,
    http::{self, AppState, ControlRequest, CreateCommandTaskRequest, CreateTimerRequest},
    provider::{provider_doctor, resolved_model_availability},
    run_once::{run_once, RunOnceRequest},
    solve::{run_solve, SolveRequest},
    storage::AppStorage,
    tui::run_tui,
    types::{AuditEvent, ControlAction, TimerStatus, TrustLevel, WaitingIntentStatus},
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "holon")]
#[command(about = "A headless, event-driven runtime for long-lived agents")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
enum ControlCommandAction {
    Pause,
    Resume,
    Stop,
    Interrupt,
}

impl ControlCommandAction {
    fn as_control_action(&self) -> Option<ControlAction> {
        match self {
            Self::Pause => Some(ControlAction::Pause),
            Self::Resume => Some(ControlAction::Resume),
            Self::Stop => Some(ControlAction::Stop),
            Self::Interrupt => None,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
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
    Task {
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
        continue_on_result: bool,
        #[arg(long)]
        agent: Option<String>,
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
    #[command(about = "Deprecated: use `holon agent pause|resume|stop|interrupt [agent-id]`")]
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
        #[arg(long, default_value = "trusted-operator")]
        trust: TrustLevel,
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
        #[arg(long, default_value = "trusted-operator")]
        trust: TrustLevel,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ServeAccess {
    Local,
    Tunnel,
    Lan,
    Tailnet,
}

#[derive(Debug, Clone, Args)]
struct ServeOptions {
    #[arg(long, value_enum, default_value_t = ServeAccess::Local)]
    access: ServeAccess,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    listen: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    #[arg(long)]
    advertise: Option<String>,
    #[arg(long)]
    token: Option<String>,
    #[arg(long)]
    token_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum ConfigCommands {
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
enum ConfigModelCommands {
    List,
}

#[derive(Debug, Subcommand)]
enum ConfigProviderCommands {
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
enum ConfigCredentialCommands {
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
enum DaemonCommands {
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
enum WorkspaceCommands {
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
enum DebugCommands {
    Prompt {
        text: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value = "trusted-operator")]
        trust: TrustLevel,
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
enum AgentCommands {
    List,
    Status {
        agent_id: Option<String>,
    },
    Create {
        agent_id: String,
        #[arg(long)]
        template: Option<String>,
    },
    Pause {
        agent_id: Option<String>,
    },
    Resume {
        agent_id: Option<String>,
    },
    Stop {
        agent_id: Option<String>,
    },
    Interrupt {
        agent_id: Option<String>,
    },
    Model {
        #[command(subcommand)]
        command: AgentModelCommands,
    },
}

#[derive(Debug, Subcommand)]
enum AgentModelCommands {
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
enum SkillsCommands {
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    Install {
        name_or_path: String,
        #[arg(long)]
        builtin: bool,
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

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Commands::Config { command } => handle_config_command(command).await,
        Commands::Run {
            text,
            trust,
            json,
            agent,
            create_agent,
            template,
            max_turns,
            no_wait_for_tasks,
            home,
            workspace_root,
            cwd,
        } => {
            run_one_shot(
                text,
                trust,
                json,
                agent,
                create_agent,
                template,
                max_turns,
                no_wait_for_tasks,
                home,
                workspace_root,
                cwd,
            )
            .await
        }
        Commands::Solve {
            target_ref,
            repo,
            base,
            goal,
            role,
            agent,
            template,
            model,
            max_turns,
            trust,
            json,
            home,
            workspace,
            workspace_root,
            cwd,
            input,
            output,
        } => {
            run_solve_command(
                target_ref,
                repo,
                base,
                goal,
                role,
                agent,
                template,
                model,
                max_turns,
                trust,
                json,
                home,
                workspace.or(workspace_root),
                cwd,
                input,
                output,
            )
            .await
        }
        command => run_runtime_command(command).await,
    }
}

async fn run_runtime_command(command: Commands) -> Result<()> {
    if let Commands::Tui {
        no_alt_screen,
        connect: Some(connect),
        token,
        token_file,
        token_profile,
    } = command
    {
        let config = AppConfig::load_for_config_inspection()?;
        let token = resolve_remote_token(&config, token, token_file, token_profile)?;
        let client = LocalClient::remote(config.clone(), connect, token)?;
        client.handshake().await?;
        return run_tui(config, no_alt_screen, Some(client)).await;
    }

    let config = AppConfig::load()?;
    match command {
        Commands::Serve { options } => serve(config, options).await,
        Commands::Daemon { command } => handle_daemon_command(config, command).await,
        Commands::Prompt { text, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config)?;
            let response = client.control_prompt(&agent, text).await?;
            print_json(&serde_json::to_value(response)?)
        }
        Commands::Status { agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config)?;
            print_json(&serde_json::to_value(client.agent_status(&agent).await?)?)
        }
        Commands::Tail { limit, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config)?;
            print_json(&serde_json::to_value(
                client.agent_briefs(&agent, limit).await?,
            )?)
        }
        Commands::Transcript { limit, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config)?;
            print_json(&serde_json::to_value(
                client.agent_transcript(&agent, limit).await?,
            )?)
        }
        Commands::Task {
            summary,
            cmd,
            workdir,
            shell,
            login,
            tty,
            yield_time_ms,
            max_output_tokens,
            continue_on_result,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            post_control_json(
                &config,
                &format!("/control/agents/{agent}/tasks"),
                &CreateCommandTaskRequest {
                    summary,
                    cmd,
                    workdir,
                    shell,
                    login,
                    tty: Some(tty),
                    yield_time_ms,
                    max_output_tokens,
                    accepts_input: Some(false),
                    continue_on_result: Some(continue_on_result),
                    trust: Some(TrustLevel::TrustedOperator),
                },
            )
            .await
        }
        Commands::Timer {
            after_ms,
            every_ms,
            summary,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            post_control_json(
                &config,
                &format!("/control/agents/{agent}/timers"),
                &CreateTimerRequest {
                    duration_ms: after_ms,
                    interval_ms: every_ms,
                    summary,
                    trust: Some(TrustLevel::TrustedOperator),
                },
            )
            .await
        }
        Commands::Control { action, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            if action == ControlCommandAction::Interrupt {
                return post_control_json(
                    &config,
                    &format!("/control/agents/{agent}/current-run/interrupt"),
                    &http::InterruptCurrentRunRequest {
                        run_id: None,
                        mode: Some("pause_after_abort".into()),
                        trust: Some(TrustLevel::TrustedOperator),
                    },
                )
                .await;
            }
            post_control_json(
                &config,
                &format!("/control/agents/{agent}/control"),
                &ControlRequest {
                    action: action
                        .as_control_action()
                        .expect("interrupt action should be handled separately"),
                    trust: Some(TrustLevel::TrustedOperator),
                },
            )
            .await
        }
        Commands::Agent { command } => handle_agent_command(&config, command).await,
        Commands::Skills { command } => handle_skills_command(&config, command).await,
        Commands::Workspace { command } => handle_workspace_command(&config, command).await,
        Commands::Tui {
            no_alt_screen,
            connect: None,
            ..
        } => run_tui(config, no_alt_screen, None).await,
        Commands::Tui {
            connect: Some(_), ..
        } => unreachable!("remote tui is handled before runtime config load"),
        Commands::Run { .. } => unreachable!("run command is handled separately"),
        Commands::Solve { .. } => unreachable!("solve command is handled separately"),
        Commands::Debug { command } => handle_debug_command(config, command).await,
        Commands::Config { .. } => unreachable!("config commands are handled separately"),
    }
}

async fn run_one_shot(
    text: String,
    trust: TrustLevel,
    json: bool,
    agent_id: Option<String>,
    create_agent: bool,
    template: Option<String>,
    max_turns: Option<u64>,
    no_wait_for_tasks: bool,
    home: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
) -> Result<()> {
    let config = AppConfig::load_with_home(home)?;
    let response = run_once(
        config,
        RunOnceRequest {
            text,
            trust,
            agent_id,
            create_agent,
            template,
            max_turns,
            wait_for_tasks: !no_wait_for_tasks,
            workspace_root,
            cwd,
        },
    )
    .await?;

    if json {
        print_json(&serde_json::to_value(&response)?)?;
    } else {
        println!("{}", response.render_text());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_solve_command(
    target_ref: String,
    repo: Option<String>,
    base: Option<String>,
    goal: Option<String>,
    role: Option<String>,
    agent: Option<String>,
    template: Option<String>,
    model: Option<String>,
    max_turns: Option<u64>,
    trust: TrustLevel,
    json: bool,
    home: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
    input_dir: Option<PathBuf>,
    output_dir: Option<PathBuf>,
) -> Result<()> {
    if let Some(model) = model.as_deref().filter(|model| !model.trim().is_empty()) {
        std::env::set_var("HOLON_MODEL", model);
    }
    let config = AppConfig::load_with_home(home)?;
    let response = run_solve(
        config,
        SolveRequest {
            target_ref,
            repo,
            base,
            goal,
            role,
            agent_id: agent,
            template,
            max_turns,
            trust,
            json,
            workspace_root,
            cwd,
            input_dir,
            output_dir,
        },
    )
    .await?;

    if json {
        print_json(&serde_json::to_value(&response)?)?;
    } else {
        println!("{}", response.render_text());
    }
    Ok(())
}

fn apply_serve_options(config: &mut AppConfig, options: ServeOptions) -> Result<Option<String>> {
    let token = resolve_inline_or_file_token(options.token, options.token_file)?;
    if let Some(token) = token {
        config.control_token = Some(token);
    }

    if options.listen.is_some() && options.port.is_some() {
        return Err(anyhow!(
            "use only one of --listen or --port; use --listen ADDRESS:PORT for full control or --port with --host for a port-only override"
        ));
    }

    if let Some(listen) = options.listen {
        config.http_addr = listen;
    } else if options.access == ServeAccess::Tunnel {
        config.http_addr = loopback_with_configured_port(&config.http_addr, options.port);
    } else if matches!(options.access, ServeAccess::Lan | ServeAccess::Tailnet) {
        config.http_addr =
            default_remote_listen_addr(options.host.as_deref(), &config.http_addr, options.port);
    } else if options.port.is_some() {
        config.http_addr = loopback_with_configured_port(&config.http_addr, options.port);
    }

    let advertise_url = match options.advertise {
        Some(url) => Some(validate_client_visible_url("advertise URL", &url)?),
        None => match options.access {
            ServeAccess::Local | ServeAccess::Tunnel => None,
            ServeAccess::Lan | ServeAccess::Tailnet => {
                let Some(host) = options
                    .host
                    .as_deref()
                    .filter(|host| !host.trim().is_empty())
                else {
                    return Err(anyhow!(
                        "--access {:?} requires --host or --advertise",
                        options.access
                    ));
                };
                Some(format!(
                    "http://{}",
                    client_visible_host_port(host, &config.http_addr)?
                ))
            }
        },
    };
    if let Some(url) = &advertise_url {
        config.callback_base_url = url.clone();
    }
    config.callback_base_url =
        validate_client_visible_url("callback base URL", &config.callback_base_url)?;

    let non_loopback_tcp = !config.tcp_listener_is_local();
    if non_loopback_tcp
        && config
            .control_token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
    {
        return Err(anyhow!(
            "non-loopback TCP listen address {} requires --token, --token-file, or HOLON_CONTROL_TOKEN",
            config.http_addr
        ));
    }
    if matches!(options.access, ServeAccess::Lan | ServeAccess::Tailnet)
        && config
            .control_token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
    {
        return Err(anyhow!(
            "--access {:?} requires --token, --token-file, or HOLON_CONTROL_TOKEN",
            options.access
        ));
    }
    if non_loopback_tcp || matches!(options.access, ServeAccess::Lan | ServeAccess::Tailnet) {
        config.control_auth_mode = ControlAuthMode::Required;
    }

    Ok(advertise_url)
}

fn default_remote_listen_addr(
    host: Option<&str>,
    current: &str,
    explicit_port: Option<u16>,
) -> String {
    let port = explicit_port
        .or_else(|| {
            current
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse::<u16>().ok())
        })
        .unwrap_or(7878);
    if let Some(host) = host {
        if let Ok(addr) = host.parse::<SocketAddr>() {
            return addr.to_string();
        }
        if host.parse::<std::net::IpAddr>().is_ok() {
            return format!("{host}:{port}");
        }
    }
    format!("0.0.0.0:{port}")
}

fn loopback_with_configured_port(current: &str, explicit_port: Option<u16>) -> String {
    let port = explicit_port
        .or_else(|| {
            current
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse::<u16>().ok())
        })
        .unwrap_or(7878);
    format!("127.0.0.1:{port}")
}

fn client_visible_host_port(host: &str, listen: &str) -> Result<String> {
    let host = host.trim();
    let listen_port = listen
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(7878);
    let host_port = if host
        .rsplit_once(':')
        .is_some_and(|(_, tail)| tail.parse::<u16>().is_ok())
    {
        host.to_string()
    } else {
        format!("{host}:{listen_port}")
    };
    let url = validate_client_visible_url("host URL", &format!("http://{host_port}"))?;
    let Some(host) = reqwest::Url::parse(&url)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
    else {
        return Err(anyhow!("client-visible host must include a host"));
    };
    if matches!(host.as_str(), "0.0.0.0" | "::") {
        return Err(anyhow!(
            "0.0.0.0/:: is only valid for --listen; provide a client-reachable --host or --advertise"
        ));
    }
    Ok(url.trim_start_matches("http://").to_string())
}

fn validate_client_visible_url(label: &str, value: &str) -> Result<String> {
    let normalized = normalize_control_base_url(value.to_string(), label)
        .with_context(|| format!("invalid {label}"))?;
    Ok(normalized)
}

fn resolve_remote_token(
    config: &AppConfig,
    token: Option<String>,
    token_file: Option<PathBuf>,
    token_profile: Option<String>,
) -> Result<String> {
    let token = match (token, token_file, token_profile) {
        (Some(token), None, None) => token,
        (None, Some(path), None) => read_token_file(&path)?,
        (None, None, Some(profile)) => read_token_profile(config, &profile)?,
        (None, None, None) => {
            return Err(anyhow!(
                "remote TUI requires explicit --token, --token-file, or --token-profile"
            ))
        }
        _ => {
            return Err(anyhow!(
                "use exactly one of --token, --token-file, or --token-profile for remote TUI"
            ))
        }
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("remote TUI token must not be empty"));
    }
    Ok(token)
}

fn resolve_inline_or_file_token(
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<Option<String>> {
    match (token, token_file) {
        (Some(_), Some(_)) => Err(anyhow!("use only one of --token or --token-file")),
        (Some(token), None) => Ok(Some(non_empty_token(token)?)),
        (None, Some(path)) => Ok(Some(non_empty_token(read_token_file(&path)?)?)),
        (None, None) => Ok(None),
    }
}

fn read_token_file(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("failed to read token file {}", path.display()))
}

fn read_token_profile(config: &AppConfig, profile: &str) -> Result<String> {
    let profile = profile.trim();
    if profile.is_empty() {
        return Err(anyhow!("token profile must not be empty"));
    }
    let store = load_credential_store_at(&credential_store_path(&config.home_dir))?;
    store
        .profiles
        .get(profile)
        .map(|entry| entry.material.clone())
        .ok_or_else(|| anyhow!("token profile {profile:?} was not found"))
}

fn non_empty_token(token: String) -> Result<String> {
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("control token must not be empty"));
    }
    Ok(token)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonServeLaunchOptions {
    args: Vec<OsString>,
    control_token_env: Option<String>,
}

fn serve_args_for_options(options: &ServeOptions) -> DaemonServeLaunchOptions {
    let mut args = vec![
        OsString::from("--access"),
        OsString::from(
            options
                .access
                .to_possible_value()
                .expect("serve access should have clap value")
                .get_name(),
        ),
    ];
    if let Some(host) = &options.host {
        args.extend([OsString::from("--host"), OsString::from(host)]);
    }
    if let Some(listen) = &options.listen {
        args.extend([OsString::from("--listen"), OsString::from(listen)]);
    }
    if let Some(port) = options.port {
        args.extend([OsString::from("--port"), OsString::from(port.to_string())]);
    }
    if let Some(advertise) = &options.advertise {
        args.extend([OsString::from("--advertise"), OsString::from(advertise)]);
    }
    if let Some(token_file) = &options.token_file {
        args.extend([
            OsString::from("--token-file"),
            token_file.as_os_str().to_os_string(),
        ]);
    }
    DaemonServeLaunchOptions {
        args,
        control_token_env: options.token.clone(),
    }
}

async fn serve(mut config: AppConfig, options: ServeOptions) -> Result<()> {
    let advertise_url = apply_serve_options(&mut config, options)?;
    std::fs::create_dir_all(config.agent_root_dir())
        .with_context(|| format!("failed to create {}", config.agent_root_dir().display()))?;
    std::fs::create_dir_all(config.run_dir())
        .with_context(|| format!("failed to create {}", config.run_dir().display()))?;
    ensure_serve_preflight(&config).await?;

    let host = RuntimeHost::new(config.clone())?;
    host.default_runtime().await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;

    let tcp_router = http::router(
        AppState::for_tcp_with_runtime_service(host.clone(), Some(runtime_service.clone()))
            .with_advertise_url(advertise_url.clone()),
    );
    let listener = TcpListener::bind(&config.http_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.http_addr))?;
    println!("Holon listening on {}", listener.local_addr()?);
    if let Some(advertise_url) = &advertise_url {
        println!("Holon advertised at {advertise_url}");
    }

    #[cfg(unix)]
    {
        ensure_socket_parent(&config.socket_path)?;
        let unix_listener = tokio::net::UnixListener::bind(&config.socket_path)
            .with_context(|| format!("failed to bind {}", config.socket_path.display()))?;
        runtime_service.write_state_files(&config)?;
        println!("Holon control socket on {}", config.socket_path.display());
        let unix_router = http::router(AppState::for_unix_with_runtime_service(
            host.clone(),
            Some(runtime_service.clone()),
        ));
        let tcp_server = axum::serve(listener, tcp_router)
            .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()));
        let unix_server = http::serve_unix(
            unix_listener,
            unix_router,
            runtime_service.shutdown_signal(),
        );
        let result = tokio::try_join!(tcp_server, unix_server)
            .map(|_| ())
            .context("runtime servers failed");
        let _ = runtime_service.cleanup_state_files(&config);
        return result;
    }

    #[cfg(not(unix))]
    {
        runtime_service.write_state_files(&config)?;
        axum::serve(listener, tcp_router)
            .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()))
            .await
            .context("HTTP server failed")?;
        let _ = runtime_service.cleanup_state_files(&config);
        Ok(())
    }
}

fn ensure_socket_parent(socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

async fn wait_for_shutdown(mut shutdown: tokio::sync::watch::Receiver<bool>) {
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
            break;
        }
    }
}

async fn dump_prompt(
    config: AppConfig,
    text: String,
    agent: Option<String>,
    trust: TrustLevel,
) -> Result<()> {
    let host = RuntimeHost::new(config.clone())?;
    let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let runtime = host.get_or_create_agent(&agent).await?;
    let prompt = runtime.preview_prompt(text, trust).await?;
    println!("{}", prompt.render_dump());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use holon::config::{provider_registry_for_tests, AltScreenMode, ModelRef};

    fn test_config() -> AppConfig {
        let home = tempfile::tempdir().unwrap().keep();
        let workspace = tempfile::tempdir().unwrap().keep();
        AppConfig {
            default_agent_id: "default".into(),
            http_addr: "127.0.0.1:7878".into(),
            callback_base_url: "http://127.0.0.1:7878".into(),
            home_dir: home.clone(),
            data_dir: home.clone(),
            socket_path: home.join("run").join("holon.sock"),
            workspace_dir: workspace,
            context_window_messages: 8,
            context_window_briefs: 8,
            compaction_trigger_messages: 10,
            compaction_keep_recent_messages: 4,
            prompt_budget_estimated_tokens: 16_384,
            compaction_trigger_estimated_tokens: 8_192,
            compaction_keep_recent_estimated_tokens: 2_048,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            control_token: Some("secret".into()),
            control_auth_mode: ControlAuthMode::Auto,
            config_file_path: home.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            fallback_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: 8_000,
            max_tool_output_tokens: 64_000,
            disable_provider_fallback: false,
            tui_alternate_screen: AltScreenMode::Auto,
            validated_model_overrides: Default::default(),
            validated_unknown_model_fallback: None,
            providers: provider_registry_for_tests(None, Some("dummy"), home.join(".codex")),
        }
    }

    #[test]
    fn tailnet_host_is_client_visible_not_listen_socket() {
        let mut config = test_config();
        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                access: ServeAccess::Tailnet,
                host: Some("lab.tailnet.ts.net".into()),
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
            },
        )
        .unwrap();

        assert_eq!(config.http_addr, "0.0.0.0:7878");
        assert_eq!(advertise.as_deref(), Some("http://lab.tailnet.ts.net:7878"));
        assert_eq!(config.callback_base_url, "http://lab.tailnet.ts.net:7878");
        assert_eq!(config.control_auth_mode, ControlAuthMode::Required);
    }

    #[test]
    fn lan_port_updates_listen_and_advertise_urls() {
        let mut config = test_config();
        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                access: ServeAccess::Lan,
                host: Some("192.168.1.10".into()),
                listen: None,
                port: Some(8787),
                advertise: None,
                token: None,
                token_file: None,
            },
        )
        .unwrap();

        assert_eq!(config.http_addr, "192.168.1.10:8787");
        assert_eq!(advertise.as_deref(), Some("http://192.168.1.10:8787"));
        assert_eq!(config.callback_base_url, "http://192.168.1.10:8787");
        assert_eq!(config.control_auth_mode, ControlAuthMode::Required);
    }

    #[test]
    fn daemon_start_accepts_and_forwards_serve_options() {
        let cli = Cli::parse_from([
            "holon",
            "daemon",
            "start",
            "--access",
            "lan",
            "--host",
            "192.168.1.10",
            "--port",
            "8787",
            "--token-file",
            "/tmp/holon.token",
        ]);
        let Commands::Daemon {
            command: DaemonCommands::Start { options },
        } = cli.command
        else {
            panic!("expected daemon start command");
        };

        assert_eq!(options.access, ServeAccess::Lan);
        let serve_launch = serve_args_for_options(&options);
        assert_eq!(
            serve_launch.args,
            vec![
                OsString::from("--access"),
                OsString::from("lan"),
                OsString::from("--host"),
                OsString::from("192.168.1.10"),
                OsString::from("--port"),
                OsString::from("8787"),
                OsString::from("--token-file"),
                OsString::from("/tmp/holon.token"),
            ]
        );
        assert_eq!(serve_launch.control_token_env, None);
    }

    #[test]
    fn agent_lifecycle_commands_parse_with_optional_agent_id() {
        let cli = Cli::parse_from(["holon", "agent", "pause"]);
        let Commands::Agent {
            command: Some(AgentCommands::Pause { agent_id }),
        } = cli.command
        else {
            panic!("expected agent pause command");
        };
        assert_eq!(agent_id, None);

        let cli = Cli::parse_from(["holon", "agent", "resume", "foo"]);
        let Commands::Agent {
            command: Some(AgentCommands::Resume { agent_id }),
        } = cli.command
        else {
            panic!("expected agent resume command");
        };
        assert_eq!(agent_id.as_deref(), Some("foo"));

        let cli = Cli::parse_from(["holon", "agent", "stop", "foo"]);
        let Commands::Agent {
            command: Some(AgentCommands::Stop { agent_id }),
        } = cli.command
        else {
            panic!("expected agent stop command");
        };
        assert_eq!(agent_id.as_deref(), Some("foo"));

        let cli = Cli::parse_from(["holon", "agent", "interrupt", "foo"]);
        let Commands::Agent {
            command: Some(AgentCommands::Interrupt { agent_id }),
        } = cli.command
        else {
            panic!("expected agent interrupt command");
        };
        assert_eq!(agent_id.as_deref(), Some("foo"));
    }

    #[test]
    fn agent_model_commands_use_positional_agent_id() {
        let cli = Cli::parse_from(["holon", "agent", "model", "get", "foo"]);
        let Commands::Agent {
            command:
                Some(AgentCommands::Model {
                    command: AgentModelCommands::Get { agent_id },
                }),
        } = cli.command
        else {
            panic!("expected agent model get command");
        };
        assert_eq!(agent_id.as_deref(), Some("foo"));

        let cli = Cli::parse_from(["holon", "agent", "model", "set", "openai/gpt-5.1", "foo"]);
        let Commands::Agent {
            command:
                Some(AgentCommands::Model {
                    command: AgentModelCommands::Set { model, agent_id },
                }),
        } = cli.command
        else {
            panic!("expected agent model set command");
        };
        assert_eq!(model, "openai/gpt-5.1");
        assert_eq!(agent_id.as_deref(), Some("foo"));

        let cli = Cli::parse_from(["holon", "agents", "list"]);
        assert!(matches!(
            cli.command,
            Commands::Agent {
                command: Some(AgentCommands::List)
            }
        ));
    }

    #[test]
    fn debug_scheduler_fixture_command_parses_agent_and_output() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-fixture",
            "--agent",
            "pm",
            "--output",
            "/tmp/scheduler-case",
        ]);
        let Commands::Debug {
            command: DebugCommands::SchedulerFixture { agent, output },
        } = cli.command
        else {
            panic!("expected debug scheduler-fixture command");
        };
        assert_eq!(agent.as_deref(), Some("pm"));
        assert_eq!(output, PathBuf::from("/tmp/scheduler-case"));
    }

    #[test]
    fn export_scheduler_fixture_writes_replay_harness_shape() {
        let config = test_config();
        let agent_home = config.data_dir.join("agents/default");
        let storage = AppStorage::new(&agent_home).unwrap();
        let mut agent = holon::types::AgentState::new("default");
        agent.current_work_item_id = Some("work-1".into());
        storage.write_agent(&agent).unwrap();
        let mut work_item = holon::types::WorkItemRecord::new(
            "default",
            "fixture work",
            holon::types::WorkItemState::Open,
        );
        work_item.id = "work-1".into();
        work_item.revision = 2;
        storage.append_work_item(&work_item).unwrap();

        let output = tempfile::tempdir().unwrap();
        export_scheduler_fixture(&config, None, output.path()).unwrap();

        let agent_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output.path().join("agent.json")).unwrap())
                .unwrap();
        let expected_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output.path().join("expected.json")).unwrap())
                .unwrap();
        assert_eq!(agent_json["current_work_item_id"].as_str(), Some("work-1"));
        assert_eq!(
            expected_json["current_work_item_revision"].as_u64(),
            Some(2)
        );
        assert!(output.path().join("ledger/messages.jsonl").exists());
        assert!(output.path().join("ledger/tools.jsonl").exists());
        assert!(output.path().join("ledger/transcript.jsonl").exists());
    }

    #[test]
    fn daemon_start_passes_inline_token_through_env_not_argv() {
        let options = ServeOptions {
            access: ServeAccess::Lan,
            host: Some("192.168.1.10".into()),
            listen: None,
            port: None,
            advertise: None,
            token: Some("secret-token".into()),
            token_file: None,
        };

        let serve_launch = serve_args_for_options(&options);
        assert_eq!(
            serve_launch.control_token_env.as_deref(),
            Some("secret-token")
        );
        let token_flag = OsString::from("--token");
        let token_value = OsString::from("secret-token");
        assert!(!serve_launch
            .args
            .iter()
            .any(|arg| arg == &token_flag || arg == &token_value));
    }

    #[test]
    fn skills_install_relative_existing_path_is_absolutized_before_control_request() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills/demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Demo").unwrap();

        let kind = build_skill_install_kind_from_cwd("skills/demo", false, dir.path()).unwrap();

        assert_eq!(
            kind,
            holon::types::SkillInstallKind::Local {
                path: skill_dir,
                mode: holon::types::SkillInstallMode::Linked,
            }
        );
    }

    #[test]
    fn skills_install_unresolved_name_stays_named_request() {
        let dir = tempfile::tempdir().unwrap();

        let kind = build_skill_install_kind_from_cwd("ghx", true, dir.path()).unwrap();

        assert_eq!(
            kind,
            holon::types::SkillInstallKind::Named {
                name: "ghx".into(),
                mode: holon::types::SkillInstallMode::Copied,
            }
        );
    }

    #[test]
    fn skills_install_relative_existing_file_stays_named_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ghx"), "not a skill directory").unwrap();

        let kind = build_skill_install_kind_from_cwd("ghx", false, dir.path()).unwrap();

        assert_eq!(
            kind,
            holon::types::SkillInstallKind::Named {
                name: "ghx".into(),
                mode: holon::types::SkillInstallMode::Linked,
            }
        );
    }

    #[test]
    fn unspecified_listen_accepts_explicit_advertise_url() {
        let mut config = test_config();
        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                access: ServeAccess::Lan,
                host: None,
                listen: Some("0.0.0.0:7878".into()),
                port: None,
                advertise: Some("http://lab.example.test:7878".into()),
                token: None,
                token_file: None,
            },
        )
        .unwrap();

        assert_eq!(config.http_addr, "0.0.0.0:7878");
        assert_eq!(advertise.as_deref(), Some("http://lab.example.test:7878"));
        assert_eq!(config.callback_base_url, "http://lab.example.test:7878");
        assert_eq!(config.control_auth_mode, ControlAuthMode::Required);
    }

    #[test]
    fn callback_base_url_is_normalized_when_validated() {
        let mut config = test_config();
        config.callback_base_url = "http://lab.example.test:7878/#fragment".into();

        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                access: ServeAccess::Local,
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
            },
        )
        .unwrap();

        assert_eq!(advertise, None);
        assert_eq!(config.callback_base_url, "http://lab.example.test:7878");
    }

    #[test]
    fn latency_diagnostics_groups_turn_phase_events() {
        let at = |secs: i64| {
            chrono::DateTime::parse_from_rfc3339(&format!("2026-05-07T00:00:{secs:02}Z"))
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let event = |kind: &str, created_at, data| AuditEvent {
            id: uuid::Uuid::new_v4().to_string(),
            created_at,
            kind: kind.into(),
            data,
        };
        let events = vec![
            event(
                "message_admitted",
                at(0),
                serde_json::json!({ "message_id": "msg-1" }),
            ),
            event(
                "message_processing_started",
                at(2),
                serde_json::json!({ "id": "msg-1" }),
            ),
            event(
                "turn_started",
                at(2),
                serde_json::json!({
                    "turn_index": 42,
                    "message_id": "msg-1",
                    "run_id": "run-1",
                }),
            ),
            event(
                "turn_context_built",
                at(3),
                serde_json::json!({ "turn_index": 42, "duration_ms": 100 }),
            ),
            event(
                "provider_round_completed",
                at(4),
                serde_json::json!({
                    "turn_index": 42,
                    "round": 1,
                    "context_build_ms": 20,
                    "provider_round_ms": 300,
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "provider_attempt_timeline": {
                        "winning_model_ref": "anthropic/claude-sonnet-4-6",
                        "attempts": [{
                            "provider": "anthropic",
                            "model_ref": "anthropic/claude-sonnet-4-6",
                            "outcome": "succeeded"
                        }]
                    }
                }),
            ),
            event(
                "tool_executed",
                at(5),
                serde_json::json!({
                    "turn_index": 42,
                    "tool_name": "ExecCommand",
                    "duration_ms": 400,
                    "summary": "ok"
                }),
            ),
            event(
                "turn_terminal",
                at(6),
                serde_json::json!({
                    "turn_index": 42,
                    "kind": "completed",
                    "duration_ms": 1000
                }),
            ),
        ];

        let diagnostics = build_latency_diagnostics(&events, 10);

        assert_eq!(diagnostics.len(), 1);
        let turn = &diagnostics[0];
        assert_eq!(turn.turn_index, 42);
        assert_eq!(turn.queue_wait_ms, Some(2000));
        assert_eq!(turn.context_build_ms, 120);
        assert_eq!(turn.provider_context_build_ms, 20);
        assert_eq!(turn.provider_round_ms, 300);
        assert_eq!(turn.tool_execution_ms, 400);
        assert_eq!(turn.total_ms, Some(1000));
        assert_eq!(turn.runtime_cleanup_ms(), Some(280));
        assert_eq!(
            turn.provider_rounds[0].provider.as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            turn.provider_rounds[0].model_ref.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
    }
}

async fn handle_debug_command(config: AppConfig, command: DebugCommands) -> Result<()> {
    match command {
        DebugCommands::Prompt { text, agent, trust } => {
            dump_prompt(config, text, agent, trust).await
        }
        DebugCommands::Latency {
            agent,
            limit,
            events_limit,
        } => print_latency_diagnostics(&config, agent, limit, events_limit),
        DebugCommands::SchedulerFixture { agent, output } => {
            export_scheduler_fixture(&config, agent, &output)
        }
    }
}

fn export_scheduler_fixture(
    config: &AppConfig,
    agent: Option<String>,
    output: &Path,
) -> Result<()> {
    let agent_id = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let agent_home = config.data_dir.join("agents").join(&agent_id);
    let storage = AppStorage::new(&agent_home)?;
    let agent = storage.read_agent()?.with_context(|| {
        format!(
            "agent state not found for {agent_id} in {}",
            agent_home.display()
        )
    })?;
    let ledger_dir = output.join("ledger");
    fs::create_dir_all(&ledger_dir)
        .with_context(|| format!("failed to create {}", ledger_dir.display()))?;

    let pending_wake_hint_reason = agent
        .pending_wake_hint
        .as_ref()
        .map(|pending| pending.reason.clone());
    let last_turn_terminal_kind = agent
        .last_turn_terminal
        .as_ref()
        .map(|terminal| terminal.kind.clone());
    write_json_pretty(
        &output.join("agent.json"),
        &serde_json::json!({
            "current_work_item_id": agent.current_work_item_id,
            "pending_wake_hint_reason": pending_wake_hint_reason,
            "turn_index": agent.turn_index,
            "last_turn_terminal_kind": last_turn_terminal_kind,
        }),
    )?;

    let work_queue = storage.work_queue_prompt_projection()?;
    let active_tasks = storage.latest_active_task_records_for_agent(&agent.id, usize::MAX)?;
    let has_blocking_active_tasks = active_tasks.iter().any(|task| task.is_blocking());
    let active_waiting_intents = storage
        .latest_waiting_intents()?
        .into_iter()
        .filter(|intent| {
            intent.agent_id == agent.id && intent.status == WaitingIntentStatus::Active
        })
        .collect::<Vec<_>>();
    let active_timers = storage
        .latest_timer_records()?
        .into_iter()
        .filter(|timer| timer.agent_id == agent.id && timer.status == TimerStatus::Active)
        .collect::<Vec<_>>();
    let replay_message_ids = storage
        .recovery_snapshot()?
        .replay_messages
        .into_iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    write_json_pretty(
        &output.join("expected.json"),
        &serde_json::json!({
            "current_work_item_id": work_queue.current.as_ref().map(|item| item.id.clone()),
            "current_work_item_revision": work_queue.current.as_ref().map(|item| item.revision),
            "queued_work_items": work_queue.queued_blocked.iter().filter(|item| item.is_runnable()).count(),
            "active_tasks": active_tasks.len(),
            "has_blocking_active_tasks": has_blocking_active_tasks,
            "pending_wake_hint": agent.pending_wake_hint.is_some(),
            "active_waiting_intents": active_waiting_intents.len(),
            "active_work_item_waiting_intents": active_waiting_intents.iter().filter(|intent| matches!(intent.scope, holon::types::ExternalTriggerScope::WorkItem)).count(),
            "active_agent_waiting_intents": active_waiting_intents.iter().filter(|intent| matches!(intent.scope, holon::types::ExternalTriggerScope::Agent)).count(),
            "active_timers": active_timers.len(),
            "runtime_error": storage.read_recent_events(128)?.iter().any(|event| event.kind == "runtime_error"),
            "replay_message_ids": replay_message_ids,
            "turn_terminal_kind": last_turn_terminal_kind,
        }),
    )?;

    write_jsonl(
        &ledger_dir.join("messages.jsonl"),
        &storage.read_recent_messages(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("queue_entries.jsonl"),
        &storage.read_recent_queue_entries(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("events.jsonl"),
        &storage.read_recent_events(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("tasks.jsonl"),
        &storage.read_recent_tasks(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("work_items.jsonl"),
        &storage.read_recent_work_items(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("waiting_intents.jsonl"),
        &storage.read_recent_waiting_intents(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("timers.jsonl"),
        &storage.read_recent_timers(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("tools.jsonl"),
        &storage.read_recent_tool_executions(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("briefs.jsonl"),
        &storage.read_recent_briefs(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("transcript.jsonl"),
        &storage.read_recent_transcript(usize::MAX)?,
    )?;
    write_jsonl(
        &ledger_dir.join("external_triggers.jsonl"),
        &storage.read_recent_external_triggers(usize::MAX)?,
    )?;

    println!(
        "Exported scheduler fixture for agent {agent_id} to {}",
        output.display()
    );
    Ok(())
}

fn write_json_pretty(path: &Path, value: &serde_json::Value) -> Result<()> {
    let content = serde_json::to_vec_pretty(value)?;
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn write_jsonl<T: serde::Serialize>(path: &Path, values: &[T]) -> Result<()> {
    let mut content = Vec::new();
    for value in values {
        serde_json::to_writer(&mut content, value)?;
        content.push(b'\n');
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

#[derive(Debug, Default)]
struct TurnLatencyDiagnostics {
    turn_index: u64,
    run_id: Option<String>,
    message_id: Option<String>,
    terminal_kind: Option<String>,
    total_ms: Option<u64>,
    queue_wait_ms: Option<u64>,
    context_build_ms: u64,
    provider_context_build_ms: u64,
    provider_round_ms: u64,
    tool_execution_ms: u64,
    provider_rounds: Vec<ProviderRoundLatencyDiagnostics>,
    tools: Vec<ToolLatencyDiagnostics>,
}

impl TurnLatencyDiagnostics {
    fn runtime_cleanup_ms(&self) -> Option<u64> {
        self.total_ms.map(|total| {
            total.saturating_sub(
                self.provider_context_build_ms
                    .saturating_add(self.provider_round_ms)
                    .saturating_add(self.tool_execution_ms),
            )
        })
    }
}

#[derive(Debug)]
struct ProviderRoundLatencyDiagnostics {
    round: u64,
    duration_ms: u64,
    provider: Option<String>,
    model_ref: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Debug)]
struct ToolLatencyDiagnostics {
    tool_name: String,
    duration_ms: u64,
    summary: Option<String>,
}

fn print_latency_diagnostics(
    config: &AppConfig,
    agent: Option<String>,
    limit: usize,
    events_limit: usize,
) -> Result<()> {
    let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let agent_home = config.data_dir.join("agents").join(&agent);
    let storage = AppStorage::new(&agent_home)?;
    let events = storage.read_recent_events(events_limit)?;
    let diagnostics = build_latency_diagnostics(&events, limit);
    if diagnostics.is_empty() {
        println!(
            "No turn latency data found for agent {agent} in {}",
            agent_home.join(".holon/ledger/events.jsonl").display()
        );
        return Ok(());
    }
    for turn in diagnostics {
        let total = turn
            .total_ms
            .map(format_duration_ms)
            .unwrap_or_else(|| "unknown".into());
        let run = turn.run_id.as_deref().unwrap_or("unknown");
        let kind = turn.terminal_kind.as_deref().unwrap_or("unknown");
        println!(
            "turn {} run={} kind={} runtime_total={}",
            turn.turn_index, run, kind, total
        );
        println!(
            "  queue_wait        {}",
            turn.queue_wait_ms
                .map(format_duration_ms)
                .unwrap_or_else(|| "unknown".into())
        );
        println!(
            "  context_build     {}",
            format_duration_ms(turn.context_build_ms)
        );
        for provider_round in &turn.provider_rounds {
            let provider = provider_round.provider.as_deref().unwrap_or("provider");
            let model = provider_round.model_ref.as_deref().unwrap_or("model");
            let input = provider_round
                .input_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".into());
            let output = provider_round
                .output_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "  provider round {:<2} {}  {}/{} input={} output={}",
                provider_round.round,
                format_duration_ms(provider_round.duration_ms),
                provider,
                model,
                input,
                output
            );
        }
        for tool in &turn.tools {
            let summary = tool
                .summary
                .as_deref()
                .filter(|summary| !summary.trim().is_empty())
                .map(|summary| format!("  {}", summary.trim()))
                .unwrap_or_default();
            println!(
                "  tool {:<14} {}{}",
                tool.tool_name,
                format_duration_ms(tool.duration_ms),
                summary
            );
        }
        println!(
            "  tool_execution    {}",
            format_duration_ms(turn.tool_execution_ms)
        );
        println!(
            "  turn_cleanup      {}",
            turn.runtime_cleanup_ms()
                .map(format_duration_ms)
                .unwrap_or_else(|| "unknown".into())
        );
    }
    Ok(())
}

fn build_latency_diagnostics(events: &[AuditEvent], limit: usize) -> Vec<TurnLatencyDiagnostics> {
    let mut turns = BTreeMap::<u64, TurnLatencyDiagnostics>::new();
    let mut admitted_at_by_message = BTreeMap::<String, chrono::DateTime<chrono::Utc>>::new();
    let mut processing_at_by_message = BTreeMap::<String, chrono::DateTime<chrono::Utc>>::new();
    for event in events {
        match event.kind.as_str() {
            "message_admitted" => {
                if let Some(message_id) = event
                    .data
                    .get("message_id")
                    .and_then(|value| value.as_str())
                {
                    admitted_at_by_message.insert(message_id.to_string(), event.created_at);
                }
            }
            "message_processing_started" => {
                if let Some(message_id) = event.data.get("id").and_then(|value| value.as_str()) {
                    processing_at_by_message.insert(message_id.to_string(), event.created_at);
                }
            }
            "turn_started" => {
                if let Some(turn_index) = event
                    .data
                    .get("turn_index")
                    .and_then(|value| value.as_u64())
                {
                    let turn = turns
                        .entry(turn_index)
                        .or_insert_with(|| TurnLatencyDiagnostics {
                            turn_index,
                            ..TurnLatencyDiagnostics::default()
                        });
                    turn.message_id = event
                        .data
                        .get("message_id")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    turn.run_id = event
                        .data
                        .get("run_id")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                }
            }
            "turn_context_built" => {
                if let Some(turn_index) = event
                    .data
                    .get("turn_index")
                    .and_then(|value| value.as_u64())
                {
                    let turn = turns
                        .entry(turn_index)
                        .or_insert_with(|| TurnLatencyDiagnostics {
                            turn_index,
                            ..TurnLatencyDiagnostics::default()
                        });
                    turn.context_build_ms = turn.context_build_ms.saturating_add(
                        event
                            .data
                            .get("duration_ms")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0),
                    );
                    if turn.run_id.is_none() {
                        turn.run_id = event
                            .data
                            .get("run_id")
                            .and_then(|value| value.as_str())
                            .map(ToString::to_string);
                    }
                }
            }
            "provider_round_completed" => {
                if let Some(turn_index) = event
                    .data
                    .get("turn_index")
                    .and_then(|value| value.as_u64())
                {
                    let duration_ms = event
                        .data
                        .get("provider_round_ms")
                        .or_else(|| event.data.get("duration_ms"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let turn = turns
                        .entry(turn_index)
                        .or_insert_with(|| TurnLatencyDiagnostics {
                            turn_index,
                            ..TurnLatencyDiagnostics::default()
                        });
                    turn.provider_round_ms = turn.provider_round_ms.saturating_add(duration_ms);
                    let context_build_ms = event
                        .data
                        .get("context_build_ms")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    turn.context_build_ms = turn.context_build_ms.saturating_add(context_build_ms);
                    turn.provider_context_build_ms = turn
                        .provider_context_build_ms
                        .saturating_add(context_build_ms);
                    if turn.run_id.is_none() {
                        turn.run_id = event
                            .data
                            .get("run_id")
                            .and_then(|value| value.as_str())
                            .map(ToString::to_string);
                    }
                    let timeline = event.data.get("provider_attempt_timeline");
                    let winning = timeline
                        .and_then(|value| value.get("winning_model_ref"))
                        .and_then(|value| value.as_str());
                    let attempt = timeline
                        .and_then(|value| value.get("attempts"))
                        .and_then(|value| value.as_array())
                        .and_then(|attempts| {
                            attempts
                                .iter()
                                .rev()
                                .find(|attempt| {
                                    attempt.get("outcome").and_then(|value| value.as_str())
                                        == Some("succeeded")
                                })
                                .or_else(|| attempts.last())
                        });
                    turn.provider_rounds.push(ProviderRoundLatencyDiagnostics {
                        round: event
                            .data
                            .get("round")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0),
                        duration_ms,
                        provider: attempt
                            .and_then(|attempt| attempt.get("provider"))
                            .and_then(|value| value.as_str())
                            .map(ToString::to_string),
                        model_ref: winning
                            .or_else(|| {
                                attempt
                                    .and_then(|attempt| attempt.get("model_ref"))
                                    .and_then(|value| value.as_str())
                            })
                            .map(ToString::to_string),
                        input_tokens: event
                            .data
                            .get("input_tokens")
                            .and_then(|value| value.as_u64()),
                        output_tokens: event
                            .data
                            .get("output_tokens")
                            .and_then(|value| value.as_u64()),
                    });
                }
            }
            "tool_executed" => {
                if let Some(turn_index) = event
                    .data
                    .get("turn_index")
                    .and_then(|value| value.as_u64())
                {
                    let duration_ms = event
                        .data
                        .get("duration_ms")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let tool_name = event
                        .data
                        .get("tool_name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("tool")
                        .to_string();
                    let turn = turns
                        .entry(turn_index)
                        .or_insert_with(|| TurnLatencyDiagnostics {
                            turn_index,
                            ..TurnLatencyDiagnostics::default()
                        });
                    turn.tool_execution_ms = turn.tool_execution_ms.saturating_add(duration_ms);
                    turn.tools.push(ToolLatencyDiagnostics {
                        tool_name,
                        duration_ms,
                        summary: event
                            .data
                            .get("summary")
                            .and_then(|value| value.as_str())
                            .map(ToString::to_string),
                    });
                }
            }
            "turn_terminal" => {
                if let Some(turn_index) = event
                    .data
                    .get("turn_index")
                    .and_then(|value| value.as_u64())
                {
                    let turn = turns
                        .entry(turn_index)
                        .or_insert_with(|| TurnLatencyDiagnostics {
                            turn_index,
                            ..TurnLatencyDiagnostics::default()
                        });
                    turn.total_ms = event
                        .data
                        .get("duration_ms")
                        .and_then(|value| value.as_u64());
                    turn.terminal_kind = event
                        .data
                        .get("kind")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                }
            }
            _ => {}
        }
    }

    for turn in turns.values_mut() {
        if let Some(message_id) = turn.message_id.as_ref() {
            if let (Some(admitted_at), Some(processing_at)) = (
                admitted_at_by_message.get(message_id),
                processing_at_by_message.get(message_id),
            ) {
                turn.queue_wait_ms = processing_at
                    .signed_duration_since(*admitted_at)
                    .num_milliseconds()
                    .try_into()
                    .ok();
            }
        }
    }

    let mut values = turns.into_values().collect::<Vec<_>>();
    values.sort_by_key(|turn| turn.turn_index);
    values.into_iter().rev().take(limit).collect()
}

fn format_duration_ms(duration_ms: u64) -> String {
    if duration_ms >= 1000 {
        format!("{:.1}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{duration_ms}ms")
    }
}

async fn handle_daemon_command(config: AppConfig, command: DaemonCommands) -> Result<()> {
    let value = match command {
        DaemonCommands::Start { options } => {
            let mut config = config;
            let serve_launch = serve_args_for_options(&options);
            apply_serve_options(&mut config, options)?;
            serde_json::to_value(
                daemon_start(
                    &config,
                    &serve_launch.args,
                    serve_launch.control_token_env.as_deref(),
                )
                .await?,
            )?
        }
        DaemonCommands::Stop => serde_json::to_value(daemon_stop(&config).await?)?,
        DaemonCommands::Status => serde_json::to_value(daemon_status(&config).await?)?,
        DaemonCommands::Restart { options } => {
            let mut config = config;
            let serve_launch = serve_args_for_options(&options);
            apply_serve_options(&mut config, options)?;
            serde_json::to_value(
                daemon_restart(
                    &config,
                    &serve_launch.args,
                    serve_launch.control_token_env.as_deref(),
                )
                .await?,
            )?
        }
        DaemonCommands::Logs { tail } => serde_json::to_value(daemon_logs(&config, tail)?)?,
    };
    print_json(&value)
}

async fn handle_workspace_command(config: &AppConfig, command: WorkspaceCommands) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    match command {
        WorkspaceCommands::Attach { agent, path } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client
                    .attach_workspace(&agent, path.display().to_string())
                    .await?,
            )?)
        }
        WorkspaceCommands::Exit { agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(client.exit_workspace(&agent).await?)?)
        }
        WorkspaceCommands::Detach {
            agent,
            workspace_id,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client.detach_workspace(&agent, workspace_id).await?,
            )?)
        }
    }
}

async fn handle_agent_command(config: &AppConfig, command: Option<AgentCommands>) -> Result<()> {
    match command {
        None | Some(AgentCommands::List) => {
            let client = LocalClient::new(config.clone())?;
            print_json(&serde_json::to_value(client.list_agents().await?)?)
        }
        Some(AgentCommands::Status { agent_id }) => {
            let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config.clone())?;
            print_json(&serde_json::to_value(client.agent_status(&agent).await?)?)
        }
        Some(AgentCommands::Create { agent_id, template }) => {
            post_control_json(
                config,
                &format!("/control/agents/{agent_id}/create"),
                &http::CreateAgentRequest {
                    trust: Some(TrustLevel::TrustedOperator),
                    template,
                },
            )
            .await
        }
        Some(AgentCommands::Pause { agent_id }) => {
            control_agent_lifecycle(config, agent_id, ControlAction::Pause).await
        }
        Some(AgentCommands::Resume { agent_id }) => {
            control_agent_lifecycle(config, agent_id, ControlAction::Resume).await
        }
        Some(AgentCommands::Stop { agent_id }) => {
            control_agent_lifecycle(config, agent_id, ControlAction::Stop).await
        }
        Some(AgentCommands::Interrupt { agent_id }) => {
            let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
            post_control_json(
                config,
                &format!("/control/agents/{agent}/current-run/interrupt"),
                &http::InterruptCurrentRunRequest {
                    run_id: None,
                    mode: Some("pause_after_abort".into()),
                    trust: Some(TrustLevel::TrustedOperator),
                },
            )
            .await
        }
        Some(AgentCommands::Model { command }) => {
            let client = LocalClient::new(config.clone())?;
            match command {
                AgentModelCommands::Get { agent_id } => {
                    let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
                    let summary = client.agent_status(&agent).await?;
                    print_json(&serde_json::to_value(summary.model)?)
                }
                AgentModelCommands::Set { model, agent_id } => {
                    let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
                    print_json(&client.set_agent_model_override(&agent, model, None).await?)
                }
                AgentModelCommands::Clear { agent_id } => {
                    let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
                    print_json(&client.clear_agent_model_override(&agent).await?)
                }
            }
        }
    }
}

async fn control_agent_lifecycle(
    config: &AppConfig,
    agent_id: Option<String>,
    action: ControlAction,
) -> Result<()> {
    let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
    let client = LocalClient::new(config.clone())?;
    print_json(&client.control_agent(&agent, action).await?)
}

async fn handle_skills_command(config: &AppConfig, command: SkillsCommands) -> Result<()> {
    match command {
        SkillsCommands::List { agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let response: serde_json::Value =
                get_json(config, &format!("/agents/{agent}/skills")).await?;
            print_json(&response)
        }
        SkillsCommands::Install {
            name_or_path,
            builtin,
            copy,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let kind = if builtin {
                holon::types::SkillInstallKind::Builtin { name: name_or_path }
            } else {
                build_skill_install_kind(&name_or_path, copy)?
            };
            post_control_json(
                config,
                &format!("/control/agents/{agent}/skills/install"),
                &holon::types::InstallSkillRequest { kind },
            )
            .await
        }
        SkillsCommands::Uninstall { name, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            post_control_json(
                config,
                &format!("/control/agents/{agent}/skills/uninstall"),
                &holon::types::UninstallSkillRequest { name },
            )
            .await
        }
    }
}

fn build_skill_install_kind(
    name_or_path: &str,
    copy: bool,
) -> Result<holon::types::SkillInstallKind> {
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    build_skill_install_kind_from_cwd(name_or_path, copy, &cwd)
}

fn build_skill_install_kind_from_cwd(
    name_or_path: &str,
    copy: bool,
    cwd: &Path,
) -> Result<holon::types::SkillInstallKind> {
    let mode = if copy {
        holon::types::SkillInstallMode::Copied
    } else {
        holon::types::SkillInstallMode::Linked
    };
    let path = PathBuf::from(name_or_path);
    if path.is_absolute() {
        return Ok(holon::types::SkillInstallKind::Local { path, mode });
    }
    let resolved = cwd.join(&path);
    if resolved.is_dir() {
        Ok(holon::types::SkillInstallKind::Local {
            path: resolved,
            mode,
        })
    } else {
        Ok(holon::types::SkillInstallKind::Named {
            name: name_or_path.into(),
            mode,
        })
    }
}

async fn post_control_json<T: serde::Serialize>(
    config: &AppConfig,
    path: &str,
    payload: &T,
) -> Result<()> {
    post_json_with_auth(config, path, payload, true).await
}

async fn get_json(config: &AppConfig, path: &str) -> Result<serde_json::Value> {
    let request = reqwest::Client::new().get(format!("http://{}{}", config.http_addr, path));
    let response = request.send().await.context("HTTP request failed")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("GET {} returned {}: {}", path, status, body);
    }
    let body = response
        .text()
        .await
        .context("failed to read response body")?;
    serde_json::from_str(&body).context("failed to parse JSON response")
}

async fn post_json_with_auth<T: serde::Serialize>(
    config: &AppConfig,
    path: &str,
    payload: &T,
    include_control_auth: bool,
) -> Result<()> {
    let mut request = reqwest::Client::new()
        .post(format!("http://{}{}", config.http_addr, path))
        .json(payload);
    if include_control_auth {
        if let Some(token) = &config.control_token {
            request = request.bearer_auth(token);
        }
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("failed to post {}", path))?;
    let body = response.text().await?;
    println!("{body}");
    Ok(())
}

async fn handle_config_command(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Get { key } => {
            let path = config_file_path();
            let config = load_persisted_config_at(&path)?;
            print_json(&get_config_key(&config, &key)?)
        }
        ConfigCommands::Set { key, value } => {
            let path = config_file_path();
            let mut config = load_persisted_config_at(&path)?;
            set_config_key(&mut config, &key, &value)?;
            save_persisted_config_at(&path, &config)?;
            print_json(&get_config_key(&config, &key)?)
        }
        ConfigCommands::Unset { key } => {
            let path = config_file_path();
            let mut config = load_persisted_config_at(&path)?;
            unset_config_key(&mut config, &key)?;
            save_persisted_config_at(&path, &config)?;
            print_json(&serde_json::json!({
                "key": key,
                "status": "unset"
            }))
        }
        ConfigCommands::Providers { command } => handle_config_providers_command(command).await,
        ConfigCommands::Credentials { command } => handle_config_credentials_command(command).await,
        ConfigCommands::Models { command } => handle_config_models_command(command).await,
        ConfigCommands::List => {
            let path = config_file_path();
            let config = load_persisted_config_at(&path)?;
            print_json(&serde_json::to_value(config)?)
        }
        ConfigCommands::Schema => print_json(&serde_json::to_value(config_schema())?),
        ConfigCommands::Doctor => {
            let config = AppConfig::load_for_config_inspection()?;
            print_json(&provider_doctor(&config))
        }
    }
}

async fn handle_config_models_command(command: ConfigModelCommands) -> Result<()> {
    match command {
        ConfigModelCommands::List => {
            let config = AppConfig::load_for_config_inspection()?;
            print_json(&serde_json::to_value(resolved_model_availability(&config))?)
        }
    }
}

async fn handle_config_providers_command(command: ConfigProviderCommands) -> Result<()> {
    match command {
        ConfigProviderCommands::Set {
            provider,
            transport,
            base_url,
            credential_source,
            credential_kind,
            credential_env,
            credential_profile,
            credential_external,
        } => {
            let id = ProviderId::parse(&provider)?;
            let built_in_defaults = built_in_provider_default_config(&id)?;
            let transport = match (transport, built_in_defaults.as_ref()) {
                (Some(raw), Some(defaults)) => {
                    let parsed = ProviderTransportKind::parse(&raw)?;
                    let id_str = id.as_str();
                    if (id_str.ends_with("-anthropic") || id_str.ends_with("-openai"))
                        && parsed != defaults.transport
                    {
                        return Err(anyhow!(
                            "provider {} has fixed transport {}; remove --transport or set it to {}",
                            id_str,
                            defaults.transport.as_str(),
                            defaults.transport.as_str()
                        ));
                    }
                    parsed
                }
                (Some(raw), None) => ProviderTransportKind::parse(&raw)?,
                (None, Some(defaults)) => defaults.transport,
                (None, None) => {
                    return Err(anyhow!(
                        "provider {} requires --transport when no built-in default exists",
                        id.as_str()
                    ));
                }
            };
            let base_url = match (base_url, built_in_defaults.as_ref()) {
                (Some(value), _) => value,
                (None, Some(defaults)) => defaults.base_url.clone(),
                (None, None) => {
                    return Err(anyhow!(
                        "provider {} requires --base-url when no built-in default exists",
                        id.as_str()
                    ));
                }
            };
            let provider_config = ProviderConfigFile {
                transport,
                base_url,
                auth: ProviderAuthConfig {
                    source: CredentialSource::parse(&credential_source)?,
                    kind: CredentialKind::parse(&credential_kind)?,
                    env: credential_env,
                    profile: credential_profile.map(|value| value.trim().to_string()),
                    external: credential_external,
                },
                reasoning_effort: None,
            };
            validate_provider_config(&id, &provider_config)?;

            let path = config_file_path();
            let mut config = load_persisted_config_at(&path)?;
            config.providers.insert(id.clone(), provider_config);
            save_persisted_config_at(&path, &config)?;

            let effective = AppConfig::load_for_config_inspection()?;
            let provider = effective.providers.get(&id).with_context(|| {
                format!("provider {} was saved but did not resolve", id.as_str())
            })?;
            print_json(&serde_json::json!({
                "applied_via": "offline_store",
                "provider": provider_config_view(&effective, provider),
            }))
        }
        ConfigProviderCommands::Get { provider } => {
            let id = ProviderId::parse(&provider)?;
            let config = AppConfig::load_for_config_inspection()?;
            let provider = config
                .providers
                .get(&id)
                .with_context(|| format!("unknown provider {}", id.as_str()))?;
            print_json(&serde_json::to_value(provider_config_view(
                &config, provider,
            ))?)
        }
        ConfigProviderCommands::List => {
            let config = AppConfig::load_for_config_inspection()?;
            print_json(&serde_json::to_value(provider_config_views(&config))?)
        }
        ConfigProviderCommands::Remove { provider } => {
            let id = ProviderId::parse(&provider)?;
            let path = config_file_path();
            let mut config = load_persisted_config_at(&path)?;
            let removed = config.providers.remove(&id).is_some();
            save_persisted_config_at(&path, &config)?;
            print_json(&serde_json::json!({
                "applied_via": "offline_store",
                "provider": id.as_str(),
                "status": if removed { "removed" } else { "not_configured" },
            }))
        }
        ConfigProviderCommands::Doctor { provider } => {
            let id = ProviderId::parse(&provider)?;
            let config = AppConfig::load_for_config_inspection()?;
            let provider_cfg = config
                .providers
                .get(&id)
                .with_context(|| format!("unknown provider {}", id.as_str()))?;
            let doctor = provider_doctor(&config);
            let chain_entries = doctor["providers"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|entry| entry["provider"].as_str() == Some(id.as_str()))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            print_json(&serde_json::json!({
                "provider": provider_config_view(&config, provider_cfg),
                "model_chain_diagnostics": chain_entries,
            }))
        }
    }
}

async fn handle_config_credentials_command(command: ConfigCredentialCommands) -> Result<()> {
    let path = credentials_file_path();
    match command {
        ConfigCredentialCommands::Set {
            profile,
            kind,
            stdin,
            material,
        } => {
            if stdin && material.is_some() {
                anyhow::bail!("--stdin cannot be used together with --material");
            }
            let mut material = if let Some(value) = material {
                value
            } else if stdin {
                eprint!("Enter credential material for {profile} and press Enter: ");
                std::io::stderr()
                    .flush()
                    .context("failed to flush credential prompt to stderr")?;
                let mut value = String::new();
                std::io::stdin()
                    .read_line(&mut value)
                    .context("failed to read credential material from stdin")?;
                value
            } else {
                anyhow::bail!("credential set requires either --stdin or --material");
            };
            trim_trailing_newlines(&mut material);
            let status = set_credential_profile_at(
                &path,
                &profile,
                CredentialKind::parse(&kind)?,
                material,
            )?;
            print_json(&serde_json::json!({
                "applied_via": "offline_store",
                "credential": status,
            }))
        }
        ConfigCredentialCommands::List => {
            let profiles = list_credential_profiles_at(&path)?;
            print_json(&serde_json::to_value(profiles)?)
        }
        ConfigCredentialCommands::Remove { profile } => {
            let status = remove_credential_profile_at(&path, &profile)?;
            print_json(&serde_json::json!({
                "applied_via": "offline_store",
                "credential": status,
            }))
        }
    }
}

fn config_file_path() -> std::path::PathBuf {
    let home_dir = std::env::var("HOLON_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| default_holon_home());
    persisted_config_path(&home_dir)
}

fn credentials_file_path() -> std::path::PathBuf {
    let home_dir = std::env::var("HOLON_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| default_holon_home());
    credential_store_path(&home_dir)
}

fn trim_trailing_newlines(value: &mut String) {
    while value.ends_with('\n') || value.ends_with('\r') {
        value.pop();
    }
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
