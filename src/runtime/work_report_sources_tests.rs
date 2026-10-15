use super::*;
use crate::{
    runtime::{tests::support::context_config, RuntimeHandle},
    runtime_db::{
        evidence::{
            append_transcript_entry_tx, insert_brief_evidence_tx,
            insert_delivery_summary_evidence_tx, insert_message_evidence_tx,
            upsert_agent_identity_tx, upsert_agent_state_tx,
        },
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, BriefContentSourceRelation, BriefKind, MessageEnvelope,
        MessageKind, MessageOrigin, Priority, TranscriptEntry,
    },
};
use rusqlite::{params, Connection, Transaction};

fn time() -> chrono::DateTime<chrono::Utc> {
    "2026-10-12T00:00:00.123456789Z".parse().unwrap()
}
fn work(id: &str) -> WorkItemRecord {
    let mut record = WorkItemRecord::new("report-agent", id, WorkItemState::Completed);
    record.id = id.into();
    record.created_at = time();
    record.updated_at = time();
    record
}
fn summary(id: &str, work: &str, text: &str, offset: i64) -> DeliverySummaryRecord {
    let mut value = DeliverySummaryRecord::new("report-agent", work, text, Some(7), None);
    value.id = id.into();
    value.created_at = time() + chrono::Duration::nanoseconds(offset);
    value
}
fn insert_work(tx: &Transaction<'_>, value: &WorkItemRecord) -> Result<()> {
    // Original v81 canonical projection: later compatibility columns do not
    // exist until open_and_migrate. Keep payload nanos and normalized millis.
    tx.execute("INSERT INTO work_items(work_item_id,agent_id,state,objective,plan_status,revision,current_focus,created_at,updated_at,completed_at,plan_artifact_path,last_turn_id,payload_json) VALUES(?1,?2,?3,?4,?5,?6,0,?7,?8,?9,?10,?11,?12)",
        params![value.id,value.agent_id,serde_json::to_value(&value.state)?.as_str().unwrap(),value.objective,
            serde_json::to_value(value.plan_status)?.as_str().unwrap(),value.revision,
            crate::runtime_db::repositories::timestamp(value.created_at),crate::runtime_db::repositories::timestamp(value.updated_at),
            (value.state==WorkItemState::Completed).then(||crate::runtime_db::repositories::timestamp(value.updated_at)),
            value.plan_artifact.as_ref().map(|artifact|artifact.path.display().to_string()),value.turn_id,serde_json::to_string(value)?])?;
    Ok(())
}

fn fixture(
    seed: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<(tempfile::TempDir, RuntimeHandle)> {
    fixture_with_work_import_marker(seed, true)
}
fn fixture_with_work_import_marker(
    seed: impl FnOnce(&Transaction<'_>) -> Result<()>,
    canonical_work_complete: bool,
) -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempfile::tempdir()?;
    let state_dir = home.path().join(".holon/state");
    std::fs::create_dir_all(&state_dir)?;
    let mut connection = Connection::open(state_dir.join("runtime.sqlite"))?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 81)
    {
        apply_migration(&mut connection, migration)?;
    }
    {
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(
            &tx,
            &AgentIdentityRecord::new(
                "report-agent",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(&tx, &AgentState::new("report-agent"))?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[time().to_rfc3339()])?;
        // The released original importer finishes Work before Episode. These
        // are canonical v81 rows, not a still-pending plan-file import.
        if canonical_work_complete {
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('work_items',1,'complete','db',?1)",[time().to_rfc3339()])?;
        }
        // This fixture declares a fully canonical pre-upgrade evidence cohort,
        // not input to the original turn-id normalizer/derived importer.
        for domain in ["messages", "transcript_entries", "turn_records", "evidence"] {
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES(?1,1,'complete','db',?2)", params![domain,time().to_rfc3339()])?;
        }
        seed(&tx)?;
        tx.commit()?;
    }
    let original_messages = canonical_message_versions(&connection)?;
    drop(connection);
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "report-agent",
        home.path().to_path_buf(),
        context_config(),
    )?;
    assert_eq!(
        canonical_message_versions(&runtime.inner.runtime_db.connection()?)?,
        original_messages,
        "canonical v81 fixture messages must not be normalized again after the fixed cut"
    );
    Ok((home, runtime))
}
fn canonical_message_versions(connection: &Connection) -> Result<Vec<(String, [u8; 32])>> {
    use sha2::Digest;
    let mut query =
        connection.prepare("SELECT message_id,payload_json FROM messages ORDER BY message_id")?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get(0)?,
            sha2::Sha256::digest(row.get_ref(1)?.as_bytes()?).into(),
        ))
    })?;
    let versions = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(versions)
}
async fn get(runtime: &RuntimeHandle, id: &str) -> Result<serde_json::Value> {
    Ok(crate::tool::tools::get_work_item::execute(
        runtime,
        "report-agent",
        &AuthorityClass::OperatorInstruction,
        &serde_json::json!({"work_item_id":id}),
    )
    .await?
    .envelope
    .result
    .context("Get report result absent")?)
}
async fn list(runtime: &RuntimeHandle) -> Result<serde_json::Value> {
    Ok(crate::tool::tools::list_work_items::execute(
        runtime,
        "report-agent",
        &AuthorityClass::OperatorInstruction,
        &serde_json::json!({"filter":"completed","limit":100}),
    )
    .await?
    .envelope
    .result
    .context("List report result absent")?)
}

#[tokio::test]
async fn actual_work_report_tools_keep_two_legacy_head_semantics_after_source_tables_are_forbidden(
) -> Result<()> {
    let (_home, runtime) = fixture(|tx| {
        insert_work(tx, &work("legacy"))?;
        insert_delivery_summary_evidence_tx(
            tx,
            &summary("older-nonempty", "legacy", "original nonempty", 0),
        )?;
        insert_delivery_summary_evidence_tx(tx, &summary("newest-empty", "legacy", "", 1_000_000))?;
        insert_delivery_summary_evidence_tx(
            tx,
            &summary("unrelated", "outside", "must stay outside capture", 2),
        )?;
        Ok(())
    })?;
    let actual_original_head = runtime
        .inner
        .runtime_db
        .evidence()
        .latest_delivery_summary("report-agent", "legacy")?
        .context("original latestAny head absent")?;
    assert_eq!(actual_original_head.id, "newest-empty");
    assert!(actual_original_head.text.is_empty());
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    // Bootstrap starts the observer by invalidating WatchRestart; cancelling
    // that task can leave its first refresh pending. Freeze an actual Ready
    // observation before forbidding every ordinary reader.
    runtime.refresh_context_resources().await?;
    assert_eq!(
        runtime.inner.context_resources.freshness()?,
        crate::runtime::resource_observation::ResourceFreshness::Ready
    );
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let sources = snapshot
        .report_sources_for_test()
        .context("report capture missing")?;
    assert_eq!(sources.records.len(), 2);
    assert!(!sources
        .records
        .iter()
        .any(|value| value.locator.key.id == "unrelated"));
    let weak = sources
        .records
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TABLE work_items; DROP TABLE delivery_summaries; DROP TABLE briefs; DROP TABLE transcript_entries; DROP TABLE messages;")?;
    let single = crate::diagnostics::resource_io::deny_readers(get(&runtime, "legacy")).await?;
    assert!(
        single["work_item"]["completion_report"].is_null(),
        "Get latestAny empty must not select an older report"
    );
    let listed = crate::diagnostics::resource_io::deny_readers(list(&runtime)).await?;
    assert_eq!(listed["returned"], 1);
    assert_eq!(
        listed["work_items"][0]["completion_report"]["text"],
        "original nonempty"
    );
    assert_eq!(
        listed["work_items"][0]["completion_report"]["delivery_summary_id"],
        "older-nonempty"
    );
    let record = snapshot.selected_work_item("legacy").unwrap();
    assert_eq!(
        snapshot
            .work_report_delivery_summary(record, false)?
            .unwrap()
            .id,
        "newest-empty"
    );
    drop(runtime);
    assert!(
        weak.iter().all(|weak| weak.upgrade().is_some()),
        "same publication report Pins survive runtime release"
    );
    assert_eq!(
        snapshot
            .work_report_delivery_summary(record, true)?
            .unwrap()
            .text,
        "original nonempty"
    );
    drop(snapshot);
    assert!(weak.iter().all(|weak| weak.upgrade().is_none()));
    Ok(())
}

#[tokio::test]
async fn actual_brief_report_uses_owning_transcript_message_and_exact_optional_absence_without_sql(
) -> Result<()> {
    let (_home, runtime) = fixture(|tx| {
        let mut record = work("resolved");
        record.result_brief_id = Some("brief".into());
        insert_work(tx, &record)?;
        let mut message = MessageEnvelope::new(
            "report-agent",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Json {
                value: serde_json::json!({"original":"message report"}),
            },
        );
        message.id = "message".into();
        message.message_seq = Some(1);
        message.created_at = time();
        insert_message_evidence_tx(tx, &message)?;
        let mut entry = TranscriptEntry::new(
            "report-agent",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some("message".into()),
            serde_json::json!({"body":{"text":"transcript fallback"}}),
        );
        entry.id = "transcript".into();
        entry.created_at = time();
        append_transcript_entry_tx(tx, &entry)?;
        let mut brief = BriefRecord::new(
            "report-agent",
            BriefKind::Result,
            "brief fallback",
            None,
            None,
        );
        brief.id = "brief".into();
        brief.work_item_id = Some("resolved".into());
        brief.created_at = time();
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: "transcript".into(),
            relation: BriefContentSourceRelation::DerivedFrom,
        };
        insert_brief_evidence_tx(tx, &brief)?;
        let mut optional = work("optional");
        optional.result_brief_id = Some("optional-brief".into());
        insert_work(tx, &optional)?;
        brief.id = "optional-brief".into();
        brief.work_item_id = Some("optional".into());
        brief.text = "accepted optional fallback".into();
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: "missing-original-transcript".into(),
            relation: BriefContentSourceRelation::DerivedFrom,
        };
        insert_brief_evidence_tx(tx, &brief)?;
        // The missing Transcript is optional only along this selected Work
        // completion edge. A recent-Brief owning path would remain strict.
        for index in 0..crate::runtime::scheduler::SCHEDULER_BRIEF_LIMIT {
            let mut recent = BriefRecord::new(
                "report-agent",
                BriefKind::Result,
                "independent recent Brief",
                None,
                None,
            );
            recent.id = format!("recent-without-source-{index:03}");
            recent.created_at = time() + chrono::Duration::seconds(1);
            insert_brief_evidence_tx(tx, &recent)?;
        }
        Ok(())
    })?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    // Bootstrap starts the observer by invalidating WatchRestart; cancelling
    // that task can leave its first refresh pending. Freeze an actual Ready
    // observation before forbidding every ordinary reader.
    runtime.refresh_context_resources().await?;
    assert_eq!(
        runtime.inner.context_resources.freshness()?,
        crate::runtime::resource_observation::ResourceFreshness::Ready
    );
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let sources = snapshot.report_sources_for_test().unwrap();
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let captured = root.input_evidence.as_ref().unwrap().capture()?;
    let missing = Key::new(Family::Transcript, "missing-original-transcript");
    assert!(captured.completion_source_observations.iter().any(|proof| {
        proof.declared_target == missing && proof.head.key.id == "optional-brief"
    }));
    assert!(!captured.unavailable.contains(&missing));
    assert!(captured.lookup(&missing).is_err());
    assert!(captured
        .windows
        .iter()
        .all(|window| { !window.keys.iter().any(|key| key.id == "optional-brief") }));
    drop(captured);
    drop(root);
    assert_eq!(
        sources
            .records
            .iter()
            .filter(|value| value.locator.key.family == Family::LegacyDelivery)
            .count(),
        0
    );
    assert!(sources
        .unavailable
        .iter()
        .any(|key| key.id == "missing-original-transcript"));
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TABLE work_items; DROP TABLE delivery_summaries; DROP TABLE briefs; DROP TABLE transcript_entries; DROP TABLE messages;")?;
    let value = crate::diagnostics::resource_io::deny_readers(get(&runtime, "resolved")).await?;
    assert_eq!(
        value["work_item"]["completion_report"]["source"],
        "result_brief"
    );
    assert_eq!(
        value["work_item"]["completion_report"]["text"],
        "{\"original\":\"message report\"}"
    );
    let value = crate::diagnostics::resource_io::deny_readers(get(&runtime, "optional")).await?;
    assert_eq!(
        value["work_item"]["completion_report"]["text"],
        "accepted optional fallback"
    );
    assert_eq!(
        value["work_item"]["completion_report"]["brief_id"],
        "optional-brief"
    );
    Ok(())
}

#[tokio::test]
async fn work_report_source_pins_and_render_copies_share_original_query_allowance() -> Result<()> {
    let (_home, runtime) = fixture(|tx| {
        insert_work(tx, &work("large"))?;
        let mut value = summary("large-source", "large", &"report".repeat(512), 0);
        value.evidence = Some(serde_json::json!({"not_cloned_by_report":vec![0;1000]}));
        insert_delivery_summary_evidence_tx(tx, &value)?;
        Ok(())
    })?;
    runtime.stop_context_resource_observer().await?;
    let owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let work = root
        .work
        .as_ref()
        .unwrap()
        .capture(&super::super::resident_work::WorkCaptureRequest::current())?;
    let inputs = root.input_evidence.as_deref();
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    let mut total = (0usize, 0usize);
    let mut owned = (0usize, 0usize);
    let captured = CapturedWorkReportSources::capture(&work, inputs, ledger, |cost, is_owned| {
        total.0 += cost.0;
        total.1 += cost.1;
        if is_owned {
            owned.0 += cost.0;
            owned.1 += cost.1;
        }
        Ok(())
    })?;
    assert!(
        owned.1 < 1024,
        "Legacy evidence nodes are shared, not cloned into report fields"
    );
    assert!(total.1 > owned.1);
    let pins = ledger.report()?.observed_bytes;
    assert_eq!(
        pins, before,
        "Pins classify the identical source Arc without another body allocation"
    );
    drop(captured);
    assert_eq!(ledger.report()?.observed_bytes, before);
    let limit = total.0 - 1;
    let mut used = 0usize;
    let failure = CapturedWorkReportSources::capture(&work, inputs, ledger, |cost, _| {
        used = used.checked_add(cost.0).context("fixture byte overflow")?;
        ensure!(
            used <= limit,
            super::super::resident_work::WorkReadError::ResourceExhausted("report joint phase")
        );
        Ok(())
    })
    .err()
    .context("whole original report phase must reject")?;
    assert!(
        matches!(
            failure.downcast_ref::<super::super::resident_work::WorkReadError>(),
            Some(super::super::resident_work::WorkReadError::ResourceExhausted(_))
        ),
        "{failure:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}

#[tokio::test]
async fn original_empty_existing_message_uses_brief_fallback_in_actual_get_and_list() -> Result<()>
{
    let (_home, runtime) = fixture(|tx| {
        for (index, body) in [
            MessageBody::Text { text: "   ".into() },
            MessageBody::Brief {
                text: "".into(),
                title: None,
                attachments: None,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("empty-{index}");
            let mut record = work(&id);
            let message_id = format!("empty-message-{index}");
            let entry_id = format!("empty-transcript-{index}");
            let brief_id = format!("empty-brief-{index}");
            record.result_brief_id = Some(brief_id.clone());
            insert_work(tx, &record)?;
            let mut message = MessageEnvelope::new(
                "report-agent",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                body,
            );
            message.id = message_id.clone();
            message.message_seq = Some(index as u64 + 1);
            message.created_at = time();
            insert_message_evidence_tx(tx, &message)?;
            let mut entry = TranscriptEntry::new(
                "report-agent",
                TranscriptEntryKind::IncomingMessage,
                None,
                Some(message_id),
                serde_json::json!({"body":{"text":"must not use this transcript text"}}),
            );
            entry.id = entry_id.clone();
            entry.created_at = time();
            append_transcript_entry_tx(tx, &entry)?;
            let mut brief = BriefRecord::new(
                "report-agent",
                BriefKind::Result,
                "original Brief fallback",
                None,
                None,
            );
            brief.id = brief_id;
            brief.work_item_id = Some(id);
            brief.created_at = time();
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id,
                relation: BriefContentSourceRelation::DerivedFrom,
            };
            insert_brief_evidence_tx(tx, &brief)?;
        }
        Ok(())
    })?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    // Bootstrap starts the observer by invalidating WatchRestart; cancelling
    // that task can leave its first refresh pending. Freeze an actual Ready
    // observation before forbidding every ordinary reader.
    runtime.refresh_context_resources().await?;
    assert_eq!(
        runtime.inner.context_resources.freshness()?,
        crate::runtime::resource_observation::ResourceFreshness::Ready
    );
    // Compare against the actual old resolver before its SQL tables vanish.
    for index in 0..2 {
        let brief = runtime
            .storage()
            .read_brief_by_id(&format!("empty-brief-{index}"))?
            .unwrap();
        assert_eq!(
            crate::object_resolver::RuntimeObjectResolver::new(runtime.storage())
                .resolve_brief_content(&brief)?,
            "original Brief fallback"
        );
    }
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TABLE work_items; DROP TABLE delivery_summaries; DROP TABLE briefs; DROP TABLE transcript_entries; DROP TABLE messages;")?;
    for index in 0..2 {
        let result =
            crate::diagnostics::resource_io::deny_readers(get(&runtime, &format!("empty-{index}")))
                .await?;
        assert_eq!(
            result["work_item"]["completion_report"]["text"],
            "original Brief fallback"
        );
    }
    let result = crate::diagnostics::resource_io::deny_readers(list(&runtime)).await?;
    assert_eq!(result["returned"], 2);
    assert!(result["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|work| work["completion_report"]["text"] == "original Brief fallback"));
    assert!(snapshot
        .report_sources_for_test()
        .unwrap()
        .records
        .iter()
        .any(|body| body.locator.key.id == "empty-message-0"));
    Ok(())
}

#[tokio::test]
async fn sixteen_work_owners_of_shared_long_report_debit_each_render_and_keep_single_source_arcs(
) -> Result<()> {
    let text = "shared long report ".repeat(1024);
    let (_home, runtime) = fixture(|tx| {
        for n in 0..16 {
            let mut record = work(&format!("shared-{n:02}"));
            record.result_brief_id = Some("shared-brief".into());
            insert_work(tx, &record)?;
        }
        let mut message = MessageEnvelope::new(
            "report-agent",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.clone() },
        );
        message.id = "shared-message".into();
        message.message_seq = Some(1);
        message.created_at = time();
        insert_message_evidence_tx(tx, &message)?;
        let mut entry = TranscriptEntry::new(
            "report-agent",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(message.id),
            serde_json::json!({"blocks":[]}),
        );
        entry.id = "shared-transcript".into();
        entry.created_at = time();
        append_transcript_entry_tx(tx, &entry)?;
        let mut brief = BriefRecord::new(
            "report-agent",
            BriefKind::Result,
            "original shared fallback",
            None,
            None,
        );
        brief.id = "shared-brief".into();
        brief.created_at = time();
        assert!(brief.work_item_id.is_none());
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: entry.id,
            relation: BriefContentSourceRelation::DerivedFrom,
        };
        insert_brief_evidence_tx(tx, &brief)?;
        Ok(())
    })?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    // Bootstrap starts the observer by invalidating WatchRestart; cancelling
    // that task can leave its first refresh pending. Freeze an actual Ready
    // observation before forbidding every ordinary reader.
    runtime.refresh_context_resources().await?;
    assert_eq!(
        runtime.inner.context_resources.freshness()?,
        crate::runtime::resource_observation::ResourceFreshness::Ready
    );
    let owner = runtime.begin_mutation().await?;
    let full = runtime
        .capture_work_report_query_with_limits_for_test(&owner, (64 * 1024 * 1024, 65_536))?;
    let sources = full.report_sources_for_test().unwrap();
    assert_eq!(sources.records.len(), 3);
    let message = sources
        .records
        .iter()
        .find(|value| value.locator.key.family == Family::Message)
        .unwrap();
    let per_owner = report_output_material(&message.record)?;
    let cost = full.material_estimate()?;
    let cap = usize::try_from(cost.0)?
        .checked_sub(
            per_owner
                .0
                .checked_mul(7)
                .context("fixture render overflow")?,
        )
        .context("fixture prior underflow")?;
    assert!(cap > per_owner.0 * 2 && cap < 64 * 1024 * 1024);
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .capture_work_report_query_with_limits_for_test(&owner, (cap, 65_536))
        .err()
        .context("shared source render material was charged only once")?;
    assert!(
        matches!(
            error.downcast_ref::<super::super::resident_work::WorkReadError>(),
            Some(super::super::resident_work::WorkReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    let weak = sources
        .records
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    drop(owner);
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TABLE work_items; DROP TABLE delivery_summaries; DROP TABLE briefs; DROP TABLE transcript_entries; DROP TABLE messages;")?;
    let result = crate::diagnostics::resource_io::deny_readers(list(&runtime)).await?;
    assert_eq!(result["returned"], 16);
    assert!(result["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|work| work["completion_report"]["text"] == text));
    drop(runtime);
    assert!(weak.iter().all(|weak| weak.upgrade().is_some()));
    drop(full);
    assert!(weak.iter().all(|weak| weak.upgrade().is_none()));
    Ok(())
}

#[tokio::test]
async fn actual_foreign_get_inspects_only_current_report_source_without_input_or_work_adoption(
) -> Result<()> {
    let (_home, runtime) = fixture(|_| Ok(()))?;
    runtime.stop_context_resource_observer().await?;
    let before = runtime.capture_work_query_snapshot_with_resources().await?;
    let work_root = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .work
        .clone()
        .unwrap();
    let input_root = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .input_evidence
        .clone();
    let mut recalled = work("explicit-foreign-report");
    recalled.agent_id = "foreign-report-owner".into();
    recalled.result_brief_id = Some("explicit-current-brief".into());
    let mut brief = BriefRecord::new(
        "report-agent",
        BriefKind::Result,
        "current original explicit report",
        None,
        None,
    );
    brief.id = "explicit-current-brief".into();
    brief.work_item_id = Some(recalled.id.clone());
    // Post-cutover raw evidence is never adopted by an ordinary capture. This
    // original explicit by-ID inspection cause is the only reader of these rows.
    runtime.inner.runtime_db.transaction(|tx| {
        insert_work(tx, &recalled)?;
        insert_brief_evidence_tx(tx, &brief)?;
        Ok(())
    })?;
    assert!(before.selected_work_item(&recalled.id).is_none());
    let report = get(&runtime, &recalled.id).await?;
    assert_eq!(report["work_item"]["agent_id"], "foreign-report-owner");
    assert_eq!(report["work_item"]["completion_report"]["text"], brief.text);
    assert_eq!(
        report["work_item"]["completion_report"]["brief_id"],
        brief.id
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(Arc::ptr_eq(root.work.as_ref().unwrap(), &work_root));
    assert!(match (&input_root, &root.input_evidence) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    });
    let aliases:u64=runtime.inner.runtime_db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_evidence_current WHERE record_id='explicit-current-brief')+(SELECT COUNT(*) FROM work_domain_records WHERE record_id='explicit-foreign-report')",[],|row|row.get(0))?;
    assert_eq!(aliases, 0);
    Ok(())
}

#[tokio::test]
async fn prepared_completion_keeps_original_matching_legacy_metadata_before_new_brief_ack(
) -> Result<()> {
    let text = "original matching prepared report";
    let (_home, runtime) = fixture(|tx| {
        let mut selected = work("prepared-legacy");
        selected.state = WorkItemState::Open;
        insert_work(tx, &selected)?;
        insert_delivery_summary_evidence_tx(
            tx,
            &summary("original-prepared-metadata", &selected.id, text, 0),
        )?;
        Ok(())
    })?;
    let candidate = crate::tool::spec::CompletionReportCandidate {
        text: text.into(),
        citations: Vec::new(),
        source_turn_index: 1,
        source_round: 1,
        source_turn_id: None,
        source_message_id: None,
        source_assistant_round_id: "original-prepared-round".into(),
        source_tool_call_id: "original-prepared-tool".into(),
    };
    let tool = crate::tool::tools::complete_work_item::complete_with_report_candidate(
        &runtime,
        "prepared-legacy".into(),
        crate::runtime::WorkItemCompletionAuthority::Control,
        Some(&candidate),
        Vec::new(),
        "same_assistant_round_preceding_text",
    )
    .await?;
    let result = tool
        .envelope
        .result
        .as_ref()
        .context("actual prepared result absent")?;
    assert_eq!(result["completion_phase"], "prepared");
    assert_eq!(result["work_item"]["completion_report"]["text"], text);
    assert_eq!(
        result["work_item"]["completion_report"]["source"],
        "delivery_summary"
    );
    assert_eq!(
        result["work_item"]["completion_report"]["delivery_summary_id"],
        "original-prepared-metadata"
    );
    assert_eq!(
        result["work_item"]["completion_report"]["source_turn_index"],
        7
    );
    assert_eq!(
        result["work_item"]["completion_report"]["created_at"],
        serde_json::to_value(time())?
    );
    assert!(result["work_item"]["completion_report"]["brief_id"].is_null());
    let prepared = tool
        .prepared_work_item_completion
        .as_ref()
        .context("actual prepared command absent")?;
    assert!(runtime
        .storage()
        .read_brief_by_id(&prepared.brief.id)?
        .is_none());
    assert_eq!(
        runtime
            .latest_work_item("prepared-legacy")
            .await?
            .unwrap()
            .state,
        WorkItemState::Open
    );
    let material = prepared
        ._result_view_material
        .as_ref()
        .context("original prepared material absent")?
        .clone();
    let read = runtime.prepare_planned_work_item_result_read(
        &prepared.record,
        prepared
            ._resource_plan
            .clone()
            .context("original plan absent")?,
    )?;
    let snapshot = runtime
        .capture_work_query_snapshot_for_mutation_result(read.material())
        .await?;
    runtime.stop_context_resource_observer().await?;
    runtime
        .inner
        .runtime_db
        .connection()?
        .execute_batch("DROP TABLE briefs;DROP TABLE delivery_summaries;DROP TABLE work_items;")?;
    let (view, wire) = crate::diagnostics::resource_io::deny_readers(async {
        crate::tool::tools::work_item_query::view_for_owned_work_item_in_snapshot(
            &runtime, snapshot, read,
        )
    })
    .await?;
    let report = view
        .completion_report
        .context("prepared report disappeared")?;
    assert_eq!(
        report.delivery_summary_id.as_deref(),
        Some("original-prepared-metadata")
    );
    assert_eq!(report.source_turn_index, Some(7));
    assert_eq!(report.created_at, Some(time()));
    let weak = Arc::downgrade(&material);
    drop(material);
    drop(tool);
    assert!(
        weak.upgrade().is_none(),
        "separate original read wire owns its own guarded result"
    );
    drop(wire);
    Ok(())
}

#[tokio::test]
async fn canonical_v81_report_setup_preserves_payload_and_rejects_pending_work_rewrite(
) -> Result<()> {
    let original = work("canonical-original");
    let (home, runtime) = fixture(|tx| insert_work(tx, &original))?;
    assert_eq!(
        runtime.latest_work_item(&original.id).await?.as_ref(),
        Some(&original)
    );
    let raw: String = runtime.inner.runtime_db.connection()?.query_row(
        "SELECT payload_json FROM work_items WHERE work_item_id=?1",
        [&original.id],
        |row| row.get(0),
    )?;
    assert_eq!(raw, serde_json::to_string(&original)?);
    assert!(!crate::work_item_plan::plan_path(home.path(), &original.id).exists());
    let fixed: bool = runtime.inner.runtime_db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM episode_continuity_seeds WHERE agent_id='report-agent' AND import_required=0)", [], |row|row.get(0))?;
    assert!(fixed);
    runtime.stop_context_resource_observer().await?;
    let error = fixture_with_work_import_marker(|tx| insert_work(tx, &original), false)
        .err()
        .context("Episode marker incorrectly authorized pending Work rewrite")?;
    assert!(
        error
            .to_string()
            .contains("qualified Work canonical record changed"),
        "{error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn independent_recent_brief_path_cannot_use_work_report_optional_absence() -> Result<()> {
    let error = fixture(|tx| {
        let mut record = work("strict-recent-owner");
        record.result_brief_id = Some("strict-recent-head".into());
        insert_work(tx, &record)?;
        let mut brief = BriefRecord::new(
            "report-agent",
            BriefKind::Result,
            "original fallback",
            None,
            None,
        );
        brief.id = "strict-recent-head".into();
        brief.work_item_id = Some(record.id);
        brief.created_at = time();
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: "missing-strict-recent-transcript".into(),
            relation: BriefContentSourceRelation::DerivedFrom,
        };
        insert_brief_evidence_tx(tx, &brief)?;
        Ok(())
    })
    .err()
    .context("independent recent Brief path accepted a missing Transcript")?;
    assert!(
        format!("{error:#}")
            .contains("transcript:missing-strict-recent-transcript unavailable without tombstone"),
        "{error:#}"
    );
    Ok(())
}
