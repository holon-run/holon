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

pub struct AvatarResources {
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

#[cfg(feature = "test-support")]
mod test_support;

#[cfg(test)]
mod tests;
