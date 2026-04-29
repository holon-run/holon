//! Public provider test-support utilities for integration tests and harnesses.
//!
//! These helpers are intentionally vendor-neutral. They operate at Holon's
//! `AgentProvider` boundary so runtime scheduling tests can stay deterministic
//! without depending on a real LLM or a model-specific HTTP protocol. They are
//! exported from the library so integration tests can use them through the same
//! public crate boundary as downstream harnesses.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

use super::{AgentProvider, ModelBlock, ProviderTurnRequest, ProviderTurnResponse};

#[derive(Debug, Clone)]
pub enum ScriptedProviderStep {
    Response(ProviderTurnResponse),
    Failure(String),
}

impl ScriptedProviderStep {
    pub fn response(response: ProviderTurnResponse) -> Self {
        Self::Response(response)
    }

    pub fn response_blocks(blocks: Vec<ModelBlock>) -> Self {
        Self::Response(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            request_diagnostics: None,
        })
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::response_blocks(vec![ModelBlock::Text { text: text.into() }])
    }

    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self::response_blocks(vec![ModelBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }])
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self::Failure(message.into())
    }

    pub fn with_token_usage(mut self, input_tokens: u64, output_tokens: u64) -> Self {
        if let Self::Response(response) = &mut self {
            response.input_tokens = input_tokens;
            response.output_tokens = output_tokens;
        }
        self
    }
}

impl From<ProviderTurnResponse> for ScriptedProviderStep {
    fn from(response: ProviderTurnResponse) -> Self {
        Self::Response(response)
    }
}

#[derive(Clone)]
pub struct ScriptedAgentProvider {
    inner: Arc<Mutex<ScriptedAgentProviderState>>,
}

#[derive(Default)]
struct ScriptedAgentProviderState {
    steps: VecDeque<ScriptedProviderStep>,
    requests: Vec<ProviderTurnRequest>,
    configured_model_refs: Vec<String>,
}

impl ScriptedAgentProvider {
    pub fn new(steps: impl IntoIterator<Item = ScriptedProviderStep>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ScriptedAgentProviderState {
                steps: steps.into_iter().collect(),
                requests: Vec::new(),
                configured_model_refs: vec!["scripted".into()],
            })),
        }
    }

    pub fn from_responses(responses: impl IntoIterator<Item = ProviderTurnResponse>) -> Self {
        Self::new(responses.into_iter().map(ScriptedProviderStep::Response))
    }

    pub fn with_configured_model_refs(
        self,
        refs: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut state = self.inner.lock().expect("scripted provider mutex poisoned");
        state.configured_model_refs = refs.into_iter().map(Into::into).collect();
        drop(state);
        self
    }

    pub fn configured_model_refs_snapshot(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("scripted provider mutex poisoned")
            .configured_model_refs
            .clone()
    }

    pub fn requests(&self) -> Vec<ProviderTurnRequest> {
        self.inner
            .lock()
            .expect("scripted provider mutex poisoned")
            .requests
            .clone()
    }

    pub fn request_count(&self) -> usize {
        self.inner
            .lock()
            .expect("scripted provider mutex poisoned")
            .requests
            .len()
    }

    pub fn remaining_steps(&self) -> usize {
        self.inner
            .lock()
            .expect("scripted provider mutex poisoned")
            .steps
            .len()
    }
}

impl Default for ScriptedAgentProvider {
    fn default() -> Self {
        Self::new([])
    }
}

#[async_trait]
impl AgentProvider for ScriptedAgentProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let (step, request_count) = {
            let mut state = self.inner.lock().expect("scripted provider mutex poisoned");
            state.requests.push(request);
            let request_count = state.requests.len();
            (state.steps.pop_front(), request_count)
        };

        match step {
            Some(ScriptedProviderStep::Response(response)) => Ok(response),
            Some(ScriptedProviderStep::Failure(message)) => Err(anyhow!(message)),
            None => Err(anyhow!(
                "scripted agent provider exhausted after {request_count} request(s)"
            )),
        }
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("scripted provider mutex poisoned")
            .configured_model_refs
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ConversationMessage;

    fn request() -> ProviderTurnRequest {
        ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("hello".into())],
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn scripted_provider_captures_requests_and_returns_steps_in_order() {
        let provider = ScriptedAgentProvider::new([
            ScriptedProviderStep::text("first"),
            ScriptedProviderStep::text("second"),
        ]);

        let first = provider.complete_turn(request()).await.unwrap();
        let second = provider.complete_turn(request()).await.unwrap();

        assert_eq!(provider.request_count(), 2);
        assert_eq!(provider.remaining_steps(), 0);
        assert!(matches!(
            first.blocks.as_slice(),
            [ModelBlock::Text { text }] if text == "first"
        ));
        assert!(matches!(
            second.blocks.as_slice(),
            [ModelBlock::Text { text }] if text == "second"
        ));
    }

    #[tokio::test]
    async fn scripted_provider_fails_when_script_is_exhausted() {
        let provider = ScriptedAgentProvider::new([]);

        let err = provider.complete_turn(request()).await.unwrap_err();

        assert!(err
            .to_string()
            .contains("scripted agent provider exhausted after 1 request"));
        assert_eq!(provider.request_count(), 1);
    }
}
