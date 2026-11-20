use super::*;
use holon_domain::runtime_event::{EVENT_CONTRACT_VERSION_HEADER, RUNTIME_EVENT_CONTRACT_VERSION};
use tokio_stream::StreamExt;

#[tokio::test(start_paused = true)]
async fn runtime_event_stream_sends_exact_heartbeat_and_closes_after_buffered_frames() {
    let (tx, rx) = mpsc::channel(1);
    let response = runtime_event_stream(rx);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
    assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");
    assert_eq!(
        response.headers()[EVENT_CONTRACT_VERSION_HEADER],
        RUNTIME_EVENT_CONTRACT_VERSION.to_string()
    );
    let mut frames = response.into_body().into_data_stream();
    assert_eq!(frames.next().await.unwrap().unwrap(), ": heartbeat\n\n");
    let payload = Bytes::from_static(b"id: 1\ndata: {}\n\n");
    tx.send(Ok(payload.clone())).await.unwrap();
    drop(tx);
    assert_eq!(frames.next().await.unwrap().unwrap(), payload);
    assert!(frames.next().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn axum_event_stream_sends_configured_heartbeat() {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(1);
    let response = heartbeat_sse(rx, Duration::from_secs(15));
    let start = tokio::time::Instant::now();
    let mut frames = response.into_body().into_data_stream();
    assert_eq!(frames.next().await.unwrap().unwrap(), ": heartbeat\n\n");
    assert_eq!(start.elapsed(), Duration::from_secs(15));
    drop(tx);
    assert!(frames.next().await.is_none());
}
