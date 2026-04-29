use super::*;
use anyhow::anyhow;
use serde_json::Value;
use std::sync::OnceLock;
use tokio::time::Duration;

use crate::types::{
    OperatorDeliveryRecord, OperatorDeliveryStatus, OperatorDeliveryTriggerKind,
    OperatorNotificationBoundary, OperatorNotificationRecord, OperatorTransportBinding,
    OperatorTransportBindingStatus, OperatorTransportDeliveryAuthKind,
};

const OPERATOR_NOTIFICATION_SUMMARY_LIMIT: usize = 160;
const OPERATOR_DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
static OPERATOR_DELIVERY_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

impl RuntimeHandle {
    pub async fn notify_operator(&self, message: String) -> Result<OperatorNotificationRecord> {
        let message = validate_operator_notification_message(message)?;
        let requested_by_agent_id = self.agent_id().await?;
        let identity = self.agent_identity_view().await?;
        let (target_operator_boundary, target_parent_agent_id) =
            operator_notification_target(&identity);
        let work_item_id = self
            .agent_state()
            .await?
            .current_turn_work_item_id
            .or_else(|| {
                self.inner
                    .storage
                    .waiting_contract_anchor()
                    .ok()
                    .flatten()
                    .map(|item| item.id)
            });
        let record = OperatorNotificationRecord {
            notification_id: Uuid::new_v4().to_string(),
            agent_id: target_parent_agent_id
                .clone()
                .unwrap_or_else(|| requested_by_agent_id.clone()),
            requested_by_agent_id,
            target_operator_boundary,
            target_parent_agent_id,
            summary: operator_notification_summary(&message),
            message,
            work_item_id,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        };
        self.persist_operator_notification(&record)?;
        if let Some(target_parent_agent_id) = record.target_parent_agent_id.as_deref() {
            if target_parent_agent_id != record.requested_by_agent_id {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    let mirrored = bridge
                        .record_operator_notification(target_parent_agent_id, &record)
                        .await;
                    if let Err(error) = mirrored {
                        self.append_audit_event(
                            "operator_notification_mirror_failed",
                            serde_json::json!({
                                "notification_id": record.notification_id,
                                "requested_by_agent_id": record.requested_by_agent_id,
                                "target_parent_agent_id": target_parent_agent_id,
                                "error": error.to_string(),
                            }),
                        )?;
                    } else {
                        bridge
                            .submit_operator_notification_delivery(target_parent_agent_id, &record)
                            .await?;
                    }
                }
            } else {
                self.submit_operator_notification_delivery(&record).await?;
            }
        } else {
            self.submit_operator_notification_delivery(&record).await?;
        }
        Ok(record)
    }

    pub(crate) fn persist_operator_notification(
        &self,
        record: &OperatorNotificationRecord,
    ) -> Result<()> {
        self.inner.storage.append_operator_notification(record)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "operator_notification_requested",
            to_json_value(record),
        ))?;
        Ok(())
    }

    pub async fn recent_operator_notifications(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorNotificationRecord>> {
        self.inner.storage.read_recent_operator_notifications(limit)
    }

    pub async fn upsert_operator_transport_binding(
        &self,
        mut binding: OperatorTransportBinding,
    ) -> Result<OperatorTransportBinding> {
        let now = Utc::now();
        if binding.created_at.timestamp_millis() == 0 {
            binding.created_at = now;
        }
        if binding.status != OperatorTransportBindingStatus::Active {
            return Err(anyhow!(
                "operator transport binding must be active when created"
            ));
        }
        self.inner
            .storage
            .append_operator_transport_binding(&binding)?;
        self.append_audit_event(
            "operator_transport_binding_upserted",
            operator_transport_binding_audit_value(&binding),
        )?;
        Ok(binding)
    }

    pub async fn latest_operator_transport_bindings(
        &self,
    ) -> Result<Vec<OperatorTransportBinding>> {
        self.inner.storage.latest_operator_transport_bindings()
    }

    pub async fn active_operator_transport_binding(
        &self,
        binding_id: &str,
    ) -> Result<Option<OperatorTransportBinding>> {
        Ok(self
            .inner
            .storage
            .latest_operator_transport_bindings()?
            .into_iter()
            .find(|binding| {
                binding.binding_id == binding_id
                    && binding.status == OperatorTransportBindingStatus::Active
            }))
    }

    pub async fn default_operator_transport_binding(
        &self,
    ) -> Result<Option<OperatorTransportBinding>> {
        let agent_id = self.agent_id().await?;
        Ok(self
            .inner
            .storage
            .latest_operator_transport_bindings()?
            .into_iter()
            .find(|binding| {
                binding.target_agent_id == agent_id
                    && binding.status == OperatorTransportBindingStatus::Active
            }))
    }

    pub async fn recent_operator_delivery_records(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorDeliveryRecord>> {
        self.inner
            .storage
            .read_recent_operator_delivery_records(limit)
    }

    pub async fn submit_operator_notification_delivery(
        &self,
        notification: &OperatorNotificationRecord,
    ) -> Result<Option<OperatorDeliveryRecord>> {
        let agent_state = self.agent_state().await?;
        let preferred_binding = match agent_state.current_turn_operator_binding_id.as_deref() {
            Some(binding_id) => self.active_operator_transport_binding(binding_id).await?,
            None => None,
        };
        let binding_matches_turn = preferred_binding.is_some();
        let Some(binding) = preferred_binding.or(self.default_operator_transport_binding().await?)
        else {
            return Ok(None);
        };
        let desired_route_id = if binding_matches_turn {
            agent_state.current_turn_operator_reply_route_id.clone()
        } else {
            None
        };
        let route_id = if let Some(route_id) = desired_route_id.as_deref() {
            if binding.default_route_id == route_id {
                binding.default_route_id.clone()
            } else {
                route_id.to_string()
            }
        } else {
            binding.default_route_id.clone()
        };
        let delivery_intent_id = format!("odi_{}", Uuid::new_v4().simple());
        let now = Utc::now();
        let mut record = OperatorDeliveryRecord {
            delivery_intent_id: delivery_intent_id.clone(),
            output_event_id: notification.notification_id.clone(),
            agent_id: notification.agent_id.clone(),
            route_id,
            binding_id: binding.binding_id.clone(),
            trigger_kind: OperatorDeliveryTriggerKind::OperatorNotification,
            status: OperatorDeliveryStatus::Pending,
            transport_delivery_id: None,
            failure_summary: None,
            created_at: now,
            updated_at: now,
        };
        self.inner
            .storage
            .append_operator_delivery_record(&record)?;
        self.append_audit_event("operator_delivery_submitted", to_json_value(&record))?;

        record = submit_delivery_intent(&binding, notification, record).await;
        self.inner
            .storage
            .append_operator_delivery_record(&record)?;
        self.append_audit_event("operator_delivery_completed", to_json_value(&record))?;
        Ok(Some(record))
    }
}

async fn submit_delivery_intent(
    binding: &OperatorTransportBinding,
    notification: &OperatorNotificationRecord,
    mut record: OperatorDeliveryRecord,
) -> OperatorDeliveryRecord {
    let payload = serde_json::json!({
        "delivery_intent_id": record.delivery_intent_id,
        "binding_id": binding.binding_id,
        "route_id": record.route_id,
        "target_agent_id": record.agent_id,
        "kind": "operator_output",
        "text": notification.message,
        "created_at": record.created_at,
        "correlation_id": notification.correlation_id,
        "causation_id": notification.causation_id,
    });
    let client = operator_delivery_client();
    let mut request = client
        .post(&binding.delivery_callback_url)
        .header("Idempotency-Key", &record.delivery_intent_id)
        .json(&payload);
    match binding.delivery_auth.kind {
        OperatorTransportDeliveryAuthKind::Bearer => {
            let Some(token) = binding.delivery_auth.bearer_token.as_deref() else {
                mark_delivery_failed(
                    &mut record,
                    "bearer delivery auth missing bearer_token".into(),
                );
                return record;
            };
            request = request.bearer_auth(token);
        }
        OperatorTransportDeliveryAuthKind::Hmac => {
            mark_delivery_failed(
                &mut record,
                "hmac delivery auth is not supported until signing is implemented".into(),
            );
            return record;
        }
    }

    match request.send().await {
        Ok(response) if response.status().is_success() => {
            let transport_delivery_id = response.json::<Value>().await.ok().and_then(|value| {
                value
                    .get("transport_delivery_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });
            record.status = OperatorDeliveryStatus::AcceptedByTransport;
            record.transport_delivery_id = transport_delivery_id;
            record.updated_at = Utc::now();
        }
        Ok(response) => {
            mark_delivery_failed(
                &mut record,
                format!("transport callback returned HTTP {}", response.status()),
            );
        }
        Err(error) => {
            mark_delivery_failed(&mut record, error.to_string());
        }
    }
    record
}

fn operator_delivery_client() -> &'static reqwest::Client {
    OPERATOR_DELIVERY_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(OPERATOR_DELIVERY_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn mark_delivery_failed(record: &mut OperatorDeliveryRecord, failure_summary: String) {
    record.status = OperatorDeliveryStatus::FailedToSubmit;
    record.failure_summary = Some(crate::tool::helpers::truncate_text(&failure_summary, 240));
    record.updated_at = Utc::now();
}

fn operator_transport_binding_audit_value(binding: &OperatorTransportBinding) -> Value {
    let mut value = to_json_value(binding);
    if let Some(delivery_auth) = value
        .get_mut("delivery_auth")
        .and_then(Value::as_object_mut)
    {
        delivery_auth.remove("bearer_token");
    }
    value
}

fn validate_operator_notification_message(message: String) -> Result<String> {
    if message.trim().is_empty() {
        return Err(anyhow!(
            "NotifyOperator `message` must be a non-empty string"
        ));
    }
    Ok(message)
}

fn operator_notification_target(
    identity: &AgentIdentityView,
) -> (OperatorNotificationBoundary, Option<String>) {
    if identity.profile_preset == crate::types::AgentProfilePreset::PrivateChild
        && identity.kind == AgentKind::Child
    {
        return (
            OperatorNotificationBoundary::ParentSupervisor,
            identity.parent_agent_id.clone(),
        );
    }
    (OperatorNotificationBoundary::PrimaryOperator, None)
}

fn operator_notification_summary(message: &str) -> String {
    let first_line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    crate::tool::helpers::truncate_text(first_line, OPERATOR_NOTIFICATION_SUMMARY_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_notification_summary_uses_first_non_empty_line() {
        assert_eq!(
            operator_notification_summary("\n\n  First useful line  \nsecond"),
            "First useful line"
        );
    }

    #[test]
    fn operator_notification_message_rejects_whitespace_only_input() {
        let error = validate_operator_notification_message(" \n\t ".into()).unwrap_err();
        assert!(error.to_string().contains("must be a non-empty string"));
    }
}
