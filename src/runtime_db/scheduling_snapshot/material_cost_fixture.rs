//! Independent scalar accounting for the fixed budget-test fixtures.
//! Never runs the loader or Material::charge to discover sufficient headroom.
use super::*;
use crate::diagnostics::resource_io::charge_owned_metadata;
use crate::runtime_db::RuntimeDb;

pub(in crate::runtime_db) struct ExpectedSqlMaterial<'a> {
    db: &'a RuntimeDb,
    scope: &'a InputEvidenceScope,
}

impl<'a> ExpectedSqlMaterial<'a> {
    pub(in crate::runtime_db) fn new(db: &'a RuntimeDb, scope: &'a InputEvidenceScope) -> Self {
        Self { db, scope }
    }

    pub(in crate::runtime_db) fn charge(&self, bytes: usize, records: usize) -> Result<()> {
        charge_owned_metadata((bytes + self.scope.agent_id.len() + 128) as u64, records)?;
        Ok(())
    }

    pub(in crate::runtime_db) fn before_selection(
        &self,
        load: &SchedulingDomainLoadRequest,
        coverage: &SchedulingCoverage,
    ) -> Result<()> {
        let conn = self.db.connection()?;
        let agent = &self.scope.agent_id;
        self.charge(serde_json::to_vec(self.scope)?.len(), 1)?;
        let identity: usize = conn.query_row(
        "SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1",
        [&agent], |r| r.get(0),
    )?;
        self.charge(identity, 1)?;
        let (state, nodes): (usize, usize) = conn.query_row(
        "SELECT length(CAST(payload_json AS BLOB)),(SELECT COUNT(*) FROM json_tree(payload_json)) FROM agent_states WHERE agent_id=?1",
        [&agent], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
        self.charge(state + 256, 1)?;
        self.charge(nodes * std::mem::size_of::<serde_json::Value>(), nodes)?;
        let binding: usize = conn.query_row(
        "SELECT length(CAST(mode AS BLOB))+length(CAST(seed_id AS BLOB)) FROM agent_episode_continuity WHERE agent_id=?1",
        [&agent], |r| r.get(0),
    )?;
        self.charge(binding, 1)?;
        let captures: usize = conn.query_row(
        "SELECT SUM(length(CAST(inventory_id AS BLOB))+64) FROM scheduling_domain_captures WHERE seed_id=(SELECT seed_id FROM agent_episode_continuity WHERE agent_id=?1)",
        [&agent], |r| r.get(0),
    )?;
        self.charge(captures, 3)?;
        let mut manifests = conn.prepare(
        "SELECT length(CAST(agent_id AS BLOB))+length(CAST(tables_json AS BLOB))+length(CAST(content_digest AS BLOB))+128 FROM scheduling_fixed_manifests WHERE inventory_id IN(SELECT inventory_id FROM scheduling_domain_captures WHERE seed_id=(SELECT seed_id FROM agent_episode_continuity WHERE agent_id=?1))",
    )?;
        for bytes in manifests.query_map([&agent], |r| r.get::<_, usize>(0))? {
            self.charge(bytes?, 1)?;
        }
        let saved: usize = conn.query_row(
        "SELECT length(CAST(coverage_json AS BLOB)) FROM scheduling_domain_current WHERE agent_id=?1",
        [&agent], |r| r.get(0),
    )?;
        self.charge(saved + 32, 1)?;
        let entries = coverage.active_timers.len()
            + coverage.pending_wakes.len()
            + coverage.active_triggers.len()
            + coverage.required_keys.len()
            + coverage.observations.len()
            + coverage.history.len()
            + coverage
                .history
                .values()
                .map(|h| h.keys.len() + h.loaded_keys.len())
                .sum::<usize>();
        self.charge(entries * 128, entries)?;
        self.charge(
            serde_json::to_vec(&load.capture.required_keys)?.len(),
            load.capture.required_keys.len(),
        )?;
        // The caller checkpoints each owning selector copy independently.
        Ok(())
    }

    fn query(&self, length: usize) -> Result<()> {
        self.charge(length + std::mem::size_of::<String>(), 1)
    }

    fn selector_lengths(&self, family: SchedulingFamily) -> Result<(usize, usize)> {
        // Original SQL literal lengths, independently derived from its text.
        // Inventory IDs are measured as scalars, never via a production builder.
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT length(CAST(inventory_id AS BLOB))+length(inventory_id)-length(replace(inventory_id,'''',''))+2
             FROM scheduling_domain_captures WHERE seed_id=(SELECT seed_id FROM agent_episode_continuity WHERE agent_id=?1)
             ORDER BY CASE capture_phase WHEN 'first_import' THEN 1 ELSE 0 END",
        )?;
        let mut inventory = 0;
        let mut ranks = 0;
        let mut count = 0;
        for (index, literal) in statement
            .query_map([&self.scope.agent_id], |row| row.get::<_, usize>(0))?
            .enumerate()
        {
            let literal = literal?;
            inventory += literal + usize::from(index > 0);
            ranks += literal + 11 + index.to_string().len() + usize::from(index > 0);
            count += 1;
        }
        if count == 0 {
            inventory = 4; // NULL
            ranks = 14; // WHEN '' THEN -1
        }
        let (live, heads) = match family {
            SchedulingFamily::Timer => (649, 806),
            SchedulingFamily::TimerWake => (847, 821),
            SchedulingFamily::ExternalTrigger => (760, 839),
        };
        Ok((live + inventory, heads + 2 * inventory + 2 * ranks))
    }

    pub(in crate::runtime_db) fn ids<'b>(
        &self,
        family: SchedulingFamily,
        live: bool,
        limit: Option<usize>,
        keys: impl IntoIterator<Item = &'b SchedulingKey>,
    ) -> Result<()> {
        let (live_length, heads) = self.selector_lengths(family)?;
        let mut selected = if live {
            live_length
        } else {
            heads
                + match family {
                    SchedulingFamily::Timer => 140,
                    SchedulingFamily::TimerWake => 124,
                    SchedulingFamily::ExternalTrigger => 123,
                }
        };
        if let Some(limit) = limit {
            selected += 7 + limit.to_string().len();
        }
        self.query(selected + 77)?;
        let (mut bytes, mut count) = (0, 0);
        for key in keys {
            bytes += key.id.len() + 64;
            count += 1;
        }
        self.charge(bytes, count)
    }

    pub(in crate::runtime_db) fn alias(&self, key: &SchedulingKey) -> Result<()> {
        let conn = self.db.connection()?;
        let bytes: usize = conn.query_row(
        "SELECT length(CAST(record_id AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(projection_digest AS BLOB)),0)+length(CAST(availability AS BLOB))+COALESCE(length(CAST(status AS BLOB)),0)+COALESCE(length(CAST(sort_at AS BLOB)),0)+COALESCE(length(CAST(created_at AS BLOB)),0)+64 FROM scheduling_domain_records WHERE agent_id=?1 AND family=?2 AND record_id=?3",
        params![self.scope.agent_id, family_name(key.family), key.id], |r| r.get(0),
    )?;
        self.charge(bytes, 1)
    }

    pub(in crate::runtime_db) fn stored(&self, key: &SchedulingKey) -> Result<()> {
        // Reuse only canonical schema projections, not the loader's admission path.
        let (from, id, owner, _, sort) = stored::source(key.family);
        let projection = stored::projection(key.family);
        let raw = if key.family == SchedulingFamily::TimerWake {
            projection
        } else {
            "c.payload_json"
        };
        let bytes: usize = self.db.connection()?.query_row(
        &format!("SELECT length(CAST({raw} AS BLOB))+length(CAST({projection} AS BLOB))+length(CAST({owner} AS BLOB))+length(CAST({id} AS BLOB))+length(CAST({sort} AS BLOB))+length(CAST(c.created_at AS BLOB))+128 FROM {from} WHERE {id}=?1"),
        [&key.id], |r| r.get(0),
    )?;
        self.charge(bytes, 1)?;
        self.alias(key)
    }

    pub(in crate::runtime_db) fn loaded_history(
        &self,
        family: SchedulingFamily,
        limit: usize,
        key: Option<&SchedulingKey>,
    ) -> Result<()> {
        let (_, heads) = self.selector_lengths(family)?;
        let (qualified, projection_bound, candidates_fixed, validated_fixed) = match family {
            SchedulingFamily::Timer => (56, 616, 399, 171),
            SchedulingFamily::ExternalTrigger => (39, 505, 428, 193),
            SchedulingFamily::TimerWake => unreachable!(),
        };
        let qualified = heads + qualified;
        let order = 137 + (limit - 1).to_string().len();
        let boundary = 280 + 3 * qualified + 2 * order;
        let candidates = candidates_fixed + projection_bound + boundary;
        // Intermediate fragments borrow their inputs; only executed queries own strings.
        self.query(348 + candidates)?;
        let pruned = if let Some(key) = key {
            ensure!(family == SchedulingFamily::Timer);
            let conn = self.db.connection()?;
            let (header, pruned): (usize, bool) = conn.query_row(
        "SELECT length(CAST(record_id AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(availability AS BLOB))+256,availability='pruned' FROM scheduling_domain_records WHERE agent_id=?1 AND family='timer' AND record_id=?2",
        params![self.scope.agent_id, key.id], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
            let body = if pruned {
                0
            } else {
                conn.query_row(
            "SELECT length(CAST(agent_id AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(payload_json AS BLOB))+
             6*(length(CAST(timer_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+COALESCE(length(CAST(summary AS BLOB)),0)+length(CAST(created_at AS BLOB))+length(CAST(duration_ms AS BLOB))+COALESCE(length(CAST(interval_ms AS BLOB)),0)+length(CAST(repeat AS BLOB))+COALESCE(length(CAST(next_fire_at AS BLOB)),0)+COALESCE(length(CAST(last_fired_at AS BLOB)),0)+length(CAST(fire_count AS BLOB))+length(CAST(updated_at AS BLOB)))+1024 FROM timers WHERE timer_id=?1",
            [&key.id], |r| r.get::<_, usize>(0),
        )?
            };
            self.charge(header + body, 1)?;
            pruned
        } else {
            self.charge(0, 0)?;
            false
        };
        self.query(82 + candidates)?;
        self.query(validated_fixed + stored::projection(family).len() + boundary)?;
        if !pruned {
            if let Some(key) = key {
                self.alias(key)?;
            }
            self.query(74 + candidates)?;
        }
        Ok(())
    }
}
