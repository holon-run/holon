//! Multi-profile system prompt and tool-description topology drift test.
//!
//! Refresh workflow for intentional prompt or tool-guidance changes:
//!
//! ```bash
//! make snapshots-refresh
//! make snapshots-check
//! ```

const SNAPSHOT_PATH: &str = "tests/snapshots/prompt_topology_inventory.json";

#[test]
fn prompt_topology_snapshot_matches_generated_inventory() {
    let live = serde_json::to_string_pretty(
        &holon::prompt::prompt_topology_inventory().expect("generate prompt topology inventory"),
    )
    .expect("serialize generated prompt topology inventory")
        + "\n";
    let stored = std::fs::read_to_string(SNAPSHOT_PATH).unwrap_or_else(|err| {
        panic!(
            "failed to read {SNAPSHOT_PATH}: {err}\n\
            Hint: refresh intentionally with `make snapshots-refresh`"
        )
    });

    if live.replace("\r\n", "\n") != stored.replace("\r\n", "\n") {
        eprintln!(
            "Prompt topology inventory drift detected. Refresh intentionally with:\n  make snapshots-refresh\n"
        );
        eprintln!("=== GENERATED PROMPT TOPOLOGY INVENTORY ===");
        eprintln!("{live}");
        panic!("generated prompt topology inventory does not match checked-in snapshot");
    }
}

#[test]
#[ignore]
fn refresh_prompt_topology_snapshot() {
    let live = serde_json::to_string_pretty(
        &holon::prompt::prompt_topology_inventory().expect("generate prompt topology inventory"),
    )
    .expect("serialize generated prompt topology inventory")
        + "\n";
    std::fs::create_dir_all("tests/snapshots").expect("create tests/snapshots directory");
    std::fs::write(SNAPSHOT_PATH, live).expect("write prompt topology inventory snapshot");
}
