use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::runtime::ViewImageObservationCacheKey;
use crate::system::ExecutionScopeKind;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::tool::ToolError;
use crate::types::AuthorityClass;
use crate::types::ViewImageReferenceSize;
use crate::types::ViewImageResult;
use crate::types::ViewImageSelectedMode;
use crate::types::ViewImageVisionSelection;
use crate::types::ViewImageVisualReference;
use anyhow::anyhow;
use anyhow::Result;
use chrono::Utc;
pub(crate) use holon_provider::provider::image_delivery::MAX_VIEW_IMAGE_BYTES as MAX_IMAGE_BYTES;
use serde_json::json;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub(crate) use holon_tools::builtins::view_image::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ViewImageArgs = parse_tool_args(NAME, input)?;
    let path = validate_non_empty(args.path, NAME, "path")?;
    let prompt = validate_non_empty(args.prompt, NAME, "prompt")?;
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    let resolved_path = execution.workspace.resolve_read_path(&path)?;
    let image = read_visual_reference(&resolved_path)?;
    let mut visual_reference = image.visual_reference;
    visual_reference.workspace_uri = Some(resolved_path.to_string_lossy().into_owned());
    let prepared = runtime
        .prepare_view_image_observation(args.model.as_deref())
        .await?;
    let resolved_selection = prepared.route_selection.clone();
    let vision_selection = resolved_selection.selection.clone();
    if vision_selection.selected_mode == ViewImageSelectedMode::Unavailable {
        return Err(vision_adapter_unavailable(vision_selection));
    }
    let cache_key = observation_cache_key(
        &visual_reference,
        &prompt,
        vision_selection
            .resolved_model
            .as_deref()
            .ok_or_else(|| anyhow!("ViewImage selection missing canonical route"))?,
        &prepared.endpoint_config_identity,
    );
    if let Some(observation) = runtime.cached_view_image_observation(&cache_key).await {
        let selected_mode = vision_selection.selected_mode.clone();
        return serialize_success(
            NAME,
            &ViewImageResult {
                cache_hit: true,
                visual_reference,
                observation,
                selected_mode,
                vision_selection,
                summary_text: Some("ViewImage reused cached visual observation".to_string()),
            },
        );
    }
    let generation_runtime = runtime.clone();
    let generation_selection = prepared;
    let generation_media_type = visual_reference.mime.clone();
    let generation_bytes = image.bytes;
    let generation_result = generate_and_parse_view_image_observation(
        &prompt,
        &visual_reference,
        &vision_selection,
        move |request_prompt| {
            let runtime = generation_runtime.clone();
            let selection = generation_selection.clone();
            let media_type = generation_media_type.clone();
            let bytes = generation_bytes.clone();
            async move {
                runtime
                    .generate_view_image_observation(
                        &selection,
                        &request_prompt,
                        &media_type,
                        &bytes,
                    )
                    .await
            }
        },
    )
    .await;
    let observation = match generation_result {
        Ok(observation) => {
            runtime
                .record_view_image_candidate_success(&resolved_selection)
                .await;
            observation
        }
        Err(ViewImageObservationGenerationError::Provider {
            initial_validation_error: None,
            error,
            ..
        }) => {
            let health = runtime
                .record_view_image_candidate_provider_failure(&resolved_selection, &error)
                .await;
            return Err(vision_observation_failed(&vision_selection, error, health));
        }
        Err(ViewImageObservationGenerationError::Provider {
            initial_validation_error: Some(first_validation_error),
            initial_response_preview,
            error,
        }) => {
            let health = runtime
                .record_view_image_candidate_provider_failure(&resolved_selection, &error)
                .await;
            return Err(vision_observation_failed_after_validation_retry(
                &vision_selection,
                &first_validation_error,
                initial_response_preview.as_deref().unwrap_or(""),
                error,
                health,
            ));
        }
        Err(ViewImageObservationGenerationError::Validation {
            first_raw,
            second_raw,
            first_error,
            second_error,
        }) => {
            let candidate_health = runtime
                .record_view_image_candidate_protocol_failure(&resolved_selection)
                .await;
            return Err(vision_observation_validation_failed(
                &vision_selection,
                &first_raw,
                &second_raw,
                &first_error,
                second_error,
                resolved_selection
                    .selected_route
                    .as_ref()
                    .map(|route| route.as_string()),
                candidate_health,
            ));
        }
    };
    runtime
        .cache_view_image_observation(cache_key, observation.clone())
        .await;
    let summary_text = Some(format!(
        "ViewImage generated a visual observation with {}/{} for {}",
        vision_selection
            .vision_provider
            .as_deref()
            .unwrap_or("unknown-provider"),
        vision_selection
            .vision_model
            .as_deref()
            .unwrap_or("unknown-model"),
        visual_reference.mime
    ));
    serialize_success(
        NAME,
        &ViewImageResult {
            cache_hit: false,
            visual_reference,
            observation,
            selected_mode: vision_selection.selected_mode.clone(),
            vision_selection,
            summary_text,
        },
    )
}

fn vision_provider_failure_details(
    selection: &ViewImageVisionSelection,
    error: &anyhow::Error,
    health: Option<crate::runtime::ViewImageCandidateFailureStatus>,
) -> Value {
    json!({
        "selection": compact_vision_selection(selection),
        "provider_failure": crate::provider::classify_provider_error(error),
        "provider_error_code": crate::provider::provider_error_code(error),
        "provider_error": crate::provider::sanitized_provider_error(error),
        "failure_stage": "provider_generation",
        "selected_mode": selection.selected_mode,
        "selection_reason": selection.selection_reason,
        "vision_provider": selection.vision_provider,
        "vision_model": selection.vision_model,
        "candidate_health": {
            "model_ref": selection.resolved_model,
            "consecutive_protocol_failures": health.map(|status| status.consecutive_protocol_failures),
            "failure_threshold": crate::runtime::VIEW_IMAGE_PROTOCOL_FAILURE_THRESHOLD,
            "circuit_open": health.is_some_and(|status| status.circuit_open),
            "cooldown_seconds": health.map_or(0, |status| status.cooldown_seconds),
        },
    })
}

fn vision_observation_failed(
    selection: &ViewImageVisionSelection,
    error: anyhow::Error,
    health: Option<crate::runtime::ViewImageCandidateFailureStatus>,
) -> anyhow::Error {
    let failure = crate::provider::classify_provider_error(&error);
    ToolError::new(
        "vision_observation_failed",
        "ViewImage provider could not generate a visual observation",
    )
    .with_retryable(failure.disposition == crate::provider::RetryDisposition::Retryable)
    .with_details(vision_provider_failure_details(selection, &error, health))
    .with_recovery_hint("configure an OpenAI-compatible vision model with valid credentials, or retry after provider failures are resolved")
    .into()
}

fn vision_observation_failed_after_validation_retry(
    selection: &ViewImageVisionSelection,
    first_validation_error: &anyhow::Error,
    initial_response_preview: &str,
    error: anyhow::Error,
    health: Option<crate::runtime::ViewImageCandidateFailureStatus>,
) -> anyhow::Error {
    let failure = crate::provider::classify_provider_error(&error);
    let mut details = vision_provider_failure_details(selection, &error, health);
    details["initial_validation_error"] = json!(first_validation_error.to_string());
    details["initial_response_preview"] = json!(bounded_response_preview(initial_response_preview));
    ToolError::new(
        "vision_observation_failed",
        "ViewImage provider failed after a response correction retry",
    )
    .with_retryable(failure.disposition == crate::provider::RetryDisposition::Retryable)
    .with_details(details)
    .with_recovery_hint(
        "check the configured vision provider or model credentials and retry after provider failures are resolved",
    )
    .into()
}

fn vision_observation_validation_failed(
    selection: &ViewImageVisionSelection,
    first_raw: &str,
    second_raw: &str,
    first_error: &anyhow::Error,
    second_error: anyhow::Error,
    candidate_model_ref: Option<String>,
    candidate_health: Option<crate::runtime::ViewImageCandidateFailureStatus>,
) -> anyhow::Error {
    ToolError::new(
        "vision_observation_failed",
        format!(
            "ViewImage received invalid visual observations after one correction retry: {second_error}"
        ),
    )
    .with_details(json!({
        "failure_stage": "response_validation",
        "selected_mode": selection.selected_mode,
        "selection_reason": selection.selection_reason,
        "vision_provider": selection.vision_provider,
        "vision_model": selection.vision_model,
        "retry_attempted": true,
        "initial_validation_error": first_error.to_string(),
        "final_validation_error": second_error.to_string(),
        "initial_response_preview": bounded_response_preview(first_raw),
        "final_response_preview": bounded_response_preview(second_raw),
        "candidate_health": {
            "model_ref": candidate_model_ref,
            "consecutive_protocol_failures": candidate_health.map(|health| health.consecutive_protocol_failures),
            "failure_threshold": crate::runtime::VIEW_IMAGE_PROTOCOL_FAILURE_THRESHOLD,
            "circuit_open": candidate_health.is_some_and(|health| health.circuit_open),
            "cooldown_seconds": candidate_health.map_or(0, |health| health.cooldown_seconds),
        },
    }))
    .with_recovery_hint(
        "the vision endpoint returned an incompatible response twice; inspect the selected model's JSON compatibility or configure another vision model",
    )
    .into()
}

fn observation_cache_key(
    visual_reference: &ViewImageVisualReference,
    prompt: &str,
    canonical_route: &str,
    endpoint_config_identity: &str,
) -> ViewImageObservationCacheKey {
    ViewImageObservationCacheKey {
        canonical_route: canonical_route.to_string(),
        endpoint_config_identity: endpoint_config_identity.to_string(),
        contract_version: crate::config::VIEW_IMAGE_CONTRACT_VERSION.to_string(),
        visual_reference_id: visual_reference.id.clone(),
        prompt: prompt.to_string(),
        observation_schema: VISUAL_OBSERVATION_SCHEMA.to_string(),
        generation_policy: VIEW_IMAGE_OBSERVATION_GENERATION_POLICY.to_string(),
    }
}

pub(crate) fn read_visual_reference(path: &Path) -> Result<ReadImage> {
    crate::diagnostics::resource_io::admit_path(path)?;
    let file_metadata = fs::metadata(path).map_err(|error| {
        invalid_tool_input(
            NAME,
            "ViewImage `path` must refer to a readable local image file",
            json!({
                "field": "path",
                "path": path,
                "io_error": error.to_string(),
            }),
            "provide a readable PNG, JPEG, GIF, or WebP image file path",
        )
    })?;
    if !file_metadata.is_file() {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` must refer to a regular file",
            json!({
                "field": "path",
                "path": path,
                "validation_error": "not a regular file",
            }),
            "provide a path to a readable PNG, JPEG, GIF, or WebP image file",
        ));
    }
    let byte_count = file_metadata.len();
    if byte_count > MAX_IMAGE_BYTES {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` exceeds the maximum supported image file size",
            json!({
                "field": "path",
                "path": path,
                "byte_count": byte_count,
                "max_byte_count": MAX_IMAGE_BYTES,
            }),
            "provide a smaller local image file",
        ));
    }
    let mut file = File::open(path).map_err(|error| {
        ToolError::new(
            "image_read_failed",
            format!("failed to read image file: {error}"),
        )
        .with_details(json!({
            "path": path,
            "io_error": error.to_string(),
        }))
        .with_recovery_hint("provide a readable local image file path")
    })?;
    crate::diagnostics::resource_io::admit_file_bytes(file.metadata()?.len())?;
    #[cfg(test)]
    crate::diagnostics::resource_io::run_after_file_metadata();
    let mut bytes = Vec::new();
    let mut reader = file.by_ref().take(MAX_IMAGE_BYTES + 1);
    crate::diagnostics::resource_io::read_to_end(&mut reader, &mut bytes)
        .unwrap_or_else(|| reader.read_to_end(&mut bytes))
        .map_err(|error| {
            ToolError::new(
                "image_read_failed",
                format!("failed to read image file: {error}"),
            )
            .with_details(json!({
                "path": path,
                "io_error": error.to_string(),
            }))
            .with_recovery_hint("provide a readable local image file path")
        })?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` exceeds the maximum supported image file size",
            json!({
                "field": "path",
                "path": path,
                "byte_count": bytes.len(),
                "max_byte_count": MAX_IMAGE_BYTES,
            }),
            "provide a smaller local image file",
        ));
    }
    let Some(header) = ImageHeader::parse(&bytes) else {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` must refer to a supported image file",
            json!({
                "field": "path",
                "path": path,
                "validation_error": "unsupported image header",
                "supported_media_types": ["image/png", "image/jpeg", "image/gif", "image/webp"],
            }),
            "provide a PNG, JPEG, GIF, or WebP image file",
        ));
    };
    if let (Some(width), Some(height)) = (header.width, header.height) {
        let pixels = u64::from(width) * u64::from(height);
        if pixels > MAX_IMAGE_PIXELS {
            return Err(invalid_tool_input(
                NAME,
                "ViewImage `path` exceeds the maximum supported decoded pixel count",
                json!({
                    "field": "path",
                    "path": path,
                    "width": width,
                    "height": height,
                    "pixel_count": pixels,
                    "max_pixel_count": MAX_IMAGE_PIXELS,
                }),
                "provide an image with fewer decoded pixels",
            ));
        }
    }
    crate::diagnostics::resource_io::admit_cloned_value(&path)?;
    crate::diagnostics::resource_io::charge_owned_metadata(1024, 12)?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let visual_reference = ViewImageVisualReference {
        kind: "visual_reference".to_string(),
        id: format!("vis_{}", &sha256[..16]),
        path: path.to_path_buf(),
        workspace_uri: None,
        sha256,
        mime: header.media_type.to_string(),
        byte_count: bytes.len() as u64,
        size: ViewImageReferenceSize {
            width: header.width,
            height: header.height,
        },
        created_at: Utc::now(),
    };
    Ok(ReadImage {
        visual_reference,
        bytes,
    })
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn reads_visual_reference() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("image.png");
        let bytes = png_header_bytes(320, 240);
        fs::write(&path, &bytes).unwrap();

        let image = read_visual_reference(&path).unwrap();
        let reference = image.visual_reference;

        assert_eq!(reference.kind, "visual_reference");
        assert_eq!(reference.mime, "image/png");
        assert_eq!(reference.byte_count, bytes.len() as u64);
        assert_eq!(reference.size.width, Some(320));
        assert_eq!(reference.size.height, Some(240));
        assert_eq!(reference.sha256, format!("{:x}", Sha256::digest(&bytes)));
        assert!(reference.id.starts_with("vis_"));
        assert_eq!(image.bytes, bytes);
    }

    #[tokio::test]
    async fn classifies_provider_failure_during_correction_retry() {
        let mut call_count = 0;

        let error = generate_and_parse_view_image_observation(
            "Describe the image.",
            &test_visual_reference(),
            &test_vision_selection(),
            |_request_prompt| {
                call_count += 1;
                let response = if call_count == 1 {
                    Ok("not valid visual observation".to_string())
                } else {
                    Err(anyhow!("provider unavailable on retry"))
                };
                async move { response }
            },
        )
        .await
        .unwrap_err();

        assert_eq!(call_count, 2);
        let ViewImageObservationGenerationError::Provider {
            initial_validation_error,
            initial_response_preview,
            error,
        } = error
        else {
            panic!(
                "expected the correction retry provider failure to be classified as provider error"
            );
        };
        assert_eq!(
            initial_validation_error
                .expect("initial response should fail validation")
                .to_string(),
            "vision adapter response did not contain JSON"
        );
        assert_eq!(
            initial_response_preview.as_deref(),
            Some("not valid visual observation")
        );
        assert_eq!(error.to_string(), "provider unavailable on retry");

        let tool_error =
            crate::tool::tool_error_from_anyhow(&vision_observation_failed_after_validation_retry(
                &test_vision_selection(),
                &anyhow!("initial response was invalid"),
                "not valid visual observation",
                anyhow!("provider unavailable on retry"),
                None,
            ));
        assert_eq!(tool_error.kind, "vision_observation_failed");
        assert_eq!(
            tool_error.details.as_ref().unwrap()["failure_stage"],
            "provider_generation"
        );
        assert_eq!(
            tool_error.details.as_ref().unwrap()["initial_validation_error"],
            "initial response was invalid"
        );
        assert_eq!(
            tool_error.details.as_ref().unwrap()["initial_response_preview"],
            "not valid visual observation"
        );
    }

    #[test]
    fn observation_cache_key_includes_schema_and_generation_policy() {
        let reference = test_visual_reference();

        let key = observation_cache_key(
            &reference,
            "What is visible?",
            "openai@default/gpt-5.4",
            "config-a",
        );

        assert_eq!(key.visual_reference_id, reference.id);
        assert_eq!(key.prompt, "What is visible?");
        assert_eq!(key.observation_schema, "visual_observation.v1");
        assert_eq!(key.generation_policy, "openai-compatible-image-input.v1");
    }

    #[test]
    fn observation_cache_key_is_route_config_and_contract_aware() {
        let reference = test_visual_reference();
        let key =
            observation_cache_key(&reference, "Inspect", "openai@default/vision-a", "config-a");
        assert_eq!(
            key,
            observation_cache_key(&reference, "Inspect", "openai@default/vision-a", "config-a")
        );
        assert_ne!(
            key,
            observation_cache_key(&reference, "Inspect", "openai@other/vision-a", "config-a")
        );
        assert_ne!(
            key,
            observation_cache_key(&reference, "Inspect", "openai@default/vision-b", "config-a")
        );
        assert_ne!(
            key,
            observation_cache_key(&reference, "Inspect", "openai@default/vision-a", "config-b")
        );
        let mut older = key.clone();
        older.contract_version = "view_image.v1".to_string();
        assert_ne!(key, older);
    }

    #[test]
    fn view_image_provider_failures_preserve_failure_classification() {
        use crate::provider::{
            provider_transport_error_with_code, ProviderFailureClassification, ProviderFailureKind,
            RetryDisposition,
        };
        for (kind, expected, disposition) in [
            (
                ProviderFailureKind::RateLimited,
                "rate_limited",
                RetryDisposition::Retryable,
            ),
            (
                ProviderFailureKind::Connection,
                "connection",
                RetryDisposition::Retryable,
            ),
            (
                ProviderFailureKind::AuthError,
                "auth_error",
                RetryDisposition::FailFast,
            ),
            (
                ProviderFailureKind::ContractError,
                "contract_error",
                RetryDisposition::FailFast,
            ),
        ] {
            for correction_retry in [false, true] {
                let error = provider_transport_error_with_code(
                    ProviderFailureClassification { kind, disposition },
                    None, None, None, "provider rejected observation at https://private-endpoint.example/api?token=secret",
                );
                let error = if correction_retry {
                    vision_observation_failed_after_validation_retry(
                        &test_vision_selection(),
                        &anyhow!("invalid JSON"),
                        "invalid response",
                        error,
                        None,
                    )
                } else {
                    vision_observation_failed(&test_vision_selection(), error, None)
                };
                let error = crate::tool::tool_error_from_anyhow(&error);
                assert!(!error.render().contains("private-endpoint"));
                assert!(!error.render().contains("token=secret"));
                assert_eq!(error.retryable, disposition == RetryDisposition::Retryable);
                let details = error.details.unwrap();
                assert_eq!(details["provider_failure"]["kind"], expected);
                assert_eq!(
                    details["selection"]["resolved_model"],
                    "openai@default/gpt-5.4"
                );
            }
        }
    }

    #[test]
    fn rejects_missing_file_as_invalid_tool_input() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.png");

        let error = crate::tool::tool_error_from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("readable local image file"));
    }

    #[test]
    fn rejects_non_image_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("not-image.txt");
        fs::write(&path, b"hello").unwrap();

        let error = crate::tool::tool_error_from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("supported image file"));
    }

    #[test]
    fn rejects_oversized_file_before_reading_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.png");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();

        let error = crate::tool::tool_error_from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("file size"));
    }

    #[test]
    fn rejects_oversized_decoded_pixel_count() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("huge.png");
        fs::write(&path, png_header_bytes(100_000, 100_000)).unwrap();

        let error = crate::tool::tool_error_from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("pixel count"));
    }

    fn png_header_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn test_visual_reference() -> ViewImageVisualReference {
        ViewImageVisualReference {
            kind: "visual_reference".to_string(),
            id: "vis_test".to_string(),
            path: Path::new("image.png").to_path_buf(),
            workspace_uri: None,
            sha256: "sha256".to_string(),
            mime: "image/png".to_string(),
            byte_count: 10,
            size: ViewImageReferenceSize {
                width: Some(1),
                height: Some(1),
            },
            created_at: Utc::now(),
        }
    }

    #[test]
    fn provider_failure_payload_is_bounded_redacted_and_keeps_the_selected_candidate() {
        let mut selection = test_vision_selection();
        selection.candidates = (0..1020)
            .map(|index| crate::types::ViewImageVisionCandidate {
                provider: "openai".into(),
                model: format!("vision-{index}"),
                model_ref: format!("openai@default/vision-{index}"),
                image_input: true,
                reason: "model_advertises_image_input".into(),
            })
            .collect();
        selection.resolved_model = Some("openai@default/vision-1019".into());
        let raw = format!(
            "invalid_request_error Thinking mode does not support this tool_choice https://private-endpoint.example/?token=secret Authorization: Bearer secret-token api_key=secret-key sk-bare-secret {}",
            "长".repeat(1000),
        );
        for correction_retry in [false, true] {
            let error = if correction_retry {
                vision_observation_failed_after_validation_retry(
                    &selection,
                    &anyhow!("invalid JSON"),
                    "invalid response",
                    anyhow!(raw.clone()),
                    None,
                )
            } else {
                vision_observation_failed(&selection, anyhow!(raw.clone()), None)
            };
            let error = crate::tool::tool_error_from_anyhow(&error);
            let rendered = error.render();
            for secret in [
                "private-endpoint",
                "token=secret",
                "secret-token",
                "secret-key",
                "sk-bare-secret",
            ] {
                assert!(!rendered.contains(secret), "must redact {secret}");
            }
            let details = error.details.unwrap();
            let candidates = details["selection"]["candidates"].as_array().unwrap();
            assert_eq!(candidates.len(), 10);
            assert_eq!(details["selection"]["candidates_total"], 1020);
            assert_eq!(details["selection"]["candidates_truncated"], true);
            assert_eq!(
                candidates[9]["model_ref"],
                selection.resolved_model.as_deref().unwrap()
            );
            let summary = details["provider_error"].as_str().unwrap();
            assert!(summary.contains("Thinking mode does not support this tool_choice"));
            assert!(summary.chars().count() <= 500);
            assert!(rendered.len() < 10_000);
        }
        selection.candidates.clear();
        assert_eq!(
            compact_vision_selection(&selection)["candidates_truncated"],
            false
        );
    }

    fn test_vision_selection() -> ViewImageVisionSelection {
        ViewImageVisionSelection {
            requested_model: None,
            resolved_model: Some("openai@default/gpt-5.4".to_string()),
            selection_source: Some("automatic".to_string()),
            contract_version: Some(crate::config::VIEW_IMAGE_CONTRACT_VERSION.to_string()),
            selected_mode: ViewImageSelectedMode::VisionAdapter,
            vision_provider: Some("openai".to_string()),
            vision_model: Some("gpt-5.4".to_string()),
            selection_reason: "test".to_string(),
            primary_provider: Some("openai".to_string()),
            primary_model: Some("gpt-5.4".to_string()),
            candidates: Vec::new(),
        }
    }
}
