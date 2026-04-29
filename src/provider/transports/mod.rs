mod anthropic;
mod openai;

use anyhow::{Context, Result};
use reqwest::Client;

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

fn build_http_client() -> Result<Client> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("failed to build HTTP client")
}
