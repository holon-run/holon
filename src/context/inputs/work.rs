//! Work reads use one selected resident domain; optional completion sources are
//! frozen by the original same-view head observation, never inferred on read.

use std::collections::BTreeSet;

use anyhow::{ensure, Result};

use crate::{
    runtime::{
        resident_input_evidence::{
            CapturedInputEvidence, InputEvidenceFamily, InputEvidenceKey, InputEvidenceReadError,
            PreparedInputEvidence,
        },
        resident_work::{CanonicalWorkRecord, CapturedWork, WorkFamily, WorkKey, WorkReadError},
    },
    runtime_db::context_inputs::CompletionBriefInput,
    types::{
        BriefKind, TranscriptEntryKind, WaitConditionRecord, WaitConditionStatus, WorkItemRecord,
    },
};

pub(crate) fn work_item(work: &CapturedWork, id: &str) -> Result<Option<WorkItemRecord>> {
    let key = WorkKey::new(WorkFamily::Work, id);
    work.lookup(&key)?
        .map(|qualified| {
            qualified.validate(&work.scope)?;
            match &qualified.record {
                CanonicalWorkRecord::Work(record) => Ok(record.clone()),
                _ => anyhow::bail!("resident Work family mismatch"),
            }
        })
        .transpose()
}

pub(super) fn active_waits(work: &CapturedWork) -> Result<Vec<WaitConditionRecord>> {
    if !work.live_complete {
        return Err(WorkReadError::IncompleteLive.into());
    }
    let mut waits = Vec::new();
    let mut owners = Vec::new();
    let mut owner_ids = BTreeSet::new();
    for id in &work.unresolved_wait_ids {
        let key = WorkKey::new(WorkFamily::Wait, id);
        let qualified = work
            .lookup(&key)?
            .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?;
        qualified.validate(&work.scope)?;
        let CanonicalWorkRecord::Wait(wait) = &qualified.record else {
            anyhow::bail!("resident Wait family mismatch")
        };
        if wait.status != WaitConditionStatus::Active {
            continue;
        }
        if let Some(id) = &wait.work_item_id {
            if owner_ids.insert(id.clone()) {
                if let Some(owner) = work_item(work, id)? {
                    owners.push(owner);
                }
            }
        }
        waits.push(wait.clone());
    }
    waits.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(crate::storage::filter_context_active_waits(waits, owners))
}

pub(super) fn completion_input<'a>(
    work: &CapturedWork,
    inputs: &'a CapturedInputEvidence,
    work_item_id: &str,
) -> Result<Option<CompletionBriefInput<'a>>> {
    ensure!(
        work.scope == inputs.scope,
        "completion source scope mismatch"
    );
    crate::diagnostics::resource_io::admit_cloned_value(&work_item_id)?;
    let Some(head) = work.latest_result(work_item_id)? else {
        return Ok(None);
    };
    let read = |family, id: &str| {
        crate::diagnostics::resource_io::admit_cloned_value(&id)?;
        let key = InputEvidenceKey::new(family, id);
        let record = inputs
            .lookup(&key)?
            .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(key.clone()))?;
        ensure!(
            record.locator.scope == work.scope && record.locator.key == key,
            "completion source scope/key mismatch"
        );
        Ok::<_, anyhow::Error>(record)
    };
    let brief = read(InputEvidenceFamily::Brief, &head.brief_id)?;
    let PreparedInputEvidence::Brief(brief) = &brief.record else {
        anyhow::bail!("completion Brief family mismatch")
    };
    ensure!(
        brief.agent_id == work.scope.agent_id
            && brief.work_item_id.as_deref() == Some(work_item_id)
            && brief.kind == BriefKind::Result
            && !brief.text.is_empty(),
        "completion report scope/head mismatch"
    );
    let content_source = head
        .transcript_entry_id
        .as_ref()
        .map(|id| {
            ensure!(
                brief.content_source.transcript_entry_id() == Some(id.as_str()),
                "completion Transcript differs from the frozen head"
            );
            let record = read(InputEvidenceFamily::Transcript, id)?;
            match &record.record {
                PreparedInputEvidence::Transcript(entry)
                    if entry.agent_id == work.scope.agent_id =>
                {
                    Ok(std::borrow::Cow::Borrowed(entry))
                }
                _ => anyhow::bail!("completion Transcript family/scope mismatch"),
            }
        })
        .transpose()?;
    let source_message = head
        .source_message_id
        .as_ref()
        .map(|id| {
            ensure!(
                content_source
                    .as_ref()
                    .is_some_and(|entry| entry.kind == TranscriptEntryKind::IncomingMessage
                        && entry.related_message_id.as_deref() == Some(id.as_str())),
                "completion Message differs from the frozen Transcript head"
            );
            let record = read(InputEvidenceFamily::Message, id)?;
            match &record.record {
                PreparedInputEvidence::Message(message)
                    if message.agent_id == work.scope.agent_id =>
                {
                    Ok(std::borrow::Cow::Borrowed(message))
                }
                _ => anyhow::bail!("completion Message family/scope mismatch"),
            }
        })
        .transpose()?;
    Ok(Some(CompletionBriefInput {
        brief: std::borrow::Cow::Borrowed(brief),
        content_source,
        source_message,
    }))
}

/// Only the original bounded display candidates select completion paths. The
/// head itself must qualify; optional source edges are not strict body roots.
pub(crate) fn selected_completion_brief_ids(
    work: &CapturedWork,
    work_item_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    crate::diagnostics::resource_io::check_active_budget()?;
    let mut heads = BTreeSet::new();
    for id in work_item_ids {
        if let Some(head) = work.latest_result(id)? {
            crate::diagnostics::resource_io::admit_cloned_value(&head.brief_id)?;
            heads.insert(head.brief_id.clone());
        }
    }
    Ok(heads)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{
        runtime::{
            resident_input_evidence::{
                EvidenceLocator, InputEvidenceScope, QualifiedInputEvidence,
            },
            resident_work::{
                QualifiedWorkRecord, WorkAvailability, WorkCompletionRef, WorkHistoryCoverage,
                WORK_NONOPEN_WINDOW,
            },
        },
        types::{
            AgentState, AuthorityClass, BriefContentSource, BriefContentSourceRelation,
            BriefRecord, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
            TranscriptEntry, WorkItemState,
        },
    };
    use std::{collections::BTreeMap, sync::Arc};

    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        }
    }
    pub(in crate::context::inputs) fn capture(
        records: Vec<CanonicalWorkRecord>,
        live_work_ids: Vec<String>,
        unresolved_wait_ids: Vec<String>,
        latest_results: BTreeMap<String, Option<WorkCompletionRef>>,
    ) -> CapturedWork {
        let records = records
            .into_iter()
            .map(|record| {
                Arc::new(QualifiedWorkRecord {
                    scope: scope(),
                    key: record.key(),
                    revision: 1,
                    content_digest: "a".repeat(64),
                    resident_bytes: serde_json::to_vec(&record).unwrap().len(),
                    record,
                })
            })
            .collect::<Vec<_>>();
        CapturedWork {
            scope: scope(),
            revision: 1,
            live_complete: true,
            live_work_ids: live_work_ids.into_iter().collect(),
            unresolved_wait_ids: unresolved_wait_ids.into_iter().collect(),
            active_frame_ids: BTreeSet::new(),
            turn_wait_keys: BTreeMap::new(),
            task_wait_keys: BTreeMap::new(),
            trigger_wait_keys: BTreeMap::new(),
            nonopen: WorkHistoryCoverage {
                limit: WORK_NONOPEN_WINDOW,
                keys: Vec::new(),
                is_exhaustive: true,
            },
            latest_results,
            observations: records
                .iter()
                .map(|r| (r.key.clone(), WorkAvailability::Present))
                .collect(),
            records,
            budget: None,
            budget_views: Vec::new(),
        }
    }
    fn evidence(record: PreparedInputEvidence) -> Arc<QualifiedInputEvidence> {
        Arc::new(QualifiedInputEvidence {
            locator: EvidenceLocator {
                scope: scope(),
                key: record.key(),
                revision: 1,
                content_digest: "a".repeat(64),
            },
            record,
            attempt_turn_keys: None,
            created_at: "2026-10-13T00:00:00Z".into(),
            order_sequence: None,
            turn_index: None,
            owner: None,
            resident_bytes: 1024,
        })
    }
    fn inputs(records: Vec<PreparedInputEvidence>) -> CapturedInputEvidence {
        CapturedInputEvidence {
            query: None,
            query_acquisition: None,
            selected_completion_brief_ids: Default::default(),
            completion_source_observations: Default::default(),
            recovery_source_absences: Default::default(),
            execution: Default::default(),
            budget: None,
            budget_views: Vec::new(),
            scope: scope(),
            windows: Vec::new(),
            records: records.into_iter().map(evidence).collect(),
            unavailable: BTreeSet::new(),
            turn_transcript_keys: BTreeMap::new(),
            message_turn_keys: Default::default(),
        }
    }
    fn completion() -> (
        BriefRecord,
        TranscriptEntry,
        MessageEnvelope,
        WorkCompletionRef,
    ) {
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original completion body".into(),
            },
        );
        let transcript = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(message.id.clone()),
            serde_json::json!({"text":"old preview"}),
        );
        let mut brief =
            BriefRecord::new("default", BriefKind::Result, "Brief fallback", None, None);
        brief.work_item_id = Some("work".into());
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: transcript.id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        let head = WorkCompletionRef {
            brief_id: brief.id.clone(),
            transcript_entry_id: Some(transcript.id.clone()),
            source_message_id: Some(message.id.clone()),
        };
        (brief, transcript, message, head)
    }

    #[test]
    fn completion_reads_selected_bodies_without_any_sql_history_table() {
        let (brief, transcript, message, head) = completion();
        let work = capture(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::from([("work".into(), Some(head))]),
        );
        let selected = inputs(vec![
            PreparedInputEvidence::Brief(brief.clone()),
            PreparedInputEvidence::Transcript(transcript),
            PreparedInputEvidence::Message(message.clone()),
        ]);
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = crate::runtime_db::context_inputs::ContextDurableRead::new(&tx, "default");
        let state = AgentState::new("default");
        let adapter =
            super::super::ResidentContextInputs::new(&durable, &selected, &work, &state, &[])
                .unwrap();
        use super::super::ContextInputs;
        let result = adapter
            .latest_completion_brief_for_work_item("work")
            .unwrap()
            .unwrap();
        assert_eq!(result.source_message.unwrap().body, message.body);
        assert!(matches!(result.brief, std::borrow::Cow::Borrowed(_)));
        assert_eq!(*result.brief, brief);
        // Exercise the complete selection/resolution helper, not just the
        // resident leaf. No Result may trigger a legacy SQL fallback.
        let reports = crate::context::latest_completion_report_by_work_item(
            &adapter,
            "default",
            &BTreeSet::from(["work".into()]),
        )
        .unwrap();
        assert_eq!(reports["work"].text, "original completion body");
        assert_eq!(reports["work"].brief_id.as_deref(), Some(brief.id.as_str()));
    }

    #[test]
    fn completion_admits_borrowed_lookup_ids_before_constructing_missing_keys() {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let large_id = "x".repeat(64 * 1024);
        for large_source_id in [false, true] {
            let (work, work_item_id) = if large_source_id {
                let mut head = completion().3;
                head.brief_id = large_id.clone();
                (
                    capture(
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        BTreeMap::from([("work".into(), Some(head))]),
                    ),
                    "work",
                )
            } else {
                (
                    capture(Vec::new(), Vec::new(), Vec::new(), BTreeMap::new()),
                    large_id.as_str(),
                )
            };
            let selected = inputs(Vec::new());
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default()).unwrap();
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), 16 * 1024, 1024).unwrap();
            {
                let _active = phase.enter();
                let error = completion_input(&work, &selected, work_item_id).unwrap_err();
                assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
            }
            assert!(phase.finish().is_err());
            drop(phase);
            // Without a resource phase, preserve the original missing-key errors.
            let error = completion_input(&work, &selected, work_item_id).unwrap_err();
            if large_source_id {
                assert!(matches!(
                    error.downcast_ref::<InputEvidenceReadError>(),
                    Some(InputEvidenceReadError::NeedsPreparation(_))
                ));
            } else {
                assert!(matches!(
                    error.downcast_ref::<WorkReadError>(),
                    Some(WorkReadError::NeedsPreparation(_))
                ));
            }
        }
    }

    #[test]
    fn completion_borrows_large_source_but_refuses_owning_resolution_at_original_limit() {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let (brief, transcript, mut message, head) = completion();
        message.body = MessageBody::Json {
            value: serde_json::json!({"text": "\\\"[{},:]\n".repeat(4096)}),
        };
        let work = capture(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::from([("work".into(), Some(head))]),
        );
        let selected = inputs(vec![
            PreparedInputEvidence::Brief(brief),
            PreparedInputEvidence::Transcript(transcript),
            PreparedInputEvidence::Message(message),
        ]);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default()).unwrap();
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), 2 * 1024 * 1024, 8192)
                .unwrap();
        {
            let _active = phase.enter();
            // Consume the same original phase; a fresh full allowance would fit.
            charge_owned_metadata(128 * 1024, 0).unwrap();
            let input = completion_input(&work, &selected, "work").unwrap().unwrap();
            assert!(matches!(input.brief, std::borrow::Cow::Borrowed(_)));
            assert!(matches!(
                input.content_source,
                Some(std::borrow::Cow::Borrowed(_))
            ));
            assert!(matches!(
                input.source_message,
                Some(std::borrow::Cow::Borrowed(_))
            ));
            let error = crate::context::resolved_brief_text_from_values(
                &input.brief,
                input.content_source.as_deref(),
                input.source_message.as_deref(),
            )
            .unwrap_err();
            assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
        }
        assert!(phase.finish().is_err());
        drop(phase);
        let fresh =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), 2 * 1024 * 1024, 8192)
                .unwrap();
        {
            let _active = fresh.enter();
            let input = completion_input(&work, &selected, "work").unwrap().unwrap();
            assert!(input.source_message.is_some());
            let text = crate::context::resolved_brief_text_from_values(
                &input.brief,
                input.content_source.as_deref(),
                input.source_message.as_deref(),
            )
            .unwrap();
            assert!(text.contains("text"));
            assert_eq!(
                input.into_brief().unwrap().work_item_id.as_deref(),
                Some("work")
            );
        }
        fresh.finish().unwrap();
    }

    #[test]
    fn completion_final_brief_copy_obeys_exact_byte_and_record_limits() {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::{cloned_value_material, ResourceReadMaterial},
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let brief = completion().0;
        let (bytes, records) = cloned_value_material(&brief).unwrap();
        let header_bytes = (scope().agent_id.len() as u64 + 256) * 8;
        let exact_bytes = header_bytes + bytes as u64 * 8;
        let exact_records = 64 + records * 8;
        for (bytes, records, accepted) in [
            (exact_bytes - 1, exact_records, false),
            (exact_bytes, exact_records - 1, false),
            (exact_bytes, exact_records, true),
        ] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default()).unwrap();
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records).unwrap();
            let result = phase.read(|| {
                CompletionBriefInput {
                    brief: std::borrow::Cow::Borrowed(&brief),
                    content_source: None,
                    source_message: None,
                }
                .into_brief()
            });
            assert_eq!(result.is_ok(), accepted);
            if accepted {
                assert_eq!(result.unwrap(), brief);
                assert_eq!(phase.finish().unwrap(), (exact_bytes, exact_records));
            } else {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("ResourceExhausted"));
                assert!(phase.finish().is_err());
            }
        }
    }

    #[test]
    fn frozen_optional_absence_preserves_fallback_but_missing_selected_body_is_not_absence() {
        let (brief, _, _, mut head) = completion();
        let selected = inputs(vec![PreparedInputEvidence::Brief(brief.clone())]);
        let work = capture(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::from([("work".into(), Some(head.clone()))]),
        );
        assert!(completion_input(&work, &selected, "work")
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        head.transcript_entry_id = None;
        head.source_message_id = None;
        let work = capture(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::from([("work".into(), Some(head))]),
        );
        let result = completion_input(&work, &selected, "work").unwrap().unwrap();
        assert!(result.content_source.is_none());
        assert!(result.source_message.is_none());
        assert_eq!(result.brief.content_source, brief.content_source);
        assert!(completion_input(&work, &selected, "unknown-work")
            .unwrap_err()
            .downcast_ref::<WorkReadError>()
            .is_some());
        let mut pruned = inputs(vec![PreparedInputEvidence::Brief(brief.clone())]);
        pruned.records.clear();
        pruned
            .unavailable
            .insert(InputEvidenceKey::new(InputEvidenceFamily::Brief, &brief.id));
        assert!(completion_input(&work, &pruned, "work").is_err());
    }

    #[test]
    fn work_lookup_distinguishes_unprepared_absent_and_foreign_incarnation() {
        let mut work = capture(Vec::new(), Vec::new(), Vec::new(), BTreeMap::new());
        assert!(work_item(&work, "not-prepared")
            .unwrap_err()
            .downcast_ref::<WorkReadError>()
            .is_some());
        work.observations.insert(
            WorkKey::new(WorkFamily::Work, "not-prepared"),
            WorkAvailability::Absent,
        );
        assert!(work_item(&work, "not-prepared").unwrap().is_none());
        let selected = inputs(Vec::new());
        work.scope.identity_incarnation = 2;
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = crate::runtime_db::context_inputs::ContextDurableRead::new(&tx, "default");
        let state = AgentState::new("default");
        assert!(
            super::super::ResidentContextInputs::new(&durable, &selected, &work, &state, &[])
                .is_err()
        );
    }

    #[test]
    fn incomplete_live_wait_coverage_is_never_an_empty_projection() {
        let mut work = capture(Vec::new(), Vec::new(), Vec::new(), BTreeMap::new());
        work.live_complete = false;
        assert!(active_waits(&work)
            .unwrap_err()
            .downcast_ref::<WorkReadError>()
            .is_some());
        let record = WorkItemRecord::new("default", "current", WorkItemState::Open);
        let work = capture(
            vec![CanonicalWorkRecord::Work(record.clone())],
            vec![record.id.clone()],
            Vec::new(),
            BTreeMap::from([(record.id.clone(), None)]),
        );
        assert_eq!(work_item(&work, &record.id).unwrap(), Some(record));
    }

    #[test]
    fn completion_selector_borrows_known_absence_but_admits_missing_key_errors() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let id = "x".repeat(64 * 1024);
        let work = capture(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::from([(id.clone(), None)]),
        );
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 4096, 64)?;
        let header = phase.finish()?;
        {
            let _active = phase.enter();
            assert!(selected_completion_brief_ids(&work, &BTreeSet::from([id])).is_ok());
        }
        assert_eq!(
            phase.finish()?,
            header,
            "a borrowed proven absence allocates nothing"
        );
        drop(phase);
        let missing = BTreeSet::from(["missing".repeat(16 * 1024)]);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 4096, 64)?;
        {
            let _active = phase.enter();
            let error = selected_completion_brief_ids(&work, &missing).unwrap_err();
            assert!(
                error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("resourceexhausted"),
                "{error:#}"
            );
            assert!(selected_completion_brief_ids(&work, &BTreeSet::new()).is_err());
        }
        assert!(phase.finish().is_err());
        Ok(())
    }

    #[test]
    fn completion_heads_follow_the_bounded_display_selector_without_strict_source_roots() {
        let mut records = Vec::new();
        let mut heads = BTreeMap::new();
        for index in 0..20 {
            let mut record = WorkItemRecord::new(
                "default",
                format!("completed {index}"),
                WorkItemState::Completed,
            );
            record.id = format!("work-{index:02}");
            record.updated_at = chrono::DateTime::from_timestamp_millis(1000 + index).unwrap();
            record.created_at = record.updated_at;
            heads.insert(
                record.id.clone(),
                Some(WorkCompletionRef {
                    brief_id: format!("brief-{index:02}"),
                    transcript_entry_id: Some(format!("transcript-{index:02}")),
                    source_message_id: Some(format!("message-{index:02}")),
                }),
            );
            records.push(CanonicalWorkRecord::Work(record));
        }
        let mut work = capture(records, Vec::new(), Vec::new(), heads);
        work.nonopen.keys = work
            .records
            .iter()
            .map(|record| record.key.clone())
            .collect();
        let state = AgentState::new("default");
        let projection: crate::work_item_scheduling::ContextWorkQueue =
            work.work_queue(&state, Vec::new()).unwrap().into();
        let ids = crate::context::completion_work_item_ids(None, &projection).unwrap();
        assert_eq!(ids.len(), 3);
        let heads = selected_completion_brief_ids(&work, &ids).unwrap();
        assert_eq!(heads.len(), 3);
        assert!(!heads.contains("brief-00"));
        assert!(heads.iter().all(|id| id.starts_with("brief-")));
        assert_eq!(
            heads,
            ids.iter()
                .map(|id| work.latest_result(id).unwrap().unwrap().brief_id.clone())
                .collect()
        );
    }
}
