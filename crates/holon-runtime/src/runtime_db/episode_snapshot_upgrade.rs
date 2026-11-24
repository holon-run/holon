//! The upgrade visits history incrementally; total history is not a read quota.

use super::*;

pub(super) fn tool_work_is_terminal_annotation(
    connection: &Connection,
    agent: &str,
    tool: &ToolExecutionRecord,
    tables: &BTreeSet<String>,
) -> Result<bool> {
    if !tables.contains("work_items")
        || !tables.contains("turn_records")
        || tool.status == crate::types::ToolExecutionStatus::Deferred
    {
        return Ok(false);
    }
    let (Some(work), Some(turn_id)) = (&tool.work_item_id, &tool.turn_id) else {
        return Ok(false);
    };
    let external_completed: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_items WHERE work_item_id=?1 AND agent_id!=?2 AND state='completed')",
        params![work, agent], |row|row.get(0),
    )?;
    if !external_completed {
        return Ok(false);
    }
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload_json FROM turn_records WHERE agent_id=?1 AND turn_id=?2",
            params![agent, turn_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let turn: TurnRecord = serde_json::from_str(&payload)?;
    ensure!(
        turn.agent_id == agent && turn.turn_id == *turn_id,
        "historical tool Turn projection mismatch"
    );
    if turn.terminal.is_none()
        || turn.current_work_item_id.is_some()
        || !matches!(&turn.owner, Some(crate::types::TurnOwner::AgentLifecycle { agent_id }) if agent_id == agent)
    {
        return Ok(false);
    }
    let state = if tables.contains("agent_states") {
        load_state(connection, agent)?.0
    } else {
        None
    };
    Ok(state.as_ref().is_none_or(|state| {
        state.current_turn_id.as_ref() != Some(turn_id)
            && state
                .current_execution_binding
                .as_ref()
                .is_none_or(|binding| &binding.turn_id != turn_id)
    }))
}
