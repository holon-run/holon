use super::*;

#[test]
fn supplemental_throttle_scope_preserves_legacy_chain_fingerprint() {
    let home = tempfile::tempdir().unwrap();
    let mut config = config_with_urls(home.path(), ["http://127.0.0.1:1/v1", "http://127.0.0.1:2"]);
    for endpoint in config.providers.values_mut() {
        endpoint.credential = None;
        endpoint.auth.kind = crate::config::CredentialKind::None;
        endpoint.auth.source = crate::config::CredentialSource::None;
    }
    let runtime = open_runtime(home.path(), config);
    let state = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(runtime.agent_state())
        .unwrap();
    let chain = runtime.provider_admission_chain(&state).unwrap().unwrap();
    let legacy: Vec<_> = chain
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.model.as_string(),
                candidate
                    .quota
                    .iter()
                    .chain([&candidate.connection, &candidate.request])
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        chain.fingerprint,
        format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&legacy).unwrap())
        )
    );
    for candidate in &chain.candidates {
        assert!(candidate.quota.is_none());
        assert_eq!(
            candidate.scopes,
            [
                candidate.connection.clone(),
                candidate.request.clone(),
                candidate.payment.clone(),
                candidate.contract.clone(),
                candidate.rate_limit.clone(),
            ]
        );
    }
}

#[test]
fn unauthenticated_http_429_selects_healthy_backup_without_epoch_churn() {
    let thread = std::thread::Builder::new()
        .name("provider-admission-429".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    use std::sync::atomic::Ordering;

                    let primary = OutageServer::start_with_status(
                        true,
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                    )
                    .await;
                    let backup = OutageServer::start(false).await;
                    backup.healthy.store(true, Ordering::SeqCst);
                    let home = tempfile::tempdir().unwrap();
                    let mut config = config_with_urls(home.path(), [&primary.url, &backup.url]);
                    for provider in [ProviderId::openai(), ProviderId::anthropic()] {
                        let endpoint = config.providers.get_mut(&provider).unwrap();
                        endpoint.credential = None;
                        endpoint.auth.kind = crate::config::CredentialKind::None;
                        endpoint.auth.source = crate::config::CredentialSource::None;
                    }
                    let runtime = open_runtime(home.path(), config);
                    let chain = runtime
                        .provider_admission_chain(&runtime.agent_state().await.unwrap())
                        .unwrap()
                        .unwrap();
                    assert!(chain
                        .candidates
                        .iter()
                        .all(|candidate| candidate.quota.is_none()));
                    runtime.enqueue(operator()).await.unwrap();
                    assert!(process_next_model_input(&runtime).await);
                    assert_eq!(primary.calls.load(Ordering::SeqCst), 1);
                    assert!(process_next_model_input(&runtime).await);
                    assert_eq!(
                        primary.calls.load(Ordering::SeqCst),
                        1,
                        "the rejected route must cool instead of reopening a resolved epoch"
                    );
                    assert_eq!(backup.calls.load(Ordering::SeqCst), 1);
                    assert!(!process_next_model_input(&runtime).await);
                    runtime
                        .runtime_db()
                        .with_read(|connection| {
                            let (requests, epochs, limited): (i64, i64, i64) = connection
                                .query_row(
                                    "SELECT COUNT(*), COUNT(DISTINCT epoch),
                                        SUM(outcome = 'rate_limited')
                                 FROM provider_recovery_requests",
                                    [],
                                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                                )?;
                            assert_eq!((requests, epochs, limited), (2, 1, 1));
                            Ok(())
                        })
                        .unwrap();
                });
        })
        .unwrap();
    if let Err(panic) = thread.join() {
        std::panic::resume_unwind(panic);
    }
}
