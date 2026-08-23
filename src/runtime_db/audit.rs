use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::{
    runtime_db::{migrations::current_schema_version, RuntimeDb},
    runtime_event::{
        classify_projection_effect, ProjectionEffect, ProjectionEffectClassification,
        UnsupportedProjectionEvent,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeDbAuditCheck {
    ProjectionEffects,
    BriefIntegrity,
    All,
}

impl RuntimeDbAuditCheck {
    fn includes_projection_effects(self) -> bool {
        matches!(self, Self::ProjectionEffects | Self::All)
    }

    fn includes_brief_integrity(self) -> bool {
        matches!(self, Self::BriefIntegrity | Self::All)
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeDbAuditOptions {
    pub check: RuntimeDbAuditCheck,
    pub baseline_through: Option<DateTime<Utc>>,
    pub sample_limit: usize,
}

impl Default for RuntimeDbAuditOptions {
    fn default() -> Self {
        Self {
            check: RuntimeDbAuditCheck::All,
            baseline_through: None,
            sample_limit: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDbAuditReport {
    pub generated_at: DateTime<Utc>,
    pub check: RuntimeDbAuditCheck,
    pub database: RuntimeDbAuditDatabase,
    pub baseline: RuntimeDbAuditBaseline,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_effects: Option<ProjectionEffectsAudit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_integrity: Option<BriefIntegrityAudit>,
}

impl RuntimeDbAuditReport {
    pub fn has_new_violations(&self) -> bool {
        self.projection_effects
            .as_ref()
            .is_some_and(|audit| audit.totals.new_violation > 0)
            || self.brief_integrity.as_ref().is_some_and(|audit| {
                audit.agents.iter().any(|agent| {
                    agent
                        .categories
                        .iter()
                        .any(|category| category.new_violation > 0)
                })
            })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDbAuditDatabase {
    pub path: String,
    pub schema_version: i64,
    pub runtime_id: String,
    pub event_log_epoch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDbAuditBaseline {
    pub boundary_kind: &'static str,
    pub baseline_through: Option<DateTime<Utc>>,
    pub without_boundary_policy: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectionEffectsAudit {
    pub groups: Vec<ProjectionEffectInventoryGroup>,
    pub totals: ViolationSplit,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectionEffectInventoryGroup {
    pub agent_id: String,
    pub kind: String,
    pub contract_version: i64,
    pub payload_schema: String,
    pub payload_schema_version: i64,
    pub classification: &'static str,
    pub projection_effect: Option<&'static str>,
    pub unsupported_reason: Option<&'static str>,
    pub count: u64,
    pub historical_baseline: u64,
    pub new_violation: u64,
    pub sample_event_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ViolationSplit {
    pub historical_baseline: u64,
    pub new_violation: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefIntegrityAudit {
    pub agents: Vec<AgentBriefIntegrityAudit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentBriefIntegrityAudit {
    pub agent_id: String,
    pub counts: BriefIntegrityCounts,
    pub retained_event_floor: Option<u64>,
    pub event_head: Option<u64>,
    pub categories: Vec<BriefIntegrityCategory>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BriefIntegrityCounts {
    pub operator_messages: u64,
    pub terminal_turns: u64,
    pub operator_visible_assistant_deliveries: u64,
    pub canonical_briefs: u64,
    pub canonical_briefs_by_kind: BTreeMap<String, u64>,
    pub brief_created_events: u64,
    pub valid_linked_briefs: u64,
    pub missing_or_ambiguous_linkage: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefIntegrityCategory {
    pub category: &'static str,
    pub description: &'static str,
    pub observable_offline: bool,
    pub historical_baseline: u64,
    pub new_violation: u64,
    pub sample_ids: Vec<String>,
}

impl RuntimeDb {
    pub fn audit(&self, options: RuntimeDbAuditOptions) -> Result<RuntimeDbAuditReport> {
        let connection = self.connection()?;
        let baseline = options
            .baseline_through
            .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true));
        let sample_limit = i64::try_from(options.sample_limit).unwrap_or(i64::MAX);
        let projection_effects = options
            .check
            .includes_projection_effects()
            .then(|| projection_effects_audit(&connection, baseline.as_deref(), sample_limit))
            .transpose()?;
        let brief_integrity = options
            .check
            .includes_brief_integrity()
            .then(|| brief_integrity_audit(&connection, baseline.as_deref(), sample_limit))
            .transpose()?;
        Ok(RuntimeDbAuditReport {
            generated_at: Utc::now(),
            check: options.check,
            database: RuntimeDbAuditDatabase {
                path: self.path().display().to_string(),
                schema_version: current_schema_version(&connection)?,
                runtime_id: self.runtime_id()?,
                event_log_epoch: self.event_log_epoch()?,
            },
            baseline: RuntimeDbAuditBaseline {
                boundary_kind: "created_at",
                baseline_through: options.baseline_through,
                without_boundary_policy: "count_as_new_violation",
            },
            projection_effects,
            brief_integrity,
        })
    }
}

fn projection_effects_audit(
    connection: &Connection,
    baseline: Option<&str>,
    sample_limit: i64,
) -> Result<ProjectionEffectsAudit> {
    let mut groups = Vec::new();
    let mut statement = connection.prepare(
        "SELECT COALESCE(agent_id, '<runtime>'), kind,
                COALESCE(json_extract(data_json, '$.contract_version'), 1),
                COALESCE(json_extract(data_json, '$.payload_schema'), ''),
                COALESCE(json_extract(data_json, '$.payload_schema_version'), 0),
                COUNT(*),
                SUM(CASE WHEN ?1 IS NOT NULL AND created_at <= ?1 THEN 1 ELSE 0 END)
         FROM audit_events
         GROUP BY COALESCE(agent_id, '<runtime>'), kind,
                  COALESCE(json_extract(data_json, '$.contract_version'), 1),
                  COALESCE(json_extract(data_json, '$.payload_schema'), ''),
                  COALESCE(json_extract(data_json, '$.payload_schema_version'), 0)
         ORDER BY 1, 2, 4, 5, 3",
    )?;
    let rows = statement.query_map([baseline], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    for row in rows {
        let (
            agent_id,
            kind,
            contract_version,
            payload_schema,
            payload_schema_version,
            count,
            historical,
        ) = row?;
        let classification = classify_group(
            &kind,
            contract_version,
            &payload_schema,
            payload_schema_version,
        );
        groups.push(ProjectionEffectInventoryGroup {
            agent_id,
            kind,
            contract_version,
            payload_schema,
            payload_schema_version,
            classification: classification.0,
            projection_effect: classification.1,
            unsupported_reason: classification.2,
            count: to_u64(count, "projection inventory count")?,
            historical_baseline: to_u64(historical, "historical projection inventory count")?,
            new_violation: to_u64(count - historical, "new projection inventory count")?,
            sample_event_ids: Vec::new(),
        });
    }
    let mut indexes = BTreeMap::new();
    for (index, group) in groups.iter().enumerate() {
        indexes.insert(
            (
                group.agent_id.clone(),
                group.kind.clone(),
                group.contract_version,
                group.payload_schema.clone(),
                group.payload_schema_version,
            ),
            index,
        );
    }
    let mut statement = connection.prepare(
        "WITH ranked AS (
           SELECT COALESCE(agent_id, '<runtime>') AS agent_id, kind,
                  COALESCE(json_extract(data_json, '$.contract_version'), 1) AS contract_version,
                  COALESCE(json_extract(data_json, '$.payload_schema'), '') AS payload_schema,
                  COALESCE(json_extract(data_json, '$.payload_schema_version'), 0) AS payload_schema_version,
                  audit_event_id,
                  ROW_NUMBER() OVER (
                    PARTITION BY COALESCE(agent_id, '<runtime>'), kind,
                      COALESCE(json_extract(data_json, '$.contract_version'), 1),
                      COALESCE(json_extract(data_json, '$.payload_schema'), ''),
                      COALESCE(json_extract(data_json, '$.payload_schema_version'), 0)
                    ORDER BY event_seq, audit_event_id
                  ) AS sample_rank
           FROM audit_events
         )
         SELECT agent_id, kind, contract_version, payload_schema, payload_schema_version,
                audit_event_id
         FROM ranked WHERE sample_rank <= ?1
         ORDER BY agent_id, kind, payload_schema, payload_schema_version, contract_version,
                  sample_rank",
    )?;
    let rows = statement.query_map([sample_limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    for row in rows {
        let (agent_id, kind, contract_version, payload_schema, payload_schema_version, event_id) =
            row?;
        if let Some(index) = indexes.get(&(
            agent_id,
            kind,
            contract_version,
            payload_schema,
            payload_schema_version,
        )) {
            groups[*index].sample_event_ids.push(event_id);
        }
    }
    let totals = groups
        .iter()
        .filter(|group| group.unsupported_reason.is_some())
        .fold(ViolationSplit::default(), |mut totals, group| {
            totals.historical_baseline += group.historical_baseline;
            totals.new_violation += group.new_violation;
            totals
        });
    Ok(ProjectionEffectsAudit { groups, totals })
}

fn classify_group(
    kind: &str,
    contract_version: i64,
    payload_schema: &str,
    payload_schema_version: i64,
) -> (&'static str, Option<&'static str>, Option<&'static str>) {
    let classification = match (
        u32::try_from(contract_version),
        u32::try_from(payload_schema_version),
    ) {
        (Ok(contract_version), Ok(payload_schema_version)) => classify_projection_effect(
            kind,
            contract_version,
            payload_schema,
            payload_schema_version,
        ),
        _ => {
            ProjectionEffectClassification::Unsupported(UnsupportedProjectionEvent::InvalidMetadata)
        }
    };
    match classification {
        ProjectionEffectClassification::Exact(effect) => {
            ("exact", Some(projection_effect_name(effect)), None)
        }
        ProjectionEffectClassification::ConservativeLegacy(effect) => (
            "conservative_legacy",
            Some(projection_effect_name(effect)),
            None,
        ),
        ProjectionEffectClassification::Unsupported(reason) => {
            ("unsupported", None, Some(unsupported_reason_name(reason)))
        }
    }
}

fn projection_effect_name(effect: ProjectionEffect) -> &'static str {
    match effect {
        ProjectionEffect::None => "none",
        ProjectionEffect::DisplayInvalidation => "display_invalidation",
    }
}

fn unsupported_reason_name(reason: UnsupportedProjectionEvent) -> &'static str {
    match reason {
        UnsupportedProjectionEvent::UnknownTypedKind => "unknown_typed_kind",
        UnsupportedProjectionEvent::PayloadSchemaMismatch => "payload_schema_mismatch",
        UnsupportedProjectionEvent::FuturePayloadSchemaVersion => "future_payload_schema_version",
        UnsupportedProjectionEvent::InvalidMetadata => "invalid_metadata",
    }
}

fn brief_integrity_audit(
    connection: &Connection,
    baseline: Option<&str>,
    sample_limit: i64,
) -> Result<BriefIntegrityAudit> {
    let mut agents = load_agents(connection)?;
    load_simple_count(
        connection,
        "SELECT agent_id, COUNT(*) FROM messages
         WHERE json_extract(payload_json, '$.authority_class') = 'operator_instruction'
         GROUP BY agent_id",
        &mut agents,
        |counts, value| counts.operator_messages = value,
    )?;
    load_simple_count(
        connection,
        "SELECT agent_id, COUNT(*) FROM turn_records
         WHERE completed_at IS NOT NULL GROUP BY agent_id",
        &mut agents,
        |counts, value| counts.terminal_turns = value,
    )?;
    load_simple_count(
        connection,
        "SELECT agent_id, COUNT(DISTINCT turn_id) FROM transcript_entries
         WHERE turn_id IS NOT NULL AND kind = 'assistant_round'
           AND json_extract(payload_json, '$.data.visibility') = 'operator_visible'
           AND EXISTS (
             SELECT 1 FROM json_each(payload_json, '$.data.blocks') block
             WHERE json_extract(block.value, '$.type') = 'text'
               AND TRIM(COALESCE(json_extract(block.value, '$.text'), '')) != ''
           )
         GROUP BY agent_id",
        &mut agents,
        |counts, value| counts.operator_visible_assistant_deliveries = value,
    )?;
    load_simple_count(
        connection,
        "SELECT agent_id, COUNT(*) FROM briefs GROUP BY agent_id",
        &mut agents,
        |counts, value| counts.canonical_briefs = value,
    )?;
    load_simple_count(
        connection,
        "SELECT COALESCE(agent_id, json_extract(data_json, '$.data.agent_id')), COUNT(*)
         FROM audit_events WHERE kind = 'brief_created' GROUP BY 1",
        &mut agents,
        |counts, value| counts.brief_created_events = value,
    )?;
    load_simple_count(
        connection,
        "SELECT b.agent_id, COUNT(*) FROM briefs b
         WHERE b.created_event_seq IS NOT NULL
           AND (SELECT COUNT(*) FROM audit_events e
                WHERE e.kind = 'brief_created'
                  AND e.event_seq = b.created_event_seq
                  AND COALESCE(e.agent_id, json_extract(e.data_json, '$.data.agent_id')) = b.agent_id
                  AND json_extract(e.data_json, '$.data.brief_id') = b.evidence_id) = 1
         GROUP BY b.agent_id",
        &mut agents,
        |counts, value| counts.valid_linked_briefs = value,
    )?;
    {
        let mut statement = connection
            .prepare("SELECT agent_id, kind, COUNT(*) FROM briefs GROUP BY agent_id, kind")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (agent_id, kind, count) = row?;
            agents
                .entry(agent_id)
                .or_default()
                .counts
                .canonical_briefs_by_kind
                .insert(kind, to_u64(count, "brief kind count")?);
        }
    }
    {
        let mut statement = connection.prepare(
            "SELECT agent_id, MIN(event_seq), MAX(event_seq)
             FROM audit_events WHERE agent_id IS NOT NULL GROUP BY agent_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?;
        for row in rows {
            let (agent_id, floor, head) = row?;
            let agent = agents.entry(agent_id).or_default();
            agent.retained_event_floor = optional_u64(floor, "retained event floor")?;
            agent.event_head = optional_u64(head, "event head")?;
        }
    }
    load_brief_mismatches(connection, baseline, sample_limit, &mut agents)?;
    for agent in agents.values_mut() {
        agent.counts.missing_or_ambiguous_linkage = agent
            .categories
            .iter()
            .filter(|category| matches!(category.category, "C" | "D"))
            .map(|category| category.historical_baseline + category.new_violation)
            .sum();
    }
    Ok(BriefIntegrityAudit {
        agents: agents
            .into_iter()
            .map(|(agent_id, agent)| AgentBriefIntegrityAudit {
                agent_id,
                counts: agent.counts,
                retained_event_floor: agent.retained_event_floor,
                event_head: agent.event_head,
                categories: agent.categories,
            })
            .collect(),
    })
}

#[derive(Default)]
struct AgentBriefBuilder {
    counts: BriefIntegrityCounts,
    retained_event_floor: Option<u64>,
    event_head: Option<u64>,
    categories: Vec<BriefIntegrityCategory>,
}

fn load_agents(connection: &Connection) -> Result<BTreeMap<String, AgentBriefBuilder>> {
    let mut agents = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT agent_id FROM (
           SELECT agent_id FROM messages
           UNION SELECT agent_id FROM turn_records
           UNION SELECT agent_id FROM transcript_entries
           UNION SELECT agent_id FROM briefs
           UNION SELECT agent_id FROM audit_events WHERE agent_id IS NOT NULL
         ) ORDER BY agent_id",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        agents.insert(row?, AgentBriefBuilder::default());
    }
    Ok(agents)
}

fn load_simple_count(
    connection: &Connection,
    sql: &str,
    agents: &mut BTreeMap<String, AgentBriefBuilder>,
    apply: impl Fn(&mut BriefIntegrityCounts, u64),
) -> Result<()> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (agent_id, count) = row?;
        if let Some(agent_id) = agent_id {
            apply(
                &mut agents.entry(agent_id).or_default().counts,
                to_u64(count, "audit count")?,
            );
        }
    }
    Ok(())
}

fn load_brief_mismatches(
    connection: &Connection,
    baseline: Option<&str>,
    sample_limit: i64,
    agents: &mut BTreeMap<String, AgentBriefBuilder>,
) -> Result<()> {
    let sql = r#"
WITH operator_turns AS (
  SELECT t.agent_id, t.turn_id, COALESCE(t.completed_at, t.created_at) AS observed_at
  FROM turn_records t
  JOIN messages m ON m.evidence_id = t.trigger_message_id
  WHERE t.completed_at IS NOT NULL
    AND json_extract(m.payload_json, '$.authority_class') = 'operator_instruction'
),
deliveries AS (
  SELECT DISTINCT agent_id, turn_id FROM transcript_entries
  WHERE turn_id IS NOT NULL AND kind = 'assistant_round'
    AND json_extract(payload_json, '$.data.visibility') = 'operator_visible'
    AND EXISTS (
      SELECT 1 FROM json_each(payload_json, '$.data.blocks') block
      WHERE json_extract(block.value, '$.type') = 'text'
        AND TRIM(COALESCE(json_extract(block.value, '$.text'), '')) != ''
    )
),
event_windows AS (
  SELECT agent_id, MIN(event_seq) AS event_floor
  FROM audit_events WHERE agent_id IS NOT NULL GROUP BY agent_id
),
classified AS (
  SELECT o.agent_id, 'A' AS category, o.turn_id AS sample_id, o.observed_at
  FROM operator_turns o
  LEFT JOIN deliveries d ON d.agent_id = o.agent_id AND d.turn_id = o.turn_id
  LEFT JOIN briefs b ON b.agent_id = o.agent_id AND b.turn_id = o.turn_id
  WHERE d.turn_id IS NULL AND b.evidence_id IS NULL
  UNION ALL
  SELECT d.agent_id, 'B', d.turn_id, t.completed_at
  FROM deliveries d
  JOIN turn_records t ON t.agent_id = d.agent_id AND t.turn_id = d.turn_id
  LEFT JOIN briefs b ON b.agent_id = d.agent_id AND b.turn_id = d.turn_id
  WHERE b.evidence_id IS NULL
  UNION ALL
  SELECT b.agent_id, 'C', b.evidence_id, b.created_at
  FROM briefs b
  LEFT JOIN event_windows w ON w.agent_id = b.agent_id
  WHERE b.created_event_seq IS NULL
     OR (
       (SELECT COUNT(*) FROM audit_events e
        WHERE e.kind = 'brief_created'
          AND e.event_seq = b.created_event_seq
          AND COALESCE(e.agent_id, json_extract(e.data_json, '$.data.agent_id')) = b.agent_id
          AND json_extract(e.data_json, '$.data.brief_id') = b.evidence_id) != 1
       AND (w.event_floor IS NULL OR b.created_event_seq >= w.event_floor)
     )
     OR (
       b.created_event_seq IS NOT NULL
       AND (SELECT COUNT(*) FROM briefs shared
            WHERE shared.agent_id = b.agent_id
              AND shared.created_event_seq = b.created_event_seq) > 1
     )
  UNION ALL
  SELECT b.agent_id, 'D', b.evidence_id, b.created_at
  FROM briefs b
  JOIN event_windows w ON w.agent_id = b.agent_id
  WHERE b.created_event_seq IS NOT NULL AND b.created_event_seq < w.event_floor
    AND NOT EXISTS (
      SELECT 1 FROM audit_events e
      WHERE e.kind = 'brief_created' AND e.event_seq = b.created_event_seq
        AND COALESCE(e.agent_id, json_extract(e.data_json, '$.data.agent_id')) = b.agent_id
        AND json_extract(e.data_json, '$.data.brief_id') = b.evidence_id
    )
),
ranked AS (
  SELECT agent_id, category, sample_id,
         SUM(CASE WHEN ?1 IS NOT NULL AND observed_at <= ?1 THEN 1 ELSE 0 END)
           OVER (PARTITION BY agent_id, category) AS historical_baseline,
         SUM(CASE WHEN ?1 IS NULL OR observed_at > ?1 THEN 1 ELSE 0 END)
           OVER (PARTITION BY agent_id, category) AS new_violation,
         ROW_NUMBER() OVER (
           PARTITION BY agent_id, category ORDER BY observed_at, sample_id
         ) AS sample_rank
  FROM classified
)
SELECT agent_id, category, historical_baseline, new_violation, sample_id
FROM ranked WHERE sample_rank <= ?2
ORDER BY agent_id, category, sample_rank
"#;
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params![baseline, sample_limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut indexed = BTreeMap::<(String, String), (u64, u64, Vec<String>)>::new();
    for row in rows {
        let (agent_id, category, historical, new, sample_id) = row?;
        let entry = indexed.entry((agent_id, category)).or_insert((
            to_u64(historical, "historical brief mismatch count")?,
            to_u64(new, "new brief mismatch count")?,
            Vec::new(),
        ));
        entry.2.push(sample_id);
    }
    for (agent_id, agent) in agents {
        agent.categories = ["A", "B", "C", "D"]
            .into_iter()
            .map(|category| {
                let (historical_baseline, new_violation, sample_ids) = indexed
                    .remove(&(agent_id.clone(), category.to_string()))
                    .unwrap_or_default();
                BriefIntegrityCategory {
                    category,
                    description: brief_category_description(category),
                    observable_offline: true,
                    historical_baseline,
                    new_violation,
                    sample_ids,
                }
            })
            .chain(std::iter::once(BriefIntegrityCategory {
                category: "E",
                description: brief_category_description("E"),
                observable_offline: false,
                historical_baseline: 0,
                new_violation: 0,
                sample_ids: Vec::new(),
            }))
            .collect();
    }
    Ok(())
}

fn brief_category_description(category: &str) -> &'static str {
    match category {
        "A" => "operator turn without operator-visible assistant delivery or canonical Brief",
        "B" => "operator-visible assistant delivery without canonical Brief",
        "C" => "canonical Brief with missing, ambiguous, or invalid brief_created linkage",
        "D" => "canonical Brief linkage lies below the retained event floor",
        "E" => "browser received event but hydration or projection failed; not observable offline",
        _ => "unknown",
    }
}

fn to_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("{label} is negative"))
}

fn optional_u64(value: Option<i64>, label: &str) -> Result<Option<u64>> {
    value.map(|value| to_u64(value, label)).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rusqlite::params;

    fn temp_db() -> Result<(tempfile::TempDir, RuntimeDb)> {
        let temp_dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp_dir.path().join("state/runtime.sqlite"),
            temp_dir.path().join("state/runtime.lock"),
        )?;
        Ok((temp_dir, db))
    }

    fn insert_event(
        connection: &Connection,
        id: &str,
        seq: i64,
        agent_id: &str,
        kind: &str,
        created_at: &str,
        data_json: &str,
    ) -> Result<()> {
        connection.execute(
            "INSERT INTO audit_events (
               audit_event_id, event_seq, agent_id, kind, created_at, data_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, seq, agent_id, kind, created_at, data_json],
        )?;
        Ok(())
    }

    fn insert_linked_brief(
        connection: &Connection,
        agent_id: &str,
        brief_id: &str,
        event_id: &str,
        event_seq: i64,
    ) -> Result<()> {
        connection.execute(
            "INSERT INTO briefs (
               evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
             ) VALUES (
               ?1, ?2, '2026-01-01T00:00:00.000Z', 'result', 'bounded preview', '{}', ?3
             )",
            params![brief_id, agent_id, event_seq],
        )?;
        insert_event(
            connection,
            event_id,
            event_seq,
            agent_id,
            "brief_created",
            "2026-01-01T00:00:00.000Z",
            &serde_json::json!({
                "data": {"agent_id": agent_id, "brief_id": brief_id}
            })
            .to_string(),
        )
    }

    #[test]
    fn projection_inventory_splits_mixed_legacy_and_typed_events_at_baseline() -> Result<()> {
        let (_temp_dir, db) = temp_db()?;
        let connection = db.connection()?;
        insert_event(
            &connection,
            "event_legacy",
            1,
            "alpha",
            "message_enqueued",
            "2026-01-01T00:00:00.000Z",
            r#"{"data":{}}"#,
        )?;
        insert_event(
            &connection,
            "event_exact",
            2,
            "alpha",
            "brief_created",
            "2026-01-02T00:00:00.000Z",
            r#"{
              "contract_version":3,
              "payload_schema":"holon.runtime_event.brief_created",
              "payload_schema_version":1,
              "data":{"agent_id":"alpha","brief_id":"brief_exact"}
            }"#,
        )?;
        insert_event(
            &connection,
            "event_old_unsupported",
            3,
            "alpha",
            "future_kind",
            "2026-01-03T00:00:00.000Z",
            r#"{
              "contract_version":3,
              "payload_schema":"holon.runtime_event.future",
              "payload_schema_version":1,
              "data":{}
            }"#,
        )?;
        insert_event(
            &connection,
            "event_new_unsupported",
            4,
            "alpha",
            "brief_created",
            "2026-01-05T00:00:00.000Z",
            r#"{
              "contract_version":3,
              "payload_schema":"holon.runtime_event.brief_created",
              "payload_schema_version":2,
              "data":{"agent_id":"alpha","brief_id":"brief_future"}
            }"#,
        )?;
        drop(connection);

        let report = db.audit(RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::ProjectionEffects,
            baseline_through: Some(
                Utc.with_ymd_and_hms(2026, 1, 4, 0, 0, 0)
                    .single()
                    .expect("valid baseline"),
            ),
            sample_limit: 1,
        })?;
        let audit = report
            .projection_effects
            .as_ref()
            .expect("projection audit");

        assert_eq!(audit.groups.len(), 4);
        assert_eq!(audit.totals.historical_baseline, 1);
        assert_eq!(audit.totals.new_violation, 1);
        assert!(report.has_new_violations());
        assert!(audit.groups.iter().any(|group| {
            group.classification == "conservative_legacy"
                && group.sample_event_ids == ["event_legacy"]
        }));
        assert!(audit.groups.iter().any(|group| {
            group.classification == "exact"
                && group.projection_effect == Some("display_invalidation")
                && group.sample_event_ids == ["event_exact"]
        }));
        assert!(audit.groups.iter().any(|group| {
            group.unsupported_reason == Some("unknown_typed_kind")
                && group.historical_baseline == 1
                && group.new_violation == 0
        }));
        assert!(audit.groups.iter().any(|group| {
            group.unsupported_reason == Some("future_payload_schema_version")
                && group.historical_baseline == 0
                && group.new_violation == 1
        }));
        Ok(())
    }

    #[test]
    fn brief_integrity_reports_categories_a_through_e_without_content() -> Result<()> {
        let (_temp_dir, db) = temp_db()?;
        let connection = db.connection()?;
        for (turn_id, created_at) in [
            ("turn_a", "2026-01-01T00:00:00.000Z"),
            ("turn_b", "2026-01-03T00:00:00.000Z"),
        ] {
            connection.execute(
                "INSERT INTO messages (
                   evidence_id, agent_id, turn_id, created_at, kind, payload_json
                 ) VALUES (?1, 'alpha', ?2, ?3, 'operator_prompt',
                   '{\"authority_class\":\"operator_instruction\"}')",
                params![format!("message_{turn_id}"), turn_id, created_at],
            )?;
            connection.execute(
                "INSERT INTO turn_records (
                   turn_id, turn_index, agent_id, trigger_message_id,
                   terminal_kind, created_at, completed_at, payload_json
                 ) VALUES (?1, ?2, 'alpha', ?3, 'completed', ?4, ?4, '{}')",
                params![
                    turn_id,
                    if turn_id == "turn_a" { 1 } else { 2 },
                    format!("message_{turn_id}"),
                    created_at
                ],
            )?;
        }
        connection.execute(
            "INSERT INTO transcript_entries (
               evidence_id, agent_id, turn_id, created_at, kind, payload_json
             ) VALUES (
               'transcript_b', 'alpha', 'turn_b', '2026-01-03T00:00:00.000Z',
               'assistant_round',
               '{\"data\":{\"visibility\":\"operator_visible\",\"blocks\":[{\"type\":\"text\",\"text\":\"delivered\"}]}}'
             )",
            [],
        )?;
        connection.execute(
            "INSERT INTO briefs (
               evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
             ) VALUES (
               'brief_c', 'alpha', '2026-01-01T00:00:00.000Z',
               'result', 'bounded preview', '{}', NULL
             )",
            [],
        )?;
        connection.execute(
            "INSERT INTO briefs (
               evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
             ) VALUES (
               'brief_d', 'alpha', '2026-01-03T00:00:00.000Z',
               'result', 'bounded preview', '{}', 5
             )",
            [],
        )?;
        insert_event(
            &connection,
            "event_floor",
            10,
            "alpha",
            "message_enqueued",
            "2026-01-03T00:00:00.000Z",
            r#"{"data":{}}"#,
        )?;
        drop(connection);

        let report = db.audit(RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::BriefIntegrity,
            baseline_through: Some(
                Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0)
                    .single()
                    .expect("valid baseline"),
            ),
            sample_limit: 2,
        })?;
        let audit = report.brief_integrity.as_ref().expect("brief audit");
        let agent = audit
            .agents
            .iter()
            .find(|agent| agent.agent_id == "alpha")
            .unwrap();
        let category = |name| {
            agent
                .categories
                .iter()
                .find(|category| category.category == name)
                .unwrap()
        };

        assert_eq!(agent.retained_event_floor, Some(10));
        assert_eq!(agent.event_head, Some(10));
        assert_eq!(agent.counts.operator_messages, 2);
        assert_eq!(agent.counts.terminal_turns, 2);
        assert_eq!(agent.counts.operator_visible_assistant_deliveries, 1);
        assert_eq!(agent.counts.canonical_briefs, 2);
        assert_eq!(agent.counts.missing_or_ambiguous_linkage, 2);
        assert_eq!(category("A").historical_baseline, 1);
        assert_eq!(category("A").sample_ids, ["turn_a"]);
        assert_eq!(category("B").new_violation, 1);
        assert_eq!(category("B").sample_ids, ["turn_b"]);
        assert_eq!(category("C").historical_baseline, 1);
        assert_eq!(category("C").sample_ids, ["brief_c"]);
        assert_eq!(category("D").new_violation, 1);
        assert_eq!(category("D").sample_ids, ["brief_d"]);
        assert!(!category("E").observable_offline);
        assert!(category("E").sample_ids.is_empty());
        assert!(report.has_new_violations());
        Ok(())
    }

    #[test]
    fn brief_integrity_scopes_shared_sequences_by_agent() -> Result<()> {
        let (_temp_dir, db) = temp_db()?;
        let connection = db.connection()?;
        insert_linked_brief(&connection, "alpha", "brief_alpha", "event_alpha", 1)?;
        insert_linked_brief(&connection, "beta", "brief_beta", "event_beta", 1)?;
        connection.execute_batch(
            "DROP INDEX briefs_agent_created_event_seq;
             DROP INDEX idx_audit_events_agent_event_seq_unique;",
        )?;
        insert_linked_brief(
            &connection,
            "gamma",
            "brief_gamma_one",
            "event_gamma_one",
            1,
        )?;
        insert_linked_brief(
            &connection,
            "gamma",
            "brief_gamma_two",
            "event_gamma_two",
            1,
        )?;
        drop(connection);

        let report = db.audit(RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::BriefIntegrity,
            baseline_through: None,
            sample_limit: 10,
        })?;
        let audit = report.brief_integrity.as_ref().expect("brief audit");
        for agent_id in ["alpha", "beta"] {
            let agent = audit
                .agents
                .iter()
                .find(|agent| agent.agent_id == agent_id)
                .expect("agent audit");
            assert_eq!(agent.counts.valid_linked_briefs, 1);
            assert_eq!(agent.counts.missing_or_ambiguous_linkage, 0);
            assert_eq!(
                agent
                    .categories
                    .iter()
                    .find(|category| category.category == "C")
                    .expect("category C")
                    .new_violation,
                0
            );
        }
        let gamma = audit
            .agents
            .iter()
            .find(|agent| agent.agent_id == "gamma")
            .expect("gamma audit");
        assert_eq!(gamma.counts.valid_linked_briefs, 2);
        assert_eq!(gamma.counts.missing_or_ambiguous_linkage, 2);
        let category_c = gamma
            .categories
            .iter()
            .find(|category| category.category == "C")
            .expect("category C");
        assert_eq!(category_c.new_violation, 2);
        assert_eq!(
            category_c.sample_ids,
            ["brief_gamma_one", "brief_gamma_two"]
        );
        Ok(())
    }

    #[test]
    fn projection_inventory_aggregates_large_event_sets_with_bounded_samples() -> Result<()> {
        let (_temp_dir, db) = temp_db()?;
        let mut connection = db.connection()?;
        let transaction = connection.transaction()?;
        for index in 0..5_000 {
            transaction.execute(
                "INSERT INTO audit_events (
                   audit_event_id, event_seq, agent_id, kind, created_at, data_json
                 ) VALUES (?1, ?2, 'scale', 'message_enqueued',
                   '2026-01-01T00:00:00.000Z', ?3)",
                params![
                    format!("event_{index:05}"),
                    index + 1,
                    r#"{
                      "contract_version":3,
                      "payload_schema":"holon.runtime_event.message_lifecycle",
                      "payload_schema_version":1,
                      "data":{}
                    }"#
                ],
            )?;
        }
        transaction.commit()?;
        drop(connection);

        let report = db.audit(RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::ProjectionEffects,
            baseline_through: None,
            sample_limit: 3,
        })?;
        let groups = report.projection_effects.expect("projection audit").groups;

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 5_000);
        assert_eq!(groups[0].new_violation, 5_000);
        assert_eq!(groups[0].sample_event_ids.len(), 3);
        assert_eq!(
            groups[0].sample_event_ids,
            ["event_00000", "event_00001", "event_00002"]
        );
        Ok(())
    }
}
