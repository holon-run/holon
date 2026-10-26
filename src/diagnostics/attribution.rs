//! Bounded, content-free read-side timings. Nested stages overlap; timers count
//! attempts including errors/cancellation. Snapshots are not transactional.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) struct Stage {
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

    pub(crate) fn start(&'static self) -> Timer {
        Timer {
            stage: self,
            started: Instant::now(),
            rows: 0,
        }
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

pub(crate) struct Timer {
    stage: &'static Stage,
    started: Instant,
    rows: u64,
}

impl Timer {
    pub(crate) fn rows(&mut self, rows: usize) {
        self.rows = rows as u64;
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let elapsed_ns = self.started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.stage.total_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.stage.max_ns.fetch_max(elapsed_ns, Ordering::Relaxed);
        self.stage.rows.fetch_add(self.rows, Ordering::Relaxed);
        self.stage.count.fetch_add(1, Ordering::Relaxed);
        tracing::trace!(
            target: "holon::performance",
            stage = self.stage.name, elapsed_ns, rows = self.rows,
            "read-side stage completed"
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StageSnapshot {
    pub name: String,
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub rows: u64,
}

macro_rules! stages {
    ($($id:ident => $name:literal),+ $(,)?) => {
        $(pub(crate) static $id: Stage = Stage::new($name);)+
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
    ROSTER_READ => "projection.roster.read",
    ROSTER_LATEST_BRIEFS => "projection.roster.latest_briefs_query",
    AGENT_PROJECTION_READ => "projection.agent_anchors.read",
    AGENT_LATEST_BRIEF => "projection.agent_anchors.latest_brief_query",
    INDEX_PENDING_COUNT => "index.outbox.pending_count_query",
    INDEX_PENDING_OLDEST => "index.outbox.pending_oldest_query",
    CAPABILITY_PREFLIGHT => "http.projection.capability_preflight",
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
mod tests {
    use super::*;

    #[test]
    fn workspace_state_stages_are_fixed_and_unique() {
        let stages = snapshot()
            .into_iter()
            .filter(|stage| stage.name.starts_with("workspace.state."))
            .map(|stage| stage.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            stages,
            [
                "workspace.state.total",
                "workspace.state.agent_lock_wait",
                "workspace.state.agent_clone",
                "workspace.state.root_records",
                "workspace.state.workspace_records",
                "workspace.state.git_list",
                "workspace.state.git_status",
                "workspace.state.occupancies",
                "workspace.state.serialize",
                "workspace.state.render",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        assert_eq!(
            snapshot()
                .iter()
                .filter(|stage| stage.name.starts_with("workspace.state."))
                .count(),
            stages.len()
        );
    }

    #[test]
    fn timer_records_attempt_rows_and_submillisecond_precision() {
        static STAGE: Stage = Stage::new("test");
        let mut timer = STAGE.start();
        timer.rows(7);
        timer.started = Instant::now() - std::time::Duration::from_micros(100);
        drop(timer);
        let snapshot = STAGE.snapshot();
        assert_eq!(snapshot.count, 1);
        assert_eq!(snapshot.rows, 7);
        assert!(snapshot.total_ns >= 100_000);
        assert_eq!(snapshot.total_ns, snapshot.max_ns);
    }

    #[test]
    fn timer_records_failed_attempt_once() {
        static STAGE: Stage = Stage::new("test.failed");
        let attempt = || -> anyhow::Result<()> {
            let _timer = STAGE.start();
            anyhow::bail!("synthetic failure")
        };
        assert!(attempt().is_err());
        assert_eq!(STAGE.snapshot().count, 1);
    }

    #[tokio::test]
    async fn timer_records_cancelled_attempt_once() {
        static STAGE: Stage = Stage::new("test.cancelled");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _timer = STAGE.start();
            started_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(STAGE.snapshot().count, 1);
    }
}
