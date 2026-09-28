// HTTP Local App Engine route integration tests.

#![allow(dead_code, unused_imports)]

use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::StreamExt;
use holon::host::RuntimeHost;
use reqwest::header::CONTENT_TYPE;
use reqwest::Client;
use tokio::time::{timeout, Duration};

use super::spawn_server;

fn agent_apps_dir(host: &RuntimeHost, agent_id: &str) -> PathBuf {
    host.config()
        .data_dir
        .join("agents")
        .join(agent_id)
        .join("apps")
}

fn write_app(apps_dir: &Path, app_id: &str, files: &[(&str, &str)]) -> Result<()> {
    let root = apps_dir.join(app_id);
    for (name, contents) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
    }
    Ok(())
}

fn manifest(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"{id} app","version":"1.0.0","entry":"index.html"}}"#)
}

pub async fn apps_discovery_and_static_hosting() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);
    write_app(
        &apps_dir,
        "hello",
        &[
            ("manifest.json", &manifest("hello")),
            (
                "index.html",
                "<!doctype html><html><body><h1>Hello App</h1><script src=\"app.js\"></script></body></html>",
            ),
            ("app.js", "console.log('hello');"),
            ("assets/logo.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
        ],
    )?;
    // A structurally invalid app (manifest id mismatch) must be skipped.
    write_app(
        &apps_dir,
        "broken",
        &[
            (
                "manifest.json",
                r#"{"id":"other","name":"Broken","version":"1","entry":"index.html"}"#,
            ),
            ("index.html", "broken"),
        ],
    )?;

    let client = Client::new();

    let list = client.get(format!("{base}/apps/{agent}")).send().await?;
    assert_eq!(list.status(), 200);
    let body: serde_json::Value = list.json().await?;
    let apps = body["apps"].as_array().expect("apps array");
    assert_eq!(apps.len(), 1, "invalid app should not be discoverable");
    assert_eq!(apps[0]["id"], "hello");
    assert_eq!(apps[0]["name"], "hello app");
    assert_eq!(apps[0]["url"], format!("/apps/{agent}/hello/"));

    let entry = client
        .get(format!("{base}/apps/{agent}/hello/"))
        .send()
        .await?;
    assert_eq!(entry.status(), 200);
    assert_eq!(
        entry
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        entry
            .headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert!(
        entry.headers().contains_key("content-security-policy"),
        "hosted apps should carry a CSP"
    );
    assert!(entry.text().await?.contains("Hello App"));

    // Entry is also reachable without the trailing slash.
    let entry_no_slash = client
        .get(format!("{base}/apps/{agent}/hello"))
        .send()
        .await?;
    assert_eq!(entry_no_slash.status(), 200);

    let js = client
        .get(format!("{base}/apps/{agent}/hello/app.js"))
        .send()
        .await?;
    assert_eq!(js.status(), 200);
    assert_eq!(
        js.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );

    let svg = client
        .get(format!("{base}/apps/{agent}/hello/assets/logo.svg"))
        .send()
        .await?;
    assert_eq!(svg.status(), 200);
    assert_eq!(
        svg.headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/svg+xml")
    );

    server.abort();
    Ok(())
}

pub async fn apps_allow_same_app_id_across_agents() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();

    let default_apps = agent_apps_dir(&host, &agent);
    write_app(
        &default_apps,
        "shared",
        &[
            ("manifest.json", &manifest("shared")),
            ("index.html", "default owner"),
        ],
    )?;
    let other_apps = agent_apps_dir(&host, "second-agent");
    write_app(
        &other_apps,
        "shared",
        &[
            ("manifest.json", &manifest("shared")),
            ("index.html", "second owner"),
        ],
    )?;

    let client = Client::new();
    let first = client
        .get(format!("{base}/apps/{agent}/shared/"))
        .send()
        .await?;
    assert_eq!(first.status(), 200);
    assert!(first.text().await?.contains("default owner"));

    let second = client
        .get(format!("{base}/apps/second-agent/shared/"))
        .send()
        .await?;
    assert_eq!(second.status(), 200);
    assert!(second.text().await?.contains("second owner"));

    // An app that only exists for another agent is not reachable here.
    let cross = client
        .get(format!("{base}/apps/second-agent/{agent}-only/"))
        .send()
        .await?;
    assert_eq!(cross.status(), 404);

    server.abort();
    Ok(())
}

pub async fn apps_reject_unknown_agent_and_app() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let client = Client::new();

    let unknown_app = client
        .get(format!("{base}/apps/{agent}/missing/"))
        .send()
        .await?;
    assert_eq!(unknown_app.status(), 404);

    // Unknown agents must not be confused with a valid agent with no apps.
    let unknown_agent_list = client
        .get(format!("{base}/apps/no-such-agent"))
        .send()
        .await?;
    assert_eq!(unknown_agent_list.status(), 404);

    let unknown_agent_entry = client
        .get(format!("{base}/apps/no-such-agent/app/"))
        .send()
        .await?;
    assert_eq!(unknown_agent_entry.status(), 404);

    server.abort();
    Ok(())
}

pub async fn apps_reject_invalid_manifest_and_missing_entry() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);

    write_app(
        &apps_dir,
        "mismatch",
        &[
            (
                "manifest.json",
                r#"{"id":"other","name":"M","version":"1","entry":"index.html"}"#,
            ),
            ("index.html", "body"),
        ],
    )?;
    write_app(
        &apps_dir,
        "missing-entry",
        &[(
            "manifest.json",
            r#"{"id":"missing-entry","name":"M","version":"1","entry":"gone.html"}"#,
        )],
    )?;

    let client = Client::new();
    let mismatch = client
        .get(format!("{base}/apps/{agent}/mismatch/"))
        .send()
        .await?;
    assert_eq!(mismatch.status(), 422);

    let missing_entry = client
        .get(format!("{base}/apps/{agent}/missing-entry/"))
        .send()
        .await?;
    assert_eq!(missing_entry.status(), 404);

    server.abort();
    Ok(())
}

pub async fn apps_reject_unsupported_asset_type() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);
    write_app(
        &apps_dir,
        "hello",
        &[
            ("manifest.json", &manifest("hello")),
            ("index.html", "hello"),
            ("run.sh", "echo nope"),
        ],
    )?;

    let response = Client::new()
        .get(format!("{base}/apps/{agent}/hello/run.sh"))
        .send()
        .await?;
    assert_eq!(response.status(), 415);

    server.abort();
    Ok(())
}

pub async fn apps_reject_path_traversal() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);
    write_app(
        &apps_dir,
        "hello",
        &[
            ("manifest.json", &manifest("hello")),
            ("index.html", "hello"),
        ],
    )?;
    // A sibling of the app root that must never be reachable through `..`.
    std::fs::write(apps_dir.join("secret.txt"), "top secret")?;

    let response = Client::new()
        .get(format!("{base}/apps/{agent}/hello/..%2Fsecret.txt"))
        .send()
        .await?;
    assert_eq!(response.status(), 403);
    assert!(!response.text().await?.contains("top secret"));

    server.abort();
    Ok(())
}

#[cfg(unix)]
pub async fn apps_reject_symlink_escape() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);
    write_app(
        &apps_dir,
        "hello",
        &[
            ("manifest.json", &manifest("hello")),
            ("index.html", "hello"),
        ],
    )?;
    let outside = host.config().data_dir.join("outside.txt");
    std::fs::write(&outside, "outside secret")?;
    std::os::unix::fs::symlink(&outside, apps_dir.join("hello").join("link.txt"))?;

    let response = Client::new()
        .get(format!("{base}/apps/{agent}/hello/link.txt"))
        .send()
        .await?;
    assert_eq!(response.status(), 403);
    assert!(!response.text().await?.contains("outside secret"));

    // A symlinked app directory must not let one agent serve another agent's app.
    let other_apps = agent_apps_dir(&host, "second-agent");
    write_app(
        &other_apps,
        "private",
        &[
            ("manifest.json", &manifest("private")),
            ("index.html", "second agent secret"),
        ],
    )?;
    std::os::unix::fs::symlink(other_apps.join("private"), apps_dir.join("linked"))?;
    let linked = Client::new()
        .get(format!("{base}/apps/{agent}/linked/"))
        .send()
        .await?;
    assert!(linked.status().is_client_error());

    // The apps root itself must not redirect one agent to another agent's apps.
    let root_link_agent = host.config().default_agent_id.clone();
    let root_link_apps = agent_apps_dir(&host, "root-link-target");
    write_app(
        &root_link_apps,
        "private",
        &[
            ("manifest.json", &manifest("private")),
            ("index.html", "root link secret"),
        ],
    )?;
    let real_apps = host.config().data_dir.join("real-apps-root");
    std::fs::rename(&agent_apps_dir(&host, &root_link_agent), &real_apps)?;
    std::os::unix::fs::symlink(&real_apps, agent_apps_dir(&host, &root_link_agent))?;
    let root_linked = Client::new()
        .get(format!("{base}/apps/{root_link_agent}/private/"))
        .send()
        .await?;
    assert_eq!(root_linked.status(), 403);

    server.abort();
    Ok(())
}

pub async fn apps_sdk_request_and_events() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);
    write_app(
        &apps_dir,
        "sdk",
        &[
            ("manifest.json", &manifest("sdk")),
            ("index.html", "<script src=\"holon.js\"></script>"),
        ],
    )?;
    let client = Client::new();

    let context: serde_json::Value = client
        .get(format!("{base}/apps/{agent}/sdk/context"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(context["sdk_version"], "1");
    assert_eq!(context["agent_id"], agent);
    assert_eq!(context["app_id"], "sdk");
    assert_eq!(context["session"]["authenticated"], true);

    let sdk = client
        .get(format!("{base}/apps/{agent}/sdk/holon.js"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    assert!(sdk.contains("window.Holon"));
    assert!(sdk.contains("request"));
    assert!(sdk.contains("events"));

    let event_response = client
        .get(format!("{base}/apps/{agent}/sdk/events"))
        .send()
        .await?
        .error_for_status()?;
    let mut event_stream = event_response.bytes_stream();
    let response: serde_json::Value = client
        .post(format!("{base}/apps/{agent}/sdk/request"))
        .json(&serde_json::json!({
            "version": "1",
            "request_id": "req-sdk-1",
            "request_type": "submit",
            "payload": {"text": "hello"}
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(response["ok"], true);
    assert_eq!(response["request_id"], "req-sdk-1");
    assert_eq!(response["status"], "accepted");
    assert_eq!(response["agent_id"], agent);
    assert_eq!(response["app_id"], "sdk");

    let event = timeout(Duration::from_secs(5), async {
        let mut body = String::new();
        while let Some(chunk) = event_stream.next().await {
            body.push_str(&String::from_utf8_lossy(&chunk?));
            if body.contains("req-sdk-1") {
                return Ok::<_, anyhow::Error>(body);
            }
        }
        anyhow::bail!("app event stream ended before request event")
    })
    .await??;
    assert!(event.contains("holon_event"));
    assert!(event.contains("\"app_id\":\"sdk\""));
    assert!(event.contains("\"request_id\":\"req-sdk-1\""));

    let invalid = client
        .post(format!("{base}/apps/{agent}/sdk/request"))
        .json(&serde_json::json!({
            "version": "999",
            "request_type": "submit",
            "payload": {}
        }))
        .send()
        .await?;
    assert_eq!(invalid.status(), 400);

    server.abort();
    Ok(())
}
