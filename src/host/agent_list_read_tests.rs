use super::*;
#[path = "complete_api_tests.rs"]
mod complete_api;
use crate::{
    provider::StubProvider,
    runtime::resident_input_evidence::InputEvidenceScope,
    types::{AgentOwnership, AgentProfilePreset},
};

fn host() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = Some(home.path().to_path_buf());
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.ensure_default_agent_identity()?;
    Ok((home, host))
}

#[tokio::test]
async fn full_fleet_summary_refuses_unproved_no_state_without_partial_return_or_activation(
) -> Result<()> {
    let (_home, host) = host()?;
    let identity = register(&host, "z-unproved-no-state", "summary-parent")?;
    let summaries = host.list_agents().await?;
    assert!(summaries
        .iter()
        .any(|summary| summary.agent.id == identity.agent_id));
    drop(summaries);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    // A normal new identity has explicit fresh-empty continuity. Remove only
    // that proof to model an unprepared legacy namespace, not a fresh Agent.
    assert_eq!(
        host.runtime_db().connection()?.execute(
            "DELETE FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2",
            rusqlite::params![identity.agent_id, identity.incarnation],
        )?,
        1
    );
    let error = host.list_agents().await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<PublicAgentError>(),
            Some(PublicAgentError::Runtime(error))
                if matches!(
                    error.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                )
        ),
        "{error:#}"
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

fn register(host: &RuntimeHost, id: &str, parent: &str) -> Result<AgentIdentityRecord> {
    let record = AgentIdentityRecord::new(
        id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
    .with_lineage_parent_agent_id(Some(parent.into()));
    host.append_agent_identity(&record)?;
    host.runtime_db().agent_identities().upsert(&record)?;
    Ok(record)
}

#[tokio::test]
async fn full_fleet_summaries_filter_before_copy_and_keep_cold_agents_unloaded() -> Result<()> {
    let (_home, host) = host()?;
    for id in ["z-cold-summary", "a-cold-summary"] {
        let identity = register(&host, id, "summary-parent")?;
        let mut state = AgentState::new(id);
        state.last_wake_reason = Some(format!("stored {id} 汉🦀"));
        host.runtime_db()
            .agent_states()
            .initialize_cold_for_fixture(&state, identity.incarnation)?;
    }
    for (id, private) in [("private-summary", true), ("deleting-summary", false)] {
        let mut record = register(&host, id, "summary-parent")?;
        record.name = Some(format!("{id} {}", "excluded 汉🦀".repeat(8192)));
        if private {
            record.visibility = AgentVisibility::Private;
        } else {
            record.status = AgentRegistryStatus::Deleting;
        }
        host.append_agent_identity(&record)?;
        host.runtime_db().agent_identities().upsert(&record)?;
    }
    let summaries = host.list_agents().await?;
    let ids = summaries
        .iter()
        .map(|summary| summary.agent.id.as_str())
        .collect::<Vec<_>>();
    let config = host.config();
    let mut expected = vec![
        "a-cold-summary",
        config.default_agent_id.as_str(),
        "z-cold-summary",
    ];
    expected.sort_unstable();
    assert_eq!(ids, expected);
    for summary in &summaries {
        if summary.agent.id.ends_with("-cold-summary") {
            assert_eq!(
                summary.agent.last_wake_reason.as_deref(),
                Some(format!("stored {} 汉🦀", summary.agent.id).as_str())
            );
        }
    }
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    drop(summaries);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn full_fleet_inventory_survives_summary_wire_cache_and_last_slice() -> Result<()> {
    let (_home, host) = host()?;
    let identities = host
        .inner
        .registry
        .agent_identity_reads(&host.inner.input_evidence_budget, |record| {
            record.status == AgentRegistryStatus::Active
        })?;
    let (identity, inventory) = identities.into_iter().next().context("default identity")?;
    let guard = Arc::downgrade(&inventory.guard()?);
    let summary = host
        .local_agent_summary(&identity.agent_id)
        .await
        .map_err(anyhow::Error::from)?
        .with_fleet_inventory(&inventory)?;
    let expected = serde_json::to_value(&summary)?;
    drop(inventory);
    assert!(guard.upgrade().is_some());
    let bytes = summary.into_json_bytes()?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        expected
    );
    let cache = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    drop(bytes);
    assert!(guard.upgrade().is_some());
    drop(cache);
    assert!(guard.upgrade().is_some());
    drop(slice);
    assert!(guard.upgrade().is_none());
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn full_fleet_inventory_checks_scope_and_admits_holder_before_growth() -> Result<()> {
    let (_home, host) = host()?;
    let identity = host.ensure_default_agent_identity()?;
    let ledger = &host.inner.input_evidence_budget;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let summary = || async {
        host.local_agent_summary(&identity.agent_id)
            .await
            .map_err(anyhow::Error::from)
    };
    let measure = ResourceReadMaterial::with_test_limits(ledger, &scope, 4 * 1024 * 1024, 8192)?;
    let first = summary().await?.with_fleet_inventory(&measure)?;
    let usage = measure.finish()?;
    drop((first, measure));
    let exact = ResourceReadMaterial::with_test_limits(ledger, &scope, usage.0, usage.1)?;
    drop(summary().await?.with_fleet_inventory(&exact)?);
    assert_eq!(exact.finish()?, usage);
    drop(exact);
    for (bytes, records) in [(usage.0 - 1, usage.1), (usage.0, usage.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(ledger, &scope, bytes, records)?;
        let error = summary().await?.with_fleet_inventory(&phase).unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
        assert!(phase.finish().is_err());
        assert!(phase.read(|| Ok(())).is_err());
    }
    for foreign in [
        InputEvidenceScope {
            agent_id: "other-summary-agent".into(),
            ..scope.clone()
        },
        InputEvidenceScope {
            identity_incarnation: scope.identity_incarnation + 1,
            ..scope.clone()
        },
    ] {
        let phase =
            ResourceReadMaterial::with_test_limits(ledger, &foreign, 4 * 1024 * 1024, 8192)?;
        let before = phase.finish()?;
        let error = summary().await?.with_fleet_inventory(&phase).unwrap_err();
        assert!(format!("{error:#}").contains("inventory scope differs"));
        assert_eq!(phase.finish()?, before);
    }
    assert_eq!(ledger.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn fleet_public_filter_does_not_copy_excluded_inventory_or_activate_cold_agents() -> Result<()>
{
    let (_home, host) = host()?;
    register(&host, "selected-roster", "lineage-parent")?;
    let first = host
        .list_agent_entries_for_parent(Some("lineage-parent"))
        .await?;
    assert_eq!(first.len(), 1);
    let usage = first[0].material.finish()?;
    let expected = serde_json::to_value(&first)?;
    drop(first);
    for (id, parent, private, deleting) in [
        ("private-roster", "lineage-parent", true, false),
        ("unrelated-roster", "other-parent", false, false),
        ("deleting-roster", "lineage-parent", false, true),
    ] {
        let mut record = register(&host, id, parent)?;
        record.name = Some(format!("{id}{}", "unselected 汉🦀".repeat(8192)));
        if private {
            record.visibility = AgentVisibility::Private;
        }
        if deleting {
            record.status = AgentRegistryStatus::Deleting;
        }
        host.append_agent_identity(&record)?;
        host.runtime_db().agent_identities().upsert(&record)?;
    }
    let next = host
        .list_agent_entries_for_parent(Some("lineage-parent"))
        .await?;
    assert_eq!(serde_json::to_value(&next)?, expected);
    assert_eq!(next[0].material.finish()?, usage);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    drop(next);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn fleet_wire_keeps_every_original_acquisition_until_cache_and_slice_release() -> Result<()> {
    let (_home, host) = host()?;
    register(&host, "second-wire-agent", "wire-parent")?;
    let entries = host.list_agent_entries().await?;
    let expected = serde_json::to_value(&entries)?;
    let guards = entries
        .iter()
        .map(|entry| Ok(Arc::downgrade(&entry.material.guard()?)))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(guards.len(), 2);
    let bytes = entries.into_json_bytes()?;
    let cache = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        expected
    );
    drop(bytes);
    assert!(guards.iter().all(|guard| guard.upgrade().is_some()));
    drop(cache);
    assert!(guards.iter().all(|guard| guard.upgrade().is_some()));
    drop(slice);
    assert!(guards.iter().all(|guard| guard.upgrade().is_none()));
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    let empty = host
        .list_agent_entries_for_parent(Some("no-such-parent"))
        .await?;
    assert_eq!(empty.into_json_bytes()?.as_ref(), b"[]");
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn fleet_wire_exact_cumulative_allowance_and_one_short_latch() -> Result<()> {
    let (_home, host) = host()?;
    let original = host.list_agent_entries().await?;
    let scope = InputEvidenceScope {
        agent_id: original[0].identity.agent_id.clone(),
        identity_incarnation: original[0].identity.incarnation,
    };
    let ledger = &host.inner.input_evidence_budget;
    let product = |phase: &ResourceReadMaterial| -> Result<_> {
        phase
            .read(|| {
                resource_io::admit_cloned_value(&original[0].entry)?;
                let mut entries = Vec::new();
                crate::runtime_db::repositories::reserve_source_vec_slot(&mut entries)?;
                entries.push(OwnedAgentListEntry {
                    entry: original[0].entry.clone(),
                    material: phase.clone(),
                });
                Ok(OwnedAgentListRead { entries })
            })?
            .into_json_bytes()
    };
    let measure = ResourceReadMaterial::with_test_limits(ledger, &scope, 4 * 1024 * 1024, 8192)?;
    let bytes = product(&measure)?;
    let usage = measure.finish()?;
    drop((bytes, measure));
    let exact = ResourceReadMaterial::with_test_limits(ledger, &scope, usage.0, usage.1)?;
    let bytes = product(&exact)?;
    assert_eq!(exact.finish()?, usage);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        serde_json::to_value(&original)?
    );
    drop((bytes, exact));
    for (bytes, records) in [(usage.0 - 1, usage.1), (usage.0, usage.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(ledger, &scope, bytes, records)?;
        let error = product(&phase).unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
        assert!(phase.finish().is_err());
        assert!(phase.read(|| Ok(())).is_err());
    }
    drop(original);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}
