//! Responsibility belongs to the admitted physical writer, not its waiter.
use super::context_resources::ContextResources;
use anyhow::{ensure, Result};
use std::sync::Arc;

pub(crate) struct ResourceWrite {
    resources: Arc<ContextResources>,
}
impl ResourceWrite {
    /// Called inside the original blocking kernel, never in its waiter.
    pub(crate) fn enter_physical(&self) {
        #[cfg(test)]
        if let Some(barrier) = self.resources.physical_checkpoint.lock().unwrap().take() {
            barrier.reached.notify_one();
            let mut released = barrier.released.lock().unwrap();
            while !*released {
                released = barrier.release.wait(released).unwrap();
            }
        }
    }
}
#[cfg(test)]
#[derive(Default)]
pub(crate) struct PhysicalWriteBarrier {
    pub(crate) reached: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    release: std::sync::Condvar,
}
#[cfg(test)]
pub(crate) struct PausedPhysicalWriter(Arc<PhysicalWriteBarrier>);
#[cfg(test)]
impl PausedPhysicalWriter {
    pub(crate) async fn wait(&self) {
        self.0.reached.notified().await;
    }
    pub(crate) fn release(&self) {
        *self.0.released.lock().unwrap() = true;
        self.0.release.notify_all();
    }
}
#[cfg(test)]
impl Drop for PausedPhysicalWriter {
    fn drop(&mut self) {
        self.release();
    }
}
#[cfg(test)]
impl ContextResources {
    pub(crate) fn pause_next_physical_writer(&self) -> PausedPhysicalWriter {
        let barrier = Arc::new(PhysicalWriteBarrier::default());
        *self.physical_checkpoint.lock().unwrap() = Some(barrier.clone());
        PausedPhysicalWriter(barrier)
    }
}
impl ContextResources {
    /// Call only after the original authorization and operation admission.
    pub(crate) fn begin_write(self: &Arc<Self>) -> Result<ResourceWrite> {
        let mut state = self.lock()?;
        ensure!(
            !state.closing,
            "context resource writer admission is Closing"
        );
        let active = state
            .active
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("resource writer count exhausted"))?;
        Self::changed(&mut state)?;
        state.active = active;
        Ok(ResourceWrite {
            resources: self.clone(),
        })
    }
}
impl Drop for ResourceWrite {
    fn drop(&mut self) {
        if let Ok(mut state) = self.resources.lock() {
            state.active -= 1;
            state.failed |= std::thread::panicking();
            // Failure, panic and cancellation are dirty too.
            let _ = ContextResources::changed(&mut state);
            self.resources.drained.notify_waiters();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelling_waiter_keeps_physical_writer_and_closing_join() -> Result<()> {
        let resources = Arc::new(ContextResources::default());
        let writer = resources.begin_write()?;
        let (send, receive) = std::sync::mpsc::channel();
        let kernel = tokio::task::spawn_blocking(move || {
            let _writer = writer;
            receive.recv().unwrap();
        });
        let waiter = tokio::spawn(async move { kernel.await });
        waiter.abort();
        resources.begin_close()?;
        assert!(resources.begin_write().is_err());
        assert!(resources.generation().is_err());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            resources.join_writers()
        )
        .await
        .is_err());
        send.send(())?;
        resources.join_writers().await?;
        Ok(())
    }
}

#[cfg(test)]
mod actual_http_tests {
    use super::*;
    use crate::{config::AppConfig, host::RuntimeHost, provider::StubProvider};
    use std::time::Duration;

    async fn fixture() -> Result<(
        tempfile::TempDir,
        RuntimeHost,
        String,
        tokio::task::JoinHandle<()>,
    )> {
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        let user_home = home.path().join("user");
        std::fs::create_dir_all(&user_home)?;
        config.user_home_dir = Some(user_home);
        let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
        host.default_runtime().await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let router = crate::http::router(crate::http::AppState::for_tcp(host.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Ok((home, host, format!("http://{address}"), server))
    }

    #[tokio::test]
    async fn actual_http_cancel_does_not_release_kernel_or_closing_join() -> Result<()> {
        let (home, host, base, server) = fixture().await?;
        let local = home.path().join("source/cancelled-http");
        std::fs::create_dir_all(&local)?;
        std::fs::write(
            local.join("SKILL.md"),
            "# CancelledHttp\nA real physical install\n",
        )?;
        let gate = host.context_resources();
        let pause = gate.pause_next_physical_writer();
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base}/api/skills/catalog/add"))
                .json(&serde_json::json!({"kind":{"kind":"local","path":local}}))
                .send()
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), pause.wait()).await?;
        request.abort();
        assert!(gate.generation().is_err());
        let closing = tokio::spawn({
            let host = host.clone();
            async move { host.shutdown().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!closing.is_finished());
        assert!(gate.begin_write().is_err());
        pause.release();
        tokio::time::timeout(Duration::from_secs(10), closing).await???;
        assert!(home
            .path()
            .join("user/.agents/skills/cancelled-http/SKILL.md")
            .exists());
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn actual_http_job_kernel_outlives_job_request_and_shutdown_waits() -> Result<()> {
        let (home, host, base, server) = fixture().await?;
        let local = home.path().join("source/job-kernel");
        std::fs::create_dir_all(&local)?;
        std::fs::write(
            local.join("SKILL.md"),
            "# JobKernel\nA real background install\n",
        )?;
        let gate = host.context_resources();
        let pause = gate.pause_next_physical_writer();
        let response = reqwest::Client::new().post(format!("{base}/api/jobs"))
            .json(&serde_json::json!({"kind":"skill.install","params":{"kind":{"kind":"local","path":local}}}))
            .send().await?;
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
        drop(response);
        tokio::time::timeout(Duration::from_secs(10), pause.wait()).await?;
        let closing = tokio::spawn({
            let host = host.clone();
            async move { host.shutdown().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!closing.is_finished());
        pause.release();
        tokio::time::timeout(Duration::from_secs(10), closing).await???;
        assert!(home
            .path()
            .join("user/.agents/skills/job-kernel/SKILL.md")
            .exists());
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn actual_apply_patch_cancel_keeps_kernel_and_closing_join() -> Result<()> {
        let (_home, host, _base, server) = fixture().await?;
        let runtime = host.default_runtime().await?;
        let execution = runtime
            .effective_execution(crate::system::ExecutionScopeKind::AgentTurn)
            .await?;
        let file = execution
            .workspace
            .execution_root()
            .join("physical-patch.txt");
        std::fs::write(&file, "old\n")?;
        let pause = host.context_resources().pause_next_physical_writer();
        let request = tokio::spawn(async move {
            crate::tool::tools::apply_patch_tool::execute(
                &runtime, "default", &crate::types::AuthorityClass::OperatorInstruction,
                &serde_json::json!({"patch":"--- a/physical-patch.txt\n+++ b/physical-patch.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"}),
            ).await
        });
        tokio::time::timeout(Duration::from_secs(10), pause.wait()).await?;
        request.abort();
        assert!(host.context_resources().generation().is_err());
        let closing = tokio::spawn({
            let host = host.clone();
            async move { host.shutdown().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!closing.is_finished());
        pause.release();
        tokio::time::timeout(Duration::from_secs(10), closing).await???;
        assert_eq!(std::fs::read_to_string(file)?, "new\n");
        server.abort();
        Ok(())
    }
}

#[cfg(test)]
mod join_failure_tests {
    use super::*;
    #[tokio::test]
    async fn physical_kernel_panic_keeps_closing_and_returns_join_failure() -> Result<()> {
        let gate = Arc::new(ContextResources::default());
        let writer = gate.begin_write()?;
        let kernel = tokio::task::spawn_blocking(move || {
            let _writer = writer;
            panic!("controlled original physical kernel failure");
        });
        assert!(kernel.await.is_err());
        gate.begin_close()?;
        assert!(gate.join_writers().await.is_err());
        assert!(gate.begin_write().is_err());
        assert!(gate.generation().is_err());
        Ok(())
    }
}
