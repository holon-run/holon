//! Durable idempotency records for operations which may terminate this daemon.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::runtime_db::RuntimeDb;
use crate::types::TurnRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestructiveOperationPhase {
    Planned,
    Scheduled,
    DaemonInterrupted,
    Verified,
}

impl DestructiveOperationPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Scheduled => "scheduled",
            Self::DaemonInterrupted => "daemon_interrupted",
            Self::Verified => "verified",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "planned" => Ok(Self::Planned),
            "scheduled" => Ok(Self::Scheduled),
            "daemon_interrupted" => Ok(Self::DaemonInterrupted),
            "verified" => Ok(Self::Verified),
            _ => Err(anyhow!("unknown destructive operation phase: {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructiveOperationRecord {
    pub operation_id: String,
    pub owner_turn_id: String,
    pub owner_work_item_id: Option<String>,
    pub command_digest: String,
    pub phase: DestructiveOperationPhase,
    pub verification_target: String,
    pub recovery_policy: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub fn command_digest(command: &str) -> String {
    format!("{:x}", Sha256::digest(command.as_bytes()))
}

/// Outcome of fencing a destructive operation.
///
/// `should_dispatch` is true only for the single call allowed to hand the
/// restart to the external supervisor. Every other call must report
/// "already scheduled" and move on to verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDestructiveOperation {
    pub record: DestructiveOperationRecord,
    pub should_dispatch: bool,
}

pub struct DestructiveOperationRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

impl DestructiveOperationRepository<'_> {
    /// Fence a destructive operation before dispatch.
    ///
    /// Returns the durable record plus whether this call may dispatch. A caller
    /// must not dispatch when the operation id already exists or when another
    /// active (non-`verified`) operation already fences the same owner WorkItem.
    pub fn plan_and_schedule(
        &self,
        record: &DestructiveOperationRecord,
    ) -> Result<PlannedDestructiveOperation> {
        self.db.transaction(|tx| {
            let existing = tx
                .query_row(
                    "SELECT operation_id, owner_turn_id, owner_work_item_id, phase,
                            verification_target, recovery_policy, command_digest,
                            created_at, updated_at
                     FROM destructive_operations WHERE operation_id = ?1",
                    [&record.operation_id],
                    decode_operation,
                )
                .optional()?;
            if let Some(existing) = existing {
                if existing.owner_turn_id != record.owner_turn_id
                    || existing.owner_work_item_id != record.owner_work_item_id
                    || existing.command_digest != record.command_digest
                    || existing.verification_target != record.verification_target
                {
                    return Err(anyhow!(
                        "destructive operation id {} is bound to a different operation",
                        record.operation_id
                    ));
                }
                if existing.phase == DestructiveOperationPhase::Planned {
                    // The previous dispatch attempt failed before leaving the
                    // daemon; re-arm the same durable id instead of minting a
                    // new one.
                    tx.execute(
                        "UPDATE destructive_operations SET phase = 'scheduled', updated_at = ?1
                         WHERE operation_id = ?2",
                        params![Utc::now().to_rfc3339(), &record.operation_id],
                    )?;
                    let mut rearmed = existing;
                    rearmed.phase = DestructiveOperationPhase::Scheduled;
                    return Ok(PlannedDestructiveOperation {
                        record: rearmed,
                        should_dispatch: true,
                    });
                }
                return Ok(PlannedDestructiveOperation {
                    record: existing,
                    should_dispatch: false,
                });
            }

            if let Some(work_item_id) = record.owner_work_item_id.as_deref() {
                let active = tx
                    .query_row(
                        "SELECT operation_id, owner_turn_id, owner_work_item_id, phase,
                                verification_target, recovery_policy, command_digest,
                                created_at, updated_at
                         FROM destructive_operations
                         WHERE owner_work_item_id = ?1 AND phase != 'verified'
                         ORDER BY updated_at DESC
                         LIMIT 1",
                        [work_item_id],
                        decode_operation,
                    )
                    .optional()?;
                if let Some(active) = active {
                    return Ok(PlannedDestructiveOperation {
                        record: active,
                        should_dispatch: false,
                    });
                }
            }

            let payload = tx
                .query_row(
                    "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                    [&record.owner_turn_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("owner turn {} is not durable", record.owner_turn_id))?;
            let mut turn: TurnRecord = serde_json::from_str(&payload)?;
            if let Some(existing_id) = turn.destructive_operation_id.as_deref() {
                if existing_id != record.operation_id {
                    return Err(anyhow!(
                        "owner turn {} already has destructive operation {}",
                        record.owner_turn_id,
                        existing_id
                    ));
                }
            }
            turn.destructive_operation_id = Some(record.operation_id.clone());
            tx.execute(
                "UPDATE turn_records SET payload_json = ?1 WHERE turn_id = ?2",
                params![serde_json::to_string(&turn)?, record.owner_turn_id],
            )?;
            tx.execute(
                "INSERT INTO destructive_operations
                 (operation_id, owner_turn_id, owner_work_item_id, phase,
                  verification_target, recovery_policy, command_digest, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'scheduled', ?4, ?5, ?6, ?7, ?7)",
                params![
                    record.operation_id,
                    record.owner_turn_id,
                    record.owner_work_item_id,
                    record.verification_target,
                    record.recovery_policy,
                    record.command_digest,
                    record.created_at.to_rfc3339(),
                ],
            )?;
            let scheduled = tx.query_row(
                "SELECT operation_id, owner_turn_id, owner_work_item_id, phase,
                         verification_target, recovery_policy, command_digest,
                         created_at, updated_at
                 FROM destructive_operations WHERE operation_id = ?1",
                [&record.operation_id],
                decode_operation,
            )?;
            Ok(PlannedDestructiveOperation {
                record: scheduled,
                should_dispatch: true,
            })
        })
    }

    /// Insert once; repeated requests return the durable record.
    pub fn plan_or_get(
        &self,
        record: &DestructiveOperationRecord,
    ) -> Result<DestructiveOperationRecord> {
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO destructive_operations
                 (operation_id, owner_turn_id, owner_work_item_id, phase,
                  verification_target, recovery_policy, command_digest, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    record.operation_id,
                    record.owner_turn_id,
                    record.owner_work_item_id,
                    record.phase.as_str(),
                    record.verification_target,
                    record.recovery_policy,
                    record.command_digest,
                    record.created_at.to_rfc3339(),
                ],
            )?;
            tx.query_row(
                "SELECT operation_id, owner_turn_id, owner_work_item_id, phase,
                         verification_target, recovery_policy, command_digest,
                         created_at, updated_at
                 FROM destructive_operations WHERE operation_id = ?1",
                [&record.operation_id],
                decode_operation,
            )
            .map_err(Into::into)
        })
    }

    pub fn by_id(&self, operation_id: &str) -> Result<Option<DestructiveOperationRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT operation_id, owner_turn_id, owner_work_item_id, phase,
                        verification_target, recovery_policy, command_digest,
                        created_at, updated_at
                 FROM destructive_operations WHERE operation_id = ?1",
                [operation_id],
                decode_operation,
            )
            .optional()
            .map_err(Into::into)
    }

    /// CAS transition used immediately before the executor leaves this daemon.
    pub fn transition(
        &self,
        operation_id: &str,
        expected: DestructiveOperationPhase,
        next: DestructiveOperationPhase,
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            Ok(tx.execute(
                "UPDATE destructive_operations
                     SET phase = ?1, updated_at = ?2
                     WHERE operation_id = ?3 AND phase = ?4",
                params![
                    next.as_str(),
                    Utc::now().to_rfc3339(),
                    operation_id,
                    expected.as_str()
                ],
            )? == 1)
        })
    }

    pub fn pending_verification(&self) -> Result<Vec<DestructiveOperationRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT operation_id, owner_turn_id, owner_work_item_id, phase,
                    verification_target, recovery_policy, command_digest,
                    created_at, updated_at
             FROM destructive_operations
             WHERE phase = 'daemon_interrupted' AND recovery_policy = 'verify_only'
             ORDER BY updated_at, operation_id",
        )?;
        let operations = statement
            .query_map([], decode_operation)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(operations)
    }
}

fn decode_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<DestructiveOperationRecord> {
    let created_at = row.get::<_, String>(7)?.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let updated_at = row.get::<_, String>(8)?.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let phase = DestructiveOperationPhase::parse(&row.get::<_, String>(3)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })?;
    Ok(DestructiveOperationRecord {
        operation_id: row.get(0)?,
        owner_turn_id: row.get(1)?,
        owner_work_item_id: row.get(2)?,
        command_digest: row.get(6)?,
        phase,
        verification_target: row.get(4)?,
        recovery_policy: row.get(5)?,
        created_at,
        updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TurnRecord;

    fn runtime_db() -> Result<(tempfile::TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        Ok((dir, db))
    }

    fn durable_turn(db: &RuntimeDb, turn_id: &str) -> Result<()> {
        db.turn_records()
            .upsert(&TurnRecord::new("agent-a", turn_id, 1))?;
        Ok(())
    }

    fn request(
        operation_id: &str,
        owner_turn_id: &str,
        work_item_id: Option<&str>,
    ) -> DestructiveOperationRecord {
        let now = Utc::now();
        DestructiveOperationRecord {
            operation_id: operation_id.into(),
            owner_turn_id: owner_turn_id.into(),
            owner_work_item_id: work_item_id.map(str::to_string),
            command_digest: command_digest("systemctl --user restart holon.service"),
            phase: DestructiveOperationPhase::Planned,
            verification_target: "holon.service active".into(),
            recovery_policy: "verify_only".into(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn plan_and_schedule_fences_turn_and_does_not_redispatch() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        durable_turn(&db, "turn-a")?;
        let repo = db.destructive_operations();

        let first = repo.plan_and_schedule(&request("op-a", "turn-a", Some("work-a")))?;
        assert!(first.should_dispatch);
        assert_eq!(first.record.phase, DestructiveOperationPhase::Scheduled);

        // The fence is written onto the owning turn.
        let turn = db.turn_records().by_id(Some("agent-a"), "turn-a")?.unwrap();
        assert_eq!(turn.destructive_operation_id.as_deref(), Some("op-a"));

        // A repeat of the same id never returns a dispatchable plan.
        let repeat = repo.plan_and_schedule(&request("op-a", "turn-a", Some("work-a")))?;
        assert!(!repeat.should_dispatch);
        assert_eq!(repeat.record.operation_id, "op-a");
        let count: i64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM destructive_operations",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn plan_and_schedule_blocks_second_operation_for_same_work_item() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        durable_turn(&db, "turn-a")?;
        durable_turn(&db, "turn-b")?;
        let repo = db.destructive_operations();
        assert!(
            repo.plan_and_schedule(&request("op-a", "turn-a", Some("work-a")))?
                .should_dispatch
        );

        // A replay that mints a new id for the same fence must not dispatch again.
        let replay = repo.plan_and_schedule(&request("op-b", "turn-b", Some("work-a")))?;
        assert!(!replay.should_dispatch);
        assert_eq!(replay.record.operation_id, "op-a");

        // A different WorkItem is unaffected.
        assert!(
            repo.plan_and_schedule(&request("op-c", "turn-b", Some("work-b")))?
                .should_dispatch
        );
        Ok(())
    }

    #[test]
    fn plan_and_schedule_rejects_conflicting_rebinding() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        durable_turn(&db, "turn-a")?;
        durable_turn(&db, "turn-b")?;
        let repo = db.destructive_operations();
        repo.plan_and_schedule(&request("op-a", "turn-a", Some("work-a")))?;

        assert!(repo
            .plan_and_schedule(&request("op-a", "turn-b", None))
            .is_err());
        assert!(repo
            .plan_and_schedule(&request("op-b", "turn-a", None))
            .is_err());
        Ok(())
    }

    #[test]
    fn plan_and_schedule_rearms_after_dispatch_failure() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        durable_turn(&db, "turn-a")?;
        let repo = db.destructive_operations();
        repo.plan_and_schedule(&request("op-a", "turn-a", Some("work-a")))?;
        assert!(repo.transition(
            "op-a",
            DestructiveOperationPhase::Scheduled,
            DestructiveOperationPhase::Planned,
        )?);

        let retry = repo.plan_and_schedule(&request("op-a", "turn-a", Some("work-a")))?;
        assert!(retry.should_dispatch);
        assert_eq!(retry.record.phase, DestructiveOperationPhase::Scheduled);
        Ok(())
    }

    #[test]
    fn transition_is_compare_and_swap() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        durable_turn(&db, "turn-a")?;
        let repo = db.destructive_operations();
        repo.plan_and_schedule(&request("op-a", "turn-a", Some("work-a")))?;

        assert!(!repo.transition(
            "op-a",
            DestructiveOperationPhase::Planned,
            DestructiveOperationPhase::Scheduled,
        )?);
        assert!(repo.transition(
            "op-a",
            DestructiveOperationPhase::Scheduled,
            DestructiveOperationPhase::DaemonInterrupted,
        )?);
        assert!(!repo.transition(
            "op-a",
            DestructiveOperationPhase::Scheduled,
            DestructiveOperationPhase::DaemonInterrupted,
        )?);
        assert_eq!(
            repo.by_id("op-a")?.unwrap().phase,
            DestructiveOperationPhase::DaemonInterrupted
        );
        assert!(repo.transition(
            "op-a",
            DestructiveOperationPhase::DaemonInterrupted,
            DestructiveOperationPhase::Verified,
        )?);
        Ok(())
    }

    #[test]
    fn plan_or_get_is_insert_once() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let repo = db.destructive_operations();
        let inserted = repo.plan_or_get(&request("op-a", "turn-a", None))?;
        assert_eq!(inserted.phase, DestructiveOperationPhase::Planned);
        let again = repo.plan_or_get(&request("op-a", "turn-b", None))?;
        assert_eq!(again.owner_turn_id, "turn-a");
        assert_eq!(repo.by_id("missing")?, None);
        Ok(())
    }
}
