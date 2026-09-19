use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulingAdvisory {
    pub kind: String,
    pub severity: SchedulingAdvisorySeverity,
    pub message: String,
    pub work_item_id: Option<String>,
    pub wait_condition_id: Option<String>,
    pub evidence: Vec<String>,
}

impl SchedulingAdvisory {
    fn warning(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            severity: SchedulingAdvisorySeverity::Warning,
            message: message.into(),
            work_item_id: None,
            wait_condition_id: None,
            evidence: Vec::new(),
        }
    }

    fn work_item_id(mut self, work_item_id: impl Into<String>) -> Self {
        self.work_item_id = Some(work_item_id.into());
        self
    }

    fn wait_condition_id(mut self, wait_condition_id: impl Into<String>) -> Self {
        self.wait_condition_id = Some(wait_condition_id.into());
        self
    }

    fn evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence.push(evidence.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulingAdvisorySeverity {
    Warning,
}

impl SchedulingAdvisorySeverity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
        }
    }
}

/// Derive evidence-based scheduler diagnostics from authoritative runtime facts.
///
/// Diagnostics are advisory observability signals only. They must not be used as
/// scheduler decisions or as a replacement for the posture/work-item state
/// derivation itself.
#[cfg(test)]
pub(crate) fn scheduling_advisories(
    storage: &AppStorage,
    agent: &AgentState,
) -> Result<Vec<SchedulingAdvisory>> {
    scheduling_advisories_with_queue_len(storage, agent, agent.pending)
}

pub(crate) fn scheduling_advisories_with_queue_len(
    storage: &AppStorage,
    agent: &AgentState,
    queue_len: usize,
) -> Result<Vec<SchedulingAdvisory>> {
    let projection = SchedulerProjection::from_state_with_queue_len(storage, agent, queue_len)?;
    let posture = storage.agent_posture_projection(agent)?;
    let work_queue = storage.work_queue_prompt_projection()?;
    let wait_conditions = storage.active_wait_conditions()?;

    Ok(scheduling_advisories_for_facts(
        agent,
        &projection,
        &posture,
        &work_queue,
        &wait_conditions,
    ))
}

pub(crate) fn scheduling_advisories_for_facts(
    agent: &AgentState,
    projection: &SchedulerProjection,
    posture: &AgentPostureProjection,
    work_queue: &WorkQueueReadModel,
    wait_conditions: &[WaitConditionRecord],
) -> Vec<SchedulingAdvisory> {
    let mut diagnostics = Vec::new();

    if posture.posture == AgentSchedulingPosture::Idle {
        if let Some(signal) = projection.work_reactivation_signal() {
            diagnostics.push(
                SchedulingAdvisory::warning(
                    "idle_posture_has_runnable_work",
                    "agent posture is idle while scheduler facts contain runnable work",
                )
                .work_item_id(signal.work_item_id)
                .evidence("posture=Idle")
                .evidence(format!("reactivation_mode={:?}", signal.reactivation_mode)),
            );
        } else if projection.queue_len > 0 {
            diagnostics.push(
                SchedulingAdvisory::warning(
                    "idle_posture_has_queued_input",
                    "agent posture is idle while scheduler facts contain queued input",
                )
                .evidence("posture=Idle")
                .evidence(format!("queue_len={}", projection.queue_len)),
            );
        }
    }

    for condition in wait_conditions.iter().filter(|condition| {
        condition.agent_id == agent.id && condition.status == WaitConditionStatus::Active
    }) {
        match condition.external_recoverability() {
            Some(ExternalWaitRecoverability::Weak) => {
                diagnostics.push(
                    SchedulingAdvisory::warning(
                        "external_wait_has_weak_recoverability",
                        "active external wait lacks a durable recovery path",
                    )
                    .wait_condition_id(condition.id.clone())
                    .maybe_work_item_id(condition.work_item_id.clone())
                    .evidence("external_recoverability=Weak")
                    .evidence(format!("wake_sources={:?}", condition.wake_sources)),
                );
            }
            Some(ExternalWaitRecoverability::ExplicitNoFallback) => {
                let mut diagnostic = SchedulingAdvisory::warning(
                    "external_wait_has_no_fallback",
                    "active external wait explicitly has no fallback recovery path",
                )
                .wait_condition_id(condition.id.clone())
                .maybe_work_item_id(condition.work_item_id.clone())
                .evidence("external_recoverability=ExplicitNoFallback")
                .evidence(format!("wake_sources={:?}", condition.wake_sources));
                if let Some(reason) = condition.no_fallback_reason() {
                    diagnostic = diagnostic.evidence(format!("no_fallback_reason={reason}"));
                }
                diagnostics.push(diagnostic);
            }
            Some(ExternalWaitRecoverability::Recoverable) | None => {}
        }
    }

    for item in work_queue.items.iter().filter(|item| {
        item.scheduling_state == WorkItemSchedulingState::Blocked
            && item.work_item.agent_id == agent.id
            && item.work_item.blocked_by.is_some()
            && item.work_item.recheck_at.is_none()
            && !item.has_active_waits
            && !item.has_active_task_waits
    }) {
        diagnostics.push(
            SchedulingAdvisory::warning(
                "blocked_work_item_without_recheck_or_wait",
                "blocked WorkItem has no recheck deadline or active wait condition",
            )
            .work_item_id(item.work_item.id.clone())
            .evidence("scheduling_state=Blocked")
            .evidence("blocked_by_present=true")
            .evidence("recheck_at=None")
            .evidence("has_active_waits=false"),
        );
    }

    diagnostics
}

fn scheduling_advisory_event(diagnostic: &SchedulingAdvisory) -> AuditEvent {
    AuditEvent::legacy(
        "scheduling_advisory",
        serde_json::json!({
            "kind": &diagnostic.kind,
            "severity": diagnostic.severity.as_str(),
            "message": &diagnostic.message,
            "work_item_id": &diagnostic.work_item_id,
            "wait_condition_id": &diagnostic.wait_condition_id,
            "evidence": &diagnostic.evidence,
        }),
    )
}

pub(crate) fn append_scheduling_advisories(
    storage: &AppStorage,
    agent: &AgentState,
    queue_len: usize,
) -> Result<usize> {
    let diagnostics = scheduling_advisories_with_queue_len(storage, agent, queue_len)?;
    let recent_events = storage.read_recent_events(64)?;
    let mut seen_data = Vec::new();
    let mut appended = 0;

    for diagnostic in diagnostics {
        let event = scheduling_advisory_event(&diagnostic);
        if seen_data.iter().any(|data| data == &event.data) {
            continue;
        }
        seen_data.push(event.data.clone());

        let duplicate = recent_events
            .iter()
            .any(|latest| latest.kind == event.kind && latest.data == event.data);
        if duplicate {
            continue;
        }
        storage.append_event(&event)?;
        appended += 1;
    }

    Ok(appended)
}

pub(crate) fn append_ambiguous_wait_advisory(
    storage: &AppStorage,
    message: &MessageEnvelope,
    wait_condition_ids: &[String],
) -> Result<()> {
    let diagnostic = SchedulingAdvisory::warning(
        "ambiguous_canonical_wait_binding",
        "canonical activation input matches multiple active waits and remains queued",
    )
    .maybe_work_item_id(message.work_item_id.clone())
    .evidence(format!("message_id={}", message.id))
    .evidence(format!(
        "wait_condition_ids={}",
        wait_condition_ids.join(",")
    ));
    let event = scheduling_advisory_event(&diagnostic);
    let duplicate = storage
        .read_recent_events(64)?
        .iter()
        .any(|latest| latest.kind == event.kind && latest.data == event.data);
    if !duplicate {
        storage.append_event(&event)?;
    }
    Ok(())
}

trait SchedulingAdvisoryExt {
    fn maybe_work_item_id(self, work_item_id: Option<String>) -> Self;
}

impl SchedulingAdvisoryExt for SchedulingAdvisory {
    fn maybe_work_item_id(mut self, work_item_id: Option<String>) -> Self {
        self.work_item_id = work_item_id;
        self
    }
}
