use super::*;

use std::{process::Command, sync::Arc};

use crate::{
    config::AppConfig,
    host::RuntimeHost,
    provider::StubProvider,
    runtime::RuntimeHandle,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
};
use tempfile::{tempdir, TempDir};

struct WorkspaceFixture {
    home: TempDir,
    _host: RuntimeHost,
    runtime: RuntimeHandle,
    a: WorkspaceEntry,
    b: WorkspaceEntry,
    worktree: PathBuf,
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl WorkspaceFixture {
    async fn new() -> Self {
        let home = tempdir().unwrap();
        fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        config.user_home_dir = None;
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused"))).unwrap();
        let runtime = host.default_runtime().await.unwrap();
        let a_path = home.path().join("project-a");
        let b_path = home.path().join("project-b");
        fs::create_dir_all(&a_path).unwrap();
        fs::create_dir_all(&b_path).unwrap();
        git(&a_path, &["init", "--quiet", "-b", "main"]);
        git(
            &a_path,
            &[
                "-c",
                "user.name=Holon Test",
                "-c",
                "user.email=holon-test@example.com",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        let worktree = home.path().join("existing-worktree");
        git(
            &a_path,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "index-fixture",
                worktree.to_str().unwrap(),
            ],
        );
        let a = WorkspaceEntry::new(
            "ws-index-a",
            a_path.canonicalize().unwrap(),
            Some("activationprofile alpha".into()),
        );
        let b = WorkspaceEntry::new(
            "ws-index-b",
            b_path.canonicalize().unwrap(),
            Some("activationprofile beta".into()),
        );
        runtime.attach_workspace(&a).await.unwrap();
        runtime.attach_workspace(&b).await.unwrap();
        Self {
            home,
            _host: host,
            runtime,
            a,
            b,
            worktree,
        }
    }

    async fn activate(&self, step: usize) {
        match step % 8 {
            0 | 1 | 2 => {
                self.runtime
                    .enter_workspace(
                        if step % 8 == 2 { &self.b } else { &self.a },
                        WorkspaceProjectionKind::CanonicalRoot,
                        WorkspaceAccessMode::SharedRead,
                        None,
                        None,
                    )
                    .await
                    .unwrap();
            }
            3 | 7 => self
                .runtime
                .activate_agent_home(WorkspaceAccessMode::SharedRead, None)
                .await
                .unwrap(),
            4 | 5 => self
                .runtime
                .enter_existing_git_worktree(
                    &self.a,
                    self.worktree.clone(),
                    WorkspaceAccessMode::SharedRead,
                    None,
                )
                .await
                .unwrap(),
            6 => self.runtime.exit_workspace().await.unwrap(),
            _ => unreachable!(),
        }
    }
}

fn assert_no_self_heal(storage: &AppStorage) {
    let agent_id = storage_agent_id(storage);
    assert!(!memory_index_is_dirty(storage));
    assert!(
        memory_index_agent_ids_needing_backfill(storage, std::slice::from_ref(&agent_id))
            .unwrap()
            .is_empty()
    );
    let index = MemoryIndex::open(storage).unwrap();
    assert!(index.store.rebuild_job(&agent_id).unwrap().is_none());
    assert!(index
        .store
        .rebuild_intents_for_agent(&agent_id)
        .unwrap()
        .is_empty());
}

fn drain_incremental(storage: &AppStorage) -> MemorySearchIndexStatus {
    for _ in 0..64 {
        let status = refresh_memory_index_bounded(storage, None, 64).unwrap();
        assert!(status.rebuild_phase.is_none());
        assert!(status.cursor <= status.high_watermark);
        if status.freshness == "fresh" {
            assert_eq!(status.cursor, status.high_watermark);
            assert_eq!(status.pending_count, 0);
            return status;
        }
    }
    panic!("small fixture did not drain");
}

fn index_status_now(storage: &AppStorage) -> MemorySearchIndexStatus {
    MemoryIndex::open(storage)
        .unwrap()
        .index_status(storage, &storage_agent_id(storage))
        .unwrap()
}

fn finish_generation(storage: &AppStorage, generation: &str) -> MemorySearchIndexStatus {
    let agent_id = storage_agent_id(storage);
    for _ in 0..128 {
        let job = MemoryIndex::open(storage)
            .unwrap()
            .store
            .rebuild_job(&agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(job.generation, generation);
        let status = refresh_memory_index_bounded(storage, None, 4).unwrap();
        if status.rebuild_phase.is_none() {
            return status;
        }
    }
    panic!("small fixture generation did not finish");
}

#[tokio::test]
async fn fresh_workspace_activation_does_not_enqueue_self_heal() {
    let fixture = WorkspaceFixture::new().await;
    let storage = fixture.runtime.storage();
    rebuild_memory_index(storage, None).unwrap();

    for step in 0..16 {
        fixture.activate(step).await;
        assert_no_self_heal(storage);
        let snapshot = fixture.runtime.execution_snapshot().await.unwrap();
        let results = search_memory(
            storage,
            "activationprofile",
            10,
            snapshot.workspace_id.as_deref(),
            false,
        )
        .unwrap();
        let profiles: Vec<_> = results
            .iter()
            .filter(|result| result.kind == "workspace_profile")
            .collect();
        if let Some(workspace_id @ ("ws-index-a" | "ws-index-b")) = snapshot.workspace_id.as_deref()
        {
            assert_eq!(profiles.len(), 1);
            assert_eq!(profiles[0].workspace_id.as_deref(), Some(workspace_id));
        } else {
            assert!(profiles.is_empty());
        }
    }
    drain_incremental(storage);
    let drained = index_status_now(storage);
    assert_eq!(drained.freshness, "fresh");
    assert_eq!(drained.pending_count, 0);

    // A second pass must not enqueue redundant index work either; the agent
    // home entry registered by the first pass is the only legitimate append.
    for step in 16..32 {
        fixture.activate(step).await;
        assert_no_self_heal(storage);
    }
    let repeated = index_status_now(storage);
    assert_eq!(repeated.pending_count, 0);
    assert_eq!(repeated.high_watermark, drained.high_watermark);
    assert_eq!(repeated.freshness, "fresh");
    assert_no_self_heal(storage);
}

#[tokio::test]
async fn workspace_profile_writes_remain_incrementally_searchable() {
    let fixture = WorkspaceFixture::new().await;
    let storage = fixture.runtime.storage();
    rebuild_memory_index(storage, None).unwrap();
    let new_path = fixture.home.path().join("project-c");
    fs::create_dir_all(&new_path).unwrap();
    let new_workspace = WorkspaceEntry::new(
        "ws-index-c",
        new_path.canonicalize().unwrap(),
        Some("incrementalnewprofile".into()),
    );
    fixture
        .runtime
        .attach_workspace(&new_workspace)
        .await
        .unwrap();
    assert!(!memory_index_is_dirty(storage));
    drain_incremental(storage);
    assert!(search_memory(
        storage,
        "incrementalnewprofile",
        10,
        Some("ws-index-c"),
        false
    )
    .unwrap()
    .iter()
    .any(|result| result.source_ref == "workspace_profile:ws-index-c"));

    let mut updated = fixture.a.clone();
    updated.repo_name = Some("incrementalupdatedprofile".into());
    updated.updated_at = Utc::now();
    fixture.runtime.attach_workspace(&updated).await.unwrap();
    for step in 0..16 {
        fixture.activate(step).await;
        assert!(!memory_index_is_dirty(storage));
    }
    drain_incremental(storage);
    assert!(search_memory(
        storage,
        "incrementalupdatedprofile",
        10,
        Some("ws-index-a"),
        false
    )
    .unwrap()
    .iter()
    .any(|result| result.source_ref == "workspace_profile:ws-index-a"));
    assert!(
        search_memory(storage, "activationprofile", 10, Some("ws-index-a"), false)
            .unwrap()
            .iter()
            .all(|result| result.kind != "workspace_profile")
    );
    assert!(search_memory(
        storage,
        "incrementalupdatedprofile",
        10,
        Some("ws-index-b"),
        false
    )
    .unwrap()
    .is_empty());
    assert_no_self_heal(storage);
}

#[tokio::test]
async fn workspace_activation_during_rebuild_does_not_requeue_generation() {
    let fixture = WorkspaceFixture::new().await;
    let storage = fixture.runtime.storage();
    rebuild_memory_index(storage, None).unwrap();
    storage.mark_memory_index_dirty().unwrap();
    request_memory_index_rebuild(storage, None, "explicit_fixture").unwrap();
    let first = refresh_memory_index_bounded(storage, None, 1).unwrap();
    assert_eq!(first.rebuild_phase.as_deref(), Some("scan"));
    assert!(!memory_index_is_dirty(storage));
    let agent_id = storage_agent_id(storage);
    let job = MemoryIndex::open(storage)
        .unwrap()
        .store
        .rebuild_job(&agent_id)
        .unwrap()
        .unwrap();

    for step in 0..16 {
        fixture.activate(step).await;
        assert!(!memory_index_is_dirty(storage));
    }
    let mut updated = fixture.b.clone();
    updated.repo_name = Some("postsnapshotprofile".into());
    updated.updated_at = Utc::now();
    fixture.runtime.attach_workspace(&updated).await.unwrap();
    let completed = finish_generation(storage, &job.generation);
    assert_eq!(completed.cursor, job.runtime_high_watermark);
    assert!(completed.pending_count > 0);
    assert!(completed.cursor < completed.high_watermark);
    assert_no_self_heal(storage);

    drain_incremental(storage);
    assert!(search_memory(
        storage,
        "postsnapshotprofile",
        10,
        Some("ws-index-b"),
        false
    )
    .unwrap()
    .iter()
    .any(|result| result.source_ref == "workspace_profile:ws-index-b"));
    let drained = index_status_now(storage);
    assert_eq!(drained.freshness, "fresh");
    assert_eq!(drained.pending_count, 0);
    assert_no_self_heal(storage);
}

#[tokio::test]
async fn real_dirty_during_workspace_rebuild_still_requires_self_heal() {
    let fixture = WorkspaceFixture::new().await;
    let storage = fixture.runtime.storage();
    rebuild_memory_index(storage, None).unwrap();
    request_memory_index_rebuild(storage, None, "explicit_fixture").unwrap();
    refresh_memory_index_bounded(storage, None, 1).unwrap();
    let agent_id = storage_agent_id(storage);
    let first_job = MemoryIndex::open(storage)
        .unwrap()
        .store
        .rebuild_job(&agent_id)
        .unwrap()
        .unwrap();

    fs::write(
        agent_memory_self_path(storage.data_dir()),
        "genuinedirtysentinel",
    )
    .unwrap();
    storage.mark_memory_index_dirty().unwrap();
    for step in 0..16 {
        fixture.activate(step).await;
        assert!(memory_index_is_dirty(storage));
    }
    finish_generation(storage, &first_job.generation);
    assert!(memory_index_is_dirty(storage));
    assert_eq!(
        memory_index_agent_ids_needing_backfill(storage, std::slice::from_ref(&agent_id)).unwrap(),
        vec![agent_id.clone()]
    );

    request_memory_index_rebuild(storage, None, "self_heal").unwrap();
    refresh_memory_index_bounded(storage, None, 1).unwrap();
    let recovery_job = MemoryIndex::open(storage)
        .unwrap()
        .store
        .rebuild_job(&agent_id)
        .unwrap()
        .unwrap();
    assert_ne!(recovery_job.generation, first_job.generation);
    finish_generation(storage, &recovery_job.generation);
    drain_incremental(storage);
    assert_no_self_heal(storage);
    assert!(
        search_memory(storage, "genuinedirtysentinel", 10, None, false)
            .unwrap()
            .iter()
            .any(|result| result.source_ref == "agent_memory:self")
    );
}
