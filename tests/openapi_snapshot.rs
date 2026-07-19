//! OpenAPI baseline drift test.
//!
//! Refresh workflow for intentional HTTP surface changes:
//!
//! ```bash
//! make snapshots-refresh
//! make snapshots-check
//! ```

const SNAPSHOT_PATH: &str = "docs/website/reference/openapi.json";

#[test]
fn openapi_snapshot_matches_generated_schema() {
    let live = serde_json::to_string_pretty(&holon::openapi::generate_openapi_json())
        .expect("serialize generated OpenAPI");
    let stored = std::fs::read_to_string(SNAPSHOT_PATH)
        .unwrap_or_else(|err| panic!("failed to read {SNAPSHOT_PATH}: {err}"));

    if live.replace("\r\n", "\n") != stored.replace("\r\n", "\n") {
        eprintln!(
            "OpenAPI snapshot drift detected. Refresh intentionally with:\n  make snapshots-refresh\n"
        );
        eprintln!("=== GENERATED OPENAPI ===");
        eprintln!("{live}");
        panic!("generated OpenAPI does not match checked-in snapshot");
    }
}

#[test]
#[ignore]
fn refresh_openapi_snapshot() {
    let live = serde_json::to_string_pretty(&holon::openapi::generate_openapi_json())
        .expect("serialize generated OpenAPI");
    std::fs::write(SNAPSHOT_PATH, live).expect("write OpenAPI snapshot");
}
