use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

const MEMORY_INDEX_WRITE_QUEUE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct WriteCoordinatorState {
    next_ticket: u64,
    serving_ticket: u64,
    cancelled_tickets: BTreeSet<u64>,
}

#[derive(Debug)]
pub(super) struct MemoryIndexWriteCoordinator {
    state: Mutex<WriteCoordinatorState>,
    available: Condvar,
    db_path_hash: String,
}

pub(super) struct MemoryIndexWriteTurn {
    coordinator: Arc<MemoryIndexWriteCoordinator>,
    operation: &'static str,
    queue_wait_ms: u64,
    acquired_at: Instant,
}

static MEMORY_INDEX_WRITE_COORDINATORS: OnceLock<
    Mutex<BTreeMap<PathBuf, Arc<MemoryIndexWriteCoordinator>>>,
> = OnceLock::new();

pub(super) fn memory_index_write_coordinator(
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
    pub(super) fn wait_turn(
        self: &Arc<Self>,
        operation: &'static str,
    ) -> Result<MemoryIndexWriteTurn> {
        self.wait_turn_for(operation, MEMORY_INDEX_WRITE_QUEUE_TIMEOUT)
    }

    fn wait_turn_for(
        self: &Arc<Self>,
        operation: &'static str,
        timeout: Duration,
    ) -> Result<MemoryIndexWriteTurn> {
        let wait_started_at = Instant::now();
        let ticket = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?;
            let ticket = state.next_ticket;
            state.next_ticket = state
                .next_ticket
                .checked_add(1)
                .context("memory index write coordinator ticket overflow")?;
            ticket
        };

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?;
        while state.serving_ticket != ticket {
            let remaining = timeout.saturating_sub(wait_started_at.elapsed());
            if remaining.is_zero() {
                state.cancelled_tickets.insert(ticket);
                advance_serving_ticket(&mut state);
                self.available.notify_all();
                tracing::warn!(
                    db_role = "index",
                    db_path_hash = %self.db_path_hash,
                    operation,
                    ticket,
                    queue_wait_ms = elapsed_millis(wait_started_at),
                    "memory index writer queue wait timed out"
                );
                return Err(anyhow!(
                    "memory index writer queue wait timed out after {} ms",
                    timeout.as_millis()
                ));
            }
            let (next_state, _) = self
                .available
                .wait_timeout(state, remaining)
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?;
            state = next_state;
        }
        let queue_wait_ms = elapsed_millis(wait_started_at);
        tracing::debug!(
            db_role = "index",
            db_path_hash = %self.db_path_hash,
            operation,
            ticket,
            queue_wait_ms,
            "acquired memory index writer turn"
        );
        Ok(MemoryIndexWriteTurn {
            coordinator: Arc::clone(self),
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
        if writer_turn_ms >= 1_000 || self.queue_wait_ms >= 1_000 {
            tracing::warn!(
                db_role = "index",
                db_path_hash = %self.coordinator.db_path_hash,
                operation = self.operation,
                queue_wait_ms = self.queue_wait_ms,
                writer_turn_ms,
                "slow memory index writer turn"
            );
        } else {
            tracing::debug!(
                db_role = "index",
                db_path_hash = %self.coordinator.db_path_hash,
                operation = self.operation,
                queue_wait_ms = self.queue_wait_ms,
                writer_turn_ms,
                "released memory index writer turn"
            );
        }
        if let Ok(mut state) = self.coordinator.state.lock() {
            advance_serving_ticket(&mut state);
            self.coordinator.available.notify_all();
        }
    }
}

fn advance_serving_ticket(state: &mut WriteCoordinatorState) {
    state.serving_ticket = state.serving_ticket.saturating_add(1);
    while state.cancelled_tickets.remove(&state.serving_ticket) {
        state.serving_ticket = state.serving_ticket.saturating_add(1);
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
        let first_turn = coordinator.wait_turn("test.first")?;
        let (sender, receiver) = mpsc::channel();
        let mut handles = Vec::new();

        for value in [1, 2] {
            let waiter = Arc::clone(&coordinator);
            let sender = sender.clone();
            handles.push(thread::spawn(move || -> Result<()> {
                let _turn = waiter.wait_turn("test.waiter")?;
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
    fn coordinator_releases_turn_when_writer_panics() -> Result<()> {
        let directory = tempdir()?;
        let coordinator =
            memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
        let first_turn = coordinator.wait_turn("test.first")?;

        let panicking_coordinator = Arc::clone(&coordinator);
        let panicking_handle = thread::spawn(move || {
            let _turn = panicking_coordinator
                .wait_turn("test.panicking")
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
            let _turn = successor_coordinator.wait_turn("test.successor")?;
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
        let first_turn = coordinator.wait_turn("test.first")?;

        let timed_out_coordinator = Arc::clone(&coordinator);
        let timed_out_handle = thread::spawn(move || {
            match timed_out_coordinator.wait_turn_for("test.timeout", Duration::from_millis(50)) {
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
            let _turn = successor_coordinator.wait_turn("test.successor")?;
            sender.send(())?;
            Ok(())
        });

        let timeout_error = timed_out_handle
            .join()
            .expect("timed out writer thread panicked");
        assert!(timeout_error.to_string().contains("timed out"));
        drop(first_turn);

        receiver.recv_timeout(Duration::from_secs(1))?;
        successor_handle
            .join()
            .expect("successor writer thread panicked")?;
        Ok(())
    }
}
