//! Predicate observations for qualified Results, without foreign Task body authority.
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};

// Only fields consumed by the original durable-Task predicate. In particular,
// work_item_id is the raw Task field, not effective_work_item_id's fallback.
pub(super) const TASK_PREDICATE_SQL: &str = "json_array(task_id,owner_agent_id,status,revision,
 json_type(payload_json,'$.id'),json_extract(payload_json,'$.id'),
 json_type(payload_json,'$.agent_id'),json_extract(payload_json,'$.agent_id'),
 json_type(payload_json,'$.session_id'),json_extract(payload_json,'$.session_id'),
 json_type(payload_json,'$.status'),json_extract(payload_json,'$.status'),
 json_type(payload_json,'$.work_item_id'),json_extract(payload_json,'$.work_item_id'),
 json_type(payload_json,'$.parent_message_id'),json_extract(payload_json,'$.parent_message_id'),
 json_type(payload_json,'$.detail'),
 json_type(payload_json,'$.detail.rejoin_obligation_id'),json_extract(payload_json,'$.detail.rejoin_obligation_id'),
 json_type(payload_json,'$.detail.rejoin_generation'),substr(payload_json -> '$.detail.rejoin_generation',1),
 json_type(payload_json,'$.detail.parent_turn_id'),json_extract(payload_json,'$.detail.parent_turn_id'),
 json_type(payload_json,'$.detail.command_creation_id'),json_extract(payload_json,'$.detail.command_creation_id'))";

/// The observation's guards survive its caller's preparation. Neither the
/// observation nor the guards retain a Task body, resident domain or root.
pub(crate) struct PreparedTaskResultDependency {
    pub observation: TaskResultDependencyObservation,
    _guards: Vec<Arc<BudgetViewGuard>>,
}

/// `None` means no original settlement row for this exact pending pair. It does
/// not mean Task absence: an independent current Task request remains strict.
pub(crate) fn prepare_pending_task_result_dependency_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message_id: &str,
    task_id: &str,
    request: &TaskDomainLoadRequest,
    budget: &InputEvidenceBudgetLedger,
) -> Result<Option<PreparedTaskResultDependency>> {
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(budget));
    let mut dependency = prepare_pending_task_result_dependency_using_material_tx(
        tx,
        scope,
        message_id,
        task_id,
        request,
        &mut material,
    )?;
    if let Some(dependency) = &mut dependency {
        dependency._guards = material.into_guards();
    }
    Ok(dependency)
}

/// An original grouped recovery shares one phase allowance before every decode.
pub(in crate::runtime_db) fn prepare_pending_task_result_dependency_using_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message_id: &str,
    task_id: &str,
    request: &TaskDomainLoadRequest,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<Option<PreparedTaskResultDependency>> {
    let identity_bytes: usize = tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",
        [&scope.agent_id], |r| r.get(0),
    )?;
    material.charge_relation(identity_bytes)?;
    maintenance::require_existing_maintenance_scope_tx(tx, scope)?;
    ensure!(
        !message_id.is_empty() && !task_id.is_empty(),
        "empty pending Task/result pair"
    );
    let seed = binding(tx, scope)?;
    // The existing schema's agent/message uniqueness makes this a finite lookup.
    let row: Option<(usize, usize)> = tx.query_row(
        "SELECT length(CAST(result_identity AS BLOB)),length(CAST(payload_json AS BLOB))+length(CAST(json_array(result_identity,agent_id,task_id,message_id,work_item_id,rejoin_generation,parent_turn_id,state,activation_id,disposition,created_at,updated_at,admitted_at,settled_at,deferred_reason,deferred_at,next_recheck_at,command_creation_id) AS BLOB)) FROM task_result_settlements WHERE agent_id=?1 AND message_id=?2",
        params![scope.agent_id,message_id], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let Some((key_bytes, bytes)) = row else {
        return Ok(None);
    };
    bound(
        1,
        bytes
            .checked_add(key_bytes)
            .and_then(|n| n.checked_add(128))
            .context("Task result size overflow")?,
        Some(request),
    )?;
    // Reserve the normalized address and all body/projection copies before
    // decoding even this finite ID: uniqueness does not bound its byte length.
    material.charge_record_address(key_bytes, bytes)?;
    let id: String = tx.query_row(
        "SELECT result_identity FROM task_result_settlements WHERE agent_id=?1 AND message_id=?2",
        params![scope.agent_id, message_id],
        |r| r.get(0),
    )?;
    ensure!(
        id.len() == key_bytes && !id.is_empty(),
        "Task result address is invalid"
    );
    let key = TaskKey::new(TaskFamily::ResultSettlement, id);
    let item = stored_on(tx, &scope.agent_id, &key)?.context("pending Task result disappeared")?;
    qualification::validate_member(tx, scope, seed.as_deref(), &key, &item)?;
    let CanonicalTaskFact::ResultSettlement(result) = &item.fact else {
        unreachable!()
    };
    ensure!(
        result.message_id == message_id && result.task_id == task_id,
        "pending Task/result pair mismatch"
    );
    let observation =
        observe_result_dependency_tx(tx, scope, seed.as_deref(), &item, request, material)?;
    Ok(Some(PreparedTaskResultDependency {
        observation,
        _guards: Vec::new(),
    }))
}

pub(super) fn observe_result_dependency_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    item: &StoredTask,
    request: &TaskDomainLoadRequest,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<TaskResultDependencyObservation> {
    let CanonicalTaskFact::ResultSettlement(result) = &item.fact else {
        anyhow::bail!("Task relation requires Result family")
    };
    let result_key = item.fact.key();
    let revision = qualification::validate_member(tx, scope, seed, &result_key, item)?;
    let result_locator = TaskFactLocator {
        scope: scope.clone(),
        key: result_key,
        revision,
        content_digest: item.digest(),
        projection_digest: item.projection_digest.clone(),
    };
    let (relation, _, _) = observe_task_predicate_tx(tx, scope, seed, result, request, material)?;
    Ok(TaskResultDependencyObservation {
        result: result_locator,
        relation,
    })
}

pub(super) fn observe_task_predicate_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    result: &super::super::task_result_settlement::TaskResultSettlementRecord,
    request: &TaskDomainLoadRequest,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<(
    TaskResultDependencyRelation,
    Option<String>,
    Option<TaskFactLocator>,
)> {
    crate::runtime_db::command_task_creation::validate_source_tx(
        tx,
        scope,
        &result.task_id,
        result.rejoin_generation,
        &result.source,
        result.work_item_id.as_deref(),
        None,
        |bytes, records| material.charge(bytes, records, 8, false),
    )?;
    let key = TaskKey::new(TaskFamily::Task, &result.task_id);
    let current = alias(tx, scope, &key)?;
    let fixed = qualification::eligible_digest(tx, seed, &key)?;
    let fixed_unavailable: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_domain_eligibility WHERE seed_id=?1 AND family='task' AND record_id=?2 AND availability='unavailable')",
        params![seed,key.id], |r| r.get(0),
    )?;
    let size: Option<usize> = tx
        .query_row(
            &format!(
                "SELECT length(CAST({TASK_PREDICATE_SQL} AS BLOB)) FROM tasks WHERE task_id=?1"
            ),
            [&key.id],
            |r| r.get(0),
        )
        .optional()?;
    let metadata_bytes = size
        .unwrap_or_default()
        .checked_add(key.id.len() + 256)
        .context("Task predicate bytes overflow")?;
    bound(1, metadata_bytes, Some(request))?;
    material.charge_relation(metadata_bytes)?;
    let raw: Option<String> = tx
        .query_row(
            &format!("SELECT {TASK_PREDICATE_SQL} FROM tasks WHERE task_id=?1"),
            [&key.id],
            |r| r.get(0),
        )
        .optional()?;
    let invalid = |target, reason, predicate: &[u8]| TaskResultDependencyRelation::Invalid {
        target,
        reason,
        predicate_digest: digest(predicate),
    };
    let Some(raw) = raw else {
        if let Some(alias) = &current {
            match alias.availability {
                TaskAvailability::Present => anyhow::bail!("qualified Result Task body missing"),
                TaskAvailability::Pruned => {
                    return Ok((
                        invalid(
                            TaskDependencyTarget::Pruned { task_id: key.id },
                            TaskDependencyInvalidReason::Pruned,
                            b"own-pruned",
                        ),
                        None,
                        None,
                    ))
                }
                TaskAvailability::Absent => {}
            }
        }
        ensure!(fixed.is_none(), "fixed Result Task canonical body missing");
        if fixed_unavailable {
            return Ok((
                invalid(
                    TaskDependencyTarget::FixedUnavailable { task_id: key.id },
                    TaskDependencyInvalidReason::FixedUnavailable,
                    b"fixed-unavailable",
                ),
                None,
                None,
            ));
        }
        ensure!(
            !qualification::known_anywhere(tx, &key)?,
            "known Result Task body missing"
        );
        return Ok((
            invalid(
                TaskDependencyTarget::GlobalAbsent { task_id: key.id },
                TaskDependencyInvalidReason::Missing,
                b"global-absent",
            ),
            None,
            None,
        ));
    };
    // Tombstones never become a present relation, even when a raw restore exists.
    ensure!(
        !current
            .as_ref()
            .is_some_and(|a| a.availability != TaskAvailability::Present),
        "Result Task alias is absent or pruned but canonical row exists"
    );
    ensure!(
        !fixed_unavailable || fixed.is_some(),
        "unavailable fixed Result Task appeared later"
    );
    let (owner, normalized_revision, matches) = evaluate_predicate(&raw, result)?;
    // A qualified member must validate its complete locator even when the
    // current predicate is false. Small tuple observation cannot mask drift.
    let qualified_revision = if current
        .as_ref()
        .is_some_and(|a| a.availability == TaskAvailability::Present)
        || fixed.is_some()
    {
        let n = checkpoint_material_size_on(tx, &key)?.context("qualified Task disappeared")?;
        bound(
            1,
            metadata_bytes
                .checked_add(n)
                .context("Task relation bytes overflow")?,
            Some(request),
        )?;
        material.charge_record(&key, n)?;
        let stored = stored_on(tx, &scope.agent_id, &key)?.context("qualified Task missing")?;
        Some((
            qualification::validate_member(tx, scope, seed, &key, &stored)?,
            stored,
        ))
    } else {
        None
    };
    let qualified_task = qualified_revision
        .as_ref()
        .map(|(revision, stored)| TaskFactLocator {
            scope: scope.clone(),
            key: key.clone(),
            revision: *revision,
            content_digest: stored.digest(),
            projection_digest: stored.projection_digest.clone(),
        });
    if !matches {
        return Ok((
            invalid(
                TaskDependencyTarget::CanonicalObserved {
                    task_id: key.id,
                    owner: owner.clone(),
                    normalized_revision,
                },
                if owner != scope.agent_id {
                    TaskDependencyInvalidReason::ForeignOwner
                } else {
                    TaskDependencyInvalidReason::PredicateMismatch
                },
                raw.as_bytes(),
            ),
            Some(raw),
            qualified_task,
        ));
    }
    let task = qualified_task.ok_or_else(|| TaskSnapshotError::NeedsPreparation(key.clone()))?;
    Ok((
        TaskResultDependencyRelation::QualifiedCurrent { task: task.clone() },
        Some(raw),
        Some(task),
    ))
}

fn evaluate_predicate(
    raw: &str,
    result: &super::super::task_result_settlement::TaskResultSettlementRecord,
) -> Result<(String, i64, bool)> {
    let tuple: Vec<serde_json::Value> = serde_json::from_str(raw)?;
    ensure!(tuple.len() == 25, "Task predicate tuple shape mismatch");
    let text = |i: usize| tuple[i].as_str();
    let owner = text(1).context("Task predicate owner is not text")?;
    // The SQL revision is updated_at.timestamp_millis(), including epoch zero
    // or earlier clocks. It is an observation, not the positive domain revision.
    let normalized_revision = tuple[3]
        .as_i64()
        .context("Task predicate revision is not an integer")?;
    let payload_agent = match (text(6), text(8)) {
        (Some("text"), None) => text(7),
        (None, Some("text")) => text(9),
        _ => anyhow::bail!("Task payload agent/session identity is missing or ambiguous"),
    }
    .context("Task payload agent is not text")?;
    ensure!(
        text(0) == Some(result.task_id.as_str())
            && text(4) == Some("text")
            && text(5) == text(0)
            && payload_agent == owner
            && text(10) == Some("text")
            && text(11) == text(2),
        "Task predicate normalized/payload identity or status mismatch"
    );
    let status: crate::types::TaskStatus = serde_json::from_value(tuple[11].clone())?;
    for i in [12, 14] {
        ensure!(
            matches!(text(i), None | Some("null") | Some("text")),
            "Task predicate optional string has invalid type"
        );
    }
    let generation = if text(19) == Some("integer") {
        text(20)
            .and_then(|raw| serde_json::from_str::<u64>(raw).ok())
            .filter(|n| *n > 0)
    } else {
        None
    };
    let matches = payload_agent == result.agent_id
        && text(13) == result.work_item_id.as_deref()
        && text(15).is_none_or(|id| id == result.message_id)
        && matches!(
            status,
            crate::types::TaskStatus::Completed
                | crate::types::TaskStatus::Failed
                | crate::types::TaskStatus::Cancelled
                | crate::types::TaskStatus::Interrupted
        )
        && text(16) == Some("object")
        && text(17) == Some("text")
        && text(18) == Some(result.task_id.as_str())
        && !result.task_id.is_empty()
        && generation == Some(result.rejoin_generation)
        && match &result.source {
            crate::domain::execution_protocol::TaskRejoinSource::ParentTurn { parent_turn_id } => {
                text(21) == Some("text")
                    && text(22) == Some(parent_turn_id.as_str())
                    && !parent_turn_id.is_empty()
                    && text(23).is_none()
            }
            crate::domain::execution_protocol::TaskRejoinSource::CommandCreation {
                command_creation_id,
            } => {
                text(21).is_none()
                    && text(23) == Some("text")
                    && text(24) == Some(command_creation_id.as_str())
                    && command_creation_id == &result.task_id
            }
        };
    Ok((owner.to_owned(), normalized_revision, matches))
}

pub(in crate::runtime_db) enum TaskCheckpointDependency {
    Current(TaskKey),
    Missing(TaskKey),
    Invalid,
}
/// Fixed capture uses exactly the same predicate projection as the live loader.
/// Invalid foreign/raw Tasks are never decoded or added to fixed membership.
pub(in crate::runtime_db) fn checkpoint_result_dependency_on(
    connection: &Connection,
    result: &super::super::task_result_settlement::TaskResultSettlementRecord,
) -> Result<(TaskCheckpointDependency, usize)> {
    let size: Option<usize> = connection
        .query_row(
            &format!(
                "SELECT length(CAST({TASK_PREDICATE_SQL} AS BLOB)) FROM tasks WHERE task_id=?1"
            ),
            [&result.task_id],
            |r| r.get(0),
        )
        .optional()?;
    let bytes = size
        .unwrap_or_default()
        .checked_add(result.task_id.len() + 256)
        .context("fixed Task predicate size overflow")?;
    bound(1, bytes, None)?;
    let raw: Option<String> = connection
        .query_row(
            &format!("SELECT {TASK_PREDICATE_SQL} FROM tasks WHERE task_id=?1"),
            [&result.task_id],
            |r| r.get(0),
        )
        .optional()?;
    let dependency = match raw {
        None => TaskCheckpointDependency::Missing(TaskKey::new(TaskFamily::Task, &result.task_id)),
        Some(raw) if evaluate_predicate(&raw, result)?.2 => {
            TaskCheckpointDependency::Current(TaskKey::new(TaskFamily::Task, &result.task_id))
        }
        Some(_) => TaskCheckpointDependency::Invalid,
    };
    Ok((dependency, bytes))
}
