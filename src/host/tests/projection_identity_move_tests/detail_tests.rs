use super::*;
use crate::types::AgentBootstrapSummary;

// Former assembly path, including its original read and error order.
fn borrowed_detail(host: &RuntimeHost, identity: &AgentIdentityRecord) -> Result<AgentDetail> {
    let deletion = host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(&identity.agent_id)?;
    let bootstrap = host
        .runtime_db()
        .agent_bootstraps()
        .latest(&identity.agent_id)?
        .map(|record| record.summary());
    let canonical_relations = host
        .runtime_db()
        .agent_canonical_relations()
        .latest(&identity.agent_id)?
        .ok_or_else(|| anyhow!("missing canonical relations for {}", identity.agent_id))?;
    let lineage_children = host
        .runtime_db()
        .agent_canonical_relations()
        .lineage_children(&identity.agent_id)?;
    Ok(AgentDetail {
        display_name: identity.display_name(),
        name: identity.name.clone(),
        created_at: identity.created_at,
        updated_at: identity.updated_at,
        identity: AgentIdentityView::from_record(identity, &host.config().default_agent_id),
        canonical_relations,
        lineage_children,
        bootstrap,
        deletion,
    })
}

#[tokio::test]
async fn detail_consumes_original_identity_text_and_matches_borrowed_wire() -> Result<()> {
    let (_home, host) = fixture().await?;
    let mut cases = 0;
    for empty in [false, true] {
        for mask in 0..16 {
            let mut source = record();
            for (bit, field) in [
                &mut source.name,
                &mut source.parent_agent_id,
                &mut source.lineage_parent_agent_id,
                &mut source.delegated_from_task_id,
            ]
            .into_iter()
            .enumerate()
            {
                if mask & (1 << bit) == 0 {
                    *field = None;
                } else if empty {
                    *field = Some(text(""));
                }
            }
            let expected = borrowed_detail(&host, &source)?;
            let original = allocations(record_fields(&source));
            let detail = host.agent_detail_from_identity(source)?;
            assert_eq!(detail, expected);
            assert_eq!(
                serde_json::to_value(&detail)?,
                serde_json::to_value(&expected)?
            );
            assert_eq!(allocations(view_fields(&detail.identity)), original);
            if let (Some(outer), Some(inner)) = (&detail.name, &detail.identity.name) {
                assert_eq!(outer, inner);
                if !outer.is_empty() {
                    assert_ne!(outer.as_ptr(), inner.as_ptr());
                }
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 32);
    Ok(())
}

#[tokio::test]
async fn detail_public_operator_and_rename_keep_existing_contract() -> Result<()> {
    let (_home, host) = fixture().await?;
    let source = host.agent_identity_record("web")?.unwrap();
    let expected = borrowed_detail(&host, &source)?;
    let public = host
        .public_agent_detail("web")
        .map_err(|error| anyhow!("{error}"))?;
    let operator = host
        .operator_agent_detail("web")
        .map_err(|error| anyhow!("{error}"))?;
    assert_eq!(public, expected);
    assert_eq!(operator, expected);
    let renamed = host
        .rename_public_agent("web", "新名字 🦀", "detail-test")
        .map_err(|error| anyhow!("{error}"))?;
    let updated = host.agent_identity_record("web")?.unwrap();
    assert_eq!(renamed, borrowed_detail(&host, &updated)?);
    assert_eq!(renamed.display_name, "新名字 🦀");
    assert_eq!(renamed.name.as_deref(), Some("新名字 🦀"));
    for detail in [
        host.public_agent_detail("missing-detail"),
        host.operator_agent_detail("missing-detail"),
    ] {
        assert!(matches!(detail, Err(PublicAgentError::NotFound { .. })));
    }
    for detail in [
        host.public_agent_detail("invalid/path"),
        host.operator_agent_detail("invalid/path"),
    ] {
        assert!(matches!(detail, Err(PublicAgentError::Runtime(_))));
    }
    Ok(())
}

#[tokio::test]
async fn detail_keeps_canonical_failure_and_original_lineage_holder() -> Result<()> {
    let (_home, host) = fixture().await?;
    let mut missing = record();
    missing.agent_id = "missing-detail".into();
    let expected = borrowed_detail(&host, &missing).unwrap_err().to_string();
    let actual = host
        .agent_detail_from_identity(missing)
        .unwrap_err()
        .to_string();
    assert_eq!(actual, expected);
    assert_eq!(actual, "missing canonical relations for missing-detail");

    let source = record();
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let before = Arc::strong_count(&holder);
    let detail = {
        let _entry = phase.enter();
        host.agent_detail_from_identity(source)?
    };
    let lineage_holder = detail.lineage_children._material.as_ref().unwrap();
    assert!(Arc::ptr_eq(lineage_holder, &holder));
    assert_eq!(Arc::strong_count(&holder), before + 1);
    let cost = phase.finish()?;
    assert!(cost.0 > 0 && cost.1 > 0);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(detail);
    assert_eq!(Arc::strong_count(&holder), 1);
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn bootstrap_steps(record: &mut AgentBootstrapRecord) -> [&mut AgentBootstrapStep; 5] {
    [
        &mut record.template,
        &mut record.runtime,
        &mut record.workspace,
        &mut record.model,
        &mut record.initial_message,
    ]
}

fn bootstrap_allocations(summary: &AgentBootstrapSummary) -> [Option<(*const u8, usize)>; 5] {
    [
        &summary.template,
        &summary.runtime,
        &summary.workspace,
        &summary.model,
        &summary.initial_message,
    ]
    .map(|step| {
        step.last_error
            .as_ref()
            .map(|value| (value.as_ptr(), value.capacity()))
    })
}

#[test]
fn owned_bootstrap_summary_moves_errors_and_preserves_every_status_combination() -> Result<()> {
    let statuses = [
        AgentBootstrapStepStatus::Pending,
        AgentBootstrapStepStatus::Succeeded,
        AgentBootstrapStepStatus::Failed,
    ];
    for combination in 0..243 {
        for mask in 0..32 {
            let mut record = AgentBootstrapRecord::legacy_ready("web");
            let mut digits = combination;
            let mut errors = mask;
            let original = bootstrap_steps(&mut record).map(|step| {
                step.status = statuses[digits % 3];
                digits /= 3;
                step.attempts = combination as u32 + 7;
                // Some("") still has an original allocation to transfer.
                step.last_error = Some(text(if errors & 1 == 0 {
                    ""
                } else {
                    "bootstrap 汉字 🦀"
                }));
                errors >>= 1;
                step.last_error
                    .as_ref()
                    .map(|value| (value.as_ptr(), value.capacity()))
            });
            let expected = record.summary();
            let all_succeeded = [
                &record.template,
                &record.runtime,
                &record.workspace,
                &record.model,
                &record.initial_message,
            ]
            .into_iter()
            .all(|step| step.status == AgentBootstrapStepStatus::Succeeded);
            assert_eq!(all_succeeded, combination == 121);
            assert_eq!(
                expected.status,
                if all_succeeded {
                    AgentBootstrapStatus::Ready
                } else {
                    AgentBootstrapStatus::Degraded
                }
            );
            let owned = record.into_summary();
            assert_eq!(owned, expected);
            assert_eq!(
                serde_json::to_value(&owned)?,
                serde_json::to_value(&expected)?
            );
            assert_eq!(bootstrap_allocations(&owned), original);
        }
    }
    // Exercise absent optional errors independently of all status combinations.
    for mask in 0..32 {
        let mut record = AgentBootstrapRecord::legacy_ready("web");
        for (index, step) in bootstrap_steps(&mut record).into_iter().enumerate() {
            step.last_error = (mask & (1 << index) != 0).then(|| text("错误 🦀"));
        }
        let expected = record.summary();
        let original = bootstrap_steps(&mut record).map(|step| {
            step.last_error
                .as_ref()
                .map(|value| (value.as_ptr(), value.capacity()))
        });
        let owned = record.into_summary();
        assert_eq!(owned, expected);
        assert_eq!(bootstrap_allocations(&owned), original);
        assert_eq!(
            serde_json::to_value(&owned)?,
            serde_json::to_value(&expected)?
        );
    }
    Ok(())
}

#[test]
fn owned_bootstrap_summary_adds_no_material_and_preserves_original_phase_holder() -> Result<()> {
    let mut record = AgentBootstrapRecord::legacy_ready("web");
    let original = bootstrap_steps(&mut record).map(|step| {
        step.last_error = Some(text("原错误 🦀"));
        step.last_error
            .as_ref()
            .map(|value| (value.as_ptr(), value.capacity()))
    });
    let expected = record.summary();
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let before = phase.finish()?;
    let owned = {
        let _entry = phase.enter();
        record.into_summary()
    };
    assert_eq!(phase.finish()?, before);
    assert_eq!(owned, expected);
    assert_eq!(bootstrap_allocations(&owned), original);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, before.0);
    drop(owned);
    assert_eq!(ledger.report()?.observed_bytes, before.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn detail_bootstrap_move_matches_old_public_operator_and_rename_wire() -> Result<()> {
    let (_home, host) = fixture().await?;
    for failed in [false, true] {
        let mut bootstrap = AgentBootstrapRecord::legacy_ready("web");
        bootstrap.revision = host
            .runtime_db()
            .agent_bootstraps()
            .latest("web")?
            .unwrap()
            .revision
            + 1;
        for (index, step) in bootstrap_steps(&mut bootstrap).into_iter().enumerate() {
            step.attempts = index as u32 + 3;
            step.last_error = Some(text(if index % 2 == 0 { "" } else { "错误 🦀" }));
        }
        if failed {
            bootstrap.model.status = AgentBootstrapStepStatus::Failed;
        }
        host.runtime_db().agent_bootstraps().upsert(&bootstrap)?;
        assert_eq!(
            host.runtime_db().agent_bootstraps().latest("web")?,
            Some(bootstrap.clone())
        );
        let identity = host.agent_identity_record("web")?.unwrap();
        let expected = borrowed_detail(&host, &identity)?;
        assert_eq!(expected.bootstrap, Some(bootstrap.summary()));
        for actual in [
            host.public_agent_detail("web"),
            host.operator_agent_detail("web"),
        ] {
            let actual = actual.map_err(anyhow::Error::new)?;
            assert_eq!(actual, expected);
            assert_eq!(
                serde_json::to_value(&actual)?,
                serde_json::to_value(&expected)?
            );
        }
        let renamed = host
            .rename_public_agent("web", "bootstrap 新名字 🦀", "bootstrap-move-test")
            .map_err(anyhow::Error::new)?;
        let identity = host.agent_identity_record("web")?.unwrap();
        assert_eq!(renamed, borrowed_detail(&host, &identity)?);
        assert_eq!(renamed.bootstrap, Some(bootstrap.summary()));
    }
    host.runtime_db()
        .connection()?
        .execute("DELETE FROM agent_bootstraps WHERE agent_id = 'web'", [])?;
    let identity = host.agent_identity_record("web")?.unwrap();
    let actual = host
        .operator_agent_detail("web")
        .map_err(anyhow::Error::new)?;
    assert!(actual.bootstrap.is_none());
    assert_eq!(actual, borrowed_detail(&host, &identity)?);
    Ok(())
}

#[tokio::test]
async fn detail_bootstrap_move_preserves_failure_order_and_lineage_holder() -> Result<()> {
    let (_home, host) = fixture().await?;
    let mut bootstrap = AgentBootstrapRecord::legacy_ready("web");
    bootstrap.revision = host
        .runtime_db()
        .agent_bootstraps()
        .latest("web")?
        .unwrap()
        .revision
        + 1;
    bootstrap.runtime.last_error = Some(text("保留错误 🦀"));
    host.runtime_db().agent_bootstraps().upsert(&bootstrap)?;
    assert_eq!(
        host.runtime_db().agent_bootstraps().latest("web")?,
        Some(bootstrap.clone())
    );
    let source = host.agent_identity_record("web")?.unwrap();
    let expected = borrowed_detail(&host, &source)?;
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let before = Arc::strong_count(&holder);
    let detail = {
        let _entry = phase.enter();
        host.agent_detail_from_identity(source)?
    };
    assert_eq!(detail, expected);
    assert!(Arc::ptr_eq(
        detail.lineage_children._material.as_ref().unwrap(),
        &holder
    ));
    assert_eq!(Arc::strong_count(&holder), before + 1);
    let cost = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(detail);
    assert_eq!(Arc::strong_count(&holder), 1);
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    host.runtime_db().connection()?.execute(
        "UPDATE agent_bootstraps SET payload_json = '{}' WHERE agent_id = 'web'",
        [],
    )?;
    let source = host.agent_identity_record("web")?.unwrap();
    let expected = borrowed_detail(&host, &source).unwrap_err();
    let actual = host.agent_detail_from_identity(source).unwrap_err();
    assert_eq!(actual.to_string(), expected.to_string());
    assert_eq!(actual.to_string(), "decoding agent bootstrap payload");
    for invalid in ["missing-bootstrap-detail", "invalid/path"] {
        for detail in [
            host.public_agent_detail(invalid),
            host.operator_agent_detail(invalid),
        ] {
            assert!(matches!(
                (invalid, detail),
                (
                    "missing-bootstrap-detail",
                    Err(PublicAgentError::NotFound { .. })
                ) | ("invalid/path", Err(PublicAgentError::Runtime(_)))
            ));
        }
    }
    Ok(())
}
