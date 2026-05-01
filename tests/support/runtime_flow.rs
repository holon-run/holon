#![allow(dead_code)]

// Legacy compatibility shim: keep old `runtime_flow` entrypoint available by
// re-exporting the focused runtime test implementations.
//
// The concrete behavior lives in domain-owned support modules:
// - runtime_waiting
// - runtime_tasks
// - runtime_compaction
// - runtime_subagents

pub use crate::support::runtime_compaction::*;
pub use crate::support::runtime_subagents::*;
pub use crate::support::runtime_tasks::*;
pub use crate::support::runtime_waiting::*;
