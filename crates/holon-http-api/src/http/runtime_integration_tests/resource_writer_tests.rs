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
    assert!(gate.ready().is_err());
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
        crate::tool::run_apply_patch_for_http_test(
            &runtime, "default", &crate::types::AuthorityClass::OperatorInstruction,
            &serde_json::json!({"patch":"--- a/physical-patch.txt\n+++ b/physical-patch.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"}),
        ).await
    });
    tokio::time::timeout(Duration::from_secs(10), pause.wait()).await?;
    request.abort();
    assert!(host.context_resources().ready().is_err());
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
