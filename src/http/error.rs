use anyhow::Result;
use axum::{http::StatusCode, Json};
use serde_json::{Map, Value};

use crate::{host::PublicAgentError, runtime::CurrentRunAbortError};

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HttpErrorEnvelope {
    ok: bool,
    error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

impl HttpErrorEnvelope {
    pub(super) fn new(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: error.into(),
            code: None,
            hint: None,
            extensions: Map::new(),
        }
    }

    pub(super) fn code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub(super) fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub(super) fn extension(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extensions.insert(key.into(), value.into());
        self
    }
}

pub(super) fn http_error(
    status: StatusCode,
    envelope: HttpErrorEnvelope,
) -> (StatusCode, Json<Value>) {
    let value = serde_json::to_value(envelope).expect("HTTP error envelope serializes");
    (status, Json(value))
}

pub(super) fn error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        HttpErrorEnvelope::new(error.to_string()),
    )
}

pub(super) fn forbidden(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::FORBIDDEN, HttpErrorEnvelope::new(reason))
}

pub(super) fn auth_required(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::FORBIDDEN,
        HttpErrorEnvelope::new(reason)
            .code("auth_required")
            .hint("retry with an Authorization: Bearer <token> header"),
    )
}

pub(super) fn bad_request(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::BAD_REQUEST, HttpErrorEnvelope::new(reason))
}

pub(super) fn service_unavailable(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::SERVICE_UNAVAILABLE,
        HttpErrorEnvelope::new(reason),
    )
}

pub(super) fn not_found(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::NOT_FOUND, HttpErrorEnvelope::new(reason))
}

pub(super) fn task_lifecycle_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    let message = error.to_string();
    if message.starts_with("task ") && message.ends_with(" not found") {
        not_found(message)
    } else {
        error_response(error)
    }
}

pub(super) fn work_item_lifecycle_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if (lower.contains("work item") && lower.ends_with("not found"))
        || lower.starts_with("unknown work item ")
    {
        not_found(message)
    } else if message.starts_with("cannot ") {
        bad_request(message)
    } else {
        error_response(error)
    }
}

pub(super) fn timer_lifecycle_error(err: anyhow::Error) -> (StatusCode, Json<Value>) {
    let message = err.to_string();
    if message.starts_with("timer ") && message.ends_with(" not found") {
        not_found(message)
    } else if message.starts_with("cannot ") {
        bad_request(message)
    } else {
        error_response(err)
    }
}

pub(super) fn stopped_agent_conflict(
    reason: impl Into<String>,
    agent_id: impl Into<String>,
) -> (StatusCode, Json<Value>) {
    let agent_id = agent_id.into();
    let hint = format!(
        "start with `holon agent start {}` or POST /control/agents/{}/control with JSON body {{\"action\":\"start\"}}",
        agent_id, agent_id
    );
    http_error(
        StatusCode::CONFLICT,
        HttpErrorEnvelope::new(reason)
            .code("agent_stopped")
            .hint(hint)
            .extension("agent_id", agent_id),
    )
}

pub(super) fn agent_access_error(error: PublicAgentError) -> (StatusCode, Json<Value>) {
    match error {
        PublicAgentError::Private { agent_id } => {
            forbidden(format!("agent {} is private", agent_id))
        }
        PublicAgentError::NotFound { agent_id } => {
            not_found(format!("agent {} not found", agent_id))
        }
        PublicAgentError::Archived { agent_id } => {
            not_found(format!("agent {} is archived", agent_id))
        }
        PublicAgentError::Stopped { agent_id } => stopped_agent_conflict(
            format!("agent {} is stopped; start first", agent_id),
            agent_id,
        ),
        PublicAgentError::Runtime(error) => error_response(error),
    }
}

pub(super) fn abort_error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    match error.downcast::<CurrentRunAbortError>() {
        Ok(CurrentRunAbortError::StaleRunId {
            requested_run_id,
            current_run_id,
        }) => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new(format!(
                "stale run_id {requested_run_id}; current run is {current_run_id}"
            ))
            .code("stale_run_id")
            .extension("requested_run_id", requested_run_id)
            .extension("current_run_id", current_run_id),
        ),
        Ok(CurrentRunAbortError::NoCurrentRun { agent_id }) => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new(format!("agent {agent_id} has no current run to abort"))
                .code("no_current_run")
                .extension("agent_id", agent_id),
        ),
        Err(error) => error_response(error),
    }
}

pub(super) fn skill_install_error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    match error.downcast::<crate::skills::SkillInstallConflict>() {
        Ok(conflict) => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new(conflict.to_string())
                .code("skill_already_installed")
                .hint("uninstall the existing skill first or choose a different skill name")
                .extension("skill_name", conflict.skill_name)
                .extension("destination", conflict.destination.to_string_lossy().to_string()),
        ),
        Err(error) => match error.downcast::<crate::skills::SkillManagerUnavailable>() {
            Ok(unavailable) => http_error(
                StatusCode::FAILED_DEPENDENCY,
                HttpErrorEnvelope::new(unavailable.to_string())
                    .code("skill_manager_unavailable")
                    .hint("Install Node.js/npm so `npx skills` is available, or install the skill manually into ~/.agents/skills and link it by name.")
                    .extension("manager", unavailable.manager),
            ),
            Err(error) => match error.downcast::<crate::skills::RemoteSkillInstallFailed>() {
                Ok(failed) => http_error(
                    StatusCode::BAD_GATEWAY,
                    HttpErrorEnvelope::new(failed.to_string())
                        .code("remote_skill_install_failed")
                        .extension("package", failed.package)
                        .extension("exit_status", failed.status)
                        .extension("stdout", failed.stdout)
                        .extension("stderr", failed.stderr),
                ),
                Err(error) => match error.downcast::<crate::skills::RemoteSkillInstallTimedOut>() {
                    Ok(timeout) => http_error(
                        StatusCode::GATEWAY_TIMEOUT,
                        HttpErrorEnvelope::new(timeout.to_string())
                            .code("remote_skill_install_timeout")
                            .extension("package", timeout.package)
                            .extension("timeout_seconds", timeout.timeout.as_secs()),
                    ),
                    Err(error) => error_response(error),
                },
            },
        },
    }
}

pub(super) fn normalize_optional_non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|inner| {
        let trimmed = inner.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(super) fn parse_blocked_by_mutation(
    value: Value,
) -> Result<Option<String>, (StatusCode, Json<Value>)> {
    match value {
        Value::Null => Ok(None),
        Value::String(inner) => {
            let trimmed = inner.trim().to_string();
            Ok((!trimmed.is_empty()).then_some(trimmed))
        }
        _ => Err(bad_request("blocked_by must be a string or null")),
    }
}

pub(super) fn event_seq_not_found(after_seq: u64) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new(format!(
            "after_seq {after_seq} was not found in the replay window"
        ))
        .code("cursor_not_found")
        .extension("after_seq", after_seq)
        .extension("event_seq", after_seq),
    )
}
