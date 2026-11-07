use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::{material::tests::response, ProviderImageData, ProviderRequestMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

#[tokio::test]
async fn image_turn_http_body_and_response_share_explicit_acquisition() -> Result<()> {
    for transport in ["responses", "codex", "chat"] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "image-turn".into(),
            identity_incarnation: 1,
        };
        let source = ResourceReadMaterial::new(&ledger, &scope)?;
        let material = ProviderRequestMaterial::scoped(source.clone());
        let encoded = ProviderImageData::encode(b"abc", &source)?;
        let url = encoded.data_url("image/png")?;
        assert_eq!(url, "data:image/png;base64,YWJj");
        let request = json!({"model":"test","input":[{"image_url":url}]});
        let output = if transport == "chat" {
            json!({"choices":[{"message":{"role":"assistant","content":"done"}}]})
        } else {
            json!({"id":"resp-turn","status":"completed","output":[
                {"type":"message","role":"assistant","content":[
                    {"type":"output_text","text":"done"}
                ]}
            ]})
        };
        let body = if transport == "codex" {
            format!(
                "data: {}\n\n",
                json!({"type":"response.completed","response":output})
            )
        } else {
            output.to_string()
        };
        let (url, server) = response(
            body.as_bytes(),
            if transport == "codex" {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .await?;
        let parsed = match transport {
            "responses" => {
                send_openai_responses_request_with_material(
                    &Client::new(),
                    url,
                    request,
                    Vec::new(),
                    None,
                    None,
                    "test",
                    "test",
                    None,
                    &material,
                )
                .await?
            }
            "codex" => {
                send_openai_responses_streaming_request_with_material(
                    &Client::new(),
                    url,
                    &request,
                    Vec::new(),
                    None,
                    None,
                    "test",
                    "test",
                    None,
                    &material,
                )
                .await?
            }
            _ => {
                send_chat_completion_request_with_material(
                    &Client::new(),
                    url,
                    request,
                    Vec::new(),
                    None,
                    None,
                    None,
                    &material,
                )
                .await?
            }
        };
        server.await??;
        let response = material
            .complete_turn(async { Ok(parsed.response) })
            .await?;
        assert!(
            matches!(response.blocks.as_slice(), [ModelBlock::Text { text }] if text == "done")
        );
        let weak = Arc::downgrade(&source.guard()?);
        let last = response.clone();
        drop((response, source, material, encoded));
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}
