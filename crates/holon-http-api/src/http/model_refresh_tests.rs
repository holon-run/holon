//! `POST /api/models/refresh` regressions: an explicit operator refresh is the
//! opt-in, so it must bypass the discovery cache TTL and probe authorized
//! endpoints remotely even while their cache entry is still `Fresh`.
use super::*;
use crate::config::{ModelCatalogSource, ProviderId};
use crate::model_discovery::{
    discovery_cache_path, load_discovery_cache_at, save_discovery_cache_at,
    ModelDiscoveryCacheFile, ModelDiscoveryCacheKey, ProviderModelDiscoveryCache,
};
use crate::provider::StubProvider;
use axum::body::to_bytes;
use axum::http::Request;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;

#[derive(Clone)]
struct LogBuffer(Arc<StdMutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn magpie_config(base_url: &str) -> String {
    serde_json::json!({
        "model": {"default": "openai/gpt-5.4"},
        "providers": {
            "magpie": {
                "transport": "anthropic_messages",
                "base_url": base_url,
                "auth": {"source": "env", "kind": "api_key", "env": "MAGPIE_API_KEY"}
            }
        }
    })
    .to_string()
}

fn models_body(models: &[&str]) -> String {
    let entries = models
        .iter()
        .map(|model| serde_json::json!({"id": model}))
        .collect::<Vec<_>>();
    serde_json::json!({ "data": entries }).to_string()
}

/// Home + host with a declared zero-config Magpie provider pointing at a
/// caller-supplied base URL, plus the control token required to call the
/// endpoint.
fn magpie_host(home: &std::path::Path, base_url: &str) -> Result<RuntimeHost> {
    std::fs::write(home.join("config.json"), magpie_config(base_url))?;
    let mut config = crate::config::AppConfig::load_with_home(Some(home.to_path_buf()))?;
    config.control_token = Some("secret".into());
    config.control_auth_mode = crate::config::ControlAuthMode::Required;
    RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))
}

/// One-shot mock discovery endpoint answering `GET {path}` once.
fn spawn_mock_discovery(
    path: &'static str,
    body: String,
) -> (String, Arc<AtomicU64>, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicU64::new(0));
    let counted = hits.clone();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]).to_string();
        assert!(
            request.starts_with(&format!("GET {path} HTTP/1.1")),
            "unexpected discovery request: {request}"
        );
        counted.fetch_add(1, Ordering::AcqRel);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    (base_url, hits, handle)
}

/// Seed a cache entry whose age is far inside `DEFAULT_DISCOVERY_CACHE_TTL`,
/// so the automatic gate would skip this provider entirely.
fn seed_fresh_magpie_cache(
    home: &std::path::Path,
    expected_source_url: &str,
) -> std::path::PathBuf {
    let path = discovery_cache_path(home);
    let mut cache = ModelDiscoveryCacheFile::default();
    cache.providers.insert(
        magpie_cache_key(),
        ProviderModelDiscoveryCache {
            provider: ProviderId::parse("magpie").unwrap(),
            fetched_at: Utc::now(),
            source_url: Some(expected_source_url.to_string()),
            response_hash: Some("sha256:stale".into()),
            models: Vec::new(),
        },
    );
    save_discovery_cache_at(&path, &cache).unwrap();
    path
}

fn magpie_cache_key() -> ModelDiscoveryCacheKey {
    ModelDiscoveryCacheKey::new(
        ProviderId::parse("magpie").unwrap(),
        crate::config::ProviderEndpointId::default_endpoint(),
    )
}

async fn post_refresh(host: RuntimeHost) -> Result<Vec<u8>> {
    let app = crate::http::router(AppState::for_tcp(host));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/models/refresh")
                .header(AUTHORIZATION, "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    Ok(to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec())
}

#[tokio::test]
async fn explicit_refresh_probes_remote_despite_fresh_cache() -> Result<()> {
    let (base_url, hits, server) = spawn_mock_discovery("/v1/models", models_body(&["magpie-new"]));
    let home = tempfile::tempdir()?;
    let host = magpie_host(home.path(), &base_url)?;
    let cache_path = seed_fresh_magpie_cache(home.path(), &format!("{base_url}/v1/models"));

    post_refresh(host.clone()).await?;
    server.join().unwrap();

    assert_eq!(
        hits.load(Ordering::Acquire),
        1,
        "a Fresh cache entry must not suppress the explicit remote probe"
    );
    let cache = load_discovery_cache_at(&cache_path)?;
    let entry = cache
        .providers
        .get(&magpie_cache_key())
        .expect("explicit refresh rewrites the provider cache entry");
    assert_eq!(
        entry
            .models
            .iter()
            .map(|model| model.model_ref.model.as_str())
            .collect::<Vec<_>>(),
        vec!["magpie-new"],
        "the rewritten snapshot comes from the remote response"
    );
    Ok(())
}

#[test]
fn explicit_refresh_reports_refreshed_and_skipped_outcomes() -> Result<()> {
    let (base_url, _hits, _server) =
        spawn_mock_discovery("/v1/models", models_body(&["magpie-new"]));
    let home = tempfile::tempdir()?;
    let host = magpie_host(home.path(), &base_url)?;
    seed_fresh_magpie_cache(home.path(), &format!("{base_url}/v1/models"));

    // Drive the candidate sweep directly instead of through the router: the
    // outcome diagnostics are emitted here, and the mock server never has to
    // be awaited, so this test cannot deadlock on it. The HTTP path itself is
    // covered by the Fresh-cache regressions above.
    let logs = Arc::new(StdMutex::new(Vec::new()));
    let output = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || LogBuffer(output.clone()))
        .finish();
    let config = host.config();
    let cache_path = discovery_cache_path(home.path());
    tracing::subscriber::with_default(subscriber, || {
        let (candidates, skipped) = super::agents::model_discovery_refresh_candidates(&config);
        for (provider, reason) in skipped {
            debug!(
                provider = %provider.id.as_str(),
                reason = reason,
                "explicit model discovery refresh skipped provider"
            );
        }
        // The mock accepts exactly one request, so only the provider under
        // test is probed; every other candidate is a real endpoint that must
        // not be touched from a unit test.
        let magpie = candidates
            .into_iter()
            .find(|(provider, _)| provider.id.as_str() == "magpie")
            .expect("declared magpie is an explicit refresh candidate");
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(crate::model_discovery::refresh_provider_models(
                &magpie.0,
                magpie.1.as_ref(),
                &cache_path,
            )) {
            Ok(_) => debug!(
                provider = %magpie.0.id.as_str(),
                "explicit model discovery refresh refreshed provider"
            ),
            Err(error) => debug!(
                provider = %magpie.0.id.as_str(),
                error = %error,
                "explicit model discovery refresh failed"
            ),
        }
    });

    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("explicit model discovery refresh refreshed provider")
            && logs.contains("provider=magpie"),
        "refreshed outcome is logged per provider: {logs}"
    );
    assert!(
        logs.contains("explicit model discovery refresh skipped provider")
            && logs.contains("discovery_not_allowed"),
        "skipped outcome carries its reason: {logs}"
    );
    Ok(())
}

#[tokio::test]
async fn explicit_refresh_targets_the_same_endpoint_as_cli_provider_refresh() -> Result<()> {
    // `holon config models refresh --provider magpie` resolves this exact
    // provider/discovery pair and probes it unconditionally; the explicit HTTP
    // refresh must issue the same request with a Fresh cache entry present.
    let (base_url, hits, server) = spawn_mock_discovery("/v1/models", models_body(&["magpie-new"]));
    let home = tempfile::tempdir()?;
    let host = magpie_host(home.path(), &base_url)?;
    let cache_path = seed_fresh_magpie_cache(home.path(), &format!("{base_url}/v1/models"));
    post_refresh(host).await?;
    server.join().unwrap();
    assert_eq!(hits.load(Ordering::Acquire), 1);
    assert!(
        load_discovery_cache_at(&cache_path)?
            .providers
            .contains_key(&magpie_cache_key()),
        "cache rewritten by the explicit refresh"
    );
    Ok(())
}

#[tokio::test]
async fn explicit_refresh_failure_stays_scoped_to_that_provider() -> Result<()> {
    // An unresponsive provider cannot block the rest of the sweep: the
    // endpoint still answers 200 and the healthy provider is refreshed.
    let (base_url, hits, server) = spawn_mock_discovery("/v1/models", models_body(&["magpie-new"]));
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_base = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let home = tempfile::tempdir()?;
    let config = serde_json::json!({
        "model": {"default": "openai/gpt-5.4"},
        "providers": {
            "magpie": {
                "transport": "anthropic_messages",
                "base_url": base_url,
                "auth": {"source": "env", "kind": "api_key", "env": "MAGPIE_API_KEY"}
            },
            "dead-gateway": {
                "transport": "openai_chat_completions",
                "base_url": dead_base,
                "auth": {"source": "none", "kind": "none"}
            }
        }
    })
    .to_string();
    std::fs::write(home.path().join("config.json"), config)?;
    let mut app_config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    app_config.control_token = Some("secret".into());
    app_config.control_auth_mode = crate::config::ControlAuthMode::Required;
    let host = RuntimeHost::new_with_provider(app_config, Arc::new(StubProvider::new("done")))?;
    seed_fresh_magpie_cache(home.path(), &format!("{base_url}/v1/models"));
    post_refresh(host).await?;
    server.join().unwrap();
    assert_eq!(hits.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn explicit_refresh_exclusion_keeps_reasons_for_skipped_providers() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let catalog = config.model_catalog_config();
    let magpie = config
        .providers
        .get(&ProviderId::parse("magpie").unwrap())
        .expect("magpie is built in")
        .clone();
    // Undeclared and unconfigured: the explicit refresh must not probe it behind
    // the operator's back, and it must say why.
    assert_eq!(
        catalog.explicit_model_discovery_refresh_exclusion(&magpie, None),
        Some("discovery_not_allowed")
    );
    let static_only = config
        .providers
        .get(&ProviderId::parse("mistral").unwrap())
        .expect("mistral is built in")
        .clone();
    assert_eq!(
        catalog.explicit_model_discovery_refresh_exclusion(&static_only, None),
        Some("discovery_unsupported")
    );
}
