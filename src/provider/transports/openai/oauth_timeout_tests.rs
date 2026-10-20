use super::*;
use crate::provider::transports::{HttpTimeouts, HTTP_TIMEOUTS_OVERRIDE};
use std::time::Duration;

#[tokio::test]
async fn phase_timeout_oauth_clients_do_not_inherit_inference_connect_budget() {
    for total in [None, Some(Duration::from_millis(120))] {
        let timeouts = HttpTimeouts {
            connect: Duration::from_millis(50),
            request: Duration::from_millis(500),
            text_headers: Duration::from_millis(100),
            body: Duration::from_millis(500),
            idle: Duration::from_millis(500),
            total,
        };
        HTTP_TIMEOUTS_OVERRIDE
            .scope(timeouts, async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("https://{}", listener.local_addr().unwrap());
                let server = tokio::spawn(async move {
                    let mut connections = Vec::new();
                    loop {
                        let (socket, _) = listener.accept().await.unwrap();
                        connections.push(socket); // Accept TCP, but never complete TLS.
                    }
                });
                let home = tempfile::tempdir().unwrap();
                let codex = OpenAiCodexProvider::from_runtime_config(
                &test_openai_codex_config(Some(json!({
                    "tokens":{"access_token":"test","refresh_token":"test","account_id":"acct_test"}
                }).to_string())),
                "gpt-5.4", 1024, home.path(), true,
            ).unwrap();
                let oauth = OpenAiProvider::from_runtime_config(
                    &test_xai_oauth_config(json!({"access_token":"test"}).to_string()),
                    "grok-test",
                    1024,
                    home.path(),
                )
                .unwrap();
                for (client, inference) in [
                    (codex.client.clone(), true),
                    (codex.oauth_client.clone(), false),
                    (oauth.client.clone(), true),
                    (oauth.auth.client.clone(), false),
                ] {
                    let result =
                        tokio::time::timeout(Duration::from_millis(300), client.get(&url).send())
                            .await;
                    if inference || total.is_some() {
                        let error = result.expect("client deadline must fire").unwrap_err();
                        assert!(error.is_timeout(), "{error:?}");
                        if inference {
                            assert!(error.is_connect(), "{error:?}");
                        }
                    } else {
                        assert!(
                            result.is_err(),
                            "OAuth must retain its previous unbounded connect budget"
                        );
                    }
                }
                server.abort();
            })
            .await;
    }
}
