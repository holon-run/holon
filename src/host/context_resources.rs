//! Scoped invalidation and a short physical-writer/Closing fence.
use crate::runtime::resource_observation::{
    ContextResourceRegistry, ResourceFreshness, ResourceInvalidation, ResourceObservationError,
};
use anyhow::{anyhow, ensure, Result};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::Notify;

pub(crate) struct ContextResources {
    base_dir: PathBuf,
    state: Mutex<State>,
    pub(super) drained: Notify,
    #[cfg(test)]
    pub(super) physical_checkpoint:
        Mutex<Option<Arc<super::resource_writes::PhysicalWriteBarrier>>>,
}
#[derive(Default)]
pub(super) struct State {
    pub active: Vec<Arc<ResourceWriteScope>>,
    pub closing: bool,
    pub failed: bool,
    subscribers: Vec<Subscription>,
}
struct Subscription {
    registry: Weak<ContextResourceRegistry>,
    notify: Weak<Notify>,
    paths: Vec<PathBuf>,
}

#[derive(PartialEq, Eq)]
pub(crate) enum ResourceWriteScope {
    Shared,
    Paths(Vec<PathBuf>),
}
impl ResourceWriteScope {
    pub(crate) fn paths(paths: Vec<PathBuf>) -> Self {
        let mut paths = paths
            .into_iter()
            .map(|path| normalize(&path))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        Self::Paths(paths)
    }
    pub(super) fn shared() -> Self {
        Self::Shared
    }
    fn affects(&self, subscription: &Subscription) -> bool {
        match self {
            Self::Shared => true,
            Self::Paths(paths) => paths.iter().any(|written| {
                subscription
                    .paths
                    .iter()
                    .any(|read| written.starts_with(read) || read.starts_with(written))
            }),
        }
    }
}

// Source capture must not probe the filesystem. Normalize lexical aliases only.
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if result.file_name().is_some_and(|name| name != "..") => {
                result.pop();
            }
            Component::ParentDir if result.has_root() && result.file_name().is_none() => {}
            component => result.push(component.as_os_str()),
        }
    }
    result
}

fn resolve_missing_path(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            // canonicalize cannot resolve a dangling link. Its target may still
            // become a resource later, so resolve the missing target's parent.
            if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_symlink()) {
                let target = std::fs::read_link(path)?;
                return resolve_missing_path(&path.parent().unwrap().join(target));
            }
            match (path.parent(), path.file_name()) {
                (Some(parent), Some(name)) => Ok(resolve_missing_path(parent)?.join(name)),
                _ => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
impl Default for ContextResources {
    fn default() -> Self {
        Self::new().expect("resource coordinator current directory")
    }
}
pub(crate) struct ReadyFence<'a> {
    _state: MutexGuard<'a, State>,
}

#[derive(Debug, thiserror::Error)]
#[error("context resources have active writers")]
pub(crate) struct ContextResourceWritersActive;

impl ContextResources {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self::with_base_dir(std::env::current_dir()?))
    }
    fn with_base_dir(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            state: Mutex::new(State::default()),
            drained: Notify::new(),
            #[cfg(test)]
            physical_checkpoint: Mutex::new(None),
        }
    }
    fn absolute(&self, path: &Path) -> PathBuf {
        normalize(&self.base_dir.join(path))
    }
    pub(crate) fn path_scope(&self, paths: Vec<PathBuf>) -> ResourceWriteScope {
        ResourceWriteScope::paths(paths.into_iter().map(|path| self.absolute(&path)).collect())
    }
    /// Physical preparation only: keep lexical paths as well as symlink targets,
    /// including missing leaves below an existing symlinked directory.
    pub(crate) fn resolve_paths(&self, paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
        let mut aliases = Vec::new();
        for path in paths {
            let path = self.absolute(&path);
            let resolved = resolve_missing_path(&path)?;
            aliases.push(path);
            aliases.push(resolved);
        }
        aliases.sort();
        aliases.dedup();
        Ok(aliases)
    }
    pub(crate) fn resolve_scope(&self, scope: &ResourceWriteScope) -> Result<ResourceWriteScope> {
        match scope {
            ResourceWriteScope::Shared => Ok(ResourceWriteScope::Shared),
            ResourceWriteScope::Paths(paths) => Ok(ResourceWriteScope::Paths(
                self.resolve_paths(paths.clone())?,
            )),
        }
    }
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("resource fence poisoned"))
    }
    pub(crate) fn subscribe(
        &self,
        registry: &Arc<ContextResourceRegistry>,
        notify: &Arc<Notify>,
    ) -> Result<()> {
        let mut state = self.lock()?;
        ensure!(!state.closing, "context resource subscription is Closing");
        state
            .subscribers
            .retain(|entry| entry.registry.strong_count() > 0);
        state.subscribers.push(Subscription {
            registry: Arc::downgrade(registry),
            notify: Arc::downgrade(notify),
            paths: Vec::new(),
        });
        Ok(())
    }
    pub(crate) fn set_paths(
        &self,
        registry: &Arc<ContextResourceRegistry>,
        paths: Vec<PathBuf>,
    ) -> Result<()> {
        let mut state = self.lock()?;
        ensure!(!state.closing, "context resource subscription is Closing");
        let subscription = state
            .subscribers
            .iter_mut()
            .find(|entry| entry.registry.ptr_eq(&Arc::downgrade(registry)))
            .ok_or_else(|| anyhow!("context resource subscription missing"))?;
        subscription.paths = paths.into_iter().map(|path| self.absolute(&path)).collect();
        self.drained.notify_waiters();
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn ready(&self) -> Result<ReadyFence<'_>> {
        let state = self.lock()?;
        ensure!(
            !state.closing && !state.failed,
            "context resources are Closing or failed"
        );
        ensure!(state.active.is_empty(), ContextResourceWritersActive);
        Ok(ReadyFence { _state: state })
    }
    pub(crate) fn ready_for(
        &self,
        registry: &Arc<ContextResourceRegistry>,
    ) -> Result<ReadyFence<'_>> {
        let state = self.lock()?;
        Self::check_ready(&state, registry)?;
        Ok(ReadyFence { _state: state })
    }
    fn check_ready(state: &State, registry: &Arc<ContextResourceRegistry>) -> Result<()> {
        ensure!(
            !state.closing && !state.failed,
            "context resources are Closing or failed"
        );
        let subscription = state
            .subscribers
            .iter()
            .find(|entry| entry.registry.ptr_eq(&Arc::downgrade(registry)))
            .ok_or_else(|| anyhow!("context resource subscription missing"))?;
        ensure!(
            !state.active.iter().any(|scope| scope.affects(subscription)),
            ContextResourceWritersActive
        );
        Ok(())
    }
    pub(super) fn changed(state: &mut State, scope: &ResourceWriteScope) -> Result<()> {
        state
            .subscribers
            .retain(|entry| entry.registry.strong_count() > 0);
        let mut failure = None;
        for subscription in &state.subscribers {
            if !scope.affects(subscription) {
                continue;
            }
            if let Some(registry) = subscription.registry.upgrade() {
                if let Err(error) = registry.invalidate(ResourceInvalidation::KnownWrite) {
                    if !matches!(
                        error.downcast_ref::<ResourceObservationError>(),
                        Some(ResourceObservationError::NotReady(
                            ResourceFreshness::Closed
                        ))
                    ) && failure.is_none()
                    {
                        failure = Some(error);
                    }
                }
            }
            if let Some(notify) = subscription.notify.upgrade() {
                notify.notify_one();
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub(crate) fn invalidate(&self, scope: &ResourceWriteScope) -> Result<()> {
        let mut state = self.lock()?;
        ensure!(!state.closing, "context resource invalidation is Closing");
        Self::changed(&mut state, scope)
    }
    /// Synchronous config publication shares the install fence; no I/O/await.
    pub(crate) fn publish_config<T>(&self, publish: impl FnOnce() -> Result<T>) -> Result<T> {
        let state = self.lock()?;
        ensure!(!state.closing, "context resource configuration is Closing");
        publish()
    }
    pub(super) fn begin_close(&self) -> Result<()> {
        let mut state = self.lock()?;
        state.closing = true;
        for notify in state
            .subscribers
            .iter()
            .filter_map(|entry| entry.notify.upgrade())
        {
            notify.notify_one();
        }
        self.drained.notify_waiters();
        Ok(())
    }
    pub(crate) async fn wait_for_writers(
        &self,
        registry: &Arc<ContextResourceRegistry>,
    ) -> Result<()> {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let drained = {
                let state = self.lock()?;
                match Self::check_ready(&state, registry) {
                    Ok(()) => true,
                    Err(error) if error.is::<ContextResourceWritersActive>() => false,
                    Err(error) => return Err(error),
                }
            };
            if drained {
                return Ok(());
            }
            notified.await;
        }
    }
    pub(super) async fn join_writers(&self) -> Result<()> {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let drained = {
                let state = self.lock()?;
                if state.active.is_empty() {
                    ensure!(!state.failed, "physical context resource writer panicked");
                    true
                } else {
                    false
                }
            };
            if drained {
                return Ok(());
            }
            notified.await;
        }
    }
}
impl super::RuntimeHost {
    pub(crate) fn context_resources(&self) -> Arc<ContextResources> {
        self.inner.context_resources.clone()
    }
}
impl super::RuntimeHostBridge {
    pub(crate) fn context_resources(&self) -> Result<Arc<ContextResources>> {
        Ok(self.host()?.context_resources())
    }
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits, runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };

    fn subscriber(
        gate: &ContextResources,
        paths: &[&str],
    ) -> Result<(Arc<ContextResourceRegistry>, Arc<Notify>)> {
        let registry = Arc::new(ContextResourceRegistry::new(
            InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
        ));
        let notify = Arc::new(Notify::new());
        gate.subscribe(&registry, &notify)?;
        gate.set_paths(&registry, paths.iter().map(PathBuf::from).collect())?;
        Ok((registry, notify))
    }

    #[tokio::test]
    async fn scoped_resource_writers_only_invalidate_and_block_matching_consumers() -> Result<()> {
        let gate = Arc::new(ContextResources::default());
        let (first, _first_notify) =
            subscriber(&gate, &["/agents/first", "/workspace/shared/AGENTS.md"])?;
        let (second, _second_notify) =
            subscriber(&gate, &["/agents/second", "/workspace/other/AGENTS.md"])?;
        let (shared, _shared_notify) =
            subscriber(&gate, &["/agents/shared", "/workspace/shared/AGENTS.md"])?;
        let private = gate.begin_path_write(vec!["/agents/first/notes/file.md".into()])?;
        assert!(gate.ready_for(&first).is_err());
        drop(gate.ready_for(&second)?);
        drop(gate.ready_for(&shared)?);
        assert_eq!(first.generation_for_test()?, 1);
        assert_eq!(second.generation_for_test()?, 0);
        assert_eq!(shared.generation_for_test()?, 0);
        drop(private);
        assert_eq!(first.generation_for_test()?, 2);
        let writer = gate.begin_path_write(vec!["/workspace/shared/subdir/../AGENTS.md".into()])?;
        assert!(gate.ready_for(&first).is_err());
        assert!(gate.ready_for(&shared).is_err());
        drop(gate.ready_for(&second)?);
        let (late, _late_notify) = subscriber(&gate, &["/workspace/shared/AGENTS.md"])?;
        assert!(gate.ready_for(&late).is_err());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.wait_for_writers(&late)
        )
        .await
        .is_err());
        drop(writer);
        gate.wait_for_writers(&late).await?;
        assert_eq!(first.generation_for_test()?, 4);
        assert_eq!(shared.generation_for_test()?, 2);
        assert_eq!(late.generation_for_test()?, 1);
        assert_eq!(second.generation_for_test()?, 0);
        first.close()?;
        drop(gate.begin_path_write(vec!["/agents/first/notes/file.md".into()])?);
        assert_eq!(second.generation_for_test()?, 0);
        gate.begin_close()?;
        assert!(gate.ready_for(&second).is_err());
        assert!(gate.begin_path_write(vec!["/unrelated".into()]).is_err());
        Ok(())
    }

    #[test]
    fn relative_dependencies_match_absolute_writes_from_the_captured_base() -> Result<()> {
        let home = tempfile::tempdir()?;
        let gate = Arc::new(ContextResources::with_base_dir(home.path().to_path_buf()));
        let (peer, _notify) = subscriber(&gate, &["./state/agents/peer"])?;
        let (other, _notify_other) = subscriber(&gate, &["state/agents/other"])?;
        let writer = gate.begin_path_write(vec![home
            .path()
            .join("state/agents/peer/memory/operator.md")])?;
        assert!(gate.ready_for(&peer).is_err());
        drop(gate.ready_for(&other)?);
        assert_eq!(peer.generation_for_test()?, 1);
        drop(writer);
        assert_eq!(peer.generation_for_test()?, 2);
        assert_eq!(other.generation_for_test()?, 0);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn physical_aliases_fence_shared_missing_targets_until_writer_exit() -> Result<()> {
        let home = tempfile::tempdir()?;
        let base = std::fs::canonicalize(home.path())?;
        let gate = Arc::new(ContextResources::with_base_dir(base.clone()));
        std::fs::create_dir_all(base.join("first/skills"))?;
        std::fs::create_dir_all(base.join("peer/skills"))?;
        // The shared library and its entrypoint do not exist at subscription time.
        for agent in ["first", "peer"] {
            std::os::unix::fs::symlink("../../library/demo", base.join(agent).join("skills/demo"))?;
        }
        let (peer, _notify) = subscriber(&gate, &["peer/skills"])?;
        gate.set_paths(
            &peer,
            gate.resolve_paths(vec!["peer/skills/demo/SKILL.md".into()])?,
        )?;
        let writer = gate.begin_path_write(vec!["first/skills/demo/SKILL.md".into()])?;
        drop(gate.ready_for(&peer)?);
        assert_eq!(peer.generation_for_test()?, 0);
        writer.enter_physical()?;
        assert!(gate.ready_for(&peer).is_err());
        assert_eq!(peer.generation_for_test()?, 1);
        std::fs::create_dir_all(base.join("library/demo"))?;
        std::fs::write(base.join("first/skills/demo/SKILL.md"), "new shared skill")?;
        let (late, _late_notify) = subscriber(&gate, &["library/demo"])?;
        assert!(gate.ready_for(&late).is_err());
        gate.begin_close()?;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), gate.join_writers())
                .await
                .is_err()
        );
        drop(writer);
        gate.join_writers().await?;
        assert_eq!(peer.generation_for_test()?, 2);
        assert_eq!(late.generation_for_test()?, 1);
        Ok(())
    }
}
