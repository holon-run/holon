//! Only an original qualified Task's frozen terminal Message, never a Turn hint.
use super::*;

pub(super) struct DerivedTaskMessage {
    intent_digest: String,
    task_id: String,
    result_id: Option<String>,
    generation: Option<u64>,
    task_digest: Option<String>,
    first_message: bool,
    source: DerivedTaskMessageSource,
}
#[derive(Clone, Copy)]
enum DerivedTaskMessageSource {
    Terminal,
    OriginalChildRunning,
}

impl ExistingAdmissionInputProof {
    pub(crate) fn prepare_from_task_result_cleanup_tx(
        tx: &Transaction<'_>,
        mut input: Self,
        task: &crate::runtime_db::task_snapshot::OriginalTaskResultCleanupProof<'_, '_, '_>,
    ) -> Result<Self> {
        let (scope, _, message, result) = task.original_task_input_tx(tx)?;
        input.validate_scope_tx(tx, scope)?;
        ensure!(
            message.kind == crate::types::MessageKind::TaskResult
                && message.authority_class == crate::types::AuthorityClass::RuntimeInstruction
                && message.delivery_surface
                    == Some(crate::types::MessageDeliverySurface::TaskRejoin)
                && message.admission_context == Some(crate::types::AdmissionContext::RuntimeOwned)
                && matches!(&message.origin,crate::types::MessageOrigin::Task {task_id}
                    if task_id == &result.task_id)
                && message.task_id.as_deref() == Some(result.task_id.as_str())
                && message.agent_id == scope.agent_id
                && message.id == result.message_id,
            "derived Task input is not its original terminal Result Message"
        );
        let key = Key::new(Family::Message, &message.id);
        validate_existing_input_evidence_inner_tx(tx, scope, &key)?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE evidence_id=?1)",
            [&key.id],
            |row| row.get(0),
        )?;
        if !existing {
            let known: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE family='message' AND record_id=?1 UNION ALL SELECT 1 FROM input_evidence_eligibility WHERE family='message' AND record_id=?1)",
                [&key.id], |row| row.get(0),
            )?;
            ensure!(!known, "Task cleanup cannot revive an old Message ID");
        }
        let witness = DerivedTaskMessage {
            intent_digest: message_intent_digest(message)?,
            task_id: result.task_id.clone(),
            result_id: Some(result.result_identity.clone()),
            generation: Some(result.rejoin_generation),
            task_digest: None,
            first_message: !existing,
            source: DerivedTaskMessageSource::Terminal,
        };
        let bytes = input
            .allowed
            .iter()
            .try_fold(0usize, |n, key| n.checked_add(key.id.len()))
            .and_then(|n| n.checked_add(key.id.len()))
            .and_then(|n| n.checked_add(witness.task_id.len()))
            .and_then(|n| n.checked_add(witness.result_id.as_ref().map_or(0, String::len) + 128))
            .context("Task input footprint overflow")?;
        ensure!(
            input.allowed.len() + 1 <= MAX_RECORDS && bytes <= MAX_METADATA_BYTES,
            "Task input footprint exceeds fixed preparation bounds"
        );
        input.allowed.insert(key.clone());
        input.derived.insert(key.clone());
        ensure!(
            input.derived_task_messages.insert(key, witness).is_none(),
            "duplicate original Task input derivation"
        );
        Ok(input)
    }

    /// No-rejoin Stop feedback still owns a Message/Queue responsibility. The
    /// capability is minted from the initial whole Stop ACK in this same Tx.
    pub(crate) fn prepare_from_task_stop_message_tx(
        tx: &Transaction<'_>,
        input: Self,
        view: &crate::runtime_db::task_stop::OriginalTaskStopResultView<'_>,
        task: &crate::runtime_db::task_snapshot::TaskWriteProof,
    ) -> Result<Self> {
        let scope = task.scope();
        task.validate_task_before_tx(tx, &view.task().id)?;
        let before = task.task_before(&view.task().id)?;
        view.validate_tx(tx, scope, before)?;
        input.validate_scope_tx(tx, scope)?;
        let message = view.message();
        ensure!(
            crate::runtime_db::task_result_settlement::TaskResultSettlementRecord::pending(
                view.task(),
                message,
                message.created_at
            )?
            .is_none(),
            "Stop no-rejoin bridge cannot replace an actual settlement"
        );
        ensure!(
            message.kind == crate::types::MessageKind::TaskResult
                && message.authority_class == crate::types::AuthorityClass::RuntimeInstruction
                && message.delivery_surface
                    == Some(crate::types::MessageDeliverySurface::TaskRejoin)
                && message.admission_context == Some(crate::types::AdmissionContext::RuntimeOwned)
                && matches!(&message.origin,crate::types::MessageOrigin::Task{task_id} if task_id==&before.id)
                && message.task_id.as_deref() == Some(before.id.as_str())
                && message.agent_id == scope.agent_id,
            "Stop feedback is not its exact original Task Message"
        );
        input.insert_original_no_rejoin_task_message_tx(
            tx,
            scope,
            before,
            message,
            task_body_digest(before)?,
            DerivedTaskMessageSource::Terminal,
        )
    }

    pub(crate) fn prepare_from_original_command_message_tx(
        tx: &Transaction<'_>,
        input: Self,
        view: &crate::runtime_db::command_task_terminal::OriginalCommandTaskInputView<'_, '_, '_>,
    ) -> Result<Self> {
        view.validate_tx(tx)?;
        let scope = view.scope();
        input.validate_scope_tx(tx, scope)?;
        input.insert_original_no_rejoin_task_message_tx(
            tx,
            scope,
            view.task(),
            view.message(),
            task_body_digest(
                &crate::runtime_db::repositories::slim_task_record_for_payload(view.task()),
            )?,
            DerivedTaskMessageSource::Terminal,
        )
    }

    pub(in crate::runtime_db) fn prepare_from_original_child_running_tx(
        tx: &Transaction<'_>,
        input: Self,
        exact: &crate::runtime_db::transitions::ExistingOriginalChildRunningWriteProof<'_, '_, '_>,
    ) -> Result<Self> {
        exact.validate_transaction(tx)?;
        let scope = &input.scope;
        let task = exact.task();
        let message = exact.message();
        input.validate_scope_tx(tx, scope)?;
        ensure!(
            task.agent_id == scope.agent_id
                && task.status == crate::types::TaskStatus::Running
                && message.kind == crate::types::MessageKind::TaskStatus
                && message.agent_id == scope.agent_id
                && message.task_id.as_deref() == Some(task.id.as_str())
                && message
                    .metadata
                    .as_ref()
                    .and_then(|v| v.get("task_status"))
                    .and_then(serde_json::Value::as_str)
                    == Some("running"),
            "Running input differs from its independently qualified exact Task"
        );
        let scope = scope.clone();
        input.insert_original_no_rejoin_task_message_tx(
            tx,
            &scope,
            task,
            message,
            task_body_digest(&crate::runtime_db::repositories::slim_task_record_for_payload(task))?,
            DerivedTaskMessageSource::OriginalChildRunning,
        )
    }

    fn insert_original_no_rejoin_task_message_tx(
        mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        before: &crate::types::TaskRecord,
        message: &MessageEnvelope,
        expected_task_digest: String,
        source: DerivedTaskMessageSource,
    ) -> Result<Self> {
        let key = Key::new(Family::Message, &message.id);
        validate_existing_input_evidence_inner_tx(tx, scope, &key)?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE evidence_id=?1)",
            [&key.id],
            |r| r.get(0),
        )?;
        if !existing {
            let known:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE family='message' AND record_id=?1 UNION ALL SELECT 1 FROM input_evidence_eligibility WHERE family='message' AND record_id=?1)",[&key.id],|r|r.get(0))?;
            ensure!(!known, "Task cleanup cannot revive an old Message ID");
        }
        let bytes = self
            .allowed
            .iter()
            .try_fold(0usize, |n, k| n.checked_add(k.id.len()))
            .and_then(|n| n.checked_add(key.id.len() + before.id.len() + 256))
            .context("Stop input footprint overflow")?;
        ensure!(
            self.allowed.len() + 1 <= MAX_RECORDS && bytes <= MAX_METADATA_BYTES,
            "Stop input footprint exceeds fixed preparation bounds"
        );
        self.allowed.insert(key.clone());
        self.derived.insert(key.clone());
        ensure!(
            self.derived_task_messages
                .insert(
                    key,
                    DerivedTaskMessage {
                        intent_digest: message_intent_digest(message)?,
                        task_id: before.id.clone(),
                        result_id: None,
                        generation: None,
                        task_digest: Some(expected_task_digest),
                        first_message: !existing,
                        source,
                    }
                )
                .is_none(),
            "duplicate original Stop input derivation"
        );
        Ok(self)
    }

    pub(super) fn validate_derived_task_message_tx(
        &self,
        tx: &Transaction<'_>,
        actual: &CanonicalInputEvidence,
    ) -> Result<()> {
        let Some(expected) = self.derived_task_messages.get(&actual.key()) else {
            return Ok(());
        };
        let CanonicalInputEvidence::Message(message) = actual else {
            anyhow::bail!("derived Task input changed family");
        };
        // A later prune revokes even a retained original proof. Qualification
        // never permits a deleted body to be re-created under its old ID.
        ensure!(
            !alias_on(tx, &self.scope, &actual.key())?.is_some_and(|alias| !alias.present),
            "Task cleanup cannot revive a pruned evidence ID"
        );
        ensure!(
            message.message_seq.is_some_and(|seq| seq > 0)
                && message_intent_digest(message)? == expected.intent_digest,
            "derived Task Message differs from its original frozen fields"
        );
        // The normal rejoin path retains its actual settlement association.
        // A no-rejoin Stop instead binds the already qualified original Task
        // body; it creates neither a generation nor an activation/result row.
        let associated = if let (Some(result), Some(generation)) =
            (&expected.result_id, expected.generation)
        {
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_result_settlements r JOIN tasks t ON t.task_id=r.task_id WHERE r.result_identity=?1 AND r.agent_id=?2 AND r.task_id=?3 AND r.message_id=?4 AND r.rejoin_generation=?5 AND t.owner_agent_id=r.agent_id AND t.status IN ('completed','failed','cancelled','interrupted'))",
                params![result,self.scope.agent_id,expected.task_id,message.id,generation], |row|row.get::<_,bool>(0),
            )?
        } else {
            let raw: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM tasks WHERE task_id=?1 AND owner_agent_id=?2",
                    params![expected.task_id, self.scope.agent_id],
                    |row| row.get(0),
                )
                .optional()?;
            raw.map(|raw| -> Result<bool> {
                let task = crate::runtime_db::repositories::decode_task_payload(&raw)?;
                let status_matches = match expected.source {
                    DerivedTaskMessageSource::Terminal => {
                        crate::runtime_db::repositories::is_terminal_task_status(&task.status)
                    }
                    DerivedTaskMessageSource::OriginalChildRunning => {
                        task.status == crate::types::TaskStatus::Running
                            && message.kind == crate::types::MessageKind::TaskStatus
                    }
                };
                Ok(status_matches
                    && expected.task_digest.as_ref() == Some(&task_body_digest(&task)?))
            })
            .transpose()?
            .unwrap_or(false)
        };
        ensure!(
            associated,
            "derived Task Message lacks its actual original terminal authority"
        );
        if expected.first_message {
            let queued: bool=tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND message_id=?2 AND status='queued')",
                params![self.scope.agent_id,message.id],|row|row.get(0),
            )?;
            ensure!(
                queued,
                "derived Task Message lost its actual queue responsibility"
            );
        }
        Ok(())
    }
}

/// Exhaustive destructuring makes a newly added Message field a compile-time
/// decision. Streaming borrowed fields creates no extra body clone/JSON buffer;
/// only the append-assigned message_seq is excluded from the frozen intent.
fn message_intent_digest(message: &MessageEnvelope) -> Result<String> {
    let MessageEnvelope {
        id,
        message_seq: _,
        turn_id,
        agent_id,
        created_at,
        kind,
        origin,
        authority_class,
        priority,
        trigger_kind,
        work_item_id,
        task_id,
        source_refs,
        trace_context,
        body,
        delivery_surface,
        admission_context,
        metadata,
        correlation_id,
        causation_id,
        routing_context,
    } = message;
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(
        &mut writer,
        &(
            (
                id,
                turn_id,
                agent_id,
                created_at,
                kind,
                origin,
                authority_class,
                priority,
                trigger_kind,
                work_item_id,
            ),
            (
                task_id,
                source_refs,
                trace_context,
                body,
                delivery_surface,
                admission_context,
                metadata,
                correlation_id,
                causation_id,
                routing_context,
            ),
        ),
    )?;
    Ok(format!("{:x}", writer.0.finalize()))
}

fn task_body_digest(task: &crate::types::TaskRecord) -> Result<String> {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, task)?;
    Ok(format!("{:x}", writer.0.finalize()))
}
