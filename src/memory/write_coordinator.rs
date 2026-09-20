use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use crate::diagnostics;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryIndexWriteClass {
    Foreground,
    Maintenance,
}

impl MemoryIndexWriteClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Maintenance => "maintenance",
        }
    }
}

#[derive(Debug, Default)]
struct WriteCoordinatorState {
    next_ticket: u64,
    active_ticket: Option<u64>,
    foreground_waiters: VecDeque<u64>,
    maintenance_waiters: VecDeque<u64>,
}

#[derive(Debug)]
pub(crate) struct MemoryIndexWriteCoordinator {
    state: Mutex<WriteCoordinatorState>,
    available: Condvar,
    db_path_hash: String,
}

pub(crate) struct MemoryIndexWriteTurn {
    coordinator: Arc<MemoryIndexWriteCoordinator>,
    ticket: u64,
    write_class: MemoryIndexWriteClass,
    operation: &'static str,
    queue_wait_ms: u64,
    acquired_at: Instant,
}

static MEMORY_INDEX_WRITE_COORDINATORS: OnceLock<
    Mutex<BTreeMap<PathBuf, Arc<MemoryIndexWriteCoordinator>>>,
> = OnceLock::new();

pub(crate) fn memory_index_write_coordinator(
    path: &Path,
) -> Result<Arc<MemoryIndexWriteCoordinator>> {
    let key = memory_index_write_coordinator_key(path);
    let coordinators = MEMORY_INDEX_WRITE_COORDINATORS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut coordinators = coordinators
        .lock()
        .map_err(|_| anyhow!("memory index write coordinators mutex poisoned"))?;
    Ok(Arc::clone(coordinators.entry(key.clone()).or_insert_with(
        || {
            Arc::new(MemoryIndexWriteCoordinator {
                state: Mutex::new(WriteCoordinatorState::default()),
                available: Condvar::new(),
                db_path_hash: database_path_hash(&key),
            })
        },
    )))
}

impl MemoryIndexWriteCoordinator {
    pub(crate) fn wait_turn_until(
        self: &Arc<Self>,
        write_class: MemoryIndexWriteClass,
        operation: &'static str,
        deadline: Instant,
    ) -> Result<MemoryIndexWriteTurn> {
        let wait_started_at = Instant::now();
        let wait_budget = deadline.saturating_duration_since(wait_started_at);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?;
        let ticket = state.next_ticket;
        state.next_ticket = state
            .next_ticket
            .checked_add(1)
            .context("memory index write coordinator ticket overflow")?;
        state.waiters_mut(write_class).push_back(ticket);
        diagnostics::record_memory_index_writer_enqueued(
            write_class == MemoryIndexWriteClass::Maintenance,
        );
        let queued_depths = state.queue_depths();
        tracing::debug!(
            db_role = "index",
            db_path_hash = %self.db_path_hash,
            operation,
            write_class = write_class.as_str(),
            ticket,
            queue_depth = queued_depths.total,
            foreground_queue_depth = queued_depths.foreground,
            maintenance_queue_depth = queued_depths.maintenance,
            "queued memory index writer turn"
        );

        loop {
            if state.can_acquire(write_class, ticket) {
                let popped = state.waiters_mut(write_class).pop_front();
                debug_assert_eq!(popped, Some(ticket));
                state.active_ticket = Some(ticket);
                diagnostics::record_memory_index_writer_dequeued(
                    write_class == MemoryIndexWriteClass::Maintenance,
                );
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let removed = state.remove_waiter(write_class, ticket);
                debug_assert!(removed);
                let queue_wait = wait_started_at.elapsed();
                if removed {
                    diagnostics::record_memory_index_writer_timeout(
                        write_class == MemoryIndexWriteClass::Maintenance,
                        operation,
                    );
                }
                let queue_depths = state.queue_depths();
                self.available.notify_all();
                tracing::warn!(
                    db_role = "index",
                    db_path_hash = %self.db_path_hash,
                    operation,
                    write_class = write_class.as_str(),
                    ticket,
                    queue_depth = queue_depths.total,
                    foreground_queue_depth = queue_depths.foreground,
                    maintenance_queue_depth = queue_depths.maintenance,
                    queue_wait_ms = u64::try_from(queue_wait.as_millis()).unwrap_or(u64::MAX),
                    "memory index writer queue wait timed out"
                );
                diagnostics::record_memory_index_writer_queue_wait(operation, queue_wait);
                return Err(anyhow!(
                    "memory index writer queue wait timed out after {} ms",
                    wait_budget.as_millis()
                ));
            }
            let (next_state, _) = self
                .available
                .wait_timeout(state, remaining)
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?;
            state = next_state;
        }
        let queue_wait_ms = elapsed_millis(wait_started_at);
        diagnostics::record_memory_index_writer_queue_wait(operation, wait_started_at.elapsed());
        let queue_depths = state.queue_depths();
        tracing::debug!(
            db_role = "index",
            db_path_hash = %self.db_path_hash,
            operation,
            write_class = write_class.as_str(),
            ticket,
            queue_depth = queue_depths.total,
            foreground_queue_depth = queue_depths.foreground,
            maintenance_queue_depth = queue_depths.maintenance,
            queue_wait_ms,
            "acquired memory index writer turn"
        );
        Ok(MemoryIndexWriteTurn {
            coordinator: Arc::clone(self),
            ticket,
            write_class,
            operation,
            queue_wait_ms,
            acquired_at: Instant::now(),
        })
    }

    pub(super) fn db_path_hash(&self) -> &str {
        &self.db_path_hash
    }
}

impl Drop for MemoryIndexWriteTurn {
    fn drop(&mut self) {
        let writer_turn_ms = elapsed_millis(self.acquired_at);
        let queue_depths = self
            .coordinator
            .state
            .lock()
            .ok()
            .and_then(|mut state| {
                if state.active_ticket == Some(self.ticket) {
                    state.active_ticket = None;
                    let queue_depths = state.queue_depths();
                    self.coordinator.available.notify_all();
                    Some(queue_depths)
                } else {
                    None
                }
            })
            .unwrap_or_default();
        if writer_turn_ms >= 1_000 || self.queue_wait_ms >= 1_000 {
            tracing::warn!(
                db_role = "index",
                db_path_hash = %self.coordinator.db_path_hash,
                operation = self.operation,
                write_class = self.write_class.as_str(),
                ticket = self.ticket,
                queue_depth = queue_depths.total,
                foreground_queue_depth = queue_depths.foreground,
                maintenance_queue_depth = queue_depths.maintenance,
                queue_wait_ms = self.queue_wait_ms,
                writer_turn_ms,
                "slow memory index writer turn"
            );
        } else {
            tracing::debug!(
                db_role = "index",
                db_path_hash = %self.coordinator.db_path_hash,
                operation = self.operation,
                write_class = self.write_class.as_str(),
                ticket = self.ticket,
                queue_depth = queue_depths.total,
                foreground_queue_depth = queue_depths.foreground,
                maintenance_queue_depth = queue_depths.maintenance,
                queue_wait_ms = self.queue_wait_ms,
                writer_turn_ms,
                "released memory index writer turn"
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct QueueDepths {
    total: usize,
    foreground: usize,
    maintenance: usize,
}

impl WriteCoordinatorState {
    fn waiters_mut(&mut self, write_class: MemoryIndexWriteClass) -> &mut VecDeque<u64> {
        match write_class {
            MemoryIndexWriteClass::Foreground => &mut self.foreground_waiters,
            MemoryIndexWriteClass::Maintenance => &mut self.maintenance_waiters,
        }
    }

    fn can_acquire(&self, write_class: MemoryIndexWriteClass, ticket: u64) -> bool {
        if self.active_ticket.is_some() {
            return false;
        }
        match write_class {
            MemoryIndexWriteClass::Foreground => {
                self.foreground_waiters.front().copied() == Some(ticket)
            }
            MemoryIndexWriteClass::Maintenance => {
                self.foreground_waiters.is_empty()
                    && self.maintenance_waiters.front().copied() == Some(ticket)
            }
        }
    }

    fn remove_waiter(&mut self, write_class: MemoryIndexWriteClass, ticket: u64) -> bool {
        let waiters = self.waiters_mut(write_class);
        let Some(position) = waiters.iter().position(|queued| *queued == ticket) else {
            return false;
        };
        waiters.remove(position);
        true
    }

    fn queue_depths(&self) -> QueueDepths {
        QueueDepths {
            total: self.foreground_waiters.len() + self.maintenance_waiters.len(),
            foreground: self.foreground_waiters.len(),
            maintenance: self.maintenance_waiters.len(),
        }
    }
}

fn memory_index_write_coordinator_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => parent
            .canonicalize()
            .map(|parent| parent.join(file_name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn database_path_hash(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn elapsed_millis(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn coordinator_is_shared_by_normalized_database_path() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("memory.v2.sqlite3");
        let equivalent = directory.path().join(".").join("memory.v2.sqlite3");

        let first = memory_index_write_coordinator(&path)?;
        let second = memory_index_write_coordinator(&equivalent)?;

        assert!(Arc::ptr_eq(&first, &second));
        Ok(())
    }

    #[test]
    fn coordinator_serves_waiters_in_ticket_order() -> Result<()> {
        let directory = tempdir()?;
        let coordinator =
            memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
        let first_turn = coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.first",
            Instant::now() + Duration::from_secs(5),
        )?;
        let (sender, receiver) = mpsc::channel();
        let mut handles = Vec::new();

        for value in [1, 2] {
            let waiter = Arc::clone(&coordinator);
            let sender = sender.clone();
            handles.push(thread::spawn(move || -> Result<()> {
                let _turn = waiter.wait_turn_until(
                    MemoryIndexWriteClass::Foreground,
                    "test.waiter",
                    Instant::now() + Duration::from_secs(5),
                )?;
                sender.send(value)?;
                Ok(())
            }));
            let expected_next_ticket = u64::try_from(value + 1)?;
            for _ in 0..100 {
                if coordinator
                    .state
                    .lock()
                    .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                    .next_ticket
                    >= expected_next_ticket
                {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                coordinator
                    .state
                    .lock()
                    .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                    .next_ticket,
                expected_next_ticket
            );
        }
        drop(first_turn);

        assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 1);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 2);
        for handle in handles {
            handle.join().expect("writer waiter thread panicked")?;
        }
        Ok(())
    }

    #[test]
    fn coordinator_prioritizes_foreground_then_resumes_maintenance_fifo() -> Result<()> {
        let directory = tempdir()?;
        let coordinator =
            memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
        let active_turn = coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.active",
            Instant::now() + Duration::from_secs(5),
        )?;
        let (sender, receiver) = mpsc::channel();
        let mut handles = Vec::new();

        for value in [1, 2] {
            let waiter = Arc::clone(&coordinator);
            let sender = sender.clone();
            handles.push(thread::spawn(move || -> Result<()> {
                let _turn = waiter.wait_turn_until(
                    MemoryIndexWriteClass::Maintenance,
                    "test.maintenance",
                    Instant::now() + Duration::from_secs(5),
                )?;
                sender.send(value)?;
                Ok(())
            }));
            let expected_depth = usize::try_from(value)?;
            for _ in 0..100 {
                if coordinator
                    .state
                    .lock()
                    .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                    .maintenance_waiters
                    .len()
                    == expected_depth
                {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                coordinator
                    .state
                    .lock()
                    .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                    .maintenance_waiters
                    .len(),
                expected_depth
            );
        }

        let foreground_waiter = Arc::clone(&coordinator);
        let foreground_sender = sender.clone();
        handles.push(thread::spawn(move || -> Result<()> {
            let _turn = foreground_waiter.wait_turn_until(
                MemoryIndexWriteClass::Foreground,
                "test.foreground",
                Instant::now() + Duration::from_secs(5),
            )?;
            foreground_sender.send(0)?;
            Ok(())
        }));
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .foreground_waiters
                .len()
                == 1
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .queue_depths(),
            QueueDepths {
                total: 3,
                foreground: 1,
                maintenance: 2,
            }
        );

        drop(active_turn);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 0);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 1);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 2);
        for handle in handles {
            handle.join().expect("writer waiter thread panicked")?;
        }
        let state = coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?;
        assert_eq!(state.active_ticket, None);
        assert_eq!(state.queue_depths(), QueueDepths::default());
        Ok(())
    }

    #[test]
    fn coordinator_releases_turn_when_writer_panics() -> Result<()> {
        let directory = tempdir()?;
        let coordinator =
            memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
        let first_turn = coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.first",
            Instant::now() + Duration::from_secs(5),
        )?;

        let panicking_coordinator = Arc::clone(&coordinator);
        let panicking_handle = thread::spawn(move || {
            let _turn = panicking_coordinator
                .wait_turn_until(
                    MemoryIndexWriteClass::Foreground,
                    "test.panicking",
                    Instant::now() + Duration::from_secs(5),
                )
                .expect("panicking writer failed to acquire turn");
            panic!("test writer panic");
        });
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket
                >= 2
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket,
            2
        );

        let successor_coordinator = Arc::clone(&coordinator);
        let (sender, receiver) = mpsc::channel();
        let successor_handle = thread::spawn(move || -> Result<()> {
            let _turn = successor_coordinator.wait_turn_until(
                MemoryIndexWriteClass::Foreground,
                "test.successor",
                Instant::now() + Duration::from_secs(5),
            )?;
            sender.send(())?;
            Ok(())
        });
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket
                >= 3
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket,
            3
        );

        drop(first_turn);
        assert!(panicking_handle.join().is_err());
        receiver.recv_timeout(Duration::from_secs(1))?;
        successor_handle
            .join()
            .expect("successor writer thread panicked")?;
        Ok(())
    }

    #[test]
    fn coordinator_timeout_cancels_ticket_and_releases_successor() -> Result<()> {
        let directory = tempdir()?;
        let coordinator =
            memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
        let first_turn = coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.first",
            Instant::now() + Duration::from_secs(5),
        )?;

        let timed_out_coordinator = Arc::clone(&coordinator);
        let timed_out_handle = thread::spawn(move || {
            match timed_out_coordinator.wait_turn_until(
                MemoryIndexWriteClass::Foreground,
                "test.timeout",
                Instant::now() + Duration::from_millis(50),
            ) {
                Ok(_turn) => panic!("queued writer should time out"),
                Err(error) => error,
            }
        });
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket
                >= 2
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        let successor_coordinator = Arc::clone(&coordinator);
        let (sender, receiver) = mpsc::channel();
        let successor_handle = thread::spawn(move || -> Result<()> {
            let _turn = successor_coordinator.wait_turn_until(
                MemoryIndexWriteClass::Foreground,
                "test.successor",
                Instant::now() + Duration::from_secs(5),
            )?;
            sender.send(())?;
            Ok(())
        });
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket
                >= 3
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        let timeout_error = timed_out_handle
            .join()
            .expect("timed out writer thread panicked");
        assert!(timeout_error.to_string().contains("timed out"));
        assert!(
            receiver.recv_timeout(Duration::from_millis(100)).is_err(),
            "successor writer acquired while the first turn was still active"
        );
        drop(first_turn);

        receiver.recv_timeout(Duration::from_secs(1))?;
        successor_handle
            .join()
            .expect("successor writer thread panicked")?;
        Ok(())
    }

    #[test]
    fn maintenance_timeout_does_not_block_foreground_successor() -> Result<()> {
        let directory = tempdir()?;
        let coordinator =
            memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
        let active_turn = coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.active",
            Instant::now() + Duration::from_secs(5),
        )?;

        let timed_out_coordinator = Arc::clone(&coordinator);
        let timed_out_handle = thread::spawn(move || {
            match timed_out_coordinator.wait_turn_until(
                MemoryIndexWriteClass::Maintenance,
                "test.maintenance_timeout",
                Instant::now() + Duration::from_millis(50),
            ) {
                Ok(_turn) => panic!("queued maintenance writer should time out"),
                Err(error) => error,
            }
        });
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .maintenance_waiters
                .len()
                == 1
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        let successor_coordinator = Arc::clone(&coordinator);
        let (sender, receiver) = mpsc::channel();
        let successor_handle = thread::spawn(move || -> Result<()> {
            let _turn = successor_coordinator.wait_turn_until(
                MemoryIndexWriteClass::Foreground,
                "test.foreground_successor",
                Instant::now() + Duration::from_secs(5),
            )?;
            sender.send(())?;
            Ok(())
        });

        let timeout_error = timed_out_handle
            .join()
            .expect("timed out maintenance writer thread panicked");
        assert!(timeout_error.to_string().contains("timed out"));
        assert!(
            receiver.recv_timeout(Duration::from_millis(100)).is_err(),
            "foreground successor acquired while the active turn was still held"
        );
        drop(active_turn);

        receiver.recv_timeout(Duration::from_secs(1))?;
        successor_handle
            .join()
            .expect("foreground successor thread panicked")?;
        Ok(())
    }
}
