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
//! make snapshots-refresh
//! ```
//!
//! Review the generated diff, then run the unified check:
//!
//! ```bash
//! make snapshots-check
//! ```

use holon::cli;

const SNAPSHOT_PATH: &str = "tests/snapshots/cli_command_tree.json";

/// Assert the live command tree matches the checked-in snapshot.
#[test]
fn test_cli_snapshot_matches() {
    let entries = cli::collect_snapshot();
    let live = serde_json::to_string_pretty(&entries).expect("serialize snapshot");

    let stored = std::fs::read_to_string(SNAPSHOT_PATH).unwrap_or_else(|e| {
        panic!(
            "failed to read snapshot at {SNAPSHOT_PATH}: {e}\n\
            Hint: create the initial snapshot by running `make snapshots-refresh`"
        )
    });

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
              make snapshots-refresh\n"
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
/// make snapshots-refresh
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
