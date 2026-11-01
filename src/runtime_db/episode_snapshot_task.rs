//! Fixed Task namespace checkpoint, independent of D1 Episode ownership.
use super::*;
use crate::runtime::resident_task::{CanonicalTaskFact, TaskFamily, TaskKey};
#[path = "episode_snapshot_task_queries.rs"]
pub(in crate::runtime_db) mod queries;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct TaskContinuityCheckpoint {
    pub policy_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_inventory: Option<queries::TaskQueryInventory>,
    pub task_table_observed: bool,
    pub result_table_observed: bool,
    pub record_digests: BTreeMap<String, String>,
    pub projection_digests: BTreeMap<String, String>,
    pub unavailable_refs: BTreeSet<String>,
    /// Optional legacy window fixed by the SQLite upgrade.
    pub terminal: Option<TaskLegacyWindow>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct TaskLegacyWindow {
    pub task_ids: Vec<String>,
    pub is_exhaustive: bool,
}
pub(in crate::runtime_db) fn parse_key(value: &str) -> Result<TaskKey> {
    let (family, id) = value.split_once(':').context("invalid fixed Task key")?;
    ensure!(!id.is_empty(), "empty Task key");
    Ok(TaskKey::new(
        match family {
            "task" => TaskFamily::Task,
            "settlement" => TaskFamily::ResultSettlement,
            _ => anyhow::bail!("invalid fixed Task family"),
        },
        id,
    ))
}
fn key_text(key: &TaskKey) -> String {
    format!(
        "{}:{}",
        match key.family {
            TaskFamily::Task => "task",
            TaskFamily::ResultSettlement => "settlement",
        },
        key.id
    )
}
pub(in crate::runtime_db::episode_snapshot) fn capture(
    connection: &Connection,
    agent: &str,
    tables: Option<&BTreeSet<String>>,
    roots: &EpisodeContinuityRoots,
) -> Result<TaskContinuityCheckpoint> {
    let mut out = TaskContinuityCheckpoint {
        policy_version: 1,
        query_inventory: None,
        task_table_observed: root_table_available(tables, "tasks"),
        result_table_observed: root_table_available(tables, "task_result_settlements"),
        record_digests: BTreeMap::new(),
        projection_digests: BTreeMap::new(),
        unavailable_refs: BTreeSet::new(),
        terminal: None,
    };
    let mut keys = BTreeSet::new();
    let mut bytes = 0usize;
    for text in roots.record_digests.keys() {
        if text.starts_with("task:") || text.starts_with("settlement:") {
            insert(&mut keys, parse_key(text)?, &mut bytes)?;
        }
    }
    {
        if out.task_table_observed {
            for id in ids(connection,"SELECT task_id FROM tasks WHERE owner_agent_id=?1 AND status IN ('queued','running','cancelling') ORDER BY task_id",agent)?{insert(&mut keys,TaskKey::new(TaskFamily::Task,id),&mut bytes)?;}
        }
        if out.result_table_observed {
            for id in ids(connection,"SELECT result_identity FROM task_result_settlements WHERE agent_id=?1 AND state!='settled' ORDER BY result_identity",agent)?{insert(&mut keys,TaskKey::new(TaskFamily::ResultSettlement,id),&mut bytes)?;}
        }
        let mut terminal = Vec::new();
        if out.task_table_observed {
            let mut q=connection.prepare("SELECT task_id FROM tasks WHERE owner_agent_id=?1 AND status NOT IN ('queued','running','cancelling') ORDER BY updated_at DESC,created_at DESC,task_id ASC LIMIT 129")?;
            for row in q.query_map([agent], |r| r.get::<_, String>(0))? {
                terminal.push(row?);
            }
        }
        let exhaustive = terminal.len() <= 128;
        terminal.truncate(128);
        for id in &terminal {
            insert(&mut keys, TaskKey::new(TaskFamily::Task, id), &mut bytes)?;
        }
        out.terminal = Some(TaskLegacyWindow {
            task_ids: terminal,
            is_exhaustive: exhaustive,
        });
    }
    // Complete result index is fixed for the selected Tasks; later readers never rescan a newer tail.
    let mut pending: VecDeque<_> = keys.iter().cloned().collect();
    let mut inspected = 0usize;
    while let Some(key) = pending.pop_front() {
        let table = match key.family {
            TaskFamily::Task => "tasks",
            TaskFamily::ResultSettlement => "task_result_settlements",
        };
        if !root_table_available(tables, table) {
            out.unavailable_refs.insert(key_text(&key));
            continue;
        }
        let n = super::super::task_snapshot::checkpoint_material_size_on(connection, &key)?;
        inspected = inspected
            .checked_add(n.unwrap_or_default())
            .context("Task checkpoint bytes overflow")?;
        ensure!(
            inspected <= CONTINUITY_MAX_PAYLOAD_BYTES,
            "Task checkpoint body budget exceeded"
        );
        let Some((fact, digest, projection)) =
            super::super::task_snapshot::checkpoint_fact_on(connection, agent, &key)?
        else {
            out.unavailable_refs.insert(key_text(&key));
            continue;
        };
        out.record_digests.insert(key_text(&key), digest);
        out.projection_digests.insert(key_text(&key), projection);
        let related = match fact {
            CanonicalTaskFact::Task(task) => {
                let related = Vec::new();
                if out.result_table_observed {
                    let mut q=connection.prepare("SELECT result_identity FROM task_result_settlements WHERE agent_id=?1 AND task_id=?2 ORDER BY result_identity")?;
                    for row in q.query_map(params![agent, task.id], |r| r.get::<_, String>(0))? {
                        let key = TaskKey::new(TaskFamily::ResultSettlement, row?);
                        // Bound before collecting another reverse-index key.
                        if insert(&mut keys, key.clone(), &mut bytes)? {
                            pending.push_back(key);
                        }
                    }
                }
                related
            }
            CanonicalTaskFact::ResultSettlement(result) => {
                if !out.task_table_observed {
                    out.unavailable_refs
                        .insert(key_text(&TaskKey::new(TaskFamily::Task, result.task_id)));
                    Vec::new()
                } else {
                    use super::super::task_snapshot::{
                        checkpoint_result_dependency_on, TaskCheckpointDependency,
                    };
                    let (dependency, bytes) = checkpoint_result_dependency_on(connection, &result)?;
                    inspected = inspected
                        .checked_add(bytes)
                        .context("fixed Task predicate bytes overflow")?;
                    ensure!(
                        inspected <= CONTINUITY_MAX_PAYLOAD_BYTES,
                        "Task checkpoint predicate budget exceeded"
                    );
                    match dependency {
                        TaskCheckpointDependency::Current(key) => vec![key],
                        TaskCheckpointDependency::Missing(key) => {
                            out.unavailable_refs.insert(key_text(&key));
                            Vec::new()
                        }
                        TaskCheckpointDependency::Invalid => Vec::new(),
                    }
                }
            }
        };
        for related in related {
            if insert(&mut keys, related.clone(), &mut bytes)? {
                pending.push_back(related);
            }
        }
    }
    out.query_inventory = Some(queries::capture(connection, agent, tables)?);
    ensure!(
        serde_json::to_vec(&out)?.len() <= CONTINUITY_MAX_PAYLOAD_BYTES,
        "Task checkpoint metadata budget exceeded"
    );
    Ok(out)
}
fn insert(keys: &mut BTreeSet<TaskKey>, key: TaskKey, bytes: &mut usize) -> Result<bool> {
    if keys.contains(&key) {
        return Ok(false);
    }
    let next = bytes
        .checked_add(key.id.len() + 128)
        .context("Task checkpoint metadata overflow")?;
    ensure!(
        keys.len() < CONTINUITY_MAX_RECORDS && next <= CONTINUITY_MAX_PAYLOAD_BYTES,
        "Task checkpoint metadata budget exceeded"
    );
    *bytes = next;
    Ok(keys.insert(key))
}
fn ids(connection: &Connection, sql: &str, agent: &str) -> Result<Vec<String>> {
    let mut q = connection.prepare(sql)?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for row in q.query_map([agent], |r| r.get::<_, String>(0))? {
        let id = row?;
        bytes += id.len() + 128;
        ensure!(
            out.len() < CONTINUITY_MAX_RECORDS && bytes <= CONTINUITY_MAX_PAYLOAD_BYTES,
            "Task checkpoint ID budget exceeded"
        );
        out.push(id)
    }
    Ok(out)
}
pub(in crate::runtime_db) fn load(
    connection: &Connection,
    seed: &str,
    phase: &str,
) -> Result<TaskContinuityCheckpoint> {
    let sql = if phase == "first_import" {
        "SELECT roots_json FROM episode_continuity_imports WHERE seed_id=?1"
    } else {
        "SELECT roots_json FROM episode_continuity_seeds WHERE seed_id=?1"
    };
    let size_sql = if phase == "first_import" {
        "SELECT length(CAST(roots_json AS BLOB)) FROM episode_continuity_imports WHERE seed_id=?1"
    } else {
        "SELECT length(CAST(roots_json AS BLOB)) FROM episode_continuity_seeds WHERE seed_id=?1"
    };
    let size: usize = connection.query_row(size_sql, [seed], |r| r.get(0))?;
    ensure!(
        size <= CONTINUITY_MAX_PAYLOAD_BYTES,
        "fixed Task root material budget exceeded"
    );
    let roots: EpisodeContinuityRoots =
        serde_json::from_str(&connection.query_row(sql, [seed], |r| r.get::<_, String>(0))?)?;
    let out = roots
        .task_domain_checkpoint
        .context("fixed Task checkpoint missing from same-release cutover")?;
    ensure!(out.policy_version == 1, "unsupported fixed Task checkpoint");
    Ok(out)
}
