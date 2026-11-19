use super::{ProviderQuotaCoordinator, ProviderQuotaIdentity};
use tokio::time::{advance, Duration};

fn identity(value: &str) -> ProviderQuotaIdentity {
    ProviderQuotaIdentity::coarse("test-account", value)
}

#[tokio::test(start_paused = true)]
async fn same_identity_is_single_flight_and_different_identity_is_independent() {
    let coordinator = ProviderQuotaCoordinator::new_for_test();
    let first = coordinator.acquire(identity("same")).await;
    let second = coordinator.clone();
    let waiting = tokio::spawn(async move { second.acquire(identity("same")).await });
    let other = coordinator.acquire(identity("other")).await;

    advance(Duration::from_millis(10)).await;
    assert!(!waiting.is_finished());
    drop(other);
    drop(first);
    let permit = waiting.await.expect("waiter task");
    drop(permit);
}

#[tokio::test(start_paused = true)]
async fn rate_limit_cooldown_propagates_without_failing_waiters() {
    let coordinator = ProviderQuotaCoordinator::new_for_test();
    let first = coordinator.acquire(identity("same")).await;
    first.record_rate_limit(Some(Duration::from_secs(10)));
    drop(first);

    let second = coordinator.clone();
    let waiting = tokio::spawn(async move { second.acquire(identity("same")).await });
    advance(Duration::from_secs(9)).await;
    assert!(!waiting.is_finished());
    advance(Duration::from_secs(1)).await;
    let permit = waiting.await.expect("waiter task");
    drop(permit);

    let held = coordinator.acquire(identity("same")).await;
    let third = coordinator.clone();
    let blocked = tokio::spawn(async move { third.acquire(identity("same")).await });
    tokio::task::yield_now().await;
    advance(Duration::from_secs(1)).await;
    assert!(!blocked.is_finished());
    drop(held);
    blocked.await.expect("blocked task");
}
