use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

use super::{artwork, Format, Image, Key};

const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 512;
const MAX_BUILDERS: usize = 2;
const MAX_WAITERS: usize = 16;

#[derive(Debug, Clone, Copy)]
pub(super) enum BuildError {
    Overloaded,
    Cancelled,
    Failed,
}

type BuildResult = Result<Arc<Image>, BuildError>;
type Subscription = watch::Receiver<Option<BuildResult>>;

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Arc<Image>>,
    lru: VecDeque<Key>,
    bytes: usize,
    inflight: HashMap<Key, Subscription>,
}

impl Cache {
    fn cached(&mut self, key: &Key) -> Option<Arc<Image>> {
        let image = self.entries.get(key)?.clone();
        self.lru.retain(|entry| entry != key);
        self.lru.push_back(*key);
        Some(image)
    }

    fn insert(&mut self, key: Key, image: Arc<Image>) {
        while self.bytes + image.bytes.len() > MAX_CACHE_BYTES
            || self.entries.len() >= MAX_CACHE_ENTRIES
        {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes -= evicted.bytes.len();
            }
        }
        self.bytes += image.bytes.len();
        self.lru.push_back(key);
        self.entries.insert(key, image);
    }
}

pub(crate) struct AvatarResources {
    cache: Mutex<Cache>,
    pub(super) builders: Arc<Semaphore>,
    pub(super) waiters: Arc<Semaphore>,
}

impl Default for AvatarResources {
    fn default() -> Self {
        Self {
            cache: Mutex::new(Cache::default()),
            builders: Arc::new(Semaphore::new(MAX_BUILDERS)),
            waiters: Arc::new(Semaphore::new(MAX_WAITERS)),
        }
    }
}

enum Admission {
    Follow(Subscription, OwnedSemaphorePermit),
    Lead {
        sender: watch::Sender<Option<BuildResult>>,
        builder: Option<OwnedSemaphorePermit>,
        waiter: Option<OwnedSemaphorePermit>,
    },
}

impl AvatarResources {
    pub(super) async fn get(&self, key: Key) -> BuildResult {
        self.get_with(key, move || match key.format {
            Format::Svg => Ok(artwork::render_svg(key.recipe, key.expression).into_bytes()),
            Format::Png => artwork::render_png(key.recipe, key.expression, key.size)
                .map_err(|_| BuildError::Failed),
        })
        .await
    }

    async fn get_with<F>(&self, key: Key, build: F) -> BuildResult
    where
        F: FnOnce() -> Result<Vec<u8>, BuildError> + Send + 'static,
    {
        let admission = {
            let mut cache = self.cache.lock().expect("avatar cache");
            if let Some(image) = cache.cached(&key) {
                return Ok(image);
            }
            if let Some(receiver) = cache.inflight.get(&key) {
                Admission::Follow(
                    receiver.clone(),
                    self.waiters
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| BuildError::Overloaded)?,
                )
            } else {
                let builder = self.builders.clone().try_acquire_owned().ok();
                let waiter = if builder.is_none() {
                    Some(
                        self.waiters
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| BuildError::Overloaded)?,
                    )
                } else {
                    None
                };
                let (sender, receiver) = watch::channel(None);
                cache.inflight.insert(key, receiver);
                Admission::Lead {
                    sender,
                    builder,
                    waiter,
                }
            }
        };
        match admission {
            Admission::Follow(mut receiver, _waiter) => loop {
                if let Some(result) = receiver.borrow_and_update().clone() {
                    return result;
                }
                receiver
                    .changed()
                    .await
                    .map_err(|_| BuildError::Cancelled)?;
            },
            Admission::Lead {
                sender,
                builder,
                waiter,
            } => {
                // Dropping a cancelled leader removes the key and wakes followers.
                let mut lease = BuildLease {
                    resources: self,
                    key,
                    sender: Some(sender),
                };
                let builder = match builder {
                    Some(permit) => permit,
                    None => self
                        .builders
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|_| BuildError::Cancelled)?,
                };
                drop(waiter);
                let result = tokio::task::spawn_blocking(move || {
                    // Cancellation cannot release a still-running raster's slot.
                    let _builder = builder;
                    build().and_then(Image::new).map(Arc::new)
                })
                .await
                .unwrap_or(Err(BuildError::Failed));
                if let Ok(image) = &result {
                    self.cache
                        .lock()
                        .expect("avatar cache")
                        .insert(key, image.clone());
                }
                lease.finish(result.clone());
                result
            }
        }
    }
}

struct BuildLease<'a> {
    resources: &'a AvatarResources,
    key: Key,
    sender: Option<watch::Sender<Option<BuildResult>>>,
}

impl BuildLease<'_> {
    fn finish(&mut self, result: BuildResult) {
        if let Some(sender) = self.sender.take() {
            self.resources
                .cache
                .lock()
                .expect("avatar cache")
                .inflight
                .remove(&self.key);
            sender.send_replace(Some(result));
        }
    }
}

impl Drop for BuildLease<'_> {
    fn drop(&mut self) {
        self.finish(Err(BuildError::Cancelled));
    }
}

#[cfg(test)]
#[cfg(holon_lib_test_shard = "c")]
mod tests {
    use super::*;
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
}
