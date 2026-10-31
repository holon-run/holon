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
fn scheduler_repair_inspection_exposes_revision_faults_and_retained_sources() {
    let openapi = holon::openapi::generate_openapi_json();
    let schemas = &openapi["components"]["schemas"];
    let inspection = &schemas["SchedulerRepairInspection"];
    for field in ["source_revision_drifts", "source_revision_repair_blocks"] {
        assert_eq!(inspection["properties"][field]["type"], "array");
        assert_eq!(inspection["properties"][field]["items"]["type"], "object");
    }
    let drift = &inspection["properties"]["source_revision_drifts"]["items"];
    let repair_block = &inspection["properties"]["source_revision_repair_blocks"]["items"];
    assert_eq!(
        drift["required"],
        serde_json::json!([
            "work_item_id",
            "generation",
            "source_revision",
            "durable_revision"
        ])
    );
    assert_eq!(repair_block["properties"]["drift"], *drift);
    for field in [
        "drift_present",
        "retained_source_count",
        "retained_source_disposition",
        "alert_created_event_seq",
    ] {
        assert!(repair_block["properties"][field].is_object());
    }
}

#[test]
fn task_observability_contract_includes_targets_and_terminal_query() {
    let openapi = holon::openapi::generate_openapi_json();
    let schemas = &openapi["components"]["schemas"];
    let tasks = &openapi["paths"]["/api/agents/{agent_id}/tasks"]["get"];
    assert_eq!(
        tasks["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/TaskListResponse"
    );
    let status = tasks["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "status")
        .unwrap();
    assert_eq!(
        status["schema"]["enum"],
        serde_json::json!(["active", "terminal"])
    );
    assert!(schemas["TaskRecordDto"]["properties"]["target"].is_object());
    assert!(schemas["SlimTaskDto"]["properties"]["target"].is_object());
    assert!(schemas["TaskStatusSnapshot"]["properties"]["target"].is_object());
    assert!(schemas["ControlPromptRequest"]["properties"]["expected_incarnation"].is_object());
}

#[test]
fn timezone_clear_uses_a_dedicated_request_schema() {
    let openapi = holon::openapi::generate_openapi_json();
    for (resource, request) in [
        ("timezone", "ClearAgentTimezoneRequest"),
        ("model", "ClearAgentModelRequest"),
    ] {
        let path = format!("/api/control/agents/{{agent_id}}/{resource}/clear");
        assert_eq!(
            openapi["paths"][&path]["post"]["requestBody"]["content"]["application/json"]["schema"]
                ["$ref"],
            format!("#/components/schemas/{request}")
        );
        assert!(openapi["components"]["schemas"][request].is_object());
    }
}

#[test]
fn openapi_snapshot_matches_generated_schema() {
    let live = serde_json::to_string_pretty(&holon::openapi::generate_openapi_json())
        .expect("serialize generated OpenAPI");
    let stored = std::fs::read_to_string(SNAPSHOT_PATH)
        .unwrap_or_else(|err| panic!("failed to read {SNAPSHOT_PATH}: {err}"));

    // A trailing newline on the checked-in file is not schema drift.
    if live.replace("\r\n", "\n").trim_end() != stored.replace("\r\n", "\n").trim_end() {
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
    std::fs::write(SNAPSHOT_PATH, format!("{live}\n")).expect("write OpenAPI snapshot");
}
