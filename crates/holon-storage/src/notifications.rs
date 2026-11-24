//! SQLite projections for the operator inbox.

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use holon_domain::{BriefRecord, MessageEnvelope, WakeSource};
use rusqlite::{params, Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    OperatorFirstBrief,
    WorkItemCompleted,
    OperatorInputRequired,
}

pub fn record_work_owner(conn: &Connection, work_item_id: &str, principal: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO operator_inbox_work_owners (work_item_id, principal_id) VALUES (?1, ?2)",
        params![work_item_id, principal],
    )?;
    Ok(())
}

pub fn related_input(
    conn: &Connection,
    input_id: &str,
    agent_id: &str,
) -> Result<Option<MessageEnvelope>> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM messages WHERE evidence_id = ?1 AND agent_id = ?2",
            params![input_id, agent_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(payload.as_deref().map(serde_json::from_str).transpose()?)
}

pub fn completed_work_owner(
    conn: &Connection,
    work_id: &str,
    agent_id: &str,
    brief_id: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT o.principal_id FROM operator_inbox_work_owners o
             JOIN work_items w ON w.work_item_id = o.work_item_id
             WHERE w.work_item_id = ?1 AND w.agent_id = ?2 AND w.state = 'completed'
               AND json_extract(w.payload_json, '$.result_brief_id') = ?3",
        params![work_id, agent_id, brief_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn record_cause(
    conn: &Connection,
    brief: &BriefRecord,
    principal: &str,
    kind: NotificationKind,
    cause_id: &str,
    work_id: Option<&str>,
) -> Result<()> {
    // The FK is immediate, so create the display entry before binding the
    // cause, but only when the cause is still unclaimed.
    let priority = kind.priority();
    let stored = conn.execute(
        "INSERT INTO operator_inbox_recent
             (principal_id, brief_id, agent_id, work_item_id, priority, created_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6
         WHERE NOT EXISTS (
             SELECT 1 FROM operator_inbox_causes
             WHERE principal_id = ?1 AND priority = ?5 AND cause_id = ?7
         )
         ON CONFLICT(principal_id, brief_id) DO UPDATE SET
             priority = max(priority, excluded.priority),
             work_item_id = CASE WHEN excluded.priority > priority
                                 THEN excluded.work_item_id ELSE work_item_id END",
        params![
            principal,
            brief.id,
            brief.agent_id,
            work_id,
            priority,
            brief.created_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
            cause_id
        ],
    )?;
    if stored > 0 {
        conn.execute(
            "INSERT INTO operator_inbox_causes (principal_id, priority, cause_id, brief_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![principal, priority, cause_id, brief.id],
        )?;
    }
    Ok(())
}

pub fn recent(
    conn: &Connection,
    principal_id: &str,
    local_control: bool,
    now: DateTime<Utc>,
) -> Result<Vec<InboxNotification>> {
    let cutoff = (now - chrono::Duration::days(30)).to_rfc3339_opts(SecondsFormat::Nanos, true);
    let mut stmt = conn.prepare_cached(
        "SELECT r.brief_id, r.priority, r.created_at, r.agent_id, w.work_item_id
         FROM operator_inbox_recent r
         JOIN agent_identities i ON i.agent_id = r.agent_id AND i.status = 'active'
         LEFT JOIN work_items w ON w.work_item_id = r.work_item_id AND w.agent_id = r.agent_id
         WHERE r.principal_id = ?1 AND r.created_at >= ?2
           AND (i.visibility = 'public' OR ?3)
         ORDER BY r.created_at DESC, r.brief_id LIMIT 100",
    )?;
    let mut rows = stmt.query(params![principal_id, cutoff, local_control])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let kind = match row.get::<_, i64>(1)? {
            1 => NotificationKind::OperatorFirstBrief,
            2 => NotificationKind::WorkItemCompleted,
            3 => NotificationKind::OperatorInputRequired,
            _ => anyhow::bail!("invalid operator inbox priority"),
        };
        items.push(InboxNotification {
            id: row.get(0)?,
            kind,
            created_at: row.get::<_, String>(2)?.parse()?,
            target: NotificationTarget {
                agent_id: row.get(3)?,
                work_item_id: row.get(4)?,
            },
        });
    }
    Ok(items)
}

impl NotificationKind {
    fn priority(self) -> i64 {
        match self {
            Self::OperatorFirstBrief => 1,
            Self::WorkItemCompleted => 2,
            Self::OperatorInputRequired => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Serialize)]
pub struct NotificationTarget {
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Serialize)]
pub struct InboxNotification {
    pub id: String,
    pub kind: NotificationKind,
    pub created_at: DateTime<Utc>,
    pub target: NotificationTarget,
}

pub struct WaitOrigin {
    pub work_item_id: Option<String>,
    pub input: Option<MessageEnvelope>,
    pub owner: Option<String>,
    pub ambiguous: bool,
}

pub fn load_wait_origin(
    conn: &Connection,
    wait_id: &str,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<WaitOrigin>> {
    let row = conn
        .query_row(
            "SELECT w.work_item_id, w.wake_sources_json, m.payload_json, o.principal_id, EXISTS (
             SELECT 1 FROM json_each(t.payload_json, '$.input_message_ids') inputs
             JOIN messages other ON other.evidence_id = inputs.value AND other.agent_id = w.agent_id
             WHERE other.kind = 'operator_prompt' AND other.evidence_id IS NOT t.trigger_message_id
         )
         FROM wait_conditions w
         JOIN agent_identities i ON i.agent_id = w.agent_id AND i.status = 'active'
         JOIN turn_records t ON t.turn_id = w.last_turn_id AND t.agent_id = w.agent_id
         LEFT JOIN messages m ON m.evidence_id = t.trigger_message_id AND m.agent_id = w.agent_id
         LEFT JOIN operator_inbox_work_owners o ON o.work_item_id = w.work_item_id
         WHERE w.wait_condition_id = ?1 AND w.agent_id = ?2 AND w.last_turn_id = ?3
           AND w.source = 'WaitFor' AND w.kind = 'operator' AND w.status = 'active'
           AND (w.work_item_id IS NULL OR EXISTS (
               SELECT 1 FROM work_items k WHERE k.work_item_id = w.work_item_id
                 AND k.agent_id = w.agent_id AND k.state = 'open'
           ))",
            params![wait_id, agent_id, turn_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((work_id, wake_json, input, owner, ambiguous)) = row else {
        return Ok(None);
    };
    if !serde_json::from_str::<Vec<WakeSource>>(&wake_json)?.contains(&WakeSource::OperatorInput) {
        return Ok(None);
    }
    let input: Option<MessageEnvelope> = input.as_deref().map(serde_json::from_str).transpose()?;
    Ok(Some(WaitOrigin {
        work_item_id: work_id,
        input,
        owner,
        ambiguous,
    }))
}

/// Pending is current authority: no notification table owns a second copy of
/// waiting state. Check the registered wake, live agent and exact work owner
/// in the same read, and never silently truncate the resulting snapshot.
pub fn pending(
    conn: &Connection,
    local_control: bool,
    now: DateTime<Utc>,
    can_handle: impl Fn(Option<&MessageEnvelope>, Option<&str>, bool) -> bool,
) -> Result<Vec<InboxNotification>> {
    let mut stmt = conn.prepare_cached(
        "SELECT w.wait_condition_id, w.agent_id, w.work_item_id, w.status, w.kind, w.source,
                w.subject_ref, w.waiting_for, w.created_at, w.updated_at, w.expires_at,
                w.resolved_at, w.cancelled_at, w.last_turn_id, w.trigger_message_id, w.triggered_at,
                w.wake_sources_json, w.continuation_json, m.payload_json, o.principal_id,
                EXISTS (
                    SELECT 1 FROM json_each(t.payload_json, '$.input_message_ids') inputs
                    JOIN messages other ON other.evidence_id = inputs.value
                     AND other.agent_id = w.agent_id
                    WHERE other.kind = 'operator_prompt'
                      AND other.evidence_id IS NOT t.trigger_message_id
                )
         FROM wait_conditions w
         JOIN agent_identities i ON i.agent_id = w.agent_id AND i.status = 'active'
         LEFT JOIN agent_states a ON a.agent_id = w.agent_id
         LEFT JOIN turn_records t ON t.turn_id = w.last_turn_id AND t.agent_id = w.agent_id
         LEFT JOIN messages m
           ON m.evidence_id = t.trigger_message_id
          AND m.agent_id = w.agent_id
         LEFT JOIN operator_inbox_work_owners o ON o.work_item_id = w.work_item_id
         WHERE w.status = 'active' AND w.kind = 'operator' AND w.source = 'WaitFor'
           AND COALESCE(a.status, '') NOT IN ('stopped', 'paused')
           AND (i.visibility = 'public' OR ?1)
           AND (w.work_item_id IS NULL OR EXISTS (
                   SELECT 1 FROM work_items k
                   WHERE k.work_item_id = w.work_item_id AND k.agent_id = w.agent_id
                     AND k.state = 'open'
               ))",
    )?;
    let mut rows = stmt.query([local_control])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let wait = crate::wait::decode_wait_condition_row(row)?;
        if !wait.wake_sources.contains(&WakeSource::OperatorInput)
            || wait.expires_at.is_some_and(|at| at <= now)
        {
            continue;
        }
        let input: Option<String> = row.get(18)?;
        let input: Option<MessageEnvelope> =
            input.as_deref().map(serde_json::from_str).transpose()?;
        let owner: Option<String> = row.get(19)?;
        let has_other_operator_input: bool = row.get(20)?;
        if !can_handle(input.as_ref(), owner.as_deref(), has_other_operator_input) {
            continue;
        }
        items.push(InboxNotification {
            id: wait.id,
            kind: NotificationKind::OperatorInputRequired,
            created_at: wait.created_at,
            target: NotificationTarget {
                agent_id: wait.agent_id,
                work_item_id: wait.work_item_id,
            },
        });
    }
    items.sort_unstable_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(items)
}
#[cfg(test)]
mod tests;
