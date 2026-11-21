use super::*;

async fn callback_ingress(
    mode: CallbackIngressMode,
    callback_token: String,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let payload = build_callback_delivery_payload(&headers, body)
        .map_err(|err| bad_request(err.to_string()))?;
    let (locator, incarnation) = state
        .host
        .resolve_external_trigger_locator(&callback_token)
        .map_err(callback_internal_error)?
        .ok_or_else(|| forbidden("invalid callback token"))?;
    if locator.delivery_mode != mode.delivery_mode() {
        return Err(forbidden("callback delivery mode mismatch"));
    }
    let delivery = state
        .host
        .deliver_external_callback(
            &locator.target_agent_id,
            incarnation,
            &locator.external_trigger_id,
            payload,
        )
        .await;
    let result = match delivery {
        Ok(result) => result,
        Err(err)
            if err
                .downcast_ref::<crate::host::AgentQuarantined>()
                .is_some() =>
        {
            // A descriptor can be revoked while cold activation is waiting.
            // Do not disclose quarantine if the capability no longer resolves.
            let target = locator.target_agent_id.clone();
            let descriptor = locator.external_trigger_id.clone();
            drop(locator);
            let still_authorized = state
                .host
                .resolve_external_trigger_locator(&callback_token)
                .map_err(callback_internal_error)?
                .is_some_and(|(current, current_incarnation)| {
                    current_incarnation == incarnation
                        && current.target_agent_id == target
                        && current.external_trigger_id == descriptor
                        && current.delivery_mode == mode.delivery_mode()
                });
            return Err(if still_authorized {
                error_response(err)
            } else {
                forbidden("invalid callback token")
            });
        }
        Err(err) if callback_access_denied(&err) => {
            return Err(forbidden("invalid callback token"));
        }
        Err(err) => return Err(callback_internal_error(err)),
    };
    drop(locator);
    Ok(Json(CallbackResponse { ok: true, result }))
}

fn callback_access_denied(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<PublicAgentError>(),
        Some(
            PublicAgentError::NotFound { .. }
                | PublicAgentError::Deleting { .. }
                | PublicAgentError::Deleted { .. }
                | PublicAgentError::Stopped { .. }
        )
    ) || error
        .downcast_ref::<crate::runtime_error::RuntimeError>()
        .is_some_and(|error| {
            matches!(
                error.descriptor().code.as_str(),
                "agent_incarnation_mismatch" | "callback_descriptor_rejected"
            )
        })
}

fn callback_internal_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    error!(
        code = %describe_runtime_error(&error).code,
        "callback ingress failed internally"
    );
    http_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        HttpErrorEnvelope::new("callback_internal_error", "callback delivery failed"),
    )
}

pub async fn callback_ingress_enqueue(
    Path(callback_token): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    callback_ingress(
        CallbackIngressMode::Enqueue,
        callback_token,
        headers,
        State(state),
        body,
    )
    .await
}

pub async fn callback_ingress_wake(
    Path(callback_token): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    callback_ingress(
        CallbackIngressMode::Wake,
        callback_token,
        headers,
        State(state),
        body,
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallbackIngressMode {
    Enqueue,
    Wake,
}

impl CallbackIngressMode {
    fn delivery_mode(self) -> crate::types::CallbackDeliveryMode {
        match self {
            Self::Enqueue => crate::types::CallbackDeliveryMode::EnqueueMessage,
            Self::Wake => crate::types::CallbackDeliveryMode::WakeHint,
        }
    }
}

fn build_callback_delivery_payload(
    headers: &HeaderMap,
    body: Bytes,
) -> Result<CallbackDeliveryPayload> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let body = if body.is_empty() {
        None
    } else {
        Some(parse_callback_body(content_type.as_deref(), &body)?)
    };
    Ok(CallbackDeliveryPayload {
        body,
        content_type,
        correlation_id: None,
        causation_id: None,
    })
}

fn parse_callback_body(content_type: Option<&str>, body: &[u8]) -> Result<MessageBody> {
    match content_type {
        Some(content_type) if is_json_content_type(content_type) => {
            let value = serde_json::from_slice(body)
                .map_err(|err| anyhow!("invalid JSON callback body: {err}"))?;
            Ok(MessageBody::Json { value })
        }
        Some(content_type) if is_text_content_type(content_type) => {
            let text = std::str::from_utf8(body)
                .map_err(|err| anyhow!("invalid UTF-8 callback body: {err}"))?;
            Ok(MessageBody::Text {
                text: text.to_string(),
            })
        }
        Some(content_type) => Ok(MessageBody::Json {
            value: json!({
                "content_type": content_type,
                "body_base64": BASE64_STANDARD.encode(body),
            }),
        }),
        None => match std::str::from_utf8(body) {
            Ok(text) => Ok(MessageBody::Text {
                text: text.to_string(),
            }),
            Err(_) => Ok(MessageBody::Json {
                value: json!({
                    "body_base64": BASE64_STANDARD.encode(body),
                }),
            }),
        },
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    let normalized = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    normalized == "application/json" || normalized.ends_with("+json")
}

fn is_text_content_type(content_type: &str) -> bool {
    let normalized = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    normalized.starts_with("text/")
}

pub async fn generic_webhook(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<Value>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let trace_context = holon_server::trace::trace_context_from_headers(&headers)?;
    enqueue_internal(
        state,
        agent_id,
        EnqueueRequest {
            kind: Some(MessageKind::WebhookEvent),
            priority: Some(Priority::Normal),
            authority_class: Some(AuthorityClass::IntegrationSignal),
            body: Some(MessageBody::Json { value: payload }),
            text: None,
            json: None,
            metadata: None,
            correlation_id: None,
            causation_id: None,
            origin: Some(IncomingOrigin::Webhook {
                source: "generic_webhook".into(),
                event_type: None,
            }),
        },
        EnqueueIngress::Trusted {
            delivery_surface: MessageDeliverySurface::HttpWebhook,
            admission_context: public_admission_context(),
        },
        trace_context,
    )
    .await
}
