use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::{material::tests::response, ProviderRequestMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "generated-image".into(),
        identity_incarnation: 1,
    }
}

#[tokio::test]
async fn generated_image_public_error_keeps_material_and_typed_classification() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope())?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let (url, server) = response(b"{invalid JSON", "application/json").await?;
    let home = tempfile::tempdir()?;
    let provider = OpenAiChatCompletionsProvider {
        client: Client::new(),
        provider_id: "openai".into(),
        route_provider: "openai".into(),
        base_url: url,
        api_key: None,
        quota_identity: None,
        model: "gpt-image-2".into(),
        max_output_tokens: 128,
        max_output_tokens_parameter: "max_tokens",
        context_window_tokens: None,
        reasoning_effort: None,
        service_tier: None,
        trace_home_dir: home.path().into(),
        continuation: Default::default(),
    };
    let error = provider
        .generate_image(ProviderGenerateImageRequest {
            prompt: "image".into(),
            size: None,
            aspect_ratio: None,
            resolution: None,
            background: None,
            output_format: None,
            material: material.clone(),
        })
        .await
        .unwrap_err();
    server.await??;
    let expected = invalid_response_error("invalid OpenAI Images JSON", "malformed");
    let actual_class = crate::provider::retry::classify_provider_error(&error);
    let expected_class = crate::provider::retry::classify_provider_error(&expected);
    assert_eq!(
        (actual_class.kind, actual_class.disposition),
        (expected_class.kind, expected_class.disposition)
    );
    assert!(error
        .downcast_ref::<crate::provider::retry::ProviderTransportError>()
        .is_some());
    let weak = Arc::downgrade(&source.guard()?);
    drop((provider, source, material));
    assert!(weak.upgrade().is_some());
    drop(error);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn generated_image_trace_refuses_before_http_or_trace_capture() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let source = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        (scope.agent_id.len() as u64 + 256) * 8,
        64,
    )?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let home = tempfile::tempdir()?;
    let trace = ProviderHttpTrace::failure_only_for_tests(home.path());
    let error = send_openai_images_request(
        &Client::new(),
        "http://127.0.0.1:1/".into(),
        &json!({"model":"test","prompt":"not captured"}),
        Vec::new(),
        "test",
        Some(&trace),
        None,
        &material,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    assert!(error
        .downcast_ref::<crate::provider::retry::ProviderTransportError>()
        .is_none());
    assert!(home.path().read_dir()?.next().is_none());
    assert!(source.finish().is_err());
    drop((error, trace, material, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn generated_image_http_json_and_codex_stream_keep_explicit_material() -> Result<()> {
    for streaming in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = ResourceReadMaterial::new(&ledger, &scope())?;
        let material = ProviderRequestMaterial::scoped(source.clone());
        let body = if streaming {
            format!(
                "data: {}\n\n",
                json!({
                    "type": "response.completed",
                    "response": {
                        "id":"resp-image", "status":"completed",
                        "output":[
                            {"type":"image_generation_call","result":"YWJj"},
                            {"type":"message","role":"assistant","content":[
                                {"type":"output_text","text":"done"}
                            ]}
                        ]
                    }
                })
            )
        } else {
            json!({"data":[{"b64_json":"YWJj","mime_type":"image/png"}]}).to_string()
        };
        let (url, server) = response(
            body.as_bytes(),
            if streaming {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .await?;
        let request = json!({"model":"test","prompt":"test"});
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let images = if streaming {
            send_openai_codex_image_generation_request(
                &Client::new(),
                url,
                &request,
                Vec::new(),
                None,
                None,
                &material,
            )
            .await?
        } else {
            send_openai_images_request(
                &Client::new(),
                url,
                &request,
                Vec::new(),
                "test",
                None,
                None,
                &material,
            )
            .await?
        };
        server.await??;
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].bytes.as_ref(), b"abc");
        assert_eq!(images[0].mime.as_deref(), Some("image/png"));
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let last = images[0].bytes.slice(1..);
        let weak = Arc::downgrade(&source.guard()?);
        drop((images, material, source));
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn generated_image_decode_refuses_quota_without_invalid_response_reclassification() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let source = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 64)?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let error = parse_openai_images_response_with_material(
        json!({"data":[{"b64_json":"YWJj"}]}),
        &material,
    )
    .unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    assert!(
        !error.to_string().contains("invalid OpenAI Images base64"),
        "{error:#}"
    );
    assert!(source.finish().is_err());
    drop((material, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn generated_image_wire_excludes_material_and_retains_standalone_semantics() -> Result<()> {
    let request = ProviderGenerateImageRequest {
        prompt: "test".into(),
        size: None,
        aspect_ratio: None,
        resolution: None,
        background: None,
        output_format: None,
        material: Default::default(),
    };
    assert!(serde_json::to_value(&request)?.get("material").is_none());
    for (dialect, model) in [
        (OpenAiImagesDialect::OpenAi, "gpt-image-2"),
        (OpenAiImagesDialect::Xai, "grok-imagine-image"),
    ] {
        let expected = build_images_request(dialect, model, &request)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = ResourceReadMaterial::new(&ledger, &scope())?;
        let mut scoped = request.clone();
        scoped.material = ProviderRequestMaterial::scoped(source.clone());
        assert_eq!(build_images_request(dialect, model, &scoped)?, expected);
        drop((scoped, source));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let error = parse_openai_images_response(json!({"data":[{"b64_json":"!"}]})).unwrap_err();
    assert!(
        error.to_string().contains("invalid OpenAI Images base64"),
        "{error:#}"
    );
    Ok(())
}
