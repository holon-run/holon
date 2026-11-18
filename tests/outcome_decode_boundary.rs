//! Guard: persisted execution outcomes have exactly one decode entry.
//!
//! The retired WorkItem `needs_repair` encoding is tolerated only by the
//! migration decoder. A read path that decodes `ExecutionOutcomeRecord`
//! straight from `execution_protocol_outcomes.payload_json` re-splits the store
//! into strict and tolerant readers, which is the #4254 regression.

use std::{fs, path::Path, path::PathBuf};

const SINGLE_ENTRY: &str = "src/runtime_db/outcome_reads.rs";

/// Whitespace-free decode forms that read persisted outcomes strictly. The
/// guard matches against whitespace-stripped source, so a typed binding split
/// across lines or squeezed spacing is still caught. Fully inferred decodes
/// whose record type only appears in a distant signature cannot be caught
/// textually; every persisted-outcome call site in this crate names the record
/// through a typed binding, a turbofish, or the
/// `CanonicalInputEvidence::Outcome` constructor.
const FORBIDDEN: &[&str] = &[
    "from_str::<ExecutionOutcomeRecord>",
    "from_str::<crate::domain::execution_protocol::ExecutionOutcomeRecord>",
    "from_str::<execution_protocol::ExecutionOutcomeRecord>",
    ":ExecutionOutcomeRecord=serde_json::from_str",
    ":crate::domain::execution_protocol::ExecutionOutcomeRecord=serde_json::from_str",
    ":execution_protocol::ExecutionOutcomeRecord=serde_json::from_str",
    "CanonicalInputEvidence::Outcome(serde_json::from_str",
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
        violations.extend(guard_violations(production, &relative));
    }
    assert!(
        violations.is_empty(),
        "persisted execution outcomes must decode through {SINGLE_ENTRY}:\n{}",
        violations.join("\n")
    );
}

/// Report forbidden strict-decode forms with their original line numbers.
fn guard_violations(source: &str, file: &str) -> Vec<String> {
    let mut stripped = String::new();
    let mut offsets = Vec::new();
    for (offset, byte) in source.bytes().enumerate() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        stripped.push(byte as char);
        offsets.push(offset);
    }
    let mut violations = Vec::new();
    for pattern in FORBIDDEN {
        let mut search_from = 0;
        while let Some(found) = stripped[search_from..].find(pattern) {
            let offset = offsets[search_from + found];
            let line = source[..offset].matches('\n').count() + 1;
            violations.push(format!(
                "{file}:{line}: strict outcome decode `{pattern}` outside {SINGLE_ENTRY}"
            ));
            search_from += found + pattern.len();
        }
    }
    violations
}

#[test]
fn guard_detects_the_replaced_strict_decode_call_site_forms() {
    let turbofish = "let rows = payloads.iter().map(|payload| \
                     serde_json::from_str::<ExecutionOutcomeRecord>(payload));";
    let split_typed = "let outcome: ExecutionOutcomeRecord =\n            \
                       serde_json::from_str(borrowed_sql_text(row, 1)?)?;";
    let inferred_constructor =
        "Family::Outcome => CanonicalInputEvidence::Outcome(serde_json::from_str(payload)?),";
    for source in [turbofish, split_typed, inferred_constructor] {
        assert!(
            !guard_violations(source, "src/runtime_db/example.rs").is_empty(),
            "guard must flag the reintroduced call-site form: {source}"
        );
    }
}

#[test]
fn guard_ignores_decodes_of_other_records() {
    for source in [
        "let turn: SourceTurn = serde_json::from_str(&payload)?;",
        "let value = serde_json::from_str::<serde_json::Value>(&payload)?;",
        "let message: MessageEnvelope = serde_json::from_str(&payload)?;",
    ] {
        assert!(
            guard_violations(source, "src/runtime_db/example.rs").is_empty(),
            "guard must not flag other records' decodes: {source}"
        );
    }
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
