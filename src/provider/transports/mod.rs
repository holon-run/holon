mod anthropic;
mod gemini;
mod openai;

use anyhow::{Context, Result};
use reqwest::Client;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{env, time::Duration};

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub(crate) use openai::OpenAiCompactionPolicy;
#[cfg(test)]
pub(crate) use openai::OpenAiResponsesTransportContract;
#[cfg(test)]
pub(crate) use openai::{
    accumulate_chat_completion_stream_events, build_chat_completion_messages,
    build_chat_completion_request, build_openai_input, build_openai_responses_request,
    classify_openai_chat_completion_error, parse_chat_completion_response, parse_openai_response,
};
pub use openai::{OpenAiChatCompletionsProvider, OpenAiCodexProvider, OpenAiProvider};

const DEFAULT_REQUEST_SEND_TIMEOUT_SECS: u64 = 300;
const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
#[cfg(test)]
static STREAM_IDLE_TIMEOUT_OVERRIDE_MS: AtomicU64 = AtomicU64::new(0);

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
    #[cfg(test)]
    {
        let override_ms = STREAM_IDLE_TIMEOUT_OVERRIDE_MS.load(Ordering::Relaxed);
        if override_ms > 0 {
            return Duration::from_millis(override_ms);
        }
    }
    let timeout_ms = env::var("HOLON_PROVIDER_STREAM_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}

#[cfg(test)]
pub(crate) fn set_stream_idle_timeout_override_for_tests(timeout_ms: Option<u64>) {
    STREAM_IDLE_TIMEOUT_OVERRIDE_MS.store(timeout_ms.unwrap_or(0), Ordering::Relaxed);
}

pub(super) fn request_send_timeout() -> Duration {
    let timeout_secs = env::var("HOLON_PROVIDER_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_REQUEST_SEND_TIMEOUT_SECS);
    Duration::from_secs(timeout_secs)
}
