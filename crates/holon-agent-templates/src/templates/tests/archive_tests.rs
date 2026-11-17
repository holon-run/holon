use super::super::test_support::{
    archive_base, archive_files, archive_route, archive_server, REVIEWER_MANIFEST, WORKER_MANIFEST,
};
use std::{env, fs};

use tempfile::tempdir;

use super::super::*;
use super::{lock_env, EnvGuard, MockGithubServer, TEST_TEMPLATES};

#[tokio::test]
async fn initialize_agent_home_from_github_url_works_with_archive() {
    let _lock = lock_env();
    let home = tempdir().unwrap();
    let server = archive_server(
        "main",
        &[("templates/reviewer/AGENTS.md", "# Public reviewer rules\n")],
    );
    let _base = archive_base(&server);

    let agent_home = home.path().join("agent");
    let provenance = initialize_agent_home_from_template(
        TEST_TEMPLATES,
        &agent_home,
        "https://github.com/owner/repo/tree/main/templates/reviewer",
    )
    .await
    .unwrap();

    assert!(matches!(
        provenance.source,
        TemplateProvenanceSource::GitHubUrl { .. }
    ));
    let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
    assert!(agents_md.contains("Public reviewer rules"));
    assert!(agents_md.contains("## Holon Agent Home"));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/owner/repo/tar.gz/main");
}

#[tokio::test]
async fn discover_github_repo_templates_reads_one_archive_without_metadata_or_contents() {
    let _lock = lock_env();
    let server = archive_server(
        "main",
        &[
            ("agent_templates/worker/template.toml", WORKER_MANIFEST),
            ("agent_templates/worker/AGENTS.md", "worker"),
            ("agent_templates/reviewer/template.toml", REVIEWER_MANIFEST),
            ("README.md", "not a template"),
        ],
    );
    let _base = archive_base(&server);

    let entries = discover_github_repo_templates(TEST_TEMPLATES, "owner", "repo", "main")
        .await
        .unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].catalog_id, "remote:owner/repo/reviewer");
    assert_eq!(entries[1].catalog_id, "remote:owner/repo/worker");
    assert!(entries
        .iter()
        .all(|entry| entry.source == AgentTemplateSourceKind::Remote));
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.requests()[0].path, "/owner/repo/tar.gz/main");
}

#[tokio::test]
async fn discover_github_repo_templates_skips_invalid_and_missing_archive_templates() {
    let _lock = lock_env();
    let server = archive_server(
        "main",
        &[
            ("agent_templates/good/template.toml", WORKER_MANIFEST),
            ("agent_templates/good/AGENTS.md", "good"),
            ("agent_templates/bad/AGENTS.md", "missing manifest"),
            ("agent_templates/empty/template.toml", "not toml"),
        ],
    );
    let _base = archive_base(&server);

    let entries = discover_github_repo_templates(TEST_TEMPLATES, "owner", "repo", "main")
        .await
        .unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].template_id, "good");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn resolve_remote_agent_template_detail_reads_archive_tree() {
    let _lock = lock_env();
    let server = archive_server(
        "main",
        &[
            ("agent_templates/worker/template.toml", WORKER_MANIFEST),
            ("agent_templates/worker/AGENTS.md", "# Worker\nDoes worker things\n"),
            (
                "agent_templates/worker/skills.toml",
                "[[skills]]\nkind = \"github\"\nrepo = \"holon-run/holon\"\npath = \"skills/ghx\"\nref = \"main\"\n",
            ),
        ],
    );
    let _base = archive_base(&server);
    let entry = AgentTemplateCatalogEntry {
        catalog_id: "remote:community:worker".into(),
        template: "remote:community:worker".into(),
        template_id: "worker".into(),
        source: AgentTemplateSourceKind::Remote,
        path: None,
        name: "Worker".into(),
        schema_version: None,
        description: "Does worker things".into(),
        included_skills: vec!["holon-run/holon/skills/ghx@main".into()],
        source_id: Some("community".into()),
        resolved_ref: Some("main".into()),
        resolved_revision: None,
        source_url: Some("https://github.com/owner/repo/tree/main/agent_templates/worker".into()),
    };

    let detail = resolve_remote_agent_template_detail(TEST_TEMPLATES, &entry)
        .await
        .unwrap();
    assert_eq!(detail.catalog_id, "remote:community:worker");
    assert_eq!(
        detail.schema_version.as_deref(),
        Some("holon.agent_template.v1")
    );
    assert!(detail.agents_md.contains("Does worker things"));
    assert_eq!(detail.skills.len(), 1);
    assert_eq!(
        detail.skills[0].reference,
        "holon-run/holon/skills/ghx@main"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn remote_archive_requests_use_env_github_token() {
    let _lock = lock_env();
    let server = archive_server("main", &archive_files("# Worker\n"));
    let _base = archive_base(&server);
    let _token = EnvGuard::set("HOLON_GITHUB_TOKEN", "env-token".into());

    resolve_github_template(
        TEST_TEMPLATES,
        "https://github.com/owner/repo/tree/main/agent_templates/worker",
    )
    .await
    .unwrap();

    assert_eq!(
        server.requests()[0].authorization.as_deref(),
        Some("Bearer env-token")
    );
}

#[tokio::test]
#[cfg(unix)]
async fn remote_archive_uses_gh_cli_token_when_environment_is_empty() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = lock_env();
    let home = tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\n[ \"$1 $2 $3 $4\" = \"auth token -h github.com\" ] && printf gh-token\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let server = archive_server("main", &archive_files("# Worker\n"));
    let _base = archive_base(&server);
    let _h = EnvGuard::set("HOME", home.path().display().to_string());
    let _path = EnvGuard::set(
        "PATH",
        format!("{}:{}", bin.display(), env::var("PATH").unwrap_or_default()),
    );
    let _a = EnvGuard::remove("HOLON_GITHUB_TOKEN");
    let _b = EnvGuard::remove("GITHUB_TOKEN");
    let _c = EnvGuard::remove("GH_TOKEN");

    resolve_github_template(
        TEST_TEMPLATES,
        "https://github.com/owner/repo/tree/main/agent_templates/worker",
    )
    .await
    .unwrap();

    assert_eq!(
        server.requests()[0].authorization.as_deref(),
        Some("Bearer gh-token")
    );
}

#[test]
fn explicit_template_install_uses_archive_and_preserves_auth() {
    let _lock = lock_env();
    let server = archive_server("main", &[("AGENTS.md", "# Explicit\n")]);
    let base = format!("http://{}", server.addr);
    let client = BlockingClient::new(
        reqwest::blocking::Client::new(),
        GitHubAuth::resolve_blocking(Some("test-token".into()), &[]),
        &base,
    )
    .unwrap();
    let destination = tempdir().unwrap();

    let template_id = install_template_from_github_with_client(
        TEST_TEMPLATES,
        destination.path(),
        "https://github.com/owner/repo/tree/main",
        &client,
        &base,
    )
    .unwrap();

    assert_eq!(template_id, "repo-snapshot");
    assert_eq!(
        fs::read_to_string(
            templates_root_for_home(destination.path())
                .join("repo-snapshot")
                .join(TEMPLATE_AGENTS_FILENAME)
        )
        .unwrap(),
        "# Explicit\n"
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.requests()[0].path, "/owner/repo/tar.gz/main");
    assert_eq!(
        server.requests()[0].authorization.as_deref(),
        Some("Bearer test-token")
    );
}

#[test]
fn explicit_template_install_probes_slash_ref_then_template_path() {
    let _lock = lock_env();
    let route = archive_route("feature/topic", &[("template/AGENTS.md", "# Slash ref\n")]);
    let server = MockGithubServer::start_bytes(vec![route]);
    let base = format!("http://{}", server.addr);
    let client = BlockingClient::new(
        reqwest::blocking::Client::new(),
        GitHubAuth::anonymous(),
        &base,
    )
    .unwrap();
    let destination = tempdir().unwrap();

    install_template_from_github_with_client(
        TEST_TEMPLATES,
        destination.path(),
        "https://github.com/owner/repo/tree/feature/topic/template",
        &client,
        &base,
    )
    .unwrap();

    let paths = server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            "/owner/repo/tar.gz/feature%2Ftopic%2Ftemplate",
            "/owner/repo/tar.gz/feature%2Ftopic",
        ]
    );
}

#[test]
fn explicit_template_install_rate_limit_stops_ref_probing() {
    let _lock = lock_env();
    let path = Box::leak(
        "/owner/repo/tar.gz/feature%2Ftopic%2Ftemplate"
            .to_string()
            .into_boxed_str(),
    );
    let server = MockGithubServer::start_bytes(vec![(path, 429, b"{}".to_vec())]);
    let base = format!("http://{}", server.addr);
    let client = BlockingClient::new(
        reqwest::blocking::Client::new(),
        GitHubAuth::anonymous(),
        &base,
    )
    .unwrap();
    let error = install_template_from_github_with_client(
        TEST_TEMPLATES,
        tempdir().unwrap().path(),
        "https://github.com/owner/repo/tree/feature/topic/template",
        &client,
        &base,
    )
    .unwrap_err();

    assert!(
        format!("{error:#}").contains("HTTP 429"),
        "unexpected archive failure: {error:#}"
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn explicit_template_install_ordinary_404s_probe_all_slash_ref_candidates() {
    let _lock = lock_env();
    let server = MockGithubServer::start_bytes(vec![]);
    let base = format!("http://{}", server.addr);
    let client = BlockingClient::new(
        reqwest::blocking::Client::new(),
        GitHubAuth::anonymous(),
        &base,
    )
    .unwrap();
    let error = install_template_from_github_with_client(
        TEST_TEMPLATES,
        tempdir().unwrap().path(),
        "https://github.com/owner/repo/tree/feature/topic/template",
        &client,
        &base,
    )
    .unwrap_err();

    assert!(error
        .downcast_ref::<holon_github::archive::ArchiveHttpFailure>()
        .is_none());
    assert!(format!("{error:#}").contains("no valid ref/path"));
    let paths = server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            "/owner/repo/tar.gz/feature%2Ftopic%2Ftemplate",
            "/owner/repo/tar.gz/feature%2Ftopic",
            "/owner/repo/tar.gz/feature",
        ]
    );
}

#[tokio::test]
#[cfg(unix)]
async fn anonymous_archive_resolution_succeeds_when_gh_is_missing_or_fails() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = lock_env();
    let home = tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let _h = EnvGuard::set("HOME", home.path().display().to_string());
    let _path = EnvGuard::set("PATH", bin.display().to_string());
    let _a = EnvGuard::remove("HOLON_GITHUB_TOKEN");
    let _b = EnvGuard::remove("GITHUB_TOKEN");
    let _c = EnvGuard::remove("GH_TOKEN");

    for remove_gh in [false, true] {
        if remove_gh {
            fs::remove_file(&gh).unwrap();
        }
        let server = archive_server("main", &archive_files("# Public\n"));
        let _base = archive_base(&server);
        resolve_github_template(
            TEST_TEMPLATES,
            "https://github.com/owner/repo/tree/main/agent_templates/worker",
        )
        .await
        .unwrap();
        assert_eq!(server.requests().len(), 1);
        assert!(server.requests()[0].authorization.is_none());
    }
}
