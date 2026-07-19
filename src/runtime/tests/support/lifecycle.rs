use super::*;

pub(crate) const PRE_COMMIT_FAULTS: [crate::runtime_db::transitions::TransitionFaultPoint; 4] = [
    crate::runtime_db::transitions::TransitionFaultPoint::AfterValidation,
    crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites,
    crate::runtime_db::transitions::TransitionFaultPoint::AfterAuditWrites,
    crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit,
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DurableLifecycleSnapshot {
    pub(crate) agent_state: Option<AgentState>,
    pub(crate) work_items: Vec<WorkItemRecord>,
    pub(crate) wait_conditions: Vec<crate::types::WaitConditionRecord>,
    pub(crate) queue_entries: Vec<QueueEntryRecord>,
    pub(crate) tasks: Vec<TaskRecord>,
    pub(crate) audit_events: Vec<AuditEvent>,
    pub(crate) transcript_entries: Vec<crate::types::TranscriptEntry>,
    pub(crate) index_outbox_high_watermark: i64,
}

pub(crate) struct LifecycleHarness {
    data_dir: TempDir,
    workspace: TempDir,
    runtime: RuntimeHandle,
}

impl LifecycleHarness {
    pub(crate) fn new() -> Self {
        let data_dir = tempdir().expect("create lifecycle data dir");
        let workspace = tempdir().expect("create lifecycle workspace");
        let runtime = Self::open_runtime(&data_dir, &workspace);
        Self {
            data_dir,
            workspace,
            runtime,
        }
    }

    fn open_runtime(data_dir: &TempDir, workspace: &TempDir) -> RuntimeHandle {
        RuntimeHandle::new(
            "default",
            data_dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .expect("open lifecycle runtime")
    }

    pub(crate) fn runtime(&self) -> &RuntimeHandle {
        &self.runtime
    }

    pub(crate) fn arm_fault(&self, fault: crate::runtime_db::transitions::TransitionFaultPoint) {
        self.runtime.inject_next_transition_fault(fault);
    }

    pub(crate) fn restart(&mut self) {
        self.runtime = Self::open_runtime(&self.data_dir, &self.workspace);
    }

    pub(crate) fn snapshot(&self) -> DurableLifecycleSnapshot {
        let runtime_db = &self.runtime.inner.runtime_db;
        DurableLifecycleSnapshot {
            agent_state: runtime_db
                .agent_states()
                .latest("default")
                .expect("read agent state"),
            work_items: runtime_db
                .work_items()
                .latest_all()
                .expect("read work items"),
            wait_conditions: runtime_db
                .wait_conditions()
                .latest_all()
                .expect("read wait conditions"),
            queue_entries: runtime_db
                .queue_entries()
                .latest_all()
                .expect("read queue entries"),
            tasks: runtime_db.tasks().latest_all().expect("read tasks"),
            audit_events: self
                .runtime
                .storage()
                .read_recent_events(usize::MAX)
                .expect("read audit events"),
            transcript_entries: self
                .runtime
                .storage()
                .read_all_transcript()
                .expect("read transcript"),
            index_outbox_high_watermark: runtime_db
                .runtime_index_outbox()
                .high_watermark_for_agent("default")
                .expect("read index outbox"),
        }
    }

    pub(crate) fn assert_unchanged(&self, before: &DurableLifecycleSnapshot) {
        assert_eq!(&self.snapshot(), before);
    }
}

pub(crate) fn assert_injected_transition_fault(error: &anyhow::Error) {
    assert!(
        error
            .to_string()
            .contains("injected runtime transition fault"),
        "unexpected transition error: {error:#}"
    );
}
