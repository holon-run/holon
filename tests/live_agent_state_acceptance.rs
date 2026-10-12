//! Bounded real-provider/runtime acceptance. Ignored unless explicitly selected.
//!
//! HOLON_STATE_ACCEPTANCE=1 HOLON_STATE_ACCEPTANCE_ARM=baseline \
//! HOLON_STATE_ACCEPTANCE_REVISION=<40-char-sha> \
//! HOLON_STATE_ACCEPTANCE_REPORT=/absolute/path/baseline.json \
//! cargo test --test live_agent_state_acceptance live_agent_state_acceptance -- --ignored --exact
//!
//! The same runner/manifest is used for `new`. Route defaults to local config;
//! HOLON_STATE_ACCEPTANCE_ROUTE may select one explicit configured route.
//! No production Agent is activated, and keys never enter the fixture provider.

#[path = "support/agent_state_live.rs"]
mod agent_state_live;
mod support;

use anyhow::{ensure, Result};

#[tokio::test]
#[ignore = "requires explicit live opt-in; at most 12 real wire sends"]
async fn live_agent_state_acceptance() -> Result<()> {
    ensure!(
        std::env::var("HOLON_STATE_ACCEPTANCE").as_deref() == Ok("1"),
        "set HOLON_STATE_ACCEPTANCE=1 to enable billable acceptance"
    );
    agent_state_live::run_live().await
}

#[tokio::test]
async fn forwarding_gateway_enforces_wire_budget_and_rejects_redirects() -> Result<()> {
    agent_state_live::verify_gateway_limits().await
}

#[tokio::test]
async fn offline_acceptance_script_uses_real_host_and_fixed_manifest() -> Result<()> {
    agent_state_live::verify_offline_script().await
}
