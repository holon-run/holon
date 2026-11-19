use super::*;

#[tokio::test]
async fn phase_timeout_connect_budget_bounds_stalled_tls_handshake() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (_connection, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let mut timeouts = HttpTimeouts::from_values(None, None, None);
    timeouts.connect = Duration::from_millis(100);
    HTTP_TIMEOUTS_OVERRIDE
        .scope(timeouts, async {
            let client = super::super::build_http_client().unwrap();
            let error = tokio::time::timeout(Duration::from_millis(500), client.get(url).send())
                .await
                .expect("TLS setup must obey connect timeout")
                .unwrap_err();
            assert!(error.is_connect(), "{error:?}");
            assert!(error.is_timeout(), "{error:?}");
        })
        .await;
    server.abort();
}

#[test]
fn phase_timeout_defaults_and_legacy_precedence() {
    let defaults = HttpTimeouts::from_values(None, None, None);
    assert_eq!(defaults.connect, Duration::from_secs(30));
    assert_eq!(defaults.text_headers, Duration::from_secs(90));
    assert_eq!(defaults.request, Duration::from_secs(300));
    assert_eq!(defaults.body, Duration::from_secs(300));
    assert_eq!(defaults.idle, Duration::from_secs(300));
    assert_eq!(defaults.total, None);
    assert_eq!(
        HttpTimeouts::from_values(Some(0), Some(0), Some(0)),
        defaults
    );
    let legacy = HttpTimeouts::from_values(Some(12), Some(15), Some(17));
    assert_eq!(legacy.text_headers, Duration::from_secs(12));
    assert_eq!(legacy.request, Duration::from_secs(12));
    assert_eq!(legacy.total, Some(Duration::from_secs(12)));
    assert_eq!(legacy.body, Duration::from_secs(15));
    assert_eq!(legacy.idle, Duration::from_millis(17));
    assert_eq!(
        HttpTimeouts::from_values(Some(12), None, None).body,
        Duration::from_secs(12)
    );
}
