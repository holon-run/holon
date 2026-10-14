//! Original accepted heads select history; canonical rows only validate it.
use super::*;

pub(super) fn live_header_predicate(family: SchedulingFamily) -> &'static str {
    match family {
        SchedulingFamily::Timer => "h.status IN ('active','scheduled')",
        SchedulingFamily::TimerWake => "h.status='pending'",
        SchedulingFamily::ExternalTrigger => "h.status='active'",
    }
}

pub(super) fn accepted_heads(
    family: SchedulingFamily,
    manifests: &[SchedulingContinuityCheckpoint],
) -> String {
    let inventories = manifests
        .iter()
        .map(|m| format!("'{}'", m.inventory_id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let inventories = if inventories.is_empty() {
        "NULL"
    } else {
        &inventories
    };
    let ranks = manifests
        .iter()
        .enumerate()
        .map(|(i, m)| format!("WHEN '{}' THEN {i}", m.inventory_id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" ");
    let ranks = if ranks.is_empty() {
        "WHEN '' THEN -1"
    } else {
        &ranks
    };
    let rank = |alias: &str| format!("CASE {alias}.inventory_id {ranks} ELSE -1 END");
    let current=format!("r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family='{}' AND r.availability!='absent'",family_name(family));
    // Match reverse-manifest precedence. Original Absent never adopts a later
    // body: materialization retains the existing alias and fixed-digest gates.
    format!("SELECT r.record_id,r.status,r.sort_at,r.created_at,r.availability
        FROM scheduling_domain_records r WHERE {current}
        UNION ALL SELECT f.record_id,f.status,f.sort_at,f.created_at,'present'
        FROM scheduling_fixed_members f WHERE f.family='{}' AND f.inventory_id IN({inventories})
        AND NOT EXISTS(SELECT 1 FROM scheduling_domain_records r WHERE {current} AND r.record_id=f.record_id)
        AND NOT EXISTS(SELECT 1 FROM scheduling_fixed_members newer WHERE newer.family=f.family
         AND newer.record_id=f.record_id AND newer.inventory_id IN({inventories}) AND {}>{})",
        family_name(family),rank("newer"),rank("f"))
}

pub(super) fn loaded_history_ids(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    family: SchedulingFamily,
    limit: usize,
    manifests: &[SchedulingContinuityCheckpoint],
    material: &mut Material<'_, '_>,
) -> Result<Option<Vec<SchedulingKey>>> {
    ensure!(
        family != SchedulingFamily::TimerWake && limit > 0,
        "invalid loaded history selection"
    );
    let heads = accepted_heads(family, manifests);
    let predicate = live_header_predicate(family);
    let qualified = format!("FROM ({heads}) h WHERE NOT ({predicate})");
    // Signed/wide UTC years need numeric order. The suffix contains fixed-width
    // month-through-millisecond fields. No payload JSON runs in this query.
    let year = "CAST(h.created_at AS INTEGER)";
    let rest = "substr(h.created_at,instr(substr(h.created_at,2),'-')+2)";
    let order = format!(
        "ORDER BY {year} DESC,{rest} DESC,h.record_id ASC LIMIT 1 OFFSET {}",
        limit - 1
    );
    let boundary = format!(
        "SELECT h.* {qualified} AND ({year},{rest}) >=
        (COALESCE((SELECT {year} {qualified} {order}),-9223372036854775808),
         COALESCE((SELECT {rest} {qualified} {order}),''))"
    );
    let (from, id, owner, _, _) = stored::source(family);
    let projection = stored::projection(family);
    let columns: &[&str] = match family {
        SchedulingFamily::Timer => &[
            "timer_id",
            "agent_id",
            "status",
            "summary",
            "created_at",
            "duration_ms",
            "interval_ms",
            "repeat",
            "next_fire_at",
            "last_fired_at",
            "fire_count",
            "updated_at",
        ],
        SchedulingFamily::ExternalTrigger => &[
            "external_trigger_id",
            "target_agent_id",
            "trigger_url",
            "token_hash",
            "status",
            "created_at",
            "revoked_at",
            "last_delivered_at",
            "delivery_count",
        ],
        SchedulingFamily::TimerWake => unreachable!(),
    };
    let projection_bound = columns
        .iter()
        .map(|column| format!("COALESCE(length(CAST(c.{column} AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let projection_bound =
        format!("CASE WHEN h.availability='pruned' THEN 0 ELSE (({projection_bound})*6+1024) END");
    let candidates = format!(
        "SELECT h.record_id,h.created_at AS accepted_created,h.availability,
        CASE WHEN h.availability!='pruned' THEN {owner} END AS actual_owner,
        CASE WHEN h.availability!='pruned' THEN c.created_at END AS actual_created,
        CASE WHEN h.availability!='pruned' THEN c.payload_json END AS raw,
         {projection_bound} AS projection_bound
        FROM ({boundary}) h LEFT JOIN {from} ON {id}=h.record_id"
    );
    let (count,bytes):(usize,usize)=tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(
        length(CAST(record_id AS BLOB))+COALESCE(length(CAST(accepted_created AS BLOB)),0)
        +length(CAST(availability AS BLOB))+COALESCE(length(CAST(actual_owner AS BLOB)),0)
        +COALESCE(length(CAST(actual_created AS BLOB)),0)+COALESCE(length(CAST(raw AS BLOB)),0)+projection_bound+256),0)
        FROM ({candidates})"),params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    // Full raw length/slots precede every String and native JSON extraction.
    // An oversized cutoff bucket is a typed refusal, never a truncated top K.
    material.charge(bytes, count)?;
    let mut headers=tx.prepare(&format!("SELECT record_id,availability,actual_owner,accepted_created,actual_created FROM ({candidates})"))?;
    let mut rows = headers.query(params![scope.agent_id, scope.identity_incarnation])?;
    let mut pruned = false;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let key = SchedulingKey::new(family, id);
        let availability: String = row.get(1)?;
        if availability == "pruned" {
            pruned = true;
            continue;
        }
        let owner: Option<String> = row.get(2)?;
        ensure!(
            owner.is_some(),
            SchedulingSnapshotError::CanonicalChanged(key.clone())
        );
        ensure!(
            owner.as_deref() == Some(scope.agent_id.as_str()),
            SchedulingSnapshotError::ForeignRecord(key.clone())
        );
        let accepted: Option<String> = row.get(3)?;
        let actual: Option<String> = row.get(4)?;
        ensure!(
            accepted.is_some() && accepted == actual,
            SchedulingSnapshotError::CanonicalChanged(key)
        );
    }
    drop(rows);
    drop(headers);
    // Verify every expanded candidate before its payload time participates in
    // ordering. A corrupted time cannot fall below K and evade digest checking.
    let validated = format!(
        "SELECT h.record_id,c.payload_json,{projection},c.status,{},c.created_at
        FROM ({boundary}) h LEFT JOIN {from} ON {id}=h.record_id
        WHERE h.availability!='pruned'",
        stored::source(family).4
    );
    let mut query = tx.prepare(&validated)?;
    let mut rows = query.query(params![scope.agent_id, scope.identity_incarnation])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let raw: String = row.get(1)?;
        let projection: String = row.get(2)?;
        let status: String = row.get(3)?;
        let sort: String = row.get(4)?;
        let created: String = row.get(5)?;
        loader::verify_member_version(
            tx,
            scope,
            &SchedulingKey::new(family, id),
            (
                &digest(raw.as_bytes()),
                &digest(projection.as_bytes()),
                &status,
                &sort,
                &created,
            ),
            manifests,
            material,
        )?;
    }
    drop(rows);
    drop(query);
    // A removed body cannot supply its original nanoseconds. Keep accepted
    // cold heads; the caller may reuse only its exact previously observed order.
    if pruned {
        return Ok(None);
    }
    let mut query = tx.prepare(&format!(
        "SELECT record_id,accepted_created,json_extract(raw,'$.created_at') FROM ({candidates})"
    ))?;
    let mut rows = query.query(params![scope.agent_id, scope.identity_incarnation])?;
    let mut selected = Vec::with_capacity(count);
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let normalized: String = row.get(1)?;
        let created: String = row.get(2)?;
        let created = chrono::DateTime::parse_from_rfc3339(&created)?.with_timezone(&chrono::Utc);
        ensure!(
            repositories::timestamp(created) == normalized,
            SchedulingSnapshotError::CanonicalChanged(SchedulingKey::new(family, id.clone()))
        );
        selected.push((created, id));
    }
    selected.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    selected.truncate(limit);
    Ok(Some(
        selected
            .into_iter()
            .map(|(_, id)| SchedulingKey::new(family, id))
            .collect(),
    ))
}
