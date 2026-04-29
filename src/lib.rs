pub mod agent_template;
pub mod agents_md;
mod auth;
mod callbacks;

pub mod brief;
pub mod client;
pub mod config;
pub mod context;
pub mod daemon;
pub mod host;
mod host_registry;
pub mod http;
pub mod ingress;
pub mod memory;
pub mod model_catalog;
pub mod policy;
pub mod prompt;
pub mod provider;
pub mod queue;
pub mod run_once;
pub mod runtime;
pub mod skills;
pub mod solve;
pub mod storage;
pub mod system;
pub mod tool;
pub mod tui;
mod tui_markdown;
pub mod types;

#[cfg(test)]
mod worktree_tests;
