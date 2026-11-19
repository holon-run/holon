use super::*;

#[test]
fn obsolete_display_preferences_are_ignored_and_not_saved_again() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("local.json");
    fs::write(&path, r#"{"last_selected_agent_id":"beta","display":{"per_agent":{"beta":"debug"}},"updated_at":"2026-01-01T00:00:00Z"}"#).unwrap();
    let state = TuiClientState::load(&path).unwrap();
    state.save(&path).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["last_selected_agent_id"], "beta");
    assert!(value.get("display").is_none());
}

#[test]
fn state_round_trip_preserves_selected_agent() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state").join("tui").join("local.json");
    let state = TuiClientState::new("agent-beta");

    state.save(&path).unwrap();
    let loaded = TuiClientState::load(&path).unwrap();

    assert_eq!(loaded.last_selected_agent_id, "agent-beta");
}

#[test]
fn state_loads_legacy_selected_agent_only_json() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.json");
    fs::write(
        &path,
        r#"{
  "last_selected_agent_id": "agent-beta",
  "updated_at": "2026-01-02T03:04:05Z"
}"#,
    )
    .unwrap();

    let loaded = TuiClientState::load(&path).unwrap();

    assert_eq!(loaded.last_selected_agent_id, "agent-beta");
}
