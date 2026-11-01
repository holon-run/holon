//! Optimized, isolated Agent-state workload. See benchmarks/agent-state/README.md.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::{BTreeMap, BTreeSet},
    future::pending,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::future::try_join_all;
use holon::{
    config::AppConfig,
    diagnostics::{
        io_probe::{IoProbe, IoProbeReport, TimingKind},
        performance_snapshot,
    },
    host::RuntimeHost,
    provider::{
        AgentProvider, ConversationMessage, ModelBlock, ModelToolCallKind, ProviderTurnRequest,
        ProviderTurnResponse,
    },
    runtime::RuntimeHandle,
    runtime_db::{
        cold_fixture::{ColdFixtureEvidence, ColdFixtureWriter},
        RuntimeDb,
    },
    storage::AppStorage,
    types::{
        AdmissionContext, AgentState, AgentStatus, AuthorityClass, BriefContentSource,
        BriefContentSourceRelation, BriefKind, BriefRecord, ControlAction, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        QueueEntryRecord, QueueEntryStatus, ToolExecutionRecord, ToolExecutionStatus,
        TranscriptEntry, TranscriptEntryKind, TurnOwner, TurnRecord, TurnTerminalKind,
        TurnTerminalSummary, TurnTriggerSummary, WorkItemPlanStatus, WorkItemRecord, WorkItemState,
    },
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{Notify, Semaphore};

const HISTORY_MESSAGES: usize = 100_000;
const LINKED_TURNS: usize = 64;
const PENDING_INPUTS: usize = 100;
const PENDING_BODY_BYTES: usize = 16 * 1024;
const TOOL_STDOUT_BYTES: usize = 10 * 1024 * 1024;
const PREVIEW_ROUNDS: usize = 5;
const FIXTURE_SEED: &str = "holon-agent-state-d0-v1";

// Measures Rust requested allocation bytes, not SQLite C allocations or RSS.
struct CountingAllocator;
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegated with the caller's allocation contract unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed)
                + layout.size() as u64;
            PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: delegated with the caller's deallocation contract unchanged.
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: delegated with the caller's reallocation contract unchanged.
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            let live = if size >= layout.size() {
                LIVE_BYTES.fetch_add((size - layout.size()) as u64, Ordering::Relaxed)
                    + (size - layout.size()) as u64
            } else {
                LIVE_BYTES.fetch_sub((layout.size() - size) as u64, Ordering::Relaxed)
                    - (layout.size() - size) as u64
            };
            PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy, Serialize)]
struct Case {
    name: &'static str,
    loaded_agents: usize,
    concurrent_requests: usize,
}

impl Case {
    fn selected() -> Result<Self> {
        match std::env::var("HOLON_STATE_BENCH_CASE")
            .as_deref()
            .unwrap_or("normal")
        {
            "normal" => Ok(Self {
                name: "normal",
                loaded_agents: 8,
                concurrent_requests: 4,
            }),
            "pressure" => Ok(Self {
                name: "pressure",
                loaded_agents: 32,
                concurrent_requests: 16,
            }),
            other => {
                anyhow::bail!("unknown HOLON_STATE_BENCH_CASE {other}; use normal or pressure")
            }
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct SamplingPolicy {
    warmup_requests_per_agent: usize,
    measured_responses_per_agent: usize,
}

impl SamplingPolicy {
    fn selected(case: Case) -> Result<Self> {
        let read = |name: &str, fallback: usize| -> Result<usize> {
            let value = std::env::var(name)
                .ok()
                .map(|value| value.parse::<usize>())
                .transpose()?
                .unwrap_or(fallback);
            ensure!(
                value > 0 && value <= 1024,
                "{name} must be between 1 and 1024"
            );
            Ok(value)
        };
        Ok(Self {
            warmup_requests_per_agent: read(
                "HOLON_STATE_BENCH_WARMUP_REQUESTS",
                100usize.div_ceil(case.loaded_agents),
            )?,
            measured_responses_per_agent: read(
                "HOLON_STATE_BENCH_MEASURED_REQUESTS",
                1000usize.div_ceil(case.loaded_agents),
            )?,
        })
    }
}

#[derive(Clone, Serialize)]
struct ProviderArrival {
    agent_id: String,
    ordinal: usize,
    phase: &'static str,
    request_kind: &'static str,
    prompt_bytes: usize,
    request_visible_bytes: usize,
    conversation_messages: usize,
    current_operator_root_present: bool,
    tool_count: usize,
    prompt_shape_sha256: String,
}

#[derive(Default)]
struct ProviderState {
    counts: BTreeMap<String, usize>,
    warm_ready: BTreeSet<String>,
    held: BTreeSet<String>,
    arrivals: Vec<ProviderArrival>,
}

struct WorkloadProvider {
    state: Mutex<ProviderState>,
    sampling: SamplingPolicy,
    changed: Notify,
    measured_admission: Semaphore,
}

impl WorkloadProvider {
    fn new(sampling: SamplingPolicy) -> Self {
        Self {
            state: Mutex::new(ProviderState::default()),
            sampling,
            changed: Notify::new(),
            measured_admission: Semaphore::new(0),
        }
    }

    async fn wait_group(&self, ids: &[String], held: bool) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(600), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let ready = {
                    let state = self.state.lock().expect("provider state poisoned");
                    let ready = if held { &state.held } else { &state.warm_ready };
                    ids.iter().all(|id| ready.contains(id))
                };
                if ready {
                    return;
                }
                notified.await;
            }
        })
        .await
        .with_context(|| {
            let state = self.state.lock().expect("provider state poisoned");
            let progress = ids
                .iter()
                .map(|id| {
                    json!({
                        "agent_id": id,
                        "requests": state.counts.get(id).copied().unwrap_or(0),
                        "warm_ready": state.warm_ready.contains(id),
                        "held": state.held.contains(id),
                    })
                })
                .collect::<Vec<_>>();
            format!(
                "scripted group failed to reach request barrier: held={held} progress={progress:?}"
            )
        })
    }

    fn arrivals(&self, ids: &[String]) -> Vec<ProviderArrival> {
        self.state
            .lock()
            .expect("provider state poisoned")
            .arrivals
            .iter()
            .filter(|arrival| ids.contains(&arrival.agent_id))
            .cloned()
            .collect()
    }
}

#[async_trait]
impl AgentProvider for WorkloadProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let agent_id = request
            .prompt_frame
            .cache
            .as_ref()
            .map(|cache| cache.agent_id.clone())
            .context("scripted request missing agent cache identity")?;
        let tools = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        let has_get_agent = tools.contains(&"GetAgent");
        let benchmark_tool_result = request
            .conversation
            .iter()
            .rev()
            .find_map(|message| match message {
                ConversationMessage::UserToolResults(results) => Some(
                    results
                        .iter()
                        .filter(|result| result.tool_use_id.starts_with("d0-get-agent-"))
                        .collect::<Vec<_>>(),
                ),
                ConversationMessage::AssistantBlocks(_) | ConversationMessage::UserBlocks(_) => {
                    None
                }
                _ => None,
            })
            .unwrap_or_default();
        // Only the final conversation message's tool results establish reentry;
        // older history must not turn a new dispatch into a tool follow-up.
        let follows_get_agent = matches!(request.conversation.iter().rev()
            .find(|message| !matches!(message, ConversationMessage::UserBlocks(_))),
            Some(ConversationMessage::UserToolResults(results)) if results.iter()
                .any(|result| result.tool_use_id.starts_with("d0-get-agent-")));
        if follows_get_agent {
            ensure!(
                benchmark_tool_result.len() == 1 && !benchmark_tool_result[0].is_error,
                "scripted GetAgent must produce one successful current-agent result"
            );
        }
        let request_kind = if follows_get_agent {
            "get_agent_continuation"
        } else if has_get_agent {
            "get_agent_initial"
        } else {
            "delivery"
        };
        let shape = json!({
            "system_stabilities": request.prompt_frame.system_blocks.iter().map(|block| block.stability).collect::<Vec<_>>(),
            "context_stabilities": request.prompt_frame.context_blocks.iter().map(|block| block.stability).collect::<Vec<_>>(),
            "tools": tools,
        });
        let prompt_bytes = request.prompt_frame.system_prompt.len()
            + request
                .prompt_frame
                .context_blocks
                .iter()
                .map(|block| block.text.len())
                .sum::<usize>();
        let conversation_bytes = request
            .conversation
            .iter()
            .map(|message| match message {
                ConversationMessage::UserText(text) => Ok(text.len()),
                ConversationMessage::UserBlocks(blocks) => {
                    Ok(blocks.iter().map(|block| block.text.len()).sum())
                }
                ConversationMessage::UserImage {
                    prompt,
                    data_base64,
                    ..
                } => Ok(prompt.len() + data_base64.len()),
                ConversationMessage::AssistantBlocks(blocks) => {
                    serde_json::to_vec(blocks).map(|body| body.len())
                }
                ConversationMessage::UserToolResults(results) => {
                    serde_json::to_vec(results).map(|body| body.len())
                }
            })
            .collect::<std::result::Result<Vec<usize>, _>>()?
            .into_iter()
            .sum::<usize>();
        let request_visible_bytes = prompt_bytes + conversation_bytes;
        ensure!(
            request_visible_bytes < TOOL_STDOUT_BYTES,
            "full 10 MiB stdout leaked into provider request"
        );
        let pending_marker = format!("D0_INPUT d0-pending-{agent_id}-");
        let current_operator_root_present =
            request.conversation.iter().any(|message| match message {
                ConversationMessage::UserBlocks(blocks) => blocks.iter().any(|block| {
                    block
                        .text
                        .split_once("## current_input\n")
                        .is_some_and(|(_, input)| {
                            input
                                .split("\n## ")
                                .next()
                                .unwrap_or(input)
                                .contains(&pending_marker)
                        })
                }),
                _ => false,
            });
        ensure!(current_operator_root_present,
            "provider request dropped the current pending operator root or exhausted the fixed backlog");
        let shape_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&shape)?));
        let (ordinal, phase) = {
            let mut state = self.state.lock().expect("provider state poisoned");
            let count = state.counts.entry(agent_id.clone()).or_default();
            *count += 1;
            let ordinal = *count;
            let phase = if ordinal <= self.sampling.warmup_requests_per_agent {
                "warmup"
            } else if ordinal == self.sampling.warmup_requests_per_agent + 1 {
                "warmup_barrier"
            } else if ordinal
                <= self.sampling.warmup_requests_per_agent
                    + 1
                    + self.sampling.measured_responses_per_agent
            {
                "measured"
            } else {
                "hold"
            };
            state.arrivals.push(ProviderArrival {
                agent_id: agent_id.clone(),
                ordinal,
                phase,
                request_kind,
                prompt_bytes,
                request_visible_bytes,
                conversation_messages: request.conversation.len(),
                current_operator_root_present,
                tool_count: request.tools.len(),
                prompt_shape_sha256: shape_digest,
            });
            if phase == "warmup_barrier" {
                state.warm_ready.insert(agent_id.clone());
            }
            if phase == "hold" {
                state.held.insert(agent_id.clone());
            }
            (ordinal, phase)
        };
        // Do not retain another copy of an entire request at the benchmark barrier.
        drop(request);
        self.changed.notify_waiters();
        if phase == "warmup_barrier" {
            self.measured_admission
                .acquire()
                .await
                .context("measurement gate closed")?
                .forget();
        } else if phase == "hold" {
            return pending::<Result<ProviderTurnResponse>>().await;
        }
        let blocks = if request_kind == "get_agent_initial" {
            vec![ModelBlock::ToolUse {
                id: format!("d0-get-agent-{agent_id}-{ordinal}"),
                name: "GetAgent".into(),
                input: json!({"agent_id": agent_id, "detail": "compact"}),
                kind: ModelToolCallKind::Function,
                provider_data: None,
            }]
        } else {
            vec![ModelBlock::Text {
                text: "D0 scripted result: preserve input ownership and bounded evidence.".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

struct Fixture {
    host: RuntimeHost,
    runtimes: Vec<RuntimeHandle>,
    ids: Vec<String>,
    provider: Arc<WorkloadProvider>,
    _home: TempDir,
}

fn operator_message(agent_id: &str, id: String, text: String) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:d0-synthetic".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    message.id = id;
    message
}

fn pending_text(id: &str) -> String {
    let mut text = format!("D0_INPUT {id}: continue the same bounded verification.\n");
    text.push_str(&"x".repeat(PENDING_BODY_BYTES - text.len()));
    text
}

fn history_message(
    agent_id: &str,
    work_id: &str,
    index: usize,
    timestamp: DateTime<Utc>,
) -> MessageEnvelope {
    let mut message = operator_message(
        agent_id,
        format!("d0-history-{agent_id}-{index:06}"),
        format!(
            "{FIXTURE_SEED} historical observation {index:06}: {}",
            "evidence ".repeat(12)
        ),
    );
    message.message_seq = Some(index as u64 + 1);
    message.created_at = timestamp + chrono::Duration::milliseconds(index as i64);
    if index >= HISTORY_MESSAGES - LINKED_TURNS {
        message.turn_id = Some(format!("d0-turn-{agent_id}-{index:06}"));
        message.work_item_id = Some(work_id.to_owned());
    }
    message
}

fn message_fixture_columns(
    message: &MessageEnvelope,
) -> Result<(String, String, String, String, String)> {
    let payload = serde_json::to_string(message)?;
    let hash = format!("sha256:{:x}", Sha256::digest(payload.as_bytes()));
    let preview = serde_json::to_string(&message.body)?;
    // Every historical body is short ASCII; match evidence_preview without a
    // second truncation implementation. The 16 KiB roots use canonical APIs.
    ensure!(
        preview.is_ascii() && preview.len() < 2048,
        "bulk fixture preview exceeds canonical limit"
    );
    let kind = serde_json::to_value(&message.kind)?
        .as_str()
        .context("message kind must be a string")?
        .to_owned();
    let created_at = message
        .created_at
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    Ok((payload, hash, preview, kind, created_at))
}

fn seed_history_messages(
    db: &RuntimeDb,
    writer: &ColdFixtureWriter<'_>,
    agent_id: &str,
    work_id: &str,
    timestamp: DateTime<Utc>,
) -> Result<()> {
    let first = history_message(agent_id, work_id, 0, timestamp);
    let before = db.connection()?.query_row(
        "SELECT count(*) FROM messages WHERE agent_id = ?1",
        [agent_id],
        |row| row.get::<_, usize>(0),
    )?;
    ensure!(
        before == 0,
        "cold fixture unexpectedly contains messages before seeding"
    );
    // Anchor the fast setup template to one actual repository insertion. This
    // catches serialization/hash/preview/timestamp/schema drift before bulk I/O.
    writer.insert(ColdFixtureEvidence::Message(&first))?;
    let expected = message_fixture_columns(&first)?;
    let connection = db.connection()?;
    let canonical = connection.query_row(
        "SELECT payload_json, content_hash, preview, kind, created_at FROM messages WHERE evidence_id = ?1",
        [&first.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?)))?;
    ensure!(
        canonical == expected,
        "bulk fixture columns differ from canonical repository output"
    );
    let canonical_source = connection.query_row(
        "SELECT source_kind, agent_id, turn_id, revision FROM conversation_source_revisions WHERE source_id = ?1",
        [&first.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, u64>(3)?)))?;
    ensure!(
        canonical_source == ("operator".to_owned(), agent_id.to_owned(), None, 1),
        "bulk fixture source revision differs from fresh canonical insertion"
    );
    drop(connection);
    // Typed, insert-only batches retain prepared SQL/source sequence allocation,
    // but qualification and Native Input births now commit with each insert.
    for start in (1..HISTORY_MESSAGES).step_by(128) {
        let messages = (start..(start + 128).min(HISTORY_MESSAGES))
            .map(|index| history_message(agent_id, work_id, index, timestamp))
            .collect::<Vec<_>>();
        writer.insert_unassigned_messages(&messages)?;
    }
    let connection = db.connection()?;
    let first_activity = connection.query_row(
        "SELECT activity_seq FROM conversation_source_revisions WHERE source_kind = 'operator' AND source_id = ?1",
        [&first.id], |row| row.get::<_, i64>(0))?;
    let indexed_count = connection.query_row(
        "SELECT count(*) FROM messages WHERE agent_id = ?1
         AND evidence_id = ?2 || printf('%06d', message_seq - 1)
         AND message_id = evidence_id AND message_seq BETWEEN 1 AND ?3
         AND task_id IS NULL AND content_ref IS NULL AND kind = ?4
         AND turn_id IS CASE WHEN message_seq > ?5 THEN ?6 || printf('%06d', message_seq - 1) ELSE NULL END
         AND work_item_id IS CASE WHEN message_seq > ?5 THEN ?7 ELSE NULL END",
        rusqlite::params![agent_id, format!("d0-history-{agent_id}-"), HISTORY_MESSAGES as i64,
            expected.3, (HISTORY_MESSAGES - LINKED_TURNS) as i64, format!("d0-turn-{agent_id}-"), work_id],
        |row| row.get::<_, usize>(0))?;
    let unique_sequences = connection.query_row(
        "SELECT count(DISTINCT message_seq) FROM messages WHERE agent_id = ?1",
        [agent_id],
        |row| row.get::<_, usize>(0),
    )?;
    ensure!(
        indexed_count == HISTORY_MESSAGES && unique_sequences == HISTORY_MESSAGES,
        "bulk history lost normalized IDs, scope, linkage, or message sequence"
    );
    let source_count = connection.query_row(
        "SELECT count(*) FROM conversation_source_revisions s JOIN messages m ON m.evidence_id = s.source_id
         WHERE s.source_kind = 'operator' AND s.agent_id = ?1 AND m.agent_id = ?1
         AND s.revision = 1 AND s.turn_id IS NULL AND s.updated_at = m.created_at
         AND s.activity_seq = ?2 + m.message_seq - 1", rusqlite::params![agent_id, first_activity],
        |row| row.get::<_, usize>(0))?;
    ensure!(
        source_count == HISTORY_MESSAGES,
        "bulk history lost canonical source revision scope, sequence, or timestamp"
    );
    let sequence = connection.query_row(
        "SELECT last_value FROM runtime_sequences WHERE domain = 'message' AND scope_key = ?1",
        [format!("agent:{agent_id}")],
        |row| row.get::<_, usize>(0),
    )?;
    ensure!(
        sequence == HISTORY_MESSAGES,
        "bulk history lost runtime message sequence"
    );
    drop(connection);
    for index in [
        0,
        HISTORY_MESSAGES / 2,
        HISTORY_MESSAGES - LINKED_TURNS,
        HISTORY_MESSAGES - 1,
    ] {
        let expected_message = history_message(agent_id, work_id, index, timestamp);
        let decoded = db
            .messages()
            .by_id(Some(agent_id), &expected_message.id)?
            .context("bulk history message missing")?;
        ensure!(
            serde_json::to_value(&decoded)? == serde_json::to_value(&expected_message)?,
            "bulk history failed canonical repository roundtrip"
        );
        let expected_columns = message_fixture_columns(&expected_message)?;
        let actual_columns = db.connection()?.query_row(
            "SELECT payload_json, content_hash, preview, kind, created_at FROM messages WHERE evidence_id = ?1",
            [&expected_message.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?)))?;
        ensure!(
            actual_columns == expected_columns,
            "bulk history hash, preview, payload, or timestamp differs"
        );
    }
    ensure!(
        db.messages().count(Some(agent_id))? == HISTORY_MESSAGES
            && db.messages().max_message_seq(Some(agent_id))? == HISTORY_MESSAGES as u64,
        "bulk history failed repository count/sequence readback"
    );
    Ok(())
}

fn seed_history(storage: &AppStorage, db: &RuntimeDb, agent_id: &str) -> Result<()> {
    let writer = ColdFixtureWriter::new(db, agent_id)?;
    let timestamp: DateTime<Utc> = "2024-01-02T03:04:05.000Z".parse()?;
    let work_id = format!("d0-work-{agent_id}");
    let mut work = WorkItemRecord::new(
        agent_id,
        "D0 ongoing bounded verification",
        WorkItemState::Open,
    );
    work.id = work_id.clone();
    work.plan_status = WorkItemPlanStatus::Ready;
    work.created_at = timestamp;
    work.updated_at = timestamp;
    writer.insert_work(&work)?;
    let plan_dir = storage.data_dir().join("work-items").join(&work_id);
    std::fs::create_dir_all(&plan_dir)?;
    std::fs::write(
        plan_dir.join("plan.md"),
        "D0: verify fixed observations, retain ownership, measure every read.\n",
    )?;
    seed_history_messages(db, &writer, agent_id, &work_id, timestamp)?;
    for index in HISTORY_MESSAGES - LINKED_TURNS..HISTORY_MESSAGES {
        let turn_id = format!("d0-turn-{agent_id}-{index:06}");
        let transcript_id = format!("d0-transcript-{agent_id}-{index:06}");
        let message_id = format!("d0-history-{agent_id}-{index:06}");
        let message = storage
            .read_message_by_id(&message_id)?
            .context("history message missing")?;
        let mut brief = BriefRecord::new(
            agent_id,
            BriefKind::Result,
            format!("D0 historical observation {index} verified."),
            Some(message_id.clone()),
            None,
        );
        brief.id = format!("d0-brief-{agent_id}-{index:06}");
        brief.created_at = message.created_at;
        brief.turn_id = Some(turn_id.clone());
        brief.turn_index = Some(index as u64 + 1);
        brief.work_item_id = Some(work_id.clone());
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: transcript_id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        writer.insert(ColdFixtureEvidence::Brief(&brief))?;
        let mut turn = TurnRecord::new(agent_id, turn_id.clone(), index as u64 + 1);
        turn.created_at = message.created_at;
        turn.current_work_item_id = Some(work_id.clone());
        turn.owner = Some(TurnOwner::WorkItem {
            work_item_id: work_id.clone(),
        });
        turn.trigger = Some(TurnTriggerSummary::from_message(&message));
        turn.input_message_ids = vec![message_id.clone()];
        turn.produced_brief_ids = vec![brief.id.clone()];
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: message.created_at,
            duration_ms: 1,
        });
        if index == HISTORY_MESSAGES - 1 {
            let tool_id = format!("d0-tool-stdout-{agent_id}");
            writer.insert(ColdFixtureEvidence::Tool(&ToolExecutionRecord {
                id: tool_id.clone(), agent_id: agent_id.into(), work_item_id: Some(work_id.clone()),
                turn_index: index as u64 + 1, turn_id: Some(turn_id.clone()), tool_name: "ExecCommand".into(),
                created_at: message.created_at, completed_at: Some(message.created_at), duration_ms: 1,
                authority_class: AuthorityClass::RuntimeInstruction, status: ToolExecutionStatus::Success,
                input: json!({"cmd": "synthetic; no process was executed"}),
                output: json!({"stdout": "z".repeat(TOOL_STDOUT_BYTES), "stdout_preview": "D0_STDOUT_PREVIEW", "exit_code": 0}),
                summary: "D0_STDOUT_PREVIEW: 10 MiB synthetic stdout, bounded visible receipt".into(),
                invocation_surface: Some("commentary".into()),
            }))?;
            turn.tool_execution_ids = vec![tool_id];
        }
        writer.insert(ColdFixtureEvidence::Turn(&turn))?;
        let mut transcript = TranscriptEntry::new(
            agent_id,
            TranscriptEntryKind::AssistantRound,
            Some(1),
            Some(message_id),
            json!({"turn_id": turn_id, "blocks": [{"type": "text", "text": brief.text}]}),
        );
        transcript.id = transcript_id;
        transcript.created_at = message.created_at;
        writer.insert(ColdFixtureEvidence::Transcript(&transcript))?;
    }
    for index in 0..PENDING_INPUTS {
        let id = format!("d0-pending-{agent_id}-{index:03}");
        let mut message = operator_message(agent_id, id.clone(), pending_text(&id));
        message.message_seq = Some((HISTORY_MESSAGES + index + 1) as u64);
        message.created_at = timestamp + chrono::Duration::seconds(200_000 + index as i64);
        message.work_item_id = Some(work_id.clone());
        writer.insert_pending(
            &message,
            &QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: agent_id.into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: message.created_at,
            },
        )?;
    }
    let mut state = AgentState::new(agent_id);
    state.status = AgentStatus::Stopped;
    state.pending = PENDING_INPUTS;
    state.total_message_count = HISTORY_MESSAGES + PENDING_INPUTS;
    state.turn_index = HISTORY_MESSAGES as u64;
    state.current_work_item_id = Some(work_id);
    writer.finish_state(&state)?;
    let connection = db.connection()?;
    let count: usize = connection.query_row(
        "SELECT count(*) FROM messages WHERE agent_id = ?1",
        [agent_id],
        |row| row.get(0),
    )?;
    ensure!(
        count == HISTORY_MESSAGES + PENDING_INPUTS,
        "history/pending fixture row count mismatch"
    );
    Ok(())
}

impl Fixture {
    async fn new(case: Case, sampling: SamplingPolicy) -> Result<Self> {
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        // Never scan the operator's own resources or use their provider credentials.
        config.user_home_dir = None;
        config.prompt_budget_estimated_tokens = 8192;
        config.compaction_trigger_messages = usize::MAX;
        config.compaction_trigger_estimated_tokens = usize::MAX;
        let provider = Arc::new(WorkloadProvider::new(sampling));
        // Service shutdown drains resources but does not imply business Stop.
        // Stop each empty runtime before draining the setup Host.
        let setup_host = RuntimeHost::new_with_provider(config.clone(), provider.clone())?;
        let ids = (0..case.loaded_agents)
            .map(|index| format!("d0-agent-{index:03}"))
            .collect::<Vec<_>>();
        for id in &ids {
            setup_host.create_named_agent(id, None).await?;
        }
        for id in &ids {
            setup_host
                .control_public_agent(id, ControlAction::Stop)
                .await
                .map_err(anyhow::Error::new)?;
        }
        setup_host.shutdown().await?;
        for id in &ids {
            let started = Instant::now();
            seed_history(&setup_host.agent_storage(id)?, setup_host.runtime_db(), id)?;
            eprintln!("D0 seeded {id}: {HISTORY_MESSAGES} history messages, {PENDING_INPUTS} pending, {:?}", started.elapsed());
        }
        drop(setup_host);
        let host = RuntimeHost::new_with_provider(config, provider.clone())?;
        let mut runtimes = Vec::with_capacity(case.loaded_agents);
        for id in &ids {
            let runtime = host.get_or_create_agent(id).await?;
            let state = runtime.agent_state().await?;
            ensure!(state.pending == PENDING_INPUTS && state.status == AgentStatus::Stopped,
                "fixture activation changed accepted pending input: agent={id} pending={} status={:?}", state.pending, state.status);
            runtimes.push(runtime);
        }
        Ok(Self {
            host,
            runtimes,
            ids,
            provider,
            _home: home,
        })
    }
}

#[derive(Serialize)]
struct OperationSample {
    agent_index: usize,
    iteration: usize,
    wall_ns: u128,
    rendered_context_bytes: usize,
}

fn quantiles(values: impl IntoIterator<Item = u128>) -> Value {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_unstable();
    if values.is_empty() {
        return Value::Null;
    }
    let percentile = |p: usize| values[(values.len() * p).div_ceil(100) - 1];
    json!({"count": values.len(), "min_ns": values[0], "p50_ns": percentile(50), "p95_ns": percentile(95), "p99_ns": percentile(99), "max_ns": values[values.len()-1], "method": "nearest_rank"})
}

fn timing_summary(report: &IoProbeReport) -> Value {
    json!({
        "context_preparation_to_request": quantiles(report.timing_samples.iter()
            .filter(|sample| sample.kind == TimingKind::ContextPreparationToRequest)
            .map(|sample| u128::from(sample.elapsed_micros) * 1000)),
        "commit_publication": quantiles(report.timing_samples.iter()
            .filter(|sample| sample.kind == TimingKind::CommitPublication)
            .map(|sample| u128::from(sample.elapsed_micros) * 1000)),
    })
}

fn memory_snapshot() -> Value {
    // SAFETY: getrusage initializes the supplied plain C value on success.
    let peak_rss_bytes = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
            let value = usage.ru_maxrss.max(0) as u64;
            Some(if cfg!(target_os = "macos") {
                value
            } else {
                value * 1024
            })
        } else {
            None
        }
    };
    json!({
        "rust_requested_live_bytes": LIVE_BYTES.load(Ordering::Relaxed),
        "rust_requested_phase_peak_bytes": PEAK_BYTES.load(Ordering::Relaxed),
        "process_lifetime_peak_rss_bytes": peak_rss_bytes,
        "resident_domain_bytes": null, "snapshot_pinned_bytes": null,
        "allocation_accounting": "System GlobalAlloc requested layouts; excludes SQLite C allocations and allocator overhead",
    })
}

fn reset_memory_peak() {
    PEAK_BYTES.store(LIVE_BYTES.load(Ordering::Relaxed), Ordering::Relaxed);
}

async fn preview_round(
    fixture: &Fixture,
    concurrency: usize,
    iteration: usize,
) -> Result<Vec<OperationSample>> {
    let permits = Arc::new(Semaphore::new(concurrency));
    try_join_all(
        fixture
            .runtimes
            .iter()
            .cloned()
            .enumerate()
            .map(|(agent_index, runtime)| {
                let permits = permits.clone();
                async move {
                    let _permit = permits.acquire_owned().await?;
                    let text = pending_text("D0_PREVIEW_ROOT");
                    let started = Instant::now();
                    let prompt = runtime
                        .preview_prompt(text, AuthorityClass::OperatorInstruction)
                        .await?;
                    let wall_ns = started.elapsed().as_nanos();
                    ensure!(
                        prompt
                            .rendered_context_attachment
                            .contains("D0_PREVIEW_ROOT"),
                        "preview dropped mandatory operator root"
                    );
                    ensure!(
                        prompt.rendered_context_attachment.len() < TOOL_STDOUT_BYTES,
                        "preview materialized full stdout into prompt"
                    );
                    ensure!(
                        prompt
                            .rendered_context_attachment
                            .contains("D0_STDOUT_PREVIEW"),
                        "preview did not select the bounded large-stdout receipt"
                    );
                    Ok::<_, anyhow::Error>(OperationSample {
                        agent_index,
                        iteration,
                        wall_ns,
                        rendered_context_bytes: prompt.rendered_context_attachment.len(),
                    })
                }
            }),
    )
    .await
}

async fn measure_preview(fixture: &Fixture, case: Case, probe: &mut IoProbe) -> Result<Value> {
    preview_round(fixture, case.concurrent_requests, 0).await?;
    probe.reset();
    reset_memory_peak();
    let before_memory = memory_snapshot();
    let before_diagnostics = performance_snapshot();
    let started = Instant::now();
    let mut samples = Vec::new();
    for iteration in 0..PREVIEW_ROUNDS {
        samples.extend(preview_round(fixture, case.concurrent_requests, iteration).await?);
    }
    let elapsed = started.elapsed();
    let io = probe.snapshot();
    let after_memory = memory_snapshot();
    Ok(json!({
        "scope": "loaded_runtime_preview_acquisition_and_render; no provider request or WorkItemScoped runtime adoption",
        "wall_ns": elapsed.as_nanos(), "operations": samples.len(),
        "operations_per_second": samples.len() as f64 / elapsed.as_secs_f64(),
        "latency": quantiles(samples.iter().map(|sample| sample.wall_ns)), "samples": samples,
        "actual_io": io, "memory_before": before_memory, "memory_after": after_memory,
        "logical_stages_before": before_diagnostics, "logical_stages_after": performance_snapshot(),
    }))
}

async fn measure_enqueue(fixture: &Fixture, case: Case, probe: &mut IoProbe) -> Result<Value> {
    probe.reset();
    reset_memory_peak();
    let started = Instant::now();
    let permits = Arc::new(Semaphore::new(case.concurrent_requests));
    let samples = try_join_all(fixture.runtimes.iter().cloned().enumerate().map(
        |(index, runtime)| {
            let permits = permits.clone();
            let id = fixture.ids[index].clone();
            let db = fixture.host.runtime_db();
            async move {
                let _permit = permits.acquire_owned().await?;
                let message_id = format!("d0-measured-enqueue-{id}");
                let message = operator_message(&id, message_id.clone(), pending_text(&message_id));
                let started = Instant::now();
                let accepted = runtime.enqueue(message).await?;
                let wall_ns = started.elapsed().as_nanos();
                ensure!(accepted.id == message_id, "enqueue changed input identity: expected={message_id} actual={}", accepted.id);
                let state = runtime.agent_state().await?;
                let entry = db.queue_entries().latest(&message_id)?;
                let pending_entries = db.queue_entries().queued_for_agent(&id)?;
                let actual_status = entry.as_ref().map(|record| &record.status);
                ensure!(state.pending == PENDING_INPUTS
                    && state.status == AgentStatus::Stopped
                    && pending_entries.len() == PENDING_INPUTS
                    && actual_status == Some(&QueueEntryStatus::Aborted),
                    "stopped admission ACK mismatch: agent={id} message={message_id} live_pending={} live_status={:?} durable_pending_queue={} new_message_queue_status={actual_status:?}; expected pending={} status=Stopped durable_queue={} new_message=Aborted; live queue allocation not directly observed",
                    state.pending, state.status, pending_entries.len(), PENDING_INPUTS, PENDING_INPUTS);
                Ok::<_, anyhow::Error>(OperationSample {
                    agent_index: index,
                    iteration: 0,
                    wall_ns,
                    rendered_context_bytes: 0,
                })
            }
        },
    ))
    .await?;
    let elapsed = started.elapsed();
    Ok(json!({
        "scope": "Stopped discard admission through durable ACK and state publication; new message Aborted, original accepted queue retained; verification outside per-operation timer",
        "contract": {"status": "stopped", "disposition": "aborted", "pending_after_ack": PENDING_INPUTS,
            "durable_pending_queue": PENDING_INPUTS, "live_queue_directly_observed": false},
        "actual_io_includes_verification": true,
        "phase_wall_includes_verification": true,
        "operation_latency_excludes_verification": true,
        "wall_ns": elapsed.as_nanos(), "operations": samples.len(),
        "operations_per_second": samples.len() as f64 / elapsed.as_secs_f64(),
        "latency": quantiles(samples.iter().map(|sample| sample.wall_ns)), "samples": samples,
        "actual_io": probe.snapshot(), "timings": timing_summary(&probe.snapshot()), "memory": memory_snapshot(),
    }))
}

async fn measure_scripted(
    fixture: &mut Fixture,
    case: Case,
    sampling: SamplingPolicy,
    probe: &mut IoProbe,
) -> Result<Value> {
    let mut groups = Vec::new();
    for start in (0..fixture.ids.len()).step_by(case.concurrent_requests) {
        let ids = fixture.ids[start..start + case.concurrent_requests].to_vec();
        let replacements = try_join_all(ids.iter().map(|id| async {
            fixture
                .host
                .control_public_agent(id, ControlAction::Start)
                .await
                .map_err(anyhow::Error::new)
        }))
        .await?;
        // Start may replace a stopped runtime. Release the retired handle rather
        // than counting two generations as one loaded Agent in memory samples.
        for (offset, runtime) in replacements.into_iter().enumerate() {
            fixture.runtimes[start + offset] = runtime;
        }
        fixture.provider.wait_group(&ids, false).await?;
        // The warmup barrier request is already built; its response releases measured rounds.
        probe.reset();
        reset_memory_peak();
        let before_memory = memory_snapshot();
        let started = Instant::now();
        fixture.provider.measured_admission.add_permits(ids.len());
        fixture.provider.wait_group(&ids, true).await?;
        let elapsed = started.elapsed();
        let arrivals = fixture.provider.arrivals(&ids);
        let measured_constructed = arrivals
            .iter()
            .filter(|arrival| matches!(arrival.phase, "measured" | "hold"))
            .count();
        ensure!(
            measured_constructed == ids.len() * (sampling.measured_responses_per_agent + 1),
            "scripted measured request count mismatch"
        );
        let io = probe.snapshot();
        let memory_at_hold = memory_snapshot();
        // Stop is deliberately outside latency/throughput/read intervals. It cancels the held provider future.
        try_join_all(ids.iter().map(|id| async {
            fixture
                .host
                .control_public_agent(id, ControlAction::Stop)
                .await
                .map_err(anyhow::Error::new)
        }))
        .await?;
        groups.push(json!({
            "agent_ids": ids, "wall_ns": elapsed.as_nanos(),
            "measured_constructed_requests": measured_constructed,
            "constructed_requests_per_second": measured_constructed as f64 / elapsed.as_secs_f64(),
            "scripted_response_count": ids.len() * sampling.measured_responses_per_agent,
            "actual_io": io, "timings": timing_summary(&io), "provider_arrivals": arrivals,
            "memory_before": before_memory, "memory_at_hold": memory_at_hold,
            "memory_after_stop": memory_snapshot(),
        }));
    }
    Ok(json!({
        "scope": "actual scheduler, owner selector, context/resources and provider request construction; no network",
        "warmup_requests_per_agent": sampling.warmup_requests_per_agent,
        "warmup_barrier_requests_per_agent": 1,
        "measured_responses_per_agent": sampling.measured_responses_per_agent,
        "scripted_tool": "GetAgent current agent only, one call per dispatch",
        "held_constructed_requests_per_agent": 1,
        "groups": groups,
        "limitations": ["Throughput counts constructed requests, including the final held request, not completed tasks or provider tokens.", "The warmup barrier response and its postcommit work are inside the interval; its already-built request is excluded.", "Process-wide SQL includes business commit validation; it is not per-request context SQL attribution.", "Loaded preview is a separate diagnostic scope and does not replace actual dispatch coverage."],
    }))
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> Result<()> {
    ensure!(
        !cfg!(debug_assertions),
        "D0 contractual workload requires cargo bench/release, not debug"
    );
    let case = Case::selected()?;
    let sampling = SamplingPolicy::selected(case)?;
    // Start before opening RuntimeDb so its long-lived writer connection receives the SQL observer.
    let mut probe = IoProbe::start();
    let setup_started = Instant::now();
    let mut fixture = Fixture::new(case, sampling).await?;
    let setup_wall_ns = setup_started.elapsed().as_nanos();
    let setup_io = probe.snapshot();
    let setup_memory = memory_snapshot();
    let preview = measure_preview(&fixture, case, &mut probe).await?;
    let enqueue = measure_enqueue(&fixture, case, &mut probe).await?;
    let scripted = measure_scripted(&mut fixture, case, sampling, &mut probe).await?;
    fixture.host.shutdown().await?;
    let artifact = json!({
        "schema_version": "holon.agent_state.workload.v1", "workload_version": FIXTURE_SEED,
        "baseline_semantics_revision": "3637f8b0ba3dd9a4c74796c50520da1b5d521b4e",
        "case": case, "sampling": sampling,
        "environment": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "debug_assertions": cfg!(debug_assertions), "profile": "bench/release",
            "git_revision": command_output("git", &["rev-parse", "HEAD"]),
            "git_dirty": command_output("git", &["status", "--porcelain"]).map(|value| !value.is_empty()),
            "rustc": command_output("rustc", &["--version"]),
            "host_kernel": command_output("uname", &["-a"]),
            "rustflags": std::env::var("RUSTFLAGS").unwrap_or_default(),
            "logical_cpus": std::thread::available_parallelism().ok().map(usize::from),
            "allocator": "instrumented System; same instrumented binary contract required for comparisons",
            "network_mode": "none; scripted AgentProvider only"},
        "fixture": {"seed": FIXTURE_SEED, "history_messages_per_agent": HISTORY_MESSAGES,
            "linked_turns_per_agent": LINKED_TURNS, "history_records_per_agent": HISTORY_MESSAGES + LINKED_TURNS * 3 + 1,
            "pending_inputs_per_agent": PENDING_INPUTS, "pending_body_bytes": PENDING_BODY_BYTES,
            "large_stdout_bytes_per_agent": TOOL_STDOUT_BYTES, "prompt_budget_estimated_tokens": 8192,
            "fixture_creation_uses_public_typed_repositories": true,
            "history_seed_mode": "cold_typed_original_writer_batches_with_canonical_template_validation",
            "live_and_linked_evidence_use_typed_repositories": true, "operator_user_home_resources_disabled": true},
        "setup": {"wall_ns": setup_wall_ns, "actual_io": setup_io, "memory": setup_memory},
        "loaded_preview": preview, "enqueue_commit_publication": enqueue,
        "scripted_dispatch": scripted,
        "not_measured": {"syscall_count": null, "resident_domain_bytes": null,
            "snapshot_pinned_bytes": null, "provider_input_tokens": null, "provider_cache_tokens": null,
            "production_allocator_rss_comparison": null},
        "limitations": ["Synthetic history is 100,000 messages plus 64 linked turns/briefs/transcripts and one 10 MiB tool record per Agent, not 100,000 full turn bundles.", "Historical message bodies are short deterministic evidence; pending roots are exactly 16 KiB each.", "OS page cache is warm and uncontrolled; no cold-storage claim.", "SQL probe records actual SQLite statements, not logical helper calls; file counters cover instrumented AGENTS/curated-memory/plan/skill readers only, not all filesystem operations.", "Rust requested allocation accounting is not resident/pin accounting and excludes SQLite native allocations; RSS is process lifetime high-watermark.", "No real LLM usage or cost conclusion is supported by this offline harness."],
    });
    println!("{}", serde_json::to_string_pretty(&artifact)?);
    Ok(())
}
