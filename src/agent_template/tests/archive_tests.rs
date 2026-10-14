use std::{collections::BTreeMap, env, fs, io::Cursor};

use flate2::{write::GzEncoder, Compression};
use tar::{Builder, EntryType, Header};
use tempfile::{tempdir, TempDir};

use super::super::*;
use super::{EnvGuard, MockGithubServer};
use crate::config::{save_credential_store_at, CredentialProfileFile, CredentialStoreFile};

const WORKER_MANIFEST: &str = r#"schema = "holon.agent_template.v1"
id = "worker"
name = "Worker"
summary = "Implementation agent"
"#;
const REVIEWER_MANIFEST: &str = r#"schema = "holon.agent_template.v1"
id = "reviewer"
name = "Reviewer"
summary = "Review agent"
"#;

fn archive_bytes(root: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let mut compressed = Vec::new();
    {
        let encoder = GzEncoder::new(&mut compressed, Compression::default());
        let mut builder = Builder::new(encoder);
        // GitHub codeload archives include repository metadata before the content root.
        let metadata = b"52 comment=34b7e2cc93dc428f8c1b4bf696de6dcfe7373436\n";
        let mut header = Header::new_gnu();
        header.set_path("pax_global_header").unwrap();
        header.set_entry_type(EntryType::XGlobalHeader);
        header.set_size(metadata.len() as u64);
        header.set_cksum();
        builder.append(&header, Cursor::new(metadata)).unwrap();
        for (path, contents) in files {
            let full_path = format!("{root}/{path}");
            let mut header = Header::new_gnu();
            header.set_path(full_path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_cksum();
            builder
                .append(&header, Cursor::new(contents.as_bytes()))
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }
    compressed
}

fn archive_route(reference: &str, files: &[(&str, &str)]) -> (&'static str, u16, Vec<u8>) {
    let mut url = reqwest::Url::parse("http://archive.invalid").unwrap();
    url.path_segments_mut()
        .unwrap()
        .push("owner")
        .push("repo")
        .push("tar.gz")
        .push(reference);
    let path = Box::leak(url.path().to_owned().into_boxed_str());
    (path, 200, archive_bytes("repo-snapshot", files))
}

fn archive_server(reference: &str, files: &[(&str, &str)]) -> MockGithubServer {
    MockGithubServer::start_bytes(vec![archive_route(reference, files)])
}

fn archive_base(server: &MockGithubServer) -> EnvGuard {
    EnvGuard::set(
        GITHUB_TEMPLATE_API_BASE_ENV,
        format!("http://{}", server.addr),
    )
}

fn archive_files<'a>(worker_agents: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("agent_templates/worker/template.toml", WORKER_MANIFEST),
        ("agent_templates/worker/AGENTS.md", worker_agents),
    ]
}

fn open_db(home: &TempDir) -> RuntimeDb {
    RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.sqlite.lock"),
    )
    .unwrap()
}

fn remote_config() -> AgentTemplateRemoteSourceConfigFile {
    AgentTemplateRemoteSourceConfigFile {
        url: "https://github.com/owner/repo".into(),
        git_ref: Some("main".into()),
        enabled: Some(true),
        credential_profile: None,
    }
}

#[tokio::test]
async fn initialize_agent_home_from_github_url_works_with_archive() {
    let _lock = crate::test_env::lock_env();
    let home = tempdir().unwrap();
    let server = archive_server(
        "main",
        &[("templates/reviewer/AGENTS.md", "# Public reviewer rules\n")],
    );
    let _base = archive_base(&server);

    let agent_home = home.path().join("agent");
    let provenance = initialize_agent_home_from_template(
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
    let _lock = crate::test_env::lock_env();
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

    let entries = discover_github_repo_templates("owner", "repo", "main")
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
    let _lock = crate::test_env::lock_env();
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

    let entries = discover_github_repo_templates("owner", "repo", "main")
        .await
        .unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].template_id, "good");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn resolve_remote_agent_template_detail_reads_archive_tree() {
    let _lock = crate::test_env::lock_env();
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

    let detail = resolve_remote_agent_template_detail(&entry).await.unwrap();
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
async fn sync_remote_source_with_global_pax_installs_all_templates_from_one_archive_get() {
    let _lock = crate::test_env::lock_env();
    let home = tempdir().unwrap();
    let db = open_db(&home);
    let server = archive_server(
        "main",
        &[
            ("agent_templates/worker/template.toml", WORKER_MANIFEST),
            ("agent_templates/worker/AGENTS.md", "# Worker\n"),
            ("agent_templates/reviewer/template.toml", REVIEWER_MANIFEST),
            ("agent_templates/reviewer/AGENTS.md", "# Reviewer\n"),
        ],
    );
    let _base = archive_base(&server);

    let status = sync_agent_template_remote_source(&db, home.path(), "official", &remote_config())
        .await
        .unwrap();

    assert_eq!(status.status, AgentTemplateRemoteSourceSyncStatus::Synced);
    assert_eq!(
        fs::read_to_string(
            templates_root_for_home(home.path())
                .join("worker")
                .join(TEMPLATE_AGENTS_FILENAME)
        )
        .unwrap(),
        "# Worker\n"
    );
    assert_eq!(
        fs::read_to_string(
            templates_root_for_home(home.path())
                .join("reviewer")
                .join(TEMPLATE_AGENTS_FILENAME)
        )
        .unwrap(),
        "# Reviewer\n"
    );
    let registry = load_template_registry(&templates_root_for_home(home.path())).unwrap();
    assert!(registry.installed.contains_key("worker"));
    assert!(registry.installed.contains_key("reviewer"));
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.requests()[0].path, "/owner/repo/tar.gz/main");
}

#[tokio::test]
async fn remote_archive_requests_use_env_github_token() {
    let _lock = crate::test_env::lock_env();
    let server = archive_server("main", &archive_files("# Worker\n"));
    let _base = archive_base(&server);
    let _token = EnvGuard::set("HOLON_GITHUB_TOKEN", "env-token".into());

    resolve_github_template("https://github.com/owner/repo/tree/main/agent_templates/worker")
        .await
        .unwrap();

    assert_eq!(
        server.requests()[0].authorization.as_deref(),
        Some("Bearer env-token")
    );
}

#[tokio::test]
async fn remote_archive_credential_profile_overrides_env_token() {
    let _lock = crate::test_env::lock_env();
    let home = tempdir().unwrap();
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "github-templates".to_string(),
        CredentialProfileFile {
            kind: CredentialKind::BearerToken,
            material: "profile-token".to_string(),
        },
    );
    save_credential_store_at(
        &credential_store_path(home.path()),
        &CredentialStoreFile { profiles },
    )
    .unwrap();
    let server = archive_server("main", &archive_files("# Worker\n"));
    let _base = archive_base(&server);
    let _token = EnvGuard::set("HOLON_GITHUB_TOKEN", "env-token".into());
    let config = AgentTemplateRemoteSourceConfigFile {
        credential_profile: Some("github-templates".into()),
        ..remote_config()
    };
    let db = open_db(&home);

    sync_agent_template_remote_source(&db, home.path(), "official", &config)
        .await
        .unwrap();

    assert_eq!(
        server.requests()[0].authorization.as_deref(),
        Some("Bearer profile-token")
    );
}

#[tokio::test]
#[cfg(unix)]
async fn remote_archive_uses_gh_cli_token_when_environment_is_empty() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = crate::test_env::lock_env();
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

    resolve_github_template("https://github.com/owner/repo/tree/main/agent_templates/worker")
        .await
        .unwrap();

    assert_eq!(
        server.requests()[0].authorization.as_deref(),
        Some("Bearer gh-token")
    );
}

#[tokio::test]
async fn sync_remote_source_reports_partial_failure_and_keeps_good_template() {
    let _lock = crate::test_env::lock_env();
    let home = tempdir().unwrap();
    let db = open_db(&home);
    let server = archive_server(
        "main",
        &[
            ("agent_templates/good/template.toml", WORKER_MANIFEST),
            ("agent_templates/good/AGENTS.md", "good"),
            ("agent_templates/bad/template.toml", REVIEWER_MANIFEST),
        ],
    );
    let _base = archive_base(&server);

    let status = sync_agent_template_remote_source(&db, home.path(), "official", &remote_config())
        .await
        .unwrap();

    assert_eq!(
        status.status,
        AgentTemplateRemoteSourceSyncStatus::PartiallyFailed
    );
    assert!(status.error.as_deref().unwrap().contains("bad"));
    assert_eq!(
        fs::read_to_string(
            templates_root_for_home(home.path())
                .join("good")
                .join(TEMPLATE_AGENTS_FILENAME)
        )
        .unwrap(),
        "good"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn sync_remote_source_renames_user_owned_conflict_without_extra_archive_requests() {
    let _lock = crate::test_env::lock_env();
    let home = tempdir().unwrap();
    let db = open_db(&home);
    let existing = templates_root_for_home(home.path()).join("worker");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join(TEMPLATE_AGENTS_FILENAME), "user-owned").unwrap();
    let server = archive_server("main", &archive_files("remote-owned"));
    let _base = archive_base(&server);

    let status = sync_agent_template_remote_source(&db, home.path(), "official", &remote_config())
        .await
        .unwrap();

    assert_eq!(status.status, AgentTemplateRemoteSourceSyncStatus::Synced);
    assert_eq!(
        fs::read_to_string(existing.join(TEMPLATE_AGENTS_FILENAME)).unwrap(),
        "user-owned"
    );
    assert_eq!(
        fs::read_to_string(
            templates_root_for_home(home.path())
                .join("worker@official")
                .join(TEMPLATE_AGENTS_FILENAME)
        )
        .unwrap(),
        "remote-owned"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn sync_remote_source_rejects_dirty_managed_template_and_preserves_local_edit() {
    let _lock = crate::test_env::lock_env();
    let home = tempdir().unwrap();
    let db = open_db(&home);
    let config = remote_config();
    let server = archive_server("main", &archive_files("remote-owned"));
    let _base = archive_base(&server);
    sync_agent_template_remote_source(&db, home.path(), "official", &config)
        .await
        .unwrap();
    let template_dir = templates_root_for_home(home.path()).join("worker");
    fs::write(template_dir.join(TEMPLATE_AGENTS_FILENAME), "local edit").unwrap();

    let status = sync_agent_template_remote_source(&db, home.path(), "official", &config)
        .await
        .unwrap();

    assert_eq!(status.status, AgentTemplateRemoteSourceSyncStatus::Failed);
    assert!(status.error.as_deref().unwrap().contains("local changes"));
    assert_eq!(
        fs::read_to_string(template_dir.join(TEMPLATE_AGENTS_FILENAME)).unwrap(),
        "local edit"
    );
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn explicit_template_install_uses_archive_and_preserves_auth() {
    let _lock = crate::test_env::lock_env();
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
    let _lock = crate::test_env::lock_env();
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
    let _lock = crate::test_env::lock_env();
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
    let _lock = crate::test_env::lock_env();
    let server = MockGithubServer::start_bytes(vec![]);
    let base = format!("http://{}", server.addr);
    let client = BlockingClient::new(
        reqwest::blocking::Client::new(),
        GitHubAuth::anonymous(),
        &base,
    )
    .unwrap();
    let error = install_template_from_github_with_client(
        tempdir().unwrap().path(),
        "https://github.com/owner/repo/tree/feature/topic/template",
        &client,
        &base,
    )
    .unwrap_err();

    assert!(error
        .downcast_ref::<crate::github_archive::ArchiveHttpFailure>()
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

    let _lock = crate::test_env::lock_env();
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
        resolve_github_template("https://github.com/owner/repo/tree/main/agent_templates/worker")
            .await
            .unwrap();
        assert_eq!(server.requests().len(), 1);
        assert!(server.requests()[0].authorization.is_none());
    }
}
