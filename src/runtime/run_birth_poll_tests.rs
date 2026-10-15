use super::super::tests::{fixture, prepare_named};
use super::*;
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger, input_queries::InputQueryError,
};
use crate::types::{AuditEvent, TurnTerminalKind, TurnTerminalRecord};
use anyhow::Context;
use std::sync::Arc;
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);

#[tokio::test]
async fn actual_born_poll_ignores_resource_publication_generation_but_observes_original_activity(
) -> Result<()> {
    let (_home, runtime, sessions, _old) = fixture().await?;
    let first = runtime
        .capture_run_birth_sessions(&sessions)?
        .project_report(LIMITS)?;
    let binding_root = runtime.inner.mutation_owner.capture()?.unwrap();
    let binding_bytes = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    first
        .source
        .validate_material_binding(&runtime, &binding_root)?;
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        binding_bytes
    );
    let original = first.poll_facts()?;
    assert!(!original.runtime_error);
    let token = first.source.token;
    drop(first);
    // An actual owner publication with the same qualified sources models a
    // resource-only publication. No source alias or complete flag is invented.
    let mut held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root absent")?;
    held.begin("actual-resource-only-run-poll-publication".into())?;
    held.committed_prepared_with_current_read_domains(
        root.state.clone(),
        root.episodes.clone(),
        root.input_evidence.clone(),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        root.current_reads.clone(),
    )?;
    held.publish(root.state.clone())?;
    drop((held, root));
    let second = runtime
        .capture_run_birth_sessions(&sessions)?
        .project_report(LIMITS)?;
    assert_ne!(second.source.token.generation, token.generation);
    assert_eq!(second.poll_facts()?, original);
    assert!(second
        .source
        .validate_material_binding(&runtime, &binding_root)
        .is_err());
    let current = runtime.inner.mutation_owner.capture()?.unwrap();
    second
        .source
        .validate_material_binding(&runtime, &current)?;
    drop((binding_root, current));
    drop(second);
    runtime
        .append_owned_runtime_audit_event(AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({"actual":"born poll"}),
        ))
        .await?;
    prepare_named(&runtime).await?;
    let third = runtime
        .capture_run_birth_sessions(&sessions)?
        .project_report(LIMITS)?;
    let changed = third.poll_facts()?;
    assert!(changed.runtime_error);
    assert_ne!(changed.activity, original.activity);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO poll_forbidden_states;
         ALTER TABLE tasks RENAME TO poll_forbidden_tasks;
         ALTER TABLE audit_events RENAME TO poll_forbidden_events;
         ALTER TABLE messages RENAME TO poll_forbidden_messages;
         ALTER TABLE transcript_entries RENAME TO poll_forbidden_transcript;
         ALTER TABLE tool_executions RENAME TO poll_forbidden_tools;",
    )?;
    assert_eq!(third.poll_facts()?, changed);
    Ok(())
}

#[tokio::test]
async fn actual_terminal_text_moves_original_string_and_next_capture_borrows_its_last_holder_before_clones(
) -> Result<()> {
    let (_home, runtime, sessions, _old) = fixture().await?;
    runtime
        .update_agent_state(|state| {
            state.turn_index = sessions.baseline.turn_index + 1;
            state.last_turn_terminal = Some(TurnTerminalRecord {
                turn_id: "actual-polled-terminal".into(),
                turn_index: state.turn_index,
                kind: TurnTerminalKind::Completed,
                reason: None,
                last_assistant_message: Some(format!("  {}  ", "terminal".repeat(8192))),
                no_brief_reason: None,
                checkpoint: None,
                completed_at: chrono::Utc::now(),
                duration_ms: 1,
            });
            Ok(())
        })
        .await?;
    prepare_named(&runtime).await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let mut report = runtime
        .capture_run_birth_sessions(&sessions)?
        .project_report(LIMITS)?;
    let pointer = report
        .source
        .state
        .last_turn_terminal
        .as_ref()
        .unwrap()
        .last_assistant_message
        .as_ref()
        .unwrap()
        .as_ptr();
    let retained = report
        .source
        .retain_terminal_text()
        .context("actual terminal text absent")?;
    assert_eq!(retained.as_str().len(), 8 * 8192);
    assert_eq!(retained.as_str().as_ptr(), pointer.wrapping_add(2));
    assert!(report
        .source
        .state
        .last_turn_terminal
        .as_ref()
        .unwrap()
        .last_assistant_message
        .is_none());
    let weak = Arc::downgrade(retained.guard_for_test());
    let actual = retained.material_estimate();
    drop(report);
    assert_eq!(ledger.report()?.observed_bytes, baseline + actual.0);
    let before_rejection = ledger.report()?.observed_bytes;
    let error = runtime
        .capture_run_birth_sessions_with_terminal_text(
            &sessions,
            (usize::try_from(actual.0)? - 1, LIMITS.1),
            Some(retained.clone()),
        )
        .err()
        .context("retained text prior was not checked before first State clone")?;
    assert!(
        matches!(
            error.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before_rejection);
    let next = runtime
        .capture_run_birth_sessions_with_terminal_text(&sessions, LIMITS, Some(retained.clone()))?
        .project_report(LIMITS)?;
    let binding_root = runtime.inner.mutation_owner.capture()?.unwrap();
    let binding_bytes = ledger.report()?.observed_bytes;
    next.source
        .validate_material_binding(&runtime, &binding_root)?;
    assert_eq!(ledger.report()?.observed_bytes, binding_bytes);
    drop(binding_root);
    let full = next.material_estimate()?;
    let token = next.source.token;
    let scope = &next.source.inputs.input.scope;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(retained
        .validate_accounting(scope, &foreign, token)
        .is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    drop(next);
    let held = ledger.report()?.observed_bytes;
    for limits in [
        (usize::try_from(full.0)? - 1, full.1),
        (usize::try_from(full.0)?, full.1 - 1),
    ] {
        let error = runtime
            .capture_run_birth_sessions_with_terminal_text(
                &sessions,
                limits,
                Some(retained.clone()),
            )
            .and_then(|capture| capture.project_report(limits))
            .err()
            .context("held terminal text got another full allowance")?;
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::ResourceExhausted(_))
            ) || matches!(
                error.downcast_ref::<crate::runtime::resident_task::TaskReadError>(),
                Some(crate::runtime::resident_task::TaskReadError::ResourceExhausted(_))
            ) || matches!(
                error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),
                Some(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted)
            ) || matches!(
                error.downcast_ref::<crate::runtime::resident_input_evidence::InputCaptureError>(),
                Some(crate::runtime::resident_input_evidence::InputCaptureError::ResourceExhausted)
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, held);
    }
    // Retained material is smaller than the admitted first-copy scratch peak.
    // Probe the same actual capture, keeping the true terminal source held.
    let fits = |limits| -> Result<bool> {
        let result = runtime
            .capture_run_birth_sessions_with_terminal_text(
                &sessions,
                limits,
                Some(retained.clone()),
            )
            .and_then(|capture| capture.project_report(limits));
        let success = match result {
            Ok(capture) => {
                assert_eq!(capture.material_estimate()?, full);
                drop(capture);
                true
            }
            Err(error) => {
                assert!(matches!(error.downcast_ref::<InputQueryError>(), Some(InputQueryError::ResourceExhausted(_)))
                    || matches!(error.downcast_ref::<crate::runtime::resident_task::TaskReadError>(), Some(crate::runtime::resident_task::TaskReadError::ResourceExhausted(_)))
                    || matches!(error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(), Some(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted))
                    || matches!(error.downcast_ref::<crate::runtime::resident_input_evidence::InputCaptureError>(), Some(crate::runtime::resident_input_evidence::InputCaptureError::ResourceExhausted)), "{error:#}");
                false
            }
        };
        assert_eq!(ledger.report()?.observed_bytes, held);
        Ok(success)
    };
    let mut byte_range = (usize::try_from(full.0)?, LIMITS.0);
    while byte_range.0 < byte_range.1 {
        let middle = byte_range.0 + (byte_range.1 - byte_range.0) / 2;
        if fits((middle, LIMITS.1))? {
            byte_range.1 = middle
        } else {
            byte_range.0 = middle + 1
        }
    }
    let mut node_range = (full.1, LIMITS.1);
    while node_range.0 < node_range.1 {
        let middle = node_range.0 + (node_range.1 - node_range.0) / 2;
        if fits((byte_range.0, middle))? {
            node_range.1 = middle
        } else {
            node_range.0 = middle + 1
        }
    }
    let exact = (byte_range.0, node_range.0);
    assert!(!fits((exact.0 - 1, exact.1))?);
    assert!(!fits((exact.0, exact.1 - 1))?);
    assert!(fits(exact)?);
    let last = runtime
        .capture_run_birth_sessions_with_terminal_text(&sessions, exact, Some(retained.clone()))?
        .project_report(exact)?;
    assert_eq!(last.material_estimate()?, full);
    drop(retained);
    assert!(
        weak.upgrade().is_some(),
        "new poll released its original retained-text loan"
    );
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
