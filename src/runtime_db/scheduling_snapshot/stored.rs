use super::*;
use serde_json::{json, Value};

pub(super) fn source(
    f: SchedulingFamily,
) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    match f {
        SchedulingFamily::Timer => (
            "timers c",
            "c.timer_id",
            "c.agent_id",
            "c.status IN ('active','scheduled')",
            "c.updated_at",
        ),
        SchedulingFamily::TimerWake => (
            "timer_wakes c JOIN timers t ON t.timer_id=c.timer_id",
            "json_array(c.timer_id,c.message_id)",
            "t.agent_id",
            "c.status='pending'",
            "c.updated_at",
        ),
        SchedulingFamily::ExternalTrigger => (
            "external_triggers c",
            "c.external_trigger_id",
            "c.target_agent_id",
            "c.status='active'",
            "c.created_at",
        ),
    }
}
pub(super) fn table(f: SchedulingFamily) -> &'static str {
    match f {
        SchedulingFamily::Timer => "timers",
        SchedulingFamily::TimerWake => "timer_wakes",
        SchedulingFamily::ExternalTrigger => "external_triggers",
    }
}
pub(super) fn projection(f: SchedulingFamily) -> &'static str {
    match f {
 SchedulingFamily::Timer=>"json_object('timer_id',c.timer_id,'agent_id',c.agent_id,'status',c.status,'summary',c.summary,'created_at',c.created_at,'duration_ms',c.duration_ms,'interval_ms',c.interval_ms,'repeat',c.repeat,'next_fire_at',c.next_fire_at,'last_fired_at',c.last_fired_at,'fire_count',c.fire_count,'updated_at',c.updated_at)",
 SchedulingFamily::TimerWake=>"json_object('agent_id',t.agent_id,'timer_id',c.timer_id,'message_id',c.message_id,'fire_count',c.fire_count,'status',c.status,'created_at',c.created_at,'updated_at',c.updated_at,'incorporated_at',c.incorporated_at,'cancelled_at',c.cancelled_at)",
 SchedulingFamily::ExternalTrigger=>"json_object('external_trigger_id',c.external_trigger_id,'target_agent_id',c.target_agent_id,'trigger_url',c.trigger_url,'token_hash',c.token_hash,'status',c.status,'created_at',c.created_at,'revoked_at',c.revoked_at,'last_delivered_at',c.last_delivered_at,'delivery_count',c.delivery_count)",
 }
}
// Every normalized scalar is admitted before SQLite constructs its JSON object.
fn normalized_columns(family: SchedulingFamily) -> &'static [&'static str] {
    match family {
        SchedulingFamily::Timer => &[
            "c.timer_id",
            "c.agent_id",
            "c.status",
            "c.summary",
            "c.created_at",
            "c.duration_ms",
            "c.interval_ms",
            "c.repeat",
            "c.next_fire_at",
            "c.last_fired_at",
            "c.fire_count",
            "c.updated_at",
        ],
        SchedulingFamily::TimerWake => &[
            "t.agent_id",
            "c.timer_id",
            "c.message_id",
            "c.fire_count",
            "c.status",
            "c.created_at",
            "c.updated_at",
            "c.incorporated_at",
            "c.cancelled_at",
        ],
        SchedulingFamily::ExternalTrigger => &[
            "c.external_trigger_id",
            "c.target_agent_id",
            "c.trigger_url",
            "c.token_hash",
            "c.status",
            "c.created_at",
            "c.revoked_at",
            "c.last_delivered_at",
            "c.delivery_count",
        ],
    }
}
fn scalar_length(column: &str) -> String {
    format!("COALESCE(length(CAST({column} AS BLOB)),0)")
}
#[derive(Debug)]
pub(super) struct StoredScheduling {
    pub fact: CanonicalSchedulingFact,
    pub content: String,
    pub projection: String,
    pub status: String,
    pub sort_at: String,
    pub created_at: String,
}
impl StoredScheduling {
    pub(super) fn qualified(
        &self,
        scope: &InputEvidenceScope,
        revision: u64,
    ) -> Result<Arc<QualifiedSchedulingFact>> {
        let locator = SchedulingLocator {
            scope: scope.clone(),
            key: self.fact.key(),
            revision,
            content_digest: self.content.clone(),
            projection_digest: self.projection.clone(),
        };
        let resident_bytes = usize::try_from(
            serialized_bytes(&(&locator, &self.fact))?
                .checked_add(128)
                .context("Scheduling body estimate overflow")?,
        )?;
        let qualified = Arc::new(QualifiedSchedulingFact {
            locator,
            fact: self.fact.clone(),
            resident_bytes,
        });
        qualified.validate(scope)?;
        Ok(qualified)
    }
}
pub(super) fn read_stored(
    connection: &Connection,
    key: &SchedulingKey,
    agent: &str,
    material: Option<&mut Material<'_, '_>>,
) -> Result<Option<StoredScheduling>> {
    let (from, id, owner, _, sort) = source(key.family);
    let projection = projection(key.family);
    let raw = if key.family == SchedulingFamily::TimerWake {
        projection
    } else {
        "c.payload_json"
    };
    // Wake lookup uses the original two columns; it must not serialize every
    // candidate row's address while locating the selected tuple.
    let pair = if key.family == SchedulingFamily::TimerWake {
        Some(key.wake_pair()?)
    } else {
        None
    };
    let (predicate, id_value, message_value) = match &pair {
        Some((timer, message)) => (
            "c.timer_id=?1 AND c.message_id=?2",
            timer.as_str(),
            Some(message.as_str()),
        ),
        None => (
            if key.family == SchedulingFamily::Timer {
                "c.timer_id=?1"
            } else {
                "c.external_trigger_id=?1"
            },
            key.id.as_str(),
            None,
        ),
    };
    let columns = normalized_columns(key.family);
    let scalar_bytes = columns
        .iter()
        .map(|column| scalar_length(column))
        .collect::<Vec<_>>()
        .join("+");
    // JSON escaping is at most six bytes per scalar byte. The finite 512-byte
    // allowance covers fixed keys, punctuation, nulls and number formatting.
    let projected_bound = format!("(6*({scalar_bytes})+512)");
    let raw_bound = if pair.is_some() {
        projected_bound.clone()
    } else {
        scalar_length("c.payload_json")
    };
    let id_bound = if pair.is_some() {
        format!(
            "(6*({}+{})+16)",
            scalar_length("c.timer_id"),
            scalar_length("c.message_id")
        )
    } else {
        scalar_length(id)
    };
    let sql = format!("SELECT {owner} IS ?3,{raw_bound}+{projected_bound}+{}+{id_bound}+{}+{}+128 FROM {from} WHERE {predicate}", scalar_length(owner), scalar_length(sort), scalar_length("c.created_at"));
    let Some((same_owner, bytes)) = connection
        .query_row(&sql, params![id_value, message_value, agent], |row| {
            Ok((row.get::<_, bool>(0)?, row.get::<_, usize>(1)?))
        })
        .optional()?
    else {
        return Ok(None);
    };
    ensure!(
        same_owner,
        SchedulingSnapshotError::ForeignRecord(key.clone())
    );
    let native_nodes = columns
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(1))
        .context("Scheduling normalized native count overflow")?;
    if let Some(material) = material {
        material.charge(bytes, native_nodes)?;
    } else {
        ensure!(
            bytes.checked_mul(8).is_some_and(|n| n <= MAX_BYTES),
            SchedulingSnapshotError::ResourceExhausted("fixed single record")
        );
    }
    let mut statement = connection.prepare(&format!(
        "SELECT {owner},{raw},{projection},c.status,{sort},c.created_at FROM {from} WHERE {predicate} AND {owner} IS ?3"
    ))?;
    let mut rows = statement.query(params![id_value, message_value, agent])?;
    let row = rows
        .next()?
        .context("Scheduling row disappeared in same view")?;
    let actual_owner: String = row.get(0)?;
    ensure!(
        actual_owner == agent,
        SchedulingSnapshotError::ForeignRecord(key.clone())
    );
    let raw: String = row.get(1)?;
    let projected: String = row.get(2)?;
    let status: String = row.get(3)?;
    let sort_at: String = row.get(4)?;
    let created_at: String = row.get(5)?;
    let actual_bytes = raw
        .len()
        .checked_add(projected.len())
        .and_then(|n| n.checked_add(actual_owner.len()))
        .and_then(|n| n.checked_add(key.id.len()))
        .and_then(|n| n.checked_add(sort_at.len()))
        .and_then(|n| n.checked_add(created_at.len()))
        .and_then(|n| n.checked_add(128))
        .context("Scheduling stored bytes overflow")?;
    ensure!(actual_bytes <= bytes, "Scheduling raw scalar bound differs");
    let fact = match key.family {
        SchedulingFamily::Timer => {
            CanonicalSchedulingFact::Timer(repositories::decode_timer_payload(&raw)?)
        }
        SchedulingFamily::TimerWake => {
            CanonicalSchedulingFact::TimerWake(serde_json::from_str(&raw)?)
        }
        SchedulingFamily::ExternalTrigger => CanonicalSchedulingFact::ExternalTrigger(
            repositories::decode_external_trigger_payload(&raw)?,
        ),
    };
    ensure!(
        fact.agent_id() == agent && fact.key() == *key,
        "Scheduling canonical ID/owner mismatch"
    );
    if let Some(expected) = expected_projection(&fact)? {
        let mut normalized: Value = serde_json::from_str(&projected)?;
        if key.family == SchedulingFamily::Timer && status == "scheduled" {
            normalized["status"] = Value::String("active".into());
        }
        ensure!(
            normalized == expected,
            "Scheduling normalized columns/payload differ"
        );
    }
    Ok(Some(StoredScheduling {
        fact,
        content: digest(raw.as_bytes()),
        projection: digest(projected.as_bytes()),
        status,
        sort_at,
        created_at,
    }))
}
fn expected_projection(fact: &CanonicalSchedulingFact) -> Result<Option<Value>> {
    Ok(match fact {
        CanonicalSchedulingFact::Timer(x) => Some(
            json!({"timer_id":x.id,"agent_id":x.agent_id,"status":x.status,"summary":x.summary,"created_at":repositories::timestamp(x.created_at),"duration_ms":x.duration_ms,"interval_ms":x.interval_ms,"repeat":i64::from(x.repeat),"next_fire_at":x.next_fire_at.map(repositories::timestamp),"last_fired_at":x.last_fired_at.map(repositories::timestamp),"fire_count":x.fire_count,"updated_at":repositories::timestamp(repositories::timer_updated_at(x))}),
        ),
        CanonicalSchedulingFact::ExternalTrigger(x) => Some(
            json!({"external_trigger_id":x.external_trigger_id,"target_agent_id":x.target_agent_id,"trigger_url":x.token,"token_hash":x.token_hash,"status":x.status,"created_at":repositories::timestamp(x.created_at),"revoked_at":x.revoked_at.map(repositories::timestamp),"last_delivered_at":x.last_delivered_at.map(repositories::timestamp),"delivery_count":x.delivery_count}),
        ),
        // Wake has no independent JSON payload: its entire body is this normalized tuple.
        CanonicalSchedulingFact::TimerWake(_) => None,
    })
}
/// Inspect only the selected normalized owner relation, without acquiring a body.
/// This negative observation grants no membership, absence, or other scope access.
pub(super) fn has_foreign_owner(
    connection: &Connection,
    key: &SchedulingKey,
    agent: &str,
) -> Result<bool> {
    if key.family == SchedulingFamily::TimerWake {
        let pair = key.wake_pair()?;
        return Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM timer_wakes c JOIN timers t ON t.timer_id=c.timer_id WHERE c.timer_id=?1 AND c.message_id=?2 AND t.agent_id IS NOT ?3)",
            params![pair.0, pair.1, agent], |row| row.get(0),
        )?);
    }
    let (from, id, owner, _, _) = source(key.family);
    Ok(connection.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {from} WHERE {id}=?1 AND {owner} IS NOT ?2)"),
        params![key.id, agent],
        |row| row.get(0),
    )?)
}

pub(super) fn exists_global(connection: &Connection, key: &SchedulingKey) -> Result<bool> {
    if key.family == SchedulingFamily::TimerWake {
        let pair = key.wake_pair()?;
        return Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM timer_wakes WHERE timer_id=?1 AND message_id=?2)",
            params![pair.0, pair.1],
            |r| r.get(0),
        )?);
    }
    let (from, id, _, _, _) = source(key.family);
    Ok(connection.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {from} WHERE {id}=?1)"),
        [&key.id],
        |r| r.get(0),
    )?)
}
