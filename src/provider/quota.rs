use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use tokio::time::{sleep, Duration, Instant};

use super::ProviderQuotaIdentity;

pub(crate) const PROVIDER_QUOTA_ADMISSION_MAX_WAIT: Duration = Duration::from_secs(30);
const PROVIDER_QUOTA_COOLDOWN_CAP: Duration = Duration::from_secs(30);
const PROVIDER_QUOTA_DEFAULT_COOLDOWN: Duration = Duration::from_millis(250);
const PROVIDER_QUOTA_WAIT_POLL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub(crate) struct ProviderQuotaCoordinator {
    state: Arc<CoordinatorState>,
}

struct CoordinatorState {
    accounts: Mutex<HashMap<ProviderQuotaIdentity, Arc<AccountState>>>,
}

struct AccountState {
    gate: Mutex<AccountGate>,
    notify: tokio::sync::Notify,
}

struct AccountGate {
    in_flight: bool,
    cooldown_until: Instant,
}

pub(crate) struct ProviderQuotaPermit {
    account: Arc<AccountState>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderQuotaAdmissionTimeout {
    pub waited: Duration,
}

impl ProviderQuotaCoordinator {
    pub(crate) fn global() -> Self {
        static GLOBAL: OnceLock<ProviderQuotaCoordinator> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Self {
                state: Arc::new(CoordinatorState {
                    accounts: Mutex::new(HashMap::new()),
                }),
            })
            .clone()
    }

    #[cfg(test)]
    fn new_for_test() -> Self {
        Self {
            state: Arc::new(CoordinatorState {
                accounts: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) async fn acquire(
        &self,
        identity: ProviderQuotaIdentity,
    ) -> Result<ProviderQuotaPermit, ProviderQuotaAdmissionTimeout> {
        let account = {
            let mut accounts = self.state.accounts.lock().expect("quota state poisoned");
            accounts
                .entry(identity)
                .or_insert_with(|| {
                    Arc::new(AccountState {
                        gate: Mutex::new(AccountGate {
                            in_flight: false,
                            cooldown_until: Instant::now(),
                        }),
                        notify: tokio::sync::Notify::new(),
                    })
                })
                .clone()
        };
        let started = Instant::now();
        let deadline = started + PROVIDER_QUOTA_ADMISSION_MAX_WAIT;

        loop {
            let now = Instant::now();
            let notified = account.notify.notified();
            let wait_for = {
                let mut gate = account.gate.lock().expect("quota account state poisoned");
                if !gate.in_flight && gate.cooldown_until <= now {
                    gate.in_flight = true;
                    return Ok(ProviderQuotaPermit {
                        account: account.clone(),
                    });
                }

                gate.cooldown_until
                    .saturating_duration_since(now)
                    .min(PROVIDER_QUOTA_WAIT_POLL)
                    .max(PROVIDER_QUOTA_WAIT_POLL)
            };
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                return Err(ProviderQuotaAdmissionTimeout {
                    waited: now.saturating_duration_since(started),
                });
            }

            tokio::select! {
                _ = notified => {}
                _ = sleep(wait_for.min(remaining)) => {}
            }
        }
    }
}

impl ProviderQuotaPermit {
    pub(crate) fn record_rate_limit(&self, retry_after: Option<Duration>) {
        let cooldown = retry_after
            .unwrap_or(PROVIDER_QUOTA_DEFAULT_COOLDOWN)
            .min(PROVIDER_QUOTA_COOLDOWN_CAP);
        let mut gate = self
            .account
            .gate
            .lock()
            .expect("quota account state poisoned");
        gate.cooldown_until = gate.cooldown_until.max(Instant::now() + cooldown);
        self.account.notify.notify_waiters();
    }
}

impl Drop for ProviderQuotaPermit {
    fn drop(&mut self) {
        let mut gate = self
            .account
            .gate
            .lock()
            .expect("quota account state poisoned");
        gate.in_flight = false;
        self.account.notify.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderQuotaCoordinator, ProviderQuotaIdentity, PROVIDER_QUOTA_ADMISSION_MAX_WAIT,
    };
    use tokio::time::{advance, Duration};

    fn identity(value: &str) -> ProviderQuotaIdentity {
        ProviderQuotaIdentity::coarse("test-account", value)
    }

    #[tokio::test(start_paused = true)]
    async fn same_identity_is_single_flight_and_different_identity_is_independent() {
        let coordinator = ProviderQuotaCoordinator::new_for_test();
        let first = coordinator
            .acquire(identity("same"))
            .await
            .expect("first permit");
        let second = coordinator.clone();
        let waiting = tokio::spawn(async move { second.acquire(identity("same")).await });
        let other = coordinator
            .acquire(identity("other"))
            .await
            .expect("different identity should not wait");

        advance(Duration::from_millis(10)).await;
        assert!(!waiting.is_finished());
        drop(other);
        drop(first);
        let permit = waiting
            .await
            .expect("waiter task")
            .expect("released permit");
        drop(permit);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_cooldown_propagates_and_admission_wait_is_bounded() {
        let coordinator = ProviderQuotaCoordinator::new_for_test();
        let first = coordinator
            .acquire(identity("same"))
            .await
            .expect("first permit");
        first.record_rate_limit(Some(Duration::from_secs(10)));
        drop(first);

        let second = coordinator.clone();
        let waiting = tokio::spawn(async move { second.acquire(identity("same")).await });
        advance(Duration::from_secs(9)).await;
        assert!(!waiting.is_finished());
        advance(Duration::from_secs(1)).await;
        let permit = waiting
            .await
            .expect("waiter task")
            .expect("cooldown should expire");
        drop(permit);

        let held = coordinator
            .acquire(identity("same"))
            .await
            .expect("held permit");
        let third = coordinator.clone();
        let blocked = tokio::spawn(async move { third.acquire(identity("same")).await });
        tokio::task::yield_now().await;
        advance(PROVIDER_QUOTA_ADMISSION_MAX_WAIT).await;
        tokio::task::yield_now().await;
        assert!(blocked.is_finished());
        assert!(blocked.await.expect("blocked task").is_err());
        drop(held);
    }
}
