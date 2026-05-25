//! Model-facing built-in tool schema inventory drift test.
//!
//! Refresh workflow for intentional tool surface changes:
//!
//! ```bash
//! cargo test --test tool_schema_inventory_snapshot refresh_tool_schema_inventory_snapshot -- --ignored
//! cargo test --test tool_schema_inventory_snapshot
//! ```

const SNAPSHOT_PATH: &str = "docs/website/reference/model-tool-schema-inventory.json";

#[test]
fn tool_schema_inventory_snapshot_matches_generated_inventory() {
    let live = serde_json::to_string_pretty(
        &holon::tool::model_tool_schema_inventory().expect("generate tool schema inventory"),
    )
    .expect("serialize generated tool schema inventory");
    let stored = std::fs::read_to_string(SNAPSHOT_PATH)
        .unwrap_or_else(|err| panic!("failed to read {SNAPSHOT_PATH}: {err}"));

    if live.replace("\r\n", "\n") != stored.replace("\r\n", "\n") {
        eprintln!(
            "Tool schema inventory drift detected. Refresh intentionally with:\n  cargo test --test tool_schema_inventory_snapshot refresh_tool_schema_inventory_snapshot -- --ignored\n"
        );
        eprintln!("=== GENERATED TOOL SCHEMA INVENTORY ===");
        eprintln!("{live}");
        panic!("generated tool schema inventory does not match checked-in snapshot");
    }
}

#[test]
#[ignore]
fn refresh_tool_schema_inventory_snapshot() {
    let live = serde_json::to_string_pretty(
        &holon::tool::model_tool_schema_inventory().expect("generate tool schema inventory"),
    )
    .expect("serialize generated tool schema inventory");
    std::fs::write(SNAPSHOT_PATH, live).expect("write tool schema inventory snapshot");
}
