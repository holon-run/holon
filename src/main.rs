use std::{
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use holon::{
    client::LocalClient,
    config::{
        config_schema, credential_store_path, default_holon_home, get_config_key,
        list_credential_profiles_at, load_persisted_config_at, persisted_config_path,
        provider_config_view, provider_config_views, remove_credential_profile_at,
        save_persisted_config_at, set_config_key, set_credential_profile_at, unset_config_key,
        validate_provider_config, AppConfig, CredentialKind, CredentialSource, ProviderAuthConfig,
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
    tui::run_tui,
    types::{ControlAction, TrustLevel},
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

#[derive(Debug, Subcommand)]
enum Commands {
    Serve,
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
    Control {
        action: ControlAction,
        #[arg(long)]
        agent: Option<String>,
    },
    Agents {
        #[command(subcommand)]
        command: Option<AgentsCommands>,
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
    },
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
    },
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
        transport: String,
        #[arg(long)]
        base_url: String,
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
    },
    List,
    Remove {
        profile: String,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommands {
    Start,
    Stop,
    Status,
    Restart,
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
}

#[derive(Debug, Subcommand)]
enum AgentsCommands {
    Create {
        agent_id: String,
        #[arg(long)]
        template: Option<String>,
    },
    Model {
        #[command(subcommand)]
        command: AgentModelCommands,
    },
}

#[derive(Debug, Subcommand)]
enum AgentModelCommands {
    Get {
        #[arg(long)]
        agent: Option<String>,
    },
    Set {
        model: String,
        #[arg(long)]
        agent: Option<String>,
    },
    Clear {
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
    let config = AppConfig::load()?;
    match command {
        Commands::Serve => serve(config).await,
        Commands::Daemon { command } => handle_daemon_command(config, command).await,
        Commands::Prompt { text, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config)?;
            let response = client.control_prompt(&agent, text).await?;
            print_json(&response)
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
            post_control_json(
                &config,
                &format!("/control/agents/{agent}/control"),
                &ControlRequest {
                    action,
                    trust: Some(TrustLevel::TrustedOperator),
                },
            )
            .await
        }
        Commands::Agents { command } => handle_agents_command(&config, command).await,
        Commands::Workspace { command } => handle_workspace_command(&config, command).await,
        Commands::Tui { no_alt_screen } => run_tui(config, no_alt_screen).await,
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

async fn serve(config: AppConfig) -> Result<()> {
    std::fs::create_dir_all(config.agent_root_dir())
        .with_context(|| format!("failed to create {}", config.agent_root_dir().display()))?;
    std::fs::create_dir_all(config.run_dir())
        .with_context(|| format!("failed to create {}", config.run_dir().display()))?;
    ensure_serve_preflight(&config).await?;

    let host = RuntimeHost::new(config.clone())?;
    host.default_runtime().await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;

    let tcp_router = http::router(AppState::for_tcp_with_runtime_service(
        host.clone(),
        Some(runtime_service.clone()),
    ));
    let listener = TcpListener::bind(&config.http_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.http_addr))?;
    println!("Holon listening on {}", listener.local_addr()?);

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

async fn handle_debug_command(config: AppConfig, command: DebugCommands) -> Result<()> {
    match command {
        DebugCommands::Prompt { text, agent, trust } => {
            dump_prompt(config, text, agent, trust).await
        }
    }
}

async fn handle_daemon_command(config: AppConfig, command: DaemonCommands) -> Result<()> {
    let value = match command {
        DaemonCommands::Start => serde_json::to_value(daemon_start(&config).await?)?,
        DaemonCommands::Stop => serde_json::to_value(daemon_stop(&config).await?)?,
        DaemonCommands::Status => serde_json::to_value(daemon_status(&config).await?)?,
        DaemonCommands::Restart => serde_json::to_value(daemon_restart(&config).await?)?,
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

async fn handle_agents_command(config: &AppConfig, command: Option<AgentsCommands>) -> Result<()> {
    match command {
        None => {
            let client = LocalClient::new(config.clone())?;
            print_json(&serde_json::to_value(client.list_agents().await?)?)
        }
        Some(AgentsCommands::Create { agent_id, template }) => {
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
        Some(AgentsCommands::Model { command }) => {
            let client = LocalClient::new(config.clone())?;
            match command {
                AgentModelCommands::Get { agent } => {
                    let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
                    let summary = client.agent_status(&agent).await?;
                    print_json(&serde_json::to_value(summary.model)?)
                }
                AgentModelCommands::Set { model, agent } => {
                    let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
                    print_json(&client.set_agent_model_override(&agent, model).await?)
                }
                AgentModelCommands::Clear { agent } => {
                    let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
                    print_json(&client.clear_agent_model_override(&agent).await?)
                }
            }
        }
    }
}

async fn post_control_json<T: serde::Serialize>(
    config: &AppConfig,
    path: &str,
    payload: &T,
) -> Result<()> {
    post_json_with_auth(config, path, payload, true).await
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
            let config = AppConfig::load()?;
            print_json(&provider_doctor(&config))
        }
    }
}

async fn handle_config_models_command(command: ConfigModelCommands) -> Result<()> {
    match command {
        ConfigModelCommands::List => {
            let config = AppConfig::load()?;
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
            let provider_config = ProviderConfigFile {
                transport: ProviderTransportKind::parse(&transport)?,
                base_url,
                auth: ProviderAuthConfig {
                    source: CredentialSource::parse(&credential_source)?,
                    kind: CredentialKind::parse(&credential_kind)?,
                    env: credential_env,
                    profile: credential_profile.map(|value| value.trim().to_string()),
                    external: credential_external,
                },
            };
            validate_provider_config(&id, &provider_config)?;

            let path = config_file_path();
            let mut config = load_persisted_config_at(&path)?;
            config.providers.insert(id.clone(), provider_config);
            save_persisted_config_at(&path, &config)?;

            let effective = AppConfig::load()?;
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
            let config = AppConfig::load()?;
            let provider = config
                .providers
                .get(&id)
                .with_context(|| format!("unknown provider {}", id.as_str()))?;
            print_json(&serde_json::to_value(provider_config_view(
                &config, provider,
            ))?)
        }
        ConfigProviderCommands::List => {
            let config = AppConfig::load()?;
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
            let config = AppConfig::load()?;
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
        } => {
            if !stdin {
                anyhow::bail!("credential material input requires --stdin");
            }
            let mut material = String::new();
            std::io::stdin()
                .read_to_string(&mut material)
                .context("failed to read credential material from stdin")?;
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
