use std::{
    hint::black_box,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use holon::{
    domain::conversation::ConversationChange,
    runtime_db::RuntimeDb,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        AuditEvent, BriefKind, BriefRecord, TranscriptEntry, TranscriptEntryKind, TurnRecord,
        TurnTerminalKind, TurnTerminalSummary,
    },
};
use serde::Serialize;
use tempfile::TempDir;

const AGENT_ID: &str = "conversation-read-benchmark";
const LONG_TURN_ID: &str = "turn-long-active";
const HISTORICAL_TURNS: usize = 100;
const LONG_TURN_ACTIVITIES: usize = 96;
const SUMMARY_LIMIT: usize = 30;
const DETAIL_LIMIT: usize = 50;
const EVENT_LIMIT: usize = 256;
const ACTIVITY_LIMIT: usize = 64;
const DEFAULT_SAMPLES: usize = 10;
const WARMUP_SAMPLES: usize = 1;
const SUMMARY_MAX_BYTES: usize = 2 * 1024 * 1024;
const ACTIVITY_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize)]
struct BenchmarkArtifact {
    schema_version: &'static str,
    benchmark_suite: &'static str,
    workload_version: &'static str,
    started_at: String,
    git_revision: Option<String>,
    config: BenchmarkConfig,
    results: Vec<BenchmarkResult>,
}

#[derive(Serialize)]
struct BenchmarkConfig {
    warmup_repetitions: usize,
    measured_repetitions: usize,
    historical_turns: usize,
    long_turn_activities: usize,
    summary_limit: usize,
    detail_limit: usize,
    event_limit: usize,
    activity_limit: usize,
}

#[derive(Serialize)]
struct BenchmarkResult {
    benchmark_id: &'static str,
    logical_reads_per_sample: usize,
    samples: Vec<Sample>,
    summary: Summary,
}

#[derive(Clone, Serialize)]
struct Sample {
    iteration: usize,
    wall_time_ns: u128,
    payload_bytes: usize,
    records: usize,
    replay_events: u64,
}

#[derive(Serialize)]
struct Summary {
    median_wall_time_ns: u128,
    min_wall_time_ns: u128,
    max_wall_time_ns: u128,
    median_payload_bytes: usize,
    median_records: usize,
    median_replay_events: u64,
}

#[derive(Clone, Copy)]
struct Observation {
    payload_bytes: usize,
    records: usize,
    replay_events: u64,
}

struct Fixture {
    _root: TempDir,
    db: RuntimeDb,
    snapshot_cursor: String,
}

fn main() -> Result<()> {
    let samples = std::env::var("HOLON_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("HOLON_BENCH_SAMPLES must be a positive integer")?
        .unwrap_or(DEFAULT_SAMPLES);
    anyhow::ensure!(samples > 0, "HOLON_BENCH_SAMPLES must be positive");

    let fixture = fixture()?;
    let results = vec![
        measure("conversation_read.summary_first_page", samples, || {
            let snapshot = fixture
                .db
                .conversation()
                .summary_snapshot(AGENT_ID, SUMMARY_LIMIT, None, "benchmark", "public")?
                .context("summary snapshot must exist")?;
            let records = snapshot.value.turns.len()
                + snapshot.value.active_turns.len()
                + snapshot.value.pending_inputs.len();
            let payload_bytes = serde_json::to_vec(&snapshot.value)?.len()
                + snapshot.snapshot_cursor.len()
                + snapshot.next_before_cursor.as_deref().map_or(0, str::len);
            anyhow::ensure!(snapshot.value.turns.len() <= SUMMARY_LIMIT);
            anyhow::ensure!(payload_bytes <= SUMMARY_MAX_BYTES);
            black_box(&snapshot);
            Ok(Observation {
                payload_bytes,
                records,
                replay_events: 0,
            })
        })?,
        measure("conversation_read.long_turn_detail", samples, || {
            let snapshot = fixture
                .db
                .conversation()
                .activity_snapshot(
                    AGENT_ID,
                    LONG_TURN_ID,
                    DETAIL_LIMIT,
                    None,
                    "benchmark",
                    "public",
                )?
                .context("activity snapshot must exist")?;
            let page = snapshot.value.context("long turn detail must exist")?;
            let payload_bytes = serde_json::to_vec(&page)?.len()
                + snapshot.snapshot_cursor.len()
                + snapshot.next_before_cursor.as_deref().map_or(0, str::len);
            anyhow::ensure!(page.activities.len() == DETAIL_LIMIT);
            anyhow::ensure!(page.has_more);
            anyhow::ensure!(payload_bytes <= ACTIVITY_MAX_BYTES);
            black_box(&page);
            Ok(Observation {
                payload_bytes,
                records: page.activities.len(),
                replay_events: 0,
            })
        })?,
        measure("conversation_read.reconnect_replay", samples, || {
            let batch = fixture
                .db
                .conversation()
                .change_batch(
                    AGENT_ID,
                    Some(&fixture.snapshot_cursor),
                    EVENT_LIMIT,
                    ACTIVITY_LIMIT,
                    "benchmark",
                    "public",
                )?
                .context("change batch must exist")?;
            let inline_activities = batch
                .changes
                .iter()
                .filter(|change| matches!(change, ConversationChange::ActivityUpsert { .. }))
                .count();
            anyhow::ensure!(inline_activities == 0);
            anyhow::ensure!(batch.changes.len() <= EVENT_LIMIT + ACTIVITY_LIMIT);
            let payload_bytes = serde_json::to_vec(&batch.changes)?.len() + batch.checkpoint.len();
            let replay_events = batch.through_seq.saturating_sub(batch.from_seq);
            black_box(&batch);
            Ok(Observation {
                payload_bytes,
                records: batch.changes.len(),
                replay_events,
            })
        })?,
        measure("conversation_read.shadow_metadata", samples, || {
            let report = fixture
                .db
                .conversation()
                .shadow_diagnostics(AGENT_ID, SUMMARY_LIMIT, "benchmark", "public")?
                .context("shadow diagnostics must exist")?;
            anyhow::ensure!(report.mismatch_count == 0);
            let payload_bytes = serde_json::to_vec(&report)?.len();
            black_box(&report);
            Ok(Observation {
                payload_bytes,
                records: report.canonical.turns,
                replay_events: 0,
            })
        })?,
    ];

    let artifact = BenchmarkArtifact {
        schema_version: "holon.performance.v0",
        benchmark_suite: "conversation_read",
        workload_version: "v1",
        started_at: Utc::now().to_rfc3339(),
        git_revision: command_output("git", &["rev-parse", "HEAD"]),
        config: BenchmarkConfig {
            warmup_repetitions: WARMUP_SAMPLES,
            measured_repetitions: samples,
            historical_turns: HISTORICAL_TURNS,
            long_turn_activities: LONG_TURN_ACTIVITIES,
            summary_limit: SUMMARY_LIMIT,
            detail_limit: DETAIL_LIMIT,
            event_limit: EVENT_LIMIT,
            activity_limit: ACTIVITY_LIMIT,
        },
        results,
    };
    println!("{}", serde_json::to_string_pretty(&artifact)?);
    Ok(())
}

fn fixture() -> Result<Fixture> {
    let root = tempfile::tempdir().context("creating conversation read benchmark root")?;
    let db = RuntimeDb::open_and_migrate(
        root.path().join("runtime.sqlite"),
        root.path().join("runtime.lock"),
    )?;
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        AGENT_ID,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))?;

    for turn_index in 1..=HISTORICAL_TURNS {
        let turn_id = format!("turn-{turn_index:03}");
        let mut turn = TurnRecord::new(AGENT_ID, &turn_id, turn_index as u64);
        turn.created_at = timestamp(turn_index as i64);
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: turn.created_at + Duration::from_secs(1),
            duration_ms: 1_000,
        });
        db.turn_records().upsert(&turn)?;

        let mut brief = BriefRecord::new(
            AGENT_ID,
            BriefKind::Result,
            format!("bounded benchmark result {turn_index:03}"),
            None,
            None,
        );
        brief.id = format!("brief-{turn_index:03}");
        brief.turn_id = Some(turn_id);
        brief.turn_index = Some(turn_index as u64);
        brief.created_at = timestamp(turn_index as i64 + 1);
        db.evidence().append_brief(&brief)?;
    }

    let mut long_turn = TurnRecord::new(AGENT_ID, LONG_TURN_ID, HISTORICAL_TURNS as u64 + 1);
    long_turn.created_at = timestamp(HISTORICAL_TURNS as i64 + 2);
    db.turn_records().upsert(&long_turn)?;
    for index in 0..LONG_TURN_ACTIVITIES {
        let mut entry = TranscriptEntry::new(
            AGENT_ID,
            TranscriptEntryKind::AssistantRound,
            Some(index + 1),
            None,
            serde_json::json!({
                "turn_id": LONG_TURN_ID,
                "text": format!("bounded activity {index:03}"),
            }),
        );
        entry.id = format!("activity-{index:03}");
        entry.created_at = timestamp(HISTORICAL_TURNS as i64 + 3 + index as i64);
        db.evidence().append_transcript_entry(&entry)?;
    }

    let snapshot_cursor = db
        .conversation()
        .summary_snapshot(AGENT_ID, SUMMARY_LIMIT, None, "benchmark", "public")?
        .context("benchmark bootstrap snapshot must exist")?
        .snapshot_cursor;
    let mut event = AuditEvent::legacy(
        "conversation_benchmark_change",
        serde_json::json!({ "turn_id": LONG_TURN_ID }),
    );
    event.id = "conversation-benchmark-change".into();
    event.created_at = timestamp(HISTORICAL_TURNS as i64 + LONG_TURN_ACTIVITIES as i64 + 4);
    db.audit_events().append(Some(AGENT_ID), &event)?;

    Ok(Fixture {
        _root: root,
        db,
        snapshot_cursor,
    })
}

fn measure(
    benchmark_id: &'static str,
    repetitions: usize,
    mut operation: impl FnMut() -> Result<Observation>,
) -> Result<BenchmarkResult> {
    for _ in 0..WARMUP_SAMPLES {
        black_box(operation().with_context(|| format!("warming up {benchmark_id}"))?);
    }
    let mut samples = Vec::with_capacity(repetitions);
    for iteration in 1..=repetitions {
        let started_at = Instant::now();
        let observation = operation().with_context(|| format!("measuring {benchmark_id}"))?;
        samples.push(Sample {
            iteration,
            wall_time_ns: started_at.elapsed().as_nanos(),
            payload_bytes: observation.payload_bytes,
            records: observation.records,
            replay_events: observation.replay_events,
        });
    }
    Ok(BenchmarkResult {
        benchmark_id,
        logical_reads_per_sample: 1,
        summary: summarize(&samples),
        samples,
    })
}

fn summarize(samples: &[Sample]) -> Summary {
    let mut wall_times: Vec<_> = samples.iter().map(|sample| sample.wall_time_ns).collect();
    let mut payload_bytes: Vec<_> = samples.iter().map(|sample| sample.payload_bytes).collect();
    let mut records: Vec<_> = samples.iter().map(|sample| sample.records).collect();
    let mut replay_events: Vec<_> = samples.iter().map(|sample| sample.replay_events).collect();
    wall_times.sort_unstable();
    payload_bytes.sort_unstable();
    records.sort_unstable();
    replay_events.sort_unstable();
    Summary {
        median_wall_time_ns: wall_times[wall_times.len() / 2],
        min_wall_time_ns: wall_times[0],
        max_wall_time_ns: wall_times[wall_times.len() - 1],
        median_payload_bytes: payload_bytes[payload_bytes.len() / 2],
        median_records: records[records.len() / 2],
        median_replay_events: replay_events[replay_events.len() / 2],
    }
}

fn timestamp(offset_seconds: i64) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, 0, 0, 0)
        .single()
        .expect("valid benchmark timestamp")
        + chrono::Duration::seconds(offset_seconds)
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
