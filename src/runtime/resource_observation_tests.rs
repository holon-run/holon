use super::input_evidence_budget::{
    InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};
use super::resource_observation::*;
use super::*;
use crate::types::{
    AgentsMdKind, AgentsMdScope, AgentsMdSource, LoadedAgentMemory, LoadedAgentsMd,
    SkillsRuntimeView,
};
use std::collections::BTreeMap;

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(Default::default())?)
}
fn source(workspace: &str) -> ContextResourceSource {
    ContextResourceSource {
        scope: super::resident_input_evidence::InputEvidenceScope {
            agent_id: "resource-agent".into(),
            identity_incarnation: 1,
        },
        agent_home: "/resource-agent".into(),
        user_home: None,
        workspace_id: Some(workspace.into()),
        workspace_anchor: Some(format!("/workspace/{workspace}").into()),
        execution_root_id: Some(format!("root:{workspace}")),
        execution_root: Some(format!("/workspace/{workspace}").into()),
        include_user_skills: false,
        config_generation: 0,
        host_write_generation: None,
        work_revision: None,
        plans: Vec::new(),
    }
}
fn body(text: Option<&str>) -> ContextResourceBody {
    let mut guidance = LoadedAgentsMd::default();
    guidance.agent_source = text.map(|content| AgentsMdSource {
        scope: AgentsMdScope::Agent,
        kind: AgentsMdKind::AgentsMd,
        path: "/resource-agent/AGENTS.md".into(),
        content: content.into(),
    });
    ContextResourceBody {
        guidance,
        memory: LoadedAgentMemory::default(),
        skills: SkillsRuntimeView::default(),
        notes: None,
        notes_refresh_error: None,
        source_versions: BTreeMap::new(),
    }
}
fn material(
    ledger: &InputEvidenceBudgetLedger,
    source: &ContextResourceSource,
) -> Result<Arc<super::input_evidence_budget::BudgetViewGuard>> {
    Ok(ledger.register_owned_bytes(
        &source.scope,
        InputMemoryCategory::ResourceObservation,
        OwnedBytesEstimate {
            payload_bytes: 512,
            metadata_estimate_bytes: 512,
        },
    )?)
}
fn install_ticket(
    registry: &ContextResourceRegistry,
    ledger: &InputEvidenceBudgetLedger,
    source: &ContextResourceSource,
    ticket: ResourceRefreshTicket,
    body: ContextResourceBody,
) -> Result<Arc<ResourceObservation>> {
    let (bytes, records) = body.retained_material()?;
    let guard = ledger.register_owned_bytes(
        &source.scope,
        InputMemoryCategory::ResourceObservation,
        OwnedBytesEstimate {
            payload_bytes: bytes,
            metadata_estimate_bytes: 0,
        },
    )?;
    registry.install(ticket, source, body, BTreeMap::new(), bytes, records, guard)
}
fn install(
    registry: &ContextResourceRegistry,
    ledger: &InputEvidenceBudgetLedger,
    source: &ContextResourceSource,
    text: Option<&str>,
) -> Result<Arc<ResourceObservation>> {
    let mut ticket = registry.begin_refresh(source.clone(), material(ledger, source)?)?;
    let body = body(text);
    registry.observe_refresh_change(&mut ticket, &body.digest()?)?;
    install_ticket(registry, ledger, source, ticket, body)
}
#[test]
fn dirty_failure_absent_and_old_snapshot_are_distinct() -> Result<()> {
    let ledger = ledger()?;
    let source = source("a");
    let registry = ContextResourceRegistry::default();
    let absent = install(&registry, &ledger, &source, None)?;
    assert!(absent.body.guidance.agent_source.is_none());
    let old = registry.capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)?;
    registry.invalidate(ResourceInvalidation::KnownWrite)?;
    assert!(matches!(
        registry.freshness()?,
        ResourceFreshness::Dirty(ResourceInvalidation::KnownWrite)
    ));
    assert!(registry
        .capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)
        .is_err());
    let failed = registry.begin_refresh(source.clone(), material(&ledger, &source)?)?;
    registry.fail(failed, "permission denied".into())?;
    assert!(matches!(
        registry.freshness()?,
        ResourceFreshness::RefreshFailed(_)
    ));
    assert!(registry
        .capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)
        .is_err());
    assert!(Arc::ptr_eq(&old.observation, &absent.core));
    let present = install(&registry, &ledger, &source, Some("original content"))?;
    assert!(present.epoch > absent.epoch);
    assert_eq!(old.observation.body.guidance.agent_source, None);
    assert_eq!(
        registry
            .capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)?
            .guidance()
            .agent_source
            .unwrap()
            .content,
        "original content"
    );
    Ok(())
}
#[test]
fn late_refresh_cannot_overwrite_new_source_generation_or_newer_read() -> Result<()> {
    let ledger = ledger()?;
    let a = source("a");
    let b = source("b");
    let registry = ContextResourceRegistry::default();
    let old = install(&registry, &ledger, &a, Some("old"))?;
    let late = registry.begin_refresh(a.clone(), material(&ledger, &a)?)?;
    registry.invalidate(ResourceInvalidation::WatchOverflow)?;
    assert!(registry
        .capture(&a, &ledger, 65536, 8 * 1024 * 1024, None)
        .is_err());
    let current = install(&registry, &ledger, &b, Some("workspace b"))?;
    assert!(install_ticket(&registry, &ledger, &a, late, body(Some("late a"))).is_err());
    assert!(registry
        .capture(&a, &ledger, 65536, 8 * 1024 * 1024, None)
        .is_err());
    assert_eq!(
        registry
            .capture(&b, &ledger, 65536, 8 * 1024 * 1024, None)?
            .guidance()
            .agent_source
            .unwrap()
            .content,
        "workspace b"
    );
    let first = registry.begin_refresh(b.clone(), material(&ledger, &b)?)?;
    let second = registry.begin_refresh(b.clone(), material(&ledger, &b)?)?;
    install_ticket(&registry, &ledger, &b, second, body(Some("newer read")))?;
    assert!(install_ticket(
        &registry,
        &ledger,
        &b,
        first,
        body(Some("stale read in same generation"))
    )
    .is_err());
    let mut successor = b.clone();
    successor.scope.identity_incarnation = 2;
    assert!(registry
        .capture(&successor, &ledger, 65536, 8 * 1024 * 1024, None)
        .is_err());
    assert_eq!(
        old.body.guidance.agent_source.as_ref().unwrap().content,
        "old"
    );
    assert_eq!(
        current.body.guidance.agent_source.as_ref().unwrap().content,
        "workspace b"
    );
    Ok(())
}
#[test]
fn content_digest_and_final_pins_release_cover_private_guidance_bytes() -> Result<()> {
    let first = body(Some("first private guidance"));
    let second = body(Some("second private guidance"));
    assert_eq!(
        serde_json::to_value(&first.guidance)?,
        serde_json::to_value(&second.guidance)?
    );
    assert_ne!(
        first.digest()?,
        second.digest()?,
        "public metadata-only Serialize cannot stand in for content digest"
    );
    let ledger = ledger()?;
    let source = source("a");
    let registry = ContextResourceRegistry::default();
    let value = install(&registry, &ledger, &source, Some("retained"))?;
    let weak = Arc::downgrade(&value.core);
    let pin = registry.capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)?;
    drop(value);
    registry.close()?;
    assert!(weak.upgrade().is_some());
    assert!(registry
        .capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)
        .is_err());
    assert!(ledger.report()?.owned.payload_bytes > 0);
    drop(pin);
    assert!(weak.upgrade().is_none());
    let report = ledger.report()?;
    assert_eq!(report.owned.payload_bytes, 0);
    assert_eq!(report.owned.metadata_estimate_bytes, 0);
    Ok(())
}
#[test]
fn oversized_invalid_utf8_and_directory_count_reject_before_owned_decode() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let ledger = ledger()?;
    let scope = source("a").scope;
    let bad = directory.path().join("invalid-guidance");
    std::fs::write(&bad, vec![0xff; 65536])?;
    let baseline = ledger.report()?.owned.payload_bytes;
    {
        let material = ledger.reserve_owned_completion(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate::default(),
        )?;
        let budget =
            crate::diagnostics::resource_io::ResourceReadBudget::enter(material, 8192, 1024)?;
        let error = crate::diagnostics::io_probe::read_to_string(&bad).unwrap_err();
        assert!(error.to_string().contains("ResourceExhausted"), "{error}");
        assert!(budget.finish().is_err());
    }
    assert_eq!(ledger.report()?.owned.payload_bytes, baseline);
    for n in 0..4 {
        std::fs::write(directory.path().join(format!("entry-{n}")), "")?;
    }
    {
        let material = ledger.reserve_owned_completion(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate::default(),
        )?;
        let budget =
            crate::diagnostics::resource_io::ResourceReadBudget::enter(material, 1024 * 1024, 16)?;
        let rows = crate::diagnostics::io_probe::read_dir(directory.path())?
            .collect::<std::io::Result<Vec<_>>>();
        assert!(rows.unwrap_err().to_string().contains("ResourceExhausted"));
        assert!(budget.finish().is_err());
    }
    assert_eq!(ledger.report()?.owned.metadata_estimate_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn ordinary_resource_capture_retains_bytes_when_reader_capability_is_denied() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("AGENTS.md");
    std::fs::write(&file, "observed then independently edited")?;
    let ledger = ledger()?;
    let source = source("a");
    let registry = ContextResourceRegistry::default();
    install(&registry, &ledger, &source, Some("observed guidance"))?;
    std::fs::write(&file, "new unobserved content")?;
    crate::diagnostics::resource_io::deny_readers(async {
        let read = registry.capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)?;
        assert_eq!(
            read.guidance().agent_source.unwrap().content,
            "observed guidance"
        );
        let error = crate::diagnostics::io_probe::read_to_string(&file).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        Ok::<_, anyhow::Error>(())
    })
    .await?;
    Ok(())
}

#[test]
fn exhausted_invalidation_cannot_reinstall_the_preexisting_last_generation_ticket() -> Result<()> {
    let ledger = ledger()?;
    let source = source("overflow");
    let registry = ContextResourceRegistry::default();
    install(
        &registry,
        &ledger,
        &source,
        Some("original before overflow"),
    )?;
    registry.set_max_generation_for_test()?;
    let ticket = registry.begin_refresh(source.clone(), material(&ledger, &source)?)?;
    assert!(registry
        .invalidate(ResourceInvalidation::WatchOverflow)
        .is_err());
    assert!(install_ticket(
        &registry,
        &ledger,
        &source,
        ticket,
        body(Some("late old generation"))
    )
    .is_err());
    assert!(registry
        .capture(&source, &ledger, 65536, 8 * 1024 * 1024, None)
        .is_err());
    assert!(registry
        .begin_refresh(source.clone(), material(&ledger, &source)?)
        .is_err());
    Ok(())
}
