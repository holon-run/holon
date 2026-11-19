//! HTTP transport and response handling shared by Holon's server endpoints.

mod layers;
mod listener;
mod response;
pub mod web;

pub use layers::apply_http_layers;
pub use listener::serve_tcp;
#[cfg(unix)]
pub use listener::serve_unix;
pub use response::{
    add_retry_after_to_service_unavailable, etag_for_bytes, if_none_match_satisfied,
    prevent_agent_read_caching, warn_if_slow_json_response,
};
