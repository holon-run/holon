use std::{collections::BTreeMap, path::Path};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::execution_protocol::{
    self, AdmitExecution, ExecutionAttempt, ExecutionBinding, ExecutionOutcomeRecord,
    ExecutionProtocolState, ExecutionTransition, InterruptExecution, SettleExecution,
    WorkItemExecutionRecord, WorkItemExecutionState,
};

const FIXTURE_SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS execution_fixture_partitions (
  agent_id TEXT PRIMARY KEY,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS execution_fixture_work_items (
  agent_id TEXT NOT NULL,
  work_item_id TEXT NOT NULL,
  source_revision INTEGER NOT NULL CHECK (source_revision > 0),
  generation INTEGER NOT NULL CHECK (generation >= 0),
  lifecycle_state TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (agent_id, work_item_id)
);

CREATE TABLE IF NOT EXISTS execution_fixture_attempts (
  agent_id TEXT NOT NULL,
  attempt_id TEXT NOT NULL,
  lifecycle_state TEXT NOT NULL,
  source_identity TEXT NOT NULL,
  source_generation INTEGER NOT NULL CHECK (source_generation >= 0),
  recovery_of_attempt_id TEXT,
  terminal_outcome_id TEXT,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (agent_id, attempt_id),
  UNIQUE (agent_id, terminal_outcome_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS execution_fixture_one_open_attempt
  ON execution_fixture_attempts(agent_id)
  WHERE lifecycle_state = 'open';

CREATE TABLE IF NOT EXISTS execution_fixture_outcomes (
  agent_id TEXT NOT NULL,
  outcome_id TEXT NOT NULL,
  attempt_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (agent_id, outcome_id),
  UNIQUE (agent_id, attempt_id),
  FOREIGN KEY (agent_id, attempt_id)
    REFERENCES execution_fixture_attempts(agent_id, attempt_id)
);

CREATE TABLE IF NOT EXISTS execution_fixture_command_results (
  agent_id TEXT NOT NULL,
  command_kind TEXT NOT NULL,
  command_identity TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  result_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, command_kind, command_identity)
);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixtureFaultPoint {
    AfterStateWrites,
    BeforeCommit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FixtureCommandResult {
    references: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureCommit {
    applied: bool,
    replayed: bool,
    transition: ExecutionTransition,
}

struct ExecutionProtocolFixtureRepository {
    connection: Connection,
}

impl ExecutionProtocolFixtureRepository {
    fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(FIXTURE_SCHEMA)?;
        Ok(Self { connection })
    }

    fn initialize(&mut self, state: &ExecutionProtocolState) -> Result<()> {
        execution_protocol::assert_invariants(state)
            .map_err(|error| anyhow!("invalid execution fixture state: {error}"))?;
        let transaction = self.connection.transaction()?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM execution_fixture_partitions WHERE agent_id = ?1
             )",
            [&state.agent_id],
            |row| row.get(0),
        )?;
        if exists {
            bail!(
                "execution fixture partition for agent {} already exists",
                state.agent_id
            );
        }
        persist_state(&transaction, state)?;
        transaction.commit()?;
        Ok(())
    }

    fn load(&mut self, agent_id: &str) -> Result<ExecutionProtocolState> {
        let transaction = self.connection.transaction()?;
        let state = load_state(&transaction, agent_id)?;
        transaction.commit()?;
        Ok(state)
    }

    fn admit(
        &mut self,
        agent_id: &str,
        command: &AdmitExecution,
        fault: Option<FixtureFaultPoint>,
    ) -> Result<FixtureCommit> {
        self.commit(
            agent_id,
            "admit_execution",
            &command.attempt.attempt_id,
            command,
            fault,
            |state| execution_protocol::admit_execution(state, command),
        )
    }

    fn settle(
        &mut self,
        agent_id: &str,
        command: &SettleExecution,
        fault: Option<FixtureFaultPoint>,
    ) -> Result<FixtureCommit> {
        self.commit(
            agent_id,
            "settle_execution",
            &command.outcome.outcome_id,
            command,
            fault,
            |state| execution_protocol::settle_execution(state, command),
        )
    }

    fn interrupt(
        &mut self,
        agent_id: &str,
        command: &InterruptExecution,
        fault: Option<FixtureFaultPoint>,
    ) -> Result<FixtureCommit> {
        self.commit(
            agent_id,
            "interrupt_execution",
            &command.outcome_id,
            command,
            fault,
            |state| execution_protocol::interrupt_execution(state, command),
        )
    }

    fn commit<T: Serialize>(
        &mut self,
        agent_id: &str,
        command_kind: &str,
        command_identity: &str,
        command: &T,
        fault: Option<FixtureFaultPoint>,
        reduce: impl FnOnce(&ExecutionProtocolState) -> Result<ExecutionTransition, String>,
    ) -> Result<FixtureCommit> {
        let payload_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(command)?));
        let transaction = self.connection.transaction()?;
        if let Some((stored_hash, result_json)) = transaction
            .query_row(
                "SELECT payload_hash, result_json
                 FROM execution_fixture_command_results
                 WHERE agent_id = ?1
                   AND command_kind = ?2
                   AND command_identity = ?3",
                params![agent_id, command_kind, command_identity],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if stored_hash != payload_hash {
                bail!(
                    "execution fixture command identity conflict for {command_kind} {command_identity}"
                );
            }
            let result: FixtureCommandResult = serde_json::from_str(&result_json)?;
            let state = load_state(&transaction, agent_id)?;
            transaction.commit()?;
            return Ok(FixtureCommit {
                applied: false,
                replayed: true,
                transition: ExecutionTransition {
                    state,
                    references: result.references,
                },
            });
        }

        let state = load_state(&transaction, agent_id)?;
        let transition = reduce(&state)
            .map_err(|error| anyhow!("execution fixture command rejected: {error}"))?;
        persist_state(&transaction, &transition.state)?;
        inject_fixture_fault(fault, FixtureFaultPoint::AfterStateWrites)?;
        let result = FixtureCommandResult {
            references: transition.references.clone(),
        };
        transaction.execute(
            "INSERT INTO execution_fixture_command_results (
               agent_id, command_kind, command_identity,
               payload_hash, result_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                agent_id,
                command_kind,
                command_identity,
                payload_hash,
                serde_json::to_string(&result)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
        inject_fixture_fault(fault, FixtureFaultPoint::BeforeCommit)?;
        transaction.commit()?;
        Ok(FixtureCommit {
            applied: true,
            replayed: false,
            transition,
        })
    }
}

fn inject_fixture_fault(
    configured: Option<FixtureFaultPoint>,
    current: FixtureFaultPoint,
) -> Result<()> {
    if configured == Some(current) {
        bail!("injected execution fixture fault at {current:?}");
    }
    Ok(())
}

fn persist_state(tx: &Transaction<'_>, state: &ExecutionProtocolState) -> Result<()> {
    execution_protocol::assert_invariants(state)
        .map_err(|error| anyhow!("invalid execution fixture state: {error}"))?;
    tx.execute(
        "INSERT INTO execution_fixture_partitions (agent_id, updated_at)
         VALUES (?1, ?2)
         ON CONFLICT(agent_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![state.agent_id, Utc::now().to_rfc3339()],
    )?;
    tx.execute(
        "DELETE FROM execution_fixture_outcomes WHERE agent_id = ?1",
        [&state.agent_id],
    )?;
    tx.execute(
        "DELETE FROM execution_fixture_attempts WHERE agent_id = ?1",
        [&state.agent_id],
    )?;
    tx.execute(
        "DELETE FROM execution_fixture_work_items WHERE agent_id = ?1",
        [&state.agent_id],
    )?;

    for (work_item_id, work_record) in &state.work_items {
        tx.execute(
            "INSERT INTO execution_fixture_work_items (
               agent_id, work_item_id, source_revision, generation,
               lifecycle_state, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                state.agent_id,
                work_item_id,
                to_i64(work_record.source_revision)?,
                to_i64(work_record.generation())?,
                work_item_state_token(&work_record.state),
                serde_json::to_string(work_record)?,
            ],
        )?;
    }
    for attempt in state.attempts.values() {
        tx.execute(
            "INSERT INTO execution_fixture_attempts (
               agent_id, attempt_id, lifecycle_state,
               source_identity, source_generation, recovery_of_attempt_id,
               terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                state.agent_id,
                attempt.attempt_id,
                enum_token(&attempt.state)?,
                serde_json::to_string(&attempt.source.identity)?,
                to_i64(attempt.source.generation)?,
                attempt.recovery_of_attempt_id,
                attempt.terminal_outcome_id,
                serde_json::to_string(attempt)?,
            ],
        )?;
    }
    for outcome in state.outcomes.values() {
        tx.execute(
            "INSERT INTO execution_fixture_outcomes (
               agent_id, outcome_id, attempt_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                state.agent_id,
                outcome.outcome_id,
                outcome.attempt_id,
                serde_json::to_string(outcome)?,
            ],
        )?;
    }
    Ok(())
}

fn load_state(tx: &Transaction<'_>, agent_id: &str) -> Result<ExecutionProtocolState> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM execution_fixture_partitions WHERE agent_id = ?1
         )",
        [agent_id],
        |row| row.get(0),
    )?;
    if !exists {
        bail!("execution fixture partition for agent {agent_id} is not initialized");
    }
    let state = ExecutionProtocolState {
        agent_id: agent_id.to_owned(),
        attempts: load_payload_map(
            tx,
            "SELECT attempt_id, payload_json
             FROM execution_fixture_attempts
             WHERE agent_id = ?1",
            agent_id,
        )?,
        work_items: load_payload_map(
            tx,
            "SELECT work_item_id, payload_json
             FROM execution_fixture_work_items
             WHERE agent_id = ?1",
            agent_id,
        )?,
        outcomes: load_payload_map(
            tx,
            "SELECT outcome_id, payload_json
             FROM execution_fixture_outcomes
             WHERE agent_id = ?1",
            agent_id,
        )?,
    };
    execution_protocol::assert_invariants(&state)
        .map_err(|error| anyhow!("stored execution fixture state is invalid: {error}"))?;
    Ok(state)
}

fn load_payload_map<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
    agent_id: &str,
) -> Result<BTreeMap<String, T>> {
    let mut statement = tx.prepare(sql)?;
    let result: Result<BTreeMap<String, T>> = statement
        .query_map([agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (id, payload) = row?;
            let value = serde_json::from_str(&payload)
                .with_context(|| format!("decoding execution fixture record {id}"))?;
            Ok((id, value))
        })
        .collect();
    result
}

fn enum_token<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("execution fixture enum did not serialize to a string"))
}

fn work_item_state_token(state: &WorkItemExecutionState) -> &'static str {
    match state {
        WorkItemExecutionState::Runnable { .. } => "runnable",
        WorkItemExecutionState::InFlight { .. } => "in_flight",
        WorkItemExecutionState::Waiting { .. } => "waiting",
        WorkItemExecutionState::Paused { .. } => "paused",
        WorkItemExecutionState::NeedsRepair { .. } => "needs_repair",
        WorkItemExecutionState::Terminal { .. } => "terminal",
    }
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("execution fixture generation exceeds SQLite INTEGER")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::execution_protocol::{
        AdmittedFences, ExecutionAttemptState, ExecutionOrigin, ExecutionPriority,
        ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
    };

    fn work_item_record(state: WorkItemExecutionState) -> WorkItemExecutionRecord {
        WorkItemExecutionRecord {
            source_revision: state.generation(),
            state,
        }
    }

    fn fixture_attempt(
        id: &str,
        source_revision: u64,
        work_item_generation: u64,
        recovery_of: Option<&str>,
    ) -> ExecutionAttempt {
        ExecutionAttempt {
            attempt_id: id.into(),
            agent_id: "agent-a".into(),
            source_message_id: Some(format!("message:{id}")),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::WorkItemContinuation {
                    work_item_id: "work-a".into(),
                },
                generation: source_revision,
            },
            binding: ExecutionBinding::WorkItem {
                work_item_id: "work-a".into(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::System,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision,
                work_item_source_revision: Some(source_revision),
                work_item_generation: Some(work_item_generation),
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: None,
            recovery_of_attempt_id: recovery_of.map(str::to_owned),
            terminal_outcome_id: None,
            admitted_at: format!("2026-08-01T00:0{work_item_generation}:00Z"),
            terminal_at: None,
        }
    }

    fn fixture_state() -> ExecutionProtocolState {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        state
    }

    #[test]
    fn fixture_restart_interrupts_attempt_and_allows_second_attempt() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("execution-fixture.sqlite");
        {
            let mut repository = ExecutionProtocolFixtureRepository::open(&path)?;
            repository.initialize(&fixture_state())?;
            repository.admit(
                "agent-a",
                &AdmitExecution {
                    attempt: fixture_attempt("attempt-1", 1, 1, None),
                },
                None,
            )?;
            repository.interrupt(
                "agent-a",
                &InterruptExecution {
                    attempt_id: "attempt-1".into(),
                    outcome_id: "outcome-1".into(),
                    reason: "runtime_restart".into(),
                    interrupted_at: "2026-08-01T00:02:00Z".into(),
                },
                None,
            )?;
        }

        let mut reopened = ExecutionProtocolFixtureRepository::open(&path)?;
        let recovered = reopened.load("agent-a")?;
        assert!(recovered.open_attempt().is_none());
        let admitted = reopened.admit(
            "agent-a",
            &AdmitExecution {
                attempt: fixture_attempt("attempt-2", 1, 2, Some("attempt-1")),
            },
            None,
        )?;
        assert_eq!(
            admitted
                .transition
                .state
                .open_attempt()
                .map(|attempt| attempt.attempt_id.as_str()),
            Some("attempt-2")
        );
        Ok(())
    }

    #[test]
    fn fixture_command_is_idempotent_and_conflicting_payload_is_rejected() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("execution-fixture.sqlite");
        let mut repository = ExecutionProtocolFixtureRepository::open(&path)?;
        repository.initialize(&fixture_state())?;
        let command = AdmitExecution {
            attempt: fixture_attempt("attempt-1", 1, 1, None),
        };
        assert!(repository.admit("agent-a", &command, None)?.applied);
        assert!(repository.admit("agent-a", &command, None)?.replayed);

        let mut conflicting = command;
        conflicting.attempt.provenance.priority = ExecutionPriority::Interject;
        assert!(repository
            .admit("agent-a", &conflicting, None)
            .unwrap_err()
            .to_string()
            .contains("identity conflict"));
        Ok(())
    }

    #[test]
    fn fixture_fault_rolls_back_state_and_command_result() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("execution-fixture.sqlite");
        let mut repository = ExecutionProtocolFixtureRepository::open(&path)?;
        let initial = fixture_state();
        repository.initialize(&initial)?;
        let command = AdmitExecution {
            attempt: fixture_attempt("attempt-1", 1, 1, None),
        };
        assert!(repository
            .admit("agent-a", &command, Some(FixtureFaultPoint::BeforeCommit),)
            .is_err());
        assert_eq!(repository.load("agent-a")?, initial);
        assert!(repository.admit("agent-a", &command, None)?.applied);
        Ok(())
    }

    #[test]
    fn fixture_settlement_persists_terminal_outcome() -> Result<()> {
        use crate::domain::execution_protocol::{ExecutionOutcome, WorkItemOutcome};

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("execution-fixture.sqlite");
        let mut repository = ExecutionProtocolFixtureRepository::open(&path)?;
        repository.initialize(&fixture_state())?;
        repository.admit(
            "agent-a",
            &AdmitExecution {
                attempt: fixture_attempt("attempt-1", 1, 1, None),
            },
            None,
        )?;
        let commit = repository.settle(
            "agent-a",
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "outcome-1".into(),
                    attempt_id: "attempt-1".into(),
                    outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Complete {
                        completion: "brief-1".into(),
                    }),
                    created_at: "2026-08-01T00:02:00Z".into(),
                },
            },
            None,
        )?;
        assert!(matches!(
            commit.transition.state.work_items["work-a"].state,
            WorkItemExecutionState::Terminal { .. }
        ));
        Ok(())
    }

    #[test]
    fn fixture_full_crash_recovery_lifecycle_through_persistence() -> Result<()> {
        use crate::domain::execution_protocol::{
            AdmittedFences, CommandResult, ExecutionBinding, ExecutionOutcome, ExecutionProvenance,
            ExecutionSource, ExecutionSourceIdentity, WorkItemOutcome,
        };

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("execution-fixture-lifecycle.sqlite");

        // Phase 1: admit a WorkItem attempt, then crash (interrupt)
        {
            let mut repository = ExecutionProtocolFixtureRepository::open(&path)?;
            repository.initialize(&fixture_state())?;
            repository.admit(
                "agent-a",
                &AdmitExecution {
                    attempt: fixture_attempt("attempt-1", 1, 1, None),
                },
                None,
            )?;
            repository.interrupt(
                "agent-a",
                &InterruptExecution {
                    attempt_id: "attempt-1".into(),
                    outcome_id: "outcome-1".into(),
                    reason: "process_crash".into(),
                    interrupted_at: "2026-08-01T00:01:00Z".into(),
                },
                None,
            )?;
        }

        // Phase 2: reopen, verify state, run recovery command, then resume WorkItem
        let mut reopened = ExecutionProtocolFixtureRepository::open(&path)?;
        let recovered = reopened.load("agent-a")?;
        assert!(recovered.open_attempt().is_none());
        assert!(matches!(
            recovered.attempts["attempt-1"].state,
            crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
        ));

        // Recovery command: RuntimeRecovery → Command
        let recovery_attempt = ExecutionAttempt {
            attempt_id: "recovery-1".into(),
            agent_id: "agent-a".into(),
            source_message_id: None,
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::RuntimeRecovery {
                    recovery_id: "bootstrap-recovery".into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::Command,
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::RuntimeRecovery,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Background,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: crate::domain::execution_protocol::ExecutionAttemptState::Open,
            run_id: None,
            turn_id: None,
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: "2026-08-01T00:02:00Z".into(),
            terminal_at: None,
        };
        reopened.admit(
            "agent-a",
            &AdmitExecution {
                attempt: recovery_attempt,
            },
            None,
        )?;
        let recovery_settled = reopened.settle(
            "agent-a",
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "recovery-outcome-1".into(),
                    attempt_id: "recovery-1".into(),
                    outcome: ExecutionOutcome::Command(CommandResult::Applied {
                        references: vec!["recovered:attempt-1".into()],
                    }),
                    created_at: "2026-08-01T00:03:00Z".into(),
                },
            },
            None,
        )?;
        assert!(recovery_settled.transition.state.open_attempt().is_none());

        // Phase 3: resume WorkItem from interrupted state
        let resumed = reopened.admit(
            "agent-a",
            &AdmitExecution {
                attempt: fixture_attempt("attempt-2", 1, 2, Some("attempt-1")),
            },
            None,
        )?;
        assert_eq!(
            resumed
                .transition
                .state
                .open_attempt()
                .map(|a| a.attempt_id.as_str()),
            Some("attempt-2")
        );

        // Phase 4: settle the resumed WorkItem attempt
        let final_settled = reopened.settle(
            "agent-a",
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "outcome-2".into(),
                    attempt_id: "attempt-2".into(),
                    outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Complete {
                        completion: "brief-done".into(),
                    }),
                    created_at: "2026-08-01T00:04:00Z".into(),
                },
            },
            None,
        )?;
        assert!(matches!(
            final_settled.transition.state.work_items["work-a"].state,
            WorkItemExecutionState::Terminal { .. }
        ));
        assert!(final_settled.transition.state.open_attempt().is_none());
        Ok(())
    }
}
