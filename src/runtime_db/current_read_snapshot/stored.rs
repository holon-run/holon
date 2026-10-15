use super::*;
#[derive(Debug)]
pub(super) struct Stored {
    pub fact: CurrentReadFact,
    pub content: String,
    pub projection: String,
    pub status: String,
    pub seq: u64,
    pub sort_at: String,
    pub created_at: String,
    pub bytes: usize,
    pub nodes: usize,
}
pub(super) fn source(
    f: CurrentReadFamily,
) -> (&'static str, &'static str, &'static str, &'static str) {
    match f {
        CurrentReadFamily::Queue => (
            "queue_entries",
            "message_id",
            "payload_json",
            "json_array(message_id,agent_id,priority,status,created_at,updated_at)",
        ),
        CurrentReadFamily::Event => (
            "audit_events",
            "audit_event_id",
            "data_json",
            "json_array(audit_event_id,agent_id,event_seq,kind,created_at)",
        ),
    }
}
/// Raw normalized cells are measured before SQLite constructs JSON projection.
fn raw_size_expression(family: CurrentReadFamily) -> &'static str {
    match family {
        CurrentReadFamily::Queue => "length(CAST(payload_json AS BLOB))+length(CAST(message_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(priority AS BLOB))+length(CAST(status AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+512",
        CurrentReadFamily::Event => "length(CAST(data_json AS BLOB))+length(CAST(audit_event_id AS BLOB))+COALESCE(length(CAST(agent_id AS BLOB)),0)+length(CAST(event_seq AS BLOB))+length(CAST(kind AS BLOB))+length(CAST(created_at AS BLOB))+512",
    }
}
pub(super) fn read(
    connection: &Connection,
    key: &CurrentReadKey,
    agent: &str,
    material: &mut Material<'_>,
) -> Result<Option<Stored>> {
    let (table, id, raw, projection) = source(key.family);
    let scalar: Option<usize> = connection
        .query_row(
            &format!(
                "SELECT {} FROM {table} WHERE {id}=?1",
                raw_size_expression(key.family)
            ),
            [&key.id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(scalar) = scalar else {
        return Ok(None);
    };
    // Covers native projection construction before the original encoded sizing.
    material.charge(scalar, 1)?;
    let size: usize = connection.query_row(&format!("SELECT length(CAST({raw} AS BLOB))+length(CAST({projection} AS BLOB))+512 FROM {table} WHERE {id}=?1"), [&key.id], |r|r.get(0))?;
    material.charge(size, 1)?;
    let matches_owner: bool = connection.query_row(
        &format!("SELECT agent_id=?2 FROM {table} WHERE {id}=?1"),
        params![key.id, agent],
        |r| r.get(0),
    )?;
    ensure!(matches_owner, CurrentReadError::CanonicalChanged);
    let nodes = material.native(connection, table, raw, id, &key.id)?;
    let seq = if key.family == CurrentReadFamily::Event {
        "event_seq"
    } else {
        "0"
    };
    let status = if key.family == CurrentReadFamily::Queue {
        "status"
    } else {
        "kind"
    };
    let sort = if key.family == CurrentReadFamily::Queue {
        "updated_at"
    } else {
        "created_at"
    };
    let (owner, raw, projected, status, seq, sort_at, created_at): (
        String, String, String, String, u64, String, String,
    ) = connection.query_row(
        &format!(
            "SELECT agent_id,{raw},{projection},{status},{seq},{sort},created_at FROM {table} WHERE {id}=?1"
        ),
        [&key.id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        },
    )?;
    ensure!(owner == agent, CurrentReadError::CanonicalChanged);
    let fact = match key.family {
        CurrentReadFamily::Queue => {
            CurrentReadFact::Queue(serde_json::from_str::<QueueEntryRecord>(&raw)?)
        }
        CurrentReadFamily::Event => {
            CurrentReadFact::Event(serde_json::from_str::<AuditEvent>(&raw)?)
        }
    };
    ensure!(fact.key() == *key, CurrentReadError::CanonicalChanged);
    let expected = match &fact {
        CurrentReadFact::Queue(q) => {
            ensure!(q.agent_id == agent, CurrentReadError::CanonicalChanged);
            serde_json::json!([
                q.message_id,
                q.agent_id,
                q.priority,
                q.status,
                repositories::timestamp(q.created_at),
                repositories::timestamp(q.updated_at)
            ])
        }
        CurrentReadFact::Event(e) => {
            ensure!(
                e.event_seq > 0 && !e.event_log_epoch.is_empty(),
                CurrentReadError::CanonicalChanged
            );
            serde_json::json!([
                e.id,
                agent,
                e.event_seq,
                e.kind,
                repositories::timestamp(e.created_at)
            ])
        }
    };
    ensure!(
        serde_json::from_str::<serde_json::Value>(&projected)? == expected,
        CurrentReadError::CanonicalChanged
    );
    let bytes = size
        .checked_add(
            nodes
                .checked_mul(128)
                .context("CurrentRead native bytes overflow")?,
        )
        .context("CurrentRead resident bytes overflow")?;
    Ok(Some(Stored {
        fact,
        content: digest(raw.as_bytes()),
        projection: digest(projected.as_bytes()),
        status,
        seq,
        sort_at,
        created_at,
        bytes,
        nodes,
    }))
}
impl Stored {
    pub(super) fn qualified(
        self,
        scope: &InputEvidenceScope,
        revision: u64,
    ) -> Result<Arc<QualifiedCurrentReadFact>> {
        let locator = CurrentReadLocator {
            scope: scope.clone(),
            key: self.fact.key(),
            revision,
            content_digest: self.content,
            projection_digest: self.projection,
        };
        let resident_bytes = self
            .bytes
            .checked_add(usize::try_from(serialized_bytes(&locator)?)?)
            .and_then(|n| n.checked_add(256))
            .context("CurrentRead allocation bytes overflow")?;
        let record = Arc::new(QualifiedCurrentReadFact {
            locator,
            fact: self.fact,
            resident_bytes,
            native_records: self.nodes + 16,
        });
        record.validate(scope)?;
        Ok(record)
    }
}

/// Fixed cutover records identity and fingerprints only. It does not decode or
/// pin every historical Audit Value tree; selected body loading stays bounded.
pub(super) struct FixedFingerprint {
    pub content: String,
    pub projection: String,
    pub status: String,
    pub seq: u64,
    pub sort_at: String,
    pub created_at: String,
}
pub(super) fn fixed_fingerprint(
    tx: &Transaction<'_>,
    key: &CurrentReadKey,
    agent: &str,
) -> Result<FixedFingerprint> {
    let (table, id, raw, projection) = source(key.family);
    let scalar: usize = tx.query_row(
        &format!(
            "SELECT {} FROM {table} WHERE {id}=?1",
            raw_size_expression(key.family)
        ),
        [&key.id],
        |r| r.get(0),
    )?;
    ensure!(
        scalar
            .checked_mul(8)
            .is_some_and(|n| n <= CURRENT_READ_MAX_BYTES),
        CurrentReadError::ResourceExhausted
    );
    let size: usize = tx.query_row(&format!("SELECT length(CAST({raw} AS BLOB))+length(CAST({projection} AS BLOB))+512 FROM {table} WHERE {id}=?1"), [&key.id], |r| r.get(0))?;
    ensure!(
        size.checked_mul(8)
            .is_some_and(|n| n <= CURRENT_READ_MAX_BYTES),
        CurrentReadError::ResourceExhausted
    );
    let same_owner: bool = tx.query_row(
        &format!("SELECT agent_id=?2 FROM {table} WHERE {id}=?1"),
        params![key.id, agent],
        |r| r.get(0),
    )?;
    ensure!(same_owner, CurrentReadError::CanonicalChanged);
    let (status, seq, time) = if key.family == CurrentReadFamily::Queue {
        ("status", "0", "updated_at")
    } else {
        ("kind", "event_seq", "created_at")
    };
    let (raw, projection, status, seq, sort_at, created_at): (
        String,
        String,
        String,
        u64,
        String,
        String,
    ) = tx.query_row(
        &format!(
            "SELECT {raw},{projection},{status},{seq},{time},created_at FROM {table} WHERE {id}=?1"
        ),
        [&key.id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    Ok(FixedFingerprint {
        content: digest(raw.as_bytes()),
        projection: digest(projection.as_bytes()),
        status,
        seq,
        sort_at,
        created_at,
    })
}
