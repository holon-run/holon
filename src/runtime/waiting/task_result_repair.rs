use super::*;

impl RuntimeHandle {
    /// Repair a closed admission using a new runtime wake. The original result
    /// envelope and its ingress turn remain immutable.
    pub(in crate::runtime) fn task_result_repair_source(
        &self,
        record: &crate::runtime_db::TaskResultSettlementRecord,
        waiter_work_item_id: Option<&str>,
    ) -> Result<(MessageEnvelope, Option<QueueEntryRecord>)> {
        let mut consuming_record = record.clone();
        consuming_record.work_item_id = waiter_work_item_id.map(str::to_owned);
        let owner_key = task_result_recovery::recovery_owner_key(&consuming_record);
        if let Some(message_id) = task_result_recovery::outstanding_recovery_message_id(
            self,
            &record.agent_id,
            waiter_work_item_id,
            &owner_key,
        )? {
            let message = self
                .inner
                .storage
                .read_message_by_id(&message_id)?
                .ok_or_else(|| anyhow!("task-result recovery wake evidence is missing"))?;
            let queue = self.inner.runtime_db.queue_entries().latest(&message_id)?;
            return Ok((message, queue));
        }
        let mut message = MessageEnvelope::new(
            &record.agent_id,
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "task_result_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Background,
            MessageBody::Text {
                text: "Handle the pending task results for this exact waiter.".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.id = crate::ids::runtime_id("msg_task_result_recovery");
        message.work_item_id = waiter_work_item_id.map(str::to_owned);
        message
            .source_refs
            .insert("task_result_owner_key".into(), owner_key);
        message.source_refs.insert(
            "task_result_rejoin_generation".into(),
            record.rejoin_generation.to_string(),
        );
        let (source_key, source_id) = record.source.result_source_ref();
        message
            .source_refs
            .insert(source_key.into(), source_id.into());
        message
            .source_refs
            .insert("task_result_message_id".into(), record.message_id.clone());
        message.source_refs.insert(
            "task_result_identity".into(),
            record.result_identity.clone(),
        );
        Ok((message, None))
    }
}
