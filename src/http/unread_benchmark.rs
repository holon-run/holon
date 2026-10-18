//! Reproducible batch HTTP comparison; opt-in, never touches a running daemon.
use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
    AgentVisibility, BriefKind, BriefRecord,
};
use std::collections::BTreeMap;
use std::sync::Mutex;
use tracing::{
    field::{Field, Visit},
    Event, Subscriber,
};
use tracing_subscriber::{layer::Context as LayerContext, prelude::*, Layer};

#[derive(Clone, Default)]
struct Phases(Arc<Mutex<BTreeMap<String, u64>>>);
impl Visit for Phases {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.lock().unwrap().insert(field.name().into(), value);
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}
impl<S: Subscriber> Layer<S> for Phases {
    fn on_event(&self, event: &Event<'_>, _context: LayerContext<'_, S>) {
        if event.metadata().target() == "holon::http::unread" {
            event.record(&mut self.clone());
        }
    }
}

async fn legacy_brief_read_states(state: Arc<AppState>, headers: HeaderMap) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return auth_required(error.to_string()).into_response();
    }
    let scope_started = Instant::now();
    let roster = state.host.agent_roster_snapshot().unwrap();
    let (principal, entitlement) =
        observer_sync::observer_principal_and_entitlement(&headers, &state).unwrap();
    let scope = observer_sync::observer_visibility_scope_for(
        &roster.runtime_id,
        &principal,
        entitlement,
        roster.visibility_policy_generation,
    );
    tracing::debug!(target: "holon::http::unread", legacy_roster_scope_us = scope_started.elapsed().as_micros() as u64);
    Json(
        state
            .host
            .runtime_db()
            .brief_read_states(&principal, &scope)
            .unwrap(),
    )
    .into_response()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "fixed-workload performance comparison; cargo test --lib unread_scope_benchmark -- --ignored --nocapture"]
async fn unread_scope_benchmark() {
    let phases = Phases::default();
    // This ignored benchmark is intended to run alone, including spawn_blocking
    // events. No global subscriber is installed by normal unit tests.
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(phases.clone()))
        .unwrap();
    let (_home, host) = super::super::tests::control_token_test_host();
    const AGENTS: usize = 60;
    const HISTORY: usize = 20;
    const SAMPLES: usize = 12;
    for index in 0..AGENTS {
        let agent_id = format!("unread-bench-{index:03}");
        host.runtime_db()
            .agent_identities()
            .upsert(&AgentIdentityRecord::new(
                &agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ))
            .unwrap();
        let mut agent = AgentState::new(&agent_id);
        agent.last_wake_reason = Some("x".repeat(8192));
        let payload = serde_json::to_string(&agent).unwrap();
        host.runtime_db().transaction(|tx| {
            tx.execute("INSERT INTO agent_states (agent_id, status, updated_at, payload_json) VALUES (?1, 'idle', ?2, ?3)", rusqlite::params![agent_id, Utc::now().to_rfc3339(), payload])?;
            Ok(())
        }).unwrap();
        for number in 0..HISTORY {
            let brief = BriefRecord::new(
                &agent_id,
                BriefKind::Result,
                format!("brief {number}: {}", "b".repeat(256)),
                None,
                None,
            );
            let event = crate::types::brief_created_event_for(&brief).unwrap();
            host.runtime_db()
                .evidence()
                .append_brief_with_created_event(Some(&agent_id), &brief, &event, &[])
                .unwrap();
        }
    }
    let roster = host.runtime_db().agent_roster_snapshot_rows().unwrap();
    let scope_payload_bytes: usize = roster
        .rows
        .iter()
        .map(|row| {
            row.identity_json.len()
                + row.agent_state_json.as_ref().map_or(0, String::len)
                + row
                    .latest_brief
                    .as_ref()
                    .and_then(|brief| brief.preview.as_ref())
                    .map_or(0, String::len)
        })
        .sum();
    let state = Arc::new(AppState::for_tcp(host));
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
    let mut samples = Vec::new();
    for iteration in 0..=SAMPLES {
        // Alternate order after warmup to reduce monotonic cache/order bias.
        for legacy in if iteration % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            phases.0.lock().unwrap().clear();
            let started = Instant::now();
            let response = if legacy {
                legacy_brief_read_states(state.clone(), headers.clone()).await
            } else {
                brief_read_states(State(state.clone()), headers.clone()).await
            };
            let handler_us = started.elapsed().as_micros() as u64;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let states: Vec<crate::runtime_db::unread::BriefReadState> =
                serde_json::from_slice(&bytes).unwrap();
            assert_eq!(states.len(), AGENTS);
            assert!(states
                .iter()
                .all(|state| state.unread_count == 0 && state.revision == 0));
            if iteration > 0 {
                samples.push(json!({"legacy": legacy, "iteration": iteration, "handler_us": handler_us, "phases_us": phases.0.lock().unwrap().clone()}));
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&json!({
        "fixture": {"agents": AGENTS, "briefs_per_agent": HISTORY, "state_reason_bytes": 8192, "samples_per_path": SAMPLES, "warmup_per_path": 1},
        "scope_projection_payload_bytes": {"legacy": scope_payload_bytes, "new": 0},
        "notes": "SQL statement and connection counts are not measured; legacy roster assembly also performs per-agent posture queries. JSON payload bytes are measured roster return values, not disk I/O. Baseline is the removed handler, including synchronous roster assembly; baseline blocking queue is not applicable. New total_us is blocking task time including queue, handler_us includes static-token authentication and JSON serialization. Session authentication retains its existing synchronous DB work outside unread admission. No production-load or global SLO claim.",
        "samples": samples,
    })).unwrap());
}
