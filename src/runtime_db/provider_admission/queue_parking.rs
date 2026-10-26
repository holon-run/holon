//! Durable queue parking conditions, independent of queue and wait lifecycles.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQueueParking {
    pub agent_id: String,
    pub message_id: String,
    pub chain_fingerprint: String,
    pub episode_key: String,
    pub not_before: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderDispatchPause {
    pub parking: ProviderQueueParking,
    pub attempt_id: String,
    pub denial: &'static str,
}

impl ProviderQueueParking {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.agent_id.is_empty()
                && !self.message_id.is_empty()
                && !self.chain_fingerprint.is_empty()
                && !self.episode_key.is_empty()
                && matches!(
                    self.reason.as_str(),
                    "chain_cooling" | "recovery_budget_exhausted"
                ),
            "invalid provider queue parking condition"
        );
        Ok(())
    }
}

pub(crate) fn read_parking(
    connection: &Connection,
    agent_id: &str,
    message_id: &str,
) -> Result<Option<ProviderQueueParking>> {
    let raw = connection
        .query_row(
            "SELECT chain_fingerprint, episode_key, not_before, reason
             FROM provider_queue_parking WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, message_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    raw.map(|(chain_fingerprint, episode_key, not_before, reason)| {
        let record = ProviderQueueParking {
            agent_id: agent_id.to_owned(),
            message_id: message_id.to_owned(),
            chain_fingerprint,
            episode_key,
            not_before: not_before
                .parse()
                .context("invalid provider queue parking deadline")?,
            reason,
        };
        record.validate()?;
        Ok(record)
    })
    .transpose()
}

impl RuntimeDb {
    /// Replaying an unchanged condition does not write or move its deadline.
    /// The caller supplies the canonical gate's deadline, not a new wake-relative floor.
    pub fn park_provider_queue(&self, record: &ProviderQueueParking) -> Result<bool> {
        record.validate()?;
        self.transaction(|tx| {
            if read_parking(tx, &record.agent_id, &record.message_id)?.as_ref() == Some(record) {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO provider_queue_parking (
                    agent_id, message_id, chain_fingerprint, episode_key, not_before, reason
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(agent_id, message_id) DO UPDATE SET
                    chain_fingerprint = excluded.chain_fingerprint,
                    episode_key = excluded.episode_key,
                    not_before = excluded.not_before,
                    reason = excluded.reason",
                params![
                    record.agent_id,
                    record.message_id,
                    record.chain_fingerprint,
                    record.episode_key,
                    record.not_before.to_rfc3339(),
                    record.reason,
                ],
            )?;
            Ok(true)
        })
    }

    pub fn provider_queue_parking(
        &self,
        agent_id: &str,
        message_id: &str,
    ) -> Result<Option<ProviderQueueParking>> {
        self.with_read(|connection| read_parking(connection, agent_id, message_id))
    }

    pub fn clear_provider_queue_parking(&self, agent_id: &str, message_id: &str) -> Result<()> {
        self.transaction(|tx| {
            tx.execute(
                "DELETE FROM provider_queue_parking WHERE agent_id = ?1 AND message_id = ?2",
                params![agent_id, message_id],
            )?;
            Ok(())
        })
    }

    pub fn clear_agent_provider_queue_parking(&self, agent_id: &str) -> Result<()> {
        self.transaction(|tx| {
            tx.execute(
                "DELETE FROM provider_queue_parking WHERE agent_id = ?1",
                [agent_id],
            )?;
            Ok(())
        })
    }

    pub fn next_provider_queue_recheck_at(&self, agent_id: &str) -> Result<Option<DateTime<Utc>>> {
        self.with_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT parking.not_before
                 FROM provider_queue_parking AS parking
                 JOIN queue_entries AS source
                   ON source.agent_id = parking.agent_id
                  AND source.message_id = parking.message_id
                 WHERE parking.agent_id = ?1 AND source.status IN ('queued', 'interrupted')",
            )?;
            let mut earliest = None;
            // Parse every row: SQL MIN over text could hide a corrupt deadline
            // behind a valid one, or order equivalent timestamp offsets wrongly.
            for raw in statement.query_map([agent_id], |row| row.get::<_, String>(0))? {
                let deadline: DateTime<Utc> = raw?
                    .parse()
                    .context("invalid provider queue parking deadline")?;
                earliest =
                    Some(earliest.map_or(deadline, |current: DateTime<Utc>| current.min(deadline)));
            }
            Ok(earliest)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(dir: &tempfile::TempDir) -> Result<RuntimeDb> {
        RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )
    }

    fn parking(agent_id: &str, message_id: &str, seconds: i64) -> ProviderQueueParking {
        ProviderQueueParking {
            agent_id: agent_id.into(),
            message_id: message_id.into(),
            chain_fingerprint: "chain-a".into(),
            episode_key: "episode-a".into(),
            not_before: "2026-10-24T17:00:00Z".parse::<DateTime<Utc>>().unwrap()
                + Duration::seconds(seconds),
            reason: "chain_cooling".into(),
        }
    }

    fn queue_source(db: &RuntimeDb, agent_id: &str, message_id: &str, status: &str) -> Result<()> {
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO queue_entries (
                    message_id, agent_id, priority, status, created_at, updated_at, payload_json
                 ) VALUES (?1, ?2, 'normal', ?3, ?4, ?4, '{}')
                 ON CONFLICT(message_id) DO UPDATE SET status = excluded.status",
                params![
                    message_id,
                    agent_id,
                    status,
                    timestamp(parking(agent_id, message_id, 0).not_before)
                ],
            )?;
            Ok(())
        })
    }

    #[test]
    fn queue_parking_restart_replay_does_not_write_or_extend_deadline() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        let mut record = parking("agent-a", "message-a", 60);
        record.not_before += Duration::nanoseconds(123_456);
        queue_source(&db, &record.agent_id, &record.message_id, "queued")?;
        assert!(db.park_provider_queue(&record)?);
        assert!(!db.park_provider_queue(&record)?);
        drop(db);
        let db = open(&dir)?;
        assert!(!db.park_provider_queue(&record)?);
        assert_eq!(
            db.provider_queue_parking("agent-a", "message-a")?,
            Some(record.clone())
        );
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(record.not_before)
        );
        let mut changed = record;
        changed.episode_key = "episode-b".into();
        changed.reason = "recovery_budget_exhausted".into();
        assert!(db.park_provider_queue(&changed)?);
        assert!(!db.park_provider_queue(&changed)?);
        Ok(())
    }

    #[test]
    fn queue_parking_rechecks_and_clears_are_agent_and_message_scoped() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        queue_source(&db, "agent-a", "same-message", "queued")?;
        queue_source(&db, "agent-a", "other-message", "queued")?;
        for record in [
            parking("agent-a", "same-message", 60),
            parking("agent-a", "other-message", 30),
            parking("agent-b", "same-message", 10),
        ] {
            db.park_provider_queue(&record)?;
        }
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(parking("agent-a", "other-message", 30).not_before)
        );
        db.clear_provider_queue_parking("agent-a", "other-message")?;
        assert!(db
            .provider_queue_parking("agent-a", "same-message")?
            .is_some());
        db.clear_agent_provider_queue_parking("agent-a")?;
        assert_eq!(db.next_provider_queue_recheck_at("agent-a")?, None);
        assert!(db
            .provider_queue_parking("agent-b", "same-message")?
            .is_some());
        Ok(())
    }

    #[test]
    fn queue_parking_rechecks_ignore_claimed_terminal_missing_and_foreign_sources() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        for status in [
            "dequeued",
            "processed",
            "interjected",
            "aborted",
            "dropped",
            "quarantined",
        ] {
            let message_id = format!("message-{status}");
            queue_source(&db, "agent-a", &message_id, status)?;
            db.park_provider_queue(&parking("agent-a", &message_id, 0))?;
        }
        db.park_provider_queue(&parking("agent-a", "missing-message", 0))?;
        queue_source(&db, "agent-b", "foreign-message", "queued")?;
        db.park_provider_queue(&parking("agent-a", "foreign-message", 0))?;
        queue_source(&db, "agent-a", "queued-message", "queued")?;
        queue_source(&db, "agent-a", "interrupted-message", "interrupted")?;
        db.park_provider_queue(&parking("agent-a", "queued-message", 60))?;
        db.park_provider_queue(&parking("agent-a", "interrupted-message", 30))?;
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(parking("agent-a", "interrupted-message", 30).not_before)
        );
        queue_source(&db, "agent-a", "interrupted-message", "dequeued")?;
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(parking("agent-a", "queued-message", 60).not_before)
        );
        queue_source(&db, "agent-a", "queued-message", "processed")?;
        assert_eq!(db.next_provider_queue_recheck_at("agent-a")?, None);
        assert!(db
            .provider_queue_parking("agent-a", "missing-message")?
            .is_some());
        assert!(db
            .provider_queue_parking("agent-a", "interrupted-message")?
            .is_some());
        Ok(())
    }

    #[test]
    fn queue_parking_invalid_deadlines_fail_closed_even_when_not_the_minimum() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        queue_source(&db, "agent-a", "corrupt-message", "queued")?;
        queue_source(&db, "agent-a", "valid-message", "interrupted")?;
        let invalid = parking("agent-a", "corrupt-message", 90);
        db.park_provider_queue(&invalid)?;
        db.park_provider_queue(&parking("agent-a", "valid-message", 10))?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE provider_queue_parking SET not_before = 'not-a-date'
                 WHERE agent_id = 'agent-a' AND message_id = 'corrupt-message'",
                [],
            )?;
            Ok(())
        })?;
        assert!(db
            .provider_queue_parking("agent-a", "corrupt-message")
            .is_err());
        assert!(db.next_provider_queue_recheck_at("agent-a").is_err());
        assert!(db.park_provider_queue(&invalid).is_err());
        assert_eq!(db.next_provider_queue_recheck_at("agent-b")?, None);
        let mut invalid_reason = parking("agent-b", "message", 30);
        invalid_reason.reason = "arbitrary_retry".into();
        assert!(db.park_provider_queue(&invalid_reason).is_err());
        Ok(())
    }
}
