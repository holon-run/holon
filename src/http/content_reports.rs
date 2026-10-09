//! HTTP endpoint for reporting user-visible AI content.

use super::*;

use crate::runtime_db::content_reports::{
    ContentReportError, NewContentReport, MAX_CLIENT_REQUEST_ID_CHARS, MAX_DESCRIPTION_CHARS,
    MAX_IDENTIFIER_CHARS, REPORT_CATEGORIES,
};

pub(crate) const CONTENT_REPORT_BODY_LIMIT_BYTES: usize = 32 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct CreateContentReportRequest {
    pub agent_id: String,
    pub turn_id: String,
    pub message_id: String,
    pub category: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub client_request_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CreateContentReportResponse {
    pub report_id: String,
    pub status: String,
    pub created_at: String,
}

pub(crate) async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateContentReportRequest>,
) -> Result<(StatusCode, Json<CreateContentReportResponse>), (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|error| auth_required(error.to_string()))?;
    let actor =
        control_actor(&headers, &state).map_err(|error| auth_required(error.to_string()))?;
    validate_request(&request)?;

    let record = state
        .host
        .runtime_db()
        .content_reports()
        .create(NewContentReport {
            reporter_principal: actor.principal_id(),
            agent_id: request.agent_id,
            turn_id: request.turn_id,
            message_id: request.message_id,
            category: request.category,
            description: request
                .description
                .filter(|description| !description.trim().is_empty()),
            client_request_id: request.client_request_id,
        })
        .map_err(|error| {
            if error
                .downcast_ref::<ContentReportError>()
                .is_some_and(|error| matches!(error, ContentReportError::RateLimited))
            {
                return http_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    HttpErrorEnvelope::new("content_report_rate_limited", error.to_string())
                        .retryable(true),
                );
            }
            if error.downcast_ref::<ContentReportError>().is_some() {
                not_found("reported message was not found")
            } else {
                error_response(error)
            }
        })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateContentReportResponse {
            report_id: record.report_id,
            status: "accepted".to_string(),
            created_at: record.created_at,
        }),
    ))
}

fn validate_request(request: &CreateContentReportRequest) -> Result<(), (StatusCode, Json<Value>)> {
    for (field, value) in [
        ("agent_id", request.agent_id.as_str()),
        ("turn_id", request.turn_id.as_str()),
        ("message_id", request.message_id.as_str()),
    ] {
        if value.is_empty() || value.chars().count() > MAX_IDENTIFIER_CHARS {
            return Err(bad_request(format!(
                "{field} must be between 1 and {MAX_IDENTIFIER_CHARS} characters"
            )));
        }
    }
    if !REPORT_CATEGORIES.contains(&request.category.as_str()) {
        return Err(bad_request("category is not supported"));
    }
    if request
        .description
        .as_deref()
        .is_some_and(|description| description.chars().count() > MAX_DESCRIPTION_CHARS)
    {
        return Err(bad_request(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    if request.client_request_id.as_deref().is_some_and(|value| {
        value.is_empty()
            || value.chars().count() > MAX_CLIENT_REQUEST_ID_CHARS
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    }) {
        return Err(bad_request(format!(
            "client_request_id must use ASCII letters, numbers, '.', '-' or '_' and be at most {MAX_CLIENT_REQUEST_ID_CHARS} characters"
        )));
    }
    Ok(())
}
