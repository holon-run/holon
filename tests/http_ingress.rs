mod support;

macro_rules! http_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_ingress::$name().await
            }
        )+
    };
}

http_async_tests!(
    generic_webhook_records_public_admission_fields,
    public_channel_enqueue_rejects_stopped_agent_without_queueing,
    generic_webhook_rejects_stopped_public_agent_without_queueing,
    generic_webhook_and_multi_agent_listing_work,
    public_enqueue_rejects_privileged_origin_and_trust_override,
);
