use super::*;
use crate::runtime_db::task_stop::{
    observe_task_stops_with_result_tx, TaskStopInsertedObservation, TaskStopRecord,
};

pub(super) fn observe_with_result(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    records: &[Arc<QualifiedTaskFact>],
    material: &mut read_preparation::TaskReadMaterial<'_>,
    inserted: Option<&TaskStopInsertedObservation<'_, '_>>,
    result: Option<&crate::runtime_db::task_stop::TaskStopResultObservation<'_, '_>>,
) -> Result<(TaskStopCoverage, Vec<Arc<TaskStopRecord>>)> {
    if let Some(ack) = inserted {
        ack.validate_presence_tx(tx)?;
    }
    let mut coverage = TaskStopCoverage::empty_complete();
    let mut bodies = BTreeMap::new();
    // Count first, even for a pending effect whose Task left active/history.
    let pending:usize=tx.query_row("SELECT COUNT(*) FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND (physical_state='pending' OR child_state='pending' OR worktree_state='pending' OR result_state='pending')",
        params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    if pending > 0 {
        let pending = observe_task_stops_with_result_tx(
            tx,
            scope,
            None,
            true,
            |bytes, count| material.charge_stops(bytes, count),
            inserted,
            result,
        )?;
        for record in pending.records {
            coverage.pending_effect_seqs.insert(record.effect_seq);
            coverage.records.insert(
                record.effect_seq,
                TaskStopMetadata {
                    locator: TaskStopLocator::from_record(&record)?,
                    pending: true,
                },
            );
            bodies.insert(record.effect_seq, record);
        }
    }
    for record in records {
        let CanonicalTaskFact::Task(task) = &record.fact else {
            continue;
        };
        // Reserve each head address and locator copy before cloning metadata.
        material.charge_relation(read_preparation::encoded_metadata_bytes(&record.locator)?)?;
        let generation = task_stop_generation_digest(&record.locator, task)?;
        let head:Option<u64>=tx.query_row("SELECT MAX(effect_seq) FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3 AND task_created_at=?4 AND generation_digest=?5",
            params![scope.agent_id,scope.identity_incarnation,task.id,super::super::migrations::timestamp(task.created_at),generation],|r|r.get(0))?;
        if let Some(seq) = head {
            if !bodies.contains_key(&seq) {
                let mut observed = observe_task_stops_with_result_tx(
                    tx,
                    scope,
                    Some(seq),
                    false,
                    |bytes, count| material.charge_stops(bytes, count),
                    inserted,
                    result,
                )?;
                let body = observed
                    .records
                    .pop()
                    .context("exact Stop head missing in original view")?;
                ensure!(observed.records.is_empty(), "multiple exact Stop heads");
                bodies.insert(seq, body);
            }
            let body = &bodies[&seq];
            ensure!(
                body.intent.task.key == record.locator.key
                    && body.intent.generation_digest()? == generation,
                "Stop head has another actual Task generation"
            );
            coverage.records.insert(
                seq,
                TaskStopMetadata {
                    locator: TaskStopLocator::from_record(body)?,
                    pending: body.progress.is_pending(),
                },
            );
        }
        coverage.heads.insert(
            task.id.clone(),
            TaskStopHeadObservation {
                task: record.locator.clone(),
                generation_digest: generation,
                head: head.map(|seq| coverage.records[&seq].locator.clone()),
            },
        );
    }
    Ok((coverage, bodies.into_values().collect()))
}
