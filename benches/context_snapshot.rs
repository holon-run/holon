//! Offline storage-acquisition and pure context-rendering baseline.
//! Run: HOLON_BENCH_SAMPLES=10 cargo bench --bench context_snapshot

use std::{hint::black_box, path::PathBuf, time::Instant};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use holon::{
    context::{
        build_context_from_snapshot, capture_context_from_storage, BuiltContext, ContextConfig,
        ContextSnapshot,
    },
    prompt::PromptStability,
    storage::AppStorage,
    system::{ExecutionProfile, ExecutionSnapshot},
    types::{
        AdmissionContext, AgentState, AuthorityClass, BriefKind, BriefRecord, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        SkillsRuntimeView, ToolExecutionRecord, ToolExecutionStatus, TurnOwner, TurnRecord,
        TurnTerminalKind, TurnTerminalSummary, TurnTriggerSummary, WorkItemExecutionBinding,
        WorkItemRecord, WorkItemState,
    },
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const AGENT: &str = "default";
const WORK_ITEM: &str = "work_context_snapshot";
const CURRENT_TURN: &str = "turn_context_snapshot_current";
const CURRENT_MESSAGE: &str = "msg_context_snapshot_current";
const HISTORICAL_TURNS: usize = 6;
const FULL_BUDGET: usize = 8192;
const SMALLER_BUDGET: usize = 4096;
const DEFAULT_SAMPLES: usize = 10;
const WARMUP_SAMPLES: usize = 1;
const CURRENT_TEXT: &str =
    "CURRENT_SNAPSHOT_REQUEST: compare the observations and propose the next bounded verification.";

struct Fixture {
    storage: AppStorage,
    agent: AgentState,
    execution: ExecutionSnapshot,
    current_message: MessageEnvelope,
    config: ContextConfig,
    agent_home: PathBuf,
    // The temporary directory outlives all storage handles.
    _root: TempDir,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(root.path())?;
        let agent_home = root.path().to_path_buf();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace)?;
        let timestamp: DateTime<Utc> = "2024-01-02T03:04:05.123Z".parse()?;
        let owner = TurnOwner::WorkItem {
            work_item_id: WORK_ITEM.into(),
        };
        let mut item = WorkItemRecord::new(
            AGENT,
            "Keep context snapshot acquisition and rendering behavior equivalent",
            WorkItemState::Open,
        );
        item.id = WORK_ITEM.into();
        item.created_at = timestamp;
        item.updated_at = timestamp;
        storage.append_work_item(&item)?;
        let plan_dir = agent_home.join("work-items").join(WORK_ITEM);
        std::fs::create_dir_all(&plan_dir)?;
        std::fs::write(
            plan_dir.join("plan.md"),
            "Compare fixed observations; retain operator scope; report measured limits.\n",
        )?;

        for index in 1..=HISTORICAL_TURNS {
            let turn_id = format!("turn_context_snapshot_{index}");
            let mut message = operator_message(format!(
                "Historical observation {index}: {}",
                "Keep authority, ownership and source evidence explicit. ".repeat(24)
            ));
            message.id = format!("msg_context_snapshot_{index}");
            message.created_at = timestamp;
            message.turn_id = Some(turn_id.clone());
            message.work_item_id = Some(WORK_ITEM.into());
            storage.append_message(&message)?;

            let mut brief = BriefRecord::new(
                AGENT,
                BriefKind::Result,
                format!("Observation {index} verified against the fixed local fixture."),
                Some(message.id.clone()),
                None,
            );
            brief.id = format!("brief_context_snapshot_{index}");
            brief.created_at = timestamp;
            brief.turn_id = Some(turn_id.clone());
            brief.turn_index = Some(index as u64);
            brief.work_item_id = Some(WORK_ITEM.into());
            storage.append_brief(&brief)?;

            let tool_id = format!("tool_context_snapshot_{index}");
            storage.append_tool_execution(&ToolExecutionRecord {
                id: tool_id.clone(),
                agent_id: AGENT.into(),
                work_item_id: Some(WORK_ITEM.into()),
                turn_index: index as u64,
                turn_id: Some(turn_id.clone()),
                tool_name: "ExecCommand".into(),
                created_at: timestamp,
                completed_at: Some(timestamp),
                duration_ms: 1,
                authority_class: AuthorityClass::RuntimeInstruction,
                status: ToolExecutionStatus::Success,
                input: json!({"cmd": "fixture only; no command was executed"}),
                output: json!({"exit_code": 0, "observation": index}),
                summary: format!("Fixed local evidence for observation {index}"),
                invocation_surface: Some("commentary".into()),
            })?;

            let mut turn = TurnRecord::new(AGENT, turn_id, index as u64);
            turn.created_at = timestamp;
            turn.current_work_item_id = Some(WORK_ITEM.into());
            turn.owner = Some(owner.clone());
            turn.trigger = Some(TurnTriggerSummary::from_message(&message));
            turn.input_message_ids = vec![message.id];
            turn.produced_brief_ids = vec![brief.id];
            turn.tool_execution_ids = vec![tool_id];
            turn.terminal = Some(TurnTerminalSummary {
                kind: TurnTerminalKind::Completed,
                reason: None,
                no_brief_reason: None,
                completed_at: timestamp,
                duration_ms: 1,
            });
            storage.append_turn(&turn)?;
        }

        let mut current_message = operator_message(CURRENT_TEXT.into());
        current_message.id = CURRENT_MESSAGE.into();
        current_message.created_at = timestamp;
        current_message.turn_id = Some(CURRENT_TURN.into());
        current_message.work_item_id = Some(WORK_ITEM.into());
        storage.append_message(&current_message)?;
        let mut agent = AgentState::new(AGENT);
        agent.current_work_item_id = Some(WORK_ITEM.into());
        agent.current_execution_binding = Some(WorkItemExecutionBinding {
            activation_id: Some("activation_context_snapshot".into()),
            admission_provenance: None,
            source_message_id: CURRENT_MESSAGE.into(),
            turn_id: CURRENT_TURN.into(),
            owner: Some(owner),
            work_item_id: Some(WORK_ITEM.into()),
            claimed_work_revision: Some(item.revision),
        });
        storage.write_agent_for_fixture(&agent)?;
        ensure!(storage.read_all_messages()?.len() == HISTORICAL_TURNS + 1);
        ensure!(storage.read_recent_turns(100)?.len() == HISTORICAL_TURNS);
        ensure!(storage.read_recent_briefs(100)?.len() == HISTORICAL_TURNS);
        ensure!(storage.read_recent_tool_executions(100)?.len() == HISTORICAL_TURNS);

        let profile = ExecutionProfile::default();
        let execution = ExecutionSnapshot {
            policy: profile.policy_snapshot(),
            profile,
            attached_workspaces: Vec::new(),
            workspace_id: None,
            workspace_anchor: workspace.clone(),
            execution_root: workspace.clone(),
            cwd: workspace,
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
            execution_roots: Vec::new(),
        };
        let config = ContextConfig {
            recent_messages: HISTORICAL_TURNS + 1,
            recent_briefs: HISTORICAL_TURNS,
            prompt_budget_estimated_tokens: FULL_BUDGET,
            turn_projection_budget_ratio: 0.5,
            turn_projection_min_budget: 0,
            turn_projection_max_budget: FULL_BUDGET,
            ..ContextConfig::default()
        };
        Ok(Self {
            storage,
            agent,
            execution,
            current_message,
            config,
            agent_home,
            _root: root,
        })
    }

    fn capture(&self) -> Result<ContextSnapshot> {
        capture_context_from_storage(
            &self.storage,
            &self.agent,
            &self.execution,
            &SkillsRuntimeView::default(),
            &self.current_message,
            None,
            &self.config,
            &self.agent_home,
            None,
        )
    }

    fn normalize(&self, value: &mut Value) {
        match value {
            Value::String(text) => {
                *text = text.replace(self.agent_home.to_string_lossy().as_ref(), "$FIXTURE");
            }
            Value::Array(values) => values.iter_mut().for_each(|value| self.normalize(value)),
            Value::Object(values) => values.values_mut().for_each(|value| self.normalize(value)),
            _ => {}
        }
    }
}

fn operator_message(text: String) -> MessageEnvelope {
    MessageEnvelope::new(
        AGENT,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:context-benchmark".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    )
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct SectionSummary {
    id: String,
    name: String,
    stability: PromptStability,
    normalized_content_bytes: usize,
    normalized_content_chars: usize,
    normalized_content_sha256: String,
}

fn section_summary(fixture: &Fixture, context: &BuiltContext) -> Vec<SectionSummary> {
    context
        .sections
        .iter()
        .map(|section| {
            let content = section
                .content
                .replace(fixture.agent_home.to_string_lossy().as_ref(), "$FIXTURE");
            SectionSummary {
                id: section.id.clone(),
                name: section.name.clone(),
                stability: section.stability,
                normalized_content_bytes: content.len(),
                normalized_content_chars: content.chars().count(),
                normalized_content_sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
            }
        })
        .collect()
}

#[derive(Serialize)]
struct Sample {
    iteration: usize,
    capture_wall_time_ns: u128,
    full_render_wall_time_ns: u128,
    smaller_budget_render_wall_time_ns: u128,
    operation_bundle_wall_time_ns: u128,
    acquisition: Value,
}

fn timing_summary(samples: &[Sample], value: impl Fn(&Sample) -> u128) -> Value {
    let mut values = samples.iter().map(value).collect::<Vec<_>>();
    values.sort_unstable();
    json!({
        "min_ns": values[0],
        "median_ns": values[values.len() / 2],
        "max_ns": values[values.len() - 1],
    })
}

fn main() -> Result<()> {
    let requested = std::env::var("HOLON_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("HOLON_BENCH_SAMPLES must be an integer >= 1")?
        .unwrap_or(DEFAULT_SAMPLES);
    ensure!(requested > 0, "at least one sample is required");
    let total_repetitions = requested
        .checked_add(WARMUP_SAMPLES)
        .context("sample count is too large")?;
    let fixture = Fixture::new()?;
    let mut full_baseline = None;
    let mut smaller_baseline = None;
    let mut descriptors_baseline = None;
    let mut samples = Vec::with_capacity(requested);

    for iteration in 0..total_repetitions {
        let bundle_started = Instant::now();
        let started = Instant::now();
        let snapshot = fixture.capture()?;
        let capture_wall_time_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        let full = build_context_from_snapshot(&snapshot, FULL_BUDGET)?;
        let full_render_wall_time_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        let smaller = build_context_from_snapshot(&snapshot, SMALLER_BUDGET)?;
        let smaller_budget_render_wall_time_ns = started.elapsed().as_nanos();
        let operation_bundle_wall_time_ns = bundle_started.elapsed().as_nanos();
        black_box((&snapshot, &full, &smaller));
        for context in [&full, &smaller] {
            ensure!(
                context.sections.iter().any(|section| {
                    section.id == "current_input" && section.content.contains(CURRENT_TEXT)
                }),
                "current input must survive both budgets"
            );
        }
        let full_summary = section_summary(&fixture, &full);
        let smaller_summary = section_summary(&fixture, &smaller);
        let mut descriptors = serde_json::to_value(snapshot.descriptors())?;
        fixture.normalize(&mut descriptors);
        if let Some(baseline) = full_baseline.as_ref() {
            ensure!(
                &full_summary == baseline,
                "full sections changed across captures"
            );
            ensure!(
                Some(&smaller_summary) == smaller_baseline.as_ref(),
                "smaller-budget sections changed across captures"
            );
            ensure!(
                Some(&descriptors) == descriptors_baseline.as_ref(),
                "source descriptors changed across captures"
            );
        } else {
            full_baseline = Some(full_summary);
            smaller_baseline = Some(smaller_summary);
            descriptors_baseline = Some(descriptors);
        }
        if iteration >= WARMUP_SAMPLES {
            samples.push(Sample {
                iteration: iteration - WARMUP_SAMPLES,
                capture_wall_time_ns,
                full_render_wall_time_ns,
                smaller_budget_render_wall_time_ns,
                operation_bundle_wall_time_ns,
                acquisition: serde_json::to_value(snapshot.acquisition())?,
            });
        }
    }

    let artifact = json!({
        "schema_version": "holon.context_snapshot.benchmark.v1",
        "benchmark_suite": "context_snapshot",
        "workload_version": "fixed-owned-history-v1",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "package_version": env!("CARGO_PKG_VERSION"),
        "debug_assertions": cfg!(debug_assertions),
        "network_mode": "none",
        "storage_adapter_still_performs_io": true,
        "resident_runtime_implemented": false,
        "config": {
            "warmup_repetitions": WARMUP_SAMPLES,
            "measured_repetitions": requested,
            "historical_turns": HISTORICAL_TURNS,
            "messages": HISTORICAL_TURNS + 1,
            "briefs": HISTORICAL_TURNS,
            "tool_execution_records": HISTORICAL_TURNS,
            "work_items": 1,
            "full_budget_estimated_tokens": FULL_BUDGET,
            "smaller_budget_estimated_tokens": SMALLER_BUDGET,
            "fixed_record_timestamp": "2024-01-02T03:04:05.123Z",
        },
        "section_summary": {
            "schema_version": "holon.context_snapshot.sections.v1",
            "path_normalization": "temporary fixture root replaced by $FIXTURE before hashing",
            "full": full_baseline,
            "smaller_budget": smaller_baseline,
        },
        "source_descriptors": descriptors_baseline,
        "summary": {
            "capture": timing_summary(&samples, |sample| sample.capture_wall_time_ns),
            "full_render": timing_summary(&samples, |sample| sample.full_render_wall_time_ns),
            "smaller_budget_render": timing_summary(&samples, |sample| sample.smaller_budget_render_wall_time_ns),
            "operation_bundle": timing_summary(&samples, |sample| sample.operation_bundle_wall_time_ns),
        },
        "samples": samples,
        "not_measured": {
            "sql_statement_count": null,
            "filesystem_read_count": null,
            "syscall_count": null,
            "resident_or_pinned_bytes": null,
            "provider_usage": null,
            "provider_cache_usage": null,
        },
        "limitations": [
            "Acquisition attribution counts instrumented logical source operations and returned records, not SQL statements, file reads, or syscalls; grouped operations can contain multiple lower-level reads.",
            "Capture uses the actual storage adapter on a warm local temporary database; it is not a memory-only runtime acquisition claim.",
            "Rendering and smaller-budget rendering use one captured input; no provider client is created, no tool command is executed, and no model network request is made.",
            "Section summaries and source descriptors are reproducibility checks, not new ProjectionManifest golden files or task-quality evaluation.",
            "Timing excludes fixture setup, validation, serialization, result destruction and cleanup; the operation bundle contains one capture and both renders, not one provider request.",
            "The fixed case does not measure wait, late-input admission, provider fallback, restart, growing history, contention, or production tail latency; those require separate tests and workloads.",
        ],
    });
    println!("{}", serde_json::to_string_pretty(&artifact)?);
    Ok(())
}
