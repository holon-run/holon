use super::*;
use axum::response::IntoResponse;
use tower::ServiceExt;

fn fixture() -> Result<(tempfile::TempDir, RuntimeHost, AgentIdentityRecord)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = {
        let _lock = crate::test_env::lock_env();
        AppConfig::load_with_home(Some(home.path().into()))?
    };
    config.user_home_dir = Some(home.path().into());
    config.control_auth_mode = crate::config::ControlAuthMode::Disabled;
    let host = RuntimeHost::new_with_provider(
        config,
        Arc::new(crate::provider::StubProvider::new("unused")),
    )?;
    let identity = host.ensure_default_agent_identity()?;
    let directory = host
        .agent_data_dir(&identity.agent_id)
        .join("skills/owned-read");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("SKILL.md"),
        "---\nname: owned-read\ndescription: owned cold skill\n---\noriginal skill body",
    )?;
    Ok((home, host, identity))
}

#[tokio::test]
async fn actual_owned_skill_detail_rejects_recreated_identity_before_file_read() -> Result<()> {
    let (_home, host, mut identity) = fixture()?;
    let (view, material) = host
        .operator_agent_skills_view(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?
        .into_parts();
    let skill = view
        .discoverable_skills
        .into_iter()
        .find(|skill| skill.name == "owned-read")
        .unwrap();
    identity.incarnation += 1;
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &identity)?;
        Ok(())
    })?;
    let error = host
        .agent_skill_detail_read(material, skill)
        .await
        .err()
        .context("original selected resource must reject successor identity")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
        Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::RetiredScope)
    ));
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_no_state_skills_keep_real_scope_and_material_until_synchronous_wire() -> Result<()> {
    let (_home, host, identity) = fixture()?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    let (view, material) = host
        .operator_agent_skills_view(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?
        .into_parts();
    assert_eq!(material.scope.agent_id, identity.agent_id);
    assert_eq!(material.scope.identity_incarnation, identity.incarnation);
    assert!(view
        .discoverable_skills
        .iter()
        .any(|skill| skill.name == "owned-read"));
    let weak = Arc::downgrade(material._owned.last().context("cold observation guard")?);
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let response =
        axum::Json(serde_json::json!({"skills":view.discoverable_skills})).into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(weak.upgrade().is_some());
    drop(material);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_skill_http_list_and_detail_serialize_original_shape_without_activation() -> Result<()>
{
    let (_home, host, identity) = fixture()?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let app = crate::http::router(crate::http::AppState::for_tcp(host.clone()));
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/agents/{}/skills", identity.agent_id))
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    let list: serde_json::Value = serde_json::from_slice(&body)?;
    let skill = list["skills"]
        .as_array()
        .context("original list array")?
        .iter()
        .find(|skill| skill["name"] == "owned-read")
        .context("original skill")?;
    let skill_id = skill["skill_id"].as_str().context("original skill ID")?;
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!(
                    "/api/agents/{}/skills/{skill_id}",
                    identity.agent_id
                ))
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let detail: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1024 * 1024).await?)?;
    assert_eq!(detail["skill"]["skill_id"], skill_id);
    assert!(detail["content"]
        .as_str()
        .unwrap()
        .contains("original skill body"));
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn agent_skill_detail_bounds_changed_file_before_allocating_its_body() -> Result<()> {
    let (_home, host, identity) = fixture()?;
    let (view, material) = host
        .operator_agent_skills_view(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?
        .into_parts();
    let skill = view
        .discoverable_skills
        .iter()
        .find(|skill| skill.name == "owned-read")
        .unwrap();
    std::fs::write(&skill.path, vec![b'x'; 9 * 1024 * 1024])?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let error = host
        .read_agent_skill_content(&material, &skill.path)
        .err()
        .context("declared file exceeds remaining request allowance")?;
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    drop((view, material));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_resource_header_rejects_original_incarnation_before_successor_state_read(
) -> Result<()> {
    let (_home, host, mut identity) = fixture()?;
    let original = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    identity.incarnation += 1;
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &identity)?;
        crate::runtime_db::evidence::upsert_agent_state_tx(
            tx,
            &AgentState::new(&identity.agent_id),
        )?;
        tx.execute(
            "UPDATE agent_states SET payload_json='invalid successor State' WHERE agent_id=?1",
            [&identity.agent_id],
        )?;
        Ok(())
    })?;
    let limits = host.inner.input_evidence_budget.limits();
    let request = crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
        capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: usize::try_from(limits.record_limit)?,
    };
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let error = host
        .runtime_db()
        .transaction(|tx| {
            crate::runtime_db::scheduling_snapshot::read_cold_resource_scope_tx(
                tx,
                &original,
                &request,
                &host.inner.input_evidence_budget,
            )
        })
        .err()
        .context("old scope must be retired")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
        Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::RetiredScope)
    ));
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[test]
fn host_resource_completion_material_never_creates_agent_scope_and_releases_last_guard(
) -> Result<()> {
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let reserved = ledger.reserve_host_owned_completion(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 16,
            metadata_estimate_bytes: 0,
        },
    )?;
    assert!(reserved.matches_ledger(&ledger));
    assert!(!reserved.matches_scope(&InputEvidenceScope {
        agent_id: "not-created".into(),
        identity_incarnation: 1
    }));
    reserved.observe_material(1, 0);
    assert_eq!(reserved.guard().owned_material_bytes()?, 17);
    assert!(ledger.report()?.scopes.is_empty());
    let guard = reserved.seal_resource_observation(OwnedBytesEstimate {
        payload_bytes: 8,
        metadata_estimate_bytes: 0,
    })?;
    assert_eq!(guard.owned_material_bytes()?, 8);
    assert!(ledger.report()?.scopes.is_empty());
    let weak = Arc::downgrade(&guard);
    let held = guard.clone();
    drop(guard);
    assert!(weak.upgrade().is_some());
    drop(held);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn global_skill_http_catalog_and_detail_keep_original_wire_without_runtime_activation(
) -> Result<()> {
    let (home, host, _) = fixture()?;
    let directory = home.path().join(".agents/skills/global-owned-read");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("SKILL.md"), "---\nname: global-owned-read\ndescription: global owned catalog\n---\noriginal global body")?;
    let before_identity: usize = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM agent_identities",
        [],
        |row| row.get(0),
    )?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let app = crate::http::router(crate::http::AppState::for_tcp(host.clone()));
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/skills/catalog?scope=user_global")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let list: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1024 * 1024).await?)?;
    assert_eq!(list["library"], "user_global");
    assert_eq!(list["scope"], "user_global");
    let skill = list["catalog"]
        .as_array()
        .context("original catalog array")?
        .iter()
        .find(|entry| entry["name"] == "global-owned-read")
        .context("original global entry")?;
    let id = skill["skill_id"]
        .as_str()
        .context("original global skill ID")?;
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/skills/catalog/{id}"))
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let detail: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1024 * 1024).await?)?;
    assert_eq!(detail["library"], "user_global");
    assert_eq!(detail["skill"]["skill_id"], id);
    assert!(detail["content"]
        .as_str()
        .unwrap()
        .contains("original global body"));
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert_eq!(
        host.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM agent_identities",
            [],
            |row| row.get::<_, usize>(0)
        )?,
        before_identity
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn global_detail_rejects_changed_file_budget_and_original_host_write_fence() -> Result<()> {
    let (home, host, _) = fixture()?;
    let directory = home.path().join(".agents/skills/global-detail-budget");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("SKILL.md"),
        "---\nname: global-detail-budget\ndescription: bounded detail\n---\nbody",
    )?;
    let (catalog, material) = host
        .global_skills_catalog_read(Some(crate::types::SkillScope::UserGlobal))
        .await?
        .into_parts();
    let skill = catalog
        .into_iter()
        .find(|entry| entry.name == "global-detail-budget")
        .context("global detail entry")?;
    let weak = Arc::downgrade(&material._owned);
    std::fs::write(&skill.path, vec![b'x'; 9 * 1024 * 1024])?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let error = host
        .read_global_skill_content(&material, &skill.path)
        .err()
        .context("file body must fit same catalog request")?;
    assert!(
        format!("{error:#}").contains("ResourceExhausted"),
        "{error:#}"
    );
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let write = host.begin_loaded_context_resource_write().await?;
    let error = host
        .read_global_skill_content(&material, &skill.path)
        .err()
        .context("original Host write fence must reject old global catalog")?;
    assert!(
        !format!("{error:#}").contains("ResourceExhausted"),
        "{error:#}"
    );
    drop(write);
    let error = host
        .read_global_skill_content(&material, &skill.path)
        .err()
        .context("finished write must not restore old generation")?;
    assert!(
        !format!("{error:#}").contains("ResourceExhausted"),
        "{error:#}"
    );
    assert!(weak.upgrade().is_some());
    drop(material);
    assert!(weak.upgrade().is_none());
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_skill_first_identity_raw_and_native_gates_precede_decoder_and_release() -> Result<()>
{
    for native in [false, true] {
        let (_home, host, identity) = fixture()?;
        let payload = if native {
            serde_json::json!({
                "agent_id": false,
                "incarnation": 1,
                "unread_nodes": vec![0; 12_000],
            })
            .to_string()
        } else {
            "{".repeat(9 * 1024 * 1024)
        };
        if native {
            let limits = host.inner.input_evidence_budget.limits();
            assert!(
                (payload.len() as u64 + 8192) * 8
                    < (limits.history_per_agent_bytes + limits.pending_per_agent_bytes)
                        .min(64 * 1024 * 1024)
            );
            assert!(12_000 * 8 > limits.record_limit.min(65_536));
        }
        host.runtime_db().transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
                rusqlite::params![payload, identity.agent_id],
            )?;
            Ok(())
        })?;
        let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
        let error = host
            .operator_agent_skills_view(&identity.agent_id)
            .await
            .err()
            .context("first identity material must reject before canonical DTO decoder")?;
        assert!(matches!(error, PublicAgentError::Runtime(ref error)
            if matches!(error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
                Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::ResourceExhausted(_)))));
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        assert!(host
            .runtime_db()
            .agent_states()
            .latest(&identity.agent_id)?
            .is_none());
        // Restore only this fixture corruption so normal Host shutdown keeps
        // its original identity validation and resource drain behavior.
        host.runtime_db().transaction(|tx| {
            crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &identity)?;
            Ok(())
        })?;
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_skill_locator_preserves_not_found_and_inactive_identity_errors() -> Result<()> {
    let (_home, host, mut identity) = fixture()?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(matches!(
        host.operator_agent_skills_view("no-such-resource-agent")
            .await,
        Err(PublicAgentError::NotFound { .. })
    ));
    for status in [AgentRegistryStatus::Deleting, AgentRegistryStatus::Deleted] {
        identity.status = status;
        host.runtime_db().transaction(|tx| {
            crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &identity)?;
            Ok(())
        })?;
        let result = host.operator_agent_skills_view(&identity.agent_id).await;
        assert!(matches!(
            (status, result),
            (
                AgentRegistryStatus::Deleting,
                Err(PublicAgentError::Deleting { .. })
            ) | (
                AgentRegistryStatus::Deleted,
                Err(PublicAgentError::Deleted { .. })
            )
        ));
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        assert!(host
            .runtime_db()
            .agent_states()
            .latest(&identity.agent_id)?
            .is_none());
    }
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn loaded_skills_use_actual_published_identity_without_identity_or_state_sql() -> Result<()> {
    let (_home, host, identity) = fixture()?;
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    host.refresh_loaded_context_resources().await?;
    let (actual, guard) = runtime.capture_active_identity_read().await?;
    assert_eq!(
        actual.as_ref(),
        &host
            .agent_identity_record(&identity.agent_id)?
            .context("actual canonical identity")?
    );
    let weak = Arc::downgrade(&guard);
    host.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE agent_identities RENAME TO skills_identity_forbidden; ALTER TABLE agent_states RENAME TO skills_state_forbidden;")?;
        Ok(())
    })?;
    let read = host
        .operator_agent_skills_view(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert!(read
        .discoverable_skills
        .iter()
        .any(|skill| skill.name == "owned-read"));
    assert!(weak.upgrade().is_some());
    drop(read);
    host.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE skills_state_forbidden RENAME TO agent_states; ALTER TABLE skills_identity_forbidden RENAME TO agent_identities;")?;
        Ok(())
    })?;
    drop((actual, guard));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn actual_metadata_lost_publication_keeps_identity_gate_and_original_capture_guard(
) -> Result<()> {
    let (_home, host, identity) = fixture()?;
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    let owner = host
        .bridge()
        .mutation_owner_for(&identity.agent_id, identity.incarnation)?;
    let mut reservations = vec![owner.reserve().await?];
    let original = reservations[0]
        .capture_ready_snapshot()?
        .context("actual bootstrap root")?;
    let old_guard = Arc::downgrade(
        original
            ._identity_budget
            .as_ref()
            .context("identity material")?,
    );
    let mut next = original
        .identity
        .as_ref()
        .context("identity")?
        .as_ref()
        .clone();
    next.name = Some("original actual metadata ACK".into());
    next.revision += 1;
    host.fail_next_external_mutation_publication();
    let result = host
        .commit_external_metadata(&mut reservations, ids::audit_event_id(), || {
            host.runtime_db().transaction(|tx| {
                crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &next)?;
                Ok(())
            })?;
            Ok((
                (),
                vec![
                    crate::runtime::mutation_owner::IdentityMutationFacts::budgeted(
                        Some(&next),
                        Some(original.state.clone()),
                        &host.inner.input_evidence_budget,
                    )?,
                ],
            ))
        })
        .await;
    assert!(result.is_err());
    drop(reservations);
    let error = runtime
        .capture_active_identity_read()
        .await
        .err()
        .context("lost publication remains gated")?;
    assert!(matches!(
        error.downcast_ref::<MutationOwnerError>(),
        Some(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
    ));
    let mut reservations = vec![owner.reserve().await?];
    host.reconcile_external_mutations(&mut reservations).await?;
    drop(reservations);
    let (actual, _guard) = runtime.capture_active_identity_read().await?;
    assert_eq!(actual.name.as_deref(), Some("original actual metadata ACK"));
    assert!(old_guard.upgrade().is_some());
    drop(original);
    assert!(old_guard.upgrade().is_none());
    host.shutdown().await?;
    Ok(())
}
