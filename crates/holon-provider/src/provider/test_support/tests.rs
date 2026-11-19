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
