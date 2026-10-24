use super::*;

use anyhow::Context;
use serde::Deserialize;
use std::time::Instant;

#[cfg(test)]
#[path = "unread_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "unread_benchmark.rs"]
mod benchmark;

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct MarkBriefReadRequest {
    pub(crate) read_through_event_seq: u64,
}

async fn run_unread_db<T: Send + 'static>(
    state: &Arc<AppState>,
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let started = Instant::now();
    let permit = state.unread_db_jobs.clone().try_acquire_owned()?;
    tokio::task::spawn_blocking(move || {
        // Cancellation of the HTTP waiter must not admit another DB job while
        // this synchronous operation is still queued or running.
        let _permit = permit;
        let queued = started.elapsed();
        let result = operation();
        tracing::debug!(
            target: "holon::http::unread",
            blocking_queue_us = queued.as_micros() as u64,
            db_us = started.elapsed().saturating_sub(queued).as_micros() as u64,
            total_us = started.elapsed().as_micros() as u64,
            success = result.is_ok(),
            "unread database operation completed"
        );
        result
    })
    .await
    .context("unread database task failed")?
}

fn unread_error_response(error: anyhow::Error) -> AxumResponse {
    if error.is::<tokio::sync::TryAcquireError>() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, "1")],
            Json(
                HttpErrorEnvelope::new("unread_busy", "unread database capacity is busy")
                    .retryable(true),
            ),
        )
            .into_response();
    }
    error_response(error).into_response()
}

pub(crate) async fn brief_read_states(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let (principal, entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(value) => value,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let db = state.host.runtime_db().clone();
    match run_unread_db(&state, move || {
        db.observer_brief_read_states(&principal, entitlement)
    })
    .await
    {
        Ok(states) => Json(states).into_response(),
        Err(error) => unread_error_response(error),
    }
}

pub(crate) async fn brief_read_state(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let (principal, entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(value) => value,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let db = state.host.runtime_db().clone();
    let requested_agent_id = agent_id.clone();
    match run_unread_db(&state, move || {
        db.observer_brief_read_state(&principal, entitlement, &requested_agent_id)
    })
    .await
    {
        Ok(Some(state)) => Json(state).into_response(),
        Ok(None) => agent_not_found(&agent_id),
        Err(error) => unread_error_response(error),
    }
}

pub(crate) async fn mark_brief_read(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<MarkBriefReadRequest>,
) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let (principal, entitlement) =
        match observer_sync::observer_principal_and_entitlement(&headers, &state) {
            Ok(value) => value,
            Err(error) => return authentication_error_response(error).into_response(),
        };
    let db = state.host.runtime_db().clone();
    let requested_agent_id = agent_id.clone();
    match run_unread_db(&state, move || {
        db.observer_mark_brief_read(
            &principal,
            entitlement,
            &requested_agent_id,
            request.read_through_event_seq,
        )
    })
    .await
    {
        Ok(Some(result)) => Json(result).into_response(),
        Ok(None) => agent_not_found(&agent_id),
        Err(error) => unread_error_response(error),
    }
}

fn agent_not_found(agent_id: &str) -> AxumResponse {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new(
            "agent_not_found",
            format!("public active agent {agent_id} was not found"),
        ),
    )
    .into_response()
}
