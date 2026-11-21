use axum::extract::RawQuery;
use holon_server::avatar::AvatarResponseError;

use super::*;
pub(crate) use holon_server::avatar::AvatarResources;

pub(super) async fn get(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<AxumResponse, AxumResponse> {
    // Even a cache hit or conditional read must revalidate this session/target.
    authorize_remote_access(&headers, &state)
        .map_err(|error| authentication_error_response(error).into_response())?;
    crate::host_registry::validate_agent_id_format(&agent_id)
        .map_err(|error| bad_request(error.to_string()).into_response())?;
    let host = state.host.clone();
    let target = agent_id.clone();
    tokio::task::spawn_blocking(move || host.local_agent_resource_identity(&target))
        .await
        .map_err(|error| error_response(error.into()).into_response())?
        .map_err(|error| agent_access_error(error).into_response())?;
    state
        .avatar_resources
        .respond(&agent_id, query.as_deref().unwrap_or_default(), &headers)
        .await
        .map_err(|error| match error {
            AvatarResponseError::InvalidQuery(reason) => bad_request(reason).into_response(),
            AvatarResponseError::Failed => {
                error_response(anyhow!("avatar generation failed")).into_response()
            }
            AvatarResponseError::Busy => {
                let mut response =
                    service_unavailable("avatar generation capacity exhausted").into_response();
                response
                    .headers_mut()
                    .insert("retry-after", HeaderValue::from_static("1"));
                response
            }
        })
}

#[cfg(test)]
#[cfg(holon_lib_test_shard = "c")]
mod tests;
