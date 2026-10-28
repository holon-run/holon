//! One-shot terminal membership. IDs select bodies; they never qualify them.
use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;

use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceScope,
};

pub(crate) struct TurnInputSelector<'a> {
    pub turn_id: &'a str,
    pub source_message_id: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TurnInputMembers {
    pub messages: Vec<String>,
    pub briefs: Vec<String>,
    pub tools: Vec<String>,
    pub turns: Vec<String>,
    pub absent_turns: Vec<String>,
}

impl TurnInputMembers {
    pub(crate) fn extend_request(
        &self,
        request: &mut super::InputEvidenceLoadRequest,
    ) -> Result<()> {
        let query = request
            .query
            .as_mut()
            .context("Turn requires an Input query")?;
        for (family, ids) in [
            (Family::Message, &self.messages),
            (Family::Brief, &self.briefs),
            (Family::ToolExecution, &self.tools),
            (Family::Turn, &self.turns),
        ] {
            for id in ids {
                crate::diagnostics::resource_io::admit_cloned_value(&(family, id))?;
                query.selection.required_keys.insert(Key::new(family, id));
            }
        }
        Ok(())
    }
}

pub(crate) fn select_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selector: &TurnInputSelector<'_>,
    record_limit: usize,
) -> Result<TurnInputMembers> {
    // Bound and admit the complete relation before allocating any selected ID.
    // No history-window cutoff is allowed for a terminal Turn.
    let (messages, message_bytes): (usize, u64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(message_id AS BLOB))),0)
         FROM messages WHERE agent_id=?1 AND (turn_id=?2 OR message_id=?3)",
        params![scope.agent_id, selector.turn_id, selector.source_message_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut count = messages;
    let mut bytes = message_bytes;
    for table in ["briefs", "tool_executions"] {
        let (rows, raw): (usize, u64) = tx.query_row(
            &format!(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(evidence_id AS BLOB))),0)
                 FROM {table} WHERE agent_id=?1 AND turn_id=?2"
            ),
            params![scope.agent_id, selector.turn_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        count = count
            .checked_add(rows)
            .context("Turn member count overflow")?;
        bytes = bytes
            .checked_add(raw)
            .context("Turn member bytes overflow")?;
    }
    let source_bytes: u64 = tx.query_row(
        "SELECT COALESCE(SUM(length(CAST(turn_id AS BLOB))),0)
         FROM messages WHERE agent_id=?1 AND message_id=?2",
        params![scope.agent_id, selector.source_message_id],
        |row| row.get(0),
    )?;
    ensure!(
        count <= record_limit,
        "Turn Input membership exceeds record budget"
    );
    let nodes = count
        .checked_mul(4)
        .and_then(|n| n.checked_add(16))
        .context("Turn member nodes overflow")?;
    let bytes = bytes
        .checked_add(source_bytes)
        .and_then(|n| n.checked_add(u64::try_from(selector.turn_id.len()).ok()?))
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
        .context("Turn member material overflow")?;
    crate::diagnostics::resource_io::admit_owned_metadata(bytes, nodes)?;
    let mut statement = tx.prepare(
        "SELECT message_id FROM messages
         WHERE agent_id=?1 AND (turn_id=?2 OR message_id=?3)
         ORDER BY message_seq IS NULL DESC,message_seq ASC,created_at ASC,message_id ASC",
    )?;
    let mut messages = statement
        .query_map(
            params![scope.agent_id, selector.turn_id, selector.source_message_id],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    messages.dedup();
    let mut source_turn_id = tx
        .query_row(
            "SELECT turn_id FROM messages WHERE agent_id=?1 AND message_id=?2",
            params![scope.agent_id, selector.source_message_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let mut members = TurnInputMembers {
        messages,
        briefs: Vec::new(),
        tools: Vec::new(),
        turns: Vec::new(),
        absent_turns: Vec::new(),
    };
    for (table, ids) in [
        ("briefs", &mut members.briefs),
        ("tool_executions", &mut members.tools),
    ] {
        let mut statement = tx.prepare(&format!(
            "SELECT evidence_id FROM {table} WHERE agent_id=?1 AND turn_id=?2
             ORDER BY created_at ASC,evidence_id ASC"
        ))?;
        *ids = statement
            .query_map(params![scope.agent_id, selector.turn_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
    }
    if let Some(id) = &mut source_turn_id {
        // Trimming in place preserves the original replay predicate.
        let end = id.trim_end().len();
        id.truncate(end);
        let start = id.len() - id.trim_start().len();
        id.drain(..start);
        if id.is_empty() || id == selector.turn_id {
            source_turn_id = None;
        }
    }
    for id in std::iter::once(selector.turn_id).chain(source_turn_id.as_deref()) {
        let present =
            super::super::input_evidence_snapshot::optional_turn_present_tx(tx, scope, id)?;
        // A finite absent selection does not create an Input tombstone, alias
        // or authorization for a later body at this address.
        if present {
            members.turns.push(id.to_owned());
        } else {
            members.absent_turns.push(id.to_owned());
        }
    }
    Ok(members)
}
