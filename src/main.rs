use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{IsTerminal, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
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
    daemon::{
        daemon_logs, daemon_restart, daemon_start, daemon_status, daemon_stop,
        ensure_serve_preflight, prepare_runtime_before_server, RuntimeServiceHandle,
        DAEMON_SERVE_ARGS_ENV, PRE_SERVER_PREPARED_ENV,
    },
    fd_limit::{apply_nofile_limit_policy, DEFAULT_NOFILE_TARGET},
    host::RuntimeHost,
    http::{self, AppState, ControlRequest, CreateCommandTaskRequest, CreateTimerRequest},
    memory::{rebuild_memory_index, request_memory_index_rebuild},
    model_discovery::{discovery_cache_path, refresh_provider_models},
    onboarding::{
        apply_onboarding_wizard_draft, onboarding_report, OnboardingApplySummary,
        OnboardingSearchSelection, OnboardingWizardSubmission,
    },
    onboarding_tui::run_onboarding_tui,
    provider::{provider_doctor, resolved_model_availability},
    run_once::{run_once, RunOnceRequest},
    runtime_db::RuntimeDb,
    solve::{run_solve, SolveRequest},
    storage::AppStorage,
    tui::run_tui,
    types::{AuditEvent, AuthorityClass, ControlAction, TimerStatus},
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use holon::cli::{
    AgentCommands, AgentModelCommands, Cli, Commands, ConfigCommands, ConfigCredentialCommands,
    ConfigModelCommands, ConfigProviderCommands, ControlCommandAction, DaemonCommands,
    DebugCommands, EventsCommands, MemoryIndexCommands, ServeAccess, ServeOptions, SkillsCommands,
    TaskCommands, WorkItemCommands, WorkspaceCommands,
};
#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

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
        Commands::Events { command } => handle_events_command(&config, command).await,
        Commands::Task { command } => handle_task_command(&config, command).await,
        Commands::WorkItem { command } => handle_work_item_command(&config, command).await,
        Commands::MemoryIndex { command } => handle_memory_index_command(&config, command).await,
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
                    authority_class: Some(AuthorityClass::OperatorInstruction),
                },
            )
            .await
        }
        Commands::Control { action, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
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
        Commands::Onboard { .. } => unreachable!("onboard command is handled before runtime load"),
        Commands::Config { .. } => unreachable!("config commands are handled separately"),
    }
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

fn restart_serve_launch_options(
    config: &mut AppConfig,
    options: ServeOptions,
    metadata: Option<&holon::daemon::RuntimeServiceMetadata>,
) -> Result<DaemonServeLaunchOptions> {
    let options = if let Some(metadata) = metadata {
        let inherited = parse_serve_options_from_args(
            &metadata
                .serve_args
                .iter()
                .map(OsString::from)
                .collect::<Vec<_>>(),
        )?;
        let merged = merge_serve_options(inherited, options);
        apply_serve_options(config, merged.clone())?;
        if metadata.control_token_env_configured
            && config
                .control_token
                .as_deref()
                .is_none_or(|token| token.trim().is_empty())
        {
            return Err(anyhow!(
                "the previous daemon start used an inline --token, which is not persisted; restart with --token or --token-file"
            ));
        }
        merged
    } else {
        apply_serve_options(config, options.clone())?;
        options
    };
    Ok(serve_args_for_options(&options))
}

async fn serve(mut config: AppConfig, options: ServeOptions) -> Result<()> {
    let serve_args = serve_args_for_options(&options).args;
    let web_dist = options.web_dist.clone();
    let advertise_url = apply_serve_options(&mut config, options)?;
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
    ensure_serve_preflight(&config).await?;
    if std::env::var_os(PRE_SERVER_PREPARED_ENV).is_none() {
        prepare_runtime_before_server(&config)?;
    }
    if std::env::var_os(DAEMON_SERVE_ARGS_ENV).is_none() {
        std::env::set_var(
            DAEMON_SERVE_ARGS_ENV,
            serde_json::to_string(&safe_serve_args_for_metadata(&serve_args))?,
        );
    }

    let host = RuntimeHost::new(config.clone())?;
    host.default_runtime().await?;
    let runtime_service = RuntimeServiceHandle::new(&config)?;

    let tcp_router = http::router(
        AppState::for_tcp_with_runtime_service(host.clone(), Some(runtime_service.clone()))
            .with_advertise_url(advertise_url.clone())
            .with_web_dist(web_dist.clone()),
    );
    let listener = TcpListener::bind(&config.http_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.http_addr))?;
    println!("Holon listening on {}", listener.local_addr()?);
    if let Some(advertise_url) = &advertise_url {
        println!("Holon advertised at {advertise_url}");
    }
    // Always bind a localhost listener so local tools (holon tui, curl) can
    // connect even when the primary address targets a tailnet or LAN IP.
    // See: https://github.com/holon-run/holon/issues/1449
    let primary_ip = listener.local_addr()?.ip();
    let primary_is_unspecified = primary_ip.is_unspecified();
    // Skip when primary is already loopback or binds all interfaces (0.0.0.0 / ::),
    // since binding 127.0.0.1 on the same port would fail with EADDRINUSE.
    let local_listener = if !primary_ip.is_loopback() && !primary_is_unspecified {
        let local_addr = std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            listener.local_addr()?.port(),
        );
        let local = TcpListener::bind(local_addr)
            .await
            .with_context(|| format!("failed to bind localhost listener on {local_addr}"))?;
        println!("Holon local listening on {}", local.local_addr()?);
        Some(local)
    } else {
        None
    };

    #[cfg(unix)]
    {
        ensure_socket_parent(&config.socket_path)?;
        let unix_listener = tokio::net::UnixListener::bind(&config.socket_path)
            .with_context(|| format!("failed to bind {}", config.socket_path.display()))?;
        runtime_service.write_state_files(&config)?;
        println!("Holon control socket on {}", config.socket_path.display());
        let unix_router = http::router(
            AppState::for_unix_with_runtime_service(host.clone(), Some(runtime_service.clone()))
                .with_web_dist(web_dist.clone()),
        );
        let tcp_server = axum::serve(listener, tcp_router)
            .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()));
        let unix_server = http::serve_unix(
            unix_listener,
            unix_router,
            runtime_service.shutdown_signal(),
        );
        let result = if let Some(local) = local_listener {
            let local_router = http::router(
                AppState::for_tcp_with_runtime_service(host.clone(), Some(runtime_service.clone()))
                    .with_advertise_url(advertise_url.clone())
                    .with_web_dist(web_dist.clone()),
            );
            let local_server = axum::serve(local, local_router)
                .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()));
            tokio::try_join!(tcp_server, unix_server, local_server)
                .map(|_| ())
                .context("runtime servers failed")
        } else {
            tokio::try_join!(tcp_server, unix_server)
                .map(|_| ())
                .context("runtime servers failed")
        };
        let _ = runtime_service.cleanup_state_files(&config);
        return result;
    }

    #[cfg(not(unix))]
    {
        runtime_service.write_state_files(&config)?;
        if let Some(local) = local_listener {
            let local_router = http::router(
                AppState::for_tcp_with_runtime_service(host.clone(), Some(runtime_service.clone()))
                    .with_advertise_url(advertise_url.clone())
                    .with_web_dist(web_dist.clone()),
            );
            let local_server = axum::serve(local, local_router)
                .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()));
            let tcp_server = axum::serve(listener, tcp_router)
                .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()));
            tokio::try_join!(tcp_server, local_server)
                .map(|_| ())
                .context("runtime servers failed")?;
        } else {
            axum::serve(listener, tcp_router)
                .with_graceful_shutdown(wait_for_shutdown(runtime_service.shutdown_signal()))
                .await
                .context("HTTP server failed")?;
        }
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
    authority_class: AuthorityClass,
) -> Result<()> {
    let host = RuntimeHost::new(config.clone())?;
    let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
    let prompt = host
        .preview_agent_prompt(&agent, text, authority_class)
        .await?;
    println!("{}", prompt.render_dump());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use holon::{
        config::{provider_registry_for_tests, AltScreenMode, ModelRef},
        runtime_db::RuntimeDb,
    };

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
            api_cors: Default::default(),
            config_file_path: home.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            fallback_models: Vec::new(),
            vision_model: None,
            vision_candidate_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: 8_000,
            max_tool_output_tokens: 64_000,
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
    fn tailnet_host_is_client_visible_not_listen_socket() {
        let mut config = test_config();
        let advertise = apply_serve_options(
            &mut config,
            ServeOptions {
                access: Some(ServeAccess::Tailnet),
                host: Some("lab.tailnet.ts.net".into()),
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
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
                access: Some(ServeAccess::Lan),
                host: Some("192.168.1.10".into()),
                listen: None,
                port: Some(8787),
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
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
    fn agent_lifecycle_commands_parse_with_optional_agent_id() {
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
    fn task_lifecycle_commands_parse_with_agent_options() {
        let cli = Cli::parse_from(["holon", "task", "status", "task-1", "--agent", "runner"]);
        let Commands::Task {
            command: TaskCommands::Status { task_id, agent },
        } = cli.command
        else {
            panic!("expected task status command");
        };
        assert_eq!(task_id, "task-1");
        assert_eq!(agent.as_deref(), Some("runner"));

        let cli = Cli::parse_from([
            "holon",
            "task",
            "output",
            "task-2",
            "--block",
            "--timeout-ms",
            "100",
        ]);
        let Commands::Task {
            command:
                TaskCommands::Output {
                    task_id,
                    block,
                    timeout_ms,
                    agent,
                },
        } = cli.command
        else {
            panic!("expected task output command");
        };
        assert_eq!(task_id, "task-2");
        assert!(block);
        assert_eq!(timeout_ms, Some(100));
        assert_eq!(agent, None);

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
    fn export_scheduler_fixture_writes_replay_harness_shape() {
        let config = test_config();
        let agent_id = "default";
        let host = RuntimeHost::new(config.clone()).unwrap();
        let storage = host.agent_storage(agent_id).unwrap();
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
    fn export_scheduler_fixture_reads_agent_state_from_runtime_db() {
        let config = test_config();
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
                .unwrap();
        let mut agent = holon::types::AgentState::new("default");
        agent.current_work_item_id = Some("work-db".into());
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
            access: Some(ServeAccess::Lan),
            host: Some("192.168.1.10".into()),
            listen: None,
            port: None,
            advertise: None,
            token: Some("secret-token".into()),
            token_file: None,
            web_dist: None,
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
            serve_args: args.into_iter().map(String::from).collect(),
            control_token_env_configured,
        }
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
                access: None,
                host: Some("192.168.1.11".into()),
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
            },
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(config.http_addr, "192.168.1.11:8787");
        assert_eq!(config.callback_base_url, "http://192.168.1.11:8787");
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
                OsString::from("--token-file"),
                token_file.as_os_str().to_os_string(),
            ]
        );
        assert_eq!(launch.control_token_env, None);
        assert!(!launch.args.iter().any(|arg| arg == "--advertise"));
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
                access: Some(ServeAccess::Local),
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
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
                access: Some(ServeAccess::Tunnel),
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
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
                access: None,
                host: None,
                listen: Some("0.0.0.0:8989".into()),
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
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
                access: None,
                host: None,
                listen: None,
                port: Some(8989),
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
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
                access: None,
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: Some("restart-secret".into()),
                token_file: None,
                web_dist: None,
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
                access: None,
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
            },
            Some(&metadata),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("previous daemon start used an inline --token"));
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
                    builtin,
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
        assert!(!builtin);
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
                access: Some(ServeAccess::Lan),
                host: None,
                listen: Some("0.0.0.0:7878".into()),
                port: None,
                advertise: Some("http://lab.example.test:7878".into()),
                token: None,
                token_file: None,
                web_dist: None,
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
                access: Some(ServeAccess::Local),
                host: None,
                listen: None,
                port: None,
                advertise: None,
                token: None,
                token_file: None,
                web_dist: None,
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
            id: holon::ids::audit_event_id(),
            event_seq: 0,
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

async fn handle_events_command(config: &AppConfig, command: EventsCommands) -> Result<()> {
    match command {
        EventsCommands::Tail {
            before_seq,
            after_seq,
            limit,
            order,
            max_level,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config.clone())?;
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
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let client = LocalClient::new(config.clone())?;
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

async fn handle_debug_command(config: AppConfig, command: DebugCommands) -> Result<()> {
    match command {
        DebugCommands::Prompt {
            text,
            agent,
            authority_class,
        } => dump_prompt(config, text, agent, authority_class).await,
        DebugCommands::Latency {
            agent,
            limit,
            events_limit,
        } => print_latency_diagnostics(&config, agent, limit, events_limit),
        DebugCommands::Performance { json } => print_performance_diagnostics(&config, json).await,
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
            let metadata = holon::daemon::load_daemon_metadata(&config)?;
            let serve_launch =
                restart_serve_launch_options(&mut config, options, metadata.as_ref())?;
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

async fn handle_task_command(config: &AppConfig, command: TaskCommands) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    match command {
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
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
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
                    authority_class: Some(AuthorityClass::OperatorInstruction),
                },
            )
            .await
        }
        TaskCommands::Status { task_id, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client.task_status(&agent, &task_id).await?,
            )?)
        }
        TaskCommands::Output {
            task_id,
            block,
            timeout_ms,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client
                    .task_output(&agent, &task_id, block, timeout_ms)
                    .await?,
            )?)
        }
        TaskCommands::Input {
            task_id,
            text,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client.task_input(&agent, &task_id, text).await?,
            )?)
        }
        TaskCommands::Stop { task_id, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client.task_stop(&agent, &task_id).await?,
            )?)
        }
    }
}

async fn handle_work_item_command(config: &AppConfig, command: WorkItemCommands) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
    match command {
        WorkItemCommands::List { limit, agent } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client.agent_work_items(&agent, limit).await?,
            )?)
        }
        WorkItemCommands::Get {
            work_item_id,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            print_json(&serde_json::to_value(
                client.work_item(&agent, &work_item_id).await?,
            )?)
        }
    }
}

async fn handle_agent_command(config: &AppConfig, command: Option<AgentCommands>) -> Result<()> {
    match command {
        None | Some(AgentCommands::List) => {
            let client = LocalClient::new(config.clone())?;
            print_json(&serde_json::to_value(client.list_agent_entries().await?)?)
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
                    authority_class: Some(AuthorityClass::OperatorInstruction),
                    template,
                },
            )
            .await
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
        SkillsCommands::Catalog => {
            let response: serde_json::Value = get_json(config, "/skills/catalog").await?;
            print_json(&response)
        }
        SkillsCommands::Add {
            source,
            builtin,
            remote,
            skill,
            copy,
        } => {
            let kind = build_skill_add_kind(&source, builtin, remote, skill, copy)?;
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
            post_control_json(
                config,
                "/skills/catalog/update",
                &holon::types::UpdateSkillRequest { name },
            )
            .await
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
            builtin,
            remote,
            skill,
            copy,
            agent,
        } => {
            let agent = agent.unwrap_or_else(|| config.default_agent_id.clone());
            let kind = if builtin {
                if remote || skill.is_some() {
                    anyhow::bail!("--builtin cannot be combined with --remote or --skill");
                }
                holon::types::SkillInstallKind::Builtin { name: name_or_path }
            } else if remote {
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
    builtin: bool,
    remote: bool,
    skill: Option<String>,
    copy: bool,
) -> Result<holon::types::SkillInstallKind> {
    if builtin {
        if remote || skill.is_some() {
            anyhow::bail!("--builtin cannot be combined with --remote or --skill");
        }
        Ok(holon::types::SkillInstallKind::Builtin {
            name: source.to_string(),
        })
    } else if remote {
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
        ConfigCommands::List => config_list_command().await,
        ConfigCommands::Schema => config_schema_command().await,
        ConfigCommands::Doctor => {
            let config = AppConfig::load_for_config_inspection()?;
            print_json(&provider_doctor(&config))
        }
    }
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
