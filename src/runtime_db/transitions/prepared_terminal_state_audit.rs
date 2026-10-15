//! Original terminal State notification, after its Memory epilogue.
use super::*;

fn validate(command: &QueueTransitionCommand, event: &AuditEvent) -> Result<()> {
    let state = command
        .agent_state
        .as_ref()
        .context("terminal State audit omitted its actual State")?;
    let turn = command
        .turn_record
        .as_ref()
        .context("terminal State audit omitted its actual Turn")?;
    let terminal = state
        .record
        .last_turn_terminal
        .as_ref()
        .context("terminal State audit has no terminal record")?;
    anyhow::ensure!(
        command.operation == QueueOperation::Settle
            && turn.agent_id == command.agent_id
            && state.record.id == command.agent_id
            && state.record.current_turn_id.as_deref() == Some(turn.turn_id.as_str())
            && terminal.turn_id == turn.turn_id
            && terminal.turn_index == turn.turn_index
            && turn.terminal.as_ref()
                == Some(&crate::types::TurnTerminalSummary::from_terminal(terminal))
            && event.kind == "agent_state_changed"
            && !event.id.is_empty()
            && event.event_seq == 0
            && event.event_log_epoch.is_empty()
            && event.data
                == serde_json::to_value(crate::types::AgentStateChangedEvent::from_state(
                    &state.record
                ))?,
        "original terminal State audit differs from its exact State/Turn command"
    );
    Ok(())
}
impl PreparedQueueTransition {
    pub(crate) fn with_original_terminal_state_audit(mut self, event: AuditEvent) -> Result<Self> {
        anyhow::ensure!(
            self.original_terminal_state_audit.is_none(),
            "terminal State audit was already frozen"
        );
        validate(&self.command, &event)?;
        self.original_terminal_state_audit = Some(event);
        Ok(self)
    }
}

pub(super) fn validate_queue(queue: &PreparedQueueTransition) -> Result<()> {
    if let Some(event) = queue.original_terminal_state_audit.as_ref() {
        validate(&queue.command, event)?;
    }
    Ok(())
}

pub(super) fn finish_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    value: &mut TransitionCommit,
    current: Option<
        &mut crate::runtime_db::current_read_snapshot::CurrentReadBusinessTransaction<'_, '_>,
    >,
) -> Result<()> {
    let InputTransitionCommand::Queue(queue) = prepared.command.as_ref() else {
        return Ok(());
    };
    let Some(event) = queue.original_terminal_state_audit.as_ref() else {
        return Ok(());
    };
    validate(&queue.command, event)?;
    let final_state = value
        .effects
        .agent_state
        .as_ref()
        .context("original terminal State effects missing")?;
    // Memory finalization may change only WorkingMemory, which this DTO omits.
    // Recheck the actual final effects instead of copying an earlier body.
    anyhow::ensure!(
        event.data
            == serde_json::to_value(crate::types::AgentStateChangedEvent::from_state(
                &final_state.record
            ))?,
        "final terminal State differs from its frozen lightweight notification"
    );
    let current =
        current.context("terminal State notification needs its original actual Current proof")?;
    let (actual, inserted) = current.append_event_tx(tx, Some(&queue.command.agent_id), event)?;
    if inserted {
        value.effects.audit_events.push(actual);
    }
    value.applied |= inserted;
    Ok(())
}
