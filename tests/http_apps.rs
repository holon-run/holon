mod support;

macro_rules! http_apps_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_apps::$name().await
            }
        )+
    };
}

http_apps_tests!(
    apps_discovery_and_static_hosting,
    apps_allow_same_app_id_across_agents,
    apps_reject_unknown_agent_and_app,
    apps_reject_invalid_manifest_and_missing_entry,
    apps_serve_arbitrary_asset_types,
    apps_manifest_defaults_and_invalid_files,
    apps_static_http_and_size_settings,
    apps_session_boundary_for_static_assets,
    apps_asset_capacity_shared_and_released,
    apps_settings_follow_existing_config_reload,
    apps_reject_path_traversal,
    apps_reject_symlink_escape,
    apps_sdk_request_and_events,
    apps_sdk_event_replay_order,
    apps_reference_workbench_end_to_end,
);
