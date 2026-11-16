//! Holon's terminal client and operator presentation.
mod tui;
mod tui_markdown;

pub mod onboarding;
pub mod operator_event;
pub mod presentation;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support;
pub use tui::run_tui;

use holon_client as client;
use holon_client::http_dto;
use holon_config as config;
use holon_domain as types;
use holon_domain::{runtime_event, work_item_scheduling};
use holon_system as system;
