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
    agent_list_entries_are_slim_for_tui_bootstrap,
    agent_list_entries_tolerate_unloaded_agent_with_corrupt_work_queue,
    json_responses_support_gzip_without_compressing_sse,
    local_client_over_http_can_read_agent_state_snapshot,
    local_client_over_http_can_stream_events_with_cursor_query,
    local_client_over_http_stream_without_cursor_starts_at_tail,
);

#[cfg(unix)]
http_async_tests!(
    local_client_over_unix_socket_can_poll_without_http_fallback,
    local_client_over_unix_socket_can_read_agent_state_snapshot,
    local_client_over_unix_socket_can_stream_events_with_cursor_query,
    local_client_over_unix_socket_stream_without_cursor_starts_at_tail,
);
