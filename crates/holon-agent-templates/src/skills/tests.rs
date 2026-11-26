use std::collections::{BTreeSet, HashMap};

use tempfile::tempdir;

use super::*;
use holon_domain::{SkillActivationSource, SkillActivationState};

#[allow(dead_code)]
fn install_github_remote_skill_with_client(
    io: &'static dyn PackageIo,
    user_home: &Path,
    package: &str,
    source: &RemoteSkillSource,
    client: &BlockingClient,
) -> Result<String> {
    install_github_remote_skill_with_client_and_archive_base(
        io,
        user_home,
        package,
        source,
        client,
        DEFAULT_ARCHIVE_BASE,
    )
}

#[allow(dead_code)]
fn install_github_remote_skill_set_with_client(
    io: &'static dyn PackageIo,
    user_home: &Path,
    package: &str,
    source: &RemoteSkillSetSource,
    client: &BlockingClient,
) -> Result<Vec<RemoteSkillInstall>> {
    install_github_remote_skill_set_with_client_and_archive_base(
        io,
        user_home,
        package,
        source,
        client,
        DEFAULT_ARCHIVE_BASE,
    )
}

#[derive(Clone)]
struct RecordedRemoteSkillInstall {
    user_home: PathBuf,
    package: String,
    skill: Option<String>,
}

struct RecordingRemoteSkillInstaller {
    calls: std::sync::Arc<std::sync::Mutex<Vec<RecordedRemoteSkillInstall>>>,
}

impl RemoteSkillInstaller for RecordingRemoteSkillInstaller {
    fn add_global(
        &self,
        user_home: &Path,
        package: &str,
        skill: Option<&str>,
    ) -> Result<Vec<RemoteSkillInstall>> {
        let skill_name = skill.unwrap_or(remote_package_default_skill_name(package)?);
        self.calls.lock().unwrap().push(RecordedRemoteSkillInstall {
            user_home: user_home.into(),
            package: package.into(),
            skill: skill.map(str::to_string),
        });
        let source = RemoteSkillSource::parse(package, Some(skill_name))?;
        Ok(vec![RemoteSkillInstall::from_source(
            &source,
            source.path.clone(),
        )])
    }
}

struct UnavailableRemoteSkillInstaller;

impl RemoteSkillInstaller for UnavailableRemoteSkillInstaller {
    fn add_global(
        &self,
        _user_home: &Path,
        _package: &str,
        _skill: Option<&str>,
    ) -> Result<Vec<RemoteSkillInstall>> {
        bail!("remote installer unavailable")
    }
}

fn remote_package_default_skill_name(package: &str) -> Result<&str> {
    package
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("remote skill package has no usable default skill name"))
}

impl GithubSkillRemoteUpdater {
    fn archive_for_with(
        &mut self,
        source: &LockedRemoteSkillSource,
        fetch: impl FnOnce() -> Result<GitHubArchive>,
    ) -> Result<&GitHubArchive> {
        let key = Self::archive_key(source);
        if !self.archives.contains_key(&key) {
            self.archives.insert(key.clone(), fetch()?);
        }
        Ok(self.archives.get(&key).expect("archive inserted"))
    }
}

#[cfg(unix)]
fn create_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn create_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

#[test]
fn archive_fetch_display_is_safe_bounded_and_not_a_process_exit() {
    let package = "包".repeat(1000);
    let failure = RemoteSkillInstallFailed {
        package: package.clone(),
        status: None,
        stdout: String::new(),
        stderr: "\nwrapper\nconnection refused\n".into(),
    };
    let message = failure.to_string();
    assert!(message.ends_with("failed: connection refused"));
    assert!(message.chars().count() < 512);
    assert!(!message.contains("process exit"));
    let long_cause = RemoteSkillInstallFailed {
        stderr: "因".repeat(1000),
        ..failure.clone()
    }
    .to_string();
    assert!(long_cause.chars().count() < 512);
    assert!(long_cause.ends_with(&"因".repeat(256)));
    assert_eq!(
        RemoteSkillInstallFailed {
            package: "owner/repo".into(),
            stderr: String::new(),
            ..failure.clone()
        }
        .to_string(),
        "remote skill install for 'owner/repo' failed"
    );
    assert_eq!(
        RemoteSkillInstallFailed {
            package: "owner/repo".into(),
            status: Some(17),
            ..failure
        }
        .to_string(),
        "remote skill install for 'owner/repo' failed with exit status 17"
    );

    for cause in [
        "Authorization: Bearer secret",
        "failed https://user:password@example.com/archive?token=secret#secret",
        "/api/callbacks/wake/secret",
        "failed /private/secret/archive.tar",
        r#"failed {"access_token":"secret"}"#,
    ] {
        let error = anyhow::anyhow!(cause);
        let context = RemoteSkillArchiveFetchFailed::new(&package, &error);
        let message = context.to_string();
        assert!(!message.contains("secret"), "{message}");
        assert!(message.chars().count() < 512);
        assert!(!message.contains('\n'));
        assert!(!context.retryable());
    }
}

#[test]
fn archive_fetch_connect_failure_keeps_type_and_no_install_artifacts() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let client = BlockingClient::new(
        reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap(),
        GitHubAuth::anonymous(),
        &base,
    )
    .unwrap();
    let home = tempdir().unwrap();
    let error = install_github_remote_skill_with_client_and_archive_base(
        &crate::TestIo,
        home.path(),
        "owner/repo@demo",
        &mock_source("skills/demo"),
        &client,
        &base,
    )
    .unwrap_err();
    assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_connect());
    let context = error
        .downcast_ref::<RemoteSkillArchiveFetchFailed>()
        .unwrap();
    assert!(context.retryable());
    let rendered = format!("{error:#}");
    assert!(rendered.contains("archive fetch"));
    assert!(rendered.contains("owner/repo@demo"));
    assert!(error.downcast_ref::<RemoteSkillInstallFailed>().is_none());
    assert!(!skill_lock_path(home.path()).exists());
    assert!(!user_library_skills_root(home.path()).join("demo").exists());
}

#[test]
fn archive_fetch_invalid_archive_is_not_retryable_and_leaves_no_install() {
    let home = tempdir().unwrap();
    let (client, server, base) = github_mock_with_base(
        vec![(200, "", "not a gzip archive".into())],
        GitHubAuth::anonymous(),
        Duration::from_secs(2),
    );
    let error = install_github_remote_skill_with_client_and_archive_base(
        &crate::TestIo,
        home.path(),
        "owner/repo@demo",
        &mock_source("skills/demo"),
        &client,
        &base,
    )
    .unwrap_err();
    assert_eq!(server.join().unwrap().len(), 1);
    let context = error
        .downcast_ref::<RemoteSkillArchiveFetchFailed>()
        .unwrap();
    assert!(!context.retryable());
    assert!(!context.cause.is_empty());
    assert!(!skill_lock_path(home.path()).exists());
    assert!(!user_library_skills_root(home.path()).join("demo").exists());
}

#[test]
fn archive_fetch_interrupted_body_retains_source_without_guessing_retryability() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\npartial",
            )
            .unwrap();
    });
    let client = BlockingClient::new(
        reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap(),
        GitHubAuth::anonymous(),
        &base,
    )
    .unwrap();
    // A full commit SHA skips SHA resolution so the single mock request is
    // the archive download itself; the cache root stays empty on failure.
    let cache_root = tempfile::tempdir().unwrap();
    let error = fetch_github_archive(
        &client,
        cache_root.path(),
        "owner",
        "repo",
        "1111111111111111111111111111111111111111",
        &base,
        "owner/repo",
    )
    .unwrap_err();
    server.join().unwrap();
    let context = error
        .downcast_ref::<RemoteSkillArchiveFetchFailed>()
        .unwrap();
    assert!(!context.retryable());
    // Blocking reads may wrap reqwest in io::Error. A generic body/decode
    // failure does not on its own identify a transient network fault.
    assert!(error.chain().any(|source| source.is::<std::io::Error>()));
    assert!(error.chain().any(|source| source.is::<reqwest::Error>()));
    assert!(format!("{error:#}").contains("archive fetch"));
}

// Serve real HTTP responses without contacting GitHub or reading credentials.
fn github_mock_with_base(
    responses: Vec<(u16, &'static str, String)>,
    auth: GitHubAuth,
    timeout: Duration,
) -> (BlockingClient, std::thread::JoinHandle<Vec<String>>, String) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        listener.set_nonblocking(true).unwrap();
        for (status, headers, body) in responses {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "missing mock request");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            requests.push(String::from_utf8(request).unwrap());
            write!(
                    stream,
                    "HTTP/1.1 {status} Mock\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{headers}\r\n{body}",
                    body.len()
                )
                .unwrap();
        }
        requests
    });
    let client = BlockingClient::new(
        reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap(),
        auth,
        &base,
    )
    .unwrap();
    (client, server, base)
}

fn mock_auth() -> GitHubAuth {
    GitHubAuth::resolve_blocking(Some("test-skill-token".to_owned()), &[])
}

fn mock_source(path: &str) -> RemoteSkillSource {
    RemoteSkillSource {
        owner: "owner".into(),
        repo: "repo".into(),
        reference: "main".into(),
        path: path.into(),
        skill_name: "demo".into(),
    }
}

#[test]
fn github_skill_404_fallback_preserves_tree_rate_limit_and_stops() {
    let home = tempdir().unwrap();
    // A pinned commit skips SHA resolution, so the queued responses map
    // to the archive request and its REST fallback directly.
    let source = RemoteSkillSource {
        reference: "1111111111111111111111111111111111111111".into(),
        ..mock_source("skills/demo")
    };
    let (client, server, archive_base) = github_mock_with_base(
        vec![
            (404, "", r#"{"message":"Not Found"}"#.into()),
            (
                403,
                "X-RateLimit-Remaining: 0\r\n",
                r#"{"message":"API rate limit exceeded"}"#.into(),
            ),
        ],
        mock_auth(),
        Duration::from_secs(2),
    );
    let error = install_github_remote_skill_with_client_and_archive_base(
        &crate::TestIo,
        home.path(),
        "owner/repo",
        &source,
        &client,
        &archive_base,
    )
    .unwrap_err();
    assert!(
        error
            .chain()
            .any(|cause| cause.is::<RateLimit>() || cause.is::<ArchiveHttpFailure>()),
        "{error:#}"
    );
    assert!(format!("{error:#}").contains("HTTP 403"));
    assert!(error.downcast_ref::<RemoteSkillInstallFailed>().is_none());
    assert_eq!(server.join().unwrap().len(), 2);
    assert!(!skill_lock_path(home.path()).exists());
    assert!(!user_library_skills_root(home.path()).join("demo").exists());
}

#[test]
fn github_skill_root_recursive_and_set_rate_limits_stop() {
    for scenario in ["root", "recursive", "set"] {
        let responses = vec![(429, "Retry-After: 1\r\n", "{}".into())];
        let count = responses.len();
        let (client, server, archive_base) =
            github_mock_with_base(responses, mock_auth(), Duration::from_secs(2));
        let home = tempdir().unwrap();
        let error = match scenario {
            "set" => install_github_remote_skill_set_with_client_and_archive_base(
                &crate::TestIo,
                home.path(),
                "owner/repo",
                &RemoteSkillSetSource {
                    owner: "owner".into(),
                    repo: "repo".into(),
                    reference: "main".into(),
                    path: "skills".into(),
                },
                &client,
                &archive_base,
            )
            .unwrap_err(),
            _ => install_github_remote_skill_with_client_and_archive_base(
                &crate::TestIo,
                home.path(),
                "owner/repo",
                &mock_source(if scenario == "root" {
                    ""
                } else {
                    "skills/demo"
                }),
                &client,
                &archive_base,
            )
            .unwrap_err(),
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<RateLimit>() || cause.is::<ArchiveHttpFailure>()),
            "{scenario}: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("HTTP 429"),
            "{scenario}: {error}"
        );
        assert_eq!(server.join().unwrap().len(), count);
        assert!(!skill_lock_path(home.path()).exists());
        assert!(!user_library_skills_root(home.path()).join("demo").exists());
    }
}

struct FakeSkillRemoteUpdater {
    contents: HashMap<String, String>,
    latest_hashes: HashMap<String, String>,
}

impl SkillRemoteUpdater for FakeSkillRemoteUpdater {
    fn latest_hash(&mut self, source: &LockedRemoteSkillSource) -> Result<String> {
        self.latest_hashes
            .get(&source.skill_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing fake hash for {}", source.skill_name))
    }

    fn install(&mut self, source: &LockedRemoteSkillSource, destination: &Path) -> Result<()> {
        fs::create_dir_all(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        fs::write(
            destination.join(SKILL_ENTRYPOINT),
            self.contents
                .get(&source.skill_name)
                .cloned()
                .unwrap_or_else(|| format!("# {}", source.skill_name)),
        )?;
        Ok(())
    }
}

#[test]
fn github_skill_remote_updater_reuses_archive_for_same_repository_reference() {
    let source = LockedRemoteSkillSource {
        skill_name: "demo".into(),
        owner: "owner".into(),
        repo: "repo".into(),
        reference: String::new(),
        skill_path: "skills/demo".into(),
        skill_folder_hash: "hash".into(),
    };
    let mut updater = GithubSkillRemoteUpdater::new(&crate::TestIo, tempdir().unwrap().path());
    let mut fetches = 0;
    let archive_root = |fetches: &mut usize| {
        *fetches += 1;
        let temp_dir = tempdir().unwrap();
        Ok(GitHubArchive::from_test_root(temp_dir))
    };

    let first_root = updater
        .archive_for_with(&source, || archive_root(&mut fetches))
        .unwrap()
        .root()
        .to_path_buf();
    let second_root = updater
        .archive_for_with(&source, || archive_root(&mut fetches))
        .unwrap()
        .root()
        .to_path_buf();

    assert_eq!(fetches, 1);
    assert_eq!(first_root, second_root);
}

fn write_v3_lock_entry(user_home: &Path, name: &str, hash: &str, extra: serde_json::Value) {
    fs::create_dir_all(user_home.join(".agents")).unwrap();
    let mut entry = serde_json::json!({
        "name": name,
        "source": "vercel-labs/agent-skills",
        "sourceType": "github",
        "sourceUrl": "https://github.com/vercel-labs/agent-skills",
        "skillPath": format!("skills/{name}"),
        "skillFolderHash": hash,
        "ref": "",
        "installedAt": "2026-01-01T00:00:00Z",
        "updatedAt": "2026-01-01T00:00:00Z"
    });
    if let (Some(entry), Some(extra)) = (entry.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            entry.insert(key.clone(), value.clone());
        }
    }
    fs::write(
        user_home.join(SKILL_LOCK_FILENAME),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 3,
            "skills": {
                name: entry,
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

fn parse_npx_v3_github_lock_entry(
    lock: &serde_json::Value,
    name: &str,
) -> Option<(String, String, String, String)> {
    let entry = lock.get("skills")?.get(name)?;
    if !entry
        .get("sourceType")?
        .as_str()?
        .eq_ignore_ascii_case("github")
    {
        return None;
    }
    Some((
        entry.get("source")?.as_str()?.to_string(),
        entry.get("sourceUrl")?.as_str()?.to_string(),
        entry.get("ref")?.as_str()?.to_string(),
        normalize_locked_skill_directory(entry.get("skillPath")?.as_str()?),
    ))
}

#[test]
fn agent_skill_discovery_merges_compatible_roots() {
    let dir = tempdir().unwrap();
    let base = dir.path();
    let first_root = base.join("skills");
    let fallback_root = base.join(".codex/skills");
    fs::create_dir_all(first_root.join("alpha")).unwrap();
    fs::create_dir_all(fallback_root.join("beta")).unwrap();
    fs::write(
        first_root.join("alpha").join(SKILL_ENTRYPOINT),
        "---\nname: alpha\ndescription: first\n---\nbody",
    )
    .unwrap();
    fs::write(
        fallback_root.join("beta").join(SKILL_ENTRYPOINT),
        "---\nname: beta\ndescription: second\n---\nbody",
    )
    .unwrap();

    let view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::NonDefaultAgent,
        None,
        base,
        None,
        &[],
    )
    .unwrap();

    assert_eq!(view.discovered_roots.len(), 2);
    let ids = view
        .discoverable_skills
        .iter()
        .map(|skill| skill.skill_id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|id| id.starts_with("agent_home:")));
    let mut names = view
        .discoverable_skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["alpha", "beta"]);
}

#[test]
fn workspace_skill_discovery_preserves_agent_home_as_execution_root() {
    let home = tempdir().unwrap();
    let write = |relative: &str| {
        let path = home.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "skill description").unwrap();
    };
    let view = || {
        load_skills_runtime_view(
            &crate::TestIo,
            SkillVisibility::NonDefaultAgent,
            None,
            home.path(),
            Some(home.path()),
            &[],
        )
        .unwrap()
    };
    write("skills/local/SKILL.md");
    let local = view().discoverable_skills[0].clone();

    write(".agents/skills/extra/SKILL.md");
    let after = view();
    assert!(after.catalog_only_roots.is_empty());
    assert_eq!(after.discoverable_skills.len(), 2);
    assert!(after.discoverable_skills.contains(&local));
}

#[test]
fn workspace_skill_discovery_prefers_nonempty_curated_roots() {
    let dir = tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let agent_home = dir.path().join("agent");
    let write = |relative: &str, content: &str| {
        let path = workspace.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    };
    let view = || {
        load_skills_runtime_view(
            &crate::TestIo,
            SkillVisibility::NonDefaultAgent,
            None,
            &agent_home,
            Some(&workspace),
            &[],
        )
        .unwrap()
    };
    let paths = |view: &SkillsRuntimeView| {
        view.discoverable_skills
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>()
    };

    let product = write("skills/product-a/SKILL.md", "product");
    assert_eq!(paths(&view()), BTreeSet::from([product.clone()]));
    fs::create_dir_all(workspace.join(".agents/skills/empty")).unwrap();
    write(".codex/skills/README.md", "not a skill");
    #[cfg(unix)]
    create_symlink(
        Path::new("missing"),
        &workspace.join(".codex/skills/broken"),
    )
    .unwrap();
    assert_eq!(paths(&view()), BTreeSet::from([product.clone()]));
    assert!(view().catalog_only_roots.is_empty());

    for root in COMPAT_SKILL_ROOT_SUFFIXES {
        let curated = write(
            &format!("{root}/product-a/SKILL.md"),
            "same name, no front matter",
        );
        let selected = view();
        assert_eq!(paths(&selected), BTreeSet::from([curated.clone()]));
        assert_eq!(selected.catalog_only_roots[0].path, "skills/");
        assert_eq!(selected.discoverable_skills, selected.attached_skills);
        fs::remove_file(curated).unwrap();
    }
    assert_eq!(paths(&view()), BTreeSet::from([product]));
    let curated = COMPAT_SKILL_ROOT_SUFFIXES
        .iter()
        .enumerate()
        .map(|(i, root)| write(&format!("{root}/curated-{i}/SKILL.md"), "curated"))
        .collect();
    assert_eq!(paths(&view()), curated);
}

#[cfg(unix)]
#[test]
fn workspace_skill_roots_deduplicate_symlink_aliases() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let bare_root = workspace.join("skills");
    fs::create_dir_all(&bare_root).unwrap();
    fs::create_dir_all(workspace.join(".codex")).unwrap();
    create_symlink(Path::new("../skills"), &workspace.join(".codex/skills")).unwrap();

    assert_eq!(
        workspace_skill_roots(Some(workspace)),
        vec![bare_root.clone()]
    );

    let registrations = effective_skill_root_registrations(
        SkillVisibility::NonDefaultAgent,
        None,
        "default",
        &workspace.join("agent"),
        Some(workspace),
    );
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].root_path, bare_root);
    assert_eq!(registrations[0].source_kind, SkillRootSourceKind::Workspace);
}

#[cfg(unix)]
#[test]
fn workspace_skill_discovery_respects_whole_root_and_single_skill_links() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    for skill in ["included", "other"] {
        let path = root.join("skills").join(skill);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "original").unwrap();
    }
    let view = || {
        load_skills_runtime_view(
            &crate::TestIo,
            SkillVisibility::NonDefaultAgent,
            None,
            &root.join("agent"),
            Some(root),
            &[],
        )
        .unwrap()
    };
    let initial = view();
    let alias = root.join(".codex/skills");
    fs::create_dir_all(alias.parent().unwrap()).unwrap();
    create_symlink(Path::new("../skills"), &alias).unwrap();
    let aliased = view();
    assert_eq!(initial.discoverable_skills.len(), 2);
    assert_eq!(aliased.discoverable_skills, initial.discoverable_skills);
    assert!(aliased.catalog_only_roots.is_empty());
    fs::remove_file(&alias).unwrap();

    fs::create_dir(&alias).unwrap();
    create_symlink(Path::new("../../skills/included"), &alias.join("included")).unwrap();
    let selected = view();
    assert_eq!(selected.discoverable_skills.len(), 1);
    assert_eq!(
        selected.discoverable_skills[0].path,
        alias.join("included/SKILL.md")
    );
    assert_eq!(selected.catalog_only_roots[0].path, "skills/");
}

#[test]
fn default_agent_sees_user_scope_but_non_default_does_not() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let agent_home = dir.path().join("agent");
    fs::create_dir_all(user_home.join(".agents/skills/ghx")).unwrap();
    fs::create_dir_all(agent_home.join("skills/local")).unwrap();
    fs::write(
        user_home.join(".agents/skills/ghx").join(SKILL_ENTRYPOINT),
        "---\nname: ghx\ndescription: user skill\n---\nbody",
    )
    .unwrap();
    fs::write(
        agent_home.join("skills/local").join(SKILL_ENTRYPOINT),
        "---\nname: local\ndescription: agent skill\n---\nbody",
    )
    .unwrap();

    let default_view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::DefaultAgent,
        Some(&user_home),
        &agent_home,
        None,
        &[],
    )
    .unwrap();
    let non_default_view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::NonDefaultAgent,
        Some(&user_home),
        &agent_home,
        None,
        &[],
    )
    .unwrap();

    assert!(default_view
        .discoverable_skills
        .iter()
        .any(|entry| entry.scope == SkillScope::UserGlobal));
    assert!(non_default_view
        .discoverable_skills
        .iter()
        .all(|entry| entry.scope != SkillScope::UserGlobal));
}

#[test]
fn same_name_skills_keep_highest_precedence_catalog_and_active_record() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let agent_home = dir.path().join("agent");
    let workspace = dir.path().join("workspace");
    let user_skill = user_home.join(".agents/skills/ghx");
    let agent_skill = agent_home.join("skills/ghx");
    let workspace_skill = workspace.join(".agents/skills/ghx");
    for (skill_dir, description) in [
        (&user_skill, "user skill"),
        (&agent_skill, "agent skill"),
        (&workspace_skill, "workspace skill"),
    ] {
        fs::create_dir_all(skill_dir).unwrap();
        fs::write(
            skill_dir.join(SKILL_ENTRYPOINT),
            format!("---\nname: ghx\ndescription: {description}\n---\nbody"),
        )
        .unwrap();
    }

    let agent_skill_root = agent_home.join("skills");
    let agent_ghx_skill_id = format!(
        "{}:ghx",
        skill_root_id_for_scope(SkillScope::Agent, &agent_skill_root)
    );
    let workspace_skill_root = workspace.join(".agents/skills");
    let workspace_ghx_skill_id = format!(
        "{}:ghx",
        skill_root_id_for_scope(SkillScope::Workspace, &workspace_skill_root)
    );

    let view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::DefaultAgent,
        Some(&user_home),
        &agent_home,
        Some(&workspace),
        &[
            ActiveSkillRecord {
                skill_id: workspace_ghx_skill_id,
                name: "ghx".into(),
                path: workspace_skill.join(SKILL_ENTRYPOINT),
                scope: SkillScope::Workspace,
                agent_id: "default".into(),
                activation_source: SkillActivationSource::ImplicitFromCatalog,
                activation_state: SkillActivationState::SessionActive,
                activated_at_turn: 1,
            },
            ActiveSkillRecord {
                skill_id: agent_ghx_skill_id,
                name: "ghx".into(),
                path: agent_skill.join(SKILL_ENTRYPOINT),
                scope: SkillScope::Agent,
                agent_id: "default".into(),
                activation_source: SkillActivationSource::ImplicitFromCatalog,
                activation_state: SkillActivationState::SessionActive,
                activated_at_turn: 2,
            },
        ],
    )
    .unwrap();

    assert_eq!(view.discoverable_skills.len(), 1);
    assert!(view.discoverable_skills[0]
        .skill_id
        .starts_with("workspace:"));
    assert!(view.discoverable_skills[0].skill_id.ends_with(":ghx"));
    assert_eq!(view.discoverable_skills[0].description, "workspace skill");
    assert_eq!(view.active_skills.len(), 1);
    assert_eq!(
        view.active_skills[0].skill_id,
        view.discoverable_skills[0].skill_id
    );
}

#[test]
fn same_name_same_scope_skills_use_stable_tie_breaker() {
    let dir = tempdir().unwrap();
    let later_id_path = dir.path().join("z-skill").join(SKILL_ENTRYPOINT);
    let earlier_id_path = dir.path().join("a-skill").join(SKILL_ENTRYPOINT);
    let same_id_later_path = dir.path().join("z-root/skill").join(SKILL_ENTRYPOINT);
    let same_id_earlier_path = dir.path().join("a-root/skill").join(SKILL_ENTRYPOINT);

    let selected = retain_highest_precedence_skills(vec![
        SkillCatalogEntry {
            skill_id: "agent:z-skill".into(),
            root_id: "agent_home:z-root".into(),
            skill_dir: "z-skill".into(),
            name: "shared".into(),
            description: "later id".into(),
            path: later_id_path,
            scope: SkillScope::Agent,
        },
        SkillCatalogEntry {
            skill_id: "agent:a-skill".into(),
            root_id: "agent_home:a-root".into(),
            skill_dir: "a-skill".into(),
            name: "shared".into(),
            description: "earlier id".into(),
            path: earlier_id_path,
            scope: SkillScope::Agent,
        },
        SkillCatalogEntry {
            skill_id: "workspace:skill".into(),
            root_id: "workspace:z-root".into(),
            skill_dir: "skill".into(),
            name: "same-id".into(),
            description: "later path".into(),
            path: same_id_later_path,
            scope: SkillScope::Workspace,
        },
        SkillCatalogEntry {
            skill_id: "workspace:skill".into(),
            root_id: "workspace:a-root".into(),
            skill_dir: "skill".into(),
            name: "same-id".into(),
            description: "earlier path".into(),
            path: same_id_earlier_path,
            scope: SkillScope::Workspace,
        },
    ]);

    let by_name = selected
        .iter()
        .map(|entry| (entry.name.as_str(), entry.description.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(by_name.get("shared"), Some(&"earlier id"));
    assert_eq!(by_name.get("same-id"), Some(&"earlier path"));
}

#[test]
fn frontmatter_description_wins_over_body() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills/demo");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join(SKILL_ENTRYPOINT),
        "---\nname: demo\ndescription: frontmatter description\n---\n\nParagraph description",
    )
    .unwrap();

    let view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::NonDefaultAgent,
        None,
        dir.path(),
        None,
        &[],
    )
    .unwrap();

    assert_eq!(
        view.discoverable_skills[0].description,
        "frontmatter description"
    );
}

#[test]
fn frontmatter_folded_and_literal_block_scalars_parse_multiline_description() {
    let dir = tempdir().unwrap();
    let folded_root = dir.path().join("skills/ace-step");
    let literal_root = dir.path().join("skills/literal-demo");
    fs::create_dir_all(&folded_root).unwrap();
    fs::create_dir_all(&literal_root).unwrap();
    fs::write(
        folded_root.join(SKILL_ENTRYPOINT),
        "---\nname: ace-step\ndescription: >\n  Generate and edit music with ACE Step.\n  Endpoints: text-to-audio and audio-inpaint.\n---\n\nBody text",
    )
    .unwrap();
    fs::write(
        literal_root.join(SKILL_ENTRYPOINT),
        "---\nname: literal-demo\ndescription: |\n  First line: alpha\n  Second line: beta\n---\n\nBody text",
    )
    .unwrap();

    let view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::NonDefaultAgent,
        None,
        dir.path(),
        None,
        &[],
    )
    .unwrap();

    let by_name: std::collections::BTreeMap<&str, &str> = view
        .discoverable_skills
        .iter()
        .map(|entry| (entry.name.as_str(), entry.description.as_str()))
        .collect();
    assert_eq!(
        by_name.get("ace-step"),
        Some(&"Generate and edit music with ACE Step. Endpoints: text-to-audio and audio-inpaint.")
    );
    assert_eq!(
        by_name.get("literal-demo"),
        Some(&"First line: alpha\nSecond line: beta")
    );
}

#[test]
fn filters_active_skills_to_visible_catalog() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills/demo");
    fs::create_dir_all(&root).unwrap();
    let skill_path = root.join(SKILL_ENTRYPOINT);
    fs::write(
        &skill_path,
        "---\nname: demo\ndescription: demo skill\n---\nbody",
    )
    .unwrap();

    let agent_skill_root = dir.path().join("skills");
    let agent_demo_skill_id = format!(
        "{}:demo",
        skill_root_id_for_scope(SkillScope::Agent, &agent_skill_root)
    );

    let view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::NonDefaultAgent,
        None,
        dir.path(),
        None,
        &[ActiveSkillRecord {
            skill_id: agent_demo_skill_id,
            name: "demo".into(),
            path: skill_path,
            scope: SkillScope::Agent,
            agent_id: "default".into(),
            activation_source: SkillActivationSource::ImplicitFromCatalog,
            activation_state: SkillActivationState::TurnActive,
            activated_at_turn: 1,
        }],
    )
    .unwrap();

    assert_eq!(view.active_skills.len(), 1);
}

#[test]
fn catalog_runtime_view_filters_active_skills_to_effective_catalog_snapshot() {
    let visible_path = PathBuf::from("/agent/skills/demo/SKILL.md");
    let view = skills_runtime_view_from_catalog(
        &crate::TestIo,
        EffectiveSkillCatalog {
            entries: vec![SkillCatalogEntry {
                skill_id: "agent_home:test-root:demo".into(),
                root_id: "agent_home:test-root".into(),
                skill_dir: "demo".into(),
                name: "demo".into(),
                description: "visible".into(),
                path: visible_path.clone(),
                scope: SkillScope::Agent,
            }],
            catalog_only_roots: vec![],
        },
        &[skill_root_registration(
            SkillRootSourceKind::AgentHome,
            Some("default".into()),
            PathBuf::from("/agent/skills"),
        )],
        &[
            ActiveSkillRecord {
                skill_id: "agent_home:test-root:demo".into(),
                name: "demo".into(),
                path: visible_path,
                scope: SkillScope::Agent,
                agent_id: "default".into(),
                activation_source: SkillActivationSource::ImplicitFromCatalog,
                activation_state: SkillActivationState::SessionActive,
                activated_at_turn: 1,
            },
            ActiveSkillRecord {
                skill_id: "workspace:stale".into(),
                name: "stale".into(),
                path: PathBuf::from("/workspace/.agents/skills/stale/SKILL.md"),
                scope: SkillScope::Workspace,
                agent_id: "default".into(),
                activation_source: SkillActivationSource::ImplicitFromCatalog,
                activation_state: SkillActivationState::SessionActive,
                activated_at_turn: 1,
            },
        ],
    );

    let view = view.unwrap();
    assert_eq!(view.discoverable_skills.len(), 1);
    assert_eq!(view.active_skills.len(), 1);
    assert_eq!(view.active_skills[0].skill_id, "agent_home:test-root:demo");
}

#[test]
fn skips_unreadable_skill_entries_without_failing_catalog_load() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    fs::create_dir_all(root.join("good")).unwrap();
    fs::create_dir_all(root.join("bad")).unwrap();
    fs::write(
        root.join("good").join(SKILL_ENTRYPOINT),
        "---\nname: good\ndescription: good skill\n---\nbody",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(root.join("bad").join(SKILL_ENTRYPOINT), "secret").unwrap();
        let mut perms = fs::metadata(root.join("bad").join(SKILL_ENTRYPOINT))
            .unwrap()
            .permissions();
        perms.set_mode(0o000);
        fs::set_permissions(root.join("bad").join(SKILL_ENTRYPOINT), perms).unwrap();
    }

    let view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::NonDefaultAgent,
        None,
        dir.path(),
        None,
        &[],
    )
    .unwrap();

    assert_eq!(view.discoverable_skills.len(), 1);
    assert_eq!(view.discoverable_skills[0].name, "good");
}

#[test]
fn named_install_links_unique_user_global_skill_for_non_default_agent() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let agent_home = dir.path().join("agent");
    let source = user_home.join(".agents/skills/global-demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join(SKILL_ENTRYPOINT),
        "---\nname: global-demo\ndescription: global skill\n---\nbody",
    )
    .unwrap();

    let installed = install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        Some(&user_home),
        &holon_domain::SkillInstallKind::Named {
            name: "global-demo".into(),
            mode: SkillInstallMode::Linked,
        },
    )
    .unwrap();

    assert_eq!(installed, "global-demo");
    let destination = agent_home.join("skills/global-demo");
    assert!(fs::symlink_metadata(&destination)
        .unwrap()
        .file_type()
        .is_symlink());
    let view = load_skills_runtime_view(
        &crate::TestIo,
        SkillVisibility::NonDefaultAgent,
        Some(&user_home),
        &agent_home,
        None,
        &[],
    )
    .unwrap();
    assert_eq!(view.discoverable_skills.len(), 1);
    assert_eq!(view.discoverable_skills[0].scope, SkillScope::Agent);
    assert_eq!(view.discoverable_skills[0].name, "global-demo");
}

#[test]
fn named_install_does_not_fallback_to_builtin_skill() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let agent_home = dir.path().join("agent");

    let error = install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        Some(&user_home),
        &holon_domain::SkillInstallKind::Named {
            name: "ghx".into(),
            mode: SkillInstallMode::Linked,
        },
    )
    .unwrap_err();

    assert!(format!("{error:?}").contains("was not found in the user-global skill catalog"));
    assert!(!agent_home.join("skills/ghx").exists());
}

#[test]
fn local_install_can_copy_skill_directory() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source/demo");
    let agent_home = dir.path().join("agent");
    fs::create_dir_all(source.join("references")).unwrap();
    fs::write(
        source.join(SKILL_ENTRYPOINT),
        "---\nname: demo\ndescription: copied skill\n---\nbody",
    )
    .unwrap();
    fs::write(source.join("references/notes.md"), "notes").unwrap();

    let installed = install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        None,
        &holon_domain::SkillInstallKind::Local {
            path: source.clone(),
            mode: SkillInstallMode::Copied,
        },
    )
    .unwrap();

    assert_eq!(installed, "demo");
    let destination = agent_home.join("skills/demo");
    assert!(destination.join(SKILL_ENTRYPOINT).is_file());
    assert!(destination.join("references/notes.md").is_file());
    assert!(!fs::symlink_metadata(&destination)
        .unwrap()
        .file_type()
        .is_symlink());
    let installed = list_installed_skills(&crate::TestIo, &agent_home).unwrap();
    assert_eq!(installed[0].install_mode, "copied");
    assert!(installed[0].link_target.is_none());
}

#[test]
fn copied_local_skill_named_like_builtin_reports_copied_mode() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source/ghx");
    let agent_home = dir.path().join("agent");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join(SKILL_ENTRYPOINT),
        "---\nname: ghx\ndescription: local ghx\n---\nbody",
    )
    .unwrap();

    install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        None,
        &holon_domain::SkillInstallKind::Local {
            path: source,
            mode: SkillInstallMode::Copied,
        },
    )
    .unwrap();

    let installed = list_installed_skills(&crate::TestIo, &agent_home).unwrap();
    assert_eq!(installed[0].catalog.name, "ghx");
    assert_eq!(installed[0].install_mode, "copied");
}

#[test]
fn copy_mode_rejects_symlinks_inside_skill_directory() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source/demo");
    let agent_home = dir.path().join("agent");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
    let target = dir.path().join("target.txt");
    fs::write(&target, "target").unwrap();
    create_symlink(&target, &source.join("linked.txt")).unwrap();

    let error = install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        None,
        &holon_domain::SkillInstallKind::Local {
            path: source,
            mode: SkillInstallMode::Copied,
        },
    )
    .unwrap_err();

    assert!(format!("{error:?}").contains("does not support symlinks"));
    assert!(!agent_home.join("skills/demo").exists());
}

#[test]
fn list_installed_skills_merges_compatible_agent_roots() {
    let dir = tempdir().unwrap();
    let agent_home = dir.path().join("agent");
    for (root, name) in [("skills", "alpha"), (".codex/skills", "beta")] {
        let skill_dir = agent_home.join(root).join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join(SKILL_ENTRYPOINT),
            format!("---\nname: {name}\ndescription: installed\n---\nbody"),
        )
        .unwrap();
    }

    let installed = list_installed_skills(&crate::TestIo, &agent_home).unwrap();

    let names = installed
        .iter()
        .map(|skill| skill.catalog.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["alpha", "beta"]);
}

#[test]
fn install_existing_destination_returns_structured_conflict() {
    let dir = tempdir().unwrap();
    let agent_home = dir.path().join("agent");
    let source = dir.path().join("source/demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
    let destination = agent_home.join("skills/demo");
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join(SKILL_ENTRYPOINT), "# Existing demo").unwrap();

    let error = install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        None,
        &holon_domain::SkillInstallKind::Local {
            path: source,
            mode: SkillInstallMode::Linked,
        },
    )
    .unwrap_err();
    let conflict = error
        .downcast_ref::<SkillInstallConflict>()
        .expect("existing destination should return a structured conflict");

    assert_eq!(conflict.skill_name, "demo");
    assert_eq!(conflict.destination, destination);
}

#[test]
fn install_existing_broken_symlink_returns_structured_conflict() {
    let dir = tempdir().unwrap();
    let agent_home = dir.path().join("agent");
    let skills_root = agent_home.join("skills");
    fs::create_dir_all(&skills_root).unwrap();
    let destination = skills_root.join("demo");
    create_symlink(&dir.path().join("missing-demo"), &destination).unwrap();
    let source = dir.path().join("source/demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();

    let error = install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        None,
        &holon_domain::SkillInstallKind::Local {
            path: source,
            mode: SkillInstallMode::Linked,
        },
    )
    .unwrap_err();
    let conflict = error
        .downcast_ref::<SkillInstallConflict>()
        .expect("dangling destination symlink should return a structured conflict");

    assert_eq!(conflict.skill_name, "demo");
    assert_eq!(conflict.destination, destination);
}

#[test]
fn uninstall_linked_skill_removes_only_agent_local_link() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source/demo");
    let agent_home = dir.path().join("agent");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
    install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        None,
        &holon_domain::SkillInstallKind::Local {
            path: source.clone(),
            mode: SkillInstallMode::Linked,
        },
    )
    .unwrap();

    uninstall_skill(&agent_home, "demo").unwrap();

    assert!(source.join(SKILL_ENTRYPOINT).is_file());
    assert!(!agent_home.join("skills/demo").exists());
}

#[test]
fn uninstall_skill_searches_compatible_agent_roots() {
    let dir = tempdir().unwrap();
    let agent_home = dir.path().join("agent");
    let source = dir.path().join("source/demo");
    let legacy_destination = agent_home.join(".codex/skills/demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
    fs::create_dir_all(legacy_destination.parent().unwrap()).unwrap();
    create_symlink(&source, &legacy_destination).unwrap();

    uninstall_skill(&agent_home, "demo").unwrap();

    assert!(source.join(SKILL_ENTRYPOINT).is_file());
    assert!(!legacy_destination.exists());
}

#[test]
fn uninstall_skill_rejects_duplicate_installs_across_roots() {
    let dir = tempdir().unwrap();
    let agent_home = dir.path().join("agent");
    for root in ["skills", ".codex/skills"] {
        let skill_dir = agent_home.join(root).join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join(SKILL_ENTRYPOINT), "# Demo").unwrap();
    }

    let error = uninstall_skill(&agent_home, "demo").unwrap_err();

    assert!(error.to_string().contains("multiple agent skill roots"));
    assert!(agent_home.join("skills/demo").exists());
    assert!(agent_home.join(".codex/skills/demo").exists());
}

#[test]
fn list_installed_skills_reports_link_mode_and_broken_link_warning() {
    let dir = tempdir().unwrap();
    let agent_home = dir.path().join("agent");
    let skills_root = agent_home.join("skills");
    fs::create_dir_all(&skills_root).unwrap();
    let missing = dir.path().join("missing-skill");
    create_symlink(&missing, &skills_root.join("missing")).unwrap();

    let installed = list_installed_skills(&crate::TestIo, &agent_home).unwrap();

    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].catalog.name, "missing");
    assert_eq!(installed[0].install_mode, "linked");
    assert_eq!(installed[0].link_target.as_deref(), Some(missing.as_path()));
    assert!(installed[0]
        .warning
        .as_deref()
        .unwrap_or_default()
        .contains("link target does not exist"));
}

#[test]
fn named_install_prefers_agents_user_global_skill_over_compat_roots() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let agent_home = dir.path().join("agent");
    for (root, description) in [(".agents/skills", "preferred"), (".codex/skills", "compat")] {
        let source = user_home.join(root).join("demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(SKILL_ENTRYPOINT),
            format!("---\nname: demo\ndescription: {description}\n---\nbody"),
        )
        .unwrap();
    }

    let installed = install_skill_with_user_home(
        &crate::TestIo,
        &agent_home,
        Some(&user_home),
        &holon_domain::SkillInstallKind::Named {
            name: "demo".into(),
            mode: SkillInstallMode::Linked,
        },
    )
    .unwrap();

    assert_eq!(installed, "demo");
    assert_eq!(
        fs::read_link(agent_home.join("skills/demo")).unwrap(),
        user_home.join(".agents/skills/demo")
    );
}

#[test]
fn remote_install_surfaces_installer_errors() {
    let dir = tempdir().unwrap();
    let error = install_skill_with_user_home_and_remote_installer(
        &crate::TestIo,
        &dir.path().join("agent"),
        Some(&dir.path().join("user")),
        &holon_domain::SkillInstallKind::Remote {
            package: "vercel-labs/agent-skills".into(),
            skill: Some("demo".into()),
            mode: SkillInstallMode::Linked,
        },
        &UnavailableRemoteSkillInstaller,
    )
    .unwrap_err();

    assert!(error.to_string().contains("remote installer unavailable"));
}

#[test]
fn remote_package_validation_rejects_option_like_and_whitespace_refs() {
    for package in [
        "", " ", "--help", "-x", " demo", "demo ", "foo bar", "foo\nbar",
    ] {
        assert!(
            validate_remote_package(package).is_err(),
            "package should be rejected: {package:?}"
        );
    }
}

#[test]
fn remote_package_validation_accepts_common_package_refs() {
    for package in ["vercel-labs/agent-skills", "@scope/package", "agent-skills"] {
        validate_remote_package(package).unwrap();
    }
}

#[test]
fn remote_skill_source_parses_github_tree_url() {
    let source = RemoteSkillSource::parse(
        "https://github.com/user/repo/tree/main/skills/my-skill",
        None,
    )
    .unwrap();

    assert_eq!(
        source,
        RemoteSkillSource {
            owner: "user".into(),
            repo: "repo".into(),
            reference: "main".into(),
            path: "skills/my-skill".into(),
            skill_name: "my-skill".into(),
        }
    );
}

#[test]
fn remote_skill_source_preserves_head_default_ref_sentinel() {
    let source = RemoteSkillSource::parse(
        "https://github.com/user/repo/tree/HEAD/skills/my-skill",
        None,
    )
    .unwrap();

    assert_eq!(
        source,
        RemoteSkillSource {
            owner: "user".into(),
            repo: "repo".into(),
            reference: "HEAD".into(),
            path: "skills/my-skill".into(),
            skill_name: "my-skill".into(),
        }
    );
}

#[test]
fn remote_skill_source_parses_owner_repo_skill_package() {
    let source = RemoteSkillSource::parse("vercel-labs/agent-skills@pr-review", None).unwrap();

    assert_eq!(source.owner, "vercel-labs");
    assert_eq!(source.repo, "agent-skills");
    assert_eq!(source.reference, "");
    assert_eq!(source.path, "skills/pr-review");
    assert_eq!(source.skill_name, "pr-review");
}

#[test]
fn remote_skill_source_parses_repo_root_skill_tree_url() {
    let source =
        RemoteSkillSource::parse("https://github.com/blader/humanizer/tree/HEAD/.", None).unwrap();

    assert_eq!(
        source,
        RemoteSkillSource {
            owner: "blader".into(),
            repo: "humanizer".into(),
            reference: "HEAD".into(),
            path: ".".into(),
            skill_name: "humanizer".into(),
        }
    );
}

#[test]
fn remote_install_adds_global_skill_non_interactively_then_links_to_agent() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let agent_home = dir.path().join("agent");
    let source = user_home.join(".agents/skills/demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join(SKILL_ENTRYPOINT),
        "---\nname: demo\ndescription: remote\n---\nbody",
    )
    .unwrap();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let installer = RecordingRemoteSkillInstaller {
        calls: calls.clone(),
    };

    let installed = install_skill_with_user_home_and_remote_installer(
        &crate::TestIo,
        &agent_home,
        Some(&user_home),
        &holon_domain::SkillInstallKind::Remote {
            package: "vercel-labs/agent-skills".into(),
            skill: Some("demo".into()),
            mode: SkillInstallMode::Linked,
        },
        &installer,
    )
    .unwrap();

    assert_eq!(installed, "demo");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].user_home, user_home);
    assert_eq!(calls[0].package, "vercel-labs/agent-skills");
    assert_eq!(calls[0].skill.as_deref(), Some("demo"));
    assert_eq!(
        fs::read_link(agent_home.join("skills/demo")).unwrap(),
        user_home.join(".agents/skills/demo")
    );
    let lock = read_skill_lock(&crate::TestIo, &user_home).unwrap();
    assert_eq!(
        lock.get("version").and_then(|value| value.as_u64()),
        Some(3)
    );
    assert_eq!(
        parse_npx_v3_github_lock_entry(&lock, "demo"),
        Some((
            "vercel-labs/agent-skills".into(),
            "https://github.com/vercel-labs/agent-skills".into(),
            String::new(),
            "skills/demo".into(),
        ))
    );
    let entry = lock
        .get("skills")
        .and_then(|skills| skills.get("demo"))
        .unwrap();
    assert_eq!(
        entry.get("skillPath").and_then(|value| value.as_str()),
        Some("skills/demo/SKILL.md")
    );
    assert!(entry
        .get("skillFolderHash")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.is_empty()));
}

#[test]
fn npx_v3_skill_file_paths_parse_as_holon_remote_directories() {
    for (skill_path, expected) in [
        ("skills/demo/SKILL.md", "skills/demo"),
        ("skills/demo/skill.MD", "skills/demo"),
        (r"skills\demo\SKILL.md", "skills/demo"),
        ("SKILL.md", ""),
        ("skills/demo", "skills/demo"),
        ("skills/my-SKILL.md-assets", "skills/my-SKILL.md-assets"),
    ] {
        let entry = serde_json::json!({
            "name": "demo",
            "source": "vercel-labs/agent-skills",
            "sourceType": "github",
            "sourceUrl": "https://github.com/vercel-labs/agent-skills",
            "skillPath": skill_path,
            "skillFolderHash": "hash",
            "ref": ""
        });

        let source = LockedRemoteSkillSource::from_lock_entry("demo", &entry).unwrap();

        assert_eq!(source.skill_path, expected, "skillPath={skill_path:?}");
    }
}

#[test]
fn remote_install_does_not_downgrade_future_lock_version() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let skill_dir = user_home.join(".agents/skills/demo");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join(SKILL_ENTRYPOINT), "# demo").unwrap();
    fs::write(
        user_home.join(SKILL_LOCK_FILENAME),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 4,
            "skills": {}
        }))
        .unwrap(),
    )
    .unwrap();

    sync_remote_skill_lock_entries(
        &crate::TestIo,
        &user_home,
        &[RemoteSkillInstall {
            skill_name: "demo".into(),
            owner: "vercel-labs".into(),
            repo: "agent-skills".into(),
            reference: String::new(),
            skill_path: "skills/demo".into(),
        }],
    )
    .unwrap();

    let lock = read_skill_lock(&crate::TestIo, &user_home).unwrap();
    assert_eq!(
        lock.get("version").and_then(serde_json::Value::as_u64),
        Some(4)
    );
    assert!(lock
        .get("skills")
        .and_then(|skills| skills.get("demo"))
        .is_some());
}

#[test]
fn update_library_skills_reports_unchanged_remote_skill() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    write_v3_lock_entry(
        &user_home,
        "demo",
        "same-hash",
        serde_json::json!({"xUnknown": "preserved"}),
    );
    fs::create_dir_all(user_home.join(".agents/skills/demo")).unwrap();
    fs::write(
        user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT),
        "# old",
    )
    .unwrap();
    let mut updater = FakeSkillRemoteUpdater {
        latest_hashes: HashMap::from([("demo".into(), "same-hash".into())]),
        contents: HashMap::new(),
    };

    let result = update_library_skills_with_remote_updater(
        &crate::TestIo,
        &user_home,
        None,
        &mut updater,
        |_| {},
    )
    .unwrap();

    assert_eq!(result.statuses.len(), 1);
    assert_eq!(result.statuses[0].name, "demo");
    assert_eq!(
        result.statuses[0].status,
        SkillLibraryUpdateState::Unchanged
    );
    assert_eq!(
        fs::read_to_string(user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT)).unwrap(),
        "# old"
    );
    let lock = read_skill_lock(&crate::TestIo, &user_home).unwrap();
    let entry = lock
        .get("skills")
        .and_then(|skills| skills.get("demo"))
        .unwrap();
    assert_eq!(
        entry.get("xUnknown").and_then(|value| value.as_str()),
        Some("preserved")
    );
    assert_eq!(
        entry.get("source").and_then(|value| value.as_str()),
        Some("vercel-labs/agent-skills")
    );
}

#[test]
fn skill_folder_hash_ignores_holon_install_metadata() {
    let dir = tempdir().unwrap();
    let skill_dir = dir.path().join("demo");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join(SKILL_ENTRYPOINT), "# demo").unwrap();
    let before = hash_skill_folder(&crate::TestIo, &skill_dir).unwrap();

    record_install_metadata(&skill_dir, "remote").unwrap();

    assert_eq!(
        hash_skill_folder(&crate::TestIo, &skill_dir).unwrap(),
        before
    );
}

#[test]
fn update_library_skills_updates_changed_remote_skill_and_preserves_lock_fields() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    write_v3_lock_entry(
        &user_home,
        "demo",
        "old-hash",
        serde_json::json!({"custom": {"nested": true}}),
    );
    fs::create_dir_all(user_home.join(".agents/skills/demo")).unwrap();
    fs::write(
        user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT),
        "# old",
    )
    .unwrap();
    let mut updater = FakeSkillRemoteUpdater {
        latest_hashes: HashMap::from([("demo".into(), "new-hash".into())]),
        contents: HashMap::from([("demo".into(), "# new".into())]),
    };

    let result = update_library_skills_with_remote_updater(
        &crate::TestIo,
        &user_home,
        Some("demo"),
        &mut updater,
        |_| {},
    )
    .unwrap();

    assert_eq!(result.statuses.len(), 1);
    assert_eq!(result.statuses[0].status, SkillLibraryUpdateState::Updated);
    assert_eq!(
        fs::read_to_string(user_home.join(".agents/skills/demo").join(SKILL_ENTRYPOINT)).unwrap(),
        "# new"
    );
    let lock = read_skill_lock(&crate::TestIo, &user_home).unwrap();
    let entry = lock
        .get("skills")
        .and_then(|skills| skills.get("demo"))
        .unwrap();
    assert_eq!(
        entry
            .get("skillFolderHash")
            .and_then(|value| value.as_str()),
        Some("new-hash")
    );
    assert_eq!(
        entry
            .get("custom")
            .and_then(|value| value.get("nested"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        entry.get("sourceType").and_then(|value| value.as_str()),
        Some("github")
    );
    assert_eq!(
        entry.get("skillPath").and_then(|value| value.as_str()),
        Some("skills/demo/SKILL.md")
    );
}

#[test]
fn update_library_skills_skips_unsupported_and_legacy_lock_entries() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    fs::create_dir_all(user_home.join(".agents")).unwrap();
    fs::write(
        user_home.join(SKILL_LOCK_FILENAME),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 3,
            "skills": {
                "local": {
                    "name": "local",
                    "sourceType": "local",
                    "path": "/tmp/local"
                },
                "legacy": {
                    "name": "legacy",
                    "source": "local"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut updater = FakeSkillRemoteUpdater {
        latest_hashes: HashMap::new(),
        contents: HashMap::new(),
    };

    let result = update_library_skills_with_remote_updater(
        &crate::TestIo,
        &user_home,
        None,
        &mut updater,
        |_| {},
    )
    .unwrap();

    assert_eq!(result.statuses.len(), 2);
    assert!(result
        .statuses
        .iter()
        .all(|status| status.status == SkillLibraryUpdateState::Skipped));
}

#[test]
fn reconcile_remains_local_only_and_update_preserves_remote_source_semantics() {
    let dir = tempdir().unwrap();
    let user_home = dir.path().join("user");
    let skill_dir = user_home.join(".agents/skills/demo");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join(SKILL_ENTRYPOINT), "# demo").unwrap();
    write_v3_lock_entry(&user_home, "demo", "old-hash", serde_json::json!({}));

    let reconcile = reconcile_library_skills(&crate::TestIo, &user_home, Some("demo")).unwrap();
    assert!(reconcile.added.is_empty());
    assert!(reconcile.removed.is_empty());
    let lock = read_skill_lock(&crate::TestIo, &user_home).unwrap();
    let entry = lock
        .get("skills")
        .and_then(|skills| skills.get("demo"))
        .unwrap();
    assert_eq!(
        entry.get("source").and_then(|value| value.as_str()),
        Some("vercel-labs/agent-skills")
    );
    assert_eq!(
        entry
            .get("skillFolderHash")
            .and_then(|value| value.as_str()),
        Some("old-hash")
    );

    let mut updater = FakeSkillRemoteUpdater {
        latest_hashes: HashMap::from([("demo".into(), "new-hash".into())]),
        contents: HashMap::from([("demo".into(), "# updated".into())]),
    };
    update_library_skills_with_remote_updater(
        &crate::TestIo,
        &user_home,
        Some("demo"),
        &mut updater,
        |_| {},
    )
    .unwrap();
    let lock = read_skill_lock(&crate::TestIo, &user_home).unwrap();
    let entry = lock
        .get("skills")
        .and_then(|skills| skills.get("demo"))
        .unwrap();
    assert_eq!(
        entry.get("source").and_then(|value| value.as_str()),
        Some("vercel-labs/agent-skills")
    );
    assert_eq!(
        entry
            .get("skillFolderHash")
            .and_then(|value| value.as_str()),
        Some("new-hash")
    );
}
