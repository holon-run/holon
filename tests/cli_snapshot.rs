//! CLI command-tree snapshot / contract test.
//!
//! ## Purpose
//!
//! Lock the full CLI shape — command paths, positional arguments, flags with
//! their defaults and possible values, and visible aliases — so that
//! accidental changes are caught in CI.
//!
//! ## Refresh workflow
//!
//! When a deliberate CLI change is made, regenerate the snapshot file:
//!
//! ```bash
//! cargo test --test cli_snapshot test_cli_snapshot_matches -- --include-ignored
//! ```
//!
//! The test will fail and print the expected JSON. Copy the printed JSON into
//! `tests/snapshots/cli_command_tree.json`, then run again to confirm:
//!
//! ```bash
//! cargo test --test cli_snapshot
//! ```
//!
//! The `--refresh` helper (hidden behind `--ignored`) overwrites the snapshot
//! file automatically. Use it only when you are sure the new shape is
//! intentional:
//!
//! ```bash
//! cargo test --test cli_snapshot refresh_cli_snapshot -- --ignored
//! ```

use holon::cli;

const SNAPSHOT_PATH: &str = "tests/snapshots/cli_command_tree.json";

/// Assert the live command tree matches the checked-in snapshot.
#[test]
fn test_cli_snapshot_matches() {
    let entries = cli::collect_snapshot();
    let live = serde_json::to_string_pretty(&entries).expect("serialize snapshot");

    let stored = std::fs::read_to_string(SNAPSHOT_PATH)
        .unwrap_or_else(|e| panic!("failed to read snapshot at {SNAPSHOT_PATH}: {e}\n\
            Hint: create the initial snapshot by running `cargo test --test cli_snapshot refresh_cli_snapshot -- --ignored`"));

    // Normalise line endings so the test works cross-platform.
    let live_normalised = live.replace("\r\n", "\n");
    let stored_normalised = stored.replace("\r\n", "\n");

    if live_normalised != stored_normalised {
        eprintln!(
            "CLI snapshot mismatch!\n\n\
            --- STORED  ({SNAPSHOT_PATH})\n\
            +++ LIVE\n\
            \n\
            If the live version is correct, refresh the snapshot:\n\
              cargo test --test cli_snapshot refresh_cli_snapshot -- --ignored\n"
        );
        // Print the live version so it can be used as the new snapshot.
        eprintln!("=== EXPECTED SNAPSHOT ===");
        eprintln!("{live}");
        panic!("CLI command-tree snapshot does not match. See stderr for the expected snapshot.");
    }
}

/// Overwrite the snapshot file with the current live command tree.
///
/// Marked `#[ignore]` so it only runs on explicit request:
///
/// ```bash
/// cargo test --test cli_snapshot refresh_cli_snapshot -- --ignored
/// ```
#[test]
#[ignore]
fn refresh_cli_snapshot() {
    let entries = cli::collect_snapshot();
    let live = serde_json::to_string_pretty(&entries).expect("serialize snapshot");
    std::fs::create_dir_all("tests/snapshots").expect("create snapshots dir");
    std::fs::write(SNAPSHOT_PATH, &live).expect("write snapshot");
    eprintln!(
        "Snapshot written to {SNAPSHOT_PATH} ({entries_len} entries)",
        entries_len = entries.len()
    );
}
