mod support;

macro_rules! http_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_client::$name().await
            }
        )+
    };
}

http_async_tests!(
    local_client_over_http_can_read_agent_state_snapshot,
    local_client_over_http_can_stream_events_with_since_query,
    local_client_over_http_can_stream_events_with_last_event_id_header,
);

#[cfg(unix)]
http_async_tests!(
    local_client_over_unix_socket_can_poll_without_http_fallback,
    local_client_over_unix_socket_can_read_agent_state_snapshot,
    local_client_over_unix_socket_can_stream_events_with_since_query,
    local_client_over_unix_socket_can_stream_events_with_last_event_id_header,
);
