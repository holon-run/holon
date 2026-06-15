mod support;

macro_rules! http_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_events::$name().await
            }
        )+
    };
}

http_async_tests!(
    events_route_supports_cursor_pagination,
    events_route_supports_cursor_replay,
    events_stream_supports_cursor_and_rfc3339_ts,
    events_route_preserves_replay_provenance,
    events_route_payload_includes_full_fields,
    events_route_max_level_filters_with_bounded_visible_pages,
    events_stream_includes_tool_payload,
    tool_execution_route_returns_canonical_output,
    events_stream_includes_assistant_round_payload,
    events_stream_preserves_raw_payload_with_control_auth,
    events_page_cursor_seq_seeds_stream_resume,
    state_snapshot_bounds_large_projection_fields,
    events_stream_requires_control_auth_when_configured,
    events_stream_with_missing_cursor_returns_not_found,
);
