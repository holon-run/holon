use super::*;
use crate::{
    diagnostics::resource_io::{assert_retryable_resource_error, ResourceReadMaterial},
    runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        repositories::upsert_task_tx,
        task_snapshot::TaskSnapshotError,
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, TaskKind, TaskRecord, TaskStatus,
    },
};

struct Fixture {
    _home: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
}

impl Fixture {
    // Deliberately State-only: Task preparation remains a per-test choice.
    fn state_only() -> Result<Self> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let identity = AgentIdentityRecord::new(
            "task-fixture-agent",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        let scope = InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        db.transaction(|tx| {
            upsert_agent_identity_tx(tx, &identity)?;
            upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
            Ok(())
        })?;
        Ok(Self {
            _home: home,
            db,
            scope,
        })
    }

    fn prepare(
        &self,
        scenario: &str,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<TaskCurrentSnapshot> {
        self.db.transaction(|tx| {
            prepare_current_task_fixture_tx(tx, scenario, &self.scope, request, ledger)
        })
    }

    fn task_and_qualification_counts(&self) -> Result<Vec<u64>> {
        let connection = self.db.connection()?;
        [
            "tasks",
            "task_domain_current",
            "task_domain_records",
            "task_domain_captures",
            "task_domain_eligibility",
            "task_domain_import_inventory",
            "task_domain_seed_recovery_current",
        ]
        .iter()
        .map(|table| {
            Ok(connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
        })
        .collect()
    }
}

fn request(record_limit: usize) -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit,
        byte_limit: 8 * 1024 * 1024,
    }
}

fn assert_scenario_and_scope(error: &anyhow::Error, scenario: &str, scope: &InputEvidenceScope) {
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(scenario), "{diagnostic}");
    assert!(diagnostic.contains(&scope.agent_id), "{diagnostic}");
    assert!(
        diagnostic.contains(&format!("incarnation {}", scope.identity_incarnation)),
        "{diagnostic}"
    );
}

#[test]
fn state_only_stays_cold_until_explicit_prepare_and_can_prepare_again() -> Result<()> {
    let f = Fixture::state_only()?;
    assert_eq!(f.task_and_qualification_counts()?, vec![0; 7]);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for limit in [4096, 65536] {
        let current = f.prepare("explicit opt-in", &request(limit), &ledger)?;
        assert_eq!(current.resident.scope(), &f.scope);
        assert!(current.resident.coverage().active_task_ids.is_empty());
        assert!(current.resident.coverage().unsettled_result_ids.is_empty());
        assert!(current.resident.coverage().terminal.keys.is_empty());
        // Only the explicit domain metadata is installed, never Task/qualification data.
        assert_eq!(
            f.task_and_qualification_counts()?,
            vec![0, 1, 0, 0, 0, 0, 0]
        );
        drop(current);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn unqualified_task_is_not_adopted_and_rejection_preserves_original_cause() -> Result<()> {
    let f = Fixture::state_only()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    drop(f.prepare("empty preparation", &request(4096), &ledger)?);
    let at = "2026-11-17T00:00:00Z".parse()?;
    let task = TaskRecord {
        id: "unqualified-history".into(),
        agent_id: f.scope.agent_id.clone(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("raw history is not authority".into()),
        detail: None,
        recovery: None,
    };
    f.db.transaction(|tx| upsert_task_tx(tx, &task))?;
    let before = f.task_and_qualification_counts()?;
    let mut required = request(4096);
    let key = TaskKey::new(TaskFamily::Task, &task.id);
    required.capture.required_keys.insert(key.clone());
    let error = f
        .prepare("unqualified negative", &required, &ledger)
        .unwrap_err();
    assert_scenario_and_scope(&error, "unqualified negative", &f.scope);
    assert!(
        matches!(error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::NeedsPreparation(actual)) if actual == &key),
        "{error:#}"
    );
    assert_eq!(f.task_and_qualification_counts()?, before);
    assert_eq!(
        f.db.tasks().latest(&task.id)?.unwrap().summary,
        task.summary
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn budget_rejection_is_not_retried_or_expanded_and_keeps_its_cause() -> Result<()> {
    let f = Fixture::state_only()?;
    let before = f.task_and_qualification_counts()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let load = request(4096);
    let header = ResourceReadMaterial::new(&ledger, &f.scope)?.finish()?;
    // Exercise real SQL preparation under the existing material fault budget.
    // A header-only allowance cannot admit the next read; no helper mock is used.
    let reject = |helper| {
        f.db.transaction(|tx| {
            let material =
                ResourceReadMaterial::with_test_limits(&ledger, &f.scope, header.0, header.1)?;
            material.read(|| {
                if helper {
                    prepare_current_task_fixture_tx(tx, "budget negative", &f.scope, &load, &ledger)
                } else {
                    prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &load, &ledger)?
                        .require_current()
                }
            })
        })
    };
    let original = reject(false).unwrap_err();
    let error = reject(true).unwrap_err();
    assert_scenario_and_scope(&error, "budget negative", &f.scope);
    assert_retryable_resource_error(&error);
    assert_eq!(
        error.root_cause().to_string(),
        original.root_cause().to_string()
    );
    assert_eq!(f.task_and_qualification_counts()?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn retired_incarnation_and_no_state_remain_negative_without_task_writes() -> Result<()> {
    let f = Fixture::state_only()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let before = f.task_and_qualification_counts()?;
    for scope in [
        InputEvidenceScope {
            identity_incarnation: f.scope.identity_incarnation + 1,
            ..f.scope.clone()
        },
        InputEvidenceScope {
            agent_id: "no-state-or-identity".into(),
            ..f.scope.clone()
        },
    ] {
        let error =
            f.db.transaction(|tx| {
                prepare_current_task_fixture_tx(
                    tx,
                    "retired negative",
                    &scope,
                    &request(4096),
                    &ledger,
                )
            })
            .unwrap_err();
        assert_scenario_and_scope(&error, "retired negative", &scope);
        assert!(matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::RetiredScope)
        ));
        assert_eq!(f.task_and_qualification_counts()?, before);
    }
    Ok(())
}
