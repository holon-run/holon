#[path = "support/runtime_waiting.rs"]
mod runtime_waiting;

mod support;

macro_rules! runtime_async_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                runtime_waiting::$name().await
            }
        )*
    };
}

macro_rules! http_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_callback::$name().await
            }
        )+
    };
}

macro_rules! operator_ingress_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_operator_ingress::$name().await
            }
        )+
    };
}

runtime_async_tests!(
    timer_tick_wakes_sleeping_session,
    wake_hint_coalesces_while_running_and_reenters_once,
    paused_agent_ignores_wake_hint,
    notify_operator_records_default_public_and_private_child_targets,
    notify_operator_does_not_stop_same_turn_tool_execution,
    notify_operator_prefers_reply_route_for_delivery,
    notify_operator_ignores_reply_route_when_binding_no_longer_matches,
    notify_operator_falls_back_to_default_route_without_reply_route,
    callback_tools_register_and_revoke_waiting_state,
    timer_wait_surfaces_waiting_reason,
);

http_async_tests!(
    callback_enqueue_message_repeats_until_cancelled,
    callback_wake_hint_routes_through_wake_hint,
    callback_wake_hint_rejects_stopped_public_agent_without_side_effects,
    unknown_callback_token_is_rejected,
    callback_mode_mismatch_is_rejected,
    invalid_json_callback_body_returns_bad_request,
    wake_callback_without_content_type_accepts_binary_body,
    callback_enqueue_rejects_stopped_public_agent_after_restart,
);

operator_ingress_async_tests!(
    operator_ingress_records_remote_operator_provenance,
    operator_ingress_defaults_provider_provenance_from_binding,
    operator_ingress_requires_control_auth,
    operator_transport_binding_validates_delivery_auth_and_redacts_audit,
    operator_ingress_validates_binding_and_actor,
    operator_ingress_rejects_stopped_agent_without_queueing,
    operator_notification_delivery_callback_records_acceptance,
    operator_notification_delivery_callback_records_failed_submit,
);
