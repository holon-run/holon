use std::path::PathBuf;

use anyhow::Result;

mod support;

use support::{GitWorkspaceFixture, TestConfigBuilder};

fn create_then_return_test_resources() -> Result<(PathBuf, PathBuf)> {
    let config = TestConfigBuilder::new().build();
    let data_dir = config.data_dir().to_path_buf();
    let workspace_dir = config.workspace_dir().to_path_buf();
    let state_dir = data_dir.join("state");
    std::fs::create_dir_all(&state_dir)?;
    std::fs::create_dir_all(&workspace_dir)?;
    for name in ["runtime.sqlite", "runtime.sqlite-wal", "runtime.sqlite-shm"] {
        std::fs::write(state_dir.join(name), b"fixture")?;
    }
    Ok((data_dir, workspace_dir))
}

#[test]
fn test_config_drop_cleans_early_return_sqlite_and_workspace_artifacts() -> Result<()> {
    let (data_dir, workspace_dir) = create_then_return_test_resources()?;

    assert!(!data_dir.exists(), "data dir should be removed by TempDir");
    assert!(
        !workspace_dir.exists(),
        "workspace dir should be removed by TempDir"
    );
    Ok(())
}

#[test]
fn git_workspace_fixture_drop_cleans_repository_root() -> Result<()> {
    let (temp_root, repo_root) = {
        let fixture = GitWorkspaceFixture::new()?;
        let temp_root = fixture.temp_root().to_path_buf();
        let repo_root = fixture.root.clone();
        assert!(repo_root.join(".git").exists());
        (temp_root, repo_root)
    };

    assert!(!repo_root.exists());
    assert!(!temp_root.exists());
    Ok(())
}
