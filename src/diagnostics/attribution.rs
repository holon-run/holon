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
    CONNECTION => "db.connection.attempt",
    SIDECAR => "db.connection.sidecar_check",
    SQLITE_OPEN => "db.connection.sqlite_open",
    CONFIGURE => "db.connection.configure",
    WORK_QUEUE => "projection.work_queue",
    WORK_ITEMS => "projection.work_queue.latest_items",
    WAIT_QUERY => "projection.waits.active_all_query",
    WAIT_FILTER => "projection.waits.live_scope_filter",
    WAIT_ITEM => "projection.waits.work_item_lookup",
    AGENT_LOCK => "projection.agent_state.lock_wait",
    AGENT_CLONE => "projection.agent_state.clone",
    POSTURE => "projection.state.posture",
    CLOSURE => "projection.state.closure",
    CHILDREN => "projection.state.children",
    IDENTITY => "projection.state.identity",
    ACTIVE_TASKS => "projection.state.active_tasks",
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
