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

fn write_reference_app(apps_dir: &Path) -> Result<()> {
    write_app(
        apps_dir,
        "agent-workbench",
        &[
            (
                "manifest.json",
                include_str!("../../examples/local-app/agent-workbench/manifest.json"),
            ),
            (
                "index.html",
                include_str!("../../examples/local-app/agent-workbench/index.html"),
            ),
            (
                "app.js",
                include_str!("../../examples/local-app/agent-workbench/app.js"),
            ),
            (
                "styles.css",
                include_str!("../../examples/local-app/agent-workbench/styles.css"),
            ),
        ],
    )
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
        !entry.headers().contains_key("content-security-policy"),
        "hosted apps should not impose a CSP by default"
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

    let mismatch_context = client
        .get(format!("{base}/apps/{agent}/mismatch/context"))
        .send()
        .await?;
    assert_eq!(mismatch_context.status(), 422);

    let mismatch_request = client
        .post(format!("{base}/apps/{agent}/mismatch/request"))
        .json(&serde_json::json!({
            "version": "1",
            "request_type": "submit",
            "payload": {}
        }))
        .send()
        .await?;
    assert_eq!(mismatch_request.status(), 422);

    let mismatch_events = client
        .get(format!("{base}/apps/{agent}/mismatch/events"))
        .send()
        .await?;
    assert_eq!(mismatch_events.status(), 422);

    let mismatch_sdk = client
        .get(format!("{base}/apps/{agent}/mismatch/holon.js"))
        .send()
        .await?;
    assert_eq!(mismatch_sdk.status(), 422);

    let missing_entry = client
        .get(format!("{base}/apps/{agent}/missing-entry/"))
        .send()
        .await?;
    assert_eq!(missing_entry.status(), 404);

    server.abort();
    Ok(())
}

pub async fn apps_serve_arbitrary_asset_types() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);
    write_app(
        &apps_dir,
        "hello",
        &[
            ("manifest.json", &manifest("hello")),
            ("index.html", "hello"),
            ("run.sh", "echo not-executed"),
            ("Build/game.wasm", "\0asm\u{1}\0\0\0"),
            ("Build/game.data", "game-data"),
            ("asset.unrecognized", "opaque"),
            ("LICENSE", "no extension"),
        ],
    )?;

    for (name, mime, bytes) in [
        ("run.sh", "application/x-sh", "echo not-executed"),
        ("Build/game.wasm", "application/wasm", "\0asm\u{1}\0\0\0"),
        ("Build/game.data", "application/octet-stream", "game-data"),
        ("asset.unrecognized", "application/octet-stream", "opaque"),
        ("LICENSE", "application/octet-stream", "no extension"),
    ] {
        let response = Client::new()
            .get(format!("{base}/apps/{agent}/hello/{name}"))
            .send()
            .await?;
        assert_eq!(response.status(), 200, "{name}");
        assert_eq!(response.headers()[CONTENT_TYPE], mime, "{name}");
        assert_eq!(response.text().await?, bytes);
    }

    server.abort();
    Ok(())
}

pub async fn apps_manifest_defaults_and_invalid_files() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps = agent_apps_dir(&host, &agent);
    write_app(&apps, "plain", &[("index.html", "zero-config")])?;
    write_app(
        &apps,
        "broken-json",
        &[("index.html", "no fallback"), ("manifest.json", "{")],
    )?;
    write_app(&apps, "no-index", &[("other.html", "not guessed")])?;
    write_app(&apps, "manifest-directory", &[("index.html", "not usable")])?;
    std::fs::create_dir(apps.join("manifest-directory/manifest.json"))?;
    write_app(
        &apps,
        "huge-manifest",
        &[
            ("index.html", "not usable"),
            ("manifest.json", &" ".repeat(65537)),
        ],
    )?;
    write_app(
        &apps,
        "custom",
        &[
            (
                "manifest.json",
                r#"{"id":"custom","name":"Custom title","version":"2","entry":"pages/start.html"}"#,
            ),
            ("pages/start.html", "custom entry"),
        ],
    )?;
    let client = Client::new();
    let list: serde_json::Value = client
        .get(format!("{base}/apps/{agent}"))
        .send()
        .await?
        .json()
        .await?;
    let found = list["apps"].as_array().unwrap();
    assert_eq!(found.len(), 2, "{list}");
    let plain = found.iter().find(|app| app["id"] == "plain").unwrap();
    assert_eq!(plain["name"], "plain");
    assert_eq!(plain["version"], "0.0.0");
    assert_eq!(plain["entry"], "index.html");
    assert!(plain["description"].is_null());
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/plain/"))
            .send()
            .await?
            .text()
            .await?,
        "zero-config"
    );
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/custom/"))
            .send()
            .await?
            .text()
            .await?,
        "custom entry"
    );
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/plain/context"))
            .send()
            .await?
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/plain/holon.js"))
            .send()
            .await?
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/plain/manifest.json"))
            .send()
            .await?
            .status(),
        404
    );
    assert!(
        !apps.join("plain/manifest.json").exists(),
        "GET must not write a manifest"
    );
    for id in ["broken-json", "manifest-directory", "huge-manifest"] {
        for path in ["", "context", "index.html", "holon.js"] {
            assert_eq!(
                client
                    .get(format!("{base}/apps/{agent}/{id}/{path}"))
                    .send()
                    .await?
                    .status(),
                422,
                "{id}/{path}"
            );
        }
    }
    for path in ["", "context", "other.html", "holon.js"] {
        assert_eq!(
            client
                .get(format!("{base}/apps/{agent}/no-index/{path}"))
                .send()
                .await?
                .status(),
            404
        );
    }
    let valid = manifest("boundary");
    let exact_manifest = format!("{valid}{}", " ".repeat(65536 - valid.len()));
    write_app(
        &apps,
        "boundary",
        &[
            ("manifest.json", &exact_manifest),
            ("index.html", "boundary"),
        ],
    )?;
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/boundary/"))
            .send()
            .await?
            .status(),
        200
    );
    std::fs::write(
        apps.join("boundary/manifest.json"),
        format!("{exact_manifest} "),
    )?;
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/boundary/"))
            .send()
            .await?
            .status(),
        422
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        for id in ["dangling", "escaped", "default-escape"] {
            write_app(&apps, id, &[("index.html", "no fallback")])?;
        }
        symlink("missing.json", apps.join("dangling/manifest.json"))?;
        // Even valid JSON outside this app must not be read.
        std::fs::write(apps.join("external.json"), manifest("escaped"))?;
        symlink("../external.json", apps.join("escaped/manifest.json"))?;
        std::fs::remove_file(apps.join("default-escape/index.html"))?;
        symlink(
            "../plain/index.html",
            apps.join("default-escape/index.html"),
        )?;
        for id in ["dangling", "escaped"] {
            assert_eq!(
                client
                    .get(format!("{base}/apps/{agent}/{id}/"))
                    .send()
                    .await?
                    .status(),
                422
            );
        }
        assert_eq!(
            client
                .get(format!("{base}/apps/{agent}/default-escape/context"))
                .send()
                .await?
                .status(),
            403
        );
        // Links within the app remain usable, but cannot expose hidden files.
        write_app(
            &apps,
            "plain",
            &[(".env", "secret"), (".git/config", "secret")],
        )?;
        symlink(".env", apps.join("plain/public.txt"))?;
        for path in [".env", ".git/config", "public.txt"] {
            assert_eq!(
                client
                    .get(format!("{base}/apps/{agent}/plain/{path}"))
                    .send()
                    .await?
                    .status(),
                400
            );
        }
        symlink("index.html", apps.join("plain/alias.html"))?;
        assert_eq!(
            client
                .get(format!("{base}/apps/{agent}/plain/alias.html"))
                .send()
                .await?
                .status(),
            200
        );
    }
    server.abort();
    Ok(())
}

pub async fn apps_static_http_and_size_settings() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps = agent_apps_dir(&host, &agent);
    write_app(
        &apps,
        "game",
        &[
            ("index.html", "game"),
            ("payload.data", "0123456789"),
            ("empty", ""),
        ],
    )?;
    let client = Client::new();
    let url = format!("{base}/apps/{agent}/game/payload.data");
    let full = client.get(&url).send().await?;
    assert_eq!(full.status(), 200);
    assert_eq!(full.headers()["cache-control"], "no-store");
    assert_eq!(full.headers()["x-content-type-options"], "nosniff");
    let etag = full.headers()["etag"].clone();
    let modified = full.headers()["last-modified"].clone();
    assert_eq!(full.text().await?, "0123456789");
    for (range, expected) in [
        ("bytes=2-4", "234"),
        ("bytes=8-", "89"),
        ("bytes=-3", "789"),
    ] {
        let response = client.get(&url).header("range", range).send().await?;
        assert_eq!(response.status(), 206);
        assert_eq!(response.text().await?, expected);
    }
    let partial = client.get(&url).header("range", "bytes=2-4").send().await?;
    assert_eq!(partial.headers()["content-range"], "bytes 2-4/10");
    assert_eq!(partial.headers()["content-length"], "3");
    assert_eq!(partial.text().await?, "234");
    for validator in [etag.clone(), modified] {
        assert_eq!(
            client
                .get(&url)
                .header("range", "bytes=0-1")
                .header("if-range", validator)
                .send()
                .await?
                .status(),
            206
        );
    }
    assert_eq!(
        client
            .get(&url)
            .header("range", "bytes=0-1")
            .header("if-range", "\"stale\"")
            .send()
            .await?
            .status(),
        200
    );
    for _ in 0..20 {
        let unchanged = client
            .get(&url)
            .header("if-none-match", &etag)
            .send()
            .await?;
        assert_eq!(unchanged.status(), 304);
        assert_eq!(unchanged.headers()["cache-control"], "no-store");
        assert!(!unchanged.headers().contains_key("content-security-policy"));
        assert!(unchanged.bytes().await?.is_empty());
        let invalid = client
            .get(&url)
            .header("range", "bytes=100-")
            .send()
            .await?;
        assert_eq!(invalid.status(), 416);
        assert_eq!(invalid.headers()["content-range"], "bytes */10");
        assert_eq!(invalid.headers()["cache-control"], "no-store");
        let head = client
            .head(&url)
            .header("range", "bytes=0-1")
            .send()
            .await?;
        assert_eq!(head.status(), 200, "HEAD ignores GET-only Range");
        assert_eq!(head.headers()["content-length"], "10");
        assert!(head.bytes().await?.is_empty());
    }
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/game/empty"))
            .send()
            .await?
            .content_length(),
        Some(0)
    );
    let limit = 64 * 1024 * 1024;
    // Exercise a full transfer larger than the old 8 MiB cap, using the
    // reported Unity WASM byte size without committing a game binary.
    std::fs::File::create(apps.join("game/unity-sized.wasm"))?.set_len(15_110_911)?;
    let unity_sized = client
        .get(format!("{base}/apps/{agent}/game/unity-sized.wasm"))
        .send()
        .await?;
    assert_eq!(unity_sized.status(), 200);
    assert_eq!(unity_sized.headers()[CONTENT_TYPE], "application/wasm");
    assert_eq!(unity_sized.bytes().await?.len(), 15_110_911);
    for (name, len) in [("at-limit.wasm", limit), ("too-big.wasm", limit + 1)] {
        std::fs::File::create(apps.join("game").join(name))?.set_len(len)?;
    }
    let allowed = format!("{base}/apps/{agent}/game/at-limit.wasm");
    let head = client.head(&allowed).send().await?;
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], limit.to_string());
    assert_eq!(head.headers()[CONTENT_TYPE], "application/wasm");
    let range = client
        .get(&allowed)
        .header("range", "bytes=0-7")
        .send()
        .await?;
    assert_eq!(range.status(), 206);
    assert_eq!(range.bytes().await?.as_ref(), &[0; 8]);
    let rejected = format!("{base}/apps/{agent}/game/too-big.wasm");
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        assert_eq!(
            client
                .request(method, &rejected)
                .header("range", "bytes=0-7")
                .send()
                .await?
                .status(),
            413
        );
    }
    server.abort();

    let test = super::TestConfigBuilder::new().build();
    let mut config = test.config().clone();
    config.local_apps.max_asset_bytes = Some(limit + 1);
    config.local_apps.csp = Some("default-src 'self'; script-src 'self' 'wasm-unsafe-eval'".into());
    let (host, base, server) = super::spawn_server_with_config(config).await?;
    let agent = host.config().default_agent_id.clone();
    let apps = agent_apps_dir(&host, &agent);
    write_app(&apps, "game", &[("index.html", "game")])?;
    std::fs::File::create(apps.join("game/big.data"))?.set_len(limit + 1)?;
    let url = format!("{base}/apps/{agent}/game/big.data");
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        let response = client
            .request(method, &url)
            .header("range", "bytes=0-0")
            .send()
            .await?;
        assert!(response.status().is_success());
        assert_eq!(
            response.headers()["content-security-policy"],
            host.config().local_apps.csp.as_deref().unwrap()
        );
    }
    let response = client
        .get(&url)
        .header("range", "bytes=999999999-")
        .send()
        .await?;
    assert_eq!(response.status(), 416);
    assert!(response.headers().contains_key("content-security-policy"));
    server.abort();

    let small_test = super::TestConfigBuilder::new().build();
    let mut config = small_test.config().clone();
    config.local_apps.max_asset_bytes = Some(4);
    let (host, base, server) = super::spawn_server_with_config(config).await?;
    let agent = host.config().default_agent_id.clone();
    let apps = agent_apps_dir(&host, &agent);
    write_app(
        &apps,
        "small",
        &[("index.html", "1234"), ("more.data", "12345")],
    )?;
    assert_eq!(
        client
            .get(format!("{base}/apps/{agent}/small/"))
            .send()
            .await?
            .status(),
        200
    );
    let response = client
        .get(format!("{base}/apps/{agent}/small/more.data"))
        .send()
        .await?;
    assert_eq!(response.status(), 413);
    assert_eq!(
        response.json::<serde_json::Value>().await?["code"],
        "app_asset_too_large"
    );
    server.abort();
    Ok(())
}

pub async fn apps_session_boundary_for_static_assets() -> Result<()> {
    let test = super::TestConfigBuilder::new()
        .with_control_auth_mode(holon::config::ControlAuthMode::Required)
        .with_control_token("secret")
        .build();
    let (host, base, server) = super::spawn_server_with_config(test.config().clone()).await?;
    let agent = host.config().default_agent_id.clone();
    write_app(
        &agent_apps_dir(&host, &agent),
        "game",
        &[
            ("index.html", "game"),
            ("game.wasm", "\0asm\u{1}\0\0\0"),
            ("game.data", "data"),
        ],
    )?;
    let client = Client::new();
    let session: serde_json::Value = client
        .post(format!("{base}/api/auth/session/exchange/native"))
        .json(&serde_json::json!({"credential":"secret"}))
        .send()
        .await?
        .json()
        .await?;
    let credential = session["credential"]
        .as_str()
        .expect("real session credential");
    for path in [
        "",
        "/game/",
        "/game/game.wasm",
        "/game/game.data",
        "/game/context",
        "/game/holon.js",
        "/game/events",
    ] {
        let url = format!("{base}/apps/{agent}{path}");
        assert_eq!(client.get(&url).send().await?.status(), 401, "{path}");
        assert_eq!(
            client
                .get(&url)
                .bearer_auth(credential)
                .send()
                .await?
                .status(),
            200,
            "{path}"
        );
    }
    assert_eq!(
        client
            .head(format!("{base}/apps/{agent}/game/game.wasm"))
            .send()
            .await?
            .status(),
        401
    );
    server.abort();
    Ok(())
}

pub async fn apps_asset_capacity_shared_and_released() -> Result<()> {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let (host, _, server) = spawn_server().await?;
    for agent in ["default", "other"] {
        for app in ["first", "second"] {
            write_app(
                &agent_apps_dir(&host, agent),
                app,
                &[("index.html", "body")],
            )?;
        }
    }
    let router = holon::http::router(holon::http::AppState::for_tcp(host));
    let get = |path: &str| Request::builder().uri(path).body(Body::empty()).unwrap();
    let mut responses = Vec::new();
    for agent in ["default", "other"] {
        for app in ["first", "second"] {
            for _ in 0..4 {
                let response = router
                    .clone()
                    .oneshot(get(&format!("/apps/{agent}/{app}/")))
                    .await?;
                assert_eq!(response.status(), 200);
                responses.push(response);
            }
        }
    }
    let busy = router.clone().oneshot(get("/apps/default/first/")).await?;
    assert_eq!(busy.status(), 503);
    assert_eq!(busy.headers()["retry-after"], "1");
    let error: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(busy.into_body(), 4096).await?)?;
    assert_eq!(error["code"], "app_asset_busy");
    for path in [
        "/apps/default/first/context",
        "/apps/default/first/holon.js",
        "/apps/default",
    ] {
        assert_eq!(
            router.clone().oneshot(get(path)).await?.status(),
            200,
            "asset capacity must not block SDK/discovery"
        );
    }
    drop(responses.pop());
    // Bodyless replies and errors must not consume the just-released slot.
    for _ in 0..20 {
        let head = Request::builder()
            .method("HEAD")
            .uri("/apps/default/first/")
            .body(Body::empty())?;
        assert_eq!(router.clone().oneshot(head).await?.status(), 200);
        let range = Request::builder()
            .uri("/apps/default/first/")
            .header("range", "bytes=100-")
            .body(Body::empty())?;
        assert_eq!(router.clone().oneshot(range).await?.status(), 416);
        assert_eq!(
            router
                .clone()
                .oneshot(get("/apps/default/first/missing"))
                .await?
                .status(),
            404
        );
    }
    let response = router.clone().oneshot(get("/apps/default/first/")).await?;
    let bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
    assert_eq!(&bytes[..], b"body");
    assert_eq!(
        router.oneshot(get("/apps/other/second/")).await?.status(),
        200
    );
    drop(responses);
    server.abort();
    Ok(())
}

pub async fn apps_settings_follow_existing_config_reload() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps = agent_apps_dir(&host, &agent);
    write_app(
        &apps,
        "settings",
        &[("index.html", "1234"), ("asset.data", "12345")],
    )?;
    let client = Client::new();
    let asset_url = format!("{base}/apps/{agent}/settings/asset.data");
    assert_eq!(client.get(&asset_url).send().await?.status(), 200);
    let policy = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'";
    // Reload rebuilds providers instead of retaining the injected StubProvider.
    // Use a credential-free local route; this test never sends a model request.
    let update: serde_json::Value = client
        .patch(format!("{base}/api/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({"updates":[
            {"key":"local_apps.max_asset_bytes","value":4},
            {"key":"local_apps.csp","value":policy},
            {"key":"model.default","value":"ollama/qwen3:latest"}
        ]}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(update["changed"], true, "{update}");
    assert_eq!(update["results"][0]["effect"], "accepted_reload_scheduled");
    timeout(Duration::from_secs(5), async {
        loop {
            let config = host.config();
            if config.local_apps.max_asset_bytes() == 4
                && config.local_apps.csp.as_deref() == Some(policy)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("reload did not apply: {:?}", host.config_reload_status()))?;
    let entry = client
        .get(format!("{base}/apps/{agent}/settings/"))
        .send()
        .await?;
    assert_eq!(entry.status(), 200);
    assert_eq!(entry.headers()["content-security-policy"], policy);
    assert_eq!(client.get(&asset_url).send().await?.status(), 413);
    let invalid: serde_json::Value = client
        .patch(format!("{base}/api/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({"updates":[
            {"key":"local_apps.max_asset_bytes","value":99},
            {"key":"local_apps.csp","value":"invalid\r\nInjected: yes"}
        ]}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(invalid["changed"], false);
    let stored = holon::config::load_persisted_config_at(&host.config().config_file_path)?;
    assert_eq!(
        stored.local_apps.max_asset_bytes(),
        4,
        "invalid batch must be atomic"
    );
    assert_eq!(stored.local_apps.csp.as_deref(), Some(policy));
    let reset: serde_json::Value = client
        .patch(format!("{base}/api/control/runtime/config"))
        .bearer_auth("secret")
        .json(&serde_json::json!({"updates":[
            {"key":"local_apps.max_asset_bytes","unset":true},
            {"key":"local_apps.csp","unset":true}
        ]}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(reset["changed"], true, "{reset}");
    timeout(Duration::from_secs(5), async {
        while host.config().local_apps.max_asset_bytes() != 64 * 1024 * 1024
            || host.config().local_apps.csp.is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let response = client.get(&asset_url).send().await?;
    assert_eq!(response.status(), 200);
    assert!(!response.headers().contains_key("content-security-policy"));
    server.abort();
    host.shutdown().await?;
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
    assert_eq!(
        context["session"].as_object().map(|session| session.len()),
        Some(1)
    );
    assert!(context["session"].get("permissions").is_none());

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

pub async fn apps_sdk_event_replay_order() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    let apps_dir = agent_apps_dir(&host, &agent);
    write_app(
        &apps_dir,
        "sdk-replay",
        &[
            ("manifest.json", &manifest("sdk-replay")),
            ("index.html", "replay"),
        ],
    )?;
    let client = Client::new();

    for request_id in ["req-replay-1", "req-replay-2"] {
        client
            .post(format!("{base}/apps/{agent}/sdk-replay/request"))
            .json(&serde_json::json!({
                "version": "1",
                "request_id": request_id,
                "request_type": "submit",
                "payload": {"text": request_id}
            }))
            .send()
            .await?
            .error_for_status()?;
    }

    let response = client
        .get(format!("{base}/apps/{agent}/sdk-replay/events"))
        .header("Last-Event-ID", "0")
        .send()
        .await?
        .error_for_status()?;
    let mut stream = response.bytes_stream();
    let body = timeout(Duration::from_secs(5), async {
        let mut body = String::new();
        while let Some(chunk) = stream.next().await {
            body.push_str(&String::from_utf8_lossy(&chunk?));
            if body.contains("req-replay-1") && body.contains("req-replay-2") {
                return Ok::<_, anyhow::Error>(body);
            }
        }
        anyhow::bail!("app event replay ended before both request events")
    })
    .await??;

    let first = body
        .find("req-replay-1")
        .expect("first replayed request should be present");
    let second = body
        .find("req-replay-2")
        .expect("second replayed request should be present");
    assert!(first < second, "replayed events should remain oldest-first");

    server.abort();
    Ok(())
}

pub async fn apps_reference_workbench_end_to_end() -> Result<()> {
    let (host, base, server) = spawn_server().await?;
    let agent = host.config().default_agent_id.clone();
    write_reference_app(&agent_apps_dir(&host, &agent))?;
    let client = Client::new();

    let discovery: serde_json::Value = client
        .get(format!("{base}/apps/{agent}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(discovery["apps"][0]["id"], "agent-workbench");
    assert_eq!(discovery["apps"][0]["entry"], "index.html");

    let entry = client
        .get(format!("{base}/apps/{agent}/agent-workbench/"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        entry
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let entry_body = entry.text().await?;
    assert!(entry_body.contains("Agent Workbench"));
    assert!(entry_body.contains("holon.js"));

    let script = client
        .get(format!("{base}/apps/{agent}/agent-workbench/app.js"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    assert!(script.contains("Holon.context()"));
    assert!(script.contains("Holon.request"));
    assert!(script.contains("Holon.events"));

    let context: serde_json::Value = client
        .get(format!("{base}/apps/{agent}/agent-workbench/context"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(context["agent_id"], agent);
    assert_eq!(context["app_id"], "agent-workbench");

    let event_response = client
        .get(format!("{base}/apps/{agent}/agent-workbench/events"))
        .header("Last-Event-ID", "0")
        .send()
        .await?
        .error_for_status()?;
    let mut event_stream = event_response.bytes_stream();

    let response: serde_json::Value = client
        .post(format!("{base}/apps/{agent}/agent-workbench/request"))
        .json(&serde_json::json!({
            "version": "1",
            "request_id": "workbench-e2e",
            "request_type": "message",
            "payload": {"text": "hello from the reference app"}
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(response["ok"], true);
    assert_eq!(response["request_id"], "workbench-e2e");
    assert_eq!(response["app_id"], "agent-workbench");

    let events = timeout(Duration::from_secs(5), async {
        let mut body = String::new();
        while let Some(chunk) = event_stream.next().await {
            body.push_str(&String::from_utf8_lossy(&chunk?));
            if body.contains("workbench-e2e") {
                return Ok::<_, anyhow::Error>(body);
            }
        }
        anyhow::bail!("reference app event stream ended before the request event")
    })
    .await??;
    assert!(events.contains("holon_event"));
    assert!(events.contains("\"app_id\":\"agent-workbench\""));
    assert!(events.contains("\"request_id\":\"workbench-e2e\""));

    server.abort();
    Ok(())
}
