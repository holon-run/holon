use super::serialize_success;
use crate::decision_telemetry::stable_decision_id;
use crate::decision_telemetry::DecisionAdvisoryCompletedEvent;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::spec::ToolExecutionContext;
use crate::types::AuthorityClass;
use anyhow::Result;
use decision_core::DecisionOutcome;
use decision_core::DecisionRequest;
use serde_json::json;
use serde_json::Value;
use std::time::Duration;

pub(crate) use holon_tools::builtins::advisory_decision::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
    context: &ToolExecutionContext,
) -> Result<crate::tool::ToolResult> {
    ensure_authority(authority_class)?;
    let mut args: AdvisoryDecisionArgs = parse_tool_args(NAME, input)?;
    validate_args(&args)?;
    args.question = args.question.trim().to_owned();
    args.options = args
        .options
        .into_iter()
        .map(|option| option.trim().to_owned())
        .collect();
    let state = sanitize_state(&args.state);
    let fingerprint = fingerprint(&args.question, &args.options, &state);
    let (enabled, max_calls, timeout_ms, min_confidence) = runtime.advisory_decision_tool_config();
    if !enabled {
        return finish_result(
            runtime,
            agent_id,
            context,
            &args,
            &fingerprint,
            abstain_result(
                "disabled",
                "advisory decision tool is disabled",
                "",
                fingerprint.clone(),
            ),
        )
        .await;
    }
    if let Some(max_calls) = max_calls {
        let call_index = context
            .decision_tool_calls
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if call_index >= max_calls {
            return finish_result(
                runtime,
                agent_id,
                context,
                &args,
                &fingerprint,
                abstain_result(
                    "rate_limited",
                    "per-turn advisory decision limit reached",
                    "",
                    fingerprint.clone(),
                ),
            )
            .await;
        }
    }

    let request = DecisionRequest {
        request_id: format!("advisory-{fingerprint}"),
        input: json!({
            "question": args.question,
            "state": state,
        }),
        candidates: args.options.iter().map(|option| json!(option)).collect(),
        schema: SCHEMA.into(),
        schema_version: SCHEMA_VERSION.into(),
        metadata: [("integration".into(), "holon-agent-tool".into())]
            .into_iter()
            .collect(),
        deadline_ms: Some(timeout_ms),
    };
    let started = std::time::Instant::now();
    let response = match tokio::time::timeout(
        Duration::from_millis(timeout_ms.max(1)),
        runtime.execute_advisory_decision(request),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return finish_result(
                runtime,
                agent_id,
                context,
                &args,
                &fingerprint,
                abstain_result(
                    "provider_failure",
                    &redact_text(&error.to_string(), 240),
                    "",
                    fingerprint.clone(),
                ),
            )
            .await;
        }
        Err(_) => {
            return finish_result(
                runtime,
                agent_id,
                context,
                &args,
                &fingerprint,
                abstain_result(
                    "timeout",
                    "advisory decision provider timed out",
                    "",
                    fingerprint.clone(),
                ),
            )
            .await;
        }
    };
    let (response, provider, model) = response;
    let latency_ms = response
        .elapsed_ms
        .or_else(|| Some(started.elapsed().as_millis() as u64));
    if let Err(error) = response.validate(SCHEMA_VERSION) {
        return finish_result(
            runtime,
            agent_id,
            context,
            &args,
            &fingerprint,
            abstain_result_with_metadata(
                "invalid_response",
                &redact_text(&error.to_string(), 240),
                provider,
                model,
                response.confidence,
                latency_ms,
                Vec::new(),
                fingerprint.clone(),
            ),
        )
        .await;
    }
    let confidence = response.confidence;
    let evidence = response
        .evidence
        .into_iter()
        .map(|item| {
            json!({
                "kind": item.kind,
                "summary": redact_text(&item.summary, 240),
            })
        })
        .collect();
    let result = match response.outcome {
        DecisionOutcome::Select { value } => {
            let choice = value.as_str().map(str::to_owned);
            if confidence.is_none_or(|value| value < min_confidence) {
                abstain_result_with_metadata(
                    "low_confidence",
                    "provider confidence is missing or below the configured threshold",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                )
            } else if choice
                .as_deref()
                .is_some_and(|value| args.options.iter().any(|option| option == value))
            {
                AdvisoryDecisionResult {
                    outcome: "select".into(),
                    choice,
                    confidence,
                    abstain: false,
                    reason: None,
                    provider,
                    model,
                    latency_ms,
                    evidence,
                    question_fingerprint: fingerprint.clone(),
                }
            } else {
                abstain_result_with_metadata(
                    "invalid_choice",
                    "provider returned an option outside the request",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                )
            }
        }
        DecisionOutcome::Fallback { value } => {
            let choice = value.as_str().map(str::to_owned);
            if confidence.is_none_or(|value| value < min_confidence) {
                abstain_result_with_metadata(
                    "low_confidence",
                    "provider confidence is missing or below the configured threshold",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                )
            } else if choice
                .as_deref()
                .is_some_and(|value| args.options.iter().any(|option| option == value))
            {
                AdvisoryDecisionResult {
                    outcome: "fallback".into(),
                    choice,
                    confidence,
                    abstain: false,
                    reason: None,
                    provider,
                    model,
                    latency_ms,
                    evidence,
                    question_fingerprint: fingerprint.clone(),
                }
            } else {
                abstain_result_with_metadata(
                    "invalid_choice",
                    "provider returned an option outside the request",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                )
            }
        }
        DecisionOutcome::Abstain { reason } => abstain_result_with_metadata(
            "provider_abstain",
            &redact_text(&reason, 240),
            provider,
            model,
            confidence,
            latency_ms,
            evidence,
            fingerprint.clone(),
        ),
        DecisionOutcome::Rank { .. } => abstain_result_with_metadata(
            "unsupported_outcome",
            "provider returned a ranking instead of a single advisory choice",
            provider,
            model,
            confidence,
            latency_ms,
            evidence,
            fingerprint.clone(),
        ),
    };
    finish_result(runtime, agent_id, context, &args, &fingerprint, result).await
}

async fn finish_result(
    runtime: &RuntimeHandle,
    agent_id: &str,
    context: &ToolExecutionContext,
    args: &AdvisoryDecisionArgs,
    question_fingerprint: &str,
    result: AdvisoryDecisionResult,
) -> Result<crate::tool::ToolResult> {
    let decision_id = stable_decision_id(
        agent_id,
        context.turn_id.as_deref(),
        context.message_id.as_deref(),
        context.effective_work_item_id.as_deref(),
        context.tool_call_id.as_deref(),
        question_fingerprint,
    );
    let error_class = result
        .reason
        .as_deref()
        .and_then(|reason| reason.split_once(':').map(|(class, _)| class.to_string()));
    let event = DecisionAdvisoryCompletedEvent {
        decision_id,
        agent_id: agent_id.to_string(),
        turn_id: context.turn_id.clone(),
        message_id: context.message_id.clone(),
        work_item_id: context.effective_work_item_id.clone(),
        request_fingerprint: question_fingerprint.to_string(),
        provider: result.provider.clone(),
        model: result.model.clone(),
        latency_ms: result.latency_ms,
        token_count: None,
        cost_usd: None,
        outcome: result.outcome.clone(),
        choice: result.choice.clone(),
        confidence: result.confidence,
        abstain: result.abstain,
        fallback: result.outcome == "fallback",
        timeout: error_class.as_deref() == Some("timeout"),
        error_class,
        reason: result.reason.as_deref().and_then(|reason| {
            crate::decision_telemetry::safe_value(Some(&Value::String(reason.to_string())), 512)
        }),
        evidence: result.evidence.clone(),
        recorded_at: chrono::Utc::now(),
    };
    if let Err(error) = runtime.append_decision_advisory_event(&event).await {
        tracing::warn!(error = %error, "failed to append decision advisory telemetry");
    }
    let audit = json!({
        "agent_id": agent_id,
        "work_item_id": context.effective_work_item_id,
        "question_fingerprint": question_fingerprint,
        "options_summary": args
            .options
            .iter()
            .map(|option| redact_text(option, 120))
            .collect::<Vec<_>>(),
        "provider": result.provider,
        "model": result.model,
        "latency_ms": result.latency_ms,
        "outcome": result.outcome,
        "abstain": result.abstain,
        "reason": result.reason,
        "confidence": result.confidence,
        "state_policy": "truncated_redacted_not_recorded",
        "question_chars": args.question.chars().count(),
    });
    if let Err(error) = runtime
        .append_audit_event("advisory_decision_tool", audit)
        .await
    {
        tracing::warn!(error = %error, "failed to append advisory decision audit event");
    }
    Ok(serialize_success(NAME, &result)?)
}
