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
    runtime_search_route_returns_memory_search_results,
    runtime_search_route_filters_memory_results_by_agent_ids,
    agent_brief_route_returns_full_brief_by_id,
    agent_state_route_scopes_work_items_to_requested_agent,
    agent_state_route_includes_bootstrap_projection_fields_when_present,
    list_skills_includes_all_agent_skill_roots,
    agent_skills_endpoint_uses_effective_registry_snapshot,
    agent_skills_endpoint_does_not_leak_stale_roots_between_agents,
    skills_catalog_returns_global_user_library_only,
    install_skill_existing_destination_returns_conflict,
    add_skill_to_catalog_existing_destination_returns_conflict,
    skill_library_add_remove_and_agent_enable_disable_are_separate,
    skill_library_update_and_check_reconcile_lock_file,
    control_agent_model_override_set_and_clear_updates_status,
    control_prompt_requires_bearer_token_when_required,
    remote_tcp_surfaces_require_bearer_token_when_required,
    control_wake_records_liveness_only_system_tick_on_loopback_auto,
    control_prompt_requires_bearer_token_for_non_loopback_auto,
    control_prompt_records_message_admission_fields,
    control_prompt_rejects_stopped_agent_without_queueing,
    stopped_status_includes_lifecycle_start_guidance,
    control_wake_rejects_stopped_agent_with_start_guidance,
    control_start_restores_live_runtime_loop_for_stopped_agent,
    daemon_shutdown_restart_preserves_public_agent_http_runnability,
    runtime_status_route_reports_runtime_metadata,
    runtime_readiness_route_omits_activity_summary,
    runtime_config_route_reads_and_updates_persisted_runtime_config,
    cors_preflight_allows_default_localhost_origins,
    cors_preflight_allows_default_put_credentials_route,
    cors_preflight_respects_configured_origin,
    runtime_status_route_reports_waiting_activity_summary,
    runtime_status_route_reports_last_runtime_failure_summary,
    runtime_shutdown_route_requests_shutdown,
);

#[cfg(unix)]
http_async_tests!(
    control_prompt_is_open_over_unix_socket_auto,
    control_runtime_status_is_open_over_unix_socket_when_auth_required,
    control_runtime_readiness_is_open_over_unix_socket_when_auth_required,
);
