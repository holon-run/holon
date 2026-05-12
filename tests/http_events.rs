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
    events_route_operator_projection_preserves_tool_payload,
    events_route_operator_projection_preserves_assistant_round_payload,
    events_route_operator_projection_preserves_workspace_payload,
    state_snapshot_seeds_projected_events_tail_and_stream_resumes_after_cursor,
    state_snapshot_bounds_large_projection_fields,
    events_route_local_debug_projection_requires_control_auth,
    events_stream_with_missing_cursor_returns_not_found,
);
