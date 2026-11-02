//! One RunOnce cut: born Task facts and qualified Message/Tool membership.
use super::*;
use crate::{
    runtime::resident_input_evidence::{
        CapturedBirthInputs, PreparedInputEvidence, ToolContextProjection,
    },
    types::{AgentState, MessageEnvelope, TaskRecord, ToolExecutionRecord},
};

#[derive(Debug)]
pub(crate) struct RunOnceRead {
    pub(crate) state: Arc<AgentState>,
    pub(crate) tasks: Arc<crate::runtime::resident_task::CapturedTasks>,
    pub(crate) inputs: CapturedBirthInputs,
    pub(crate) current: crate::runtime_db::current_query::CapturedCurrent,
    cursor: u64,
    _task_selectors: Option<Arc<BudgetViewGuard>>,
}

fn referenced_task_ids<'a>(
    inputs: &'a CapturedBirthInputs,
    current: &'a crate::runtime_db::current_query::CapturedCurrent,
) -> impl Iterator<Item = &'a str> {
    inputs
        .records
        .iter()
        .filter_map(|record| {
            if let PreparedInputEvidence::ToolExecution(tool) = &record.record {
                run_tool_task_id(tool)
            } else {
                None
            }
        })
        .chain(current.events().filter_map(run_event_task_id))
}

pub(crate) fn run_tool_task_id(tool: &ToolContextProjection) -> Option<&str> {
    if tool.tool_name != "ExecCommand" || tool.status != crate::types::ToolExecutionStatus::Success
    {
        return None;
    }
    tool.command.task_id.as_deref()
}

pub(crate) fn exec_command_event_task_id(event: &crate::types::AuditEvent) -> Option<&str> {
    if event.kind != "tool_executed"
        || event.data.get("tool_name").and_then(|value| value.as_str()) != Some("ExecCommand")
        || event
            .data
            .get("status")
            .and_then(|value| value.as_str())
            .is_some_and(|status| status != "success")
    {
        return None;
    }
    event
        .data
        .get("task_handle")
        .and_then(|value| value.get("task_id"))
        .and_then(|value| value.as_str())
}

pub(crate) fn process_execution_event_task_id(event: &crate::types::AuditEvent) -> Option<&str> {
    if event.kind != "process_execution_requested"
        || event.data.get("surface").and_then(|value| value.as_str()) != Some("command_task")
        || event
            .data
            .get("promoted_from_exec_command")
            .and_then(|value| value.as_bool())
            != Some(true)
    {
        return None;
    }
    event.data.get("task_id").and_then(|value| value.as_str())
}

pub(crate) fn run_event_task_id(event: &crate::types::AuditEvent) -> Option<&str> {
    exec_command_event_task_id(event).or_else(|| process_execution_event_task_id(event))
}

pub(crate) struct RunOnceOwnedRecords {
    _read: RunOnceRead,
    _copies: Arc<BudgetViewGuard>,
    _tools: Option<crate::diagnostics::resource_io::ResourceReadMaterial>,
}

impl RunOnceOwnedRecords {
    pub(crate) fn events(&self) -> impl DoubleEndedIterator<Item = &crate::types::AuditEvent> {
        self._read.events()
    }

    pub(crate) fn retain_tools(
        &mut self,
        material: crate::diagnostics::resource_io::ResourceReadMaterial,
    ) {
        self._tools = Some(material);
    }
}

impl RunOnceRead {
    pub(crate) fn events(&self) -> impl DoubleEndedIterator<Item = &crate::types::AuditEvent> {
        self.current.events()
    }

    pub(crate) fn task_ids(&self) -> Result<impl Iterator<Item = &str>> {
        self.tasks.born_task_ids_after(self.cursor)
    }

    pub(crate) fn referenced_task_ids(&self) -> impl Iterator<Item = &str> {
        referenced_task_ids(&self.inputs, &self.current)
    }

    pub(crate) fn task_output_status(&self, id: &str) -> Result<crate::types::TaskStatus> {
        super::super::task_reads::task_output_status(&self.tasks, id)
    }

    pub(crate) fn tools(&self) -> impl Iterator<Item = &ToolContextProjection> {
        self.inputs.records.iter().filter_map(|record| {
            if let PreparedInputEvidence::ToolExecution(tool) = &record.record {
                Some(tool)
            } else {
                None
            }
        })
    }

    pub(crate) fn messages(&self) -> impl Iterator<Item = &MessageEnvelope> {
        self.inputs.records.iter().filter_map(|record| {
            if let PreparedInputEvidence::Message(message) = &record.record {
                Some(message)
            } else {
                None
            }
        })
    }
}

impl crate::runtime::RuntimeHandle {
    pub(crate) fn run_once_tools(
        &self,
        read: &RunOnceRead,
    ) -> Result<(
        Vec<ToolExecutionRecord>,
        crate::diagnostics::resource_io::ResourceReadMaterial,
    )> {
        anyhow::ensure!(
            read.tasks.scope.agent_id == self.inner.mutation_owner.agent_id()
                && read.tasks.scope.identity_incarnation == self.inner.identity_incarnation,
            "RunOnce product Tool runtime differs"
        );
        read.inputs
            .validate_binding(&read.tasks.scope, &self.inner.input_evidence_budget)?;
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &read.tasks.scope,
        )?;
        let tools = material.read(|| {
            let count = read.tools().count();
            crate::diagnostics::resource_io::charge_owned_metadata(
                u64::try_from(
                    count
                        .checked_mul(std::mem::size_of::<ToolExecutionRecord>())
                        .context("RunOnce Tool vector size overflow")?,
                )?,
                count,
            )?;
            let mut connection = self.runtime_db().connection()?;
            let transaction = connection.transaction()?;
            let mut tools = Vec::with_capacity(count);
            for accepted in &read.inputs.records {
                if matches!(accepted.record, PreparedInputEvidence::ToolExecution(_)) {
                    tools.push(
                        crate::runtime_db::input_evidence_snapshot::qualified_tool_body_on(
                            &transaction,
                            accepted,
                        )?,
                    );
                }
            }
            tools.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
            Ok(tools)
        })?;
        Ok((tools, material))
    }

    pub(crate) fn capture_run_once(&self, session: &TaskBirthSession) -> Result<RunOnceRead> {
        self.capture_run_once_inner(session, false)
    }

    pub(crate) fn capture_run_once_statuses(
        &self,
        session: &TaskBirthSession,
    ) -> Result<RunOnceRead> {
        self.capture_run_once_inner(session, true)
    }

    pub(crate) async fn prepare_run_once_current_query(
        &self,
        session: &TaskBirthSession,
    ) -> Result<()> {
        {
            let _reservation = self.inner.mutation_owner.reserve().await?;
            let root = self
                .inner
                .mutation_owner
                .capture()?
                .context("RunOnce root is unpublished")?;
            self.validate_task_birth_session_from_root(session, &root)?;
            if root
                .current
                .as_ref()
                .and_then(|current| current.query.as_ref())
                .is_some()
            {
                return Ok(());
            }
        }
        // Missing query coverage needs an explicit actor. Ready reads and
        // released Unknown writers must never turn into reconciliation here.
        let mut owner = self.begin_mutation().await?;
        let root = owner.capture_published_root()?;
        self.validate_task_birth_session_from_root(session, &root)?;
        if root
            .current
            .as_ref()
            .and_then(|current| current.query.as_ref())
            .is_none()
        {
            owner.prepare_current_query(None).await?;
        }
        Ok(())
    }

    pub(crate) async fn capture_run_once_statuses_after_writer(
        &self,
        session: &TaskBirthSession,
    ) -> Result<RunOnceRead> {
        // Wait for an in-flight original writer, not for reconciliation. A
        // released owner that remains Unknown must still fail the pure read.
        let _reservation = self.inner.mutation_owner.reserve().await?;
        self.capture_run_once_statuses(session)
    }

    fn capture_run_once_inner(
        &self,
        session: &TaskBirthSession,
        statuses: bool,
    ) -> Result<RunOnceRead> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("RunOnce root is unpublished")?;
        self.validate_task_birth_session_from_root(session, &root)?;
        let domain = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        let inputs = root
            .input_evidence
            .as_ref()
            .context("RunOnce Input domain is unpublished")?;
        anyhow::ensure!(
            inputs.scope() == domain.scope(),
            "RunOnce Task/Input scope differs"
        );
        let inputs = inputs.capture_birth_inputs(
            session.input_lease.selection(),
            &self.inner.input_evidence_budget,
        )?;
        let current = root
            .current
            .as_ref()
            .and_then(|current| current.query.as_ref())
            .context("RunOnce Current query is unpublished")?
            .capture(
                session.current_lease.selection(),
                &self.inner.input_evidence_budget,
            )?;
        let (tasks, selectors) = if statuses {
            // References choose existing qualified Task bodies, never hydrate
            // canonical rows or turn an unobserved address into an empty set.
            let selectors =
                self.task_session_id_budget(session, referenced_task_ids(&inputs, &current))?;
            let selection = TaskQuerySelection {
                birth_after: Some(session.cursor),
                required_keys: referenced_task_ids(&inputs, &current)
                    .map(|id| TaskKey::new(TaskFamily::Task, id))
                    .collect(),
                ..Default::default()
            };
            (domain.capture_query(&selection)?, Some(selectors))
        } else {
            (domain.capture_query(session.lease.selection())?, None)
        };
        Ok(RunOnceRead {
            state: root.state.clone(),
            tasks: Arc::new(tasks),
            inputs,
            current,
            cursor: session.cursor,
            _task_selectors: selectors,
        })
    }

    pub(crate) fn run_once_records(
        &self,
        read: RunOnceRead,
    ) -> Result<(Vec<TaskRecord>, Vec<MessageEnvelope>, RunOnceOwnedRecords)> {
        let mut bytes = 128u64;
        let mut count = 0usize;
        for id in read.task_ids()? {
            let task = super::super::task_reads::task_record(&read.tasks, id)?
                .context("RunOnce accepted Task body is missing")?;
            bytes = bytes
                .checked_add(serialized_bytes(task)?)
                .and_then(|n| n.checked_add(std::mem::size_of::<TaskRecord>() as u64))
                .context("RunOnce Task bytes overflow")?;
            count = count.checked_add(1).context("RunOnce count overflow")?;
        }
        for message in read.messages() {
            bytes = bytes
                .checked_add(serialized_bytes(message)?)
                .and_then(|n| n.checked_add(std::mem::size_of::<MessageEnvelope>() as u64))
                .context("RunOnce Message bytes overflow")?;
            count = count.checked_add(1).context("RunOnce count overflow")?;
        }
        // Events stay borrowed from the retained Current capture; unlike the
        // Task/Message products below, they allocate no second body.
        let copies = self.task_birth_owned_budget(&read.tasks.scope, bytes, count)?;
        let mut tasks = read
            .task_ids()?
            .map(|id| {
                super::super::task_reads::task_record(&read.tasks, id)?
                    .cloned()
                    .context("RunOnce accepted Task body is missing")
            })
            .collect::<Result<Vec<_>>>()?;
        // Match the original Task reader's ID order before its stable time sort.
        tasks.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        let mut messages = read.messages().cloned().collect::<Vec<_>>();
        messages.sort_by(compare_message_order);
        Ok((
            tasks,
            messages,
            RunOnceOwnedRecords {
                _read: read,
                _copies: copies,
                _tools: None,
            },
        ))
    }
}

fn compare_message_order(left: &MessageEnvelope, right: &MessageEnvelope) -> std::cmp::Ordering {
    left.message_seq
        .cmp(&right.message_seq)
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

#[cfg(test)]
#[path = "run_once_reads_tests.rs"]
mod tests;

#[cfg(test)]
mod current_preparation_tests {
    use super::*;
    use crate::{runtime::tests::support::LifecycleHarness, types::AuditEvent};

    #[tokio::test]
    async fn explicit_run_once_current_preparation_keeps_pure_reads_and_live_selection(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        runtime
            .append_original_audit_event(AuditEvent::legacy(
                "before-run-once",
                serde_json::json!({}),
            ))
            .await?;
        let (_, session) = runtime.begin_task_birth_session().await?;
        runtime
            .append_original_audit_event(AuditEvent::legacy(
                "during-run-once",
                serde_json::json!({}),
            ))
            .await?;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await?;
        reservation.begin("run-once-unprepared-current".into())?;
        reservation.committed_prepared_with_scheduling_domains(
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            root.work.clone(),
            root.tasks.clone(),
            root.scheduling.clone(),
        )?;
        let unprepared = reservation.publish(root.state.clone())?;
        drop(reservation);
        assert!(unprepared.current.is_none());
        for error in [
            runtime.capture_run_once(&session).unwrap_err(),
            runtime.capture_run_once_statuses(&session).unwrap_err(),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("RunOnce Current query is unpublished"),
                "{error:#}"
            );
        }
        assert!(Arc::ptr_eq(
            &unprepared,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));

        runtime.prepare_run_once_current_query(&session).await?;
        let read = runtime
            .capture_run_once_statuses_after_writer(&session)
            .await?;
        assert_eq!(
            read.events()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            ["during-run-once"]
        );
        assert!(Arc::ptr_eq(&read.state, &unprepared.state));
        let ready = runtime.inner.mutation_owner.capture()?.unwrap();
        let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE messages RENAME TO run_once_messages_forbidden;
                 ALTER TABLE tasks RENAME TO run_once_tasks_forbidden;
                 ALTER TABLE agent_states RENAME TO run_once_states_forbidden;
                 ALTER TABLE agent_identities RENAME TO run_once_identities_forbidden;",
            )?;
            Ok(())
        })?;
        runtime.prepare_run_once_current_query(&session).await?;
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            before
        );
        assert!(Arc::ptr_eq(
            &ready,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
        drop(read);

        let mut reservation = runtime.inner.mutation_owner.reserve().await?;
        reservation.begin("run-once-preparation-unknown".into())?;
        drop(reservation);
        assert!(runtime
            .prepare_run_once_current_query(&session)
            .await
            .is_err());
        assert!(runtime.inner.mutation_owner.capture().is_err());
        Ok(())
    }
}
