use super::*;
use crate::avatar::test_support::benchmark_resources;
use serde_json::json;
use std::time::Duration;

fn key(index: usize) -> Key {
    Key {
        recipe: index,
        ..Key::parse("fixture", "format=svg").unwrap()
    }
}

async fn until(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn held_build(
    resources: Arc<AvatarResources>,
    key: Key,
) -> (
    tokio::task::JoinHandle<BuildResult>,
    std::sync::mpsc::Sender<()>,
) {
    let (release, blocked) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        resources
            .get_with(key, move || {
                let _ = started.send(());
                blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(vec![1, 2, 3])
            })
            .await
    });
    ready.await.unwrap();
    (task, release)
}

#[test]
fn cache_enforces_both_limits_and_lru() {
    let mut cache = Cache::default();
    for index in 0..MAX_CACHE_ENTRIES {
        cache.insert(key(index), Arc::new(Image::new(vec![1]).unwrap()));
    }
    cache.cached(&key(0)).unwrap();
    cache.insert(
        key(MAX_CACHE_ENTRIES),
        Arc::new(Image::new(vec![2]).unwrap()),
    );
    assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
    assert!(cache.entries.contains_key(&key(0)));
    assert!(!cache.entries.contains_key(&key(1)));
    let image = Arc::new(Image::new(vec![0; super::super::MAX_IMAGE_BYTES]).unwrap());
    for index in 0..20 {
        cache.insert(key(index + 600), image.clone());
    }
    assert_eq!(cache.bytes, MAX_CACHE_BYTES);
    assert_eq!(cache.entries.len(), 8);
    assert!(Image::new(vec![0; super::super::MAX_IMAGE_BYTES + 1]).is_err());
}

#[tokio::test]
async fn single_flight_counts_followers_and_hot_hits_skip_admission() {
    let resources = Arc::new(AvatarResources::default());
    let (leader, release) = held_build(resources.clone(), key(0)).await;
    let mut followers = Vec::new();
    for _ in 0..MAX_WAITERS {
        let resources = resources.clone();
        followers.push(tokio::spawn(async move {
            resources
                .get_with(key(0), || panic!("duplicate build"))
                .await
        }));
    }
    until(|| resources.waiters.available_permits() == 0).await;
    assert!(matches!(
        resources.get(key(0)).await,
        Err(BuildError::Overloaded)
    ));
    release.send(()).unwrap();
    let first = leader.await.unwrap().unwrap();
    for task in followers {
        assert!(Arc::ptr_eq(&first, &task.await.unwrap().unwrap()));
    }
    let builders = resources
        .builders
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let waiters = resources
        .waiters
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first, &resources.get(key(0)).await.unwrap()));
    drop((builders, waiters));
}

#[tokio::test]
async fn distinct_keys_bound_builders_and_queued_leaders() {
    let resources = Arc::new(AvatarResources::default());
    let (first, release_first) = held_build(resources.clone(), key(0)).await;
    let (second, release_second) = held_build(resources.clone(), key(1)).await;
    let mut queued = Vec::new();
    for index in 2..18 {
        let resources = resources.clone();
        queued.push(tokio::spawn(async move {
            resources.get_with(key(index), || Ok(vec![9])).await
        }));
    }
    until(|| resources.waiters.available_permits() == 0).await;
    assert_eq!(resources.builders.available_permits(), 0);
    assert!(matches!(
        resources.get(key(18)).await,
        Err(BuildError::Overloaded)
    ));
    for task in queued {
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
    assert_eq!(resources.waiters.available_permits(), MAX_WAITERS);
    assert_eq!(resources.cache.lock().unwrap().inflight.len(), 2);
    release_first.send(()).unwrap();
    release_second.send(()).unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(resources.builders.available_permits(), MAX_BUILDERS);
}

#[tokio::test]
async fn cancellation_wakes_followers_without_releasing_a_running_raster() {
    let resources = Arc::new(AvatarResources::default());
    let (leader, release) = held_build(resources.clone(), key(0)).await;
    let follower = {
        let resources = resources.clone();
        tokio::spawn(async move { resources.get(key(0)).await })
    };
    until(|| resources.waiters.available_permits() == MAX_WAITERS - 1).await;
    leader.abort();
    assert!(leader.await.unwrap_err().is_cancelled());
    assert!(matches!(
        follower.await.unwrap(),
        Err(BuildError::Cancelled)
    ));
    assert!(resources.cache.lock().unwrap().inflight.is_empty());
    assert_eq!(resources.waiters.available_permits(), MAX_WAITERS);
    assert_eq!(resources.builders.available_permits(), MAX_BUILDERS - 1);
    release.send(()).unwrap();
    until(|| resources.builders.available_permits() == MAX_BUILDERS).await;
    resources.get_with(key(0), || Ok(vec![9])).await.unwrap();
}

#[tokio::test]
async fn failures_and_panics_do_not_poison_cache_or_leases() {
    let resources = AvatarResources::default();
    assert!(resources
        .get_with(key(0), || Err(BuildError::Failed))
        .await
        .is_err());
    assert!(resources
        .get_with(key(0), || panic!("synthetic raster failure"))
        .await
        .is_err());
    assert!(resources.cache.lock().unwrap().entries.is_empty());
    assert!(resources.cache.lock().unwrap().inflight.is_empty());
    assert_eq!(resources.builders.available_permits(), MAX_BUILDERS);
    assert_eq!(resources.waiters.available_permits(), MAX_WAITERS);
    resources.get_with(key(0), || Ok(vec![1])).await.unwrap();
}

#[tokio::test]
#[ignore = "offline release measurement, not a timing assertion"]
async fn avatar_cache_churn_benchmark() {
    use std::time::Instant;

    for group in 0..3 {
        let resources = AvatarResources::default();
        for phase in ["cold", "evicted", "hot"] {
            // Cycling all recipes exceeds the 512-entry cap. The last 512
            // remain resident, so the final phase isolates cache-hit overhead.
            let indices = if phase == "hot" { 640..1152 } else { 0..1152 };
            let mut samples = Vec::new();
            let mut bytes = 0;
            let cpu_before = benchmark_resources().0;
            for recipe in indices {
                let key = Key {
                    recipe,
                    ..Key::parse("avatar-http-fixture", "format=svg").unwrap()
                };
                let start = Instant::now();
                let image = resources.get(key).await.unwrap();
                samples.push(start.elapsed().as_nanos() as u64);
                bytes += image.bytes.len();
            }
            let (cpu_us, rss_bytes) = benchmark_resources();
            samples.sort_unstable();
            println!(
                "{}",
                json!({
                    "benchmark":"avatar_cache_churn","group":group,"phase":phase,
                    "count":samples.len(),"p50_us":samples[samples.len()/2] as f64/1000.0,
                    "p95_us":samples[samples.len()*95/100] as f64/1000.0,
                    "process_cpu_us":cpu_us.saturating_sub(cpu_before),
                    "process_peak_rss_bytes":rss_bytes,"bytes":bytes
                })
            );
        }
    }
}
