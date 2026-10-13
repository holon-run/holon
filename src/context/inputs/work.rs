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

pub(super) fn completion_input(
    work: &CapturedWork,
    inputs: &CapturedInputEvidence,
    work_item_id: &str,
) -> Result<Option<CompletionBriefInput>> {
    ensure!(
        work.scope == inputs.scope,
        "completion source scope mismatch"
    );
    let Some(head) = work.latest_result(work_item_id)? else {
        return Ok(None);
    };
    let read = |family, id: &str| {
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
                    Ok(entry.clone())
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
                    Ok(message.clone())
                }
                _ => anyhow::bail!("completion Message family/scope mismatch"),
            }
        })
        .transpose()?;
    Ok(Some(CompletionBriefInput {
        brief: brief.clone(),
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
    let mut heads = BTreeSet::new();
    for id in work_item_ids {
        if let Some(head) = work.latest_result(id)? {
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
            message_turn_keys: BTreeMap::new(),
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
        assert_eq!(result.brief, brief);
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
        let projection = work.work_queue(&state, Vec::new()).unwrap();
        let ids = crate::context::completion_work_item_ids(None, &projection);
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
