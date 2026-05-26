mod support;

macro_rules! http_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_tasks::$name().await
            }
        )+
    };
}

http_async_tests!(
    create_command_task_route_rejects_legacy_kind_field,
    create_task_route_rejects_unknown_prompt_field,
    create_command_task_route_rejects_continue_on_result_field,
    create_command_task_route_accepts_integration_authority,
    create_command_task_route_accepts_command_request,
    tasks_and_state_routes_return_active_latest_tasks_only,
    task_status_and_output_routes_return_task_lifecycle_snapshots,
    task_input_and_stop_routes_manage_task_lifecycle,
    create_work_item_route_persists_queued_item_without_message_ingress,
    work_item_routes_list_and_return_work_item_detail,
    create_work_item_route_does_not_replace_existing_active_item,
    create_work_item_route_rejects_empty_objective_with_bad_request,
    work_item_mutation_routes_pick_update_and_complete,
    work_item_mutation_routes_validate_bad_requests,
    timer_detail_route_returns_latest_timer_record,
);
