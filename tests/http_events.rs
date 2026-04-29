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
    events_route_supports_cursor_replay,
    events_route_supports_last_event_id_header_and_rfc3339_ts,
    events_route_with_missing_cursor_returns_refresh_hint,
);
