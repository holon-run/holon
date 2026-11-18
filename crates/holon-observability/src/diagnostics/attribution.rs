//! Bounded, content-free stage timings. Nested stages overlap; timers count
//! attempts including errors/cancellation. Snapshots are not transactional.

pub use holon_domain::diagnostics::StageSnapshot;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Stage {
    name: &'static str,
    count: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    rows: AtomicU64,
}

impl Stage {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            count: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            rows: AtomicU64::new(0),
        }
    }

    pub fn start(&'static self) -> Timer {
        Timer {
            stage: self,
            started: Instant::now(),
            rows: 0,
        }
    }

    pub fn record(&self, elapsed: Duration, rows: u64) {
        let elapsed_ns = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        self.total_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.max_ns.fetch_max(elapsed_ns, Ordering::Relaxed);
        self.rows.fetch_add(rows, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        tracing::trace!(
            target: "holon::performance",
            stage = self.name, elapsed_ns, rows = rows,
            "performance stage completed"
        );
    }

    fn snapshot(&self) -> StageSnapshot {
        StageSnapshot {
            name: self.name.to_owned(),
            count: self.count.load(Ordering::Relaxed),
            total_ns: self.total_ns.load(Ordering::Relaxed),
            max_ns: self.max_ns.load(Ordering::Relaxed),
            rows: self.rows.load(Ordering::Relaxed),
        }
    }
}

pub struct Timer {
    stage: &'static Stage,
    started: Instant,
    rows: u64,
}

impl Timer {
    pub fn rows(&mut self, rows: usize) {
        self.rows = rows as u64;
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.stage.record(self.started.elapsed(), self.rows);
    }
}

macro_rules! stages {
    ($($id:ident => $name:literal),+ $(,)?) => {
        $(pub static $id: Stage = Stage::new($name);)+
        pub fn snapshot() -> Vec<StageSnapshot> {
            vec![$($id.snapshot()),+]
        }
    };
}

stages! {
    CONTEXT_ACQUIRE => "context.snapshot.storage_acquisition",
    CONTEXT_RENDER => "context.snapshot.render",
    CONTEXT_HISTORY_PREPARE => "context.snapshot.scoped_history_acquisition",
    CONNECTION => "db.connection.attempt",
    SIDECAR => "db.connection.sidecar_check",
    SQLITE_OPEN => "db.connection.sqlite_open",
    CONFIGURE => "db.connection.configure",
    READER_ADMISSION => "db.reader.async_admission",
    READER_BLOCKING_ADMISSION => "db.reader.blocking_admission",
    READER_CHECKOUT => "db.reader.checkout",
    READER_LEASE => "db.reader.lease_held",
    READER_OPERATION => "db.reader.operation",
    TASK_OUTPUT_HEADS => "db.task.output_heads",
    TASK_PREPARE_ATTEMPT => "runtime.task.prepare.attempt",
    TASK_PREPARE_REUSE => "runtime.task.prepare.reuse",
    TASK_PREPARE_MISS_DOMAIN => "runtime.task.prepare.miss.domain",
    TASK_PREPARE_MISS_COVERAGE => "runtime.task.prepare.miss.coverage",
    TASK_PREPARE_MISS_RECOVERY_TIME => "runtime.task.prepare.miss.recovery_time",
    TASK_DOMAIN_PREPARE => "db.task.domain_prepare",
    TASK_DOMAIN_LOAD => "db.task.domain_load",
    TASK_DOMAIN_REBUILD => "runtime.task.domain_rebuild",
    TASK_LIVE_DEPENDENCY_CHECK => "db.task.live.dependency_check",
    TASK_LIVE_UNCHANGED => "db.task.live.unchanged",
    TASK_CANDIDATE_READ => "db.task.candidate.read",
    TASK_CANDIDATE_VALIDATE => "db.task.candidate.validate",
    TASK_CANDIDATE_APPLY => "db.task.candidate.apply",
    TASK_WRITE_DELTA_FINALIZE => "db.task.write.delta_finalize",
    INPUT_SCOPE_STATE => "db.input.scope.state",
    INPUT_SCOPE_FINGERPRINT => "db.input.scope.fingerprint",
    INPUT_STATE_DECODE => "db.input.state.decode",
    INPUT_PREPARE_QUEUE => "db.input.prepare.queue",
    INPUT_PREPARE_EPISODES => "db.input.prepare.episodes",
    INPUT_PREPARE_SCHEDULING => "db.input.prepare.scheduling",
    INPUT_PREPARE_RECORDS => "db.input.prepare.records",
    INPUT_PREPARE_CURRENT => "db.input.prepare.current",
    INPUT_PREPARE_SOURCE_OWNER_CHANGE => "db.input.prepare.source.owner_change",
    INPUT_PREPARE_SOURCE_VALIDATED_RECOVERY => "db.input.prepare.source.validated_recovery",
    INPUT_PREPARE_SOURCE_CONTEXT => "db.input.prepare.source.context",
    INPUT_PREPARE_SOURCE_GENERIC => "db.input.prepare.source.generic",
    INPUT_PREPARE_CLOSURE_ROOTS => "db.input.prepare.closure.roots",
    INPUT_PREPARE_CLOSURE_RECORDS => "db.input.prepare.closure.records",
    INPUT_PREPARE_CLOSURE_EDGES => "db.input.prepare.closure.edges",
    INPUT_PREPARE_CLOSURE_DUP_EDGES => "db.input.prepare.closure.duplicate_edges",
    INPUT_PREPARE_FAMILY_MESSAGE => "db.input.prepare.family.message",
    INPUT_PREPARE_FAMILY_TURN => "db.input.prepare.family.turn",
    INPUT_PREPARE_FAMILY_TOOL_EXECUTION => "db.input.prepare.family.tool_execution",
    INPUT_PREPARE_FAMILY_BRIEF => "db.input.prepare.family.brief",
    INPUT_PREPARE_FAMILY_TRANSCRIPT => "db.input.prepare.family.transcript",
    INPUT_PREPARE_FAMILY_ATTEMPT => "db.input.prepare.family.attempt",
    INPUT_PREPARE_FAMILY_OUTCOME => "db.input.prepare.family.outcome",
    INPUT_PREPARE_FAMILY_WORK_EXECUTION => "db.input.prepare.family.work_execution",
    INPUT_PREPARE_ALIAS_READ => "db.input.prepare.op.alias_read",
    INPUT_PREPARE_FINGERPRINT_READ => "db.input.prepare.op.fingerprint_read",
    INPUT_PREPARE_DECODE => "db.input.prepare.op.decode",
    INPUT_PREPARE_HASH => "db.input.prepare.op.hash",
    INPUT_PREPARE_SIZING => "db.input.prepare.op.sizing",
    INPUT_PREPARE_QUALIFY_REREAD => "db.input.prepare.op.qualification_reread",
    CURRENT_QUERY_PREPARE => "db.current.query.prepare",
    TASK_RESULT_RECOVERY_QUEUE => "db.task_result.recovery.queue_observation",
    QUEUE_INPUT_PREFLIGHT => "db.queue.input.preflight",
    QUEUE_INPUT_FINALIZATION => "db.queue.input.finalization",
    TASK_RESULT_RECOVERY_TASK_PREPARE => "db.task_result.recovery.task_prepare",
    TASK_RESULT_RECOVERY_CONVERGE => "runtime.task_result.recovery.converge",
    TASK_RESULT_RECOVERY_PROBE => "runtime.task_result.recovery.deferred_probe",
    TASK_RESULT_DEFER_NARROW => "db.task_result.defer.narrow",
    TASK_RESULT_DEFER_FALLBACK => "db.task_result.defer.fallback",
    TASK_RESULT_DEFER_PREFLIGHT => "db.task_result.defer.preflight",
    TASK_RESULT_DEFER_RESULT => "db.task_result.defer.result",
    TASK_RESULT_DEFER_FINALIZATION => "db.task_result.defer.finalization",
    TASK_RESULT_DEFER_RECEIPT => "db.task_result.defer.receipt",
    TASK_RESULT_DEFER_PUBLICATION => "runtime.task_result.defer.publication",
    TASK_WRITE_BASELINE_ADOPT => "db.task_write.baseline.adopt",
    TASK_WRITE_BASELINE_COLD => "db.task_write.baseline.cold",
    ROSTER_READ => "projection.roster.read",
    ROSTER_LATEST_BRIEFS => "projection.roster.latest_briefs_query",
    AGENT_PROJECTION_READ => "projection.agent_anchors.read",
    AGENT_LATEST_BRIEF => "projection.agent_anchors.latest_brief_query",
    INDEX_PENDING_COUNT => "index.outbox.pending_count_query",
    INDEX_PENDING_OLDEST => "index.outbox.pending_oldest_query",
    CAPABILITY_PREFLIGHT => "http.projection.capability_preflight",
    PREFLIGHT_ADMISSION_WAIT => "http.projection.preflight_admission_wait",
    PROJECTION_GATE => "http.projection.gate_decision",
    PROJECTION_FOLLOWER => "http.projection.follower_wait",
    PROJECTION_BUILD => "http.projection.leader_build",
    WORK_QUEUE => "projection.work_queue",
    WORK_ITEMS => "projection.work_queue.latest_items",
    WAIT_QUERY => "projection.waits.active_all_query",
    WAIT_QUERY_AGENT => "projection.waits.active_agent_query",
    WAIT_FILTER => "projection.waits.live_scope_filter",
    WAIT_ITEM => "projection.waits.work_item_lookup",
    AGENT_LOCK => "projection.agent_state.lock_wait",
    AGENT_CLONE => "projection.agent_state.clone",
    POSTURE => "projection.state.posture",
    CLOSURE => "projection.state.closure",
    CHILDREN => "projection.state.children",
    CHILD_IDENTITIES => "projection.child.identities",
    CHILD_STATE => "projection.child.state",
    CHILD_ACTIVE_COUNT => "projection.child.active_count",
    CHILD_CLOSURE => "projection.child.closure",
    CHILD_TASK_READ => "projection.child.task_read",
    CHILD_TASK_SORT => "projection.child.task_sort",
    CHILD_BRIEFS => "projection.child.briefs",
    CHILD_ASSEMBLE => "projection.child.assemble",
    IDENTITY => "projection.state.identity",
    ACTIVE_TASKS => "projection.state.active_tasks",
    WORKSPACE_STATE_TOTAL => "workspace.state.total",
    WORKSPACE_STATE_AGENT_LOCK => "workspace.state.agent_lock_wait",
    WORKSPACE_STATE_AGENT_CLONE => "workspace.state.agent_clone",
    WORKSPACE_STATE_ROOT_RECORDS => "workspace.state.root_records",
    WORKSPACE_STATE_WORKSPACE_RECORDS => "workspace.state.workspace_records",
    WORKSPACE_STATE_GIT_LIST => "workspace.state.git_list",
    WORKSPACE_STATE_GIT_STATUS => "workspace.state.git_status",
    WORKSPACE_STATE_OCCUPANCIES => "workspace.state.occupancies",
    WORKSPACE_STATE_SERIALIZE => "workspace.state.serialize",
    WORKSPACE_STATE_RENDER => "workspace.state.render",
}

#[cfg(test)]
mod tests;
