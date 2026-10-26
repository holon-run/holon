use super::super::plan::plan_openai_responses_request;
use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::{material::tests::response, ProviderRequestMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    token_estimate::estimate_json_tokens,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "continuation-material".into(),
        identity_incarnation: 1,
    }
}

fn request(material: ProviderRequestMaterial) -> ProviderTurnRequest {
    let mut request = ProviderTurnRequest::plain("system", Vec::new(), Vec::new());
    request.material = material;
    request.continuation_scope_id = ContinuationScopeId::new("continuation-material");
    request
}

fn body() -> Value {
    json!({"model":"test","instructions":"system","store":true,"input":[
        {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}
    ]})
}

fn plan(
    body: Value,
    request: &ProviderTurnRequest,
    state: &Arc<Mutex<OpenAiContinuationState>>,
    response_id: bool,
) -> Result<OpenAiRequestPlan> {
    plan_openai_responses_request(
        body, request, state, response_id,
        OpenAiResponsesContinuationContract::StoreResponsesAndOmitInstructionsWithPreviousResponseId,
    )
}

fn window(
    material: ProviderRequestMaterial,
    request: &ProviderTurnRequest,
    body: &Value,
) -> OpenAiProviderWindow {
    let items = body["input"].as_array().unwrap().clone();
    OpenAiProviderWindow {
        material,
        response_id: Some("resp-previous".into()),
        request_shape: request_shape_without_input(body, request),
        append_match_items: openai_append_match_input_items(&items),
        items,
        replay_loss_reason: None,
        latest_compaction_index: None,
        latest_input_tokens: 100_000,
        generation: 1,
    }
}

#[test]
fn continuation_snapshot_and_candidate_borrow_original_material_until_last_holder() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope())?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let request = request(material.clone());
    let state = Arc::new(Mutex::new(OpenAiContinuationState::default()));
    let plan = plan(body(), &request, &state, true)?;
    let parsed = super::super::parse::parse_openai_response_with_transport_state(json!({
        "id":"response-1","output":[{"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"answer"}]}]
    }))?;
    update_openai_continuation(
        &state,
        plan.scope.clone(),
        material.cloned(&plan.request_shape)?,
        material.cloned(&plan.append_match_input)?,
        material.cloned(&plan.provider_input)?,
        None,
        &parsed,
        &material,
    )?;
    let snapshot = lock_openai_continuation(&state)
        .windows
        .get(request.continuation_scope_id.as_ref().unwrap())
        .unwrap()
        .clone();
    let before = source.finish()?;
    let second = snapshot.clone();
    assert!(Arc::ptr_eq(&snapshot, &second));
    let candidate = openai_provider_window_compaction_candidate(&snapshot).unwrap();
    assert_eq!(candidate.items.as_ptr(), snapshot.items.as_ptr());
    assert_eq!(source.finish()?, before);
    let weak = Arc::downgrade(&source.guard()?);
    drop((state, second, plan, parsed, request, material, source));
    assert!(weak.upgrade().is_some());
    drop(snapshot);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn continuation_plan_admits_exact_bytes_and_records_before_copy() -> Result<()> {
    let mut exact = None;
    for shortage in [None, Some(0), Some(1), Some(2)] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = if let Some((bytes, records)) = exact {
            ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                bytes - u64::from(shortage == Some(1)),
                records - usize::from(shortage == Some(2)),
            )?
        } else {
            ResourceReadMaterial::new(&ledger, &scope())?
        };
        let request = request(ProviderRequestMaterial::scoped(source.clone()));
        let state = Arc::new(Mutex::new(OpenAiContinuationState::default()));
        let result = plan(body(), &request, &state, true);
        if shortage == Some(1) || shortage == Some(2) {
            assert!(result.is_err());
            assert!(source.finish().is_err());
        } else {
            let plan = result?;
            let used = source.finish()?;
            if let Some(exact) = exact {
                assert_eq!(used, exact);
            }
            exact = Some(used);
            assert_eq!(plan.body["input"].as_array().unwrap().len(), 1);
            drop(plan);
        }
        assert!(lock_openai_continuation(&state).windows.is_empty());
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop((request, source, state));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn continuation_replay_copies_keep_store_instructions_and_owning_material() -> Result<()> {
    for response_id in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = ResourceReadMaterial::new(&ledger, &scope())?;
        let material = ProviderRequestMaterial::scoped(source.clone());
        let request = request(material.clone());
        let old_body = body();
        let prior = Arc::new(window(Default::default(), &request, &old_body));
        let state = Arc::new(Mutex::new(OpenAiContinuationState::default()));
        lock_openai_continuation(&state).windows.insert(
            request.continuation_scope_id.clone().unwrap(),
            prior.clone(),
        );
        let mut next = body();
        next["input"].as_array_mut().unwrap().push(json!({
            "type":"message","role":"user","content":[{"type":"input_text","text":"again"}]
        }));
        let plan = plan(next, &request, &state, response_id)?;
        assert_eq!(plan.body["store"], true);
        if response_id {
            assert_eq!(plan.body["previous_response_id"], "resp-previous");
            assert!(plan.body.get("instructions").is_none());
            let (replay, input) = plan.fallback_replay.as_ref().unwrap();
            assert_eq!(replay["instructions"], "system");
            assert!(replay.get("previous_response_id").is_none());
            assert_eq!(replay["input"].as_array().unwrap(), input);
            assert_eq!(input.len(), 2);
            assert_eq!(plan.provider_input.len(), 1);
        } else {
            assert_eq!(plan.body["instructions"], "system");
            assert_eq!(plan.provider_input.len(), 2);
        }
        assert_eq!(prior.items.len(), 1);
        let weak = Arc::downgrade(&source.guard()?);
        drop((prior, state, request, material, source));
        assert!(weak.upgrade().is_some());
        drop(plan);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn compact_material_refusal_is_not_optional_diagnostic_or_window_install() -> Result<()> {
    for request_plan in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 1024 * 1024, 64)?;
        let material = ProviderRequestMaterial::scoped(source.clone());
        let request = request(Default::default());
        let old_body = body();
        let prior = Arc::new(window(material.clone(), &request, &old_body));
        let state = Arc::new(Mutex::new(OpenAiContinuationState::default()));
        let key = request.continuation_scope_id.clone().unwrap();
        lock_openai_continuation(&state)
            .windows
            .insert(key.clone(), prior.clone());
        let weak = Arc::downgrade(&source.guard()?);
        let policy = OpenAiCompactionPolicy {
            trigger_input_tokens: 1,
        };
        let result = if request_plan {
            let mut next = body();
            next["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"message","content":"next"}));
            let mut plan = plan_openai_responses_request(
                next,
                &request,
                &state,
                true,
                OpenAiResponsesContinuationContract::Standard,
            )?;
            assert_eq!(
                plan.diagnostics.request_lowering_mode,
                "incremental_continuation"
            );
            plan.material = material.clone();
            let before = source.finish()?;
            assert!(maybe_compact_openai_request_plan(
                &state,
                &mut plan,
                OpenAiCompactionPolicy {
                    trigger_input_tokens: u64::MAX
                },
                &Client::new(),
                "http://127.0.0.1:1/compact".into(),
                Vec::new(),
                None,
                None,
                "test",
                "test",
                None,
            )
            .await?
            .is_none());
            assert_eq!(source.finish()?, before);
            maybe_compact_openai_request_plan(
                &state,
                &mut plan,
                policy,
                &Client::new(),
                "http://127.0.0.1:1/compact".into(),
                Vec::new(),
                None,
                None,
                "test",
                "test",
                None,
            )
            .await
        } else {
            let before = source.finish()?;
            assert!(maybe_compact_openai_provider_window(
                &state,
                Some(&key),
                &prior.request_shape,
                OpenAiCompactionPolicy {
                    trigger_input_tokens: u64::MAX
                },
                &Client::new(),
                "http://127.0.0.1:1/compact".into(),
                Vec::new(),
                None,
                None,
                "test",
                "test",
                None,
            )
            .await?
            .is_none());
            assert_eq!(source.finish()?, before);
            maybe_compact_openai_provider_window(
                &state,
                Some(&key),
                &prior.request_shape,
                policy,
                &Client::new(),
                "http://127.0.0.1:1/compact".into(),
                Vec::new(),
                None,
                None,
                "test",
                "test",
                None,
            )
            .await
        };
        let error = result.expect_err("material denial must propagate");
        assert!(error.to_string().contains("ResourceExhausted"));
        assert!(Arc::ptr_eq(
            lock_openai_continuation(&state).windows.get(&key).unwrap(),
            &prior
        ));
        assert_eq!(lock_openai_continuation(&state).next_generation, 0);
        drop((prior, state, request, source, material));
        assert!(weak.upgrade().is_some());
        drop(error);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn compact_http_success_and_typed_error_keep_original_acquisition() -> Result<()> {
    for success in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = ResourceReadMaterial::new(&ledger, &scope())?;
        let material = ProviderRequestMaterial::scoped(source.clone());
        let text = if success {
            r#"{"output":[{"type":"compaction_summary","encrypted_content":"opaque","id":"private"}]}"#
        } else {
            "not JSON"
        };
        let (url, server) = response(text.as_bytes(), "application/json").await?;
        let result = send_openai_compact_request(
            &Client::new(),
            url,
            json!({"model":"test","input":[]}),
            Vec::new(),
            None,
            None,
            "test",
            "test",
            &material,
        )
        .await;
        server.await??;
        let weak = Arc::downgrade(&source.guard()?);
        drop((source, material));
        assert!(weak.upgrade().is_some());
        match result {
            Ok(items) => {
                assert!(success);
                assert_eq!(items[0]["type"], "compaction");
                assert!(items[0].get("id").is_none());
                drop(items);
            }
            Err(error) => {
                assert!(!success);
                assert!(error
                    .downcast_ref::<crate::provider::ProviderTransportError>()
                    .is_some());
                drop(error);
            }
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn continuation_canonical_hash_and_borrowed_trigger_match_original_contract() {
    let value = json!({"z":[{"b":"x\n","a":1}], "a":true});
    assert_eq!(
        canonical_json(&value),
        r#"{"a":true,"z":[{"a":1,"b":"x\n"}]}"#
    );
    assert_eq!(
        canonical_json_hash(&value),
        sha256_hex(canonical_json(&value).as_bytes())
    );
    assert_eq!(
        estimate_compaction_json_tokens(&value),
        estimate_json_tokens(&value)
    );
    let request = request(Default::default());
    let previous = window(Default::default(), &request, &body());
    let state = Arc::new(Mutex::new(OpenAiContinuationState::default()));
    let mut plan = plan(body(), &request, &state, true).unwrap();
    plan.provider_input = vec![value];
    let joined = [previous.items.as_slice(), plan.provider_input.as_slice()].concat();
    let expected = estimate_openai_provider_payload_tokens(
        &plan.request_shape,
        openai_items_after_latest_compaction(&joined),
    );
    let trigger = openai_compaction_trigger_for_request_plan(
        &previous,
        &plan,
        OpenAiCompactionPolicy {
            trigger_input_tokens: expected,
        },
    )
    .unwrap();
    assert_eq!(trigger.estimated_input_tokens, Some(expected));
    assert!(openai_compaction_trigger_for_request_plan(
        &previous,
        &plan,
        OpenAiCompactionPolicy {
            trigger_input_tokens: expected + 1
        },
    )
    .is_none());
}
