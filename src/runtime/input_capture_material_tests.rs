use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;
use crate::types::{MessageBody, MessageKind, MessageOrigin, Priority};

const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65_536);
fn message(id: &str, body: MessageBody) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        body,
    );
    message.id = id.into();
    message
}
fn request(key: InputEvidenceKey) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([key]),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        byte_limit: LIMITS.0,
        record_limit: LIMITS.1,
    }
}

#[tokio::test]
async fn actual_selected_capture_is_pure_and_admits_caller_before_clones() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    // Stop and join the independent observer; do not hold its refresh mutex
    // while the real queue writer performs its foreground refresh.
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let large = runtime
        .enqueue(message(
            "unselected-input",
            MessageBody::Json {
                value: serde_json::json!({"large":"x".repeat(256 * 1024),"nodes":vec![0;512]}),
            },
        ))
        .await?;
    let selected = runtime
        .enqueue(message(
            "selected-input",
            MessageBody::Text {
                text: "small accepted input".into(),
            },
        ))
        .await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Input root missing")?;
    let domain = root
        .input_evidence
        .as_ref()
        .context("actual Input domain missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let selected_key = InputEvidenceKey::new(InputEvidenceFamily::Message, &selected.id);
    let wanted = request(selected_key.clone());
    let (captured, material) =
        domain.capture_for_request_with_material(&wanted, false, LIMITS, (0, 0), ledger)?;
    assert_eq!(captured.records.len(), 1);
    assert_eq!(captured.records[0].locator.key, selected_key);
    let original = domain.records.get(&selected_key).unwrap();
    assert!(Arc::ptr_eq(&captured.records[0], original));
    let peak = material.phase_peak();
    let own = material.material_estimate()?;
    assert!(
        peak.0 < 256 * 1024,
        "unselected body entered the phase allowance"
    );
    assert!(
        peak.1 < 512,
        "unselected native body entered the phase allowance"
    );
    let (large_bytes, large_nodes) = domain
        .records
        .get(&InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            &large.id,
        ))
        .unwrap()
        .resident_material_estimate()?;
    assert!(u64::try_from(large_bytes)? > peak.0 && large_nodes > peak.1);
    drop((captured, material));
    let prior = (1024 * 1024u64, 32usize);
    let guard = ledger.register_owned_bytes(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let physical = vec![7u8; usize::try_from(prior.0)?];
    let baseline = ledger.report()?.observed_bytes;
    let exact = (usize::try_from(prior.0 + peak.0)?, prior.1 + peak.1);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE messages RENAME TO input_capture_forbidden_messages;
         ALTER TABLE turn_records RENAME TO input_capture_forbidden_turns;
         ALTER TABLE tool_executions RENAME TO input_capture_forbidden_tools;
         ALTER TABLE transcript_entries RENAME TO input_capture_forbidden_transcript;
         ALTER TABLE briefs RENAME TO input_capture_forbidden_briefs;
         ALTER TABLE agent_states RENAME TO input_capture_forbidden_states;",
    )?;
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let error = domain
            .capture_for_request_with_material(&wanted, false, limits, prior, ledger)
            .err()
            .context("Input capture reset caller allowance")?;
        assert!(
            matches!(
                error.downcast_ref::<InputCaptureError>(),
                Some(InputCaptureError::ResourceExhausted)
            ),
            "{error:#}"
        );
        assert_eq!(
            ledger.report()?.observed_bytes,
            baseline,
            "failed allocation guard leaked"
        );
    }
    let (captured, material) =
        domain.capture_for_request_with_material(&wanted, false, exact, prior, ledger)?;
    assert_eq!(
        material.material_estimate()?,
        own,
        "caller prior entered retained own material"
    );
    assert_eq!(material.phase_peak(), peak);
    assert!(Arc::ptr_eq(&captured.records[0], original));
    let weak = Arc::downgrade(&material._guards[0]);
    drop(material);
    assert!(
        weak.upgrade().is_some(),
        "metadata guard did not reach captured wire owner"
    );
    drop(captured);
    assert!(weak.upgrade().is_none());
    drop((guard, physical));
    assert_eq!(ledger.report()?.observed_bytes, baseline - prior.0);
    Ok(())
}

#[tokio::test]
async fn actual_capture_refuses_foreign_ledger_and_successor_scope_before_any_new_guard(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let accepted = runtime
        .enqueue(message(
            "scope-input",
            MessageBody::Text {
                text: "actual".into(),
            },
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let input = root.input_evidence.as_ref().unwrap();
    let wanted = request(InputEvidenceKey::new(
        InputEvidenceFamily::Message,
        &accepted.id,
    ));
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let error = input
        .capture_for_request_with_material(&wanted, false, LIMITS, (0, 0), &foreign)
        .err()
        .context("foreign ledger was accepted")?;
    assert!(matches!(
        error.downcast_ref::<InputCaptureError>(),
        Some(InputCaptureError::ForeignMaterial)
    ));
    assert_eq!(foreign.report()?.observed_bytes, 0);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    // An adversarial read-only copy cannot reinterpret the original actual
    // guards as a successor's scope. This is never installed as a Ready root.
    let mut successor = input.as_ref().clone();
    successor.scope.identity_incarnation += 1;
    let error = successor
        .capture_for_request_with_material(&wanted, false, LIMITS, (0, 0), ledger)
        .err()
        .context("original guard was borrowed under successor scope")?;
    assert!(matches!(
        error.downcast_ref::<InputCaptureError>(),
        Some(InputCaptureError::ForeignMaterial)
    ));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn observation_capture_preserves_tombstone_and_missing_qualification_after_sql_denial(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let accepted = runtime
        .enqueue(message(
            "original-observation",
            MessageBody::Text {
                text: "actual".into(),
            },
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let input = root.input_evidence.as_ref().unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &accepted.id);
    let mut boundary = input.as_ref().clone();
    // Exercise the original private read-view tombstone boundary, using the
    // actual W1 scope/guards, without granting or publishing another body.
    boundary.records.remove(&key);
    boundary.coverage.mandatory_roots.remove(&key);
    boundary.coverage.unavailable.insert(key.clone());
    let wanted = request(key.clone());
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE messages RENAME TO observation_forbidden_messages")?;
    let (captured, material) =
        boundary.capture_for_request_with_material(&wanted, true, LIMITS, (0, 0), ledger)?;
    assert!(captured.lookup(&key)?.is_none());
    assert!(captured.records.is_empty());
    drop((captured, material));
    let missing = InputEvidenceKey::new(InputEvidenceFamily::Message, "never-qualified");
    let baseline = ledger.report()?.observed_bytes;
    let error = input
        .capture_for_request_with_material(&request(missing.clone()), true, LIMITS, (0, 0), ledger)
        .err()
        .context("missing key was treated as a tombstone")?;
    assert_eq!(
        error.downcast_ref::<InputEvidenceReadError>(),
        Some(&InputEvidenceReadError::NeedsPreparation(missing))
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn actual_full_input_capture_keeps_original_mandatory_sources_and_only_loans_same_arc(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let first = runtime
        .enqueue(message(
            "full-current-first",
            MessageBody::Text {
                text: "original source".into(),
            },
        ))
        .await?;
    let second = runtime
        .enqueue(message(
            "full-current-second",
            MessageBody::Json {
                value: serde_json::json!({"body":"x".repeat(64*1024)}),
            },
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let domain = root.input_evidence.as_ref().unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &first.id);
    let (mut loan, loan_hold) = domain.capture_for_request_with_material(
        &request(key.clone()),
        false,
        LIMITS,
        (0, 0),
        ledger,
    )?;
    assert_eq!(loan.records.len(), 1);
    let prior = loan_hold.material_estimate()?;
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE messages RENAME TO full_current_forbidden_messages;
         ALTER TABLE turn_records RENAME TO full_current_forbidden_turns;
         ALTER TABLE tool_executions RENAME TO full_current_forbidden_tools;
         ALTER TABLE transcript_entries RENAME TO full_current_forbidden_transcript;
         ALTER TABLE briefs RENAME TO full_current_forbidden_briefs;
         ALTER TABLE agent_states RENAME TO full_current_forbidden_states;",
    )?;
    let (full, hold) = domain.capture_with_material(ledger, LIMITS, prior, Some(&loan))?;
    assert!(full.records.iter().any(|r| r.locator.key == key));
    assert!(full
        .records
        .iter()
        .any(|r| r.locator.key == InputEvidenceKey::new(InputEvidenceFamily::Message, &second.id)));
    assert!(full
        .records
        .iter()
        .any(|r| Arc::ptr_eq(r, &loan.records[0])));
    let body = loan.records[0].resident_material_estimate()?;
    let own = hold.material_estimate()?;
    let additional = hold.additional_material_estimate()?;
    assert_eq!(own.0 - additional.0, u64::try_from(body.0)?);
    assert_eq!(own.1 - additional.1, body.1);
    assert!(
        additional.0 > 0 && additional.1 > 0,
        "new metadata was waived"
    );
    hold.validate_accounting(domain.scope(), ledger)?;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(hold.validate_accounting(domain.scope(), &foreign).is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    drop((full, hold));
    assert!(domain
        .capture_with_material(
            ledger,
            LIMITS,
            (u64::try_from(body.0)? - 1, body.1),
            Some(&loan)
        )
        .is_err());
    // An adversarial same-locator copy retains actual allocation accounting,
    // but never becomes a new qualified source in a publication.
    let incoming = ledger.register_owned_bytes(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(body.0)?,
            metadata_estimate_bytes: 256,
        },
    )?;
    let other = Arc::new((*loan.records[0]).clone());
    let physical = ledger.register_view(
        domain.scope(),
        InputMemoryCategory::Pins,
        std::slice::from_ref(&other),
    )?;
    drop(incoming);
    loan.records[0] = other;
    loan.budget_views.push(physical);
    let actual_prior = (
        prior.0.checked_mul(2).unwrap(),
        prior.1.checked_mul(2).unwrap(),
    );
    let (full, hold) = domain.capture_with_material(ledger, LIMITS, actual_prior, Some(&loan))?;
    assert_eq!(
        hold.material_estimate()?,
        hold.additional_material_estimate()?
    );
    assert!(!full
        .records
        .iter()
        .any(|r| Arc::ptr_eq(r, &loan.records[0])));
    assert_eq!(
        Arc::strong_count(&root),
        strong,
        "capture retained a publication"
    );
    let weak = Arc::downgrade(&hold._guards[0]);
    drop(full);
    assert!(weak.upgrade().is_some());
    drop(hold);
    assert!(weak.upgrade().is_none());
    drop((loan, loan_hold));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn actual_full_input_caller_joint_limits_and_retained_handoff_are_bounded() -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime
        .enqueue(message(
            "full-joint-input",
            MessageBody::Text {
                text: "actual full source".into(),
            },
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let domain = root.input_evidence.as_ref().unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let (full, hold) = domain.capture_with_material(ledger, LIMITS, (0, 0), None)?;
    let peak = hold.phase_peak();
    drop((full, hold));
    let bytes = vec![9u8; 128 * 1024];
    let prior = (u64::try_from(bytes.len())?, 16usize);
    let guard = ledger.register_owned_bytes(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let held = ledger.report()?.observed_bytes;
    let exact = (usize::try_from(prior.0 + peak.0)?, prior.1 + peak.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let error = domain
            .capture_with_material(ledger, limits, prior, None)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<InputCaptureError>(),
            Some(InputCaptureError::ResourceExhausted)
        ));
        assert_eq!(ledger.report()?.observed_bytes, held);
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
    }
    let (full, hold) = domain.capture_with_material(ledger, exact, prior, None)?;
    assert_eq!(hold.phase_peak(), peak);
    assert_eq!(
        hold.material_estimate()?,
        hold.additional_material_estimate()?
    );
    drop((full, hold));
    assert_eq!(ledger.report()?.observed_bytes, held);
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(domain
        .capture_with_material(&foreign, LIMITS, prior, None)
        .is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    let mut phase = CapturePhase::new_with_body_loan(domain, ledger, LIMITS, prior, None)?;
    let captured = domain.capture_observed(&mut phase)?;
    let scratch = phase.owned.as_ref().unwrap().guard();
    assert!(phase
        .finish_captured(captured)
        .unwrap_err()
        .to_string()
        .contains("another live holder"));
    assert!(ledger.report()?.observed_bytes > held);
    drop(scratch);
    assert_eq!(ledger.report()?.observed_bytes, held);
    let mut phase = CapturePhase::new_with_body_loan(domain, ledger, LIMITS, prior, None)?;
    let captured = domain.capture_observed(&mut phase)?;
    phase.owned = Some(foreign.reserve_owned_completion(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: phase.owned_bytes,
        },
    )?);
    assert!(phase
        .finish_captured(captured)
        .unwrap_err()
        .to_string()
        .contains("another phase"));
    assert_eq!(foreign.report()?.observed_bytes, 0);
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop((guard, bytes));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[test]
fn borrowed_input_key_observer_matches_actual_key_bytes_and_native_shape() -> Result<()> {
    for family in [
        InputEvidenceFamily::Message,
        InputEvidenceFamily::Turn,
        InputEvidenceFamily::ToolExecution,
    ] {
        for id in ["", "current", "quote\"\\\n雪"] {
            let key = InputEvidenceKey::new(family, id);
            let borrowed = BorrowedInputKey { family, id };
            assert_eq!(serde_json::to_vec(&key)?, serde_json::to_vec(&borrowed)?);
            assert_eq!(
                borrowed_json_material(&key)?,
                borrowed_json_material(&borrowed)?
            );
        }
    }
    Ok(())
}
