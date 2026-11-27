//! Provider/Model listing surfaces must agree.
//!
//! The Agent tools (`ListModelProviders` / `ListProviderModels`), the HTTP
//! endpoints under `/api/models/providers[...]` and the CLI
//! (`config model-providers list` / `config models list`) all filter the same
//! resolved projection through `holon_runtime::provider::model_query`. This
//! test pins that agreement against one shared config + discovery-cache
//! snapshot, without asserting concrete catalog members.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use holon::provider::{
    model_query, resolved_model_providers, resolved_provider_models, StubProvider,
};
use serde_json::Value;

const CREDENTIAL_ENV: &str = "DEEPSEEK_API_KEY";
const CREDENTIAL_VALUE: &str = "sk-test-model-query-surfaces";

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("CARGO_BIN_EXE_holon should be set for integration tests")
}

fn cli(home: &Path) -> Command {
    let mut command = Command::new(holon_bin());
    command
        .env("HOLON_HOME", home)
        .env("HOLON_AGENT_ID", "main")
        .env("HOLON_HTTP_ADDR", "127.0.0.1:9")
        .env("HOLON_SOCKET_PATH", home.join("run").join("missing.sock"))
        .env(CREDENTIAL_ENV, CREDENTIAL_VALUE)
        .env_remove("HOLON_CONTROL_TOKEN")
        .env_remove("HOLON_CONTROL_AUTH_MODE")
        .env_remove("RUST_LOG");
    command
}

fn run_cli_json(mut command: Command, context: &str) -> Value {
    let output = command.output().expect("spawn holon CLI");
    assert!(
        output.status.success(),
        "{context} failed: status {:?}\nstderr:\n{}\nstdout:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{context} did not print JSON: {error}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn collect_ids(entries: &[Value], field: &str) -> BTreeSet<String> {
    entries
        .iter()
        .map(|entry| {
            entry[field]
                .as_str()
                .unwrap_or_else(|| panic!("entry is missing string field {field}: {entry}"))
                .to_string()
        })
        .collect()
}

fn projected_provider_ids(
    config: &holon::config::AppConfig,
    include_unavailable: bool,
) -> BTreeSet<String> {
    let mut providers = resolved_model_providers(config);
    providers.retain(|provider| model_query::provider_matches(provider, None, include_unavailable));
    providers.into_iter().map(|provider| provider.id).collect()
}

fn projected_model_refs(
    config: &holon::config::AppConfig,
    provider: &str,
    include_unavailable: bool,
) -> BTreeSet<String> {
    let mut models = resolved_provider_models(config, provider);
    models.retain(|model| model_query::model_matches(model, None, include_unavailable));
    models.into_iter().map(|model| model.model_ref).collect()
}

fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[tokio::test]
async fn model_listing_surfaces_agree() -> anyhow::Result<()> {
    support::init_test_tracing();
    let home = support::tempdir()?;
    // The CLI subprocess and this process must resolve the same config, the same
    // credential environment and the same discovery cache directory.
    std::env::set_var("HOLON_HOME", home.path());
    std::env::set_var(CREDENTIAL_ENV, CREDENTIAL_VALUE);

    // The persisted config is produced by the CLI itself so the fixture matches
    // the on-disk contract every surface reads.
    let setup: [&[&str]; 2] = [
        &[
            "config",
            "providers",
            "set",
            "deepseek",
            "--credential-source",
            "env",
            "--credential-kind",
            "api_key",
            "--credential-env",
            CREDENTIAL_ENV,
        ],
        &[
            "config",
            "set",
            "model.default",
            "deepseek@default/deepseek-chat",
        ],
    ];
    for args in setup {
        let mut command = cli(home.path());
        command.args(args);
        let output = command.output()?;
        assert!(
            output.status.success(),
            "holon {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut config = holon::config::AppConfig::load_for_config_inspection()?;
    config.http_addr = "127.0.0.1:0".to_string();

    // The tool host resolves providers/models through these helpers and then
    // applies the shared predicates, so this is the tool-surface expectation.
    let expected_providers = projected_provider_ids(&config, false);
    assert!(
        !expected_providers.is_empty(),
        "fixture must resolve at least one selectable provider"
    );

    let harness = support::HttpHarness::with_config_and_provider(
        config.clone(),
        Arc::new(StubProvider::new("ok")),
    )
    .await?;
    let client = reqwest::Client::new();

    let providers_body: Value = client
        .get(format!("{}/api/models/providers", harness.base_url))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let http_providers = collect_ids(
        providers_body["providers"]
            .as_array()
            .expect("providers array"),
        "id",
    );
    assert_eq!(
        http_providers, expected_providers,
        "HTTP provider view must equal the tool projection"
    );

    let mut command = cli(home.path());
    command.args(["config", "model-providers", "list"]);
    let cli_providers = run_cli_json(command, "config model-providers list");
    let cli_provider_ids = collect_ids(cli_providers.as_array().expect("provider array"), "id");
    assert_eq!(
        cli_provider_ids, expected_providers,
        "CLI provider view must equal the tool projection"
    );

    let mut provider_with_models = None;
    for provider in &expected_providers {
        let expected_models = projected_model_refs(&config, provider, true);
        if expected_models.is_empty() {
            continue;
        }
        provider_with_models = Some(provider.clone());

        // HTTP: page through the diagnostic view so paging is exercised too.
        let mut http_models = BTreeSet::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/api/models/providers/{}/models?include_unavailable=true&limit=2",
                harness.base_url,
                encode(provider)
            );
            if let Some(cursor) = cursor.as_ref() {
                url.push_str(&format!("&cursor={}", encode(cursor)));
            }
            let page: Value = client
                .get(url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let models = page["models"].as_array().expect("models array");
            http_models.extend(collect_ids(models, "model_ref"));
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            http_models, expected_models,
            "HTTP model view for {provider} must equal the tool projection"
        );

        let mut command = cli(home.path());
        command.args(["config", "models", "list", "--provider", provider, "--all"]);
        let cli_models = run_cli_json(command, "config models list --provider");
        let cli_model_refs = collect_ids(cli_models.as_array().expect("model array"), "model_ref");
        assert_eq!(
            cli_model_refs, expected_models,
            "CLI model view for {provider} must equal the tool projection"
        );
    }
    let provider_with_models = provider_with_models
        .expect("fixture must resolve at least one model for one selectable provider");

    // Default (non-diagnostic) views keep unavailable entries out on every surface.
    let default_models = projected_model_refs(&config, &provider_with_models, false);
    let page: Value = client
        .get(format!(
            "{}/api/models/providers/{}/models",
            harness.base_url,
            encode(&provider_with_models)
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        collect_ids(
            page["models"].as_array().expect("models array"),
            "model_ref"
        ),
        default_models
    );
    let mut command = cli(home.path());
    command.args([
        "config",
        "models",
        "list",
        "--provider",
        &provider_with_models,
    ]);
    let cli_default = run_cli_json(command, "config models list (default view)");
    assert_eq!(
        collect_ids(cli_default.as_array().expect("model array"), "model_ref"),
        default_models
    );

    // Unknown providers and stale cursors are typed HTTP failures.
    let unknown = client
        .get(format!(
            "{}/api/models/providers/{}/models",
            harness.base_url,
            encode("definitely-not-a-provider")
        ))
        .send()
        .await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = unknown.json().await?;
    assert_eq!(body["code"], "unknown_provider");

    let bad_cursor = client
        .get(format!(
            "{}/api/models/providers/{}/models?cursor=missing-model-ref",
            harness.base_url,
            encode(&provider_with_models)
        ))
        .send()
        .await?;
    assert_eq!(bad_cursor.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = bad_cursor.json().await?;
    assert_eq!(body["code"], "invalid_cursor");

    Ok(())
}
