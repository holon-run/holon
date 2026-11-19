use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use tokio::time::{sleep, Duration, Instant};

use super::ProviderQuotaIdentity;

const PROVIDER_QUOTA_COOLDOWN_CAP: Duration = Duration::from_secs(30);
const PROVIDER_QUOTA_DEFAULT_COOLDOWN: Duration = Duration::from_millis(250);
const PROVIDER_QUOTA_WAIT_POLL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub struct ProviderQuotaCoordinator {
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

pub struct ProviderQuotaPermit {
    account: Arc<AccountState>,
}

impl ProviderQuotaCoordinator {
    pub fn global() -> Self {
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

    fn account_for(&self, identity: ProviderQuotaIdentity) -> Arc<AccountState> {
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
    }

    pub async fn acquire(&self, identity: ProviderQuotaIdentity) -> ProviderQuotaPermit {
        let account = self.account_for(identity);
        loop {
            let now = Instant::now();
            let notified = account.notify.notified();
            let wait_for = {
                let mut gate = account.gate.lock().expect("quota account state poisoned");
                if !gate.in_flight && gate.cooldown_until <= now {
                    gate.in_flight = true;
                    return ProviderQuotaPermit {
                        account: account.clone(),
                    };
                }

                gate.cooldown_until
                    .saturating_duration_since(now)
                    .min(PROVIDER_QUOTA_WAIT_POLL)
                    .max(PROVIDER_QUOTA_WAIT_POLL)
            };
            tokio::select! {
                _ = notified => {}
                _ = sleep(wait_for) => {}
            }
        }
    }

    pub fn record_rate_limit(
        &self,
        identity: ProviderQuotaIdentity,
        retry_after: Option<Duration>,
    ) {
        let account = self.account_for(identity);
        set_cooldown(&account, retry_after);
    }
}

impl ProviderQuotaPermit {
    pub fn record_rate_limit(&self, retry_after: Option<Duration>) {
        set_cooldown(&self.account, retry_after);
    }
}

fn set_cooldown(account: &AccountState, retry_after: Option<Duration>) {
    let cooldown = retry_after
        .unwrap_or(PROVIDER_QUOTA_DEFAULT_COOLDOWN)
        .min(PROVIDER_QUOTA_COOLDOWN_CAP);
    let mut gate = account.gate.lock().expect("quota account state poisoned");
    gate.cooldown_until = gate.cooldown_until.max(Instant::now() + cooldown);
    account.notify.notify_waiters();
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
mod tests;
