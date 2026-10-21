use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{IsTerminal, Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use clap::ValueEnum;
use holon::{
    client::{normalize_control_base_url, LocalClient, LocalHttpError},
    config::{
        built_in_provider_default_config, config_schema, credential_store_path, default_holon_home,
        get_config_key, list_credential_profiles_at, load_credential_store_at,
        load_persisted_config_at, persisted_config_path, provider_config_view,
        provider_config_views, remove_credential_profile_at, save_persisted_config_at,
        set_config_key, set_credential_profile_at, unset_config_key, validate_provider_config,
        AppConfig, ControlAuthMode, CredentialKind, CredentialSource, ProviderAuthConfig,
        ProviderConfigFile, ProviderId, ProviderTransportKind,
    },
    config::{AgentTemplateRemoteSourceConfigFile, AgentTemplatesConfigFile},
    daemon::{
        daemon_launch_with_profile, daemon_logs, daemon_prepare_update, daemon_status, daemon_stop,
        prepare_runtime_before_server_observed, prepare_serve_with_profile, DaemonServeProfile,
        ResolvedDaemonServePlan, RuntimeControlAuthMode, RuntimeServiceHandle, ServeProfileAccess,
        ServeProfileAuthentication, DAEMON_SERVE_ARGS_ENV, DAEMON_SERVE_RESOLUTION_ENV,
        PRE_SERVER_PREPARED_ENV,
    },
    diagnostics_store::DiagnosticsWriter,
    fd_limit::{apply_nofile_limit_policy, DEFAULT_NOFILE_TARGET},
    host::RuntimeHost,
    http::{
        self, AppState, CompleteWorkItemRequest, ControlRequest, CreateCommandTaskRequest,
        CreateWorkItemRequest, PickWorkItemRequest, TaskInputRequest, TaskStopRequest,
        UpdateWorkItemRequest,
    },
    memory::{rebuild_memory_index, request_memory_index_rebuild},
    model_discovery::{discovery_cache_path, refresh_provider_models},
    onboarding::{
        apply_onboarding_wizard_draft, onboarding_report, OnboardingApplySummary,
        OnboardingSearchSelection, OnboardingWizardSubmission,
    },
    onboarding_tui::run_onboarding_tui,
    otlp_exporter::OtlpExporter,
    provider::{provider_doctor, resolved_model_availability},
    resource_policy::{apply_startup_policy, startup_report},
    run_once::{run_once, RunOnceRequest},
    runtime::{maybe_enqueue_first_run_intro, seed_scheduler_terminal_recovery_fixture},
    runtime_db::{
        RuntimeDb, RuntimeDbAuditCheck, RuntimeDbAuditOptions, RuntimeDbAuditReport, RuntimeDbLock,
    },
    solve::{run_solve, SolveRequest},
    storage::AppStorage,
    tui::run_tui,
    types::{
        AgentDeletionPhase, AgentDeletionStatus, AgentInvocationContext, AgentRegistryStatus,
        AuditEvent, AuthorityClass, ControlAction, TimerStatus, TodoItem, WorkItemPlanStatus,
        HOLON_CALLER_AUTHORITY_CLASS_ENV,
    },
};
use tokio::net::TcpListener;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use holon::cli::{
    AgentCommands, AgentDetailArg, AgentModelCommands, Cli, Commands, ConfigCommands,
    ConfigCredentialCommands, ConfigModelCommands, ConfigProviderCommands, ControlCommandAction,
    DaemonCommands, DebugCommands, EventsCommands, MemoryIndexCommands, ModelsDevCommands,
    ServeAccess, ServeOptions, SkillsCommands, TaskCommands, TimerCommands, TimerCreateArgs,
    WorkItemCommands, WorkspaceCommands,
};

// Linux daemons swap in jemalloc so freed pages are purged back to the OS
// instead of accumulating per-arena high-watermark RSS (#2931). Keep
// `allocator_label` below in sync with this declaration.
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn allocator_label() -> &'static str {
    if cfg!(target_os = "linux") {
        "jemalloc"
    } else {
        "system"
    }
}

fn main() -> std::process::ExitCode {
    // Capture managed stdio before tracing, startup diagnostics or Tokio.
    let logs = match holon::daemon::logging::ManagedDaemonLog::from_env() {
        Ok(logs) => logs,
        Err(error) => {
            eprintln!("Error: {error:#}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let result = run_main();
    let code = match &result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if let Some(error) = error.downcast_ref::<clap::Error>() {
                let _ = error.print();
                std::process::ExitCode::from(error.exit_code() as u8)
            } else {
                eprintln!("Error: {error:#}");
                std::process::ExitCode::FAILURE
            }
        }
    };
    drop(logs);
    code
}

fn run_main() -> Result<()> {
    init_tracing();
    let policy = apply_startup_policy(allocator_label());
    let cli = Cli::try_parse()?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(policy.tokio_worker_threads)
        .enable_all()
        .build()?
        .block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Config { command } => handle_config_command(command).await,
        Commands::Run {
            text,
            authority_class,
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
                authority_class,
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
            timeout_seconds,
            authority_class,
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
                timeout_seconds,
                authority_class,
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
    if let Commands::Onboard { json } = &command {
        let config = AppConfig::load_for_config_inspection()?;
        let report = onboarding_report(&config);
        if *json || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return print_json(&serde_json::to_value(report)?);
        }
        let submission = run_onboarding_tui(config.clone())?;
        let summary = apply_onboarding_submission(&config, &submission).await?;
        println!("Holon onboarding configured.");
        println!("- applied via: {}", summary.applied_via);
        println!("- provider: {}", summary.provider);
        println!("- credential profile: {}", summary.credential_profile);
        println!("- default model: {}", summary.default_model);
        println!("- search: {}", summary.search);
        if summary.credential_written {
            println!("Credential material was stored locally and was not printed.");
        }
        return Ok(());
    }

    if let Commands::Tui {
        no_alt_screen,
        connect: Some(connect),
        token,
        token_file,
        token_profile,
    } = &command
    {
        let config = AppConfig::load_for_config_inspection()?;
        let token = resolve_remote_token(
            &config,
            token.clone(),
            token_file.clone(),
            token_profile.clone(),
        )?;
        let client = LocalClient::remote(config.clone(), connect.clone(), token)?;
        client.handshake().await?;
        return run_tui(config, *no_alt_screen, Some(client)).await;
    }

    let config = if runtime_command_uses_config_inspection(&command) {
        AppConfig::load_for_config_inspection()?
    } else {
        AppConfig::load()?
    };
    match command {
        Commands::Context => {
            let context = AgentInvocationContext::from_env().map_err(|error| anyhow!(error))?;
            print_json(&serde_json::json!({
                "mode": if context.is_some() { "agent" } else { "operator" },
                "context": context,
                "declaration_based": true,
                "authenticated": false,
            }))
        }
        Commands::Commands => print_json(&serde_json::to_value(holon::cli::collect_snapshot())?),
        Commands::Serve { options } => serve(config, options).await,
        Commands::Daemon { command } => handle_daemon_command(config, command).await,
        Commands::Prompt { text, agent } => {
            let client = LocalClient::new(config)?;
            let agent = resolve_agent_id(&client, agent).await?;
            let response = client.control_prompt(&agent, text).await?;
            print_json(&serde_json::to_value(response)?)
        }
        Commands::Tail { options, agent } => {
            let client = LocalClient::new(config)?;
            let agent = resolve_agent_id(&client, agent).await?;
            if !options.include_progress && !options.follow {
                print_json(&serde_json::to_value(
                    client.agent_briefs(&agent, options.limit).await?,
                )?)
            } else {
                holon::tail::observe(
                    &client,
                    &agent,
                    &options,
                    &mut holon::tail::stdout::Stdout::new()?,
                )
                .await
            }
        }
        Commands::Transcript { limit, agent } => {
            let client = LocalClient::new(config)?;
            let agent = resolve_agent_id(&client, agent).await?;
            print_json(&serde_json::to_value(
                client.agent_transcript(&agent, limit).await?,
            )?)
        }
        Commands::Events { command } => handle_events_command(&config, command).await,
        Commands::Task { command } => handle_task_command(&config, command).await,
        Commands::WorkItem { command } => handle_work_item_command(&config, command).await,
        Commands::MemoryIndex { command } => handle_memory_index_command(&config, command).await,
        Commands::Timer { command, legacy } => {
            let command = match command {
                Some(command) => command,
                None => TimerCommands::Create {
                    options: TimerCreateArgs {
                        after_ms: legacy.after_ms.ok_or_else(|| {
                            anyhow!(
                                "`holon timer` requires `--after-ms <ms>` or a timer subcommand"
                            )
                        })?,
                        every_ms: legacy.every_ms,
                        summary: legacy.summary,
                        agent: legacy.agent,
                    },
                },
            };
            handle_timer_command(&config, command).await
        }
        Commands::Control { action, agent } => {
            let client = LocalClient::new(config.clone())?;
            let agent = resolve_agent_id(&client, agent).await?;
            if action == ControlCommandAction::Abort {
                return post_control_json(
                    &config,
                    &format!("/control/agents/{agent}/current-run/abort"),
                    &http::AbortCurrentRunRequest {
                        run_id: None,
                        mode: Some("stop_after_abort".into()),
                        authority_class: Some(AuthorityClass::OperatorInstruction),
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
                        .expect("abort action should be handled separately"),
                    authority_class: Some(AuthorityClass::OperatorInstruction),
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
        Commands::ModelsDev { command } => handle_models_dev_command(command).await,
        Commands::Onboard { .. } => unreachable!("onboard command is handled before runtime load"),
        Commands::Config { .. } => unreachable!("config commands are handled separately"),
    }
}

fn runtime_command_uses_config_inspection(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Context
            | Commands::Commands
            | Commands::Events {
                command: EventsCommands::Tail { offline: true, .. }
            }
            | Commands::Debug {
                command: DebugCommands::SchedulerRecovery { .. }
                    | DebugCommands::SchedulerRecoveryFixture { .. }
                    | DebugCommands::SchedulerRestartFixture { .. }
            }
            | Commands::ModelsDev { .. }
    )
}

async fn handle_memory_index_command(
    config: &AppConfig,
    command: MemoryIndexCommands,
) -> Result<()> {
    match command {
        MemoryIndexCommands::Rebuild {
            agent,
            workspace,
            offline,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let agent_home = config.data_dir.join("agents").join(&agent);
            let runtime_db = RuntimeDb::open_and_migrate(
                config.runtime_db_path(),
                config.runtime_db_lock_path(),
            )?;
            if offline {
                let storage = AppStorage::new_for_agent(&agent_home, agent.clone(), runtime_db)?;
                rebuild_memory_index(&storage, workspace.as_deref())?;
                println!(
                    "Rebuilt memory index for agent `{}`{} in offline mode.",
                    agent,
                    workspace
                        .as_deref()
                        .map(|workspace| format!(" in workspace `{workspace}`"))
                        .unwrap_or_default()
                );
            } else {
                let storage = AppStorage::new_for_agent(&agent_home, agent.clone(), runtime_db)?;
                request_memory_index_rebuild(&storage, workspace.as_deref(), "cli_rebuild")?;
                println!(
                    "Requested memory index rebuild for agent `{}`{}; the background indexer will apply it.",
                    agent,
                    workspace
                        .as_deref()
                        .map(|workspace| format!(" in workspace `{workspace}`"))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }
    }
}

async fn resolve_agent_id(client: &LocalClient, agent: Option<String>) -> Result<String> {
    if let Some(agent) = agent {
        return Ok(agent);
    }
    match client.configured_default_agent_id().await? {
        Some(agent) => Ok(agent),
        None => Err(anyhow!(
            "no default agent configured; create an agent first"
        )),
    }
}

async fn run_one_shot(
    text: String,
    authority_class: AuthorityClass,
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
            authority_class,
            agent_id,
            create_agent,
            template,
            max_turns,
            timeout_seconds: None,
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
    timeout_seconds: Option<u64>,
    authority_class: AuthorityClass,
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
            timeout_seconds,
            authority_class,
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
    let access = options.access.unwrap_or(ServeAccess::Local);
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
    } else if access == ServeAccess::Tunnel {
        config.http_addr = loopback_with_configured_port(&config.http_addr, options.port);
    } else if matches!(access, ServeAccess::Lan | ServeAccess::Tailnet) {
        config.http_addr =
            default_remote_listen_addr(options.host.as_deref(), &config.http_addr, options.port);
    } else if options.port.is_some() {
        config.http_addr = loopback_with_configured_port(&config.http_addr, options.port);
    }

    let advertise_url = match options.advertise {
        Some(url) => Some(validate_client_visible_url("advertise URL", &url)?),
        None => match access {
            ServeAccess::Local | ServeAccess::Tunnel => None,
            ServeAccess::Lan | ServeAccess::Tailnet => {
                let Some(host) = options
                    .host
                    .as_deref()
                    .filter(|host| !host.trim().is_empty())
                else {
                    return Err(anyhow!(
                        "--access {:?} requires --host or --advertise",
                        access
                    ));
                };
                Some(format!(
                    "http://{}",
                    client_visible_host_port(host, &config.http_addr)?
                ))
            }
        },
    };
    // callback_base_url is the local address for same-host webhook callbacks.
    // Always derive from the listen port unless explicitly overridden via env var.
    if std::env::var_os("HOLON_CALLBACK_BASE_URL").is_none() {
        let port = config
            .http_addr
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .unwrap_or(7878);
        config.callback_base_url = format!("http://127.0.0.1:{port}");
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
    if matches!(access, ServeAccess::Lan | ServeAccess::Tailnet)
        && config
            .control_token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
    {
        return Err(anyhow!(
            "--access {:?} requires --token, --token-file, or HOLON_CONTROL_TOKEN",
            access
        ));
    }
    if non_loopback_tcp || matches!(access, ServeAccess::Lan | ServeAccess::Tailnet) {
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

fn safe_serve_args_for_metadata(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn parse_serve_options_from_args(args: &[OsString]) -> Result<ServeOptions> {
    let argv = std::iter::once(OsString::from("holon"))
        .chain(std::iter::once(OsString::from("serve")))
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    let cli = Cli::try_parse_from(argv).map_err(|err| anyhow!(err.to_string()))?;
    let Commands::Serve { options } = cli.command else {
        return Err(anyhow!(
            "recorded daemon start arguments did not parse as serve options"
        ));
    };
    Ok(options)
}

fn serve_args_for_options(options: &ServeOptions) -> DaemonServeLaunchOptions {
    let mut args = Vec::new();
    if let Some(access) = options.access {
        args.extend([
            OsString::from("--access"),
            OsString::from(
                access
                    .to_possible_value()
                    .expect("serve access should have clap value")
                    .get_name(),
            ),
        ]);
    }
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
    if let Some(enabled) = options.desktop_integration {
        args.push(OsString::from(format!("--desktop-integration={enabled}")));
    }
    if let Some(web_dist) = &options.web_dist {
        args.extend([
            OsString::from("--web-dist"),
            web_dist.as_os_str().to_os_string(),
        ]);
    }
    DaemonServeLaunchOptions {
        args,
        control_token_env: options.token.clone(),
    }
}

fn merge_serve_options(inherited: ServeOptions, explicit: ServeOptions) -> ServeOptions {
    let explicit_token = explicit.token.is_some();
    let explicit_token_file = explicit.token_file.is_some();
    let explicit_access = explicit.access.is_some();
    let explicit_host = explicit.host.is_some();
    let explicit_listen = explicit.listen.is_some();
    let explicit_port = explicit.port.is_some();
    let explicit_advertise = explicit.advertise.is_some();
    let explicit_web_dist = explicit.web_dist.is_some();
    let clear_inherited_advertise =
        explicit_access || explicit_host || explicit_listen || explicit_port;
    ServeOptions {
        reset_serve_options: false,
        desktop_integration_if_unconfigured: false,
        desktop_integration: explicit
            .desktop_integration
            .or(inherited.desktop_integration),
        access: explicit.access.or(inherited.access),
        host: explicit.host.or(inherited.host),
        listen: if explicit_listen {
            explicit.listen
        } else if explicit_port {
            None
        } else {
            inherited.listen
        },
        port: if explicit_port {
            explicit.port
        } else if explicit_listen {
            None
        } else {
            inherited.port
        },
        advertise: if explicit_advertise {
            explicit.advertise
        } else if clear_inherited_advertise {
            None
        } else {
            inherited.advertise
        },
        token: if explicit_token {
            explicit.token
        } else if explicit_token_file {
            None
        } else {
            inherited.token
        },
        token_file: if explicit_token_file {
            explicit.token_file
        } else if explicit_token {
            None
        } else {
            inherited.token_file
        },
        web_dist: if explicit_web_dist {
            explicit.web_dist
        } else {
            inherited.web_dist
        },
    }
}

#[cfg(test)]
fn restart_serve_launch_options(
    config: &mut AppConfig,
    options: ServeOptions,
    metadata: Option<&holon::daemon::RuntimeServiceMetadata>,
) -> Result<DaemonServeLaunchOptions> {
    let plan = resolve_daemon_serve_plan_with_inputs(
        config.clone(),
        options,
        None,
        metadata,
        parse_serve_options_from_args(&[])?,
        false,
    )?;
    *config = plan.config;
    Ok(DaemonServeLaunchOptions {
        args: plan.serve_args,
        control_token_env: plan.control_token_env,
    })
}

fn options_from_profile(profile: &DaemonServeProfile) -> ServeOptions {
    ServeOptions {
        reset_serve_options: false,
        desktop_integration_if_unconfigured: false,
        access: Some(match profile.access {
            ServeProfileAccess::Local => ServeAccess::Local,
            ServeProfileAccess::Tunnel => ServeAccess::Tunnel,
            ServeProfileAccess::Lan => ServeAccess::Lan,
            ServeProfileAccess::Tailnet => ServeAccess::Tailnet,
        }),
        host: profile.host.clone(),
        listen: profile.listen.clone(),
        port: profile.port,
        advertise: profile.advertise.clone(),
        token: None,
        token_file: match &profile.authentication {
            ServeProfileAuthentication::TokenFile { path } => Some(path.clone()),
            _ => None,
        },
        desktop_integration: Some(profile.desktop_integration),
        web_dist: profile.web_dist.clone(),
    }
}

fn explicit_serve_fields(options: &ServeOptions) -> Vec<&'static str> {
    let mut fields = Vec::new();
    for (present, field) in [
        (options.access.is_some(), "access"),
        (options.host.is_some(), "host"),
        (options.listen.is_some(), "listen"),
        (options.port.is_some(), "port"),
        (options.advertise.is_some(), "advertise"),
        (options.token.is_some(), "authentication(--token)"),
        (options.token_file.is_some(), "authentication(--token-file)"),
        (options.desktop_integration.is_some(), "desktop_integration"),
        (options.web_dist.is_some(), "web_dist"),
        (options.reset_serve_options, "reset_serve_options"),
    ] {
        if present {
            fields.push(field);
        }
    }
    fields
}

fn resolve_daemon_serve_plan(
    config: AppConfig,
    explicit: ServeOptions,
    profile: Option<&DaemonServeProfile>,
    metadata: Option<&holon::daemon::RuntimeServiceMetadata>,
) -> Result<ResolvedDaemonServePlan> {
    let mut environment = parse_serve_options_from_args(&[])?;
    if let Some(value) = std::env::var_os("HOLON_HTTP_ADDR") {
        environment.listen = Some(
            value
                .into_string()
                .map_err(|_| anyhow!("HOLON_HTTP_ADDR is not UTF-8"))?,
        );
    }
    if let Some(value) = std::env::var_os("HOLON_CONTROL_TOKEN") {
        environment.token = Some(
            value
                .into_string()
                .map_err(|_| anyhow!("HOLON_CONTROL_TOKEN is not UTF-8"))?,
        );
    }
    let explicit_auth_mode = std::env::var_os("HOLON_CONTROL_AUTH_MODE").is_some();
    let plan = resolve_daemon_serve_plan_with_inputs(
        config,
        explicit,
        profile,
        metadata,
        environment,
        explicit_auth_mode,
    )?;
    eprintln!("{}", plan.startup_summary);
    Ok(plan)
}

fn resolve_daemon_serve_plan_with_inputs(
    mut config: AppConfig,
    explicit: ServeOptions,
    profile: Option<&DaemonServeProfile>,
    metadata: Option<&holon::daemon::RuntimeServiceMetadata>,
    environment: ServeOptions,
    explicit_auth_mode: bool,
) -> Result<ResolvedDaemonServePlan> {
    if explicit.reset_serve_options && explicit.desktop_integration_if_unconfigured {
        anyhow::bail!(
            "--desktop-integration-if-unconfigured cannot be combined with --reset-serve-options"
        );
    }
    let initialize_desktop = explicit.desktop_integration_if_unconfigured
        && explicit.desktop_integration.is_none()
        && profile.is_none()
        && metadata.is_none();
    if explicit.listen.is_some() && explicit.port.is_some() {
        anyhow::bail!("use only one of --listen or --port");
    }
    if explicit.token.is_some() && explicit.token_file.is_some() {
        anyhow::bail!("use only one of --token or --token-file");
    }
    let cli_fields = explicit_serve_fields(&explicit);
    let mut env_fields = Vec::new();
    if environment.listen.is_some() {
        env_fields.push("HOLON_HTTP_ADDR(listen)");
    }
    if environment.token.is_some() {
        env_fields.push("HOLON_CONTROL_TOKEN(authentication)");
    }
    if explicit_auth_mode {
        env_fields.push("HOLON_CONTROL_AUTH_MODE(control_auth_mode)");
    } else if let Some(profile) = profile {
        config.control_auth_mode = match profile.control_auth_mode {
            RuntimeControlAuthMode::Auto => ControlAuthMode::Auto,
            RuntimeControlAuthMode::Required => ControlAuthMode::Required,
            RuntimeControlAuthMode::Disabled => ControlAuthMode::Disabled,
        };
    }
    let (base, inherited, external_required) = if let Some(profile) = profile {
        profile.validate()?;
        (
            "profile",
            options_from_profile(profile),
            matches!(
                profile.authentication,
                ServeProfileAuthentication::ExternalRequired
            ),
        )
    } else if let Some(metadata) = metadata {
        let mut inherited = parse_serve_options_from_args(
            &metadata
                .serve_args
                .iter()
                .map(OsString::from)
                .collect::<Vec<_>>(),
        )
        .map_err(|_| anyhow!("invalid legacy daemon serve options"))?;
        if inherited.token.is_some() || inherited.reset_serve_options {
            anyhow::bail!("legacy daemon metadata must not contain token material or reset flags");
        }
        if inherited.desktop_integration_if_unconfigured {
            anyhow::bail!(
                "legacy daemon metadata must not contain --desktop-integration-if-unconfigured"
            );
        }
        if inherited.listen.is_none() && inherited.port.is_none() {
            inherited.listen = Some(metadata.http_addr.clone());
        }
        (
            "legacy_metadata",
            inherited,
            metadata.control_token_env_configured,
        )
    } else {
        let mut defaults = parse_serve_options_from_args(&[])?;
        defaults.access = Some(ServeAccess::Local);
        ("defaults(no historical serve options)", defaults, false)
    };
    let access_override = explicit.access.is_some()
        && explicit.listen.is_none()
        && explicit.port.is_none()
        && environment.listen.is_none();
    let mut options = merge_serve_options(merge_serve_options(inherited, environment), explicit);
    if initialize_desktop {
        options.desktop_integration = Some(true);
    }
    if access_override {
        let inherited_port = options
            .listen
            .as_deref()
            .and_then(|listen| listen.rsplit_once(':'))
            .and_then(|(_, port)| port.parse::<u16>().ok());
        options.listen = None;
        options.port = options.port.or(inherited_port).or(Some(7878));
    }
    let cwd = std::env::current_dir().context("cannot resolve serve option paths")?;
    for path in [&mut options.token_file, &mut options.web_dist]
        .into_iter()
        .flatten()
    {
        if path.is_relative() {
            *path = cwd.join(&*path);
        }
    }
    // A stored requirement is not downgraded just because this listener is local.
    if external_required && options.token.is_none() && options.token_file.is_none() {
        anyhow::bail!("saved daemon authentication requires --token, --token-file, or HOLON_CONTROL_TOKEN; use --reset-serve-options to explicitly reset all serve intent");
    }
    config.http_addr = "127.0.0.1:7878".into();
    config.control_token = None;
    let advertise = apply_serve_options(&mut config, options.clone())?;
    let _: SocketAddr = config
        .http_addr
        .parse()
        .context("invalid serve listen address")?;
    if config.control_auth_mode == ControlAuthMode::Required && config.control_token.is_none() {
        anyhow::bail!(
            "required daemon authentication needs --token, --token-file, or HOLON_CONTROL_TOKEN"
        );
    }
    if options.desktop_integration == Some(true) && !cfg!(target_os = "macos") {
        anyhow::bail!("desktop integration requires macOS");
    }
    if let Some(path) = &options.web_dist {
        if !path.is_dir() {
            anyhow::bail!("--web-dist must point to a directory: {}", path.display());
        }
    }
    if let Some(advertise) = &advertise {
        let url = reqwest::Url::parse(advertise).context("invalid advertise URL")?;
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("advertise URL must not contain credentials");
        }
    }
    // Fill the default binding while retaining the user's listen-versus-port choice.
    if options.listen.is_none() && options.port.is_none() {
        options.listen = Some(config.http_addr.clone());
    }
    options.access = Some(options.access.unwrap_or(ServeAccess::Local));
    options.desktop_integration = Some(options.desktop_integration.unwrap_or(false));
    options.advertise = advertise;
    let profile = DaemonServeProfile {
        schema_version: 1,
        access: match options.access.unwrap_or(ServeAccess::Local) {
            ServeAccess::Local => ServeProfileAccess::Local,
            ServeAccess::Tunnel => ServeProfileAccess::Tunnel,
            ServeAccess::Lan => ServeProfileAccess::Lan,
            ServeAccess::Tailnet => ServeProfileAccess::Tailnet,
        },
        host: options.host.clone(),
        listen: options.listen.clone(),
        port: options.port,
        advertise: options.advertise.clone(),
        desktop_integration: options.desktop_integration.unwrap_or(false),
        web_dist: options.web_dist.clone(),
        authentication: if let Some(path) = &options.token_file {
            ServeProfileAuthentication::TokenFile { path: path.clone() }
        } else if config.control_token.is_some() {
            ServeProfileAuthentication::ExternalRequired
        } else {
            ServeProfileAuthentication::None
        },
        control_auth_mode: config.control_auth_mode.into(),
    };
    profile.validate()?;
    // Logs explain even equal-valued explicit overrides, never credential values or argv.
    let startup_summary = format!(
        "daemon serve intent: base={base}; CLI overrides={}; env overrides={}; authentication={}",
        cli_fields.join(","),
        env_fields.join(","),
        match &profile.authentication {
            ServeProfileAuthentication::None => "none",
            ServeProfileAuthentication::TokenFile { .. } => "token_file",
            ServeProfileAuthentication::ExternalRequired => "external_required",
        }
    );
    let mut launch = serve_args_for_options(&options);
    // Pass the already resolved secret through the environment, including token-file input.
    launch.control_token_env = config.control_token.clone();
    Ok(ResolvedDaemonServePlan {
        config,
        profile,
        serve_args: launch.args,
        control_token_env: launch.control_token_env,
        startup_summary,
    })
}

async fn emit_first_run_intro(config: &AppConfig, runtime: &holon::runtime::RuntimeHandle) {
    if let Err(error) = maybe_enqueue_first_run_intro(config, runtime).await {
        eprintln!("failed to enqueue first-run intro: {error:#}");
    }
}

fn resolve_recorded_daemon_serve_plan(
    config: &AppConfig,
    metadata: &holon::daemon::RuntimeServiceMetadata,
) -> Result<ResolvedDaemonServePlan> {
    let mut environment = parse_serve_options_from_args(&[])?;
    let recorded = parse_serve_options_from_args(
        &metadata
            .serve_args
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>(),
    )?;
    if metadata.control_token_env_configured && recorded.token_file.is_none() {
        environment.token = config.control_token.clone();
    }
    resolve_daemon_serve_plan_with_inputs(
        config.clone(),
        parse_serve_options_from_args(&[])?,
        None,
        Some(metadata),
        environment,
        true,
    )
}

async fn serve(mut config: AppConfig, options: ServeOptions) -> Result<()> {
    let (options, registration_lock) = if std::env::var_os(PRE_SERVER_PREPARED_ENV).is_some() {
        // The lifecycle parent holds the lock and already accepted this exact plan.
        if let Ok(summary) = std::env::var(DAEMON_SERVE_RESOLUTION_ENV) {
            eprintln!("{summary}");
        }
        (options, None)
    } else {
        let (plan, lock) = prepare_serve_with_profile(
            &config,
            options.reset_serve_options,
            |profile, metadata| {
                resolve_daemon_serve_plan(config.clone(), options, profile, metadata)
            },
        )
        .await?;
        let options = options_from_profile(&plan.profile);
        config = plan.config;
        std::env::set_var(
            DAEMON_SERVE_ARGS_ENV,
            serde_json::to_string(&safe_serve_args_for_metadata(&plan.serve_args))?,
        );
        (options, Some(lock))
    };
    let serve_args = serve_args_for_options(&options).args;
    let desktop_integration = options.desktop_integration.unwrap_or(false);
    let web_dist = options.web_dist.clone();
    let advertise_url = apply_serve_options(&mut config, options)?;
    if desktop_integration && !cfg!(target_os = "macos") {
        anyhow::bail!("desktop integration requires macOS");
    }
    if let Some(web_dist) = &web_dist {
        if !web_dist.is_dir() {
            return Err(anyhow!(
                "--web-dist must point to a directory: {}",
                web_dist.display()
            ));
        }
    }
    std::fs::create_dir_all(config.agent_root_dir())
        .with_context(|| format!("failed to create {}", config.agent_root_dir().display()))?;
    std::fs::create_dir_all(config.run_dir())
        .with_context(|| format!("failed to create {}", config.run_dir().display()))?;
    println!(
        "{}",
        apply_nofile_limit_policy(DEFAULT_NOFILE_TARGET).startup_summary()
    );
    if let Some(report) = startup_report() {
        println!("{}", report.startup_summary());
    }
    // Correlation only; keep accepting the legacy internal "1" marker.
    let startup = std::env::var(PRE_SERVER_PREPARED_ENV)
        .ok()
        .filter(|value| value.starts_with("startup_") && value.len() <= 128)
        .map(holon::startup::StartupObservation::with_id)
        .unwrap_or_default();
    let (
        host,
        diagnostics_writer,
        otlp_exporter,
        runtime_service,
        listener,
        local_listener,
        tcp_router,
        unix_server,
    ) = startup
        .observe("serve_startup", async {
            if std::env::var_os(PRE_SERVER_PREPARED_ENV).is_none() {
                prepare_runtime_before_server_observed(&config, &startup)?;
            }
            if std::env::var_os(DAEMON_SERVE_ARGS_ENV).is_none() {
                std::env::set_var(
                    DAEMON_SERVE_ARGS_ENV,
                    serde_json::to_string(&safe_serve_args_for_metadata(&serve_args))?,
                );
            }

            let host = startup.observe_sync("host_init", || RuntimeHost::new(config.clone()))?;
            let diagnostics_writer = DiagnosticsWriter::start(
                config.diagnostics_db_path(),
                config.diagnostics_writer_config()?,
            )?;
            diagnostics_writer.install();
            let otlp_exporter = config
                .otlp_exporter_config()?
                .map(OtlpExporter::start)
                .transpose()?;
            if let Some(exporter) = otlp_exporter.as_ref() {
                exporter.install();
            }
            let runtime_service = RuntimeServiceHandle::new_starting(&config)?;
            // Internal launch handoff must not leak into commands spawned by this runtime.
            std::env::remove_var(PRE_SERVER_PREPARED_ENV);
            std::env::remove_var(DAEMON_SERVE_ARGS_ENV);
            std::env::remove_var(DAEMON_SERVE_RESOLUTION_ENV);
            host.recover_startup_observed(&startup).await?;
            let runtime = startup
                .observe("default_activation", async {
                    if host.configured_default_agent_id()?.is_some() {
                        Ok(Some(
                            host.default_runtime_observed(Some(startup.clone())).await?,
                        ))
                    } else {
                        Ok(None)
                    }
                })
                .await?;
            host.spawn_daemon_memory_indexer();
            host.spawn_daemon_runtime_db_retention();
            host.spawn_daemon_deletion_coordinator();
            spawn_stale_agent_template_remote_source_sync(&config, &host);
            startup
                .observe("first_run", async {
                    if let Some(runtime) = runtime.as_ref() {
                        emit_first_run_intro(&config, runtime).await;
                    }
                    Ok(())
                })
                .await?;

            let listener = startup
                .observe("bind", async {
                    TcpListener::bind(&config.http_addr)
                        .await
                        .with_context(|| format!("failed to bind {}", config.http_addr))
                })
                .await?;
            let primary_addr = listener.local_addr()?;
            println!("Holon listening on {primary_addr}");
            if let Some(advertise_url) = &advertise_url {
                println!("Holon advertised at {advertise_url}");
            }
            // Keep local control and the Serve backend reachable for LAN/tailnet primaries.
            // Loopback and wildcard primaries already cover their loopback address family.
            let local_listener =
                if !primary_addr.ip().is_loopback() && !primary_addr.ip().is_unspecified() {
                    let local_addr = std::net::SocketAddr::new(
                        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                        primary_addr.port(),
                    );
                    let local = startup
                        .observe("bind_loopback", async {
                            TcpListener::bind(local_addr).await.with_context(|| {
                                format!("failed to bind localhost listener on {local_addr}")
                            })
                        })
                        .await?;
                    println!("Holon local listening on {}", local.local_addr()?);
                    Some(local)
                } else {
                    None
                };
            let local_addr = local_listener
                .as_ref()
                .map(TcpListener::local_addr)
                .transpose()?;

            let tcp_state =
                AppState::for_tcp_with_runtime_service(host.clone(), Some(runtime_service.clone()))
                    .with_http_listener_addresses(primary_addr, local_addr)
                    .with_desktop_integration(desktop_integration)
                    .with_advertise_url(advertise_url.clone())
                    .with_web_dist(web_dist.clone());

            #[cfg(unix)]
            let unix_server = {
                ensure_socket_parent(&config.socket_path)?;
                let unix_listener = startup.observe_sync("bind_unix", || {
                    tokio::net::UnixListener::bind(&config.socket_path)
                        .with_context(|| format!("failed to bind {}", config.socket_path.display()))
                })?;
                runtime_service.write_state_files(&config)?;
                println!("Holon control socket on {}", config.socket_path.display());
                let unix_router = http::router(
                    AppState::for_unix_with_runtime_service(
                        host.clone(),
                        Some(runtime_service.clone()),
                    )
                    .share_listener_state_from(&tcp_state)
                    .with_web_dist(web_dist.clone()),
                );
                tokio::spawn(http::serve_unix(
                    unix_listener,
                    unix_router,
                    runtime_service.shutdown_signal(),
                ))
            };
            #[cfg(not(unix))]
            runtime_service.write_state_files(&config)?;
            #[cfg(not(unix))]
            let unix_server = ();
            drop(registration_lock);

            // The advertised address and the localhost alias are one daemon-wide HTTP
            // state: one-time pairing tickets are minted over loopback and redeemed from
            // the advertised LAN or Tailscale address, so both sockets must serve the
            // same state instead of a per-listener copy.
            let tcp_router = startup.observe_sync("router", || Ok(http::router(tcp_state)))?;
            startup.observe_sync("serving_ready", || {
                runtime_service.mark_healthy();
                Ok(())
            })?;
            Ok((
                host,
                diagnostics_writer,
                otlp_exporter,
                runtime_service,
                listener,
                local_listener,
                tcp_router,
                unix_server,
            ))
        })
        .await?;
    #[cfg(not(unix))]
    let _ = unix_server;

    #[cfg(unix)]
    {
        let tcp_server = async {
            axum::serve(
                listener,
                tcp_router
                    .clone()
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()))
            .await
            .context("TCP server failed")?;
            Ok::<(), anyhow::Error>(())
        };
        let unix_server = async {
            unix_server
                .await
                .context("unix control server task failed")??;
            Ok::<(), anyhow::Error>(())
        };
        let result = if let Some(local) = local_listener {
            let local_router = tcp_router.clone();
            let local_server = async {
                axum::serve(
                    local,
                    local_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()))
                .await
                .context("localhost server failed")?;
                Ok::<(), anyhow::Error>(())
            };
            tokio::try_join!(tcp_server, unix_server, local_server)
                .map(|_| ())
                .context("runtime servers failed")
        } else {
            tokio::try_join!(tcp_server, unix_server)
                .map(|_| ())
                .context("runtime servers failed")
        };
        let _ = runtime_service.cleanup_state_files(&config);
        host.shutdown_daemon_memory_indexer().await;
        if let Some(exporter) = otlp_exporter {
            exporter.shutdown()?;
        }
        diagnostics_writer.shutdown()?;
        return result;
    }

    #[cfg(not(unix))]
    {
        if let Some(local) = local_listener {
            let local_router = tcp_router.clone();
            let local_server = axum::serve(
                local,
                local_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()));
            let tcp_server = axum::serve(
                listener,
                tcp_router
                    .clone()
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()));
            tokio::try_join!(tcp_server, local_server)
                .map(|_| ())
                .context("runtime servers failed")?;
        } else {
            axum::serve(
                listener,
                tcp_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()))
            .await
            .context("HTTP server failed")?;
        }
        let _ = runtime_service.cleanup_state_files(&config);
        host.shutdown_daemon_memory_indexer().await;
        if let Some(exporter) = otlp_exporter {
            exporter.shutdown()?;
        }
        diagnostics_writer.shutdown()?;
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

fn spawn_stale_agent_template_remote_source_sync(config: &AppConfig, host: &RuntimeHost) {
    let sync_candidates = stale_agent_template_remote_source_sync_candidates(
        &config.stored_config.agent_templates,
        host.runtime_db(),
    );
    if sync_candidates.is_empty() {
        return;
    }

    let db = host.runtime_db().clone();
    let user_home = config.home_dir.clone();
    tokio::spawn(async move {
        for (source_id, source_config) in sync_candidates {
            if let Err(error) = holon::agent_template::sync_agent_template_remote_source(
                &db,
                &user_home,
                &source_id,
                &source_config,
            )
            .await
            {
                let message = error.to_string();
                if let Err(record_error) =
                    holon::agent_template::record_agent_template_remote_source_sync_failure(
                        &db,
                        &source_id,
                        &source_config,
                        &message,
                    )
                {
                    warn!(
                        source_id = %source_id,
                        error = %record_error,
                        "failed to persist agent template remote source startup sync failure"
                    );
                }
                warn!(
                    source_id = %source_id,
                    error = %error,
                    "agent template remote source startup sync failed"
                );
            }
        }
    });
}

fn stale_agent_template_remote_source_sync_candidates(
    config: &AgentTemplatesConfigFile,
    db: &RuntimeDb,
) -> Vec<(String, AgentTemplateRemoteSourceConfigFile)> {
    holon::agent_template::effective_agent_template_remote_sources(config)
        .into_iter()
        .filter_map(|(source_id, source_config)| {
            if !source_config.enabled() {
                return None;
            }
            match holon::agent_template::agent_template_remote_source_needs_sync(
                db,
                &source_id,
                chrono::Duration::hours(24),
            ) {
                Ok(true) => Some((source_id, source_config)),
                Ok(false) => None,
                Err(error) => {
                    warn!(
                        source_id = %source_id,
                        error = %error,
                        "failed to inspect agent template remote source sync state"
                    );
                    None
                }
            }
        })
        .collect()
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
    authority_class: AuthorityClass,
    manifest: bool,
    budget: Option<usize>,
) -> Result<()> {
    let host = RuntimeHost::new(config.clone())?;
    let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let prompt = if let Some(budget) = budget {
        host.preview_agent_prompt_with_budget(&agent, text, authority_class, budget)
            .await?
    } else {
        host.preview_agent_prompt(&agent, text, authority_class)
            .await?
    };
    if manifest {
        print!("{}", prompt.projection_manifest().canonical_json()?);
    } else {
        println!("{}", prompt.render_dump());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn serve_profile_plan(
        cli: &[&str],
        environment: &[&str],
        profile: Option<&DaemonServeProfile>,
    ) -> Result<ResolvedDaemonServePlan> {
        let parse = |args: &[&str]| {
            parse_serve_options_from_args(&args.iter().map(OsString::from).collect::<Vec<_>>())
        };
        resolve_daemon_serve_plan_with_inputs(
            test_config(),
            parse(cli)?,
            profile,
            None,
            parse(environment)?,
            false,
        )
    }

    #[test]
    fn serve_profile_cli_env_and_inheritance_preserve_field_groups() {
        let initial = serve_profile_plan(
            &["--listen", "0.0.0.0:8787", "--token", "first-secret"],
            &[],
            None,
        )
        .unwrap();
        let inherited =
            serve_profile_plan(&[], &["--token", "second-secret"], Some(&initial.profile)).unwrap();
        assert_eq!(inherited.config.http_addr, "0.0.0.0:8787");
        assert_eq!(inherited.profile.listen, initial.profile.listen);
        let overridden = serve_profile_plan(
            &["--port", "9898"],
            &["--listen", "127.0.0.1:9797", "--token", "second-secret"],
            Some(&initial.profile),
        )
        .unwrap();
        assert_eq!(overridden.config.http_addr, "127.0.0.1:9898");
        assert_eq!(overridden.profile.listen, None);
        assert_eq!(overridden.profile.port, Some(9898));
        let encoded = serde_json::to_string(&overridden.profile).unwrap();
        assert!(!encoded.contains("first-secret"));
        assert!(!encoded.contains("second-secret"));
        assert!(overridden.startup_summary.contains("base=profile"));
        assert!(overridden.startup_summary.contains("CLI overrides=port"));
        assert!(overridden
            .startup_summary
            .contains("HOLON_CONTROL_TOKEN(authentication)"));
        assert!(!overridden.startup_summary.contains("second-secret"));
        assert!(!overridden
            .serve_args
            .iter()
            .any(|arg| arg == "second-secret"));
    }

    #[test]
    fn serve_profile_preserves_ephemeral_port_intent() {
        let args = ["--listen", "127.0.0.1:0"];
        for initial in [
            serve_profile_plan(&args, &[], None),
            serve_profile_plan(&[], &args, None),
        ] {
            let initial = initial.unwrap();
            assert_eq!(initial.config.http_addr, "127.0.0.1:0");
            assert_eq!(initial.profile.listen.as_deref(), Some("127.0.0.1:0"));
            assert_eq!(initial.profile.port, None);
            let encoded = serde_json::to_vec(&initial.profile).unwrap();
            let saved: DaemonServeProfile = serde_json::from_slice(&encoded).unwrap();
            saved.validate().unwrap();
            let inherited = serve_profile_plan(&[], &[], Some(&saved)).unwrap();
            assert_eq!(inherited.config.http_addr, "127.0.0.1:0");
            assert_eq!(inherited.profile, initial.profile);
            let reparsed = parse_serve_options_from_args(&inherited.serve_args).unwrap();
            assert_eq!(reparsed.listen, initial.profile.listen);
            assert_eq!(reparsed.port, initial.profile.port);
        }
        assert!(serve_profile_plan(&["--port", "0"], &[], None).is_err());
    }

    #[test]
    fn serve_profile_external_authentication_never_downgrades_on_loopback() {
        let initial = serve_profile_plan(&["--token", "initial-secret"], &[], None).unwrap();
        assert_eq!(initial.config.http_addr, "127.0.0.1:7878");
        let error = serve_profile_plan(&[], &[], Some(&initial.profile))
            .err()
            .unwrap();
        assert!(error
            .to_string()
            .contains("saved daemon authentication requires"));
        let reset = serve_profile_plan(&["--reset-serve-options"], &[], None).unwrap();
        assert_eq!(
            reset.profile.authentication,
            ServeProfileAuthentication::None
        );
        assert_eq!(reset.config.control_token, None);
    }

    #[test]
    fn serve_profile_relative_paths_and_auth_only_override_are_stable() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let token_file = directory.path().join("token");
        fs::write(&token_file, "file-secret").unwrap();
        let initial = serve_profile_plan(
            &[
                "--access",
                "lan",
                "--host",
                "lab.example.test",
                "--listen",
                "0.0.0.0:8787",
                "--advertise",
                "https://lab.example.test",
                "--token-file",
                token_file.to_str().unwrap(),
                "--web-dist",
                ".",
            ],
            &[],
            None,
        )
        .unwrap();
        assert!(initial.profile.web_dist.as_ref().unwrap().is_absolute());
        let ServeProfileAuthentication::TokenFile { path } = &initial.profile.authentication else {
            panic!("expected token-file authentication");
        };
        assert!(path.is_absolute());
        let next =
            serve_profile_plan(&["--token", "inline-secret"], &[], Some(&initial.profile)).unwrap();
        assert_eq!(next.config.http_addr, initial.config.http_addr);
        assert_eq!(next.profile.advertise, initial.profile.advertise);
        assert_eq!(next.profile.web_dist, initial.profile.web_dist);
        assert_eq!(
            next.profile.desktop_integration,
            initial.profile.desktop_integration
        );
        assert_eq!(
            next.profile.authentication,
            ServeProfileAuthentication::ExternalRequired
        );
    }

    #[test]
    fn serve_profile_explicit_local_access_closes_inherited_wildcard() {
        let initial = serve_profile_plan(
            &[
                "--access",
                "lan",
                "--host",
                "lab.example.test",
                "--listen",
                "0.0.0.0:8787",
                "--token",
                "secret",
            ],
            &[],
            None,
        )
        .unwrap();
        for access in ["local", "tunnel"] {
            let next = serve_profile_plan(
                &["--access", access, "--token", "secret"],
                &[],
                Some(&initial.profile),
            )
            .unwrap();
            assert_eq!(next.config.http_addr, "127.0.0.1:8787");
            assert_eq!(next.profile.advertise, None);
        }
    }

    #[test]
    fn serve_profile_bind_overrides_invalidate_inherited_advertise() {
        let initial = serve_profile_plan(
            &[
                "--access",
                "lan",
                "--host",
                "lab.example.test",
                "--port",
                "8787",
                "--advertise",
                "https://old.example.test",
                "--token",
                "secret",
            ],
            &[],
            None,
        )
        .unwrap();
        let next = serve_profile_plan(
            &[],
            &["--listen", "0.0.0.0:9898", "--token", "secret"],
            Some(&initial.profile),
        )
        .unwrap();
        assert_eq!(
            next.profile.advertise.as_deref(),
            Some("http://lab.example.test:9898")
        );
        let explicit = serve_profile_plan(
            &["--advertise", "https://explicit.example.test"],
            &["--listen", "0.0.0.0:9898", "--token", "secret"],
            Some(&initial.profile),
        )
        .unwrap();
        assert_eq!(
            explicit.profile.advertise.as_deref(),
            Some("https://explicit.example.test")
        );
    }

    #[test]
    fn serve_profile_invalid_inputs_are_rejected_before_acceptance() {
        for args in [
            vec!["--token", ""],
            vec!["--listen", "127.0.0.1:8787", "--port", "9898"],
            vec!["--listen", "not-an-address"],
            vec!["--advertise", "https://user:password@example.test"],
            vec!["--token", "secret", "--token-file", "/missing"],
        ] {
            assert!(serve_profile_plan(&args, &[], None).is_err(), "{args:?}");
        }
    }

    #[test]
    fn serve_profile_desktop_false_and_required_policy_are_preserved() {
        let mut initial = serve_profile_plan(&[], &[], None).unwrap().profile;
        initial.desktop_integration = true;
        let next =
            serve_profile_plan(&["--desktop-integration=false"], &[], Some(&initial)).unwrap();
        assert!(!next.profile.desktop_integration);
        initial.desktop_integration = false;
        initial.control_auth_mode = RuntimeControlAuthMode::Required;
        assert!(serve_profile_plan(&[], &[], Some(&initial)).is_err());
    }

    #[test]
    fn serve_profile_desktop_hint_only_initializes_without_history() {
        let hint = "--desktop-integration-if-unconfigured";
        let fresh = serve_profile_plan(&[hint], &[], None);
        if cfg!(target_os = "macos") {
            assert!(fresh.unwrap().profile.desktop_integration);
        } else {
            assert!(fresh
                .err()
                .unwrap()
                .to_string()
                .contains("desktop integration requires macOS"));
        }
        let mut saved = serve_profile_plan(&[], &[], None).unwrap().profile;
        let inherited = serve_profile_plan(&[hint], &[], Some(&saved)).unwrap();
        assert!(!inherited.profile.desktop_integration);
        assert_eq!(inherited.profile, saved);
        saved.desktop_integration = true;
        for profile in [None, Some(&saved)] {
            let explicit =
                serve_profile_plan(&[hint, "--desktop-integration=false"], &[], profile).unwrap();
            assert!(!explicit.profile.desktop_integration);
        }
        let explicit = serve_profile_plan(&[hint, "--desktop-integration=true"], &[], None);
        if cfg!(target_os = "macos") {
            assert!(explicit.unwrap().profile.desktop_integration);
        } else {
            assert!(explicit
                .err()
                .unwrap()
                .to_string()
                .contains("desktop integration requires macOS"));
        }
        for args in [vec![], vec!["--desktop-integration=false"]] {
            let config = test_config();
            let metadata = runtime_metadata_with_serve_args(&config, args, false);
            let plan = resolve_daemon_serve_plan_with_inputs(
                config,
                parse_serve_options_from_args(&[hint.into()]).unwrap(),
                None,
                Some(&metadata),
                parse_serve_options_from_args(&[]).unwrap(),
                false,
            )
            .unwrap();
            assert!(!plan.profile.desktop_integration);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serve_profile_desktop_hint_observes_profile_created_while_waiting_for_lock() {
        let home = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.home_dir = home.path().to_path_buf();
        config.data_dir = config.home_dir.clone();
        config.socket_path = config.home_dir.join("run/holon.sock");
        let lock =
            RuntimeDbLock::try_lock(holon::daemon::daemon_paths(&config).lifecycle_lock_path)
                .unwrap();
        assert!(holon::daemon::load_serve_profile(&config)
            .unwrap()
            .is_none());
        let request =
            parse_serve_options_from_args(&["--desktop-integration-if-unconfigured".into()])
                .unwrap();
        let pending = prepare_serve_with_profile(&config, false, |profile, metadata| {
            resolve_daemon_serve_plan_with_inputs(
                config.clone(),
                request,
                profile,
                metadata,
                parse_serve_options_from_args(&[])?,
                false,
            )
        });
        tokio::pin!(pending);
        tokio::select! {
            biased;
            _ = &mut pending => panic!("request must wait for the lifecycle lock"),
            _ = std::future::ready(()) => {}
        }
        let saved = serve_profile_plan(&["--desktop-integration=false"], &[], None)
            .unwrap()
            .profile;
        holon::daemon::persist_serve_profile(&config, &saved).unwrap();
        drop(lock);
        let (plan, registration_lock) =
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut pending)
                .await
                .unwrap()
                .unwrap();
        assert!(!plan.profile.desktop_integration);
        assert_eq!(plan.profile, saved);
        assert_eq!(
            holon::daemon::load_serve_profile(&config).unwrap(),
            Some(saved)
        );
        drop(registration_lock);
    }

    #[test]
    fn serve_profile_desktop_hint_rejects_reset_and_legacy_replay() {
        let hint = OsString::from("--desktop-integration-if-unconfigured");
        assert!(
            parse_serve_options_from_args(&[hint.clone(), "--reset-serve-options".into(),])
                .is_err()
        );
        let mut request = parse_serve_options_from_args(&[hint]).unwrap();
        request.reset_serve_options = true;
        let error = resolve_daemon_serve_plan_with_inputs(
            test_config(),
            request,
            None,
            None,
            parse_serve_options_from_args(&[]).unwrap(),
            false,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("cannot be combined"));
        let config = test_config();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec!["--desktop-integration-if-unconfigured"],
            false,
        );
        let error = resolve_recorded_daemon_serve_plan(&config, &metadata)
            .err()
            .unwrap();
        assert!(error.to_string().contains(
            "legacy daemon metadata must not contain --desktop-integration-if-unconfigured"
        ));
    }

    #[test]
    fn serve_profile_desktop_hint_is_request_only() {
        for command in [
            vec!["serve"],
            vec!["daemon", "start"],
            vec!["daemon", "restart"],
        ] {
            let cli = Cli::try_parse_from(
                ["holon"]
                    .into_iter()
                    .chain(command)
                    .chain(["--desktop-integration-if-unconfigured"]),
            )
            .unwrap();
            let options = match cli.command {
                Commands::Serve { options }
                | Commands::Daemon {
                    command:
                        DaemonCommands::Start { options, .. } | DaemonCommands::Restart { options, .. },
                } => options,
                _ => panic!("expected lifecycle request"),
            };
            assert!(options.desktop_integration_if_unconfigured);
            assert_eq!(options.desktop_integration, None);
            assert!(!explicit_serve_fields(&options).contains(&"desktop_integration"));
            let launch = serve_args_for_options(&options);
            assert!(launch.args.is_empty());
            let recorded = safe_serve_args_for_metadata(&launch.args);
            assert!(recorded.is_empty());
            let replayed = parse_serve_options_from_args(&launch.args).unwrap();
            assert!(!replayed.desktop_integration_if_unconfigured);
            let merged = merge_serve_options(replayed, options);
            assert!(!merged.desktop_integration_if_unconfigured);
        }
        let plan = serve_profile_plan(
            &[
                "--desktop-integration-if-unconfigured",
                "--desktop-integration=false",
            ],
            &[],
            None,
        )
        .unwrap();
        let json = serde_json::to_value(&plan.profile).unwrap();
        assert!(json.get("desktop_integration_if_unconfigured").is_none());
        assert!(!options_from_profile(&plan.profile).desktop_integration_if_unconfigured);
    }

    #[test]
    fn desktop_serve_option_roundtrips_and_can_be_disabled_on_restart() {
        let enabled =
            parse_serve_options_from_args(&[OsString::from("--desktop-integration")]).unwrap();
        assert_eq!(enabled.desktop_integration, Some(true));
        let args = serve_args_for_options(&enabled).args;
        assert!(args.contains(&OsString::from("--desktop-integration=true")));
        let inherited = parse_serve_options_from_args(&args).unwrap();
        let disabled =
            parse_serve_options_from_args(&[OsString::from("--desktop-integration=false")])
                .unwrap();
        assert_eq!(
            merge_serve_options(inherited, disabled).desktop_integration,
            Some(false)
        );
        assert_eq!(
            parse_serve_options_from_args(&[])
                .unwrap()
                .desktop_integration,
            None
        );
    }

    use super::*;
    use holon::{
        cli::RuntimeDbDebugCommands,
        config::{provider_registry_for_tests, AltScreenMode, ModelRouteRef},
        runtime_db::RuntimeDb,
    };

    fn test_config() -> AppConfig {
        let home = tempfile::tempdir().unwrap().keep();
        let workspace = tempfile::tempdir().unwrap().keep();
        AppConfig {
            default_agent_id: "default".into(),
            http_addr: "127.0.0.1:7878".into(),
            callback_base_url: "http://127.0.0.1:7878".into(),
            user_home_dir: None,
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
            auth: Default::default(),
            api_cors: Default::default(),
            api_projection: Default::default(),
            local_apps: Default::default(),
            config_file_path: home.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRouteRef::parse_compatible("anthropic/claude-sonnet-4-6").unwrap(),
            fallback_models: Vec::new(),
            vision_model: None,
            image_generation_model: None,
            vision_candidate_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: 8_000,
            max_tool_output_tokens: 64_000,
            command_task_output_retention_bytes: 8 * 1024 * 1024,
            command_task_output_quota_bytes: 64 * 1024 * 1024,
            command_task_min_free_disk_bytes: 512 * 1024 * 1024,
            command_task_min_free_disk_percent: 5,
            disable_provider_fallback: false,
            tui_alternate_screen: AltScreenMode::Auto,
            validated_model_overrides: Default::default(),
            validated_unknown_model_fallback: None,
            model_discovery_cache: Default::default(),
            providers: provider_registry_for_tests(None, Some("dummy"), home.join(".codex")),
            web_config: holon::web::WebConfig::default(),
        }
    }

    #[test]
    fn agent_cli_context_without_inherited_authority_rejects_operator_fallback() {
        let error = authority_class_for_cli_context(Some(AgentInvocationContext {
            caller_agent_id: "agent-a".into(),
            source_task_id: None,
            source_turn_id: None,
            source_work_item_id: None,
            source_activation_id: None,
            inherited_authority_class: None,
        }))
        .unwrap_err();

        assert!(error.to_string().contains(HOLON_CALLER_AUTHORITY_CLASS_ENV));
        assert!(error.to_string().contains("operator authority"));
    }

    #[test]
    fn context_free_cli_uses_operator_authority() {
        assert_eq!(
            authority_class_for_cli_context(None).unwrap(),
            AuthorityClass::OperatorInstruction
        );
    }

    #[test]
    fn tailnet_host_is_client_visible_not_listen_socket() {
        let mut config = test_config();
        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: Some(ServeAccess::Tailnet),
                host: Some("lab.tailnet.ts.net".into()),
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
        )
        .unwrap();

        assert_eq!(config.http_addr, "0.0.0.0:7878");
        assert_eq!(advertise.as_deref(), Some("http://lab.tailnet.ts.net:7878"));
        assert_eq!(config.callback_base_url, "http://127.0.0.1:7878");
        assert_eq!(config.control_auth_mode, ControlAuthMode::Required);
    }

    #[test]
    fn lan_port_updates_listen_and_advertise_urls() {
        let mut config = test_config();
        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: Some(ServeAccess::Lan),
                host: Some("192.168.1.10".into()),
                listen: None,
                port: Some(8787),
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
        )
        .unwrap();

        assert_eq!(config.http_addr, "192.168.1.10:8787");
        assert_eq!(advertise.as_deref(), Some("http://192.168.1.10:8787"));
        assert_eq!(config.callback_base_url, "http://127.0.0.1:8787");
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
            command: DaemonCommands::Start { options, .. },
        } = cli.command
        else {
            panic!("expected daemon start command");
        };

        assert_eq!(options.access, Some(ServeAccess::Lan));
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
    fn stale_template_remote_source_startup_sync_candidates_include_official_when_needed() {
        let home = tempfile::tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.sqlite.lock"),
        )
        .unwrap();
        let config = AgentTemplatesConfigFile::default();

        let candidates = stale_agent_template_remote_source_sync_candidates(&config, &db);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "official");
        assert_eq!(candidates[0].1.url, "https://github.com/holon-run/holon");
    }

    #[test]
    fn stale_template_remote_source_startup_sync_candidates_skip_recent_and_disabled_sources() {
        let home = tempfile::tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.sqlite.lock"),
        )
        .unwrap();
        db.connection()
            .unwrap()
            .execute(
                "INSERT INTO agent_template_remote_source_syncs \
                 (source_id, kind, url, requested_ref, enabled, status, last_synced_at, resolved_ref, resolved_revision, catalog_json, diagnostics_json, error, created_at, updated_at) \
                 VALUES (?1, 'github', ?2, 'main', 1, 'synced', ?3, 'main', NULL, '[]', '[]', NULL, ?3, ?3)",
                (
                    "official",
                    "https://github.com/holon-run/holon",
                    chrono::Utc::now().to_rfc3339(),
                ),
            )
            .unwrap();
        let mut config = AgentTemplatesConfigFile::default();
        config.remote_sources.insert(
            "disabled".into(),
            AgentTemplateRemoteSourceConfigFile {
                url: "https://github.com/example/templates".into(),
                git_ref: Some("main".into()),
                enabled: Some(false),
                credential_profile: None,
            },
        );

        let candidates = stale_agent_template_remote_source_sync_candidates(&config, &db);

        assert!(candidates.is_empty());
    }

    #[test]
    fn agent_lifecycle_commands_parse_with_optional_agent_id() {
        let cli = Cli::parse_from(["holon", "agent", "get", "foo"]);
        let Commands::Agent {
            command: Some(AgentCommands::Get { agent_id, detail }),
        } = cli.command
        else {
            panic!("expected agent get command");
        };
        assert_eq!(agent_id.as_deref(), Some("foo"));
        assert_eq!(detail, AgentDetailArg::Compact);

        let cli = Cli::parse_from(["holon", "agent", "start"]);
        let Commands::Agent {
            command: Some(AgentCommands::Start { agent_id }),
        } = cli.command
        else {
            panic!("expected agent start command");
        };
        assert_eq!(agent_id, None);

        let cli = Cli::parse_from(["holon", "agent", "stop", "foo"]);
        let Commands::Agent {
            command: Some(AgentCommands::Stop { agent_id }),
        } = cli.command
        else {
            panic!("expected agent stop command");
        };
        assert_eq!(agent_id.as_deref(), Some("foo"));

        let cli = Cli::parse_from(["holon", "agent", "abort", "foo"]);
        let Commands::Agent {
            command: Some(AgentCommands::Abort { agent_id }),
        } = cli.command
        else {
            panic!("expected agent abort command");
        };
        assert_eq!(agent_id.as_deref(), Some("foo"));
    }

    #[test]
    fn agent_repair_command_requires_positional_agent_id() {
        let cli = Cli::parse_from(["holon", "agent", "repair", "foo"]);
        assert!(matches!(
            cli.command,
            Commands::Agent {
                command: Some(AgentCommands::Repair { agent_id })
            } if agent_id == "foo"
        ));
    }

    #[test]
    fn agent_timezone_commands_use_positional_agent_id() {
        for action in ["get", "clear"] {
            assert!(Cli::try_parse_from(["holon", "agent", "timezone", action, "foo"]).is_ok());
            assert!(Cli::try_parse_from(["holon", "agent", "timezone", action]).is_ok());
        }
        let cli = Cli::parse_from(["holon", "agent", "timezone", "set", "Asia/Shanghai", "foo"]);
        assert!(matches!(cli.command, Commands::Agent {
            command: Some(AgentCommands::Timezone {
                command: holon::cli::AgentTimezoneCommands::Set { timezone, agent_id }
            })
        } if timezone == "Asia/Shanghai" && agent_id.as_deref() == Some("foo")));
        assert!(Cli::try_parse_from(["holon", "agent", "timezone", "set"]).is_err());
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
                command: Some(AgentCommands::List { parent: None })
            }
        ));

        let cli = Cli::parse_from(["holon", "agent", "list", "--parent", "self"]);
        assert!(matches!(
            cli.command,
            Commands::Agent {
                command: Some(AgentCommands::List {
                    parent: Some(parent)
                })
            } if parent == "self"
        ));
    }

    #[test]
    fn task_run_command_parses_creation_options() {
        let cli = Cli::parse_from([
            "holon", "task", "run", "status", "--cmd", "echo hi", "--agent", "runner",
        ]);
        let Commands::Task {
            command:
                TaskCommands::Run {
                    summary,
                    cmd,
                    agent,
                    ..
                },
        } = cli.command
        else {
            panic!("expected task run command");
        };
        assert_eq!(summary, "status");
        assert_eq!(cmd, "echo hi");
        assert_eq!(agent.as_deref(), Some("runner"));
    }

    #[test]
    fn task_creation_shape_is_clap_enforced() {
        assert!(Cli::try_parse_from(["holon", "task", "run", "summary"]).is_err());
        assert!(Cli::try_parse_from(["holon", "task", "summary", "--cmd", "echo hi"]).is_err());
    }

    #[test]
    fn timer_commands_parse_new_and_legacy_creation_shapes() {
        let cli = Cli::parse_from(["holon", "timer", "--after-ms", "100", "--every-ms", "50"]);
        let Commands::Timer { command, legacy } = cli.command else {
            panic!("expected timer command");
        };
        assert!(command.is_none());
        assert_eq!(legacy.after_ms, Some(100));
        assert_eq!(legacy.every_ms, Some(50));

        let cli = Cli::parse_from([
            "holon",
            "timer",
            "create",
            "--after-ms",
            "100",
            "--agent",
            "runner",
        ]);
        let Commands::Timer {
            command:
                Some(TimerCommands::Create {
                    options:
                        TimerCreateArgs {
                            after_ms, agent, ..
                        },
                }),
            ..
        } = cli.command
        else {
            panic!("expected timer create command");
        };
        assert_eq!(after_ms, 100);
        assert_eq!(agent.as_deref(), Some("runner"));

        let cli = Cli::parse_from(["holon", "timer", "list", "--limit", "10"]);
        assert!(matches!(
            cli.command,
            Commands::Timer {
                command: Some(TimerCommands::List { limit: 10, .. }),
                ..
            }
        ));

        let cli = Cli::parse_from(["holon", "timer", "cancel", "timer-1"]);
        assert!(matches!(
            cli.command,
            Commands::Timer {
                command: Some(TimerCommands::Cancel { timer_id, .. }),
                ..
            } if timer_id == "timer-1"
        ));
    }

    #[test]
    fn timer_command_rejects_conflicting_or_incomplete_shapes() {
        assert!(Cli::try_parse_from(["holon", "timer", "--after-ms", "100", "list"]).is_err());
        assert!(Cli::try_parse_from(["holon", "timer", "create"]).is_err());
        assert!(Cli::try_parse_from(["holon", "timer", "list", "--limit", "0"]).is_err());
    }

    #[test]
    fn task_lifecycle_commands_parse_with_agent_options() {
        let cli = Cli::parse_from(["holon", "task", "list", "--limit", "12"]);
        assert!(matches!(
            cli.command,
            Commands::Task {
                command: TaskCommands::List {
                    limit: 12,
                    agent: None
                }
            }
        ));

        let cli = Cli::parse_from(["holon", "task", "status", "task-1", "--agent", "runner"]);
        let Commands::Task {
            command: TaskCommands::Status { task_id, agent },
        } = cli.command
        else {
            panic!("expected task status command");
        };
        assert_eq!(task_id, "task-1");
        assert_eq!(agent.as_deref(), Some("runner"));

        assert!(Cli::try_parse_from([
            "holon",
            "task",
            "output",
            "task-2",
            "--block",
            "--timeout-ms",
            "100",
        ])
        .is_err());

        let cli = Cli::parse_from(["holon", "task", "input", "task-3", "--text", "hello"]);
        let Commands::Task {
            command:
                TaskCommands::Input {
                    task_id,
                    text,
                    agent,
                },
        } = cli.command
        else {
            panic!("expected task input command");
        };
        assert_eq!(task_id, "task-3");
        assert_eq!(text, "hello");
        assert_eq!(agent, None);

        let cli = Cli::parse_from(["holon", "task", "stop", "task-4"]);
        let Commands::Task {
            command: TaskCommands::Stop { task_id, agent },
        } = cli.command
        else {
            panic!("expected task stop command");
        };
        assert_eq!(task_id, "task-4");
        assert_eq!(agent, None);
    }

    #[test]
    fn work_item_inspection_commands_parse_with_agent_options() {
        let cli = Cli::parse_from(["holon", "work-item", "list", "--agent", "runner"]);
        let Commands::WorkItem {
            command: WorkItemCommands::List { limit, agent },
        } = cli.command
        else {
            panic!("expected work-item list command");
        };
        assert_eq!(limit, 50);
        assert_eq!(agent.as_deref(), Some("runner"));

        let cli = Cli::parse_from([
            "holon",
            "work-item",
            "list",
            "--limit",
            "10",
            "--agent",
            "runner",
        ]);
        let Commands::WorkItem {
            command: WorkItemCommands::List { limit, agent },
        } = cli.command
        else {
            panic!("expected work-item list command");
        };
        assert_eq!(limit, 10);
        assert_eq!(agent.as_deref(), Some("runner"));

        let cli = Cli::parse_from(["holon", "work-item", "get", "work_123"]);
        let Commands::WorkItem {
            command:
                WorkItemCommands::Get {
                    work_item_id,
                    agent,
                },
        } = cli.command
        else {
            panic!("expected work-item get command");
        };
        assert_eq!(work_item_id, "work_123");
        assert_eq!(agent, None);

        assert!(Cli::try_parse_from(["holon", "work-item", "complete", "work_123"]).is_ok());
        assert!(Cli::try_parse_from([
            "holon",
            "work-item",
            "complete",
            "work_123",
            "--report",
            "completed"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "holon",
            "work-item",
            "complete",
            "work_123",
            "--report",
            "completed",
            "--report-file",
            "-"
        ])
        .is_err());
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
    fn debug_runtime_db_agent_relations_defaults_to_report_mode() {
        let cli = Cli::parse_from(["holon", "debug", "runtime-db", "agent-relations", "--json"]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::AgentRelations {
                            apply,
                            no_backup,
                            diagnostic_sample_limit,
                            json,
                        },
                },
        } = cli.command
        else {
            panic!("expected debug runtime-db agent-relations command");
        };
        assert!(!apply);
        assert!(!no_backup);
        assert_eq!(diagnostic_sample_limit, 20);
        assert!(json);
        assert!(Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "agent-relations",
            "--no-backup",
        ])
        .is_err());
    }

    #[test]
    fn debug_runtime_db_agent_relations_parses_apply_options() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "agent-relations",
            "--apply",
            "--no-backup",
            "--diagnostic-sample-limit",
            "7",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::AgentRelations {
                            apply,
                            no_backup,
                            diagnostic_sample_limit,
                            json,
                        },
                },
        } = cli.command
        else {
            panic!("expected debug runtime-db agent-relations command");
        };
        assert!(apply);
        assert!(no_backup);
        assert_eq!(diagnostic_sample_limit, 7);
        assert!(!json);
    }

    #[test]
    fn debug_runtime_db_conversation_input_assignment_rollback_defaults_to_dry_run() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "conversation-input-assignment-rollback",
            "--json",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::ConversationInputAssignmentRollback { apply, json },
                },
        } = cli.command
        else {
            panic!("expected conversation input assignment rollback command");
        };
        assert!(!apply);
        assert!(json);
    }

    #[test]
    fn debug_runtime_db_conversation_input_assignment_rollback_parses_apply() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "conversation-input-assignment-rollback",
            "--apply",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::ConversationInputAssignmentRollback { apply, json },
                },
        } = cli.command
        else {
            panic!("expected conversation input assignment rollback command");
        };
        assert!(apply);
        assert!(!json);
    }

    #[test]
    fn debug_runtime_db_wait_final_brief_publication_defaults_to_report_mode() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "wait-final-brief-publication",
            "--agent",
            "agent-a",
            "--json",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::WaitFinalBriefPublication {
                            apply,
                            no_backup,
                            plan,
                            resume,
                            agent,
                            diagnostic_sample_limit,
                            json,
                        },
                },
        } = cli.command
        else {
            panic!("expected WaitFor final Brief publication repair command");
        };
        assert!(!apply);
        assert!(!no_backup);
        assert!(plan.is_none());
        assert!(!resume);
        assert_eq!(agent.as_deref(), Some("agent-a"));
        assert_eq!(diagnostic_sample_limit, 20);
        assert!(json);
        assert!(Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "wait-final-brief-publication",
            "--no-backup",
        ])
        .is_err());
    }

    #[test]
    fn debug_runtime_db_wait_final_brief_publication_parses_apply_options() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "wait-final-brief-publication",
            "--apply",
            "--plan",
            "repair.sqlite",
            "--no-backup",
            "--diagnostic-sample-limit",
            "7",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::WaitFinalBriefPublication {
                            apply,
                            no_backup,
                            plan,
                            resume,
                            agent,
                            diagnostic_sample_limit,
                            json,
                        },
                },
        } = cli.command
        else {
            panic!("expected WaitFor final Brief publication repair command");
        };
        assert!(apply);
        assert!(no_backup);
        assert_eq!(plan.as_deref(), Some(std::path::Path::new("repair.sqlite")));
        assert!(!resume);
        assert!(agent.is_none());
        assert_eq!(diagnostic_sample_limit, 7);
        assert!(!json);
        assert!(Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "wait-final-brief-publication",
            "--apply",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "wait-final-brief-publication",
            "--apply",
            "--plan",
            "repair.sqlite",
            "--agent",
            "agent-a",
        ])
        .is_err());
    }

    #[test]
    fn debug_runtime_db_wait_final_brief_publication_parses_resume() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "wait-final-brief-publication",
            "--plan",
            "repair.sqlite",
            "--resume",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::WaitFinalBriefPublication {
                            apply,
                            no_backup,
                            plan,
                            resume,
                            agent,
                            diagnostic_sample_limit,
                            json,
                        },
                },
        } = cli.command
        else {
            panic!("expected WaitFor final Brief publication repair command");
        };
        assert!(!apply);
        assert!(!no_backup);
        assert_eq!(plan.as_deref(), Some(std::path::Path::new("repair.sqlite")));
        assert!(resume);
        assert!(agent.is_none());
        assert_eq!(diagnostic_sample_limit, 20);
        assert!(!json);
    }

    #[test]
    fn debug_runtime_db_turn_settlement_defaults_to_dry_run() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "turn-settlement",
            "--agent",
            "agent-a",
            "--turn",
            "turn-a",
            "--json",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::TurnSettlement {
                            apply,
                            no_backup,
                            plan,
                            agent,
                            turn,
                            diagnostic_sample_limit,
                            json,
                        },
                },
        } = cli.command
        else {
            panic!("expected Turn settlement reconciliation command");
        };
        assert!(!apply);
        assert!(!no_backup);
        assert!(plan.is_none());
        assert_eq!(agent.as_deref(), Some("agent-a"));
        assert_eq!(turn.as_deref(), Some("turn-a"));
        assert_eq!(diagnostic_sample_limit, 20);
        assert!(json);
    }

    #[test]
    fn debug_runtime_db_turn_settlement_parses_apply_options() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "runtime-db",
            "turn-settlement",
            "--apply",
            "--plan",
            "turn-settlement.json",
            "--no-backup",
            "--diagnostic-sample-limit",
            "7",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::RuntimeDb {
                    command:
                        RuntimeDbDebugCommands::TurnSettlement {
                            apply,
                            no_backup,
                            plan,
                            agent,
                            turn,
                            diagnostic_sample_limit,
                            json,
                        },
                },
        } = cli.command
        else {
            panic!("expected Turn settlement reconciliation command");
        };
        assert!(apply);
        assert!(no_backup);
        assert_eq!(
            plan.as_deref(),
            Some(std::path::Path::new("turn-settlement.json"))
        );
        assert!(agent.is_none());
        assert!(turn.is_none());
        assert_eq!(diagnostic_sample_limit, 7);
        assert!(!json);
        assert!(Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "turn-settlement",
            "--apply",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "holon",
            "debug",
            "runtime-db",
            "turn-settlement",
            "--apply",
            "--plan",
            "turn-settlement.json",
            "--turn",
            "turn-a",
        ])
        .is_err());
    }

    #[test]
    fn debug_scheduler_recovery_command_parses_read_only_options() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-recovery",
            "--agent",
            "pm",
            "--json",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::SchedulerRecovery {
                    agent,
                    message_id,
                    all_affected,
                    json,
                    apply,
                    no_backup,
                    ..
                },
        } = cli.command
        else {
            panic!("expected debug scheduler-recovery command");
        };
        assert_eq!(agent.as_deref(), Some("pm"));
        assert!(message_id.is_none());
        assert!(!all_affected);
        assert!(json);
        assert!(!apply);
        assert!(!no_backup);
    }

    #[test]
    fn debug_scheduler_recovery_all_affected_conflicts_with_selectors() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-recovery",
            "--all-affected",
            "--json",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::SchedulerRecovery {
                    agent,
                    message_id,
                    all_affected,
                    json,
                    apply,
                    no_backup,
                    ..
                },
        } = cli.command
        else {
            panic!("expected debug scheduler-recovery command");
        };
        assert!(agent.is_none());
        assert!(message_id.is_none());
        assert!(all_affected);
        assert!(json);
        assert!(!apply);
        assert!(!no_backup);

        for selector in [["--agent", "pm"], ["--message-id", "message-1"]] {
            assert!(Cli::try_parse_from([
                "holon",
                "debug",
                "scheduler-recovery",
                "--all-affected",
                selector[0],
                selector[1],
            ])
            .is_err());
        }
    }

    #[test]
    fn debug_scheduler_recovery_rearm_requires_explicit_identity_agent_and_apply() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-recovery",
            "--agent",
            "pm",
            "--apply",
            "--rearm-task-result",
            "result-a",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::SchedulerRecovery {
                    rearm_task_result, ..
                },
        } = cli.command
        else {
            panic!("expected debug scheduler-recovery command");
        };
        assert_eq!(rearm_task_result.as_deref(), Some("result-a"));
        for args in [
            vec!["--rearm-task-result", "result-a"],
            vec!["--agent", "pm", "--rearm-task-result", "result-a"],
            vec!["--apply", "--rearm-task-result", "result-a"],
            vec![
                "--agent",
                "pm",
                "--apply",
                "--all-affected",
                "--rearm-task-result",
                "result-a",
            ],
            vec![
                "--agent",
                "pm",
                "--apply",
                "--message-id",
                "message-a",
                "--rearm-task-result",
                "result-a",
            ],
        ] {
            assert!(Cli::try_parse_from(
                ["holon", "debug", "scheduler-recovery"]
                    .into_iter()
                    .chain(args)
            )
            .is_err());
        }
    }

    #[test]
    fn debug_scheduler_recovery_no_backup_requires_apply() {
        assert!(
            Cli::try_parse_from(["holon", "debug", "scheduler-recovery", "--no-backup",]).is_err()
        );

        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-recovery",
            "--apply",
            "--no-backup",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::SchedulerRecovery {
                    apply, no_backup, ..
                },
        } = cli.command
        else {
            panic!("expected debug scheduler-recovery command");
        };
        assert!(apply);
        assert!(no_backup);
    }

    #[test]
    fn debug_scheduler_recovery_command_parses_message_selector() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-recovery",
            "--message-id",
            "message:task-restart:task-1",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::SchedulerRecovery {
                    message_id, apply, ..
                },
        } = cli.command
        else {
            panic!("expected debug scheduler-recovery command");
        };
        assert_eq!(message_id.as_deref(), Some("message:task-restart:task-1"));
        assert!(!apply);
    }

    #[test]
    fn hidden_scheduler_recovery_fixture_parses_objective() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-recovery-fixture",
            "--agent",
            "runner",
            "--objective",
            "terminal before settlement",
            "--json",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::SchedulerRecoveryFixture {
                    agent,
                    objective,
                    json,
                },
        } = cli.command
        else {
            panic!("expected hidden scheduler-recovery-fixture command");
        };
        assert_eq!(agent.as_deref(), Some("runner"));
        assert_eq!(objective, "terminal before settlement");
        assert!(json);
    }

    #[test]
    fn hidden_scheduler_restart_fixture_parses_checkpoint() {
        let cli = Cli::parse_from([
            "holon",
            "debug",
            "scheduler-restart-fixture",
            "--agent",
            "runner",
            "--checkpoint",
            "ingress_queue_admission",
            "--objective",
            "atomic ingress admission",
            "--json",
        ]);
        let Commands::Debug {
            command:
                DebugCommands::SchedulerRestartFixture {
                    agent,
                    checkpoint,
                    stage,
                    objective,
                    json,
                },
        } = cli.command
        else {
            panic!("expected hidden scheduler-restart-fixture command");
        };
        assert_eq!(agent.as_deref(), Some("runner"));
        assert_eq!(checkpoint, "ingress_queue_admission");
        assert_eq!(stage, "prepare");
        assert_eq!(objective, "atomic ingress admission");
        assert!(json);
    }

    #[test]
    fn hidden_offline_scheduler_commands_skip_runtime_model_resolution() {
        for args in [
            vec!["holon", "context"],
            vec!["holon", "commands"],
            vec!["holon", "events", "tail", "--agent", "runner", "--offline"],
            vec!["holon", "debug", "scheduler-recovery"],
            vec![
                "holon",
                "debug",
                "scheduler-recovery-fixture",
                "--objective",
                "terminal before settlement",
            ],
            vec![
                "holon",
                "debug",
                "scheduler-restart-fixture",
                "--checkpoint",
                "ingress_queue_admission",
                "--objective",
                "atomic ingress admission",
            ],
        ] {
            let cli = Cli::parse_from(args);
            assert!(runtime_command_uses_config_inspection(&cli.command));
        }
    }

    #[test]
    fn offline_event_page_reads_runtime_db_in_stable_envelope_shape() {
        let config = test_config();
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
                .unwrap();
        runtime_db
            .audit_events()
            .append(
                Some("runner"),
                &AuditEvent::legacy(
                    "provider_round_completed",
                    serde_json::json!({ "round": 1, "input_tokens": 10 }),
                ),
            )
            .unwrap();
        runtime_db
            .audit_events()
            .append(
                Some("runner"),
                &AuditEvent::legacy("tool_executed", serde_json::json!({ "tool_name": "Read" })),
            )
            .unwrap();

        let page = offline_event_page(
            &config,
            "runner",
            None,
            None,
            1,
            holon::cli::EventPageOrderCli::Asc,
        )
        .unwrap();

        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event_type, "provider_round_completed");
        assert_eq!(page.events[0].payload["round"], 1);
        assert_eq!(page.newest_seq, Some(page.events[0].event_seq));
        assert!(page.has_newer);
        assert!(!page.has_older);

        let clamped = offline_event_page(
            &config,
            "runner",
            None,
            None,
            0,
            holon::cli::EventPageOrderCli::Asc,
        )
        .unwrap();
        assert_eq!(clamped.limit, 1);
        assert_eq!(clamped.events.len(), 1);
    }

    #[test]
    fn debug_performance_command_parses_json_flag() {
        let cli = Cli::parse_from(["holon", "debug", "performance", "--json"]);
        let Commands::Debug {
            command: DebugCommands::Performance { json },
        } = cli.command
        else {
            panic!("expected debug performance command");
        };
        assert!(json);
    }

    #[test]
    fn debug_trace_command_parses_query_modes() {
        let cli = Cli::parse_from(["holon", "debug", "trace", "trace-id", "--json"]);
        let Commands::Debug {
            command:
                DebugCommands::Trace {
                    trace_id,
                    search,
                    json,
                },
        } = cli.command
        else {
            panic!("expected debug trace command");
        };
        assert_eq!(trace_id.as_deref(), Some("trace-id"));
        assert_eq!(search, None);
        assert!(json);

        let cli = Cli::parse_from(["holon", "debug", "trace", "--search", "message-id"]);
        let Commands::Debug {
            command:
                DebugCommands::Trace {
                    trace_id,
                    search,
                    json,
                },
        } = cli.command
        else {
            panic!("expected debug trace search command");
        };
        assert_eq!(trace_id, None);
        assert_eq!(search.as_deref(), Some("message-id"));
        assert!(!json);

        assert!(Cli::try_parse_from([
            "holon",
            "debug",
            "trace",
            "trace-id",
            "--search",
            "message-id",
        ])
        .is_err());
    }

    #[test]
    fn export_scheduler_fixture_writes_replay_harness_shape() {
        let config = test_config();
        let agent_id = "default";
        let host = RuntimeHost::new(config.clone()).unwrap();
        let storage = host.agent_storage(agent_id).unwrap();
        let mut work_item = holon::types::WorkItemRecord::new(
            "default",
            "fixture work",
            holon::types::WorkItemState::Open,
        );
        work_item.id = "work-1".into();
        storage.insert_work_item(&work_item).unwrap();
        let mut agent = holon::types::AgentState::new("default");
        agent.current_work_item_id = Some(work_item.id.clone());
        storage.write_agent(&agent).unwrap();
        work_item.revision = 2;
        storage.update_work_item_expected(&work_item, 1).unwrap();

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
    fn export_scheduler_fixture_reads_agent_state_from_runtime_db() {
        let config = test_config();
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
                .unwrap();
        let mut work_item = holon::types::WorkItemRecord::new(
            "default",
            "fixture work",
            holon::types::WorkItemState::Open,
        );
        work_item.id = "work-db".into();
        runtime_db.work_items().insert_new(&work_item).unwrap();
        let mut agent = holon::types::AgentState::new("default");
        agent.current_work_item_id = Some(work_item.id);
        runtime_db.agent_states().upsert(&agent).unwrap();

        let output = tempfile::tempdir().unwrap();
        export_scheduler_fixture(&config, None, output.path()).unwrap();

        let agent_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output.path().join("agent.json")).unwrap())
                .unwrap();
        assert_eq!(agent_json["current_work_item_id"].as_str(), Some("work-db"));
    }

    #[test]
    fn daemon_start_passes_inline_token_through_env_not_argv() {
        let options = ServeOptions {
            reset_serve_options: false,
            desktop_integration_if_unconfigured: false,
            access: Some(ServeAccess::Lan),
            host: Some("192.168.1.10".into()),
            listen: None,
            port: None,
            advertise: None,
            token: Some("secret-token".into()),
            token_file: None,
            web_dist: None,
            desktop_integration: None,
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
    fn serve_profile_recorded_token_file_survives_child_token_environment() {
        let home = tempfile::tempdir().unwrap();
        let token_path = home.path().join("control-token");
        fs::write(&token_path, "recorded-file-token").unwrap();
        let config = test_config();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec!["--token-file", token_path.to_str().unwrap()],
            true,
        );
        let recorded = resolve_recorded_daemon_serve_plan(&config, &metadata).unwrap();
        assert_eq!(
            recorded.profile.authentication,
            ServeProfileAuthentication::TokenFile { path: token_path }
        );
        assert_eq!(
            recorded.config.control_token.as_deref(),
            Some("recorded-file-token")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serve_profile_running_legacy_start_compares_effective_options() {
        for recorded_args in [vec![], vec!["--listen", "127.0.0.1:9"], vec!["--port", "9"]] {
            let home = tempfile::tempdir().unwrap();
            let mut config = test_config();
            config.home_dir = home.path().to_path_buf();
            config.data_dir = config.home_dir.clone();
            config.socket_path = config.home_dir.join("run/holon.sock");
            config.http_addr = "127.0.0.1:9".into();
            config.control_token = None;
            let mut metadata = runtime_metadata_with_serve_args(&config, recorded_args, false);
            metadata.pid = std::process::id();
            let initial = resolve_recorded_daemon_serve_plan(&config, &metadata).unwrap();
            metadata.config_fingerprint =
                holon::daemon::config_fingerprint(&initial.config).unwrap();
            let paths = holon::daemon::daemon_paths(&config);
            fs::create_dir_all(config.run_dir()).unwrap();
            fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
            assert!(!holon::daemon::serve_profile_path(&config).exists());

            let start = |explicit: &[OsString]| {
                let options = parse_serve_options_from_args(explicit).unwrap();
                let input_config = config.clone();
                daemon_launch_with_profile(
                    &config,
                    false,
                    false,
                    None,
                    move |profile, metadata| {
                        resolve_daemon_serve_plan_with_inputs(
                            input_config,
                            options,
                            profile,
                            metadata,
                            parse_serve_options_from_args(&[])?,
                            false,
                        )
                    },
                    resolve_recorded_daemon_serve_plan,
                )
            };
            let result = start(&[]).await.unwrap();
            assert!(!result.process_created);
            assert_eq!(result.status.pid, Some(metadata.pid));
            assert_eq!(
                holon::daemon::load_serve_profile(&config).unwrap(),
                Some(initial.profile)
            );
            let result = start(&["--listen".into(), "127.0.0.1:9".into()])
                .await
                .unwrap();
            assert!(!result.process_created);
            assert_eq!(result.status.pid, Some(metadata.pid));
            let accepted = holon::daemon::load_serve_profile(&config).unwrap();
            let error = start(&["--advertise".into(), "http://127.0.0.1:9999".into()])
                .await
                .unwrap_err();
            assert!(error.to_string().contains("different serve options"));
            assert_eq!(
                holon::daemon::load_serve_profile(&config).unwrap(),
                accepted
            );
        }
    }

    fn runtime_metadata_with_serve_args(
        config: &AppConfig,
        args: Vec<&str>,
        control_token_env_configured: bool,
    ) -> holon::daemon::RuntimeServiceMetadata {
        holon::daemon::RuntimeServiceMetadata {
            pid: 123,
            home_dir: config.home_dir.clone(),
            socket_path: config.socket_path.clone(),
            http_addr: config.http_addr.clone(),
            started_at: chrono::Utc::now(),
            config_fingerprint: "test-fingerprint".into(),
            product_version: env!("HOLON_VERSION").into(),
            control_protocol_version: holon::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
            lifecycle_owner: holon::daemon::DaemonLifecycleOwner::Standalone,
            executable_path: std::env::current_exe().unwrap(),
            serve_args: args.into_iter().map(String::from).collect(),
            control_token_env_configured,
        }
    }

    #[test]
    fn daemon_restart_authentication_only_preserves_network_and_desktop_options() {
        let mut config = test_config();
        config.control_token = None;
        let token_file = config.home_dir.join("control.token");
        fs::write(&token_file, "file-secret").unwrap();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec![
                "--access",
                "lan",
                "--host",
                "192.168.1.10",
                "--listen",
                "0.0.0.0:8787",
                "--advertise",
                "https://custom.example.test",
                "--desktop-integration=false",
            ],
            false,
        );
        let launch = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: None,
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: Some(token_file.clone()),
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.http_addr, "0.0.0.0:8787");
        assert_eq!(config.callback_base_url, "http://127.0.0.1:8787");
        assert_eq!(config.control_token.as_deref(), Some("file-secret"));
        assert_eq!(
            launch.args,
            vec![
                OsString::from("--access"),
                OsString::from("lan"),
                OsString::from("--host"),
                OsString::from("192.168.1.10"),
                OsString::from("--listen"),
                OsString::from("0.0.0.0:8787"),
                OsString::from("--advertise"),
                OsString::from("https://custom.example.test"),
                OsString::from("--token-file"),
                token_file.into_os_string(),
                OsString::from("--desktop-integration=false"),
            ]
        );
        assert_eq!(launch.control_token_env.as_deref(), Some("file-secret"));
    }

    #[test]
    fn daemon_restart_inherits_recorded_start_options_and_overrides_explicit_flags() {
        let mut config = test_config();
        config.control_token = None;
        let token_file = config.home_dir.join("control.token");
        fs::write(&token_file, "file-secret").unwrap();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec![
                "--access",
                "lan",
                "--host",
                "192.168.1.10",
                "--port",
                "8787",
                "--advertise",
                "http://old.example.test:8787",
                "--token-file",
                token_file.to_str().unwrap(),
            ],
            false,
        );

        let launch = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: None,
                host: Some("192.168.1.11".into()),
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.http_addr, "192.168.1.11:8787");
        assert_eq!(config.callback_base_url, "http://127.0.0.1:8787");
        assert_eq!(config.control_token.as_deref(), Some("file-secret"));
        assert_eq!(
            launch.args,
            vec![
                OsString::from("--access"),
                OsString::from("lan"),
                OsString::from("--host"),
                OsString::from("192.168.1.11"),
                OsString::from("--port"),
                OsString::from("8787"),
                OsString::from("--advertise"),
                OsString::from("http://192.168.1.11:8787"),
                OsString::from("--token-file"),
                token_file.as_os_str().to_os_string(),
                OsString::from("--desktop-integration=false"),
            ]
        );
        assert_eq!(launch.control_token_env.as_deref(), Some("file-secret"));
        assert!(!launch
            .args
            .iter()
            .any(|arg| arg == "http://old.example.test:8787"));
    }

    #[test]
    fn daemon_restart_explicit_local_access_clears_inherited_advertise_url() {
        let mut config = test_config();
        config.control_token = None;
        let token_file = config.home_dir.join("control.token");
        fs::write(&token_file, "file-secret").unwrap();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec![
                "--access",
                "lan",
                "--host",
                "192.168.1.10",
                "--port",
                "8787",
                "--advertise",
                "http://old.example.test:8787",
                "--token-file",
                token_file.to_str().unwrap(),
            ],
            false,
        );

        let launch = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: Some(ServeAccess::Local),
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.http_addr, "127.0.0.1:8787");
        assert_ne!(config.callback_base_url, "http://old.example.test:8787");
        assert_eq!(config.control_token.as_deref(), Some("file-secret"));
        assert!(!launch.args.iter().any(|arg| arg == "--advertise"));
        assert!(!launch
            .args
            .iter()
            .any(|arg| arg == "http://old.example.test:8787"));
    }

    #[test]
    fn daemon_restart_explicit_tunnel_access_clears_inherited_advertise_url() {
        let mut config = test_config();
        config.control_token = None;
        let token_file = config.home_dir.join("control.token");
        fs::write(&token_file, "file-secret").unwrap();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec![
                "--access",
                "lan",
                "--host",
                "192.168.1.10",
                "--port",
                "8787",
                "--advertise",
                "http://old.example.test:8787",
                "--token-file",
                token_file.to_str().unwrap(),
            ],
            false,
        );

        let launch = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: Some(ServeAccess::Tunnel),
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.http_addr, "127.0.0.1:8787");
        assert_ne!(config.callback_base_url, "http://old.example.test:8787");
        assert_eq!(config.control_token.as_deref(), Some("file-secret"));
        assert!(!launch.args.iter().any(|arg| arg == "--advertise"));
        assert!(!launch
            .args
            .iter()
            .any(|arg| arg == "http://old.example.test:8787"));
    }

    #[test]
    fn daemon_restart_listen_and_port_override_their_inherited_counterpart() {
        let mut config = test_config();
        let token_file = config.home_dir.join("control.token");
        fs::write(&token_file, "file-secret").unwrap();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec![
                "--access",
                "lan",
                "--host",
                "192.168.1.10",
                "--port",
                "8787",
                "--token-file",
                token_file.to_str().unwrap(),
            ],
            false,
        );

        let launch = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: None,
                host: None,
                listen: Some("0.0.0.0:8989".into()),
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.http_addr, "0.0.0.0:8989");
        assert!(launch.args.iter().any(|arg| arg == "--listen"));
        assert!(launch.args.iter().any(|arg| arg == "0.0.0.0:8989"));
        assert!(!launch.args.iter().any(|arg| arg == "--port"));

        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec![
                "--access",
                "lan",
                "--host",
                "192.168.1.10",
                "--listen",
                "0.0.0.0:7878",
                "--token-file",
                token_file.to_str().unwrap(),
            ],
            false,
        );

        let launch = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: None,
                host: None,
                listen: None,
                port: Some(8989),
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.http_addr, "192.168.1.10:8989");
        assert!(launch.args.iter().any(|arg| arg == "--port"));
        assert!(launch.args.iter().any(|arg| arg == "8989"));
        assert!(!launch.args.iter().any(|arg| arg == "--listen"));
    }

    #[test]
    fn daemon_restart_inline_token_overrides_inherited_token_file_without_argv_secret() {
        let mut config = test_config();
        config.control_token = None;
        let token_file = config.home_dir.join("control.token");
        fs::write(&token_file, "file-secret").unwrap();
        let metadata = runtime_metadata_with_serve_args(
            &config,
            vec![
                "--access",
                "lan",
                "--host",
                "192.168.1.10",
                "--token-file",
                token_file.to_str().unwrap(),
            ],
            false,
        );

        let launch = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: None,
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: Some("restart-secret".into()),
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.control_token.as_deref(), Some("restart-secret"));
        assert_eq!(launch.control_token_env.as_deref(), Some("restart-secret"));
        assert!(!launch
            .args
            .iter()
            .any(|arg| arg == "--token-file" || arg == token_file.as_os_str()));
        assert!(!launch.args.iter().any(|arg| arg == "restart-secret"));
    }

    #[test]
    fn daemon_restart_requires_new_token_when_previous_inline_token_was_not_persisted() {
        let mut config = test_config();
        config.control_token = None;
        let metadata = runtime_metadata_with_serve_args(&config, vec!["--access", "local"], true);

        let error = restart_serve_launch_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: None,
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
            Some(&metadata),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("saved daemon authentication requires"));
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
    fn skills_install_remote_cli_builds_remote_request() {
        let cli = Cli::parse_from([
            "holon",
            "skills",
            "install",
            "vercel-labs/agent-skills",
            "--remote",
            "--skill",
            "pr-review",
        ]);
        let Commands::Skills {
            command:
                SkillsCommands::Install {
                    name_or_path,
                    remote,
                    skill,
                    copy,
                    agent,
                },
        } = cli.command
        else {
            panic!("expected skills install command");
        };

        assert_eq!(name_or_path, "vercel-labs/agent-skills");
        assert!(remote);
        assert_eq!(skill.as_deref(), Some("pr-review"));
        assert!(!copy);
        assert_eq!(agent, None);
    }

    #[test]
    fn unspecified_listen_accepts_explicit_advertise_url() {
        let mut config = test_config();
        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: Some(ServeAccess::Lan),
                host: None,
                listen: Some("0.0.0.0:7878".into()),
                port: None,
                advertise: Some("http://lab.example.test:7878".into()),
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
        )
        .unwrap();

        assert_eq!(config.http_addr, "0.0.0.0:7878");
        assert_eq!(advertise.as_deref(), Some("http://lab.example.test:7878"));
        assert_eq!(config.callback_base_url, "http://127.0.0.1:7878");
        assert_eq!(config.control_auth_mode, ControlAuthMode::Required);
    }

    #[test]
    fn callback_base_url_is_normalized_when_validated() {
        let mut config = test_config();
        config.callback_base_url = "http://lab.example.test:7878/#fragment".into();

        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                reset_serve_options: false,
                desktop_integration_if_unconfigured: false,
                access: Some(ServeAccess::Local),
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
                desktop_integration: None,
            },
        )
        .unwrap();

        assert_eq!(advertise, None);
        // Without HOLON_CALLBACK_BASE_URL, callback_base_url is always derived
        // from the listen port as localhost, ignoring any prior value.
        assert_eq!(config.callback_base_url, "http://127.0.0.1:7878");
    }

    #[test]
    fn latency_diagnostics_groups_turn_phase_events() {
        let at = |secs: i64| {
            chrono::DateTime::parse_from_rfc3339(&format!("2026-05-07T00:00:{secs:02}Z"))
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let event = |kind: &str, created_at, data| AuditEvent {
            id: holon::ids::audit_event_id(),
            event_seq: 0,
            event_log_epoch: String::new(),
            created_at,
            kind: kind.into(),
            contract_version: holon::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
            payload_schema: holon::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
            payload_schema_version: 1,
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

async fn handle_events_command(config: &AppConfig, command: EventsCommands) -> Result<()> {
    match command {
        EventsCommands::Tail {
            before_seq,
            after_seq,
            limit,
            order,
            max_level,
            agent,
            offline,
        } => {
            if offline {
                let agent = agent.ok_or_else(|| {
                    anyhow!(
                        "--offline requires an explicit --agent when no configured default is available"
                    )
                })?;
                anyhow::ensure!(
                    max_level.is_none(),
                    "--max-level is not supported with --offline"
                );
                return print_json(&serde_json::to_value(offline_event_page(
                    config, &agent, before_seq, after_seq, limit, order,
                )?)?);
            }
            let client = LocalClient::new(config.clone())?;
            let agent = resolve_agent_id(&client, agent).await?;
            let page = client
                .agent_events_page(
                    &agent,
                    holon::client::EventPageRequest {
                        before_seq,
                        after_seq,
                        limit: Some(limit),
                        order: Some(order.to_string()),
                        max_level: max_level.map(|level| level.to_string()),
                    },
                )
                .await?;
            print_json(&serde_json::to_value(page)?)
        }
        EventsCommands::Stream {
            after_seq,
            limit,
            max_events,
            agent,
        } => {
            let client = LocalClient::new(config.clone())?;
            let agent = resolve_agent_id(&client, agent).await?;
            let mut stream = client
                .stream_agent_events(
                    &agent,
                    holon::client::EventStreamRequest { after_seq, limit },
                )
                .await?;
            let mut seen = 0usize;
            loop {
                let event = stream.next_event().await?;
                println!("{}", serde_json::to_string(&event.data)?);
                seen += 1;
                if max_events.is_some_and(|max| seen >= max) {
                    return Ok(());
                }
            }
        }
    }
}

fn offline_event_page(
    config: &AppConfig,
    agent_id: &str,
    before_seq: Option<u64>,
    after_seq: Option<u64>,
    limit: usize,
    order: holon::cli::EventPageOrderCli,
) -> Result<holon::client::EventPageResponse> {
    let limit = limit.clamp(1, holon::http::MAX_EVENT_STREAM_WINDOW);
    let runtime_db =
        RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
    let descending = matches!(order, holon::cli::EventPageOrderCli::Desc);
    let mut events = runtime_db.audit_events().range(
        Some(agent_id),
        before_seq,
        after_seq,
        descending,
        limit.saturating_add(1),
    )?;
    let has_more = events.len() > limit;
    if has_more {
        events.truncate(limit);
    }
    let oldest_seq = events.iter().map(|event| event.event_seq).min();
    let newest_seq = events.iter().map(|event| event.event_seq).max();
    let event_log_epoch = runtime_db.event_log_epoch()?;
    let envelopes = events
        .into_iter()
        .map(|event| {
            let legacy = holon::runtime_event::is_legacy_event_shape(
                &event.payload_schema,
                event.contract_version,
            );
            holon::client::StreamEventEnvelope {
                projection_effect: None,
                id: event.id,
                event_seq: event.event_seq,
                event_log_epoch: Some(if event.event_log_epoch.is_empty() {
                    event_log_epoch.clone()
                } else {
                    event.event_log_epoch
                }),
                ts: event.created_at,
                agent_id: agent_id.to_string(),
                event_type: event.kind,
                payload_schema: (!legacy).then(|| event.payload_schema.clone()),
                payload_schema_version: (!legacy).then_some(event.payload_schema_version),
                payload: event.data,
            }
        })
        .collect();
    Ok(holon::client::EventPageResponse {
        events: envelopes,
        event_log_epoch,
        contract_version: holon::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
        oldest_seq,
        newest_seq,
        cursor_seq: runtime_db.audit_events().latest_event_seq(Some(agent_id))?,
        has_older: descending && has_more,
        has_newer: !descending && has_more,
        order: order.to_string(),
        limit,
    })
}

async fn handle_debug_command(config: AppConfig, command: DebugCommands) -> Result<()> {
    match command {
        DebugCommands::Prompt {
            text,
            agent,
            authority_class,
            manifest,
            budget,
        } => dump_prompt(config, text, agent, authority_class, manifest, budget).await,
        DebugCommands::Latency {
            agent,
            limit,
            events_limit,
        } => print_latency_diagnostics(&config, agent, limit, events_limit),
        DebugCommands::Performance { json } => print_performance_diagnostics(&config, json).await,
        DebugCommands::Trace {
            trace_id,
            search,
            json,
        } => print_trace_diagnostics(&config, trace_id, search, json).await,
        DebugCommands::RuntimeDb { command } => handle_runtime_db_debug_command(&config, command),
        DebugCommands::SchedulerFixture { agent, output } => {
            export_scheduler_fixture(&config, agent, &output)
        }
        DebugCommands::SchedulerRecovery {
            agent,
            message_id,
            all_affected,
            json,
            apply,
            rearm_task_result,
            no_backup,
        } => print_scheduler_recovery_report(
            &config,
            agent,
            message_id,
            all_affected,
            json,
            apply,
            rearm_task_result,
            no_backup,
        ),
        DebugCommands::SchedulerRecoveryFixture {
            agent,
            objective,
            json,
        } => seed_scheduler_recovery_fixture(&config, agent, objective, json).await,
        DebugCommands::SchedulerRestartFixture {
            agent,
            checkpoint,
            stage,
            objective,
            json,
        } => {
            seed_scheduler_restart_fixture(&config, agent, checkpoint, stage, objective, json).await
        }
    }
}

async fn seed_scheduler_recovery_fixture(
    config: &AppConfig,
    agent: Option<String>,
    objective: String,
    json: bool,
) -> Result<()> {
    holon::runtime::require_scheduler_acceptance_fixtures_enabled()?;
    let _maintenance_lock = RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path())
        .context("scheduler recovery fixture requires holon serve to be stopped")?;
    let agent_id = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let fixture = seed_scheduler_terminal_recovery_fixture(config, &agent_id, objective).await?;
    if json {
        print_json(&serde_json::to_value(fixture)?)
    } else {
        println!("{}", serde_json::to_string_pretty(&fixture)?);
        Ok(())
    }
}

async fn seed_scheduler_restart_fixture(
    config: &AppConfig,
    agent: Option<String>,
    checkpoint: String,
    stage: String,
    objective: String,
    json: bool,
) -> Result<()> {
    holon::runtime::require_scheduler_acceptance_fixtures_enabled()?;
    let _maintenance_lock = RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path())
        .context("scheduler restart fixture requires holon serve to be stopped")?;
    let agent_id = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let fixture = holon::runtime::seed_scheduler_restart_fixture(
        config,
        &agent_id,
        &checkpoint,
        &stage,
        objective,
    )
    .await?;
    if json {
        print_json(&fixture)
    } else {
        println!("{}", serde_json::to_string_pretty(&fixture)?);
        Ok(())
    }
}

fn print_scheduler_recovery_report(
    config: &AppConfig,
    agent: Option<String>,
    message_id: Option<String>,
    all_affected: bool,
    json: bool,
    apply: bool,
    rearm_task_result: Option<String>,
    no_backup: bool,
) -> Result<()> {
    if rearm_task_result.is_some() && cli_invocation_context()?.is_some() {
        return Err(anyhow!(
            "agent_scope_denied: task-result recovery rearm requires operator mode"
        ));
    }
    let runtime_db = RuntimeDb::open_for_scheduler_recovery(
        config.runtime_db_path(),
        config.runtime_db_lock_path(),
    )?;
    let _maintenance_lock = apply
        .then(|| RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path()))
        .transpose()
        .context("scheduler recovery apply requires holon serve to be stopped")?;
    if all_affected {
        return print_all_scheduler_recovery_reports(config, &runtime_db, json, apply, no_backup);
    }
    let agent_id = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let storage = AppStorage::new_for_agent(
        config.agent_root_dir().join(&agent_id),
        agent_id.clone(),
        runtime_db.clone(),
    )?;
    let mut report = holon::runtime::scheduler_recovery_report(&storage, &runtime_db, &agent_id)?;
    filter_scheduler_recovery_report(&mut report, message_id.as_deref(), true)?;
    let mut apply_result = None;
    if apply {
        let backup_policy = if no_backup {
            holon::runtime::SchedulerRecoveryBackupPolicy::SkipApproved
        } else {
            holon::runtime::SchedulerRecoveryBackupPolicy::Required
        };
        let applied = if let Some(identity) = rearm_task_result.as_deref() {
            holon::runtime::rearm_scheduler_task_result_recovery(
                &runtime_db,
                &agent_id,
                identity,
                backup_policy,
            )?
        } else {
            holon::runtime::apply_scheduler_recovery_plan_with_backup_policy(
                &storage,
                &runtime_db,
                &agent_id,
                &report,
                backup_policy,
            )?
        };
        apply_result = Some((applied, backup_policy));
        report = holon::runtime::scheduler_recovery_report(&storage, &runtime_db, &agent_id)?;
        filter_scheduler_recovery_report(&mut report, message_id.as_deref(), false)?;
    }
    if json {
        return print_json(&serde_json::json!({
            "report": report,
            "apply": apply_result.map(|((changed, backup_path), backup_policy)| serde_json::json!({
                "changed": changed,
                "backup_path": backup_path,
                "backup_policy": backup_policy,
                "backup_created": backup_path.is_some(),
            })),
        }));
    }
    println!(
        "Scheduler recovery candidates for {} (execution partition initialized: {})",
        report.agent_id, report.execution_partition_initialized
    );
    if let Some(((changed, backup_path), backup_policy)) = apply_result {
        println!(
            "- applied recovery changes={} backup_policy={} backup={}",
            changed,
            serde_json::to_value(backup_policy)?
                .as_str()
                .unwrap_or("unknown"),
            backup_path
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not-created".into())
        );
    }
    for candidate in report.candidates {
        println!(
            "- {}: eligible={} reason={} target={:?}",
            candidate.message_id,
            candidate.eligible,
            candidate.reason,
            candidate.target_queue_status
        );
    }
    for candidate in report.task_result_claim_recoveries {
        println!(
            "- task_result_claim:{} attempt={} work_item={} health={:?} lane_blocked={} age_seconds={} eligible={} decision={} generation={} reason={} target={:?}",
            candidate.message_id,
            candidate.activation_id,
            candidate.work_item_id,
            candidate.health,
            candidate.lane_blocked,
            candidate.claim_age_seconds,
            candidate.eligible,
            candidate.recovery_decision,
            candidate.recovery_generation,
            candidate.reason,
            candidate
                .proposed_queue_entry
                .as_ref()
                .map(|entry| &entry.status),
        );
    }
    for result in report.pending_task_results {
        println!(
            "- pending_task_result:{} identity={} task={} owner={:?} state={} deferred={:?} next_recheck={:?} epoch={} attempts={}/{} paused_at={:?} root={:?}",
            result.message_id,
            result.result_identity,
            result.task_id,
            result.owner,
            result.state,
            result.deferred_reason,
            result.next_recheck_at,
            result.recovery_epoch,
            result.recovery_attempts,
            result.recovery_attempt_limit,
            result.recovery_paused_at,
            result.last_recovery_root_attempt_id,
        );
    }
    if report.pending_task_results_truncated {
        println!("- pending_task_results: truncated at 128 records");
    }
    for candidate in report.continuation_reconciliations {
        println!(
            "- continuation:{} parent={} stale_target={:?} active_target={} eligible={} reason={}",
            candidate.continuation_id,
            candidate.work_item_id,
            candidate.stale_active_work_item_id,
            candidate.active_work_item_id,
            candidate.eligible,
            candidate.reason,
        );
    }
    Ok(())
}

fn print_all_scheduler_recovery_reports(
    config: &AppConfig,
    runtime_db: &RuntimeDb,
    json: bool,
    apply: bool,
    no_backup: bool,
) -> Result<()> {
    let before = runtime_db.retired_scheduler_cleanup_inventory()?;
    let agent_ids = before.affected_agents();
    let mut storages = Vec::with_capacity(agent_ids.len());
    let mut reports = Vec::with_capacity(agent_ids.len());
    let mut report_errors = Vec::new();
    for agent_id in &agent_ids {
        let storage = AppStorage::new_for_agent(
            config.agent_root_dir().join(agent_id),
            agent_id.clone(),
            runtime_db.clone(),
        )?;
        match holon::runtime::scheduler_recovery_report(&storage, runtime_db, agent_id) {
            Ok(report) => reports.push(Some(report)),
            Err(error) => {
                report_errors.push(serde_json::json!({
                    "agent_id": agent_id,
                    "error": format!("{error:#}"),
                }));
                reports.push(None);
            }
        }
        storages.push(storage);
    }

    let mut changed = 0;
    let mut backup_path = None;
    let mut fallback_results = Vec::new();
    if apply && !before.is_fixed_point() {
        if !no_backup {
            backup_path = Some(runtime_db.create_scheduler_recovery_backup()?);
        }
        for ((storage, report), agent_id) in storages.iter().zip(&reports).zip(&agent_ids) {
            let Some(report) = report else {
                continue;
            };
            changed += holon::runtime::apply_scheduler_recovery_plan_with_backup_policy(
                storage,
                runtime_db,
                agent_id,
                report,
                holon::runtime::SchedulerRecoveryBackupPolicy::SkipApproved,
            )?
            .0;
        }
        let after_exact = runtime_db.retired_scheduler_cleanup_inventory()?;
        for result in
            runtime_db.apply_retired_scheduler_cleanup_fallbacks(&after_exact.affected_agents())?
        {
            changed += result.actions.len();
            fallback_results.push(result);
        }
    }

    let after = runtime_db.retired_scheduler_cleanup_inventory()?;
    if json {
        print_json(&serde_json::json!({
            "before": before,
            "reports": reports,
            "report_errors": report_errors,
            "apply": apply.then(|| serde_json::json!({
                "changed": changed,
                "backup_path": backup_path,
                "backup_policy": if no_backup { "skip_approved" } else { "required" },
                "backup_created": backup_path.is_some(),
                "fallback_results": fallback_results,
            })),
            "after": after,
        }))?;
    } else {
        println!(
            "Scheduler recovery affected agents={} blockers={}",
            agent_ids.join(","),
            before.blockers.len()
        );
        for (agent_id, report) in agent_ids.iter().zip(&reports) {
            if let Some(report) = report {
                println!(
                    "- {}: candidates={} task_result_claims={} continuations={}",
                    report.agent_id,
                    report.candidates.len(),
                    report.task_result_claim_recoveries.len(),
                    report.continuation_reconciliations.len(),
                );
            } else {
                println!(
                    "- {agent_id}: exact recovery report unavailable; typed fallback required"
                );
            }
        }
        if apply {
            println!(
                "- applied recovery changes={} fallbacks={} backup={}",
                changed,
                fallback_results.len(),
                backup_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "not-created".into())
            );
        }
        println!("- remaining blockers={}", after.blockers.len());
    }
    if apply && !after.is_fixed_point() {
        return Err(anyhow!(
            "scheduler recovery did not reach the retired scheduler cleanup fixed point: \
             remaining_blockers={} affected_agents={}",
            after.blockers.len(),
            after.affected_agents().join(",")
        ));
    }
    Ok(())
}

fn filter_scheduler_recovery_report(
    report: &mut holon::runtime::SchedulerRecoveryReport,
    message_id: Option<&str>,
    require_match: bool,
) -> Result<()> {
    let Some(message_id) = message_id else {
        return Ok(());
    };
    let matching = report
        .task_result_claim_recoveries
        .iter()
        .filter(|candidate| candidate.message_id == message_id)
        .count();
    if matching > 1 || (require_match && matching != 1) {
        return Err(anyhow!(
            "scheduler recovery selector `{message_id}` matched {matching} TaskResult claim candidates; expected exactly one"
        ));
    }
    report.candidates.clear();
    report.continuation_reconciliations.clear();
    report
        .task_result_claim_recoveries
        .retain(|candidate| candidate.message_id == message_id);
    Ok(())
}

async fn handle_models_dev_command(command: ModelsDevCommands) -> Result<()> {
    use holon::model_catalog::models_dev::{
        audit_mappings, generate_artifact, parse_snapshot, MappingAuditReport, ProviderMapping,
    };
    use sha2::Digest;

    match command {
        ModelsDevCommands::Refresh { url, output_dir } => {
            println!("Fetching models.dev snapshot from {url} …");
            let response = reqwest::get(&url)
                .await
                .map_err(|e| anyhow!("failed to fetch models.dev snapshot: {e}"))?;
            if !response.status().is_success() {
                anyhow::bail!("models.dev fetch returned status {}", response.status());
            }
            let raw_json = response
                .text()
                .await
                .map_err(|e| anyhow!("failed to read models.dev response: {e}"))?;
            let fetched_at = chrono::Utc::now().to_rfc3339();
            let sha = sha2::Sha256::digest(raw_json.as_bytes());
            let upstream_revision = sha.iter().map(|b| format!("{b:02x}")).collect::<String>();

            std::fs::create_dir_all(&output_dir)
                .map_err(|e| anyhow!("failed to create output dir: {e}"))?;
            let snapshot_path = output_dir.join("snapshot.json");
            std::fs::write(&snapshot_path, &raw_json)
                .map_err(|e| anyhow!("failed to write snapshot: {e}"))?;

            let generated = generate_artifact(
                &raw_json,
                &upstream_revision,
                &fetched_at,
                env!("HOLON_VERSION"),
            )
            .map_err(|e| anyhow!("artifact generation failed: {e}"))?;

            let artifact_path = output_dir.join("artifact.json");
            let artifact_json = generated
                .artifact
                .to_json()
                .map_err(|e| anyhow!("failed to serialize artifact: {e}"))?;
            std::fs::write(&artifact_path, format!("{artifact_json}\n"))
                .map_err(|e| anyhow!("failed to write artifact: {e}"))?;

            let parsed_snapshot =
                parse_snapshot(&raw_json).map_err(|e| anyhow!("parse error: {e}"))?;
            let report = audit_mappings(&parsed_snapshot, &ProviderMapping::default_mappings());

            // Draft the supplemental catalog for allowlisted providers.
            let supplement_path = output_dir.join("supplemental_catalog.json");
            let previous_supplement = match std::fs::read_to_string(&supplement_path) {
                Ok(raw) => Some(
                    holon::model_catalog::models_dev::ModelsDevSupplement::parse(&raw)
                        .map_err(|e| anyhow!("previous supplement invalid: {e}"))?,
                ),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => {
                    return Err(anyhow!("failed to read previous supplement: {err}"));
                }
            };
            let legacy = holon::model_catalog::legacy_builtin_catalog()
                .map_err(|e| anyhow!("built-in catalog invalid: {e}"))?;
            let cutoff = (chrono::Utc::now().date_naive()
                - chrono::Duration::days(holon::model_catalog::models_dev::RECENCY_WINDOW_DAYS))
            .format("%Y-%m-%d")
            .to_string();
            let update = holon::model_catalog::models_dev::supplement::generate(
                &parsed_snapshot,
                previous_supplement.as_ref(),
                &legacy,
                &cutoff,
                &upstream_revision,
                env!("HOLON_VERSION"),
            )
            .map_err(|e| anyhow!("supplement generation failed: {e}"))?;
            let supplement_json = update
                .supplement
                .to_json()
                .map_err(|e| anyhow!("failed to serialize supplement: {e}"))?;
            std::fs::write(&supplement_path, format!("{supplement_json}\n"))
                .map_err(|e| anyhow!("failed to write supplement: {e}"))?;
            let summary = holon::model_catalog::models_dev::render_summary_markdown(&update);
            let summary_path = output_dir.join("refresh-summary.md");
            std::fs::write(&summary_path, &summary)
                .map_err(|e| anyhow!("failed to write refresh summary: {e}"))?;

            println!("Snapshot  → {}", snapshot_path.display());
            println!(
                "Artifact  → {} ({} models)",
                artifact_path.display(),
                generated.artifact.model_count()
            );
            println!("Mapped    → {} providers", report.mapped.len());
            println!(
                "Unmapped  → {} upstream providers",
                report.unmapped_upstream.len()
            );
            println!("Stale     → {} Holon mappings", report.stale_holon.len());
            println!(
                "Supplement→ {} ({} models: {} drafted, {} retained, {} removed, {} deferred)",
                supplement_path.display(),
                update.supplement.models.len(),
                update.drafted.len(),
                update.retained.len(),
                update.removed.len(),
                update.deferred.len()
            );
            println!("Summary   → {}", summary_path.display());
            if !report.stale_holon.is_empty() {
                eprintln!("WARNING: stale mappings (models.dev ID not in snapshot):");
                for entry in &report.stale_holon {
                    eprintln!("  {} → {}", entry.models_dev_id, entry.holon_provider_id);
                }
            }
            Ok(())
        }
        ModelsDevCommands::Validate { input_dir } => {
            let snapshot_path = input_dir.join("snapshot.json");
            let artifact_path = input_dir.join("artifact.json");
            let raw_json = std::fs::read_to_string(&snapshot_path)
                .map_err(|e| anyhow!("failed to read snapshot: {e}"))?;
            let artifact_json = std::fs::read_to_string(&artifact_path)
                .map_err(|e| anyhow!("failed to read artifact: {e}"))?;
            let artifact: holon::model_catalog::models_dev::ModelsDevArtifact =
                serde_json::from_str(&artifact_json)
                    .map_err(|e| anyhow!("failed to parse artifact JSON: {e}"))?;
            artifact
                .validate()
                .map_err(|e| anyhow!("artifact validation failed: {e}"))?;
            let snapshot =
                parse_snapshot(&raw_json).map_err(|e| anyhow!("failed to parse snapshot: {e}"))?;
            let report = audit_mappings(&snapshot, &ProviderMapping::default_mappings());
            println!("Artifact valid ({} models)", artifact.model_count());
            println!(
                "Audit: mapped={}, unmapped={}, stale={}",
                report.mapped.len(),
                report.unmapped_upstream.len(),
                report.stale_holon.len()
            );
            if !report.stale_holon.is_empty() {
                eprintln!(
                    "WARNING: {} stale mapping(s) need attention",
                    report.stale_holon.len()
                );
            }
            let supplement_path = input_dir.join("supplemental_catalog.json");
            if supplement_path.exists() {
                let raw = std::fs::read_to_string(&supplement_path)
                    .map_err(|e| anyhow!("failed to read supplement: {e}"))?;
                let supplement = holon::model_catalog::models_dev::ModelsDevSupplement::parse(&raw)
                    .map_err(|e| anyhow!("supplement validation failed: {e}"))?;
                let legacy = holon::model_catalog::legacy_builtin_catalog()
                    .map_err(|e| anyhow!("built-in catalog invalid: {e}"))?;
                let mut catalog = legacy.clone();
                supplement
                    .apply_to(&mut catalog)
                    .map_err(|e| anyhow!("supplement validation failed: {e}"))?;
                println!("Supplement valid ({} models)", supplement.models.len());
            }
            Ok(())
        }
        ModelsDevCommands::Audit { snapshot, json } => {
            let raw_json = std::fs::read_to_string(&snapshot)
                .map_err(|e| anyhow!("failed to read snapshot: {e}"))?;
            let parsed =
                parse_snapshot(&raw_json).map_err(|e| anyhow!("failed to parse snapshot: {e}"))?;
            let report: MappingAuditReport =
                audit_mappings(&parsed, &ProviderMapping::default_mappings());
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|e| anyhow!("failed to serialize audit: {e}"))?
                );
            } else {
                println!("Provider mapping audit");
                println!("Mapped ({}):", report.mapped.len());
                for entry in &report.mapped {
                    println!("  {} → {}", entry.models_dev_id, entry.holon_provider_id);
                }
                println!("Unmapped upstream ({}):", report.unmapped_upstream.len());
                for id in &report.unmapped_upstream {
                    println!("  {id}");
                }
                println!("Stale Holon mappings ({}):", report.stale_holon.len());
                for entry in &report.stale_holon {
                    println!("  {} → {}", entry.models_dev_id, entry.holon_provider_id);
                }
            }
            Ok(())
        }
    }
}

fn handle_runtime_db_debug_command(
    config: &AppConfig,
    command: holon::cli::RuntimeDbDebugCommands,
) -> Result<()> {
    match command {
        holon::cli::RuntimeDbDebugCommands::AgentRelations {
            apply,
            no_backup,
            diagnostic_sample_limit,
            json,
        } => {
            let _maintenance_lock = apply
                .then(|| {
                    RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path())
                        .context("runtime database maintenance is already running")
                })
                .transpose()?;
            let inspection_db = RuntimeDb::open_for_agent_relation_backfill(
                config.runtime_db_path(),
                config.runtime_db_lock_path(),
            )?;
            let backup_path = if apply && !no_backup {
                Some(inspection_db.create_agent_relation_backfill_backup()?)
            } else {
                None
            };
            let db = if apply {
                drop(inspection_db);
                RuntimeDb::open_and_migrate(
                    config.runtime_db_path(),
                    config.runtime_db_lock_path(),
                )?
            } else {
                inspection_db
            };
            let report = db.agent_canonical_relations().backfill_with_backup(
                apply,
                diagnostic_sample_limit,
                backup_path.map(|path| path.display().to_string()),
            )?;
            if json {
                print_json(&serde_json::to_value(report)?)
            } else {
                println!(
                    "agent relation backfill: mode={} scanned={} changed={} unchanged={} diagnostics={} migrated_axes={}",
                    if report.apply { "apply" } else { "report" },
                    report.scanned_agents,
                    report.changed_agents,
                    report.unchanged_agents,
                    report.diagnostic_agents,
                    report.migrated_axes
                );
                if let Some(backup_path) = &report.backup_path {
                    println!("  backup: {backup_path}");
                }
                for diagnostic in &report.diagnostics {
                    println!(
                        "  agent={} migrated_axes={:?}",
                        diagnostic.agent_id, diagnostic.migrated_axes
                    );
                    for issue in &diagnostic.issues {
                        println!(
                            "    axis={:?} resolution={:?} detail={}",
                            issue.axis, issue.resolution, issue.detail
                        );
                    }
                }
                Ok(())
            }
        }
        holon::cli::RuntimeDbDebugCommands::Audit {
            check,
            baseline_through,
            sample_limit,
            json,
        } => {
            let check = match check {
                holon::cli::RuntimeDbAuditCheckArg::ProjectionEffects => {
                    RuntimeDbAuditCheck::ProjectionEffects
                }
                holon::cli::RuntimeDbAuditCheckArg::BriefIntegrity => {
                    RuntimeDbAuditCheck::BriefIntegrity
                }
                holon::cli::RuntimeDbAuditCheckArg::All => RuntimeDbAuditCheck::All,
            };
            let baseline_through = baseline_through
                .map(|value| {
                    chrono::DateTime::parse_from_rfc3339(&value)
                        .with_context(|| {
                            format!("invalid --baseline-through RFC3339 timestamp `{value}`")
                        })
                        .map(|value| value.with_timezone(&chrono::Utc))
                })
                .transpose()?;
            let db = RuntimeDb::open_and_migrate(
                config.runtime_db_path(),
                config.runtime_db_lock_path(),
            )?;
            let report = db.audit(RuntimeDbAuditOptions {
                check,
                baseline_through,
                sample_limit,
            })?;
            if json {
                print_json(&serde_json::to_value(&report)?)?;
            } else {
                print_runtime_db_audit_human(&report);
            }
            if report.has_new_violations() {
                return Err(anyhow!("runtime database audit found new violations"));
            }
            Ok(())
        }
        holon::cli::RuntimeDbDebugCommands::Retention { dry_run, json } => {
            let policy = config.runtime_db_retention_policy()?;
            let db = RuntimeDb::open_and_migrate(
                config.runtime_db_path(),
                config.runtime_db_lock_path(),
            )?;
            let report = if dry_run {
                db.plan_retention(policy, chrono::Utc::now())?
            } else {
                let _maintenance_lock =
                    RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path())
                        .context("runtime database maintenance is already running")?;
                db.run_retention_pass(policy, chrono::Utc::now())?
            };
            if json {
                print_json(&serde_json::to_value(report)?)
            } else {
                println!("{}", serde_json::to_string_pretty(&report)?);
                Ok(())
            }
        }
        holon::cli::RuntimeDbDebugCommands::Compact { json } => {
            let report = RuntimeDb::compact_offline(
                &config.runtime_db_path(),
                &config.runtime_db_lock_path(),
                &config.runtime_db_maintenance_lock_path(),
            )?;
            if json {
                print_json(&serde_json::to_value(report)?)
            } else {
                println!("{}", serde_json::to_string_pretty(&report)?);
                Ok(())
            }
        }
        holon::cli::RuntimeDbDebugCommands::ConversationInputAssignmentRollback { apply, json } => {
            let _maintenance_lock = RuntimeDbLock::try_lock(
                config.runtime_db_maintenance_lock_path(),
            )
            .context("conversation input assignment rollback requires holon serve to be stopped")?;
            let _migration_lock = RuntimeDbLock::try_lock(config.runtime_db_lock_path()).context(
                "conversation input assignment rollback cannot run during a database migration",
            )?;
            let db = RuntimeDb::open_for_conversation_input_assignment_rollback(
                config.runtime_db_path(),
                config.runtime_db_lock_path(),
            )?;
            let report = if apply {
                db.apply_conversation_input_assignment_rollback()?
            } else {
                db.plan_conversation_input_assignment_rollback()?
            };
            if json {
                print_json(&serde_json::to_value(report)?)
            } else {
                println!(
                    "conversation input assignment rollback: mode={} eligible={} changed={} backup_verified={} head=v{} ({}) target=v{} ({})",
                    if report.apply { "apply" } else { "dry-run" },
                    report.eligible,
                    report.changed,
                    report.backup_verified,
                    report.current_version,
                    report.current_name.as_deref().unwrap_or("<none>"),
                    report.target_version,
                    report.target_name
                );
                if let Some(backup_path) = &report.backup_path {
                    println!("  backup: {}", backup_path.display());
                }
                if let Some(reason) = &report.reason {
                    println!("  reason: {reason}");
                }
                Ok(())
            }
        }
        holon::cli::RuntimeDbDebugCommands::WaitFinalBriefPublication {
            apply,
            no_backup,
            plan,
            resume,
            agent,
            diagnostic_sample_limit,
            json,
        } => {
            let db = RuntimeDb::open_and_migrate(
                config.runtime_db_path(),
                config.runtime_db_lock_path(),
            )?;
            let mut progress =
                |progress: &holon::runtime_db::WaitFinalBriefPublicationRepairProgress| {
                    eprintln!("WaitFor final Brief publication repair: {progress}");
                };
            let report = if apply {
                let plan = plan
                    .as_deref()
                    .context("--apply requires a completed repair plan")?;
                db.preflight_wait_final_brief_publication_repair_plan(plan)?;
                let _maintenance_lock =
                    RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path()).context(
                        "WaitFor final Brief publication repair requires holon serve to be stopped",
                    )?;
                let backup_path = if no_backup {
                    None
                } else {
                    Some(db.create_wait_final_brief_publication_repair_backup()?)
                };
                db.apply_wait_final_brief_publication_repair_plan(
                    plan,
                    diagnostic_sample_limit,
                    backup_path,
                    &mut progress,
                )?
            } else {
                db.prepare_wait_final_brief_publication_repair(
                    plan.as_deref(),
                    resume,
                    agent.as_deref(),
                    diagnostic_sample_limit,
                    &mut progress,
                )?
            };
            if json {
                print_json(&serde_json::to_value(report)?)
            } else {
                println!(
                    "WaitFor final Brief publication repair: mode={} scanned={} repairable={} repaired={} skipped={}",
                    if report.apply {
                        "apply"
                    } else if report.plan_path.is_some() {
                        "prepare"
                    } else {
                        "dry-run"
                    },
                    report.scanned_briefs,
                    report.repairable_briefs,
                    report.repaired_briefs,
                    report.skipped_briefs
                );
                if let Some(plan_path) = &report.plan_path {
                    println!("  plan: {}", plan_path.display());
                }
                if let Some(backup_path) = &report.backup_path {
                    println!("  backup: {}", backup_path.display());
                }
                for diagnostic in &report.diagnostics {
                    println!(
                        "  {} brief={} agent={} turn={} reason={}",
                        diagnostic.status,
                        diagnostic.brief_id,
                        diagnostic.agent_id,
                        diagnostic.turn_id.as_deref().unwrap_or("<none>"),
                        diagnostic.reason
                    );
                }
                Ok(())
            }
        }
        holon::cli::RuntimeDbDebugCommands::TurnSettlement {
            apply,
            no_backup,
            plan,
            agent,
            turn,
            diagnostic_sample_limit,
            json,
        } => {
            let mut progress = |progress: &holon::runtime_db::TurnSettlementRepairProgress| {
                eprintln!("Turn settlement reconciliation: {progress}");
            };
            let report = if apply {
                let plan = plan
                    .as_deref()
                    .context("--apply requires a completed repair plan")?;
                let _maintenance_lock =
                    RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path()).context(
                        "Turn settlement reconciliation apply requires holon serve to be stopped",
                    )?;
                let _migration_lock = RuntimeDbLock::try_lock(config.runtime_db_lock_path())
                    .context(
                    "Turn settlement reconciliation apply cannot run during a database migration",
                )?;
                RuntimeDb::preflight_turn_settlement_repair_plan_read_only(
                    &config.runtime_db_path(),
                    plan,
                )?;
                let db = RuntimeDb::open_for_turn_settlement_repair(
                    config.runtime_db_path(),
                    config.runtime_db_lock_path(),
                )?;
                let backup_path = if no_backup {
                    None
                } else {
                    Some(db.create_turn_settlement_repair_backup()?)
                };
                db.apply_turn_settlement_repair_plan(
                    plan,
                    backup_path,
                    diagnostic_sample_limit,
                    &mut progress,
                )?
            } else {
                RuntimeDb::prepare_turn_settlement_repair_read_only(
                    &config.runtime_db_path(),
                    plan.as_deref(),
                    agent.as_deref(),
                    turn.as_deref(),
                    diagnostic_sample_limit,
                    &mut progress,
                )?
            };
            if json {
                print_json(&serde_json::to_value(report)?)
            } else {
                println!(
                    "Turn settlement reconciliation: mode={} scanned={} repairable={} repaired={} already_settled={} skipped={}",
                    if report.apply {
                        "apply"
                    } else if report.plan_path.is_some() {
                        "prepare"
                    } else {
                        "dry-run"
                    },
                    report.scanned_turns,
                    report.repairable_turns,
                    report.repaired_turns,
                    report.already_settled_turns,
                    report.skipped_turns
                );
                if let Some(plan_path) = &report.plan_path {
                    println!("  plan: {}", plan_path.display());
                }
                if let Some(backup_path) = &report.backup_path {
                    println!("  backup: {}", backup_path.display());
                }
                for diagnostic in &report.diagnostics {
                    println!(
                        "  {} agent={} turn={} reason={}",
                        diagnostic.status,
                        diagnostic.agent_id,
                        diagnostic.turn_id,
                        diagnostic.reason
                    );
                }
                Ok(())
            }
        }
    }
}

fn print_runtime_db_audit_human(report: &RuntimeDbAuditReport) {
    println!("runtime database audit");
    println!("  path: {}", report.database.path);
    println!("  schema version: {}", report.database.schema_version);
    println!("  runtime id: {}", report.database.runtime_id);
    println!("  event-log epoch: {}", report.database.event_log_epoch);
    println!(
        "  baseline through: {}",
        report
            .baseline
            .baseline_through
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "<none; findings count as new violations>".to_string())
    );
    if let Some(audit) = &report.projection_effects {
        println!("projection effects");
        println!(
            "  unsupported: historical_baseline={} new_violation={}",
            audit.totals.historical_baseline, audit.totals.new_violation
        );
        for group in &audit.groups {
            println!(
                "  agent={} kind={} contract={} schema={}@{} classification={} effect={} reason={} count={} historical_baseline={} new_violation={} samples={}",
                group.agent_id,
                group.kind,
                group.contract_version,
                if group.payload_schema.is_empty() {
                    "<legacy>"
                } else {
                    &group.payload_schema
                },
                group.payload_schema_version,
                group.classification,
                group.projection_effect.unwrap_or("-"),
                group.unsupported_reason.unwrap_or("-"),
                group.count,
                group.historical_baseline,
                group.new_violation,
                group.sample_event_ids.join(","),
            );
        }
    }
    if let Some(audit) = &report.brief_integrity {
        println!("brief integrity");
        for agent in &audit.agents {
            println!(
                "  agent={} retained_event_floor={} event_head={} operator_messages={} terminal_turns={} deliveries={} briefs={} brief_created_events={} valid_linked_briefs={}",
                agent.agent_id,
                agent
                    .retained_event_floor
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                agent
                    .event_head
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                agent.counts.operator_messages,
                agent.counts.terminal_turns,
                agent.counts.operator_visible_assistant_deliveries,
                agent.counts.canonical_briefs,
                agent.counts.brief_created_events,
                agent.counts.valid_linked_briefs,
            );
            for category in &agent.categories {
                println!(
                    "    category={} observable_offline={} historical_baseline={} new_violation={} samples={} description={}",
                    category.category,
                    category.observable_offline,
                    category.historical_baseline,
                    category.new_violation,
                    category.sample_ids.join(","),
                    category.description,
                );
            }
        }
    }
}

fn export_scheduler_fixture(
    config: &AppConfig,
    agent: Option<String>,
    output: &Path,
) -> Result<()> {
    let agent_id = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let host = RuntimeHost::new(config.clone())?;
    let agent_home = config.data_dir.join("agents").join(&agent_id);
    let storage = host.agent_storage(&agent_id)?;
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
            "current_work_item_id": agent.current_work_item_id.clone(),
            "pending_wake_hint_reason": pending_wake_hint_reason,
            "turn_index": agent.turn_index,
            "last_turn_terminal_kind": last_turn_terminal_kind,
        }),
    )?;

    let work_queue = storage.work_queue_prompt_projection()?;
    let active_tasks = storage.latest_active_task_records_for_agent(&agent.id, usize::MAX)?;
    let has_blocking_active_tasks = active_tasks.iter().any(|task| task.is_blocking());
    let active_timers = storage
        .latest_timer_records()?
        .into_iter()
        .filter(|timer| timer.agent_id == agent.id && timer.status == TimerStatus::Active)
        .collect::<Vec<_>>();
    let replay_message_ids = storage
        .recovery_snapshot(&agent.id)?
        .replay_messages
        .into_iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    write_json_pretty(
        &output.join("expected.json"),
        &serde_json::json!({
            "current_work_item_id": work_queue.current.as_ref().map(|item| item.id.clone()),
            "current_work_item_revision": work_queue.current.as_ref().map(|item| item.revision),
            "queued_work_items": work_queue.queued_runnable.len(),
            "yielded_work_items": work_queue.yielded.len(),
            "triggered_blocked_work_items": work_queue.triggered_blocked.len(),
            "waiting_for_operator_work_items": work_queue.waiting_for_operator.len(),
            "blocked_work_items": work_queue.blocked.len(),
            "completed_recent_work_items": work_queue.completed_recent.len(),
            "active_tasks": active_tasks.len(),
            "has_blocking_active_tasks": has_blocking_active_tasks,
            "pending_wake_hint": agent.pending_wake_hint.is_some(),
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
    let runtime_db =
        RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
    let storage = AppStorage::new_for_agent(&agent_home, agent.clone(), runtime_db)?;
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

async fn print_performance_diagnostics(config: &AppConfig, json: bool) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    let snapshot = client.performance_diagnostics().await?;
    if json {
        return print_json(&serde_json::to_value(snapshot)?);
    }

    println!(
        "process uptime {}",
        format_duration_ms(snapshot.process_uptime_ms)
    );
    print_metric_group("http", &snapshot.http);
    print_metric_group("projections", &snapshot.projections);
    print_metric_group("db", &snapshot.db);
    print_metric_group("scheduler", &snapshot.scheduler);
    Ok(())
}

async fn print_trace_diagnostics(
    config: &AppConfig,
    trace_id: Option<String>,
    search: Option<String>,
    json: bool,
) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    if let Some(query) = search {
        let traces = client.search_recent_traces(&query).await?;
        if json {
            return print_json(&serde_json::to_value(traces)?);
        }
        for trace in traces {
            println!(
                "{}  {} us  {} spans  {} errors",
                trace.trace_id, trace.duration_us, trace.span_count, trace.error_count
            );
        }
        return Ok(());
    }
    if let Some(trace_id) = trace_id {
        let trace = client.recent_trace(&trace_id).await?;
        if json {
            return print_json(&serde_json::to_value(trace)?);
        }
        println!(
            "{}  {} us  {} spans  {} errors",
            trace.trace_id, trace.duration_us, trace.span_count, trace.error_count
        );
        for span in trace.spans {
            let offset_us = span
                .started_at
                .signed_duration_since(trace.started_at)
                .num_microseconds()
                .unwrap_or_default()
                .max(0);
            println!(
                "{offset_us:>10} us  {:>10} us  {:<30} {:?}",
                span.duration_us, span.name, span.status
            );
        }
        return Ok(());
    }
    let traces = client.recent_traces().await?;
    if json {
        return print_json(&serde_json::to_value(traces)?);
    }
    for trace in traces {
        println!(
            "{}  {} us  {} spans  {} errors",
            trace.trace_id, trace.duration_us, trace.span_count, trace.error_count
        );
    }
    Ok(())
}

fn print_metric_group(group: &str, metrics: &[holon::diagnostics::MetricSnapshot]) {
    println!("{group}:");
    for metric in metrics {
        if metric.count == 0 {
            continue;
        }
        let bytes = metric
            .avg_bytes
            .map(|avg| format!(" avg_bytes={avg:.0}"))
            .unwrap_or_default();
        println!(
            "  {:<36} count={} avg_ms={:.1} max_ms={}{}",
            metric.name, metric.count, metric.avg_ms, metric.max_ms, bytes
        );
    }
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
        DaemonCommands::Start {
            options,
            start_timeout,
        } => serde_json::to_value(
            daemon_launch_with_profile(
                &config,
                false,
                options.reset_serve_options,
                start_timeout,
                |profile, metadata| {
                    resolve_daemon_serve_plan(config.clone(), options, profile, metadata)
                },
                resolve_recorded_daemon_serve_plan,
            )
            .await?,
        )?,
        DaemonCommands::Stop => serde_json::to_value(daemon_stop(&config).await?)?,
        DaemonCommands::PrepareUpdate => {
            serde_json::to_value(daemon_prepare_update(&config).await?)?
        }
        DaemonCommands::Status => serde_json::to_value(daemon_status(&config).await?)?,
        DaemonCommands::Restart {
            options,
            start_timeout,
        } => serde_json::to_value(
            daemon_launch_with_profile(
                &config,
                true,
                options.reset_serve_options,
                start_timeout,
                |profile, metadata| {
                    resolve_daemon_serve_plan(config.clone(), options, profile, metadata)
                },
                resolve_recorded_daemon_serve_plan,
            )
            .await?,
        )?,
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

async fn handle_task_command(config: &AppConfig, command: TaskCommands) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    match command {
        TaskCommands::List { limit, agent } => {
            let agent = cli_target_agent(config, agent)?;
            print_json(&serde_json::to_value(
                client.agent_tasks(&agent, limit).await?,
            )?)
        }
        TaskCommands::Run {
            summary,
            cmd,
            workdir,
            shell,
            login,
            tty,
            yield_time_ms,
            max_output_tokens,
            agent,
        } => {
            let agent = cli_target_agent(config, agent)?;
            post_control_json(
                config,
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
                    authority_class: Some(cli_authority_class()?),
                    invocation_context: cli_invocation_context()?,
                },
            )
            .await
        }
        TaskCommands::Status { task_id, agent } => {
            let agent = cli_target_agent(config, agent)?;
            print_json(&serde_json::to_value(
                client.task_status(&agent, &task_id).await?,
            )?)
        }
        TaskCommands::Input {
            task_id,
            text,
            agent,
        } => {
            let agent = cli_target_agent(config, agent)?;
            post_control_json(
                config,
                &format!("/control/agents/{agent}/tasks/{task_id}/input"),
                &TaskInputRequest {
                    text,
                    authority_class: Some(cli_authority_class()?),
                    invocation_context: cli_invocation_context()?,
                },
            )
            .await
        }
        TaskCommands::Stop { task_id, agent } => {
            let agent = cli_target_agent(config, agent)?;
            post_control_json(
                config,
                &format!("/control/agents/{agent}/tasks/{task_id}/stop"),
                &TaskStopRequest {
                    authority_class: Some(cli_authority_class()?),
                    invocation_context: cli_invocation_context()?,
                },
            )
            .await
        }
    }
}

async fn handle_timer_command(config: &AppConfig, command: TimerCommands) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    match command {
        TimerCommands::Create { options } => {
            let agent = options
                .agent
                .unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client
                    .create_timer(&agent, options.after_ms, options.every_ms, options.summary)
                    .await?,
            )?)
        }
        TimerCommands::List { limit, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(client.timers(&agent, limit).await?)?)
        }
        TimerCommands::Cancel { timer_id, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client.cancel_timer(&agent, &timer_id).await?,
            )?)
        }
    }
}

async fn handle_work_item_command(config: &AppConfig, command: WorkItemCommands) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    match command {
        WorkItemCommands::List { limit, agent } => {
            let agent = cli_target_agent(config, agent)?;
            print_json(&serde_json::to_value(
                client.agent_work_items(&agent, limit).await?,
            )?)
        }
        WorkItemCommands::Get {
            work_item_id,
            agent,
        } => {
            let agent = cli_target_agent(config, agent)?;
            print_json(&serde_json::to_value(
                client.work_item(&agent, &work_item_id).await?,
            )?)
        }
        WorkItemCommands::Create { objective, agent } => {
            let agent = cli_target_agent(config, agent)?;
            post_control_json(
                config,
                &format!("/control/agents/{agent}/work-items"),
                &CreateWorkItemRequest {
                    objective,
                    authority_class: Some(cli_authority_class()?),
                    invocation_context: cli_invocation_context()?,
                },
            )
            .await
        }
        WorkItemCommands::Pick {
            work_item_id,
            reason,
            clear_blocker,
            agent,
        } => {
            let agent = cli_target_agent(config, agent)?;
            post_control_json(
                config,
                &format!("/control/agents/{agent}/work-items/{work_item_id}/pick"),
                &PickWorkItemRequest {
                    reason,
                    clear_blocker,
                    authority_class: Some(cli_authority_class()?),
                    invocation_context: cli_invocation_context()?,
                },
            )
            .await
        }
        WorkItemCommands::Update {
            work_item_id,
            objective,
            plan_status,
            todo_json,
            blocked_by_json,
            recheck_after,
            agent,
        } => {
            let agent = cli_target_agent(config, agent)?;
            let plan_status = plan_status
                .map(|value| parse_work_item_plan_status(&value))
                .transpose()?;
            let todo_list = todo_json
                .map(|value| serde_json::from_str::<Vec<TodoItem>>(&value))
                .transpose()
                .context("--todo-json must contain a JSON array of todo items")?;
            let blocked_by = blocked_by_json
                .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                .transpose()
                .context("--blocked-by-json must contain valid JSON")?;
            post_control_json(
                config,
                &format!("/control/agents/{agent}/work-items/{work_item_id}"),
                &UpdateWorkItemRequest {
                    objective,
                    plan_status,
                    todo_list,
                    blocked_by,
                    recheck_after,
                    authority_class: Some(cli_authority_class()?),
                    invocation_context: cli_invocation_context()?,
                },
            )
            .await
        }
        WorkItemCommands::Complete {
            work_item_id,
            report,
            report_file,
            agent,
        } => {
            let agent = cli_target_agent(config, agent)?;
            let report_text = read_report_input(report, report_file)?;
            post_control_json(
                config,
                &format!("/control/agents/{agent}/work-items/{work_item_id}/complete"),
                &CompleteWorkItemRequest {
                    report_text,
                    authority_class: Some(cli_authority_class()?),
                    invocation_context: cli_invocation_context()?,
                },
            )
            .await
        }
    }
}

fn cli_target_agent(config: &AppConfig, explicit: Option<String>) -> Result<String> {
    let context = cli_invocation_context()?;
    if let Some(agent) = explicit {
        return Ok(agent);
    }
    Ok(context
        .map(|context| context.caller_agent_id)
        .unwrap_or_else(|| config.default_agent_id.clone()))
}

fn cli_agent_list_parent(parent: String) -> Result<String> {
    let context = cli_invocation_context()?;
    if parent == "self" {
        return context
            .map(|context| context.caller_agent_id)
            .ok_or_else(|| anyhow!("agent_scope_denied: --parent self requires agent mode"));
    }
    if let Some(context) = context {
        if context.caller_agent_id != parent {
            return Err(anyhow!(
                "agent_scope_denied: agent mode may only query its own direct children; \
                 use --parent self"
            ));
        }
    }
    Ok(parent)
}

fn cli_invocation_context() -> Result<Option<AgentInvocationContext>> {
    AgentInvocationContext::from_env().map_err(|error| anyhow!(error))
}

fn cli_authority_class() -> Result<AuthorityClass> {
    authority_class_for_cli_context(cli_invocation_context()?)
}

fn authority_class_for_cli_context(
    context: Option<AgentInvocationContext>,
) -> Result<AuthorityClass> {
    match context {
        Some(context) => context.inherited_authority_class.ok_or_else(|| {
            anyhow!(
                "agent invocation context is missing {HOLON_CALLER_AUTHORITY_CLASS_ENV}; \
                 refusing to fall back to operator authority"
            )
        }),
        None => Ok(AuthorityClass::OperatorInstruction),
    }
}

fn parse_work_item_plan_status(value: &str) -> Result<WorkItemPlanStatus> {
    match value.trim().to_ascii_lowercase().as_str() {
        "draft" => Ok(WorkItemPlanStatus::Draft),
        "ready" => Ok(WorkItemPlanStatus::Ready),
        "needs_input" | "needs-input" => Ok(WorkItemPlanStatus::NeedsInput),
        _ => Err(anyhow!(
            "invalid --plan-status {value:?}; expected draft, ready, or needs_input"
        )),
    }
}

fn read_report_input(report: Option<String>, report_file: Option<PathBuf>) -> Result<String> {
    if let Some(report) = report {
        return Ok(report);
    }
    let Some(path) = report_file else {
        return Err(anyhow!(
            "one of --report or --report-file <path|-> is required"
        ));
    };
    if path == Path::new("-") {
        let mut report = String::new();
        std::io::stdin().read_to_string(&mut report)?;
        return Ok(report);
    }
    fs::read_to_string(&path)
        .with_context(|| format!("failed to read completion report {}", path.display()))
}

async fn handle_agent_command(config: &AppConfig, command: Option<AgentCommands>) -> Result<()> {
    match command {
        None | Some(AgentCommands::List { parent: None }) => {
            let client = LocalClient::new(config.clone())?;
            print_json(&serde_json::to_value(client.list_agent_entries().await?)?)
        }
        Some(AgentCommands::List {
            parent: Some(parent),
        }) => {
            let parent = cli_agent_list_parent(parent)?;
            let client = LocalClient::new(config.clone())?;
            print_json(&serde_json::to_value(
                client.list_agent_entries_for_parent(&parent).await?,
            )?)
        }
        Some(AgentCommands::Get { agent_id, detail }) => {
            let agent = cli_target_agent(config, agent_id)?;
            let client = LocalClient::new(config.clone())?;
            match detail {
                AgentDetailArg::Compact => print_json(&serde_json::to_value(
                    client.get_agent_compact(&agent).await?,
                )?),
                AgentDetailArg::Full => {
                    print_json(&serde_json::to_value(client.get_agent(&agent).await?)?)
                }
            }
        }
        Some(AgentCommands::Status { agent_id, detail }) => {
            let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config.clone())?;
            let status = match detail {
                AgentDetailArg::Compact => client
                    .agent_status_compact(&agent)
                    .await
                    .and_then(|summary| serde_json::to_value(summary).map_err(Into::into)),
                AgentDetailArg::Full => client
                    .agent_status(&agent)
                    .await
                    .and_then(|summary| serde_json::to_value(summary).map_err(Into::into)),
            };
            match status {
                Ok(summary) => print_json(&summary),
                Err(err) => {
                    // If the agent is being deleted, show deletion status as JSON instead.
                    if let Some(http_err) = err.downcast_ref::<LocalHttpError>() {
                        if http_err.has_code("agent_deleting") || http_err.has_code("agent_deleted")
                        {
                            let deletion = client.get_agent_deletion_status(&agent).await?;
                            return print_json(&serde_json::to_value(&deletion)?);
                        }
                    }
                    Err(err)
                }
            }
        }
        Some(AgentCommands::Delete {
            agent_id,
            yes,
            cascade_private_children,
            wait,
            timeout,
            json,
        }) => {
            handle_agent_delete(
                config,
                &agent_id,
                yes,
                cascade_private_children,
                wait,
                timeout,
                json,
            )
            .await
        }
        Some(AgentCommands::Create { agent_id, template }) => {
            post_control_json(
                config,
                &format!("/control/agents/{agent_id}/create"),
                &http::CreateAgentRequest {
                    authority_class: Some(AuthorityClass::OperatorInstruction),
                    name: None,
                    template,
                },
            )
            .await
        }
        Some(AgentCommands::Repair { agent_id }) => {
            let client = LocalClient::new(config.clone())?;
            print_json(&serde_json::to_value(
                client.repair_agent(&agent_id).await?,
            )?)
        }
        Some(AgentCommands::Rename { agent_id, name }) => {
            let client = LocalClient::new(config.clone())?;
            let detail = client.rename_agent(&agent_id, &name).await?;
            print_json(&serde_json::to_value(detail)?)
        }
        Some(AgentCommands::Start { agent_id }) => {
            control_agent_lifecycle(config, agent_id, ControlAction::Start).await
        }
        Some(AgentCommands::Stop { agent_id }) => {
            control_agent_lifecycle(config, agent_id, ControlAction::Stop).await
        }
        Some(AgentCommands::Abort { agent_id }) => {
            let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
            post_control_json(
                config,
                &format!("/control/agents/{agent}/current-run/abort"),
                &http::AbortCurrentRunRequest {
                    run_id: None,
                    mode: Some("stop_after_abort".into()),
                    authority_class: Some(AuthorityClass::OperatorInstruction),
                },
            )
            .await
        }
        Some(AgentCommands::Timezone { command }) => {
            if cli_invocation_context()?.is_some() {
                return Err(anyhow!(
                    "agent_scope_denied: agent timezone commands require operator mode"
                ));
            }
            let client = LocalClient::new(config.clone())?;
            match command {
                holon::cli::AgentTimezoneCommands::Get { agent_id } => {
                    let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
                    print_json(&client.agent_timezone(&agent).await?)
                }
                holon::cli::AgentTimezoneCommands::Set { timezone, agent_id } => {
                    let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
                    print_json(&client.set_agent_timezone(&agent, timezone).await?)
                }
                holon::cli::AgentTimezoneCommands::Clear { agent_id } => {
                    let agent = agent_id.unwrap_or_else(|| config.default_agent_id.clone());
                    print_json(&client.clear_agent_timezone(&agent).await?)
                }
            }
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
        Some(AgentCommands::ResetCallback { agent }) => {
            let agent_id = agent.unwrap_or_else(|| config.default_agent_id.clone());
            post_control_json(
                config,
                &format!("/control/agents/{agent_id}/reset-callback"),
                &serde_json::json!({}),
            )
            .await
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

async fn handle_agent_delete(
    config: &AppConfig,
    agent_id: &str,
    yes: bool,
    cascade_private_children: bool,
    wait: bool,
    timeout: Option<u64>,
    json: bool,
) -> Result<()> {
    let client = LocalClient::new(config.clone())?;

    // Confirmation
    if !yes {
        if std::io::stdin().is_terminal() {
            eprintln!();
            eprintln!("⚠  Permanently delete agent \"{agent_id}\"?");
            eprintln!("   This operation is irreversible.");
            eprintln!("   Agent home and all associated data will be removed.");
            eprint!("\n   Proceed? [y/N]: ");
            std::io::stderr().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if !input.trim().eq_ignore_ascii_case("y") {
                eprintln!("Cancelled.");
                return Ok(());
            }
        } else {
            anyhow::bail!(
                "interactive confirmation unavailable in non-interactive mode.\n\
                 Re-run with --yes (-y) to skip confirmation:\n  \
                 holon agent delete {agent_id} --yes"
            );
        }
    }

    // Initiate deletion
    let response = match client
        .delete_agent(agent_id, cascade_private_children)
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            if let Some(http_err) = err.downcast_ref::<LocalHttpError>() {
                if http_err.has_code("agent_deleting") {
                    eprintln!(
                        "Agent \"{agent_id}\" is already being deleted.\n\
                         Check progress: holon agent status {agent_id}"
                    );
                    return Ok(());
                }
                if http_err.has_code("agent_deleted") {
                    eprintln!("Agent \"{agent_id}\" was already deleted.");
                    return Ok(());
                }
            }
            return Err(err);
        }
    };

    if json {
        return print_json(&serde_json::to_value(&response)?);
    }

    eprintln!(
        "Deleting agent \"{agent_id}\" (phase: {})",
        format_deletion_phase(&response.job.phase)
    );

    if wait {
        let timeout_secs = timeout.unwrap_or(300);
        wait_for_deletion(&client, agent_id, timeout_secs).await?;
    }
    Ok(())
}

async fn wait_for_deletion(client: &LocalClient, agent_id: &str, timeout_secs: u64) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for deletion to complete ({timeout_secs}s)\n\
                 Agent \"{agent_id}\" is still in Deleting state.\n\
                 Check progress: holon agent status {agent_id}"
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let status = client.get_agent_deletion_status(agent_id).await?;
        if let Some(ref job) = status.job {
            match job.status {
                AgentDeletionStatus::Completed => {
                    eprintln!("Deleted agent \"{agent_id}\"");
                    return Ok(());
                }
                AgentDeletionStatus::RetryableFailed => {
                    eprintln!(
                        "Deletion failed (retryable): {}\n\
                         The deletion job will be retried.\n\
                         Check progress: holon agent status {agent_id}",
                        job.last_error.as_deref().unwrap_or("unknown error")
                    );
                    return Ok(());
                }
                AgentDeletionStatus::Pending | AgentDeletionStatus::Running => {}
            }
        }
        match status.identity.status {
            AgentRegistryStatus::Deleted => {
                eprintln!("Deleted agent \"{agent_id}\"");
                return Ok(());
            }
            AgentRegistryStatus::Active => {
                eprintln!("Agent \"{agent_id}\" is active (deletion may have been cancelled).");
                return Ok(());
            }
            AgentRegistryStatus::Deleting => {} // keep polling
        }
    }
}

fn format_deletion_phase(phase: &AgentDeletionPhase) -> &'static str {
    use AgentDeletionPhase::*;
    match phase {
        Fence => "Fence",
        Quiesce => "Quiesce",
        Ingress => "Ingress",
        Scheduler => "Scheduler",
        Workspace => "Workspace",
        Index => "Index",
        Home => "Home",
        Finalize => "Finalize",
    }
}

async fn handle_skills_command(config: &AppConfig, command: SkillsCommands) -> Result<()> {
    match command {
        SkillsCommands::List { agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let response: serde_json::Value =
                get_json(config, &format!("/agents/{agent}/skills")).await?;
            print_json(&response)
        }
        SkillsCommands::Catalog => {
            let response: serde_json::Value = get_json(config, "/skills/catalog").await?;
            print_json(&response)
        }
        SkillsCommands::Add {
            source,
            remote,
            skill,
            copy,
        } => {
            let kind = build_skill_add_kind(&source, remote, skill, copy)?;
            post_control_json(
                config,
                "/skills/catalog/add",
                &holon::types::AddSkillRequest { kind },
            )
            .await
        }
        SkillsCommands::Remove { name } => {
            post_control_json(
                config,
                "/skills/catalog/remove",
                &holon::types::RemoveSkillRequest { name },
            )
            .await
        }
        SkillsCommands::Reconcile { name } => {
            post_control_json(
                config,
                "/skills/catalog/reconcile",
                &holon::types::ReconcileSkillRequest { name },
            )
            .await
        }
        SkillsCommands::Refresh => {
            post_control_json(
                config,
                "/skills/catalog/refresh",
                &holon::types::RefreshCatalogRequest {},
            )
            .await
        }
        SkillsCommands::Update { name } => {
            let response = post_control_json_value(
                config,
                "/skills/catalog/update",
                &holon::types::UpdateSkillRequest { name },
            )
            .await?;
            let job_id = response["job"]["id"]
                .as_str()
                .ok_or_else(|| anyhow!("skill update response did not include a job id"))?;
            let job = wait_for_job(config, job_id).await?;
            if job["status"] == "failed" {
                return Err(anyhow!(
                    "{}",
                    job["error"]
                        .as_str()
                        .or_else(|| job["summary"].as_str())
                        .unwrap_or("skill update failed")
                ));
            }
            print_json(job.get("result").unwrap_or(&job))
        }
        SkillsCommands::Check { name } => {
            post_control_json(
                config,
                "/skills/catalog/check",
                &holon::types::CheckSkillRequest { name },
            )
            .await
        }
        SkillsCommands::Enable { name, copy, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let mode = if copy {
                holon::types::SkillInstallMode::Copied
            } else {
                holon::types::SkillInstallMode::Linked
            };
            post_control_json(
                config,
                &format!("/control/agents/{agent}/skills/enable"),
                &holon::types::EnableSkillRequest { name, mode },
            )
            .await
        }
        SkillsCommands::Disable { name, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            post_control_json(
                config,
                &format!("/control/agents/{agent}/skills/disable"),
                &holon::types::DisableSkillRequest { name },
            )
            .await
        }
        SkillsCommands::Install {
            name_or_path,
            remote,
            skill,
            copy,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let kind = if remote {
                let mode = if copy {
                    holon::types::SkillInstallMode::Copied
                } else {
                    holon::types::SkillInstallMode::Linked
                };
                holon::types::SkillInstallKind::Remote {
                    package: name_or_path,
                    skill,
                    mode,
                }
            } else {
                if skill.is_some() {
                    anyhow::bail!("--skill requires --remote");
                }
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

fn build_skill_add_kind(
    source: &str,
    remote: bool,
    skill: Option<String>,
    copy: bool,
) -> Result<holon::types::SkillInstallKind> {
    if remote {
        let mode = if copy {
            holon::types::SkillInstallMode::Copied
        } else {
            holon::types::SkillInstallMode::Linked
        };
        Ok(holon::types::SkillInstallKind::Remote {
            package: source.to_string(),
            skill,
            mode,
        })
    } else {
        if skill.is_some() {
            anyhow::bail!("--skill requires --remote");
        }
        build_skill_install_kind(source, copy)
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
    let client = LocalClient::new(config.clone())?;
    let value: serde_json::Value = client.post_control_json(path, payload).await?;
    print_json(&value)
}

async fn post_control_json_value<T: serde::Serialize>(
    config: &AppConfig,
    path: &str,
    payload: &T,
) -> Result<serde_json::Value> {
    let client = LocalClient::new(config.clone())?;
    client.post_control_json(path, payload).await
}

async fn wait_for_job(config: &AppConfig, job_id: &str) -> Result<serde_json::Value> {
    const JOB_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
    let deadline = tokio::time::Instant::now() + JOB_WAIT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let response = get_json(config, &format!("/jobs/{job_id}")).await?;
        let job = response
            .get("job")
            .ok_or_else(|| anyhow!("job response did not include a job"))?;
        if matches!(job["status"].as_str(), Some("completed") | Some("failed")) {
            return Ok(job.clone());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(anyhow!(
        "timed out after {} seconds waiting for job {job_id}",
        JOB_WAIT_TIMEOUT.as_secs()
    ))
}

async fn get_json(config: &AppConfig, path: &str) -> Result<serde_json::Value> {
    let client = LocalClient::new(config.clone())?;
    client.get_json(path).await
}

async fn handle_config_command(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Get { key } => config_get_command(key).await,
        ConfigCommands::Set { key, value } => config_set_command(key, value).await,
        ConfigCommands::Unset { key } => config_unset_command(key).await,
        ConfigCommands::Providers { command } => handle_config_providers_command(command).await,
        ConfigCommands::Credentials { command } => handle_config_credentials_command(command).await,
        ConfigCommands::Models { command } => handle_config_models_command(command).await,
        ConfigCommands::MigrateModelRoutes { write } => {
            config_migrate_model_routes_command(write).await
        }
        ConfigCommands::List => config_list_command().await,
        ConfigCommands::Schema => config_schema_command().await,
        ConfigCommands::Doctor => {
            let config = AppConfig::load_for_config_inspection()?;
            print_json(&provider_doctor(&config))
        }
    }
}

async fn config_migrate_model_routes_command(write: bool) -> Result<()> {
    if let Some((client, _response)) = local_runtime_config().await? {
        let report = client.migrate_model_config_routes(write).await?;
        return print_json(&serde_json::to_value(report)?);
    }

    let config = AppConfig::load_for_config_inspection()?;
    let runtime_db = holon::runtime_db::RuntimeDb::open_and_migrate(
        config.runtime_db_path(),
        config.runtime_db_lock_path(),
    )?;
    let report = holon::model_config_migration::migrate_model_config_routes(
        &config.config_file_path,
        &runtime_db,
        write,
    )?;
    print_json(&serde_json::to_value(report)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigAppliedVia {
    DaemonApi,
    OfflineStore,
}

impl ConfigAppliedVia {
    fn as_str(self) -> &'static str {
        match self {
            Self::DaemonApi => "daemon_api",
            Self::OfflineStore => "offline_store",
        }
    }
}

fn print_config_applied_via(applied_via: ConfigAppliedVia) {
    eprintln!("applied_via={}", applied_via.as_str());
}

async fn local_runtime_config() -> Result<Option<(LocalClient, http::RuntimeConfigReadResponse)>> {
    let config = AppConfig::load_for_config_inspection()?;
    let client = LocalClient::new(config)?;
    match client.runtime_config().await {
        Ok(response) => Ok(Some((client, response))),
        Err(error) if is_local_runtime_absent(&error) => Ok(None),
        Err(error) => Err(error).context("local daemon runtime config API is unavailable"),
    }
}

fn is_local_runtime_absent(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(http_error) = cause.downcast_ref::<LocalHttpError>() {
            return http_error.status_code == 404;
        }
        let message = cause.to_string();
        message.contains("Connection refused")
            || message.contains("connection refused")
            || message.contains("No such file or directory")
            || message.contains("os error 2")
    })
}

async fn config_get_command(key: String) -> Result<()> {
    if let Some((_client, response)) = local_runtime_config().await? {
        let config = load_persisted_config_at(&response.config_file_path)?;
        return print_json(&get_config_key(&config, &key)?);
    }

    let path = config_file_path();
    let config = load_persisted_config_at(&path)?;
    print_json(&get_config_key(&config, &key)?)
}

async fn config_set_command(key: String, value: String) -> Result<()> {
    if let Some((client, _response)) = local_runtime_config().await? {
        let response = apply_runtime_config_update(
            &client,
            http::RuntimeConfigUpdateEntry {
                key: key.clone(),
                value: Some(serde_json::Value::String(value.clone())),
                unset: false,
            },
        )
        .await?;
        let config = load_persisted_config_at(&response.config_file_path)?;
        print_config_applied_via(ConfigAppliedVia::DaemonApi);
        return print_json(&get_config_key(&config, &key)?);
    }

    let path = config_file_path();
    let mut config = load_persisted_config_at(&path)?;
    set_config_key(&mut config, &key, &value)?;
    save_persisted_config_at(&path, &config)?;
    print_config_applied_via(ConfigAppliedVia::OfflineStore);
    print_json(&get_config_key(&config, &key)?)
}

async fn config_unset_command(key: String) -> Result<()> {
    if let Some((client, _response)) = local_runtime_config().await? {
        apply_runtime_config_update(
            &client,
            http::RuntimeConfigUpdateEntry {
                key: key.clone(),
                value: None,
                unset: true,
            },
        )
        .await?;
        print_config_applied_via(ConfigAppliedVia::DaemonApi);
        return print_json(&serde_json::json!({
            "key": key,
            "status": "unset"
        }));
    }

    let path = config_file_path();
    let mut config = load_persisted_config_at(&path)?;
    unset_config_key(&mut config, &key)?;
    save_persisted_config_at(&path, &config)?;
    print_config_applied_via(ConfigAppliedVia::OfflineStore);
    print_json(&serde_json::json!({
        "key": key,
        "status": "unset"
    }))
}

async fn config_list_command() -> Result<()> {
    if let Some((_client, response)) = local_runtime_config().await? {
        let config = load_persisted_config_at(&response.config_file_path)?;
        return print_json(&serde_json::to_value(config)?);
    }

    let path = config_file_path();
    let config = load_persisted_config_at(&path)?;
    print_json(&serde_json::to_value(config)?)
}

async fn config_schema_command() -> Result<()> {
    print_json(&serde_json::to_value(config_schema())?)
}

async fn apply_runtime_config_update(
    client: &LocalClient,
    update: http::RuntimeConfigUpdateEntry,
) -> Result<http::RuntimeConfigUpdateResponse> {
    let key = update.key.clone();
    let response = client
        .update_runtime_config(&http::RuntimeConfigUpdateRequest {
            updates: vec![update],
        })
        .await?;
    let result = response
        .results
        .iter()
        .find(|result| result.key == key)
        .with_context(|| format!("daemon runtime config response omitted result for {key}"))?;
    if result.effect == http::RuntimeConfigUpdateEffect::Rejected {
        anyhow::bail!(
            "daemon rejected runtime config update for {}: {}",
            result.key,
            result.reason
        );
    }
    if result.effect == http::RuntimeConfigUpdateEffect::AcceptedRequiresRestart {
        eprintln!("daemon_update_note={}: {}", result.key, result.reason);
    }
    Ok(response)
}

async fn apply_onboarding_submission(
    config: &AppConfig,
    submission: &OnboardingWizardSubmission,
) -> Result<OnboardingApplySummary> {
    if let Some((client, _response)) = local_runtime_config().await? {
        let mut updates = vec![http::RuntimeConfigUpdateEntry {
            key: "model.default".into(),
            value: Some(serde_json::Value::String(
                submission.draft.default_model.as_string(),
            )),
            unset: false,
        }];
        match submission.draft.search {
            OnboardingSearchSelection::Disabled => {
                updates.push(http::RuntimeConfigUpdateEntry {
                    key: "web.search.enabled".into(),
                    value: Some(serde_json::json!(false)),
                    unset: false,
                });
            }
            OnboardingSearchSelection::Auto => {
                updates.extend([
                    http::RuntimeConfigUpdateEntry {
                        key: "web.search.enabled".into(),
                        value: Some(serde_json::json!(true)),
                        unset: false,
                    },
                    http::RuntimeConfigUpdateEntry {
                        key: "web.search.builtin_provider.enabled".into(),
                        value: Some(serde_json::json!(true)),
                        unset: false,
                    },
                    http::RuntimeConfigUpdateEntry {
                        key: "web.search.provider".into(),
                        value: Some(serde_json::Value::String("auto".into())),
                        unset: false,
                    },
                ]);
            }
            OnboardingSearchSelection::ManagedDuckDuckGo => {
                updates.extend([
                    http::RuntimeConfigUpdateEntry {
                        key: "web.search.enabled".into(),
                        value: Some(serde_json::json!(true)),
                        unset: false,
                    },
                    http::RuntimeConfigUpdateEntry {
                        key: "web.search.builtin_provider.enabled".into(),
                        value: Some(serde_json::json!(false)),
                        unset: false,
                    },
                    http::RuntimeConfigUpdateEntry {
                        key: "web.search.provider".into(),
                        value: Some(serde_json::Value::String("duckduckgo".into())),
                        unset: false,
                    },
                    http::RuntimeConfigUpdateEntry {
                        key: "web.search.providers".into(),
                        value: Some(serde_json::json!(["duckduckgo"])),
                        unset: false,
                    },
                ]);
            }
        }
        apply_runtime_config_updates(&client, updates).await?;
        let mut summary = apply_onboarding_wizard_draft(
            config,
            &submission.draft,
            submission.credential_material.clone(),
        )?;
        summary.applied_via = ConfigAppliedVia::DaemonApi.as_str().into();
        return Ok(summary);
    }

    apply_onboarding_wizard_draft(
        config,
        &submission.draft,
        submission.credential_material.clone(),
    )
}

async fn apply_runtime_config_updates(
    client: &LocalClient,
    updates: Vec<http::RuntimeConfigUpdateEntry>,
) -> Result<http::RuntimeConfigUpdateResponse> {
    let response = client
        .update_runtime_config(&http::RuntimeConfigUpdateRequest { updates })
        .await?;
    for result in &response.results {
        if result.effect == http::RuntimeConfigUpdateEffect::Rejected {
            anyhow::bail!(
                "daemon rejected runtime config update for {}: {}",
                result.key,
                result.reason
            );
        }
        if result.effect == http::RuntimeConfigUpdateEffect::AcceptedRequiresRestart {
            eprintln!("daemon_update_note={}: {}", result.key, result.reason);
        }
    }
    Ok(response)
}

async fn handle_config_models_command(command: ConfigModelCommands) -> Result<()> {
    match command {
        ConfigModelCommands::List => {
            let config = AppConfig::load_for_config_inspection()?;
            print_json(&serde_json::to_value(resolved_model_availability(&config))?)
        }
        ConfigModelCommands::Refresh { provider } => {
            let provider_id = ProviderId::parse(&provider)?;
            let config = AppConfig::load_for_config_inspection()?;
            let provider = config.providers.get(&provider_id).with_context(|| {
                format!(
                    "provider {} is not configured or built in",
                    provider_id.as_str()
                )
            })?;
            let report =
                refresh_provider_models(provider, &discovery_cache_path(&config.home_dir)).await?;
            print_json(&serde_json::to_value(report)?)
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
                cache_capabilities: None,
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
                builtin_web_search: None,
                endpoints: BTreeMap::new(),
                plans: BTreeMap::new(),
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
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .try_init();
}
