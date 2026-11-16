//! CLI integration for the standalone terminal client.
use anyhow::Result;

use crate::{client::LocalClient, config::AppConfig};

pub async fn run_tui(
    config: AppConfig,
    no_alt_screen: bool,
    client: Option<LocalClient>,
    agent: Option<String>,
) -> Result<()> {
    let log_root = config.log_root_dir();
    let alt_screen_mode = config.tui_alternate_screen;
    let client = match client {
        Some(client) => client,
        None => LocalClient::new(config)?,
    };
    holon_tui::run_tui(client, log_root, alt_screen_mode, no_alt_screen, agent).await
}
