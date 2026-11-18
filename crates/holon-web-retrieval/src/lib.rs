//! Web search and page retrieval from resolved provider configuration.

pub mod fetch;
pub mod policy;
mod remote_fetch;
pub mod search;

pub use holon_config::web_config::*;

#[cfg(test)]
mod test_support;

#[cfg(test)]
use test_support::test_tool_error;
