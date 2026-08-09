use std::{fs, path::Path};

const CANONICAL_SCHEDULER_CONSUMERS: &[&str] = &[
    "src/context/mod.rs",
    "src/runtime.rs",
    "src/runtime/scheduler.rs",
    "src/runtime/scheduler_executor.rs",
    "src/runtime/tests/message_dispatch.rs",
    "src/runtime/tests/scheduler.rs",
    "src/runtime/turn/execution.rs",
    "src/types.rs",
];

const NEUTRAL_SCHEDULER_TYPES: &[&str] =
    &["ScenarioMode", "SchedulerOwner", "SchedulerScenarioClass"];

#[test]
fn canonical_scheduler_consumers_do_not_import_neutral_types_from_legacy_protocol() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    for relative_path in CANONICAL_SCHEDULER_CONSUMERS {
        let source = fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("failed to read {relative_path}: {error}"));
        for (index, line) in source.lines().enumerate() {
            if line.contains("scheduler_protocol")
                && NEUTRAL_SCHEDULER_TYPES
                    .iter()
                    .any(|type_name| line.contains(type_name))
            {
                violations.push(format!("{relative_path}:{}: {line}", index + 1));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "canonical scheduler consumers must import neutral scheduler types from \
         crate::domain::scheduler, not legacy scheduler_protocol:\n{}",
        violations.join("\n")
    );
}

#[test]
fn legacy_scheduler_protocol_reexports_neutral_types_without_wire_changes() {
    let owner: holon::domain::scheduler::SchedulerOwner =
        holon::domain::scheduler_protocol::SchedulerOwner::WorkItem {
            work_item_id: "work-a".to_string(),
        };
    let scenario: holon::domain::scheduler::SchedulerScenarioClass =
        holon::domain::scheduler_protocol::SchedulerScenarioClass::ExactWaitResume;
    let mode: holon::domain::scheduler::ScenarioMode =
        holon::domain::scheduler_protocol::ScenarioMode::Authoritative;

    assert_eq!(
        serde_json::to_value(owner).unwrap(),
        serde_json::json!({"kind": "work_item", "work_item_id": "work-a"})
    );
    assert_eq!(
        serde_json::to_value(scenario).unwrap(),
        serde_json::json!("exact_wait_resume")
    );
    assert_eq!(
        serde_json::to_value(mode).unwrap(),
        serde_json::json!("authoritative")
    );
}
