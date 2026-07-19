//! Stable runtime status enum inventory drift test.
//!
//! Refresh workflow for intentional status contract changes:
//!
//! ```bash
//! make snapshots-refresh
//! make snapshots-check
//! ```

const SNAPSHOT_PATH: &str = "docs/website/reference/runtime-status-enum-inventory.json";

#[test]
fn runtime_status_enum_inventory_snapshot_matches_generated_inventory() {
    let live = serde_json::to_string_pretty(
        &holon::contract_inventory::runtime_status_enum_inventory()
            .expect("generate runtime status enum inventory"),
    )
    .expect("serialize generated runtime status enum inventory")
        + "\n";
    let stored = std::fs::read_to_string(SNAPSHOT_PATH)
        .unwrap_or_else(|err| panic!("failed to read {SNAPSHOT_PATH}: {err}"));

    if live.replace("\r\n", "\n") != stored.replace("\r\n", "\n") {
        eprintln!(
            "Runtime status enum inventory drift detected. Refresh intentionally with:\n  make snapshots-refresh\n"
        );
        eprintln!("=== GENERATED RUNTIME STATUS ENUM INVENTORY ===");
        eprintln!("{live}");
        panic!("generated runtime status enum inventory does not match checked-in snapshot");
    }
}

#[test]
#[ignore]
fn refresh_runtime_status_enum_inventory_snapshot() {
    let live = serde_json::to_string_pretty(
        &holon::contract_inventory::runtime_status_enum_inventory()
            .expect("generate runtime status enum inventory"),
    )
    .expect("serialize generated runtime status enum inventory")
        + "\n";
    std::fs::write(SNAPSHOT_PATH, live).expect("write runtime status enum inventory snapshot");
}
