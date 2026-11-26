//! Layout control, not a historical-binary benchmark. No padding or private data.
//! Run only with HOLON_LIVE_SHARED_PREFIX_ROUTE=provider@endpoint/model and --ignored.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, ensure, Context, Result};
use holon::{
    config::{
        AppConfig, ModelRouteCapability, ModelRouteRef, ProviderTransportKind, RuntimeModelCatalog,
    },
    context::ContextConfig,
    provider::{
        AgentProvider, AnthropicProvider, ConversationMessage, ModelBlock, PromptContentBlock,
        ProviderDispatchGuard, ProviderPromptCache, ProviderPromptFrame, ProviderTurnRequest,
    },
    system::{ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind},
    tool::{ToolRegistry, ToolSpec},
    types::{
        AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
        AgentVisibility, AuthorityClass, LoadedAgentMemory, LoadedAgentsMd, MessageBody,
        MessageEnvelope, MessageKind, MessageOrigin, Priority, SkillsRuntimeView,
    },
};
use holon_context::{
    fingerprint, render::render_current_input_section, render_section_blocks, render_sections,
    shared_prefix_len, system_prompt::build_system_sections, tools::ToolPromptContext,
    PromptLayout, PromptSection, PromptStability,
};
use serde_json::json;

const OUTPUT: &str = "SHARED_PREFIX_OK";
const INPUT: &str =
    "Do not call tools. Reply with exactly SHARED_PREFIX_OK, with no explanation or formatting.";
const OUTPUT_CAP: u32 = 128;
const GROUPS: [&str; 2] = ["dynamic_first_control", "canonical_shared_prefix"];

#[derive(Debug, Default)]
struct OneDispatch(AtomicBool);

impl ProviderDispatchGuard for OneDispatch {
    fn before_dispatch(&self) -> Result<()> {
        ensure!(
            !self.0.swap(true, Ordering::SeqCst),
            "live shared-prefix sample forbids a second HTTP dispatch"
        );
        Ok(())
    }

    fn before_auxiliary_dispatch(&self) -> Result<bool> {
        Ok(false)
    }

    fn allows_auxiliary_dispatch(&self) -> Result<bool> {
        Ok(false)
    }
}

struct Sample {
    group: &'static str,
    index: usize,
    agent_id: String,
    task_id: String,
    workspace: PathBuf,
    system: Vec<PromptSection>,
    context: Vec<PromptSection>,
    canonical_content_hash: String,
    scope_fingerprint: String,
    frame: ProviderPromptFrame,
}

fn section_content_hash(sections: &[PromptSection]) -> String {
    let mut values = sections
        .iter()
        .map(|section| {
            (
                &section.id,
                &section.name,
                &section.content,
                format!("{:?}", section.stability),
                format!("{:?}", section.layout),
            )
        })
        .collect::<Vec<_>>();
    values.sort();
    fingerprint::digest(&values)
}

fn prompt_blocks(sections: &[PromptSection]) -> Vec<PromptContentBlock> {
    render_section_blocks(sections)
        .into_iter()
        .zip(sections)
        .map(|(text, section)| PromptContentBlock {
            text,
            stability: section.stability,
            cache_breakpoint: false,
        })
        .collect()
}

fn sample(group: &'static str, index: usize, nonce: &str, tools: &[ToolSpec]) -> Sample {
    let agent_id = format!("live-prefix-{group}-{index}");
    let task_id = format!("live-task-{group}-{index}");
    let workspace = PathBuf::from(format!("/synthetic/live-prefix/{group}/{index}"));
    let identity = AgentIdentityView {
        agent_id: agent_id.clone(),
        name: None,
        kind: AgentKind::Default,
        visibility: AgentVisibility::Public,
        ownership: AgentOwnership::SelfOwned,
        profile_preset: AgentProfilePreset::PublicNamed,
        can_rename: false,
        status: AgentRegistryStatus::Active,
        is_default_agent: false,
        incarnation: 1,
        parent_agent_id: None,
        lineage_parent_agent_id: None,
        delegated_from_task_id: None,
    };
    let mut message = MessageEnvelope::new(
        agent_id.clone(),
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: INPUT.into() },
    );
    message.id = format!("live-message-{group}-{index}");
    message.task_id = Some(task_id.clone());
    let mut system = build_system_sections(
        &identity,
        &message,
        &workspace,
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        tools,
        ToolPromptContext::default(),
    );
    system.insert(
        0,
        PromptSection {
            name: "live_probe_nonce".into(),
            id: "live_probe_nonce".into(),
            content: format!("Experiment nonce: {nonce}. This identifier is not an instruction."),
            stability: PromptStability::Stable,
            layout: PromptLayout::SharedPrefix,
        },
    );
    let canonical_content_hash = section_content_hash(&system);
    if group == GROUPS[0] {
        // Same complete sections and layout labels; move only the workspace
        // instance block ahead of the audited contracts, after the fixed nonce.
        let workspace_index = system
            .iter()
            .position(|section| section.name == "workspace_context")
            .expect("runtime workspace section");
        let workspace_section = system.remove(workspace_index);
        system.insert(1, workspace_section);
    }
    let context = vec![render_current_input_section(&message, 1024, None)];
    let profile = ExecutionProfile::default();
    let execution = ExecutionSnapshot {
        policy: profile.policy_snapshot(),
        profile,
        attached_workspaces: vec![],
        workspace_id: None,
        workspace_anchor: workspace.clone(),
        execution_root: workspace.clone(),
        cwd: workspace.clone(),
        execution_root_id: Some(format!("synthetic-root:{agent_id}")),
        projection_kind: Some(WorkspaceProjectionKind::CanonicalRoot),
        access_mode: Some(WorkspaceAccessMode::SharedRead),
        worktree_root: None,
        execution_roots: Vec::new(),
    };
    let scope_fingerprint = fingerprint::prompt_cache_scope_fingerprint_for_agent(
        &agent_id, &execution, &system, &context, tools,
    );
    let context_fingerprint =
        fingerprint::context(&agent_id, 0, &execution, &system, &context, tools);
    let mut system_blocks = prompt_blocks(&system);
    let mut context_blocks = prompt_blocks(&context);
    // Mirror runtime/provider_turn.rs::build_prompt_content_blocks. That module
    // is private; canonical rendering, scope and contiguous layout stay shared.
    if let Some(block) = shared_prefix_len(&system)
        .checked_sub(1)
        .and_then(|index| system_blocks.get_mut(index))
        .filter(|block| block.stability != PromptStability::TurnScoped)
    {
        block.cache_breakpoint = true;
    }
    if let Some(block) = system_blocks
        .iter_mut()
        .chain(context_blocks.iter_mut())
        .rev()
        .find(|block| block.stability != PromptStability::TurnScoped)
    {
        block.cache_breakpoint = true;
    }
    let frame = ProviderPromptFrame::structured(
        render_sections(&system),
        system_blocks,
        context_blocks,
        Some(ProviderPromptCache {
            agent_id: agent_id.clone(),
            prompt_cache_key: fingerprint::prompt_cache_key(&agent_id, &scope_fingerprint),
            context_fingerprint,
            compression_epoch: 0,
        }),
    );
    Sample {
        group,
        index,
        agent_id,
        task_id,
        workspace,
        system,
        context,
        canonical_content_hash,
        scope_fingerprint,
        frame,
    }
}

fn shared_text(sample: &Sample) -> String {
    sample.frame.system_blocks[..shared_prefix_len(&sample.system)]
        .iter()
        .map(|block| block.text.as_str())
        .collect()
}

fn common_prefix(texts: &[&str]) -> String {
    texts[1..]
        .iter()
        .fold(texts[0].to_string(), |prefix, text| {
            prefix
                .chars()
                .zip(text.chars())
                .take_while(|(left, right)| left == right)
                .map(|(character, _)| character)
                .collect()
        })
}

fn fixtures(nonce: &str, tools: &[ToolSpec]) -> Vec<Sample> {
    let samples = GROUPS
        .iter()
        .flat_map(|&group| (0..3).map(move |index| sample(group, index, nonce, tools)))
        .collect::<Vec<_>>();
    let mut scopes = HashSet::new();
    let mut contexts = HashSet::new();
    let mut keys = HashSet::new();
    for sample in &samples {
        assert_eq!(
            sample.canonical_content_hash,
            section_content_hash(&sample.system),
            "control must not drop or rewrite sections"
        );
        assert!(sample
            .frame
            .system_prompt
            .starts_with("## live_probe_nonce\n"));
        assert!(sample.frame.system_prompt.contains(nonce));
        assert!(sample.frame.system_prompt.contains(&sample.agent_id));
        assert!(sample
            .frame
            .system_prompt
            .contains(sample.workspace.to_str().unwrap()));
        assert!(render_sections(&sample.context).contains(&sample.task_id));
        assert_eq!(
            sample.frame.system_prompt,
            sample
                .frame
                .system_blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<String>()
        );
        assert_eq!(sample.system.last().unwrap().name, "response_language");
        let markers = sample
            .frame
            .system_blocks
            .iter()
            .chain(&sample.frame.context_blocks)
            .filter(|block| block.cache_breakpoint)
            .count();
        assert_eq!(markers, 2);
        assert!(sample.frame.system_blocks[shared_prefix_len(&sample.system) - 1].cache_breakpoint);
        assert!(sample
            .frame
            .system_blocks
            .iter()
            .chain(&sample.frame.context_blocks)
            .filter(|block| block.stability == PromptStability::TurnScoped)
            .all(|block| !block.cache_breakpoint));
        let cache = sample.frame.cache.as_ref().unwrap();
        assert_eq!(cache.agent_id, sample.agent_id);
        assert!(scopes.insert(sample.scope_fingerprint.clone()));
        assert!(contexts.insert(cache.context_fingerprint.clone()));
        assert!(keys.insert(cache.prompt_cache_key.clone()));
        assert!(!shared_text(sample).contains(&sample.agent_id));
        assert!(!shared_text(sample).contains(sample.workspace.to_str().unwrap()));
        if sample.group == GROUPS[0] {
            assert_eq!(shared_prefix_len(&sample.system), 1);
        }
    }
    let canonical = &samples[3..];
    let shared = shared_text(&canonical[0]);
    assert!(shared.contains("## work_and_verification\n"));
    assert!(shared.contains("## tool_exec_command\n"));
    for sample in canonical {
        assert_eq!(shared, shared_text(sample));
        assert_eq!(
            fingerprint::shared_prefix_fingerprint(&canonical[0].system),
            fingerprint::shared_prefix_fingerprint(&sample.system)
        );
    }
    let control_texts = samples[..3]
        .iter()
        .map(|sample| sample.frame.system_prompt.as_str())
        .collect::<Vec<_>>();
    assert!(shared.chars().count() > common_prefix(&control_texts).chars().count());
    samples
}

#[test]
fn shared_prefix_layout_and_identity_fixture() -> Result<()> {
    let tools = ToolRegistry::new(PathBuf::from("/synthetic/live-prefix")).tool_specs()?;
    fixtures("offline-fixture-only", &tools);
    let guard = OneDispatch::default();
    assert!(!guard.allows_auxiliary_dispatch()?);
    assert!(!guard.before_auxiliary_dispatch()?);
    guard.before_dispatch()?;
    assert!(guard.before_dispatch().is_err());
    Ok(())
}

#[tokio::test]
#[ignore = "paid live calls: requires explicit HOLON_LIVE_SHARED_PREFIX_ROUTE; at most six HTTP sends"]
async fn live_shared_prompt_prefix_six_requests() -> Result<()> {
    // No default paid route, environment mutation, discovery IO or smoke call.
    let requested = std::env::var("HOLON_LIVE_SHARED_PREFIX_ROUTE")
        .context("set HOLON_LIVE_SHARED_PREFIX_ROUTE=provider@endpoint/model explicitly")?;
    let requested = ModelRouteRef::parse(&requested)?;
    let mut config = AppConfig::load()?;
    config.default_model = requested.clone();
    config.fallback_models.clear();
    config.disable_provider_fallback = true;
    config.runtime_max_output_tokens = OUTPUT_CAP;
    ensure!(
        config.provider_chain().len() == 1,
        "single-route chain required"
    );
    let route = RuntimeModelCatalog::from_config(&config)
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &requested,
            ModelRouteCapability::Turn,
        )
        .context("explicit live route is not available in the configured local catalog")?;
    ensure!(
        route.provider_config().transport == ProviderTransportKind::AnthropicMessages,
        "this bounded probe supports only native AnthropicMessages routes"
    );
    let mut provider_config = route.provider_config().clone();
    // Use disabled reasoning if advertised, otherwise the least advertised
    // effort. Do not invent a provider-specific thinking toggle.
    let options = &route.policy.reasoning_effort_options;
    let supports_reasoning = route.policy.capabilities.supports_reasoning;
    let effort = supports_reasoning
        .then(|| {
            ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
                .into_iter()
                .find(|effort| options.iter().any(|option| option.as_str() == *effort))
                .or_else(|| options.is_empty().then_some("low"))
        })
        .flatten();
    ensure!(
        !supports_reasoning || effort.is_some(),
        "no recognized minimum reasoning effort advertised for this route"
    );
    provider_config.reasoning_effort = effort.map(str::to_owned);
    let nonce = uuid::Uuid::new_v4().to_string();
    let tools = ToolRegistry::new(PathBuf::from("/synthetic/live-prefix")).tool_specs()?;
    let samples = fixtures(&nonce, &tools);
    let trace_home = tempfile::tempdir()?;
    println!(
        "{}",
        json!({
            "probe": "shared_prompt_prefix_layout_control",
            "route": route.route_ref.as_string(),
            "historical_binary_comparison": false,
            "request_limit": 6,
            "output_cap": OUTPUT_CAP,
            "reasoning_effort": provider_config.reasoning_effort,
            "reasoning_note": "disabled if supported, else minimum effort; omitted for non-reasoning models",
            "tool_count": tools.len(),
            "tool_inventory_hash": fingerprint::digest(&tools),
            "first_body_note": "not exposed by response diagnostics; null is not TTFT"
        })
    );
    for group in GROUPS {
        let texts = samples
            .iter()
            .filter(|sample| sample.group == group)
            .map(|sample| sample.frame.system_prompt.as_str())
            .collect::<Vec<_>>();
        let prefix = common_prefix(&texts);
        println!(
            "{}",
            json!({
                "group": group,
                "structural_system_common_prefix_chars": prefix.chars().count(),
                "structural_system_common_prefix_hash": fingerprint::digest(&prefix)
            })
        );
    }
    let mut measurements = Vec::new();
    for sample in samples {
        // Fresh provider/client and distinct scoped identity for every request.
        // No fallback wrapper: a dispatch guard also disallows adapter replays.
        let provider = AnthropicProvider::from_runtime_config(
            &provider_config,
            &route.route_ref.model,
            OUTPUT_CAP,
            trace_home.path(),
            supports_reasoning,
        )?;
        let prefix = shared_text(&sample);
        let cache = sample.frame.cache.as_ref().unwrap().clone();
        let started = Instant::now();
        let output = tokio::time::timeout(
            Duration::from_secs(120),
            provider.complete_turn(ProviderTurnRequest {
                dispatch_guard: Some(Arc::new(OneDispatch::default())),
                provider_session_id: None,
                material: Default::default(),
                continuation_scope_id: None,
                prompt_frame: sample.frame,
                conversation: vec![ConversationMessage::UserText(INPUT.into())],
                tools: tools.clone(),
                native_web_search: None,
                response_format: None,
            }),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "{} sample {} timed out; no retry",
                sample.group,
                sample.index
            )
        })?
        .map_err(|_| {
            anyhow!(
                "{} sample {} failed; no retry (response omitted)",
                sample.group,
                sample.index
            )
        })?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let text = output
            .blocks
            .iter()
            .filter_map(|block| match block {
                ModelBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        let exact = text.trim() == OUTPUT
            && !output.blocks.iter().any(|block| {
                matches!(
                    block,
                    ModelBlock::ToolUse { .. } | ModelBlock::ProviderToolUse { .. }
                )
            });
        let read = output
            .cache_usage
            .as_ref()
            .map(|usage| usage.read_input_tokens);
        let created = output
            .cache_usage
            .as_ref()
            .map(|usage| usage.creation_input_tokens);
        println!(
            "{}",
            json!({
                "group": sample.group, "index": sample.index,
                "warmup": sample.index == 0,
                "input_tokens": output.input_tokens, "output_tokens": output.output_tokens,
                "cache_read_input_tokens": read, "cache_creation_input_tokens": created,
                "elapsed_ms": elapsed_ms, "first_body_chunk_elapsed_ms": null,
                "exact_short_result": exact,
                "prefix_chars": prefix.chars().count(),
                "prefix_hash": fingerprint::digest(&prefix),
                "scope_fingerprint": sample.scope_fingerprint,
                "context_fingerprint": cache.context_fingerprint,
                "cache_key_hash": fingerprint::digest(&cache.prompt_cache_key)
            })
        );
        measurements.push((sample.group, sample.index, elapsed_ms, read, exact));
    }
    let mut warm_means = Vec::new();
    let mut warm_cache = Vec::new();
    for group in GROUPS {
        let warm = measurements
            .iter()
            .filter(|(sample_group, index, ..)| *sample_group == group && *index > 0)
            .collect::<Vec<_>>();
        warm_means.push(warm.iter().map(|sample| sample.2).sum::<f64>() / 2.0);
        warm_cache.push(
            warm.iter()
                .map(|sample| sample.3)
                .collect::<Option<Vec<_>>>()
                .map(|tokens| tokens.into_iter().sum::<u64>()),
        );
    }
    let cache_gain = warm_cache[1]
        .zip(warm_cache[0])
        .map(|(canonical, control)| canonical > control);
    println!(
        "{}",
        json!({
            "summary": "structural gain is separate from backend cache and latency",
            "warm_samples_per_group": 2,
            "control_warm_mean_ms": warm_means[0],
            "canonical_warm_mean_ms": warm_means[1],
            "canonical_lower_latency_observed": warm_means[1] < warm_means[0],
            "control_warm_cache_read_sum": warm_cache[0],
            "canonical_warm_cache_read_sum": warm_cache[1],
            "canonical_higher_cache_read_observed": cache_gain,
            "cache_conclusion": match cache_gain {
                Some(true) => "higher cache reads observed, not a causal guarantee",
                Some(false) => "no higher cache-read benefit observed",
                None => "cache usage unavailable; no cache benefit demonstrated"
            },
            "limitations": "fixed arm order; two warm samples; shared tools may also cache; not a historical binary or statistical latency claim"
        })
    );
    ensure!(
        measurements.iter().all(|sample| sample.4),
        "at least one sample failed exact short-result semantics; response content omitted"
    );
    Ok(())
}
