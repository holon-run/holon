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
    create_command_task_route_no_longer_denies_integration_trust,
    create_command_task_route_accepts_command_request,
    create_work_item_route_persists_queued_item_without_message_ingress,
    create_work_item_route_does_not_replace_existing_active_item,
    create_work_item_route_rejects_empty_delivery_target_with_bad_request,
);
