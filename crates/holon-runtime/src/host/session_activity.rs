use super::*;

impl RuntimeHost {
    pub(crate) fn session_activity(&self) -> &crate::authentication::activity::SessionActivity {
        &self.inner.session_activity
    }

    pub fn spawn_daemon_session_activity(&self) {
        let mut worker = self.inner.session_activity_handle.lock().unwrap();
        if worker.is_some() {
            return;
        }
        let host = self.clone();
        *worker = Some(tokio::spawn(async move {
            loop {
                let idle_ttl = Duration::from_secs(host.config().auth.session.idle_ttl_seconds);
                tokio::select! {
                    _ = host.inner.session_activity_token.cancelled() => break,
                    _ = tokio::time::sleep(
                        crate::authentication::activity::checkpoint_interval(idle_ttl),
                    ) => {}
                }
                if let Err(error) = host.checkpoint_session_activity().await {
                    tracing::warn!(error = %error, "session activity checkpoint failed; retaining dirty activity for retry");
                }
            }
        }));
    }

    async fn checkpoint_session_activity(&self) -> Result<()> {
        let activity = self.inner.session_activity.clone();
        let db = self.inner.runtime_db.clone();
        let idle_ttl = Duration::from_secs(self.config().auth.session.idle_ttl_seconds);
        tokio::task::spawn_blocking(move || activity.checkpoint(&db, Utc::now(), idle_ttl))
            .await
            .context("session activity checkpoint worker failed")?
    }

    pub async fn shutdown_daemon_session_activity(&self) -> Result<()> {
        self.inner.session_activity_token.cancel();
        let worker = self.inner.session_activity_handle.lock().unwrap().take();
        if let Some(worker) = worker {
            worker.await.context("session activity worker failed")?;
        }
        self.checkpoint_session_activity().await
    }
}
