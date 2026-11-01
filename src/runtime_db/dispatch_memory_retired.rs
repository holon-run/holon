//! A retired identity may hand off unfinished memory evidence, never finish D1.
use super::*;
use crate::{
    runtime::ended_dispatch::EndedDispatchAuthorization,
    runtime_db::control_transition::{
        ControlCancellationKind, ControlCancellationTarget, ControlKey,
    },
};

const ADDRESS_BYTES: usize = 64 * 1024;

/// Returned only after the original physical Arc and canonical Stop validate.
/// This serializable address cannot itself authorize a new handoff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetiredPhysicalStopAddress {
    pub(crate) control_key: ControlKey,
    pub(crate) control_digest: String,
    pub(crate) obligation_id: String,
    pub(crate) owner_epoch: u64,
    pub(crate) run_id: String,
    pub(crate) runtime_lease_id: String,
    pub(crate) activation_id: Option<String>,
    pub(crate) source_message_id: String,
    pub(crate) source_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetiredDispatchMemoryHandoff {
    binding: DispatchMemoryBinding,
    checkpoint_digest: String,
    original_start: PreparedInputMutation,
    physical_stop: RetiredPhysicalStopAddress,
    stop_address_digest: String,
    target_digest: String,
    handed_off_at: DateTime<Utc>,
}

/// Retired-address lookup reads only the original Turn/header, never State.
pub(crate) fn load_retired_dispatch_memory_binding_tx(
    tx: &Transaction<'_>,
    original_scope: &InputEvidenceScope,
    actual_turn_id: &str,
) -> Result<Option<DispatchMemoryBinding>> {
    ensure!(
        retired_identity_in(tx, original_scope)?,
        "memory handoff identity is not retired"
    );
    let key = EpisodeMemoryKey {
        agent_id: original_scope.agent_id.clone(),
        identity_incarnation: original_scope.identity_incarnation,
        actual_dispatch_turn_id: actual_turn_id.into(),
    };
    let (agent, index, payload_agent, payload_index, run): (String, u64, String, u64, Option<String>) = tx.query_row(
        "SELECT agent_id,turn_index,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.turn_index'),json_extract(payload_json,'$.run_id') FROM turn_records WHERE turn_id=?1",
        [actual_turn_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    ensure!(
        agent == original_scope.agent_id && payload_agent == agent && index == payload_index,
        "retired memory Turn address differs"
    );
    let address: Option<(String, u64)> = tx.query_row(
        "SELECT source_message_id,turn_index FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
        params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let address = match address {
        Some(address) => address,
        None => tx.query_row(
            "SELECT source_message_id,turn_index FROM dispatch_memory_legacy_dispatches WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
            params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.context(DispatchMemoryError::MissingStart)?,
    };
    ensure!(address.1 == index, "retired memory seed Turn index differs");
    let invocation = EpisodeMemoryInvocation {
        message_id: address.0,
        turn_index: index,
    };
    super::binding::load_binding_header_tx(tx, original_scope, &key, &invocation, &run)
}

/// Metadata-only write under the original owner reservation. This does not
/// install State, acknowledge D1, settle a physical obligation or remove rows.
pub(crate) fn prepare_retired_physical_handoff_tx(
    tx: &Transaction<'_>,
    original_scope: &InputEvidenceScope,
    actual_turn_id: &str,
    authorization: &EndedDispatchAuthorization,
    at: DateTime<Utc>,
) -> Result<Option<RetiredDispatchMemoryHandoff>> {
    let Some(binding) =
        load_retired_dispatch_memory_binding_tx(tx, original_scope, actual_turn_id)?
    else {
        return Ok(None);
    };
    let address = authorization.retired_physical_stop_address_tx(tx, &binding)?;
    validate_address(&binding, &address)?;
    let header = original_header_in(tx, binding.key(), binding.invocation())?;
    ensure!(
        header.seed_digest == binding.seed_digest()
            && header.source_sequence == address.source_sequence,
        "retired physical source differs from original Start"
    );
    let (stop_address_digest, target_digest) = stop_addresses_in(tx, &binding, &address)?;
    let proposed = RetiredDispatchMemoryHandoff {
        binding,
        checkpoint_digest: header.checkpoint_digest,
        original_start: header.original_start,
        physical_stop: address,
        stop_address_digest,
        target_digest,
        handed_off_at: at,
    };
    if let Some(existing) = load_handoff_in(tx, proposed.binding.key())? {
        validate_handoff_in(tx, &existing)?;
        let mut repeated = proposed;
        repeated.handed_off_at = existing.handed_off_at;
        ensure!(
            repeated == existing,
            "retired memory handoff conflicts with original proof"
        );
        return Ok(Some(existing));
    }
    let payload = bounded_address(&proposed)?;
    let key = proposed.binding.key();
    let invocation = proposed.binding.invocation();
    tx.execute("INSERT INTO dispatch_memory_retired_handoffs(agent_id,identity_incarnation,turn_id,source_message_id,turn_index,seed_digest,checkpoint_digest,control_command_id,control_digest,run_id,runtime_lease_id,record_digest,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id,invocation.message_id,invocation.turn_index,proposed.binding.seed_digest(),proposed.checkpoint_digest,proposed.physical_stop.control_key.command_id,proposed.physical_stop.control_digest,proposed.physical_stop.run_id,proposed.physical_stop.runtime_lease_id,canonical_command_digest(&proposed)?,payload])?;
    let saved = load_handoff_in(tx, key)?.context("retired handoff insert disappeared")?;
    ensure!(
        saved == proposed,
        "retired handoff actual ACK differs from prepared disposition"
    );
    validate_handoff_in(tx, &saved)?;
    Ok(Some(saved))
}

/// Retention validates the immutable receipt addresses after evidence pruning.
/// No old State or Message/Turn body is needed to keep this disposition valid.
pub(super) fn validated_retired_handoff_in(
    connection: &Connection,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
) -> Result<bool> {
    let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='dispatch_memory_retired_handoffs')", [], |r| r.get(0))?;
    if !exists {
        return Ok(false);
    }
    let Some(record) = load_handoff_in(connection, key)? else {
        return Ok(false);
    };
    ensure!(
        record.binding.invocation() == invocation,
        "retired handoff invocation differs"
    );
    validate_handoff_in(connection, &record)?;
    Ok(true)
}

/// An observed durable disposition is reusable after normal evidence pruning.
/// These raw address arguments can only locate a previously authorized row.
pub(crate) fn retired_physical_handoff_exists_tx(
    tx: &Transaction<'_>,
    original_scope: &InputEvidenceScope,
    run_id: &str,
    runtime_lease_id: &str,
    owner_epoch: u64,
    source_message_id: &str,
) -> Result<bool> {
    ensure!(
        !run_id.is_empty()
            && !runtime_lease_id.is_empty()
            && owner_epoch > 0
            && !source_message_id.is_empty(),
        "incomplete retired handoff lookup address"
    );
    let mut query = tx.prepare("SELECT turn_id FROM dispatch_memory_retired_handoffs WHERE agent_id=?1 AND identity_incarnation=?2 AND run_id=?3 AND runtime_lease_id=?4 ORDER BY turn_id LIMIT 2")?;
    let ids = query
        .query_map(
            params![
                original_scope.agent_id,
                original_scope.identity_incarnation,
                run_id,
                runtime_lease_id
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        ids.len() <= 1,
        "original Run has multiple retired memory handoffs"
    );
    let Some(turn_id) = ids.first() else {
        return Ok(false);
    };
    let key = EpisodeMemoryKey {
        agent_id: original_scope.agent_id.clone(),
        identity_incarnation: original_scope.identity_incarnation,
        actual_dispatch_turn_id: turn_id.clone(),
    };
    let record = load_handoff_in(tx, &key)?.context("retired handoff disappeared")?;
    validate_handoff_in(tx, &record)?;
    ensure!(
        record.physical_stop.run_id == run_id
            && record.physical_stop.runtime_lease_id == runtime_lease_id
            && record.physical_stop.owner_epoch == owner_epoch
            && record.physical_stop.source_message_id == source_message_id,
        "retired handoff lookup differs from original physical address"
    );
    Ok(true)
}

fn retired_identity_in(connection: &Connection, original: &InputEvidenceScope) -> Result<bool> {
    ensure!(
        !original.agent_id.is_empty() && original.identity_incarnation > 0,
        "invalid retired memory scope"
    );
    let current: Option<(String, String, String, u64)> = connection.query_row(
        "SELECT agent_id,status,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.incarnation') FROM agent_identities WHERE agent_id=?1",
        [&original.agent_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((agent, status, payload_agent, incarnation)) = current else {
        return Ok(true);
    };
    let payload_status: String = connection.query_row(
        "SELECT json_extract(payload_json,'$.status') FROM agent_identities WHERE agent_id=?1",
        [&original.agent_id],
        |r| r.get(0),
    )?;
    ensure!(
        agent == original.agent_id
            && payload_agent == agent
            && payload_status == status
            && incarnation >= original.identity_incarnation
            && matches!(status.as_str(), "active" | "deleting" | "deleted"),
        "retired memory identity columns and payload disagree"
    );
    Ok(incarnation > original.identity_incarnation || status == "deleted")
}

struct OriginalHeader {
    seed_digest: String,
    checkpoint_digest: String,
    original_start: PreparedInputMutation,
    source_sequence: u64,
}
fn original_header_in(
    connection: &Connection,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
) -> Result<OriginalHeader> {
    validate_key(key, invocation)?;
    let header_sql = "json_object('key',json_extract(payload_json,'$.start.key'),'invocation',json_extract(payload_json,'$.start.invocation'),'digest',json_extract(payload_json,'$.start.digest'),'source_sequence',json_extract(payload_json,'$.start.source_sequence'),'original_start',json_extract(payload_json,'$.original_start'))";
    let size: usize = connection.query_row(&format!("SELECT length(CAST({header_sql} AS BLOB)) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3"), params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id], |r| r.get(0))?;
    ensure!(
        size <= ADDRESS_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    #[derive(Deserialize)]
    struct Header {
        key: EpisodeMemoryKey,
        invocation: EpisodeMemoryInvocation,
        digest: String,
        source_sequence: u64,
        original_start: PreparedInputMutation,
    }
    let (source,index,seed,checkpoint,status,payload_status,payload): (String,u64,String,String,String,String,String) = connection.query_row(
        &format!("SELECT source_message_id,turn_index,seed_digest,record_digest,state,json_extract(payload_json,'$.state'),{header_sql} FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3"),
        params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    let header: Header = serde_json::from_str(&payload)?;
    ensure!(
        header.key == *key
            && header.invocation == *invocation
            && source == invocation.message_id
            && index == invocation.turn_index
            && header.digest == seed
            && valid_digest(&seed)
            && valid_digest(&checkpoint)
            && header.source_sequence > 0
            && status == payload_status
            && matches!(status.as_str(), "started" | "pending"),
        "retired memory original header differs"
    );
    validate_descriptor(&header.original_start)?;
    ensure!(
        header.original_start.family() == InputMutationFamily::AgentPosture
            && header.original_start.scopes().contains(&scope(key)),
        "retired memory Start has another original scope"
    );
    let receipt_size: usize = connection.query_row("SELECT length(CAST(prepared_json AS BLOB)) FROM input_mutation_receipts WHERE command_nonce=?1", [header.original_start.command_nonce()], |r| r.get(0))?;
    ensure!(
        receipt_size <= ADDRESS_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    let (digest, scope_digest, prepared): (String,String,String) = connection.query_row("SELECT command_digest,scope_digest,prepared_json FROM input_mutation_receipts WHERE command_nonce=?1", [header.original_start.command_nonce()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    ensure!(
        digest == header.original_start.command_digest()
            && scope_digest == header.original_start.scope_digest()
            && serde_json::from_str::<PreparedInputMutation>(&prepared)? == header.original_start,
        "retired memory original Start receipt differs"
    );
    Ok(OriginalHeader {
        seed_digest: seed,
        checkpoint_digest: checkpoint,
        original_start: header.original_start,
        source_sequence: header.source_sequence,
    })
}

fn validate_address(
    binding: &DispatchMemoryBinding,
    address: &RetiredPhysicalStopAddress,
) -> Result<()> {
    binding.validate()?;
    ensure!(
        address.control_key.agent_id == binding.key().agent_id
            && address.control_key.identity_incarnation == binding.key().identity_incarnation
            && !address.control_key.command_id.is_empty()
            && valid_digest(&address.control_digest)
            && !address.obligation_id.is_empty()
            && address.owner_epoch > 0
            && !address.run_id.is_empty()
            && !address.runtime_lease_id.is_empty()
            && address.source_message_id == binding.invocation().message_id
            && address.source_sequence > 0,
        "retired physical Stop address differs"
    );
    Ok(())
}

fn stop_addresses_in(
    connection: &Connection,
    binding: &DispatchMemoryBinding,
    address: &RetiredPhysicalStopAddress,
) -> Result<(String, String)> {
    validate_address(binding, address)?;
    let key = &address.control_key;
    let size: usize = connection.query_row("SELECT length(CAST(receipt_json AS BLOB)) FROM control_command_receipts WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3", params![key.agent_id,key.identity_incarnation,key.command_id], |r| r.get(0))?;
    ensure!(
        size <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    // The full canonical validator ran when the typed physical proof was minted.
    // Freeze only this disposition's immutable address, not unrelated outcome bodies.
    let (version,action,digest,invocation_digest,committed_at,projected): (u32,String,String,String,String,String) = connection.query_row(
        "SELECT family_version,action,command_digest,invocation_digest,committed_at,json_object('version',json_extract(receipt_json,'$.family_version'),'key',json_extract(receipt_json,'$.key'),'action',json_extract(receipt_json,'$.invocation.action'),'outcome_action',json_extract(receipt_json,'$.outcome.action'),'digest',json_extract(receipt_json,'$.command_digest'),'invocation_digest',json_extract(receipt_json,'$.invocation_digest'),'committed_at',json_extract(receipt_json,'$.committed_at'),'original_run',json_extract(receipt_json,'$.outcome.original_current_run_id'),'aborted_run',json_extract(receipt_json,'$.outcome.aborted_run_id'),'status',json_extract(receipt_json,'$.outcome.status'),'current_run',json_extract(receipt_json,'$.outcome.current_run_id'),'source_aborted',EXISTS(SELECT 1 FROM json_each(json_extract(receipt_json,'$.outcome.aborted_queue_message_ids')) WHERE value=?4),'turn_interrupted',EXISTS(SELECT 1 FROM json_each(json_extract(receipt_json,'$.outcome.interrupted_turn_ids')) WHERE value=?5),'obligation_present',EXISTS(SELECT 1 FROM json_each(json_extract(receipt_json,'$.obligation_ids')) WHERE value=?6)) FROM control_command_receipts WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3",
        params![key.agent_id,key.identity_incarnation,key.command_id,address.source_message_id,binding.key().actual_dispatch_turn_id,address.obligation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    ensure!(
        projected.len() <= ADDRESS_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    let value: serde_json::Value = serde_json::from_str(&projected)?;
    let committed_at: DateTime<Utc> = committed_at.parse()?;
    let expected = serde_json::json!({"version":1,"key":key,"action":"stop","outcome_action":"stop","digest":address.control_digest,"invocation_digest":invocation_digest,"committed_at":committed_at,"original_run":address.run_id,"aborted_run":address.run_id,"status":"stopped","current_run":null,"source_aborted":1,"turn_interrupted":1,"obligation_present":1});
    ensure!(
        version == 1
            && action == "stop"
            && digest == address.control_digest
            && valid_digest(&invocation_digest)
            && value == expected,
        "retired memory original Stop receipt address differs"
    );
    let target_size: usize = connection.query_row("SELECT length(CAST(target_json AS BLOB))+length(CAST(target_generation_json AS BLOB)) FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3 AND obligation_id=?4", params![key.agent_id,key.identity_incarnation,key.command_id,address.obligation_id], |r| r.get(0))?;
    ensure!(
        target_size <= 16 * 1024,
        DispatchMemoryError::ResourceExhausted
    );
    let (kind,identity,generation,payload): (String,String,String,String) = connection.query_row("SELECT target_kind,target_identity,target_generation_json,target_json FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3 AND obligation_id=?4", params![key.agent_id,key.identity_incarnation,key.command_id,address.obligation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let target: ControlCancellationTarget = serde_json::from_str(&payload)?;
    let generation: serde_json::Value = serde_json::from_str(&generation)?;
    ensure!(
        kind == "run"
            && identity == address.run_id
            && target.target_kind == ControlCancellationKind::Run
            && target.obligation_id == address.obligation_id
            && target.target_identity == identity
            && target.target_generation == generation
            && generation.get("owner_epoch") == Some(&serde_json::json!(address.owner_epoch))
            && generation.get("runtime_lease_id")
                == Some(&serde_json::json!(address.runtime_lease_id))
            && generation.get("activation_id") == Some(&serde_json::json!(address.activation_id))
            && generation.get("source_message_id")
                == Some(&serde_json::json!(address.source_message_id))
            && target.target.get("reason") == Some(&serde_json::json!("agent_stopped")),
        "retired memory original Run target differs"
    );
    Ok((
        canonical_command_digest(&value)?,
        canonical_command_digest(&target)?,
    ))
}

fn load_handoff_in(
    connection: &Connection,
    key: &EpisodeMemoryKey,
) -> Result<Option<RetiredDispatchMemoryHandoff>> {
    let size: Option<usize> = connection.query_row("SELECT length(CAST(payload_json AS BLOB)) FROM dispatch_memory_retired_handoffs WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3", params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id], |r| r.get(0)).optional()?;
    let Some(size) = size else {
        return Ok(None);
    };
    ensure!(
        size <= ADDRESS_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    let (source,index,seed,checkpoint,command,digest,run,lease,record_digest,payload): (String,u64,String,String,String,String,String,String,String,String) = connection.query_row("SELECT source_message_id,turn_index,seed_digest,checkpoint_digest,control_command_id,control_digest,run_id,runtime_lease_id,record_digest,payload_json FROM dispatch_memory_retired_handoffs WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3", params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
    let record: RetiredDispatchMemoryHandoff = serde_json::from_str(&payload)?;
    ensure!(
        record.binding.key() == key
            && record.binding.invocation().message_id == source
            && record.binding.invocation().turn_index == index
            && record.binding.seed_digest() == seed
            && record.checkpoint_digest == checkpoint
            && record.physical_stop.control_key.command_id == command
            && record.physical_stop.control_digest == digest
            && record.physical_stop.run_id == run
            && record.physical_stop.runtime_lease_id == lease
            && canonical_command_digest(&record)? == record_digest,
        "retired memory handoff columns and payload disagree"
    );
    Ok(Some(record))
}

fn validate_handoff_in(
    connection: &Connection,
    record: &RetiredDispatchMemoryHandoff,
) -> Result<()> {
    ensure!(
        retired_identity_in(connection, &record.binding.scope())?,
        "retired memory handoff identity is no longer retired"
    );
    validate_address(&record.binding, &record.physical_stop)?;
    let header = original_header_in(
        connection,
        record.binding.key(),
        record.binding.invocation(),
    )?;
    ensure!(
        record.binding.seed_digest() == header.seed_digest
            && record.checkpoint_digest == header.checkpoint_digest
            && record.original_start == header.original_start
            && record.physical_stop.source_sequence == header.source_sequence,
        "retired memory checkpoint changed after handoff"
    );
    let (stop, target) = stop_addresses_in(connection, &record.binding, &record.physical_stop)?;
    ensure!(
        stop == record.stop_address_digest && target == record.target_digest,
        "retired memory immutable Stop address changed"
    );
    Ok(())
}

fn bounded_address(value: &impl Serialize) -> Result<String> {
    let payload = serde_json::to_string(value)?;
    ensure!(
        payload.len() <= ADDRESS_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    Ok(payload)
}
