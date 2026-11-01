use super::*;
use crate::diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial};

fn qualified(fact: CanonicalSchedulingFact) -> Result<Arc<QualifiedSchedulingFact>> {
    Ok(Arc::new(QualifiedSchedulingFact {
        locator: SchedulingLocator {
            scope: scope(),
            key: fact.key(),
            revision: 1,
            content_digest: "a".repeat(64),
            projection_digest: "b".repeat(64),
        },
        resident_bytes: usize::try_from(serialized_bytes(&fact)?)?,
        fact,
        _source_material: None,
    }))
}

#[test]
fn resident_map_key_uses_original_remainder_before_duplicate_insert() -> Result<()> {
    let record = qualified(CanonicalSchedulingFact::Timer(timer(
        &"k".repeat(16 * 1024),
        TimerStatus::Active,
    )))?;
    // A SchedulingKey has two string fields: 13 structural JSON slots.
    // This scalar oracle never runs bootstrap/loader or its admission helper.
    // Four string starts, two braces, two colons, and one comma. Closing
    // quotes are consumed inside strings, not counted as structural slots.
    let bytes = (serde_json::to_vec(&record.locator.key)?.len() as u64 * 4 + 9 * 128 + 512) * 8;
    let records = (9 + 16) * 8;
    let ledger = ledger()?;
    let byte_limit = 8 * 1024 * 1024;
    let record_limit = 4096;
    for bytes_bound in [false, true] {
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), byte_limit, record_limit)?;
        let baseline = phase.finish()?;
        let weak = Arc::downgrade(&phase.guard()?);
        let error = {
            let _entry = phase.enter();
            if bytes_bound {
                charge_owned_metadata((byte_limit - baseline.0 - 2 * bytes) / 8 + 1, 0)?;
            } else {
                charge_owned_metadata(0, (record_limit - baseline.1 - 2 * records) / 8 + 1)?;
            }
            ResidentSchedulingDomain::bootstrap(
                scope(),
                SchedulingCoverage::empty(1),
                vec![record.clone(), record.clone()],
                4096,
                16 * 1024 * 1024,
            )
            .unwrap_err()
        };
        assert!(
            error
                .to_string()
                .contains("ResourceExhausted: context resource read material"),
            "{error:#}"
        );
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let phase =
        ResourceReadMaterial::with_test_limits(&ledger, &scope(), byte_limit, record_limit)?;
    let baseline = phase.finish()?;
    let error = {
        let _entry = phase.enter();
        ResidentSchedulingDomain::bootstrap(
            scope(),
            SchedulingCoverage::empty(1),
            vec![record.clone(), record],
            4096,
            16 * 1024 * 1024,
        )
        .unwrap_err()
    };
    assert_eq!(error.to_string(), "duplicate Scheduling body");
    assert_eq!(
        phase.finish()?,
        (baseline.0 + 2 * bytes, baseline.1 + 2 * records)
    );
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn resident_map_qualification_precedes_clone_admission() -> Result<()> {
    let record = qualified(CanonicalSchedulingFact::Timer(timer(
        "qualified",
        TimerStatus::Active,
    )))?;
    let ledger = ledger()?;
    for case in 0..6 {
        let mut invalid = (*record).clone();
        let expected = match case {
            0 => {
                invalid.locator.scope.identity_incarnation += 1;
                "Scheduling scope/key/revision differs"
            }
            1 => {
                invalid.locator.key.id.push('!');
                "Scheduling scope/key/revision differs"
            }
            2 => {
                invalid.locator.revision = 0;
                "Scheduling scope/key/revision differs"
            }
            3 => {
                invalid.locator.content_digest = "invalid".into();
                "invalid Scheduling digest"
            }
            4 => {
                invalid.resident_bytes = 0;
                "Scheduling body estimate incomplete"
            }
            _ => {
                invalid.locator.revision = 2;
                "Scheduling fact revision exceeds its domain"
            }
        };
        // Only the original phase header fits; no owning clone can pass.
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            (scope().agent_id.len() as u64 + 256) * 8,
            64,
        )?;
        let baseline = phase.finish()?;
        let error = {
            let _entry = phase.enter();
            ResidentSchedulingDomain::bootstrap(
                scope(),
                SchedulingCoverage::empty(1),
                vec![Arc::new(invalid)],
                4096,
                16 * 1024 * 1024,
            )
            .unwrap_err()
        };
        assert_eq!(error.to_string(), expected, "case {case}");
        assert_eq!(phase.finish()?, baseline);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn resident_key_validation_matches_canonical_encoding_for_all_families() -> Result<()> {
    let escaped = "\"\\\n\r\t\u{0}雪";
    let facts = [
        CanonicalSchedulingFact::Timer(timer(escaped, TimerStatus::Active)),
        CanonicalSchedulingFact::ExternalTrigger({
            let mut value = trigger();
            value.external_trigger_id = escaped.into();
            value
        }),
        CanonicalSchedulingFact::TimerWake(SchedulingWakeFact {
            agent_id: scope().agent_id,
            timer_id: escaped.into(),
            message_id: format!("{escaped}message"),
            fire_count: 1,
            status: SchedulingWakeStatus::Pending,
            created_at: at(),
            updated_at: at(),
            incorporated_at: None,
            cancelled_at: None,
        }),
    ];
    for fact in facts {
        let valid = qualified(fact)?;
        valid.validate(&scope())?;
        for family in [
            SchedulingFamily::Timer,
            SchedulingFamily::TimerWake,
            SchedulingFamily::ExternalTrigger,
        ] {
            let mut value = (*valid).clone();
            value.locator.key.family = family;
            assert_eq!(
                value.validate(&scope()).is_ok(),
                family == valid.locator.key.family
            );
        }
        let mut shortened = valid.locator.key.id.clone();
        shortened.pop();
        for text in [
            String::new(),
            shortened,
            format!("{} ", valid.locator.key.id),
            format!(" {}", valid.locator.key.id),
            valid.locator.key.id.replace("\\n", "\\u000a"),
        ] {
            let mut value = (*valid).clone();
            value.locator.key.id = text;
            assert_eq!(
                value.validate(&scope()).is_ok(),
                value.locator.key == valid.fact.key()
            );
        }
    }
    Ok(())
}

#[test]
fn resident_map_key_phase_survives_input_install_and_last_capture() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("resident-holder", TimerStatus::Active))
    })?;
    let ledger = ledger()?;
    let scope = scope();
    let capture = SchedulingCaptureRequest {
        include_live: true,
        history_limit: 0,
        required_keys: BTreeSet::new(),
    };
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let weak = Arc::downgrade(&phase.guard()?);
    let installed = {
        let _entry = phase.enter();
        db.transaction(|tx| {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                tx,
                &scope,
                &AgentState::new(&scope.agent_id),
                &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
                    query: None,
                    mandatory_roots: BTreeSet::new(),
                    windows: vec![],
                    interrupted_sources: Default::default(),
                    selected_completion_brief_ids: Default::default(),
                    record_limit: 4096,
                    byte_limit: 16 * 1024 * 1024,
                },
                12,
                &ledger,
                &capture,
            )
        })?
        .scheduling
    };
    phase.finish()?;
    assert_eq!(installed.records_for_test().len(), 1);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert!(weak.upgrade().is_some());
    let delta = CommittedSchedulingDelta {
        scope: scope.clone(),
        base_revision: installed.coverage().revision,
        revision: installed.coverage().revision,
        coverage: installed.coverage().clone(),
        records: installed.records_for_test().cloned().collect(),
    };
    let replacement = installed.with_committed(&delta)?;
    drop(installed);
    let first = Arc::new(replacement.capture(&capture)?);
    assert_eq!(first.active_timer_count()?, 1);
    let last = first.clone();
    drop(replacement);
    drop(delta);
    drop(first);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
