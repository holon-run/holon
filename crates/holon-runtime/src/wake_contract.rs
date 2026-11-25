//! Apply the runtime read budget to the selected wait evidence.

use crate::diagnostics::resource_io::admit_cloned_value;
use crate::types::{MessageEnvelope, WaitConditionRecord};
use anyhow::Result;
use holon_domain::wake_matching::{self, WaitTriggerSelection as BorrowedWaitTriggerSelection};

pub(crate) use holon_domain::agent_message_reply_reference;
pub(crate) use holon_domain::wake_matching::{
    authoritative_wait_correlation, has_operator_queue_authority, is_agent_message,
    matching_wake_source, trigger_wait_order, WakeMatch,
};

pub(crate) type WaitTriggerSelection<'a> = wake_matching::WaitTriggerSelection<'a, String>;

pub(crate) fn select_wait_to_trigger<'a>(
    message: &'a MessageEnvelope,
    conditions: &'a [WaitConditionRecord],
) -> Result<WaitTriggerSelection<'a>> {
    Ok(
        match wake_matching::select_wait_to_trigger(message, conditions) {
            BorrowedWaitTriggerSelection::Match { condition, wake } => {
                WaitTriggerSelection::Match { condition, wake }
            }
            BorrowedWaitTriggerSelection::StaleExact { wait_id } => {
                admit_cloned_value(&wait_id)?;
                WaitTriggerSelection::StaleExact {
                    wait_id: wait_id.to_owned(),
                }
            }
            BorrowedWaitTriggerSelection::NoMatch => WaitTriggerSelection::NoMatch,
        },
    )
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
        MessageOrigin, Priority, WaitConditionKind, WaitConditionStatus, WakeSource,
    };
    use chrono::Utc;

    mod selector_budget_tests;

    fn task_wait(id: &str, work_item_id: Option<&str>, task_id: &str) -> WaitConditionRecord {
        let now = Utc::now();
        WaitConditionRecord {
            id: id.into(),
            agent_id: "agent-a".into(),
            work_item_id: work_item_id.map(ToString::to_string),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: Some(task_id.into()),
            waiting_for: format!("waiting for {task_id}"),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: task_id.into(),
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some("turn-wait".into()),
            trigger_message_id: None,
            triggered_at: None,
        }
    }

    fn task_result(task_id: &str, work_item_id: Option<&str>) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "agent-a",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task_id.into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "completed".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        message.work_item_id = work_item_id.map(ToString::to_string);
        message.turn_id = Some("turn-trigger".into());
        message
    }
}
