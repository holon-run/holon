//! First-copy material of the original closed Result selection, not authority.
use super::*;

/// Borrow original addresses until the actual Task phase has admitted copies.
pub(crate) enum TaskResultSelectionRead<'a> {
    Owner {agent:&'a str,work:Option<&'a str>},
    Reply {agent:&'a str,work:Option<&'a str>,message:&'a str},
    Message {message:&'a str},
    InvalidOnly {agent:&'a str,message:&'a str},
    Activation {agent:&'a str,activation:&'a str},
    Wake {agent:&'a str,work:&'a str},
}
pub(crate) fn admit_original_selection_material_tx(
    tx:&Transaction<'_>,
    selection:TaskResultSelectionRead<'_>,
    observe:&mut dyn FnMut(usize,usize,usize)->Result<()>,
)->Result<()> {
    match selection {
        TaskResultSelectionRead::Owner {agent,work}=>admit(tx,OWNER_SQL,params![agent,work,TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT as i64],true,observe),
        TaskResultSelectionRead::Reply {agent,work,message}=>admit(tx,REPLY_SQL,params![agent,work,message],true,observe),
        TaskResultSelectionRead::Message {message}=>admit(tx,"SELECT payload_json FROM task_result_settlements WHERE message_id=?1",[message],true,observe),
        TaskResultSelectionRead::InvalidOnly {agent,message}=>admit(tx,"SELECT payload_json FROM task_result_settlements WHERE agent_id=?1 AND message_id=?2",params![agent,message],false,observe),
        TaskResultSelectionRead::Activation {agent,activation}=>admit(tx,ACTIVATION_SQL,params![agent,activation],true,observe),
        TaskResultSelectionRead::Wake {agent,work}=>admit(tx,WAKE_SQL,params![agent,work],true,observe),
    }
}
const RESULT_COLUMNS:&str="result_identity,agent_id,task_id,message_id,work_item_id,rejoin_generation,parent_turn_id,state,activation_id,disposition,created_at,updated_at,admitted_at,settled_at,deferred_reason,deferred_at,next_recheck_at";
const RESULT_RAW:&str="COALESCE(length(CAST(result_identity AS BLOB)),0)+COALESCE(length(CAST(agent_id AS BLOB)),0)+COALESCE(length(CAST(task_id AS BLOB)),0)+COALESCE(length(CAST(message_id AS BLOB)),0)+COALESCE(length(CAST(work_item_id AS BLOB)),0)+COALESCE(length(CAST(rejoin_generation AS BLOB)),0)+COALESCE(length(CAST(parent_turn_id AS BLOB)),0)+COALESCE(length(CAST(state AS BLOB)),0)+COALESCE(length(CAST(activation_id AS BLOB)),0)+COALESCE(length(CAST(disposition AS BLOB)),0)+COALESCE(length(CAST(created_at AS BLOB)),0)+COALESCE(length(CAST(updated_at AS BLOB)),0)+COALESCE(length(CAST(admitted_at AS BLOB)),0)+COALESCE(length(CAST(settled_at AS BLOB)),0)+COALESCE(length(CAST(deferred_reason AS BLOB)),0)+COALESCE(length(CAST(deferred_at AS BLOB)),0)+COALESCE(length(CAST(next_recheck_at AS BLOB)),0)";
const TASK_RAW:&str="COALESCE(length(CAST(task_id AS BLOB)),0)+COALESCE(length(CAST(owner_agent_id AS BLOB)),0)+COALESCE(length(CAST(parent_agent_id AS BLOB)),0)+COALESCE(length(CAST(child_agent_id AS BLOB)),0)+COALESCE(length(CAST(kind AS BLOB)),0)+COALESCE(length(CAST(status AS BLOB)),0)+COALESCE(length(CAST(summary AS BLOB)),0)+COALESCE(length(CAST(input_target AS BLOB)),0)+COALESCE(length(CAST(wait_policy AS BLOB)),0)+COALESCE(length(CAST(output_path AS BLOB)),0)+COALESCE(length(CAST(result_summary AS BLOB)),0)+COALESCE(length(CAST(exit_status AS BLOB)),0)+COALESCE(length(CAST(terminal_reentry AS BLOB)),0)+COALESCE(length(CAST(revision AS BLOB)),0)+COALESCE(length(CAST(created_at AS BLOB)),0)+COALESCE(length(CAST(updated_at AS BLOB)),0)+COALESCE(length(CAST(completed_at AS BLOB)),0)+COALESCE(length(CAST(last_message_id AS BLOB)),0)+COALESCE(length(CAST(last_turn_id AS BLOB)),0)+COALESCE(length(CAST(causation_id AS BLOB)),0)+COALESCE(length(CAST(correlation_id AS BLOB)),0)";
fn admit<P:rusqlite::Params+Clone>(
    tx:&Transaction<'_>,sql:&str,params:P,task_projection:bool,
    observe:&mut dyn FnMut(usize,usize,usize)->Result<()>,
)->Result<()> {
    // These finite generated SQL Strings precede both body/address reads.
    // Count their borrowed text and fixed container/statement slots first.
    let sql_bytes=sql.len().checked_add(RESULT_COLUMNS.len()).and_then(|n|n.checked_add(RESULT_RAW.len()))
        .and_then(|n|n.checked_add(TASK_RAW.len())).and_then(|n|n.checked_add(4096)).context("Result read statement material overflow")?;
    observe(sql_bytes,64,8)?;
    let prefix=if sql.starts_with("SELECT settlements.payload_json") {"settlements."} else {""};
    let from=if prefix.is_empty() {"SELECT payload_json"} else {"SELECT settlements.payload_json"};
    let columns=RESULT_COLUMNS.split(',').map(|column|format!("{prefix}{column}")).collect::<Vec<_>>().join(",");
    let selected=sql.replacen(from,&format!("SELECT {prefix}payload_json,{columns}"),1);
    let scalar=format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+{RESULT_RAW}+17*64),0) FROM ({selected})");
    let (count,raw):(usize,usize)=tx.query_row(&scalar,params.clone(),|r|Ok((r.get(0)?,r.get(1)?)))?;
    let slots=count.checked_mul(17+12).and_then(|n|n.checked_add(4)).context("Result selected slots overflow")?;
    observe(raw,slots,12)?;
    // The original selected payload is now guarded before SQLite parses it,
    // and its complete values/text keys are guarded before Rust's decoder.
    let native=format!("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM ({sql}) s,json_tree(s.payload_json) j");
    let nodes:usize=tx.query_row(&native,params.clone(),|r|r.get(0))?;
    observe(0,nodes,12)?;
    if task_projection {
        // The original predicate may classify a foreign/stale Task without
        // decoding its body. Measure scalar fields only; never json_tree it.
        let selected_tasks=sql.replacen(from,&format!("SELECT {prefix}task_id"),1);
        let scalar=format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+{TASK_RAW}+21*64),0) FROM tasks WHERE task_id IN ({selected_tasks})");
        let (count,raw):(usize,usize)=tx.query_row(&scalar,params,|r|Ok((r.get(0)?,r.get(1)?)))?;
        let slots=count.checked_mul(21+8).and_then(|n|n.checked_add(4)).context("Result Task projection slots overflow")?;
        observe(raw,slots,12)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "selection_material_tests.rs"]
mod tests;
