mod anthropic;
#[cfg(test)]
mod dispatch_guard_tests;
mod gemini;
mod http;
mod openai;
mod timeouts;

use anyhow::{Context, Result};
use reqwest::Client;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use http::{read_error_response_body, send_provider_request, RequestDeadline};
use timeouts::http_timeouts;
#[cfg(test)]
pub(crate) use timeouts::{HttpTimeouts, HTTP_TIMEOUTS_OVERRIDE};

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub(crate) use openai::OpenAiBearerAuth;
pub(crate) use openai::OpenAiCompactionPolicy;
#[cfg(test)]
pub(crate) use openai::OpenAiResponsesTransportContract;
#[cfg(test)]
pub(crate) use openai::{
    accumulate_chat_completion_stream_events, build_chat_completion_messages,
    build_chat_completion_request, build_openai_input, build_openai_responses_request,
    classify_openai_chat_completion_error, parse_chat_completion_response, parse_openai_response,
    send_chat_completion_stream_request,
};
pub use openai::{OpenAiChatCompletionsProvider, OpenAiCodexProvider, OpenAiProvider};

#[cfg(test)]
static STREAM_IDLE_TIMEOUT_OVERRIDE_MS: AtomicU64 = AtomicU64::new(0);

fn build_http_client() -> Result<Client> {
    build_provider_http_client(Some(http_timeouts().connect))
}

fn build_oauth_http_client() -> Result<Client> {
    // Refresh previously shared only the optional legacy total cap.
    build_provider_http_client(None)
}

fn build_provider_http_client(connect: Option<Duration>) -> Result<Client> {
    let timeouts = http_timeouts();
    // Every model dispatch must pass through the explicit admission hook.
    let mut builder = Client::builder()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none());
    if let Some(connect) = connect {
        builder = builder.connect_timeout(connect);
    }
    if let Some(timeout) = timeouts.total {
        builder = builder.timeout(timeout);
    }
    builder.build().context("failed to build HTTP client")
}

pub(super) fn stream_idle_timeout() -> Duration {
    #[cfg(test)]
    if let Ok(idle) = HTTP_TIMEOUTS_OVERRIDE.try_with(|timeouts| timeouts.idle) {
        return idle;
    }
    #[cfg(test)]
    {
        let override_ms = STREAM_IDLE_TIMEOUT_OVERRIDE_MS.load(Ordering::Relaxed);
        if override_ms > 0 {
            return Duration::from_millis(override_ms);
        }
    }
    http_timeouts().idle
}

#[cfg(test)]
pub(crate) fn set_stream_idle_timeout_override_for_tests(timeout_ms: Option<u64>) {
    STREAM_IDLE_TIMEOUT_OVERRIDE_MS.store(timeout_ms.unwrap_or(0), Ordering::Relaxed);
}

pub(super) fn response_body_timeout() -> Duration {
    http_timeouts().body
}
