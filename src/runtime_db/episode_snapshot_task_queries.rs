//! Original fixed query members live durably; no resident all-history map.
use super::*;
use crate::types::{MessageKind, MessageOrigin, TaskKind, TaskStatus};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct TaskQueryInventory {
    pub id: String,
    pub agent_id: String,
    pub task_count: u64,
    pub message_count: u64,
    pub result_count: u64,
    pub task_table_observed: bool,
    pub message_table_observed: bool,
    pub result_table_observed: bool,
    pub metadata_digest: String,
}

/// Only the original fixed Task query capture can construct this cutover facet.
pub(in crate::runtime_db) struct FixedOutputInputCapture<'a> {
    connection: &'a Connection,
    inventory: &'a str,
    agent: &'a str,
    tables: Option<&'a BTreeSet<String>>,
}
impl FixedOutputInputCapture<'_> {
    pub(in crate::runtime_db) fn validate(&self, connection: &Connection) -> Result<()> {
        ensure!(
            std::ptr::eq(connection, self.connection),
            "focused Output capture is not its original cutover"
        );
        let open:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM task_query_fixed_manifests WHERE inventory_id=?1 AND agent_id=?2 AND sealed=0)",params![self.inventory,self.agent],|row|row.get(0))?;
        ensure!(
            open,
            "focused Output capture inventory is not its original open manifest"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn inventory(&self) -> &str {
        self.inventory
    }
    pub(in crate::runtime_db) fn agent(&self) -> &str {
        self.agent
    }
    pub(in crate::runtime_db) fn table_available(&self, table: &str) -> bool {
        self.tables.is_none_or(|tables| tables.contains(table))
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS task_query_fixed_manifests (
 inventory_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL,
 task_count INTEGER NOT NULL DEFAULT 0, message_count INTEGER NOT NULL DEFAULT 0, result_count INTEGER NOT NULL DEFAULT 0,
 task_observed INTEGER NOT NULL, message_observed INTEGER NOT NULL, result_observed INTEGER NOT NULL,
 metadata_digest TEXT, sealed INTEGER NOT NULL DEFAULT 0 CHECK(sealed IN (0,1))
);
CREATE TABLE IF NOT EXISTS task_query_fixed_tasks (
 inventory_id TEXT NOT NULL REFERENCES task_query_fixed_manifests(inventory_id),
 task_id TEXT NOT NULL, content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 reply_message_id TEXT, created_at TEXT NOT NULL,
 parent_turn_id TEXT, task_status TEXT NOT NULL DEFAULT 'queued',
 header_digest TEXT CHECK(header_digest IS NULL OR length(header_digest)=64),
 generation_created_at TEXT NOT NULL, generation_digest TEXT NOT NULL CHECK(length(generation_digest)=64),
 PRIMARY KEY(inventory_id,task_id)
);
CREATE INDEX IF NOT EXISTS idx_task_query_fixed_task_id ON task_query_fixed_tasks(task_id);
CREATE TABLE IF NOT EXISTS task_query_fixed_results (
 inventory_id TEXT NOT NULL REFERENCES task_query_fixed_manifests(inventory_id),
 result_identity TEXT NOT NULL, task_id TEXT NOT NULL,
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 PRIMARY KEY(inventory_id,result_identity)
);
CREATE INDEX IF NOT EXISTS idx_task_query_fixed_result_id ON task_query_fixed_results(result_identity);
CREATE INDEX IF NOT EXISTS idx_task_query_fixed_result_task ON task_query_fixed_results(inventory_id,task_id);
CREATE TABLE IF NOT EXISTS task_query_fixed_messages (
 inventory_id TEXT NOT NULL REFERENCES task_query_fixed_manifests(inventory_id),
 evidence_id TEXT NOT NULL, message_id TEXT, message_seq INTEGER, created_at TEXT NOT NULL,
 source_task_id TEXT, is_result INTEGER NOT NULL CHECK(is_result IN (0,1)),
 message_status TEXT NOT NULL DEFAULT 'queued' CHECK(message_status IN ('queued','running','cancelling','completed','failed','cancelled','interrupted')),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64), fingerprint_json TEXT NOT NULL,
 running_created_at TEXT, running_generation_digest TEXT CHECK(running_generation_digest IS NULL OR length(running_generation_digest)=64),
 CHECK((running_created_at IS NULL)=(running_generation_digest IS NULL)),
 PRIMARY KEY(inventory_id,evidence_id)
);
CREATE TABLE IF NOT EXISTS task_query_fixed_input_members (
 inventory_id TEXT NOT NULL REFERENCES task_query_fixed_manifests(inventory_id),
 family TEXT NOT NULL CHECK(family IN ('turn','tool_execution','brief','transcript','attempt','outcome','work_execution')),
 record_id TEXT NOT NULL,
 related_turn_id TEXT,
 fingerprint_json TEXT NOT NULL CHECK(json_valid(fingerprint_json)),
 PRIMARY KEY(inventory_id,family,record_id)
);
CREATE TABLE IF NOT EXISTS task_output_fixed_input_prunes (
 inventory_id TEXT NOT NULL REFERENCES task_query_fixed_manifests(inventory_id),
 family TEXT NOT NULL, record_id TEXT NOT NULL, fingerprint_json TEXT NOT NULL,
 content_digest TEXT CHECK(content_digest IS NULL OR length(content_digest)=64),
 PRIMARY KEY(inventory_id,family,record_id)
);
CREATE INDEX IF NOT EXISTS idx_task_query_fixed_input_turn ON task_query_fixed_input_members(inventory_id,family,related_turn_id,record_id);
CREATE TABLE IF NOT EXISTS task_output_current_input_relations (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL, record_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 related_turn_id TEXT, PRIMARY KEY(agent_id,identity_incarnation,family,record_id)
);
CREATE INDEX IF NOT EXISTS idx_task_output_current_input_turn ON task_output_current_input_relations(agent_id,identity_incarnation,family,related_turn_id,record_id);
CREATE TRIGGER IF NOT EXISTS task_query_fixed_input_insert BEFORE INSERT ON task_query_fixed_input_members WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task output Input members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_input_update BEFORE UPDATE ON task_query_fixed_input_members WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 OR (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task output Input members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_input_delete BEFORE DELETE ON task_query_fixed_input_members WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task output Input members are immutable'); END;
CREATE INDEX IF NOT EXISTS idx_task_query_fixed_message_task ON task_query_fixed_messages(inventory_id,source_task_id,is_result);
CREATE TRIGGER IF NOT EXISTS task_query_fixed_manifest_immutable_insert BEFORE INSERT ON task_query_fixed_manifests WHEN EXISTS(SELECT 1 FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed Task query inventory is immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_manifest_immutable_update BEFORE UPDATE ON task_query_fixed_manifests WHEN OLD.sealed=1 OR EXISTS(SELECT 1 FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed Task query inventory is immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_manifest_immutable_delete BEFORE DELETE ON task_query_fixed_manifests WHEN OLD.sealed=1 BEGIN SELECT RAISE(ABORT,'fixed Task query inventory is immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_task_insert BEFORE INSERT ON task_query_fixed_tasks WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_task_update BEFORE UPDATE ON task_query_fixed_tasks WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 OR (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_task_delete BEFORE DELETE ON task_query_fixed_tasks WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_result_insert BEFORE INSERT ON task_query_fixed_results WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_result_update BEFORE UPDATE ON task_query_fixed_results WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 OR (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_result_delete BEFORE DELETE ON task_query_fixed_results WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_message_insert BEFORE INSERT ON task_query_fixed_messages WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_message_update BEFORE UPDATE ON task_query_fixed_messages WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 OR (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS task_query_fixed_message_delete BEFORE DELETE ON task_query_fixed_messages WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END;
"#;

pub(in crate::runtime_db) fn ensure_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(SCHEMA)?;
    Ok(())
}

pub(super) fn capture(
    connection: &Connection,
    agent: &str,
    tables: Option<&BTreeSet<String>>,
) -> Result<TaskQueryInventory> {
    ensure_schema(connection)?;
    let id = crate::ids::runtime_id("task_queries");
    let task_observed = root_table_available(tables, "tasks");
    let message_observed = root_table_available(tables, "messages");
    let result_observed = root_table_available(tables, "task_result_settlements");
    connection.execute("INSERT INTO task_query_fixed_manifests(inventory_id,agent_id,task_observed,message_observed,result_observed) VALUES(?1,?2,?3,?4,?5)",params![id,agent,task_observed,message_observed,result_observed])?;
    if task_observed {
        let mut q=connection.prepare("SELECT length(CAST(task_id AS BLOB))+length(CAST(payload_json AS BLOB)),task_id FROM tasks WHERE owner_agent_id=?1 ORDER BY task_id")?;
        let mut rows = q.query([agent])?;
        while let Some(row) = rows.next()? {
            material(row.get(0)?)?;
            let task_id: String = row.get(1)?;
            capture_task(connection, &id, agent, &task_id, None)?;
        }
    }
    if result_observed {
        let mut query=connection.prepare("SELECT length(CAST(result_identity AS BLOB))+length(CAST(payload_json AS BLOB)),result_identity FROM task_result_settlements WHERE agent_id=?1 ORDER BY result_identity")?;
        let mut rows = query.query([agent])?;
        while let Some(row) = rows.next()? {
            material(row.get(0)?)?;
            let result_id: String = row.get(1)?;
            let key = TaskKey::new(TaskFamily::ResultSettlement, &result_id);
            material(
                super::super::super::task_snapshot::checkpoint_material_size_on(connection, &key)?
                    .context("fixed query Result missing")?,
            )?;
            let (fact, raw, projection) =
                super::super::super::task_snapshot::checkpoint_fact_on(connection, agent, &key)?
                    .context("fixed query Result missing")?;
            let CanonicalTaskFact::ResultSettlement(result) = fact else {
                unreachable!()
            };
            connection.execute("INSERT INTO task_query_fixed_results(inventory_id,result_identity,task_id,content_digest,projection_digest) VALUES(?1,?2,?3,?4,?5)",params![id,result_id,result.task_id,raw,projection])?;
        }
    }
    if message_observed {
        let mut q=connection.prepare("SELECT length(CAST(evidence_id AS BLOB))+length(CAST(payload_json AS BLOB)),evidence_id FROM messages WHERE agent_id=?1 ORDER BY evidence_id")?;
        let mut rows = q.query([agent])?;
        while let Some(row) = rows.next()? {
            material(row.get(0)?)?;
            let source: String = row.get(1)?;
            capture_message(connection, &id, agent, &source, None)?;
        }
    }
    super::super::super::input_evidence_snapshot::capture_fixed_task_output_inputs_on(
        connection,
        &FixedOutputInputCapture {
            connection,
            inventory: &id,
            agent,
            tables,
        },
    )?;
    seal(
        connection,
        &id,
        agent,
        task_observed,
        message_observed,
        result_observed,
    )
}

fn material(bytes: usize) -> Result<()> {
    if !bytes
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .is_some_and(|n| n <= CONTINUITY_MAX_PAYLOAD_BYTES)
    {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "fixed Task query row material",
        }
        .into());
    }
    Ok(())
}
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}
fn capture_task(
    connection: &Connection,
    inventory: &str,
    agent: &str,
    id: &str,
    expected: Option<(&str, &str)>,
) -> Result<()> {
    let key = TaskKey::new(TaskFamily::Task, id);
    material(
        super::super::super::task_snapshot::checkpoint_material_size_on(connection, &key)?
            .context("fixed query Task missing")?,
    )?;
    native_task_material(connection, id)?;
    let (fact, raw, projection) =
        super::super::super::task_snapshot::checkpoint_fact_on(connection, agent, &key)?
            .context("fixed query Task missing")?;
    if let Some(expected) = expected {
        ensure!(
            expected == (raw.as_str(), projection.as_str()),
            "Task query changed after original import"
        );
    }
    let CanonicalTaskFact::Task(task) = fact else {
        unreachable!()
    };
    let reply = if task.kind == TaskKind::AgentMessageWait && task.status == TaskStatus::Completed {
        task.detail
            .as_ref()
            .and_then(|detail| detail.get("message_id"))
            .and_then(serde_json::Value::as_str)
    } else {
        None
    };
    let generation = crate::runtime::resident_task::task_running_generation_digest(&task)?;
    connection.execute("INSERT INTO task_query_fixed_tasks(inventory_id,task_id,content_digest,projection_digest,reply_message_id,created_at,generation_created_at,generation_digest,parent_turn_id,task_status,header_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(inventory_id,task_id) DO UPDATE SET content_digest=excluded.content_digest,projection_digest=excluded.projection_digest,reply_message_id=excluded.reply_message_id,created_at=excluded.created_at,generation_created_at=excluded.generation_created_at,generation_digest=excluded.generation_digest,parent_turn_id=excluded.parent_turn_id,task_status=excluded.task_status,header_digest=excluded.header_digest",params![inventory,id,raw,projection,reply,super::super::super::repositories::timestamp(task.created_at),task.created_at.to_rfc3339(),generation,task.detail.as_ref().and_then(|d|d.get("parent_turn_id")).and_then(serde_json::Value::as_str),serde_json::to_value(&task.status)?.as_str().context("Task status encoding differs")?,super::super::super::task_snapshot::monitor_relations::task_header_digest(&task,&raw,&projection)?])?;
    Ok(())
}
fn capture_message(
    connection: &Connection,
    inventory: &str,
    agent: &str,
    id: &str,
    expected: Option<&str>,
) -> Result<()> {
    let source_bytes:usize=connection.query_row("SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE agent_id=?1 AND evidence_id=?2",params![agent,id],|row|row.get(0))?;
    material(source_bytes)?;
    let native: usize = connection.query_row("SELECT COUNT(*) FROM messages m,json_tree(m.payload_json) WHERE m.agent_id=?1 AND m.evidence_id=?2",params![agent,id],|row|row.get(0))?;
    native_material(native)?;
    let (message, digest, fingerprint) =
        super::super::super::input_evidence_snapshot::fixed_task_query_message_on(
            connection, agent, id,
        )?;
    if let Some(expected) = expected {
        ensure!(
            expected == digest,
            "Message query changed after original import"
        );
    }
    let task = if matches!(message.origin, MessageOrigin::Task { .. }) {
        message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("task_id"))
            .and_then(serde_json::Value::as_str)
    } else {
        None
    };
    let association = fixed_running_association(connection, inventory, agent, &message)?;
    let status = crate::runtime::resident_task::task_output_status_from_message(
        &message,
        message
            .metadata
            .as_ref()
            .unwrap_or(&serde_json::Value::Null),
    );
    let status = serde_json::to_value(status)?;
    connection.execute("INSERT INTO task_query_fixed_messages(inventory_id,evidence_id,message_id,message_seq,created_at,source_task_id,is_result,content_digest,fingerprint_json,message_status,running_created_at,running_generation_digest) SELECT ?1,m.evidence_id,m.message_id,m.message_seq,m.created_at,?2,?3,?4,?5,?8,?9,?10 FROM messages m WHERE m.agent_id=?6 AND m.evidence_id=?7 ON CONFLICT(inventory_id,evidence_id) DO UPDATE SET message_id=excluded.message_id,message_seq=excluded.message_seq,created_at=excluded.created_at,source_task_id=excluded.source_task_id,is_result=excluded.is_result,content_digest=excluded.content_digest,fingerprint_json=excluded.fingerprint_json,message_status=excluded.message_status,running_created_at=CASE WHEN task_query_fixed_messages.content_digest=excluded.content_digest THEN task_query_fixed_messages.running_created_at ELSE NULL END,running_generation_digest=CASE WHEN task_query_fixed_messages.content_digest=excluded.content_digest THEN task_query_fixed_messages.running_generation_digest ELSE NULL END",params![inventory,task,message.kind==MessageKind::TaskResult,digest,fingerprint,agent,id,status.as_str().context("Task Source status is not a string")?,association.as_ref().map(|value|value.0.as_str()),association.as_ref().map(|value|value.1.as_str())])?;
    Ok(())
}
fn native_material(nodes: usize) -> Result<()> {
    ensure!(
        nodes
            .checked_mul(8)
            .is_some_and(|count| count <= CONTINUITY_MAX_RECORDS),
        EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "fixed Running native nodes"
        }
    );
    Ok(())
}
fn native_task_material(connection: &Connection, task: &str) -> Result<()> {
    let nodes: usize = connection.query_row(
        "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
        [task],
        |row| row.get(0),
    )?;
    native_material(nodes)
}
fn fixed_running_association(
    connection: &Connection,
    inventory: &str,
    agent: &str,
    source: &crate::types::MessageEnvelope,
) -> Result<Option<(String, String)>> {
    if source.kind != MessageKind::TaskStatus {
        return Ok(None);
    }
    let queue_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='queue_entries')",
        [],
        |row| row.get(0),
    )?;
    if !queue_table {
        return Ok(None);
    }
    let pending: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND message_id=?2 AND status IN ('queued','interrupted','dequeued','interjected'))",params![agent,source.id],|row|row.get(0))?;
    if !pending {
        return Ok(None);
    }
    let Some(id) = source.task_id.as_deref() else {
        return Ok(None);
    };
    let member: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM task_query_fixed_tasks f JOIN tasks t ON t.task_id=f.task_id WHERE f.inventory_id=?1 AND f.task_id=?2 AND t.owner_agent_id=?3)",params![inventory,id,agent],|row|row.get(0))?;
    if !member {
        return Ok(None);
    }
    let key = TaskKey::new(TaskFamily::Task, id);
    let Some(size) =
        super::super::super::task_snapshot::checkpoint_material_size_on(connection, &key)?
    else {
        return Ok(None);
    };
    let source_bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
        source,
    )?)?;
    material(
        size.checked_add(source_bytes)
            .context("fixed Running joint material overflow")?,
    )?;
    let nodes:usize=connection.query_row("SELECT (SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1)+(SELECT COUNT(*) FROM messages m,json_tree(m.payload_json) WHERE m.agent_id=?2 AND m.evidence_id=?3)",params![id,agent,source.id],|row|row.get(0))?;
    native_material(nodes)?;
    let Some((CanonicalTaskFact::Task(task), raw, projection)) =
        super::super::super::task_snapshot::checkpoint_fact_on(connection, agent, &key)?
    else {
        return Ok(None);
    };
    let frozen: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM task_query_fixed_tasks WHERE inventory_id=?1 AND task_id=?2 AND content_digest=?3 AND projection_digest=?4)",params![inventory,id,raw,projection],|row|row.get(0))?;
    if !frozen || !crate::runtime::resident_task::fixed_running_matches_task(&task, source)? {
        return Ok(None);
    }
    Ok(Some((
        task.created_at.to_rfc3339(),
        crate::runtime::resident_task::task_running_generation_digest(&task)?,
    )))
}

fn seal(
    connection: &Connection,
    id: &str,
    agent: &str,
    task_observed: bool,
    message_observed: bool,
    result_observed: bool,
) -> Result<TaskQueryInventory> {
    let task_count: u64 = connection.query_row(
        "SELECT COUNT(*) FROM task_query_fixed_tasks WHERE inventory_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let message_count: u64 = connection.query_row(
        "SELECT COUNT(*) FROM task_query_fixed_messages WHERE inventory_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let result_count: u64 = connection.query_row(
        "SELECT COUNT(*) FROM task_query_fixed_results WHERE inventory_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let mut hash = Sha256::new();
    for sql in ["SELECT length(CAST(json_array(result_identity,task_id,content_digest,projection_digest) AS BLOB)),json_array(result_identity,task_id,content_digest,projection_digest) FROM task_query_fixed_results WHERE inventory_id=?1 ORDER BY result_identity", "SELECT length(CAST(json_array(task_id,content_digest,projection_digest,reply_message_id,created_at,generation_created_at,generation_digest,parent_turn_id,task_status,header_digest) AS BLOB)),json_array(task_id,content_digest,projection_digest,reply_message_id,created_at,generation_created_at,generation_digest,parent_turn_id,task_status,header_digest) FROM task_query_fixed_tasks WHERE inventory_id=?1 ORDER BY task_id", "SELECT length(CAST(json_array(evidence_id,message_id,message_seq,created_at,source_task_id,is_result,content_digest,fingerprint_json,message_status,running_created_at,running_generation_digest) AS BLOB)),json_array(evidence_id,message_id,message_seq,created_at,source_task_id,is_result,content_digest,fingerprint_json,message_status,running_created_at,running_generation_digest) FROM task_query_fixed_messages WHERE inventory_id=?1 ORDER BY evidence_id"] {
        let mut q=connection.prepare(sql)?;let mut rows=q.query([id])?;
        while let Some(row)=rows.next()? {let size:usize=row.get(0)?;material(size)?;let value:String=row.get(1)?;ensure!(value.len()==size,"fixed query seal length differs");hash.update(u64::try_from(size)?.to_le_bytes());hash.update(value.as_bytes());}
    }
    let mut query=connection.prepare("SELECT length(CAST(json_array(family,record_id,related_turn_id,fingerprint_json) AS BLOB)),json_array(family,record_id,related_turn_id,fingerprint_json) FROM task_query_fixed_input_members WHERE inventory_id=?1 ORDER BY family,record_id")?;
    let mut rows = query.query([id])?;
    while let Some(row) = rows.next()? {
        let size: usize = row.get(0)?;
        material(size)?;
        let value: String = row.get(1)?;
        ensure!(
            value.len() == size,
            "fixed Output Input seal length differs"
        );
        hash.update(u64::try_from(size)?.to_le_bytes());
        hash.update(value.as_bytes());
    }
    let metadata_digest = format!("{:x}", hash.finalize());
    connection.execute("UPDATE task_query_fixed_manifests SET task_count=?1,message_count=?2,metadata_digest=?3,result_count=?5,sealed=1 WHERE inventory_id=?4 AND sealed=0",params![task_count,message_count,metadata_digest,id,result_count])?;
    let out = TaskQueryInventory {
        id: id.into(),
        agent_id: agent.into(),
        task_count,
        message_count,
        result_count,
        task_table_observed: task_observed,
        message_table_observed: message_observed,
        result_table_observed: result_observed,
        metadata_digest,
    };
    validate(connection, &out)?;
    Ok(out)
}

pub(in crate::runtime_db) fn validate(
    connection: &Connection,
    inventory: &TaskQueryInventory,
) -> Result<()> {
    ensure!(
        inventory.id.len() <= 128 && valid_digest(&inventory.metadata_digest),
        "fixed Task query checkpoint invalid"
    );
    let sizes:(usize,usize)=connection.query_row("SELECT length(CAST(agent_id AS BLOB)),length(CAST(metadata_digest AS BLOB)) FROM task_query_fixed_manifests WHERE inventory_id=?1 AND sealed=1",[&inventory.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    ensure!(
        sizes == (inventory.agent_id.len(), 64),
        "fixed Task query manifest shape differs"
    );
    let actual:(String,u64,u64,bool,bool,String,u64,bool)=connection.query_row("SELECT agent_id,task_count,message_count,task_observed,message_observed,metadata_digest,result_count,result_observed FROM task_query_fixed_manifests WHERE inventory_id=?1 AND sealed=1",[&inventory.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)))?;
    ensure!(
        actual
            == (
                inventory.agent_id.clone(),
                inventory.task_count,
                inventory.message_count,
                inventory.task_table_observed,
                inventory.message_table_observed,
                inventory.metadata_digest.clone(),
                inventory.result_count,
                inventory.result_table_observed,
            ),
        "fixed Task query manifest differs from original checkpoint"
    );
    Ok(())
}
pub(in crate::runtime_db) fn fact_digest(
    connection: &Connection,
    inventory: &TaskQueryInventory,
    key: &TaskKey,
) -> Result<Option<(String, String)>> {
    validate(connection, inventory)?;
    let (table, column) = match key.family {
        TaskFamily::Task => ("task_query_fixed_tasks", "task_id"),
        TaskFamily::ResultSettlement => ("task_query_fixed_results", "result_identity"),
    };
    let lengths:Option<(usize,usize)>=connection.query_row(&format!("SELECT length(CAST(content_digest AS BLOB)),length(CAST(projection_digest AS BLOB)) FROM {table} WHERE inventory_id=?1 AND {column}=?2"),params![inventory.id,key.id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some(lengths) = lengths else {
        return Ok(None);
    };
    ensure!(
        lengths == (64, 64),
        "fixed Task query digest length changed"
    );
    let pair:(String,String)=connection.query_row(&format!("SELECT content_digest,projection_digest FROM {table} WHERE inventory_id=?1 AND {column}=?2"),params![inventory.id,key.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    ensure!(
        valid_digest(&pair.0) && valid_digest(&pair.1),
        "fixed Task query digest invalid"
    );
    Ok(Some(pair))
}
