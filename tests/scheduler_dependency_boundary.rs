use std::{fs, path::Path};

const NORMAL_QUEUE_TRANSITION: &str = "pub(crate) struct QueueTransitionCommand";
const NORMAL_QUEUE_TRANSITION_END: &str = "pub(crate) struct ExecutionProtocolTransition";
const CANONICAL_SCHEDULER_EXECUTOR: &str = "src/runtime/scheduler_executor.rs";

#[test]
fn retired_scheduler_wire_types_are_not_a_domain_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let domain = fs::read_to_string(root.join("src/domain/mod.rs")).expect("read domain modules");
    assert!(
        !domain.contains("scheduler_protocol"),
        "retired scheduler protocol must not remain a public domain module"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/runtime_db/mod.rs"))
            .expect("read runtime database modules")
            .lines()
            .filter(|line| line.trim() == "mod legacy_scheduler_wire;")
            .count(),
        1,
        "retired scheduler wire types must remain private to runtime_db"
    );
}

#[test]
fn normal_queue_transition_does_not_carry_legacy_scheduler_payload() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/runtime_db/transitions.rs"))
        .expect("read runtime transitions");
    let normal_start = source
        .find(NORMAL_QUEUE_TRANSITION)
        .expect("normal queue transition definition");
    let normal_end = source[normal_start..]
        .find(NORMAL_QUEUE_TRANSITION_END)
        .map(|offset| normal_start + offset)
        .expect("normal queue transition definition end");
    let normal_definition = &source[normal_start..normal_end];

    assert!(
        !normal_definition.contains("scheduler_protocol"),
        "normal QueueTransitionCommand must not carry legacy scheduler protocol payload"
    );
    assert!(
        !source.contains("commit_queue_with_legacy_scheduler_protocol")
            && !source.contains("LegacySchedulerProtocolTransition")
            && !source.contains("scheduler_protocol_repository"),
        "retired scheduler protocol queue commits and repository must not remain available"
    );
}

#[test]
fn canonical_scheduler_executor_does_not_reference_legacy_scheduler_protocol() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join(CANONICAL_SCHEDULER_EXECUTOR))
        .expect("read canonical scheduler executor");

    assert!(
        !source.contains("scheduler_protocol"),
        "{CANONICAL_SCHEDULER_EXECUTOR} must not reference legacy scheduler_protocol"
    );
}
