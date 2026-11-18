use std::{cell::RefCell, future::Future, mem};

#[derive(Clone, Copy)]
pub(crate) enum LoadLane {
    Writer,
    PreparationReader,
}

#[derive(Debug, Default)]
pub(crate) struct TaskLoadCounts {
    pub(crate) writer_attempts: usize,
    pub(crate) borrowed_roots: usize,
    pub(crate) writer_loads: usize,
    pub(crate) reader_loads: usize,
}

#[derive(Default)]
struct Probe {
    active: ActiveLanes,
    counts: TaskLoadCounts,
}

#[derive(Clone, Copy, Default)]
struct ActiveLanes {
    writer: bool,
    reader: bool,
}

tokio::task_local! {
    static PROBE: RefCell<Probe>;
}

pub(crate) async fn observe<T>(operation: impl Future<Output = T>) -> (T, TaskLoadCounts) {
    PROBE
        .scope(RefCell::default(), async {
            let result = operation.await;
            let counts = PROBE.with(|probe| mem::take(&mut probe.borrow_mut().counts));
            (result, counts)
        })
        .await
}

// Keep enclosing lanes visible so a nested reader cannot mask a writer load.
pub(crate) struct LoadLaneGuard(ActiveLanes);

impl Drop for LoadLaneGuard {
    fn drop(&mut self) {
        let _ = PROBE.try_with(|probe| probe.borrow_mut().active = self.0);
    }
}

pub(crate) fn enter(lane: LoadLane) -> LoadLaneGuard {
    let previous = PROBE
        .try_with(|probe| {
            let mut probe = probe.borrow_mut();
            let previous = probe.active;
            match lane {
                LoadLane::Writer => {
                    probe.counts.writer_attempts += 1;
                    probe.active.writer = true;
                }
                LoadLane::PreparationReader => probe.active.reader = true,
            }
            previous
        })
        .unwrap_or_default();
    LoadLaneGuard(previous)
}

pub(crate) fn record_borrowed_root() {
    let _ = PROBE.try_with(|probe| probe.borrow_mut().counts.borrowed_roots += 1);
}

pub(super) fn record_load() {
    let _ = PROBE.try_with(|probe| {
        let mut probe = probe.borrow_mut();
        if probe.active.writer {
            probe.counts.writer_loads += 1;
        }
        if probe.active.reader {
            probe.counts.reader_loads += 1;
        }
    });
}
