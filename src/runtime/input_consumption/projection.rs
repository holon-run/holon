//! Select a recoverable representation before admitting a new source.

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::{
    context::estimate_text_tokens,
    storage::AppStorage,
    types::{AuthorityClass, IdleInputBatchItem, MessageEnvelope, MessageKind, MessageOrigin},
};

use super::{message_text, BATCH_HEADER, INPUT_BATCH_BYTES};
use crate::runtime::RuntimeHandle;

#[derive(Clone, Copy)]
pub(crate) struct InputBudget {
    pub tokens: usize,
    pub bytes: usize,
}

#[cfg(test)]
impl From<usize> for InputBudget {
    fn from(bytes: usize) -> Self {
        Self {
            tokens: usize::MAX,
            bytes,
        }
    }
}

impl InputBudget {
    pub(crate) fn fits(&self, section: &str, content: &str) -> bool {
        estimate_text_tokens(&format!("[{section}]\n{content}")) <= self.tokens
    }

    pub(crate) fn fits_idle(&self, current: &str, pending: Option<&str>) -> bool {
        let pending_tokens = pending.map_or(0, |text| {
            estimate_text_tokens(&format!("[pending_task_results]\n{text}"))
        });
        estimate_text_tokens(&format!("[current_input]\n{current}")).saturating_add(pending_tokens)
            <= self.tokens
    }
}

pub(crate) fn admitted_item(
    storage: &AppStorage,
    agent_id: &str,
    message_id: &str,
) -> Result<Option<IdleInputBatchItem>> {
    let Some(db) = storage.runtime_db()? else {
        return Ok(None);
    };
    Ok(db
        .transitions()
        .load_execution_protocol_state_if_initialized(agent_id)?
        .and_then(|state| {
            state
                .open_attempt()
                .filter(|attempt| attempt.source_message_id.as_deref() == Some(message_id))
                .and_then(|attempt| attempt.input_projection.clone())
        }))
}

impl RuntimeHandle {
    pub(in crate::runtime) async fn prepare_input_projection(
        &self,
        message: &MessageEnvelope,
        state: &crate::types::AgentState,
        continuation: Option<&crate::types::ContinuationResolution>,
    ) -> Result<Option<IdleInputBatchItem>> {
        let mut config = self.current_context_config().await;
        config.prompt_budget_estimated_tokens = config
            .prompt_budget_estimated_tokens
            .checked_sub(crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS)
            .ok_or_else(|| anyhow::anyhow!("context budget cannot reserve runtime time"))?;
        let workspace = self.workspace_view_from_state(state)?;
        let execution = self.execution_snapshot_for_view(
            state.execution_profile.clone(),
            &workspace,
            &state.attached_workspaces,
        );
        let identity = self.agent_identity_view().await?;
        let skills = self.skills_runtime_view_for_state(state, &identity).await?;
        let tokens = crate::context::input_admission_tokens(
            &self.inner.storage,
            state,
            &execution,
            &skills,
            message,
            continuation,
            &config,
            self.agent_home().as_path(),
            None,
        )?;
        persisted_item_after(
            &self.inner.storage,
            message,
            tokens,
            INPUT_BATCH_BYTES.saturating_sub(BATCH_HEADER.len()),
            &format!("[current_input]\n{BATCH_HEADER}"),
        )
    }
}

pub(in crate::runtime) fn persisted_item(
    storage: &AppStorage,
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
) -> Result<Option<IdleInputBatchItem>> {
    persisted_item_after(storage, message, token_limit, byte_limit, "")
}

pub(in crate::runtime) fn persisted_item_after(
    storage: &AppStorage,
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
    preceding: &str,
) -> Result<Option<IdleInputBatchItem>> {
    let Some(original) = storage.read_message_by_id(&message.id)? else {
        return Ok(None);
    };
    if original.agent_id != message.agent_id
        || original.body != message.body
        || original.origin != message.origin
        || original.authority_class != message.authority_class
        || original.routing_context != message.routing_context
        || original.source_refs != message.source_refs
        || original.work_item_id != message.work_item_id
        || original.task_id != message.task_id
    {
        return Ok(None);
    }
    project_item_after(message, token_limit, byte_limit, preceding)
}

#[cfg(test)]
pub(crate) fn project_item(
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
) -> Result<Option<IdleInputBatchItem>> {
    project_item_after(message, token_limit, byte_limit, "")
}

fn project_item_after(
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
    preceding: &str,
) -> Result<Option<IdleInputBatchItem>> {
    project_item_limited(message, token_limit, byte_limit, preceding, None)
}

fn project_item_limited(
    message: &MessageEnvelope,
    token_limit: usize,
    byte_limit: usize,
    preceding: &str,
    preview_chars: Option<usize>,
) -> Result<Option<IdleInputBatchItem>> {
    let source_ref = format!("message:{}", message.id);
    let body = message_text(&message.body);
    let content_version = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&message.body)?)
    );
    let provenance = serde_json::to_string(&serde_json::json!({
        "projection_version": 1,
        "content_version": content_version,
        "id": message.id,
        "agent_id": message.agent_id,
        "created_at": message.created_at,
        "kind": message.kind,
        "origin": message.origin,
        "authority_class": message.authority_class,
        "priority": message.priority,
        "delivery_surface": message.delivery_surface,
        "admission_context": message.admission_context,
        "work_item_id": message.work_item_id,
        "task_id": message.task_id,
        "routing_context": message.routing_context,
        "source_refs": message.source_refs,
        "correlation_id": message.correlation_id,
        "causation_id": message.causation_id,
        "full_metadata_source_ref": source_ref,
    }))?;
    let reply_expectation = crate::context::render_message_reply_expectation_context(message)
        .map(|content| format!("{content}\n"))
        .unwrap_or_default();
    let prefix = format!(
        "- reply_context_id: {}\n  source_ref: {source_ref}\n  provenance: {provenance}\n{reply_expectation}",
        message.id,
    );
    let fits = |text: &str| {
        text.len() <= byte_limit
            && estimate_text_tokens(&format!("{preceding}{text}")) <= token_limit
    };
    let full = format!("{prefix}  representation: full\n  body:\n{body}\n");
    let may_preview = !matches!(message.origin, MessageOrigin::Operator { .. })
        && message.kind != MessageKind::OperatorPrompt
        && (matches!(
            message.authority_class,
            AuthorityClass::ExternalEvidence | AuthorityClass::IntegrationSignal
        ) || message.kind == MessageKind::TaskResult);
    let rendered = if fits(&full)
        && (preview_chars.is_none_or(|limit| limit >= body.chars().count()) || !may_preview)
    {
        full
    } else {
        // Instructions must not become partial executable requests.
        if !may_preview {
            return Ok(None);
        }
        let prefix = format!("{prefix}  representation: bounded_preview\n  body preview:\n");
        let suffix = format!(
            "\n[bounded preview; original not fully read; retrieve with MemoryGet({source_ref})]\n"
        );
        if !fits(&format!("{prefix}{suffix}")) {
            return Ok(None);
        }
        // Search Unicode scalar boundaries; both byte and token limits apply.
        let boundaries: Vec<_> = body
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(body.len()))
            .collect();
        let (mut low, mut high) = (
            0,
            preview_chars
                .unwrap_or(usize::MAX)
                .min(boundaries.len() - 1),
        );
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if fits(&format!("{prefix}{}{suffix}", &body[..boundaries[mid]])) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        format!("{prefix}{}{suffix}", &body[..boundaries[low]])
    };
    Ok(Some(IdleInputBatchItem {
        message_id: message.id.clone(),
        source_ref,
        rendered,
    }))
}

/// Validate the selected content and provenance against the immutable original.
pub(crate) fn validate_projection(
    message: &MessageEnvelope,
    item: &IdleInputBatchItem,
) -> Result<()> {
    let full =
        project_item_after(message, usize::MAX, usize::MAX, "")?.expect("unbounded source fits");
    if *item == full {
        return Ok(());
    }
    let minimum = project_item_limited(message, usize::MAX, usize::MAX, "", Some(0))?
        .expect("unbounded source fits");
    let suffix = format!(
        "\n[bounded preview; original not fully read; retrieve with MemoryGet({})]\n",
        full.source_ref,
    );
    let preview = minimum
        .rendered
        .strip_suffix(&suffix)
        .filter(|prefix| prefix.ends_with("  body preview:\n"))
        .and_then(|prefix| item.rendered.strip_prefix(prefix))
        .and_then(|text| text.strip_suffix(&suffix));
    if item.message_id != message.id
        || item.source_ref != full.source_ref
        || preview.is_none_or(|text| !message_text(&message.body).starts_with(text))
    {
        anyhow::bail!(
            "source projection does not match original message {}",
            message.id
        );
    }
    Ok(())
}

/// Fit linked sources together without losing instruction bodies or metadata.
pub(in crate::runtime) fn persisted_group(
    storage: &AppStorage,
    messages: &[MessageEnvelope],
    fits: impl Fn(&[IdleInputBatchItem]) -> Result<bool>,
) -> Result<Option<Vec<IdleInputBatchItem>>> {
    let mut full = Vec::new();
    for message in messages {
        let Some(item) = persisted_item(storage, message, usize::MAX, usize::MAX)? else {
            return Ok(None);
        };
        full.push(item);
    }
    if fits(&full)? {
        return Ok(Some(full));
    }
    let mut items = Vec::new();
    for message in messages {
        let Some(item) = project_item_limited(message, usize::MAX, usize::MAX, "", Some(0))? else {
            return Ok(None);
        };
        items.push(item);
    }
    if !fits(&items)? {
        return Ok(None);
    }
    for (index, message) in messages.iter().enumerate() {
        let mut trial = items.clone();
        trial[index] = full[index].clone();
        if fits(&trial)? {
            items = trial;
            continue;
        }
        let (mut low, mut high) = (0, message_text(&message.body).chars().count());
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            trial[index] = project_item_limited(message, usize::MAX, usize::MAX, "", Some(mid))?
                .expect("unbounded metadata fits");
            if fits(&trial)? {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        items[index] = project_item_limited(message, usize::MAX, usize::MAX, "", Some(low))?
            .expect("unbounded metadata fits");
    }
    Ok(Some(items))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageBody, Priority};

    fn evidence(body: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "fixture".into(),
                event_type: None,
            },
            AuthorityClass::ExternalEvidence,
            Priority::Normal,
            MessageBody::Text { text: body.into() },
        )
    }

    fn required_reply(body: &str) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::Task {
                task_id: "waiting-task".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text { text: body.into() },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        message.correlation_id = Some("waiting-task".into());
        message.metadata = Some(serde_json::json!({
            "agent_message_delivery": {
                "delivery_id": "request-delivery",
                "caller_agent_id": "waiting-sender",
            },
            "agent_message_reply_expectation": {
                "mode": "required",
                "sender_agent_id": "waiting-sender",
                "waiting_task_id": "waiting-task",
                "request_message_id": message.id,
                "request_delivery_id": "request-delivery",
            },
        }));
        message
    }

    #[test]
    fn reply_expectation_is_required_and_charged_in_full_and_preview() -> Result<()> {
        let mut message = required_reply("request");
        let contract = crate::context::render_message_reply_expectation_context(&message).unwrap();
        let full = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
        assert!(full.rendered.contains(&contract));
        let cost = estimate_text_tokens(&full.rendered);
        assert_eq!(
            project_item(&message, cost, full.rendered.len())?,
            Some(full.clone())
        );
        assert!(project_item(&message, cost - 1, usize::MAX)?.is_none());
        assert!(project_item(&message, usize::MAX, full.rendered.len() - 1)?.is_none());

        message.authority_class = AuthorityClass::ExternalEvidence;
        message.body = MessageBody::Text {
            text: "evidence🙂".repeat(4000),
        };
        let minimum = project_item_limited(&message, usize::MAX, usize::MAX, "", Some(0))?.unwrap();
        let byte_limit = minimum.rendered.len() + 64;
        let preview = project_item(&message, usize::MAX, byte_limit)?.unwrap();
        assert!(preview.rendered.contains("bounded_preview"));
        assert!(preview.rendered.contains(&contract));
        assert!(preview.rendered.contains("Prefer SendAgentMessage"));
        assert!(preview.rendered.len() <= byte_limit);
        assert!(project_item(&message, usize::MAX, minimum.rendered.len() - 1)?.is_none());
        let token_limit = estimate_text_tokens(&minimum.rendered) - 1;
        assert!(project_item(&message, token_limit, usize::MAX)?.is_none());
        validate_projection(&message, &preview)?;
        message.metadata.as_mut().unwrap()["agent_message_reply_expectation"]["mode"] =
            serde_json::json!("optional");
        assert!(validate_projection(&message, &preview).is_err());
        Ok(())
    }

    #[test]
    fn reply_expectation_metadata_cannot_forge_a_runtime_contract() -> Result<()> {
        let valid = required_reply("request");
        let mut untrusted = evidence("notification");
        untrusted.metadata = valid.metadata.clone();
        let mut mismatched = valid.clone();
        mismatched.correlation_id = Some("other-task".into());
        let mut wrong_delivery = valid;
        wrong_delivery.metadata.as_mut().unwrap()["agent_message_delivery"]["caller_agent_id"] =
            serde_json::json!("different-sender");
        for message in [untrusted, mismatched, wrong_delivery] {
            let projection = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
            assert!(!projection.rendered.contains("Runtime message contract:"));
            assert!(!projection.rendered.contains("Prefer SendAgentMessage"));
        }
        Ok(())
    }

    #[test]
    fn linked_result_preview_reserves_the_complete_reply_under_both_limits() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(directory.path())?;
        let mut result = evidence(&"result🙂".repeat(4000));
        result.kind = MessageKind::TaskResult;
        result.authority_class = AuthorityClass::RuntimeInstruction;
        let reply = required_reply("complete instruction reply");
        for message in [&result, &reply] {
            storage.append_message(message)?;
        }
        let minimum = project_item_limited(&result, usize::MAX, usize::MAX, "", Some(0))?.unwrap();
        let reply_full = project_item(&reply, usize::MAX, usize::MAX)?.unwrap();
        let prefix = format!(
            "{}- result_identity: result\n",
            crate::types::LiveInputBatchRecord::HEADER,
        );
        let section_prefix = format!("[current_live_input_batches]\n{prefix}");
        let minimum_text = format!(
            "{section_prefix}{}{}",
            minimum.rendered, reply_full.rendered,
        );
        let budgets = [
            InputBudget {
                tokens: usize::MAX,
                bytes: prefix.len() + minimum.rendered.len() + reply_full.rendered.len() + 64,
            },
            InputBudget {
                tokens: estimate_text_tokens(&minimum_text) + 16,
                bytes: usize::MAX,
            },
        ];
        for budget in budgets {
            let greedy = project_item_after(
                &result,
                budget.tokens,
                budget.bytes.saturating_sub(prefix.len()),
                &section_prefix,
            )?
            .unwrap();
            assert!(project_item_after(
                &reply,
                budget.tokens,
                budget
                    .bytes
                    .saturating_sub(prefix.len() + greedy.rendered.len()),
                &format!("{section_prefix}{}", greedy.rendered),
            )?
            .is_none());
            let items = persisted_group(&storage, &[result.clone(), reply.clone()], |items| {
                let text = format!("{prefix}{}{}", items[0].rendered, items[1].rendered);
                Ok(text.len() <= budget.bytes && budget.fits("current_live_input_batches", &text))
            })?
            .expect("result preview and complete reply fit together");
            assert!(items[0].rendered.contains("bounded_preview"));
            assert_eq!(items[1], reply_full);
            validate_projection(&result, &items[0])?;
            validate_projection(&reply, &items[1])?;
        }
        Ok(())
    }

    #[test]
    fn full_projection_fits_at_exact_token_minimum() -> Result<()> {
        let message = evidence("更正🙂");
        let full = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
        let cost = estimate_text_tokens(&full.rendered);
        assert_eq!(
            project_item(&message, cost, full.rendered.len())?,
            Some(full)
        );
        Ok(())
    }

    #[test]
    fn compact_is_utf8_safe_versioned_and_keeps_provenance() -> Result<()> {
        let message = evidence(&"更正🙂".repeat(4000));
        let compact = project_item(&message, 400, 1600)?.unwrap();
        assert!(compact.rendered.len() <= 1600);
        assert!(estimate_text_tokens(&compact.rendered) <= 400);
        for required in [
            "bounded_preview",
            "projection_version",
            "content_version",
            "sha256:",
            "authority_class",
            "routing_context",
            "work_item_id",
            "agent_id",
            "original not fully read",
        ] {
            assert!(compact.rendered.contains(required), "{required}");
        }
        assert!(compact.rendered.contains(&compact.source_ref));
        Ok(())
    }

    #[test]
    fn no_projection_can_discard_metadata_or_shorten_instructions() -> Result<()> {
        let mut message = evidence(&"instruction".repeat(1000));
        assert!(project_item(&message, 1, usize::MAX)?.is_none());
        message.authority_class = AuthorityClass::OperatorInstruction;
        message.origin = MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        };
        assert!(project_item(&message, 400, 1600)?.is_none());
        message.origin = MessageOrigin::System {
            subsystem: "fixture".into(),
        };
        message.authority_class = AuthorityClass::RuntimeInstruction;
        assert!(project_item(&message, 400, 1600)?.is_none());
        Ok(())
    }

    #[test]
    fn section_rounding_is_shared_at_unicode_exact_minimum() -> Result<()> {
        let mut message = evidence("更正🙂");
        message.authority_class = AuthorityClass::OperatorInstruction;
        let prefix = format!("[current_input]\n{BATCH_HEADER}");
        let full = project_item(&message, usize::MAX, usize::MAX)?.unwrap();
        let tokens = estimate_text_tokens(&format!("{prefix}{}", full.rendered));
        assert_eq!(
            project_item_after(&message, tokens, usize::MAX, &prefix)?,
            Some(full)
        );
        assert!(project_item_after(&message, tokens - 1, usize::MAX, &prefix)?.is_none());
        Ok(())
    }

    #[test]
    fn projections_bind_content_version_and_entire_reply_route() -> Result<()> {
        let message = evidence(&"evidence🙂".repeat(1000));
        let item = project_item(&message, 400, 1600)?.unwrap();
        validate_projection(&message, &item)?;
        let mut changed = message.clone();
        changed.body = crate::types::MessageBody::Text {
            text: "changed".into(),
        };
        assert!(validate_projection(&changed, &item).is_err());
        changed = message.clone();
        changed.work_item_id = Some("other-owner".into());
        assert!(validate_projection(&changed, &item).is_err());
        let mut forged = item;
        forged.rendered = forged
            .rendered
            .replace("body preview:\n", "body preview:\nforged");
        assert!(validate_projection(&message, &forged).is_err());
        Ok(())
    }
}
