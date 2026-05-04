mod anthropic;
mod openai;

use anyhow::{Context, Result};
use reqwest::Client;
use std::{env, time::Duration};

pub use anthropic::AnthropicProvider;
#[cfg(test)]
pub(crate) use openai::OpenAiResponsesTransportContract;
#[cfg(test)]
pub(crate) use openai::{
    accumulate_chat_completion_stream_events, build_chat_completion_messages,
    build_chat_completion_request, build_openai_input, build_openai_responses_request,
    classify_openai_chat_completion_error, parse_chat_completion_response, parse_openai_response,
};
pub use openai::{OpenAiChatCompletionsProvider, OpenAiCodexProvider, OpenAiProvider};

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;

fn build_http_client() -> Result<Client> {
    let timeout_secs = env::var("HOLON_PROVIDER_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0);
    let mut builder = Client::builder();
    if let Some(timeout_secs) = timeout_secs {
        builder = builder.timeout(Duration::from_secs(timeout_secs));
    }
    builder.build().context("failed to build HTTP client")
}

pub(super) fn stream_idle_timeout() -> Duration {
    let timeout_ms = env::var("HOLON_PROVIDER_STREAM_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}
