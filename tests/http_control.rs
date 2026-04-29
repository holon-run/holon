mod support;

macro_rules! http_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_control::$name().await
            }
        )+
    };
}

http_async_tests!(
    control_prompt_is_open_on_loopback_auto,
    agent_state_route_returns_aggregated_snapshot,
    agent_state_route_includes_bootstrap_projection_fields_when_present,
    control_agent_model_override_set_and_clear_updates_status,
    control_prompt_requires_bearer_token_when_required,
    control_wake_records_liveness_only_system_tick_on_loopback_auto,
    control_prompt_requires_bearer_token_for_non_loopback_auto,
    control_prompt_records_message_admission_fields,
    control_prompt_rejects_stopped_agent_without_queueing,
    stopped_status_includes_lifecycle_resume_guidance,
    control_wake_rejects_stopped_agent_with_resume_guidance,
    control_resume_restores_live_runtime_loop_for_stopped_agent,
    daemon_shutdown_restart_preserves_public_agent_http_runnability,
    runtime_status_route_reports_runtime_metadata,
    runtime_status_route_reports_waiting_activity_summary,
    runtime_status_route_reports_last_runtime_failure_summary,
    runtime_shutdown_route_requests_shutdown,
);

#[cfg(unix)]
http_async_tests!(control_prompt_is_open_over_unix_socket_auto,);
