//! The original explicit Work recall's current-agent finite report inspection.
//! No Input locator, alias, foreign incarnation, window or ResourceReady grant.
use super::*;
use crate::types::{
    BriefContentSource, BriefRecord, DeliverySummaryRecord, MessageBody, MessageEnvelope,
    TranscriptEntry, TranscriptEntryKind, WorkItemState,
};
use rusqlite::{params, Transaction};
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExplicitWorkReportSource {
    ResultBrief,
    WorkItemResultSummary,
    DeliverySummary,
}
#[derive(Debug)]
pub(crate) struct ExplicitWorkReport {
    pub text: String,
    pub source: ExplicitWorkReportSource,
    pub brief_id: Option<String>,
    pub delivery_summary_id: Option<String>,
    pub source_turn_index: Option<u64>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}
pub(super) struct ReportInspection {
    pub report: Option<ExplicitWorkReport>,
    pub cost: (u64, usize),
    pub guards: Vec<Arc<BudgetViewGuard>>,
}
struct Material<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    limit: (u64, usize),
    used: (u64, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl Material<'_> {
    fn charge(&mut self, bytes: u64, records: usize) -> Result<()> {
        let next = (
            self.used
                .0
                .checked_add(bytes)
                .context("explicit Work report byte overflow")?,
            self.used
                .1
                .checked_add(records)
                .context("explicit Work report node overflow")?,
        );
        ensure!(
            next.0 <= self.limit.0 && next.1 <= self.limit.1,
            super::super::work_snapshot::WorkSnapshotError::ResourceExhausted(
                "explicit Work report combined material"
            )
        );
        let guard = self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        self.guards.push(guard);
        self.used = next;
        Ok(())
    }
    /// All four tables use the original current storage-agent predicate. The
    /// optional summary selector keeps latestAny ORDER/LIMIT, including empty.
    fn read<T: DeserializeOwned>(
        &mut self,
        tx: &Transaction<'_>,
        table: &'static str,
        id: &str,
        summary: bool,
    ) -> Result<Option<T>> {
        let (sequence, extra) = match table {
            "messages" => ("message_seq", 0),
            "transcript_entries" => ("transcript_seq", 0),
            "briefs" => ("created_event_seq", 0),
            "delivery_summaries" => ("NULL", 0),
            _ => anyhow::bail!("unsupported original Work report table"),
        };
        let predicate = if summary {
            "agent_id=?1 AND work_item_id=?2 ORDER BY created_at DESC,evidence_id DESC LIMIT 1"
        } else {
            "agent_id=?1 AND evidence_id=?2"
        };
        let raw_sql=format!("SELECT 512+length(CAST(payload_json AS BLOB))+length(CAST(evidence_id AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(turn_id AS BLOB)),0)+COALESCE(length(CAST(message_id AS BLOB)),0)+COALESCE(length(CAST(task_id AS BLOB)),0)+COALESCE(length(CAST(work_item_id AS BLOB)),0)+length(CAST(created_at AS BLOB))+length(CAST(kind AS BLOB))+COALESCE(length(CAST(content_ref AS BLOB)),0)+COALESCE(length(CAST(content_hash AS BLOB)),0)+COALESCE(length(CAST(preview AS BLOB)),0)+COALESCE(length(CAST({sequence} AS BLOB)),0)+{extra} FROM {table} WHERE {predicate}");
        let raw: Option<u64> = tx
            .query_row(&raw_sql, params![self.scope.agent_id, id], |row| row.get(0))
            .optional()?;
        let Some(raw) = raw else { return Ok(None) };
        self.charge(
            raw.checked_mul(8)
                .context("explicit Work report raw overflow")?,
            8,
        )?;
        let native_sql=format!("SELECT COUNT(*)+COALESCE(SUM(CASE WHEN typeof(j.key)='text' THEN 1 ELSE 0 END),0)+16 FROM (SELECT payload_json FROM {table} WHERE {predicate}) s,json_tree(s.payload_json) j");
        let nodes: u64 = tx.query_row(&native_sql, params![self.scope.agent_id, id], |row| {
            row.get(0)
        })?;
        let records = usize::try_from(nodes)?
            .checked_mul(8)
            .context("explicit Work report node overflow")?;
        self.charge(
            u64::try_from(records)?
                .checked_mul(256)
                .context("explicit Work report native bytes overflow")?,
            records,
        )?;
        let payload: String = tx.query_row(
            &format!("SELECT payload_json FROM {table} WHERE {predicate}"),
            params![self.scope.agent_id, id],
            |row| row.get(0),
        )?;
        Ok(Some(serde_json::from_str(&payload)?))
    }
}
fn message_text(message: &MessageEnvelope) -> Result<Option<String>> {
    Ok(match &message.body {
        MessageBody::Text { text } | MessageBody::Brief { text, .. } => {
            (!text.trim().is_empty()).then(|| text.clone())
        }
        MessageBody::Json { value } => Some(serde_json::to_string(value)?),
    })
}
pub(super) fn inspect_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    record: &WorkItemRecord,
    prior: (u64, usize),
    limit: (u64, usize),
) -> Result<ReportInspection> {
    let mut material = Material {
        scope,
        ledger,
        limit,
        used: prior,
        guards: Vec::new(),
    };
    if record.state != WorkItemState::Completed {
        return Ok(ReportInspection {
            report: None,
            cost: prior,
            guards: Vec::new(),
        });
    }
    // Bounded SQL strings, finite row/guard slots and final report wrapper.
    // Each variable payload subsequently receives full raw/native eight-copy
    // coverage before the decoder or text renderer owns its first allocation.
    material.charge(4096, 32)?;
    let mut report = None;
    if let Some(id) = record
        .result_brief_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        if let Some(brief) = material.read::<BriefRecord>(tx, "briefs", id, false)? {
            ensure!(
                brief.id == id && brief.agent_id == scope.agent_id,
                "explicit Work report Brief row address differs"
            );
            if !brief.text.trim().is_empty() {
                let resolved = (|| -> Result<Option<String>> {
                    let mut text = None;
                    if let BriefContentSource::TranscriptEntry { entry_id, .. } =
                        &brief.content_source
                    {
                        if let Some(entry) = material.read::<TranscriptEntry>(
                            tx,
                            "transcript_entries",
                            entry_id,
                            false,
                        )? {
                            ensure!(
                                entry.id == *entry_id && entry.agent_id == scope.agent_id,
                                "explicit Work report Transcript row address differs"
                            );
                            let mut message_exists = false;
                            if entry.kind == TranscriptEntryKind::IncomingMessage {
                                if let Some(id) = entry.related_message_id.as_deref() {
                                    if let Some(message) = material
                                        .read::<MessageEnvelope>(tx, "messages", id, false)?
                                    {
                                        ensure!(
                                            message.id == id && message.agent_id == scope.agent_id,
                                            "explicit Work report Message row address differs"
                                        );
                                        message_exists = true;
                                        text = message_text(&message)?;
                                    }
                                }
                            }
                            if !message_exists {
                                text = crate::object_resolver::transcript_text(&entry);
                            }
                        }
                    }
                    Ok(text)
                })();
                let text = match resolved {
                    Ok(text) => text,
                    Err(error)
                        if error
                            .downcast_ref::<super::super::work_snapshot::WorkSnapshotError>()
                            .is_some() || error.downcast_ref::<crate::runtime::input_evidence_budget::InputEvidenceBudgetError>().is_some() =>
                    {
                        return Err(error)
                    }
                    // The original resolver treats unavailable/unreadable
                    // optional content as the accepted Brief fallback.
                    Err(_) => None,
                };
                report = Some(ExplicitWorkReport {
                    text: text.unwrap_or_else(|| brief.text.clone()),
                    source: ExplicitWorkReportSource::ResultBrief,
                    brief_id: Some(brief.id),
                    delivery_summary_id: None,
                    source_turn_index: brief.turn_index,
                    created_at: Some(brief.created_at),
                });
            }
        }
    }
    if report.is_none() {
        let delivery =
            material.read::<DeliverySummaryRecord>(tx, "delivery_summaries", &record.id, true)?;
        if let Some(delivery) = &delivery {
            ensure!(
                delivery.agent_id == scope.agent_id && delivery.work_item_id == record.id,
                "explicit Work report Delivery row address differs"
            );
        }
        let (text, source, matched) = if let Some(text) = record
            .result_summary
            .as_ref()
            .filter(|text| !text.is_empty())
        {
            (
                Some(text.clone()),
                ExplicitWorkReportSource::WorkItemResultSummary,
                delivery.as_ref().filter(|value| value.text == *text),
            )
        } else {
            (
                delivery
                    .as_ref()
                    .filter(|value| !value.text.is_empty())
                    .map(|value| value.text.clone()),
                ExplicitWorkReportSource::DeliverySummary,
                delivery.as_ref().filter(|value| !value.text.is_empty()),
            )
        };
        if let Some(text) = text {
            report = Some(ExplicitWorkReport {
                text,
                source,
                brief_id: None,
                delivery_summary_id: matched.map(|value| value.id.clone()),
                source_turn_index: matched.and_then(|value| value.source_turn_index),
                created_at: matched.map(|value| value.created_at),
            });
        }
    }
    Ok(ReportInspection {
        report,
        cost: material.used,
        guards: material.guards,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AuthorityClass, BriefAttachment, BriefKind, MessageKind, MessageOrigin, Priority,
    };
    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "original-current-inspector".into(),
            identity_incarnation: 1,
        }
    }
    fn work() -> WorkItemRecord {
        let mut value = WorkItemRecord::new(
            "foreign-work-owner",
            "explicit original report",
            WorkItemState::Completed,
        );
        value.id = "foreign-work".into();
        value.result_brief_id = Some("current-brief".into());
        value
    }
    fn fixture() -> Result<(tempfile::TempDir, RuntimeDb, WorkItemRecord, BriefRecord)> {
        fixture_with_brief(|_| {})
    }
    fn fixture_with_brief(
        prepare: impl FnOnce(&mut BriefRecord),
    ) -> Result<(tempfile::TempDir, RuntimeDb, WorkItemRecord, BriefRecord)> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let work = work();
        let mut brief = BriefRecord::new(
            &scope().agent_id,
            BriefKind::Result,
            "original current report",
            None,
            None,
        );
        brief.id = "current-brief".into();
        brief.work_item_id = Some(work.id.clone());
        prepare(&mut brief);
        db.transaction(|tx| {
            tx.execute("INSERT INTO work_items(work_item_id,agent_id,state,objective,revision,created_at,updated_at,payload_json) VALUES(?1,?2,'completed',?3,?4,?5,?6,?7)",
                params![work.id,work.agent_id,work.objective,work.revision,work.created_at.to_rfc3339(),work.updated_at.to_rfc3339(),serde_json::to_string(&work)?])?;
            super::super::super::evidence::insert_brief_evidence_tx(tx,&brief)?;Ok(())
        })?;
        Ok((home, db, work, brief))
    }
    #[test]
    fn explicit_foreign_work_report_keeps_current_storage_namespace_and_never_adopts() -> Result<()>
    {
        let (_home, db, work, brief) = fixture()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let read = db
            .inspect_work_item_for_explicit_recall(
                &scope(),
                &ledger,
                &work.id,
                64 * 1024 * 1024,
                65_536,
            )?
            .unwrap();
        let (actual, material) = read.into_parts();
        assert_eq!(actual, work);
        assert_eq!(material.completion_report().unwrap().text, brief.text);
        assert_eq!(
            material.completion_report().unwrap().brief_id.as_deref(),
            Some(brief.id.as_str())
        );
        let count:u64=db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_evidence_current)+(SELECT COUNT(*) FROM work_domain_records)",[],|row|row.get(0))?;
        assert_eq!(count, 0);
        assert!(ledger.report()?.observed_bytes > 0);
        drop((actual, material));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        // Identical foreign-only Brief does not become visible via recalled Work.
        db.transaction(|tx| {
            tx.execute(
                "UPDATE briefs SET agent_id='foreign-work-owner' WHERE evidence_id='current-brief'",
                [],
            )?;
            Ok(())
        })?;
        let read = db
            .inspect_work_item_for_explicit_recall(
                &scope(),
                &ledger,
                &work.id,
                64 * 1024 * 1024,
                65_536,
            )?
            .unwrap();
        assert!(read.into_parts().1.completion_report().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn explicit_report_empty_existing_message_keeps_original_brief_fallback() -> Result<()> {
        let (_home, db, work, brief) = fixture_with_brief(|brief| {
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: "original-entry".into(),
                relation: crate::types::BriefContentSourceRelation::DerivedFrom,
            };
        })?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut message = MessageEnvelope::new(
            &scope().agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: " ".into() },
        );
        message.id = "empty-message".into();
        message.message_seq = Some(1);
        let mut entry = TranscriptEntry::new(
            &scope().agent_id,
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(message.id.clone()),
            serde_json::json!({"body":{"text":"not the original report"}}),
        );
        entry.id = "original-entry".into();
        db.transaction(|tx| {
            super::super::super::evidence::insert_message_evidence_tx(tx, &message)?;
            super::super::super::evidence::insert_transcript_evidence_tx(tx, &entry)?;
            Ok(())
        })?;
        let read = db
            .inspect_work_item_for_explicit_recall(
                &scope(),
                &ledger,
                &work.id,
                64 * 1024 * 1024,
                65_536,
            )?
            .unwrap();
        assert_eq!(
            read.into_parts().1.completion_report().unwrap().text,
            brief.text
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn explicit_report_all_normalized_raw_and_object_key_nodes_precede_decode_and_share_work_remaining(
    ) -> Result<()> {
        let (_raw_home, raw_db, raw_work, _original_brief) = fixture()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        raw_db.transaction(|tx| {
            tx.execute(
                "UPDATE briefs SET preview=?1,payload_json='{' WHERE evidence_id='current-brief'",
                ["x".repeat(65 * 1024)],
            )?;
            Ok(())
        })?;
        let error = raw_db
            .inspect_work_item_for_explicit_recall(
                &scope(),
                &ledger,
                &raw_work.id,
                512 * 1024,
                65_536,
            )
            .err()
            .context("raw source passed before malformed JSON")?;
        assert!(
            matches!(
                error.downcast_ref::<super::super::super::work_snapshot::WorkSnapshotError>(),
                Some(
                    super::super::super::work_snapshot::WorkSnapshotError::ResourceExhausted(
                        "explicit Work report combined material"
                    )
                )
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let keys = (0..1000)
            .map(|n| (format!("k{n}"), serde_json::Value::Null))
            .collect::<serde_json::Map<_, _>>();
        let (_native_home, db, work, _native_brief) = fixture_with_brief(|brief| {
            brief.attachments = Some(vec![BriefAttachment {
                kind: "original bounded report".into(),
                name: "object keys".into(),
                uri: None,
                value: Some(serde_json::Value::Object(keys)),
            }]);
        })?;
        let raw:u64=db.connection()?.query_row("SELECT length(CAST(payload_json AS BLOB))*8 FROM briefs WHERE evidence_id='current-brief'",[],|row|row.get(0))?;
        assert!(raw < 2 * 1024 * 1024);
        let error = db
            .inspect_work_item_for_explicit_recall(
                &scope(),
                &ledger,
                &work.id,
                64 * 1024 * 1024,
                8192,
            )
            .err()
            .context("object-key nodes were not counted before Value decode")?;
        assert!(
            matches!(
                error.downcast_ref::<super::super::super::work_snapshot::WorkSnapshotError>(),
                Some(
                    super::super::super::work_snapshot::WorkSnapshotError::ResourceExhausted(
                        "explicit Work report combined material"
                    )
                )
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let read = db
            .inspect_work_item_for_explicit_recall(
                &scope(),
                &ledger,
                &work.id,
                64 * 1024 * 1024,
                65_536,
            )?
            .unwrap();
        let cost = read.material_estimate();
        drop(read);
        let error = db
            .inspect_work_item_for_explicit_recall(&scope(), &ledger, &work.id, cost.0 - 1, 65_536)
            .err()
            .context("Work row and report source reset their allowance")?;
        assert!(
            matches!(
                error.downcast_ref::<super::super::super::work_snapshot::WorkSnapshotError>(),
                Some(
                    super::super::super::work_snapshot::WorkSnapshotError::ResourceExhausted(
                        "explicit Work report combined material"
                    )
                )
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
