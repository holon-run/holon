use super::*;
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use crate::runtime_db::scheduling_snapshot::{
    prepare_cold_scheduling_query_tx, SchedulingDomainLoadRequest, SchedulingSnapshotError,
};
fn query_request() -> SchedulingDomainLoadRequest {
    SchedulingDomainLoadRequest {
        capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    }
}
#[tokio::test]
async fn cold_host_projects_actual_qualified_timer_and_trigger_without_activation() -> Result<()> {
    let (_home, host) = test_host();
    let config = host.config().as_ref().clone();
    let runtime = host.default_runtime().await?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("original cold timer".into()))
        .await?;
    let trigger = runtime
        .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
        .await?;
    let scope = InputEvidenceScope {
        agent_id: runtime.instance_agent_id().into(),
        identity_incarnation: runtime.instance_incarnation(),
    };
    drop(runtime);
    host.shutdown().await?;
    drop(host);
    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    let baseline = cold.inner.input_evidence_budget.report()?.observed_bytes;
    let projection = cold
        .operator_agent_state_projection(&scope.agent_id, 40, 50)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(projection.source, AgentStateProjectionSource::Storage);
    assert_eq!(projection.agent.agent.id, scope.agent_id);
    assert!(projection.timers.iter().any(|row| row == &timer));
    assert!(projection
        .external_triggers
        .iter()
        .any(|row| row.external_trigger_id == trigger.external_trigger_id
            && row.target_agent_id == scope.agent_id
            && row.status == ExternalTriggerStatus::Active));
    assert!(projection.scheduling_material.is_some());
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let wire = serde_json::to_value((&projection.timers, &projection.external_triggers))?;
    assert_eq!(wire[0][0]["id"], timer.id);
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    drop(wire);
    drop(projection);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    cold.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn no_state_cold_empty_display_requires_every_original_inventory_empty() -> Result<()> {
    for origin in ["empty", "raw_timer", "qualified", "fixed", "import", "wake"] {
        let (_home, host) = test_host();
        let identity = host.ensure_default_agent_identity()?;
        let scope = InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        assert!(host
            .runtime_db()
            .agent_states()
            .latest(&identity.agent_id)?
            .is_none());
        host.runtime_db().transaction(|tx| {
            match origin {
                "raw_timer"=>{
                    let timer=TimerRecord {id:"raw-cold-timer".into(),agent_id:scope.agent_id.clone(),
                        created_at:Utc::now(),duration_ms:3600000,interval_ms:None,repeat:false,
                        status:TimerStatus::Active,summary:None,next_fire_at:None,last_fired_at:None,fire_count:0};
                    crate::runtime_db::repositories::upsert_timer_tx(tx,&timer)?;
                    tx.execute("UPDATE timers SET payload_json='bad-unqualified-body' WHERE timer_id=?1",[&timer.id])?;
                }
                "qualified"=>{tx.execute("INSERT INTO scheduling_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability) VALUES(?1,?2,'timer','known-absent',1,'absent')",rusqlite::params![scope.agent_id,scope.identity_incarnation])?;}
                "fixed"=>{
                    tx.execute("INSERT INTO scheduling_fixed_manifests(inventory_id,agent_id,tables_json,member_count,metadata_bytes,content_digest,sealed) VALUES('cold-fixed',?1,'[\"timer\"]',1,128,?2,0)",rusqlite::params![scope.agent_id,"0".repeat(64)])?;
                    tx.execute("INSERT INTO scheduling_fixed_members(inventory_id,family,record_id,content_digest,projection_digest,status,sort_at,created_at) VALUES('cold-fixed','timer','fixed-absent',?1,?1,'active','2026-10-01T00:00:00Z','2026-10-01T00:00:00Z')",["0".repeat(64)])?;
                    tx.execute("UPDATE scheduling_fixed_manifests SET sealed=1 WHERE inventory_id='cold-fixed'",[])?;
                }
                "import"=>{tx.execute("INSERT INTO scheduling_import_inventory(agent_id,family,record_id,received_digest,content_digest,projection_digest) VALUES(?1,'timer','original-import',?2,?2,?2)",rusqlite::params![scope.agent_id,"0".repeat(64)])?;}
                "wake"=>{
                    let timer=TimerRecord {id:"foreign-cold-timer".into(),agent_id:"foreign-agent".into(),
                        created_at:Utc::now(),duration_ms:3600000,interval_ms:None,repeat:false,
                        status:TimerStatus::Active,summary:None,next_fire_at:None,last_fired_at:None,fire_count:1};
                    crate::runtime_db::repositories::upsert_timer_tx(tx,&timer)?;
                    let message=MessageEnvelope::new(scope.agent_id.clone(),MessageKind::TimerTick,
                        MessageOrigin::Timer {timer_id:timer.id.clone()},AuthorityClass::RuntimeInstruction,
                        Priority::Next,MessageBody::Text {text:"original owned wake source".into()});
                    let (message,_)=crate::runtime_db::evidence::append_message_tx(tx,&message)?;
                    tx.execute("INSERT INTO timer_wakes(timer_id,message_id,fire_count,status,created_at,updated_at) VALUES(?1,?2,1,'pending',?3,?3)",rusqlite::params![timer.id,message.id,crate::runtime_db::repositories::timestamp(message.created_at)])?;
                }
                _=>{}
            }
            Ok(())
        })?;
        let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
        let result = host
            .operator_agent_state_projection(&identity.agent_id, 10, 10)
            .await;
        if origin == "empty" {
            let projection = result.map_err(|error| anyhow!("{error:?}"))?;
            assert_eq!(projection.agent.agent.status, AgentStatus::Stopped);
            assert!(projection.timers.is_empty() && projection.external_triggers.is_empty());
            assert!(projection.scheduling_material.is_some());
            drop(projection);
        } else {
            let error = result
                .err()
                .context("known original Scheduling cannot become an empty display")?;
            assert!(
                matches!(error,PublicAgentError::Runtime(ref error)
                if matches!(error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
                    Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive))),
                "{origin}: {error:?}"
            );
        }
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        let counts:(u64,u64)=host.runtime_db().connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM scheduling_domain_current),(SELECT COUNT(*) FROM agent_states)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(
            counts,
            (0, 0),
            "no State/root may be fabricated for {origin}"
        );
    }
    Ok(())
}
#[test]
fn cold_preparation_native_state_budget_and_retired_scope_precede_corrupt_decode() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let mut state = serde_json::to_value(AgentState::new(&identity.agent_id))?;
    state["pending"] = serde_json::json!("corrupt-count");
    state["working_memory"] = serde_json::json!({"large":vec![0;4096]});
    host.runtime_db().transaction(|tx| {
        tx.execute("INSERT INTO agent_states(agent_id,status,current_run_id,updated_at,payload_json) VALUES(?1,?2,NULL,?3,?4)",rusqlite::params![scope.agent_id,state["status"].as_str().context("fixture State status absent")?,crate::runtime_db::repositories::timestamp(Utc::now()),serde_json::to_string(&state)?])?;
        Ok(())
    })?;
    let ledger = &host.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let mut small = query_request();
    small.record_limit = 64;
    let error = host
        .runtime_db()
        .transaction(|tx| prepare_cold_scheduling_query_tx(tx, &scope, &small, ledger))
        .err()
        .context("native node gate must precede corrupt State decoder")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    host.runtime_db().transaction(|tx| {
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&scope.agent_id])?;
        Ok(())
    })?;
    let error = host
        .runtime_db()
        .transaction(|tx| prepare_cold_scheduling_query_tx(tx, &scope, &query_request(), ledger))
        .err()
        .context("old scope cannot inspect corrupt successor State")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::RetiredScope)
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
