use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use tokio::{runtime::Handle, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::{is_listed, AgentListProjectionSource, MutationOwnerSlot, PreparedAgentList};
use crate::runtime_db::RuntimeDbReadScope;

const FIRST_RETRY: Duration = Duration::from_millis(250);
const MAX_RETRY: Duration = Duration::from_secs(15);
const READ_BUDGET: Duration = Duration::from_secs(1);

type Pending = BTreeMap<(String, u64), Weak<MutationOwnerSlot>>;

#[derive(Debug, Default)]
struct RepairQueue {
    pending: Pending,
    worker: Option<JoinHandle<()>>,
}

/// A Host owns one worker while there are list publications to repair.
#[derive(Debug)]
pub(super) struct AgentListPublisher {
    pub(super) source: AgentListProjectionSource,
    queue: Mutex<RepairQueue>,
    closing: CancellationToken,
}

impl Drop for AgentListPublisher {
    fn drop(&mut self) {
        self.closing.cancel();
    }
}

impl AgentListPublisher {
    pub(super) fn new(source: AgentListProjectionSource) -> Self {
        Self {
            source,
            queue: Mutex::new(RepairQueue::default()),
            closing: CancellationToken::new(),
        }
    }

    pub(super) fn schedule(
        self: &Arc<Self>,
        agent_id: &str,
        incarnation: u64,
        slot: &Arc<MutationOwnerSlot>,
    ) {
        let mut queue = self.queue.lock().expect("Agent list repair queue poisoned");
        if self.closing.is_cancelled() {
            return;
        }
        queue
            .pending
            .insert((agent_id.to_owned(), incarnation), Arc::downgrade(slot));
        self.start(&mut queue);
    }

    pub(super) fn start_pending(self: &Arc<Self>) {
        let mut queue = self.queue.lock().expect("Agent list repair queue poisoned");
        self.start(&mut queue);
    }

    fn start(self: &Arc<Self>, queue: &mut RepairQueue) {
        if queue.pending.is_empty()
            || self.closing.is_cancelled()
            || queue
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
        {
            return;
        }
        if let Ok(runtime) = Handle::try_current() {
            queue.worker = Some(runtime.spawn(Self::repair_agent_lists(
                Arc::downgrade(self),
                self.closing.clone(),
            )));
        }
    }

    pub(super) async fn shutdown(&self) {
        self.closing.cancel();
        let worker = {
            let mut queue = self.queue.lock().expect("Agent list repair queue poisoned");
            queue.pending.clear();
            queue.worker.take()
        };
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }

    async fn repair_agent_lists(publisher: Weak<Self>, closing: CancellationToken) {
        let mut delay = FIRST_RETRY;
        loop {
            tokio::select! {
                biased;
                _ = closing.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
            let Some(current) = publisher.upgrade() else {
                return;
            };
            let pending = {
                let mut queue = current
                    .queue
                    .lock()
                    .expect("Agent list repair queue poisoned");
                if queue.pending.is_empty() {
                    queue.worker = None;
                    return;
                }
                std::mem::take(&mut queue.pending)
            };
            let count = pending.len();
            let mut retries = Pending::new();
            for (key, slot) in pending {
                if closing.is_cancelled() {
                    return;
                }
                let Some(target) = slot.upgrade() else {
                    continue;
                };
                let current = current.clone();
                let (agent_id, incarnation) = key.clone();
                let retry = tokio::task::spawn_blocking(move || {
                    current.needs_retry(&agent_id, incarnation, &target)
                })
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(%error, "Agent list repair worker failed");
                    true
                });
                if retry {
                    retries.insert(key, slot);
                }
            }
            let mut queue = current
                .queue
                .lock()
                .expect("Agent list repair queue poisoned");
            if closing.is_cancelled() {
                return;
            }
            delay = if retries.len() < count {
                FIRST_RETRY
            } else {
                delay.saturating_mul(2).min(MAX_RETRY)
            };
            for (key, slot) in retries {
                queue.pending.entry(key).or_insert(slot);
            }
            if queue.pending.is_empty() {
                queue.worker = None;
                return;
            }
        }
    }

    fn needs_retry(&self, agent_id: &str, incarnation: u64, slot: &MutationOwnerSlot) -> bool {
        let identity = match self.source.identity(agent_id) {
            Ok(Some(identity)) if identity.incarnation == incarnation && is_listed(&identity) => {
                identity
            }
            Ok(_) => return false,
            Err(_) => return true,
        };
        let (epoch, generation) = {
            let publication = slot.publication.lock().expect("publication poisoned");
            let PreparedAgentList::Repair(revision) = publication.roster else {
                return false;
            };
            if publication.recovery.is_some() || revision != identity.revision {
                return true;
            }
            (publication.epoch, publication.generation)
        };
        let db = self
            .source
            .db
            .with_read_scope(RuntimeDbReadScope::new(Instant::now() + READ_BUDGET));
        // Keep background repair out of the pool used by mutation preparation.
        let Ok(entry) = db.with_read(|connection| {
            let tx = connection.transaction()?;
            let entry = self.source.read(&tx, &identity, None)?;
            tx.commit()?;
            Ok(entry)
        }) else {
            return true;
        };
        if self.closing.is_cancelled() {
            return false;
        }
        if db.check_read_scope().is_err() {
            return true;
        }
        let mut publication = slot.publication.lock().expect("publication poisoned");
        if publication.epoch == epoch
            && publication.generation == generation
            && publication.recovery.is_none()
            && matches!(publication.roster, PreparedAgentList::Repair(revision) if revision == identity.revision)
            && self.source.matches_identity(&identity)
        {
            publication.roster = PreparedAgentList::Ready(Arc::new(entry));
        }
        matches!(publication.roster, PreparedAgentList::Repair(_))
    }
}
