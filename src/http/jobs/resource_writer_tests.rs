use super::*;
use crate::{config::AppConfig, provider::StubProvider};
use anyhow::Context;
use axum::body::{to_bytes, Body};
use axum::http::Request;
use std::{ffi::OsString, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

struct EnvGuard(&'static str, Option<OsString>);
impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let old = std::env::var_os(key);
        std::env::set_var(key, value);
        Self(key, old)
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.1 {
            Some(value) => std::env::set_var(self.0, value),
            None => std::env::remove_var(self.0),
        }
    }
}

fn fixture(url: &str) -> Result<(tempfile::TempDir, Arc<AppState>)> {
    fixture_with_source(url, true)
}

fn fixture_with_source(url: &str, enabled: bool) -> Result<(tempfile::TempDir, Arc<AppState>)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        serde_json::to_vec(&json!({
            "model": {"default": "openai/gpt-5.4"},
            "agent_templates": {"remote_sources": {
                "official": {"url": url, "ref": "main", "enabled": enabled}
            }}
        }))?,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = Some(home.path().join("user"));
    std::fs::create_dir_all(config.user_home_dir.as_ref().unwrap())?;
    config.control_token = Some("writer-fixture-token".into());
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    Ok((home, Arc::new(AppState::for_tcp(host))))
}

async fn queue_sync(state: &Arc<AppState>) -> Result<String> {
    let response = create_template_remote_source_sync_job(
        state.clone(),
        crate::types::SyncTemplateRemoteSourcesRequest {
            source_id: Some("official".into()),
            force: false,
        },
    )
    .await
    .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
    Ok(body["job"]["id"].as_str().unwrap().into())
}

async fn settled(state: &AppState, id: &str) -> Result<JobSnapshot> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = state.jobs.get(id).context("job missing")?;
            if matches!(job.status, JobStatus::Completed | JobStatus::Failed) {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn remote_template_writer_outlives_request_and_shutdown_joins_actual_sync() -> Result<()> {
    let _env = crate::test_env::lock_env();
    let (home, state) = fixture("https://github.com/owner/repo")?;
    let user_home = home.path().join("user");
    let _home = EnvGuard::set("HOME", &user_home);
    let _token = EnvGuard::set("HOLON_GITHUB_TOKEN", "fixture-only");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let _api = EnvGuard::set(
        "HOLON_TEMPLATE_GITHUB_API_BASE",
        format!("http://{}", listener.local_addr()?),
    );
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let remote = tokio::spawn({
        let reached = reached.clone();
        let release = release.clone();
        async move {
            let files = [
                (
                    "repo-snapshot/agent_templates/worker/template.toml",
                    "schema = \"holon.agent_template.v1\"\nid = \"worker\"\nname = \"Worker\"\nsummary = \"Writer fixture\"\n",
                ),
                ("repo-snapshot/agent_templates/worker/AGENTS.md", "# Worker\n"),
                ("repo-snapshot/agent_templates/worker/skills.toml", ""),
            ];
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            for (path, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, path, contents.as_bytes())?;
            }
            let body = archive.into_inner()?.finish()?;
            let (mut stream, _) = listener.accept().await?;
            let mut request = vec![0; 8192];
            let length = stream.read(&mut request).await?;
            assert!(
                String::from_utf8_lossy(&request[..length]).contains("/owner/repo/tar.gz/main ")
            );
            reached.notify_one();
            release.notified().await;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await?;
            stream.write_all(&body).await?;
            Ok::<_, anyhow::Error>(())
        }
    });
    let gate = state.host.context_resources();
    let original_generation = gate.generation()?;
    // queue_sync consumes and drops the real request response before I/O exits.
    let id = queue_sync(&state).await?;
    tokio::time::timeout(Duration::from_secs(5), reached.notified()).await?;
    assert!(gate.generation().is_err());
    assert!(matches!(
        state.jobs.get(&id).unwrap().status,
        JobStatus::Running
    ));
    assert!(!user_home.join(".agents/agent_templates").exists());
    let mut shutdown = tokio::spawn({
        let host = state.host.clone();
        async move { host.shutdown().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err()
    );
    assert!(gate.ready(original_generation).is_err());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), remote).await???;
    let job = settled(&state, &id).await?;
    assert!(matches!(job.status, JobStatus::Completed), "{job:?}");
    tokio::time::timeout(Duration::from_secs(5), shutdown).await???;
    assert!(user_home
        .join(".agents/agent_templates/.registry.json")
        .is_file());
    assert_eq!(
        std::fs::read_to_string(user_home.join(".agents/agent_templates/worker/AGENTS.md"))?,
        "# Worker\n"
    );
    assert!(gate.generation().is_err());
    Ok(())
}

#[tokio::test]
async fn queued_remote_template_writer_cannot_enter_after_closing() -> Result<()> {
    let _env = crate::test_env::lock_env();
    let (home, state) = fixture("https://github.com/owner/repo")?;
    let user_home = home.path().join("user");
    let _home = EnvGuard::set("HOME", &user_home);
    let permit = state
        .template_remote_source_sync_jobs
        .clone()
        .acquire_owned()
        .await?;
    let id = queue_sync(&state).await?;
    // Shutdown must not wait for a job that has not admitted physical work.
    tokio::time::timeout(Duration::from_secs(5), state.host.shutdown()).await??;
    drop(permit);
    let job = settled(&state, &id).await?;
    assert!(matches!(job.status, JobStatus::Failed));
    assert!(job.error.as_deref().unwrap().contains("Closing"), "{job:?}");
    assert!(job
        .items
        .iter()
        .all(|item| matches!(item.status, JobStatus::Failed)));
    assert!(!user_home.join(".agents/agent_templates").exists());
    Ok(())
}

#[tokio::test]
async fn failed_remote_template_sync_releases_writer_and_invalidates_old_generation() -> Result<()>
{
    let _env = crate::test_env::lock_env();
    let (home, state) = fixture("not-a-github-url")?;
    let user_home = home.path().join("user");
    let _home = EnvGuard::set("HOME", &user_home);
    let gate = state.host.context_resources();
    let generation = gate.generation()?;
    let id = queue_sync(&state).await?;
    let job = settled(&state, &id).await?;
    assert!(matches!(job.status, JobStatus::Failed), "{job:?}");
    assert_eq!(gate.generation()?, generation + 2);
    assert!(gate.ready(generation).is_err());
    assert!(!user_home.join(".agents/agent_templates").exists());
    state.host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn runtime_config_writer_rejects_closing_before_persisting_candidate() -> Result<()> {
    let (home, state) = fixture("not-a-github-url")?;
    let path = home.path().join("config.json");
    let original = std::fs::read(&path)?;
    let app = crate::http::router(AppState::for_tcp(state.host.clone()));
    state.host.shutdown().await?;
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/control/runtime/config")
                .header("authorization", "Bearer writer-fixture-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"updates":[{"key":"model.default","value":"openai/gpt-5.4-mini"}]}"#,
                ))?,
        )
        .await?;
    assert!(!response.status().is_success());
    let body = to_bytes(response.into_body(), 65536).await?;
    assert!(String::from_utf8_lossy(&body).contains("Closing"));
    assert_eq!(std::fs::read(path)?, original);
    assert_eq!(state.host.config_reload_status().requested_generation, 0);
    Ok(())
}

#[tokio::test]
async fn empty_remote_template_sync_keeps_noop_semantics_without_writer_admission() -> Result<()> {
    let _env = crate::test_env::lock_env();
    let (home, state) = fixture_with_source("not-a-github-url", false)?;
    let user_home = home.path().join("user");
    let _home = EnvGuard::set("HOME", &user_home);
    let gate = state.host.context_resources();
    let generation = gate.generation()?;
    for closed in [false, true] {
        if closed {
            state.host.shutdown().await?;
        }
        let response = create_template_remote_source_sync_job(
            state.clone(),
            crate::types::SyncTemplateRemoteSourcesRequest {
                source_id: None,
                force: false,
            },
        )
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
        let id = body["job"]["id"].as_str().unwrap();
        assert!(matches!(
            settled(&state, id).await?.status,
            JobStatus::Completed
        ));
        if !closed {
            assert_eq!(gate.generation()?, generation);
        }
    }
    assert!(!user_home.join(".agents/agent_templates").exists());
    Ok(())
}
