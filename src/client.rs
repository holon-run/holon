//! Application wiring for the client.
use crate::config::AppConfig;
pub use holon_client::*;

impl From<AppConfig> for ConnectionConfig {
    fn from(config: AppConfig) -> Self {
        Self {
            home_dir: config.home_dir,
            default_agent_id: config.default_agent_id,
            socket_path: config.socket_path,
            control_token: config.control_token,
            http_addr: config.http_addr,
        }
    }
}
