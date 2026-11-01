use super::*;

/// A queue head older than this without dequeue progress is starving every
/// wake behind it (#3607).
pub(crate) const QUEUE_HEAD_STARVATION_AFTER_MS: i64 = 10 * 60 * 1000;
const QUEUE_HEAD_STARVED_ADVISORY_INTERVAL_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn scheduling_advisories_for_facts(
    agent: &AgentState,
    projection: &SchedulerProjection,
    posture: &AgentPostureProjection,
    work_queue: &WorkQueueReadModel,
    wait_conditions: &[WaitConditionRecord],
) -> Vec<SchedulingAdvisory> {
    scheduling_advisories_for_selected_facts(
        agent,
        projection,
        posture,
        work_queue
            .items
            .iter()
            .filter(|item| {
                item.scheduling_state == WorkItemSchedulingState::Blocked
                    && item.work_item.agent_id == agent.id
                    && item.work_item.blocked_by.is_some()
                    && item.work_item.recheck_at.is_none()
                    && !item.has_active_waits
                    && !item.has_active_task_waits
            })
            .map(|item| &item.work_item),
        wait_conditions,
    )
}

pub(crate) fn scheduling_advisories_for_selected_facts<'a>(
    agent: &AgentState,
    projection: &SchedulerProjection,
    posture: &AgentPostureProjection,
    blocked_work: impl Iterator<Item = &'a WorkItemRecord>,
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

    for item in blocked_work {
        diagnostics.push(
            SchedulingAdvisory::warning(
                "blocked_work_item_without_recheck_or_wait",
                "blocked WorkItem has no recheck deadline or active wait condition",
            )
            .work_item_id(item.id.clone())
            .evidence("scheduling_state=Blocked")
            .evidence("blocked_by_present=true")
            .evidence("recheck_at=None")
            .evidence("has_active_waits=false"),
        );
    }

    diagnostics
}

pub(crate) fn scheduling_advisory_event(diagnostic: &SchedulingAdvisory) -> AuditEvent {
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

#[cfg(test)]
pub(crate) fn append_scheduling_advisories(
    storage: &AppStorage,
    agent: &AgentState,
    queue_len: usize,
) -> Result<usize> {
    let diagnostics = scheduling_advisories_with_queue_len(storage, agent, queue_len)?;
    append_prepared_scheduling_advisories(storage, &diagnostics)
}

#[cfg(test)]
pub(crate) fn append_prepared_scheduling_advisories(
    storage: &AppStorage,
    diagnostics: &[SchedulingAdvisory],
) -> Result<usize> {
    let recent_events = storage.read_recent_events(64)?;
    let mut appended = 0;
    for event in pending_scheduling_advisory_events(&recent_events, diagnostics) {
        storage.append_event(&event)?;
        appended += 1;
    }
    Ok(appended)
}

pub(crate) fn pending_scheduling_advisory_events<'a>(
    recent: &'a [AuditEvent],
    diagnostics: &'a [SchedulingAdvisory],
) -> impl Iterator<Item = AuditEvent> + 'a {
    diagnostics
        .iter()
        .enumerate()
        .filter_map(move |(index, diagnostic)| {
            // These fields are exactly the event payload, without copying JSON.
            if diagnostics[..index].contains(diagnostic) {
                return None;
            }
            let event = scheduling_advisory_event(diagnostic);
            (!recent
                .iter()
                .any(|latest| latest.kind == event.kind && latest.data == event.data))
            .then_some(event)
        })
}

pub(crate) fn ambiguous_wait_advisory_event(
    message: &MessageEnvelope,
    wait_condition_ids: &[String],
) -> AuditEvent {
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
    scheduling_advisory_event(&diagnostic)
}

pub(crate) fn append_deferred_activation_advisory(
    storage: &AppStorage,
    message: &MessageEnvelope,
    scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
    reason: &str,
) -> Result<()> {
    let diagnostic = SchedulingAdvisory::warning(
        "canonical_activation_deferred_not_runnable",
        "canonical activation input waits for a runnable execution and remains queued",
    )
    .maybe_work_item_id(message.work_item_id.clone())
    .evidence(format!("message_id={}", message.id))
    .evidence(format!("scenario_class={}", scenario_class.as_str()))
    .evidence(format!("reason={reason}"));
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

pub(crate) fn append_interrupted_replay_source_advisory(
    storage: &AppStorage,
    message: &MessageEnvelope,
    invalid: &crate::runtime_db::task_result_settlement::live_turn::InterruptedReplaySourceInvalid,
) -> Result<()> {
    let diagnostic = SchedulingAdvisory::warning(
        "interrupted_replay_source_invalid",
        "interrupted replay source no longer satisfies the replay invariant and stays queued",
    )
    .maybe_work_item_id(message.work_item_id.clone())
    .evidence(format!("message_id={}", message.id))
    .evidence(format!("activation_id={}", invalid.activation_id))
    .evidence(format!("violation={}", invalid.violation))
    .evidence(format!(
        "turn_id={}",
        invalid.turn_id.as_deref().unwrap_or("none")
    ))
    .evidence(format!(
        "terminal_kind={}",
        invalid.terminal_kind.as_deref().unwrap_or("none")
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

/// Rate-limited advisory for a queue head that has waited too long without
/// dequeue progress. While it stays first, every later wake starves, and the
/// incident class can otherwise stay silent for days (#3607).
pub(crate) fn append_queue_head_starved_advisory(
    storage: &AppStorage,
    agent_id: &str,
    message_id: &str,
    queue_status: &'static str,
    waiting_ms: i64,
    queue_len: usize,
    now: DateTime<Utc>,
) -> Result<bool> {
    let diagnostic = SchedulingAdvisory::warning(
        "queue_head_starved",
        "queue head has waited without dequeue progress and blocks every later wake",
    )
    .evidence(format!("agent_id={agent_id}"))
    .evidence(format!("message_id={message_id}"))
    .evidence(format!("queue_status={queue_status}"))
    .evidence(format!("waiting_ms={waiting_ms}"))
    .evidence(format!("queue_len={queue_len}"));
    let event = scheduling_advisory_event(&diagnostic);
    let message_evidence = format!("message_id={message_id}");
    let duplicate = storage.event_log().has_recent_scheduling_advisory(
        &message_evidence,
        now - chrono::Duration::milliseconds(QUEUE_HEAD_STARVED_ADVISORY_INTERVAL_MS),
    )?;
    if duplicate {
        return Ok(false);
    }
    storage.append_event(&event)?;
    Ok(true)
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
