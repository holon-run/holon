use holon_agent_templates::templates::test_support::{
    archive_base, archive_files, archive_server, REVIEWER_MANIFEST, WORKER_MANIFEST,
};
use std::{collections::BTreeMap, fs};

use tempfile::{tempdir, TempDir};

use super::*;
use crate::config::{save_credential_store_at, CredentialProfileFile, CredentialStoreFile};

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
    let installed =
        test_support::installed_template_ids(&templates_root_for_home(home.path())).unwrap();
    assert!(installed.iter().any(|id| id == "worker"));
    assert!(installed.iter().any(|id| id == "reviewer"));
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.requests()[0].path, "/owner/repo/tar.gz/main");
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
