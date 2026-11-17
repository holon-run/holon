// Partial libtests retain shared fixtures whose callers live in other shards.
// Production builds and the default complete suite keep the usual lint policy.
#![cfg_attr(
    all(
        test,
        not(all(
            holon_lib_test_shard = "a",
            holon_lib_test_shard = "b",
            holon_lib_test_shard = "c",
            holon_lib_test_shard = "d"
        ))
    ),
    allow(dead_code, unused_imports)
)]

pub mod agent_memory;
pub mod agent_template;
pub mod agents_md;
use holon_provider::auth;
pub mod authentication;
pub mod avatar;
mod callbacks;
pub(crate) use holon_github::{archive as github_archive, client as github};
pub mod ids;
pub mod startup;

pub mod brief;
pub mod calendar;
pub mod cli;
pub mod client;
pub mod cloudflare_tunnel;
pub mod config;
pub mod context;
pub mod contract_inventory;
pub mod daemon;
pub(crate) mod decision_telemetry;
pub mod deletion;
pub mod diagnostics;
pub use holon_observability::{diagnostics_store, observability, openmetrics, otlp_exporter};
pub mod domain;
pub mod fd_limit;
pub mod host;
mod host_registry;
pub mod http;
pub mod http_dto;
pub mod ingress;
pub mod memory;
pub mod model_catalog;
pub mod model_config_migration;
pub mod model_discovery;
pub mod notes_catalog;
pub mod object_query_cache;
pub mod object_resolver;
pub mod oidc;
pub mod onboarding;
pub mod onboarding_tui;
pub mod openapi;
pub use holon_tui::operator_event;
pub mod policy;
pub use holon_tui::presentation;
pub mod projection_eval;
pub mod prompt;
pub mod provider;
pub mod queue;
pub mod resource_policy;
pub mod run_once;
pub mod runtime;
pub mod runtime_db;
pub mod runtime_error;
pub mod runtime_event;
pub mod skills;
pub mod solve;
pub mod storage;
pub mod system;
pub mod tail;
pub(crate) mod token_estimate;
pub mod tool;
pub mod tui;
pub mod types;
use holon_system::utf8;
mod wake_contract;
pub mod web;
pub mod work_item_plan;
pub mod work_item_refs;
pub mod work_item_scheduling;
pub mod x_search;

#[cfg(test)]
mod test_env;

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod worktree_tests;
