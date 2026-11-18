//! Guard: persisted execution outcomes have exactly one decode entry.
//!
//! The retired WorkItem `needs_repair` encoding is tolerated only by the
//! migration decoder. A read path that decodes `ExecutionOutcomeRecord`
//! straight from `execution_protocol_outcomes.payload_json` re-splits the store
//! into strict and tolerant readers, which is the #4254 regression.

use std::{fs, path::Path, path::PathBuf};

const SINGLE_ENTRY: &str = "src/runtime_db/outcome_reads.rs";

const FORBIDDEN: &[&str] = &[
    "from_str::<ExecutionOutcomeRecord>",
    "from_str::<crate::domain::execution_protocol::ExecutionOutcomeRecord>",
    "from_str::<execution_protocol::ExecutionOutcomeRecord>",
    ": ExecutionOutcomeRecord = serde_json::from_str",
    ": crate::domain::execution_protocol::ExecutionOutcomeRecord = serde_json::from_str",
    ": execution_protocol::ExecutionOutcomeRecord = serde_json::from_str",
];

#[test]
fn persisted_outcomes_decode_only_through_the_migration_entry() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    for file in rust_sources(&root.join("src/runtime_db")) {
        let relative = file
            .strip_prefix(root)
            .expect("runtime database source is inside the crate")
            .to_string_lossy()
            .into_owned();
        if relative == SINGLE_ENTRY {
            continue;
        }
        let source = fs::read_to_string(&file).expect("read runtime database source");
        let production = production_prefix(&source);
        for (index, line) in production.lines().enumerate() {
            if FORBIDDEN.iter().any(|pattern| line.contains(pattern)) {
                violations.push(format!("{relative}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "persisted execution outcomes must decode through {SINGLE_ENTRY}:\n{}",
        violations.join("\n")
    );
}

/// Inline test modules sit at the end of a file; a bare `#[cfg(test)] mod
/// tests;` declaration near the top leaves the rest of the file in production.
fn production_prefix(source: &str) -> &str {
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        if line.trim() == "#[cfg(test)]" {
            let rest = &source[offset + line.len()..];
            let next = rest.lines().find(|line| !line.trim().is_empty());
            if next.is_some_and(|line| line.trim_end().ends_with('{')) {
                return &source[..offset];
            }
        }
        offset += line.len();
    }
    source
}

fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).expect("read runtime database directory") {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            files.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}
