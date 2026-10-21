use super::super::*;
use crate::provider::transports::{HttpTimeouts, HTTP_TIMEOUTS_OVERRIDE};
use axum::{response::IntoResponse, routing::post, Json, Router};

#[tokio::test]
async fn image_stream_byte_limit_covers_unterminated_events_and_comments() {
    for body in [
        format!("data: {}", "x".repeat(16_384)),
        ": keepalive\n\n".repeat(4096),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/",
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = Client::new()
            .post(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let error = super::parse::read_openai_streaming_response(response, None, Some(1024))
            .await
            .unwrap_err();
        server.abort();
        let error = error.downcast_ref::<crate::tool::ToolError>().unwrap();
        assert_eq!(error.kind, "image_resource_limit");
        assert_eq!(
            error.details.as_ref().unwrap()["generation_may_have_succeeded"],
            true
        );
        assert!(!error.retryable);
    }

    let (url, server) = serve(
        json!({
            "type": "response.completed",
            "response": {"id": "ok", "status": "completed", "output": []}
        }),
        true,
    )
    .await;
    let response = Client::new().post(url).send().await.unwrap();
    let output = super::parse::read_openai_streaming_response(response, None, Some(1024))
        .await
        .unwrap();
    server.abort();
    assert_eq!(output["id"], "ok");
}

async fn serve(body: Value, streaming: bool) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/",
        post(move || {
            let body = body.clone();
            async move {
                // Later than text headers, earlier than the existing operation budget.
                tokio::time::sleep(Duration::from_millis(200)).await;
                if streaming {
                    (
                        [("content-type", "text/event-stream")],
                        format!("event: response.completed\ndata: {body}\n\n"),
                    )
                        .into_response()
                } else {
                    Json(body).into_response()
                }
            }
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}/"), task)
}

#[tokio::test]
async fn phase_timeout_openai_operations_preserve_non_text_headers_budgets() {
    let timeouts = HttpTimeouts {
        connect: Duration::from_millis(100),
        request: Duration::from_millis(800),
        text_headers: Duration::from_millis(50),
        body: Duration::from_millis(800),
        idle: Duration::from_millis(800),
        total: None,
    };
    HTTP_TIMEOUTS_OVERRIDE.scope(timeouts, async {
        let client = build_http_client().unwrap();
        let response = json!({"id":"resp_test","status":"completed","output":[{
            "type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]
        }]});
        let (url, server) = serve(response, false).await;
        send_openai_responses_request(
            &client, url, json!({"model":"test"}), vec![], None, None, "openai", "openai/test",
        ).await.expect("non-streaming Responses retains its existing deadline");
        server.abort();

        let (url, server) = serve(json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}), false).await;
        chat::send_chat_completion_request(
            &client, url, json!({"model":"test"}), vec![], None, None,
        ).await.expect("Chat retains its existing deadline");
        server.abort();

        let (url, server) = serve(json!({"data":[{"b64_json":"b2s="}]}), false).await;
        images::send_openai_images_request(
            &client, url, json!({"model":"test"}), vec![], "openai", None, None,
        ).await.expect("Images JSON retains its send-only deadline");
        server.abort();

        let image = json!({"type":"response.completed","response":{
            "id":"resp_image","status":"completed","output":[{
                "type":"image_generation_call","id":"img_1","status":"completed","result":"b2s="
            }, {
                "type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]
            }]
        }});
        let (url, server) = serve(image, true).await;
        images::send_openai_codex_image_generation_request(
            &client, url, json!({"model":"test","tools":[{"type":"image_generation"}]}),
            vec![], None, None,
        ).await.expect("Responses image tool retains its send-only deadline");
        server.abort();

        let (url, server) = serve(json!({"output":[{"type":"compaction","encrypted_content":"opaque"}]}), false).await;
        super::compaction::send_openai_compact_request(
            &client, url, json!({"model":"test"}), vec![], None, None, "openai", "openai/test",
        ).await.expect("compaction retains its full-request deadline");
        server.abort();
    }).await;
}
