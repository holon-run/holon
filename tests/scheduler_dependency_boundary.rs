use std::{
    fs,
    path::{Path, PathBuf},
};

const NEUTRAL_SCHEDULER_TYPES: &[&str] =
    &["ScenarioMode", "SchedulerOwner", "SchedulerScenarioClass"];
const SCHEDULER_DOMAIN_MODULES: &[&str] = &[
    "src/domain/scheduler.rs",
    "src/domain/scheduler_protocol.rs",
];
const NORMAL_QUEUE_TRANSITION: &str = "pub(crate) struct QueueTransitionCommand";
const LEGACY_QUEUE_TRANSITION: &str = "pub(crate) struct LegacySchedulerProtocolTransition";
const CANONICAL_SCHEDULER_EXECUTOR: &str = "src/runtime/scheduler_executor.rs";

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
    {
        let path = entry
            .unwrap_or_else(|error| {
                panic!(
                    "failed to read entry under {}: {error}",
                    directory.display()
                )
            })
            .path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

fn imports_neutral_type_from_legacy_protocol(statement: &str) -> bool {
    let compact = statement
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let mut remainder = compact.as_str();

    while let Some(protocol_index) = remainder.find("scheduler_protocol::") {
        let protocol_import = &remainder[protocol_index + "scheduler_protocol::".len()..];
        if let Some(group) = protocol_import.strip_prefix('{') {
            let mut depth = 1;
            let group_end = group.char_indices().find_map(|(index, character)| {
                match character {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index);
                        }
                    }
                    _ => {}
                }
                None
            });
            let Some(group_end) = group_end else {
                return false;
            };
            let group = &group[..group_end];
            if NEUTRAL_SCHEDULER_TYPES
                .iter()
                .any(|type_name| group.split(',').any(|item| item.starts_with(type_name)))
            {
                return true;
            }
        } else if NEUTRAL_SCHEDULER_TYPES
            .iter()
            .any(|type_name| protocol_import.starts_with(type_name))
        {
            return true;
        }

        remainder = protocol_import;
    }

    false
}

#[test]
fn canonical_scheduler_consumers_do_not_import_neutral_types_from_legacy_protocol() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    let mut violations = Vec::new();
    collect_rust_sources(&root.join("src"), &mut sources);
    sources.sort();

    for path in sources {
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or_else(|error| panic!("failed to relativize {}: {error}", path.display()))
            .to_string_lossy();
        if SCHEDULER_DOMAIN_MODULES.contains(&relative_path.as_ref()) {
            continue;
        }

        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {relative_path}: {error}"));
        let mut import = None::<(usize, String)>;
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            if import.is_none() && (trimmed.starts_with("use ") || trimmed.starts_with("pub use "))
            {
                import = Some((index + 1, String::new()));
            }
            if let Some((start_line, statement)) = import.as_mut() {
                statement.push_str(line);
                statement.push('\n');
                if line.contains(';') {
                    if imports_neutral_type_from_legacy_protocol(statement) {
                        violations.push(format!(
                            "{relative_path}:{start_line}: {}",
                            statement.trim()
                        ));
                    }
                    import = None;
                }
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

#[test]
fn normal_queue_transition_does_not_carry_legacy_scheduler_payload() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/runtime_db/transitions.rs"))
        .expect("read runtime transitions");
    let normal_start = source
        .find(NORMAL_QUEUE_TRANSITION)
        .expect("normal queue transition definition");
    let legacy_start = source[normal_start..]
        .find(LEGACY_QUEUE_TRANSITION)
        .map(|offset| normal_start + offset)
        .expect("legacy queue transition definition");
    let normal_definition = &source[normal_start..legacy_start];

    assert!(
        !normal_definition.contains("scheduler_protocol"),
        "normal QueueTransitionCommand must not carry legacy scheduler protocol payload"
    );
    assert!(
        source.contains("commit_queue_with_legacy_scheduler_protocol"),
        "legacy queue and scheduler protocol commits must use an explicit compatibility boundary"
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
