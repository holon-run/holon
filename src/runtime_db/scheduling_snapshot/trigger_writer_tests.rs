use super::tests::{at, identity, ledger, legacy, request, scope, trigger};
use super::*;
use crate::runtime_db::{
    input_mutation_receipts::InputMutationResult,
    transitions::{
        ExternalTriggerMutationOutcome, InputMutationExecution, PreparedExternalTriggerTransition,
        PreparedInputTransition, TransitionCommit, TransitionFaultPoint,
    },
};
use crate::types::{CallbackIngressDisposition, ExternalTriggerRecord, ExternalTriggerStatus};

fn fixture(
    original: Option<ExternalTriggerRecord>,
) -> Result<(
    tempfile::TempDir,
    crate::runtime_db::RuntimeDb,
    InputEvidenceBudgetLedger,
)> {
    let (home, db) = legacy(true, |tx| {
        if let Some(record) = &original {
            repositories::upsert_external_trigger_tx(tx, record)?;
        }
        Ok(())
    })?;
    let budget = ledger()?;
    drop(db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)
    })?);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok((home, db, budget))
}
fn ensure_command(
    db: &crate::runtime_db::RuntimeDb,
    budget: &InputEvidenceBudgetLedger,
    nonce: &str,
    id: &str,
    fault: Option<TransitionFaultPoint>,
) -> Result<PreparedInputTransition> {
    let observed =
        db.external_triggers()
            .observe_default_for_writer(&scope(), &request(), budget)?;
    PreparedInputTransition::external_trigger(
        scope(),
        nonce.into(),
        at() + chrono::Duration::seconds(10),
        PreparedExternalTriggerTransition::ensure(
            observed,
            id.into(),
            "fresh-token".into(),
            at() + chrono::Duration::seconds(10),
            fault,
        ),
    )
}
fn reset_command(
    db: &crate::runtime_db::RuntimeDb,
    budget: &InputEvidenceBudgetLedger,
    nonce: &str,
    id: &str,
    fault: Option<TransitionFaultPoint>,
) -> Result<PreparedInputTransition> {
    let observed =
        db.external_triggers()
            .observe_default_for_writer(&scope(), &request(), budget)?;
    PreparedInputTransition::external_trigger(
        scope(),
        nonce.into(),
        at() + chrono::Duration::seconds(11),
        PreparedExternalTriggerTransition::reset(
            observed,
            id.into(),
            "reset-token".into(),
            at() + chrono::Duration::seconds(11),
            fault,
        ),
    )
}
fn revoke_command(
    db: &crate::runtime_db::RuntimeDb,
    budget: &InputEvidenceBudgetLedger,
    id: &str,
    nonce: &str,
) -> Result<PreparedInputTransition> {
    let observed =
        db.external_triggers()
            .observe_existing_for_cleanup(&scope(), id, &request(), budget)?;
    let command = PreparedExternalTriggerTransition::revoke(
        observed,
        at() + chrono::Duration::seconds(12),
        None,
    )?;
    PreparedInputTransition::external_trigger(
        scope(),
        nonce.into(),
        at() + chrono::Duration::seconds(12),
        command,
    )
}
fn commit(
    db: &crate::runtime_db::RuntimeDb,
    budget: &InputEvidenceBudgetLedger,
    p: &PreparedInputTransition,
    load: &SchedulingDomainLoadRequest,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: load.record_limit,
        byte_limit: load.byte_limit,
    };
    let tasks = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: load.record_limit,
        byte_limit: load.byte_limit,
    };
    db.transitions_in_scope(&scope())
        .commit_prepared_transition_with_domain_requests(p, &work, budget, &tasks, load)
}
fn side_effects(db: &crate::runtime_db::RuntimeDb) -> Result<(usize, usize, usize, usize)> {
    db.transaction(|tx|Ok(tx.query_row("SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries),(SELECT COUNT(*) FROM turn_records),(SELECT COUNT(*) FROM dispatch_memory_checkpoints)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?))
}
fn saved(db: &crate::runtime_db::RuntimeDb) -> Result<(String, String, String, usize)> {
    db.transaction(|tx|Ok((
        tx.query_row("SELECT coverage_json FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope().agent_id,scope().identity_incarnation],|r|r.get(0))?,
        tx.query_row("SELECT COALESCE(json_group_array(payload_json),'[]') FROM (SELECT payload_json FROM external_triggers ORDER BY external_trigger_id)",[],|r|r.get(0))?,
        tx.query_row("SELECT COALESCE(json_group_array(json_array(family,record_id,revision,availability,content_digest,projection_digest)),'[]') FROM (SELECT * FROM scheduling_domain_records ORDER BY agent_id,identity_incarnation,family,record_id)",[],|r|r.get(0))?,
        tx.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE family='external_trigger'",[],|r|r.get(0))?)))
}
#[test]
fn external_trigger_ensure_original_ack_has_actual_scheduling_only_and_exact_replay() -> Result<()>
{
    let (_home, db, budget) = fixture(None)?;
    let before = side_effects(&db)?;
    let p = ensure_command(&db, &budget, "trigger-first", "new-trigger", None)?;
    let InputMutationExecution::Committed { value, receipt } =
        commit(&db, &budget, &p, &request())?
    else {
        anyhow::bail!("first ensure replayed")
    };
    let InputMutationResult::ExternalTrigger { outcome } = &receipt.outcome.result else {
        anyhow::bail!("wrong ACK")
    };
    let ExternalTriggerMutationOutcome::Ensured {
        record,
        created,
        revoked,
    } = outcome.as_ref()
    else {
        anyhow::bail!("wrong operation")
    };
    assert!(*created && revoked.is_none());
    assert_eq!(
        db.external_triggers().latest("new-trigger")?,
        Some(record.clone())
    );
    assert_eq!(value.effects.external_triggers, vec![record.clone()]);
    assert_eq!(
        value.effects.scheduling_write_keys,
        BTreeSet::from([SchedulingKey::new(
            SchedulingFamily::ExternalTrigger,
            "new-trigger"
        )])
    );
    assert!(value.effects.scheduling_delta_requires_prepared_domain);
    assert!(value.effects.messages.is_empty() && value.effects.agent_state.is_none());
    assert!(value.effects.work_delta.is_none() && value.effects.task_delta.is_none());
    assert_eq!(side_effects(&db)?, before);
    let exact = saved(&db)?;
    assert!(
        matches!(commit(&db,&budget,&p,&request())?,InputMutationExecution::Replayed{receipt:original,..} if original==receipt)
    );
    assert_eq!(saved(&db)?, exact);
    drop((p, value, receipt));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn default_noop_ignores_unused_new_id_collision_and_replays_original_record_after_accounting(
) -> Result<()> {
    let original = trigger();
    let (_home, db, budget) = fixture(Some(original.clone()))?;
    let mut foreign = original.clone();
    foreign.external_trigger_id = "unused-foreign".into();
    foreign.target_agent_id = "foreign-owner".into();
    foreign.status = ExternalTriggerStatus::Revoked;
    foreign.revoked_at = Some(at());
    db.transaction(|tx| repositories::upsert_external_trigger_tx(tx, &foreign))?;
    let p = ensure_command(
        &db,
        &budget,
        "original-default-noop",
        &foreign.external_trigger_id,
        None,
    )?;
    let InputMutationExecution::Committed { value, receipt } =
        commit(&db, &budget, &p, &request())?
    else {
        anyhow::bail!("noop replayed")
    };
    assert!(!value.applied && value.effects.scheduling_write_keys.is_empty());
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::ExternalTrigger{outcome} if matches!(outcome.as_ref(),ExternalTriggerMutationOutcome::Ensured{record,created:false,revoked:None} if record==&original))
    );
    let observed = db.external_triggers().observe_existing_for_cleanup(
        &scope(),
        &original.external_trigger_id,
        &request(),
        &budget,
    )?;
    let account = PreparedInputTransition::external_trigger(
        scope(),
        "later-account".into(),
        at() + chrono::Duration::seconds(20),
        PreparedExternalTriggerTransition::account(
            observed,
            at() + chrono::Duration::seconds(20),
            CallbackIngressDisposition::Ignored,
            None,
        )?,
    )?;
    drop(commit(&db, &budget, &account, &request())?);
    assert_eq!(
        db.external_triggers()
            .latest(&original.external_trigger_id)?
            .unwrap()
            .delivery_count,
        1
    );
    let exact = saved(&db)?;
    assert!(
        matches!(commit(&db,&budget,&p,&request())?,InputMutationExecution::Replayed{receipt:again,..} if again==receipt)
    );
    assert_eq!(saved(&db)?, exact);
    assert_eq!(
        db.external_triggers()
            .latest(&foreign.external_trigger_id)?,
        Some(foreign)
    );
    let changed = ensure_command(
        &db,
        &budget,
        "original-default-noop",
        "changed-unused",
        None,
    )?;
    assert_ne!(p.descriptor(), changed.descriptor());
    assert!(commit(&db, &budget, &changed, &request()).is_err());
    assert_eq!(saved(&db)?, exact);
    drop((account, changed, p, value, receipt));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn missing_token_ensure_replaces_original_default_and_advances_domain_once() -> Result<()> {
    let mut original = trigger();
    original.token = None;
    let (_home, db, budget) = fixture(Some(original.clone()))?;
    let before = db.transaction(|tx| {
        load_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    let p = ensure_command(&db, &budget, "replace-missing-token", "replacement", None)?;
    let InputMutationExecution::Committed { value, receipt } =
        commit(&db, &budget, &p, &request())?
    else {
        anyhow::bail!("replace replayed")
    };
    let delta = value.effects.scheduling_delta.as_ref().unwrap();
    assert_eq!(delta.revision, before.resident.coverage().revision + 1);
    assert_eq!(value.effects.scheduling_write_keys.len(), 2);
    assert_eq!(
        db.external_triggers()
            .latest(&original.external_trigger_id)?
            .unwrap()
            .status,
        ExternalTriggerStatus::Revoked
    );
    assert_eq!(
        before
            .resident
            .capture(&request().capture)?
            .active_default_trigger()?
            .unwrap(),
        &original
    );
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::ExternalTrigger{outcome} if matches!(outcome.as_ref(),ExternalTriggerMutationOutcome::Ensured{record,created:true,revoked:Some(old)} if record.external_trigger_id=="replacement"&&old.external_trigger_id==original.external_trigger_id))
    );
    drop((p, before, value, receipt));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn trigger_reset_faults_roll_back_both_canonical_rows_aliases_and_whole_receipt() -> Result<()> {
    for fault in [
        TransitionFaultPoint::AfterValidation,
        TransitionFaultPoint::AfterCanonicalWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        let (_home, db, budget) = fixture(Some(trigger()))?;
        let p = reset_command(&db, &budget, "reset-fault", "reset-new", Some(fault))?;
        let exact = saved(&db)?;
        let observed = budget.report()?.observed_bytes;
        assert!(commit(&db, &budget, &p, &request()).is_err());
        assert_eq!(saved(&db)?, exact);
        assert_eq!(budget.report()?.observed_bytes, observed);
        assert!(db.external_triggers().latest("reset-new")?.is_none());
        let retry = reset_command(&db, &budget, "reset-fault", "reset-new", None)?;
        assert_eq!(p.descriptor(), retry.descriptor());
        let InputMutationExecution::Committed { value, receipt } =
            commit(&db, &budget, &retry, &request())?
        else {
            anyhow::bail!("retry replayed absent ACK")
        };
        assert_eq!(value.effects.scheduling_write_keys.len(), 2);
        assert!(
            matches!(&receipt.outcome.result,InputMutationResult::ExternalTrigger{outcome} if matches!(outcome.as_ref(),ExternalTriggerMutationOutcome::Reset{record,revoked:Some(_)} if record.external_trigger_id=="reset-new"))
        );
        drop((p, retry, value, receipt));
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[test]
fn fresh_post_wake_accounting_keeps_revoked_status_clock_and_original_disposition() -> Result<()> {
    let original = trigger();
    let (_home, db, budget) = fixture(Some(original.clone()))?;
    let admission = db.external_triggers().observe_active_for_admission(
        &scope(),
        &original.external_trigger_id,
        &request(),
        &budget,
    )?;
    assert_eq!(
        admission.record().unwrap().status,
        ExternalTriggerStatus::Active
    );
    drop(admission);
    // Two separate original scopes/TXs model the original post-wake reacquire.
    let revoke = revoke_command(
        &db,
        &budget,
        &original.external_trigger_id,
        "between-phases-revoke",
    )?;
    drop(commit(&db, &budget, &revoke, &request())?);
    let revoked = db
        .external_triggers()
        .latest(&original.external_trigger_id)?
        .unwrap();
    let observed = db.external_triggers().observe_existing_for_cleanup(
        &scope(),
        &original.external_trigger_id,
        &request(),
        &budget,
    )?;
    let delivered_at = at() + chrono::Duration::seconds(5); // original pre-await clock
    let account = PreparedInputTransition::external_trigger(
        scope(),
        "post-wake-account".into(),
        delivered_at,
        PreparedExternalTriggerTransition::account(
            observed,
            delivered_at,
            CallbackIngressDisposition::Coalesced,
            None,
        )?,
    )?;
    let InputMutationExecution::Committed { value, receipt } =
        commit(&db, &budget, &account, &request())?
    else {
        anyhow::bail!("account replayed")
    };
    let actual = db
        .external_triggers()
        .latest(&original.external_trigger_id)?
        .unwrap();
    assert_eq!(actual.status, ExternalTriggerStatus::Revoked);
    assert_eq!(actual.revoked_at, revoked.revoked_at);
    assert_eq!(actual.delivery_count, 1);
    assert_eq!(actual.last_delivered_at, Some(delivered_at));
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::ExternalTrigger{outcome} if matches!(outcome.as_ref(),ExternalTriggerMutationOutcome::Accounted{record,disposition:CallbackIngressDisposition::Coalesced} if record==&actual))
    );
    let exact = saved(&db)?;
    assert!(matches!(
        commit(&db, &budget, &account, &request())?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(saved(&db)?, exact);
    assert_eq!(side_effects(&db)?, (0, 0, 0, 0));
    drop((revoke, account, value, receipt));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn trigger_new_admission_deleting_rejected_but_original_qualified_revoke_can_finish() -> Result<()>
{
    let original = trigger();
    let (_home, db, budget) = fixture(Some(original.clone()))?;
    let reset = reset_command(&db, &budget, "late-new-ingress", "late-new", None)?;
    db.transaction(|tx| {
        let mut identity = identity();
        identity.status = AgentRegistryStatus::Deleting;
        identity.revision += 1;
        identity.updated_at += chrono::Duration::seconds(30);
        crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &identity)
    })?;
    assert_eq!(
        db.agent_identities()
            .latest(&scope().agent_id)?
            .unwrap()
            .status,
        AgentRegistryStatus::Deleting
    );
    let exact = saved(&db)?;
    assert!(commit(&db, &budget, &reset, &request()).is_err());
    assert_eq!(saved(&db)?, exact);
    let revoke = revoke_command(
        &db,
        &budget,
        &original.external_trigger_id,
        "qualified-deleting-revoke",
    )?;
    drop(commit(&db, &budget, &revoke, &request())?);
    assert_eq!(
        db.external_triggers()
            .latest(&original.external_trigger_id)?
            .unwrap()
            .status,
        ExternalTriggerStatus::Revoked
    );
    assert_eq!(side_effects(&db)?, (0, 0, 0, 0));
    drop((reset, revoke));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn stale_default_replacement_and_foreign_existing_id_never_write_original_scope() -> Result<()> {
    let original = trigger();
    let (_home, db, budget) = fixture(Some(original.clone()))?;
    let stale = reset_command(&db, &budget, "stale-default", "stale-new", None)?;
    let replace = reset_command(&db, &budget, "current-default", "current-new", None)?;
    drop(commit(&db, &budget, &replace, &request())?);
    let exact = saved(&db)?;
    assert!(commit(&db, &budget, &stale, &request()).is_err());
    assert_eq!(saved(&db)?, exact);
    let mut foreign = original.clone();
    foreign.target_agent_id = "foreign".into();
    foreign.external_trigger_id = "foreign-collision".into();
    foreign.status = ExternalTriggerStatus::Revoked;
    foreign.revoked_at = Some(at());
    db.transaction(|tx| repositories::upsert_external_trigger_tx(tx, &foreign))?;
    let collision = reset_command(
        &db,
        &budget,
        "global-collision",
        &foreign.external_trigger_id,
        None,
    )?;
    let exact = saved(&db)?;
    assert!(commit(&db, &budget, &collision, &request()).is_err());
    assert_eq!(saved(&db)?, exact);
    assert_eq!(
        db.external_triggers()
            .latest(&foreign.external_trigger_id)?,
        Some(foreign)
    );
    drop((stale, replace, collision));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn normalized_trigger_material_is_budgeted_before_corrupt_body_and_scope_guards_release(
) -> Result<()> {
    let original = trigger();
    let (_home, db, budget) = fixture(Some(original.clone()))?;
    db.transaction(|tx| {tx.execute("UPDATE external_triggers SET token_hash=?2,payload_json='bad-trigger-json' WHERE external_trigger_id=?1",
        params![original.external_trigger_id,"x".repeat(128*1024)])?;Ok(())})?;
    let mut tiny = request();
    tiny.byte_limit = 512 * 1024;
    let error = db
        .external_triggers()
        .observe_active_for_admission(&scope(), &original.external_trigger_id, &tiny, &budget)
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    assert_eq!(side_effects(&db)?, (0, 0, 0, 0));
    Ok(())
}
#[test]
fn trigger_original_observation_cannot_move_to_another_actual_ledger() -> Result<()> {
    let (_home, db, budget) = fixture(None)?;
    let p = ensure_command(&db, &budget, "wrong-ledger", "never-written", None)?;
    let different = ledger()?;
    let exact = saved(&db)?;
    let error = commit(&db, &different, &p, &request()).unwrap_err();
    assert!(format!("{error:#}").contains("another ledger"));
    assert_eq!(saved(&db)?, exact);
    assert_eq!(different.report()?.observed_bytes, 0);
    drop(p);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn trigger_observation_and_intent_share_one_phase_allowance_before_any_business_sql() -> Result<()>
{
    let mut original = trigger();
    original.token = Some("t".repeat(8192));
    let (_home, db, budget) = fixture(Some(original))?;
    let observed =
        db.external_triggers()
            .observe_default_for_writer(&scope(), &request(), &budget)?;
    let (observation_bytes, _) = observed.material(&budget)?;
    let command = PreparedExternalTriggerTransition::ensure(
        observed,
        "unused-for-noop".into(),
        "fresh-token".into(),
        at(),
        None,
    );
    let intent_bytes = usize::try_from(serialized_bytes(&command)?)?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .context("fixture intent estimate overflow")?;
    let mut tiny = request();
    tiny.byte_limit = observation_bytes + intent_bytes - 1;
    assert!(observation_bytes < tiny.byte_limit && intent_bytes < tiny.byte_limit);
    let p = PreparedInputTransition::external_trigger(
        scope(),
        "combined-trigger-allowance".into(),
        at(),
        command,
    )?;
    let exact = saved(&db)?;
    let held = budget.report()?.observed_bytes;
    let error = commit(&db, &budget, &p, &tiny).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(saved(&db)?, exact);
    assert_eq!(side_effects(&db)?, (0, 0, 0, 0));
    assert_eq!(budget.report()?.observed_bytes, held);
    drop(p);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
