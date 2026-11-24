//! Agent-home layout files must stay private regardless of the process umask.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use holon_agent_templates::templates::ensure_agent_home_layout;

fn mode_of(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn agent_home_layout_is_private() {
    let home = tempfile::tempdir().unwrap();
    let shared = home.path().join("workspace");
    fs::create_dir_all(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    let agent_home = shared.join("agent");
    ensure_agent_home_layout(&agent_home).unwrap();

    assert_eq!(mode_of(&agent_home), 0o700);
    for dir in [
        "memory",
        "notes",
        "work",
        "tmp",
        "work-items",
        "skills",
        ".holon/state",
        ".holon/ledger",
        ".holon/indexes",
        ".holon/cache",
    ] {
        assert_eq!(
            mode_of(&agent_home.join(dir)),
            0o700,
            "{dir} must be private"
        );
    }
    for file in ["memory/self.md", "memory/operator.md"] {
        assert_eq!(
            mode_of(&agent_home.join(file)),
            0o600,
            "{file} must be private"
        );
    }
    // The shared parent above the agent home keeps its own mode.
    assert_eq!(mode_of(&shared), 0o755, "shared parent must stay as-is");
}

#[test]
fn existing_wide_agent_home_is_corrected() {
    let home = tempfile::tempdir().unwrap();
    let agent_home = home.path().join("agent");
    fs::create_dir_all(agent_home.join("memory")).unwrap();
    fs::set_permissions(&agent_home, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(agent_home.join("memory"), fs::Permissions::from_mode(0o755)).unwrap();
    let self_memory = agent_home.join("memory/self.md");
    fs::write(&self_memory, b"existing").unwrap();
    fs::set_permissions(&self_memory, fs::Permissions::from_mode(0o644)).unwrap();

    ensure_agent_home_layout(&agent_home).unwrap();

    assert_eq!(mode_of(&agent_home), 0o700);
    assert_eq!(mode_of(&agent_home.join("memory")), 0o700);
    assert_eq!(mode_of(&self_memory), 0o600);
    assert_eq!(fs::read(&self_memory).unwrap(), b"existing");
}
