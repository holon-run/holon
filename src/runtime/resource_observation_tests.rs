use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::io_probe::{self, IoProbe},
    runtime::mutation_owner::MutationOwner,
    types::AgentMemorySource,
};
use std::cell::{Cell, RefCell};

fn fixture() -> (ContextResourceRegistry, ContextResourceSource) {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default()).unwrap();
    (ContextResourceRegistry::new(ledger), source())
}

fn source() -> ContextResourceSource {
    ContextResourceSource {
        scope: InputEvidenceScope {
            agent_id: "resource-core".into(),
            identity_incarnation: 1,
        },
        agent_home: "/agent".into(),
        user_home: Some("/user".into()),
        workspace_id: Some("workspace-a".into()),
        workspace_anchor: Some("/workspace".into()),
        execution_root_id: Some("root-a".into()),
        execution_root: Some("/workspace/worktree".into()),
        include_user_skills: true,
        config_generation: 1,
        host_generation: 0,
        active_skills_selector: [0; 32],
        work_revision: 0,
        plans: Vec::new(),
    }
}

fn reserve(source: &ContextResourceSource) -> Result<MutationReservation> {
    Ok(
        MutationOwner::standalone(&source.scope.agent_id, source.scope.identity_incarnation)
            .try_reserve()?,
    )
}

fn body(text: &str) -> Result<ContextResourceBody> {
    resource_io::charge_owned_metadata(text.len() as u64, 1)?;
    Ok(ContextResourceBody {
        notes: Some(text.to_owned()),
        ..Default::default()
    })
}

fn refresh(
    registry: &ContextResourceRegistry,
    source: &ContextResourceSource,
    text: &str,
) -> Arc<ResourceObservation> {
    registry
        .refresh(source, || body(text), || reserve(&source), |_, _| Ok(()))
        .unwrap()
}

fn raw_candidate(
    registry: &ContextResourceRegistry,
    source: &ContextResourceSource,
    text: &str,
) -> (ContextResourceBody, ResourceReadMaterial) {
    let phase = ResourceReadMaterial::new(&registry.ledger, &source.scope).unwrap();
    let value = {
        let _io = phase.enter();
        resource_io::charge_owned_metadata(8192, 128).unwrap();
        body(text).unwrap()
    };
    (value, phase)
}

#[test]
fn context_resource_projection_borrows_catalog_and_admits_private_bodies() -> Result<()> {
    let (registry, source) = fixture();
    let catalog = Arc::new(vec![crate::types::SkillCatalogEntry {
        skill_id: "shared".into(),
        root_id: "root".into(),
        skill_dir: "shared".into(),
        name: "shared".into(),
        description: "catalog body".repeat(4096),
        path: "/skills/shared/SKILL.md".into(),
        scope: crate::types::SkillScope::Agent,
    }]);
    let value = ContextResourceBody {
        skills: SkillsRuntimeView {
            discoverable_skills: catalog.clone(),
            attached_skills: catalog.clone(),
            ..Default::default()
        },
        memory: LoadedAgentMemory {
            operator_source: Some(AgentMemorySource {
                path: "/memory/operator.md".into(),
                content: "private memory".repeat(4096),
                truncated: false,
                total_chars: 0,
            }),
            ..Default::default()
        },
        notes: Some("private notes".repeat(4096)),
        ..Default::default()
    };
    let phase =
        ResourceReadMaterial::with_test_limits(&registry.ledger, &source.scope, 64 * 1024, 4096)?;
    let _entry = phase.enter();
    let before = phase.finish()?;
    value.admit_skills_projection()?;
    // The only cloned fields serialize to [[],[],[]]: 10 bytes, 10 slots.
    assert_eq!(
        phase.finish()?,
        (
            before.0 + (4 * 10 + 128 * 10 + 512) * 8,
            before.1 + (10 + 16) * 8
        )
    );
    let skills = value.skills.clone();
    assert!(Arc::ptr_eq(&skills.discoverable_skills, &catalog));
    assert!(Arc::ptr_eq(&skills.attached_skills, &catalog));
    assert!(value.admit_guidance_memory_projection().is_err());
    assert!(phase.finish().is_err());
    drop(_entry);
    drop(phase);
    let phase =
        ResourceReadMaterial::with_test_limits(&registry.ledger, &source.scope, 64 * 1024, 4096)?;
    let _entry = phase.enter();
    assert!(value.admit_plan_notes_projection().is_err());
    assert!(phase.finish().is_err());
    Ok(())
}

#[test]
fn prompt_resources_keep_original_private_bodies_until_the_last_holder() -> Result<()> {
    use crate::types::{AgentsMdKind, AgentsMdScope, AgentsMdSource};
    let (registry, source) = fixture();
    let ledger = registry.ledger.clone();
    let observation = registry.refresh(
        &source,
        || {
            resource_io::charge_owned_metadata(32 * 1024, 16)?;
            Ok(ContextResourceBody {
                guidance: LoadedAgentsMd {
                    agent_source: Some(AgentsMdSource {
                        scope: AgentsMdScope::Agent,
                        kind: AgentsMdKind::AgentsMd,
                        path: "/agent/AGENTS.md".into(),
                        content: "private guidance".repeat(1024),
                    }),
                    ..Default::default()
                },
                memory: LoadedAgentMemory {
                    self_source: Some(AgentMemorySource {
                        path: "/agent/memory/self.md".into(),
                        content: "private memory".repeat(1024),
                        truncated: false,
                        total_chars: 14 * 1024,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            })
        },
        || reserve(&source),
        |_, _| Ok(()),
    )?;
    let guidance =
        crate::prompt::PromptResource::observed(observation.clone(), |body| &body.guidance);
    let memory = crate::prompt::PromptResource::observed(observation.clone(), |body| &body.memory);
    assert_eq!(
        serde_json::to_value(&guidance)?,
        serde_json::to_value(&observation.body().guidance)?
    );
    assert_eq!(
        serde_json::to_value(&memory)?,
        serde_json::to_value(&observation.body().memory)?
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &source.scope, 4096, 128)?;
    let entry = phase.enter();
    let before = phase.finish()?;
    let last = memory.clone();
    let guidance_clone = guidance.clone();
    assert!(std::ptr::eq(&*guidance_clone, &observation.body().guidance));
    assert!(std::ptr::eq(&*last, &observation.body().memory));
    assert_eq!(phase.finish()?, before);
    assert!(observation
        .body()
        .admit_guidance_memory_projection()
        .is_err());
    assert!(phase.finish().is_err());
    drop((entry, phase));
    registry.close()?;
    drop((registry, observation, guidance, guidance_clone, memory));
    assert!(ledger.report()?.observed_bytes > 0);
    assert_eq!(last.self_source.as_ref().unwrap().content.len(), 14 * 1024);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn ticket(
    registry: &ContextResourceRegistry,
    source: &ContextResourceSource,
) -> ResourceRefreshTicket {
    registry
        .begin_refresh(PreparedSource::new(source, &registry.ledger).unwrap())
        .unwrap()
}

#[test]
fn dirty_failure_absent_and_old_snapshot_are_distinct() {
    let (registry, source) = fixture();
    assert_eq!(registry.freshness().unwrap(), ResourceFreshness::Unobserved);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("optional-notes");
    let absent = registry
        .refresh(
            &source,
            || {
                Ok(ContextResourceBody {
                    notes: read_optional_text(&path)?,
                    ..Default::default()
                })
            },
            || reserve(&source),
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(absent.body().notes.is_none());
    registry
        .invalidate(ResourceInvalidation::KnownWrite)
        .unwrap();
    assert!(matches!(
        registry.capture(&source).unwrap_err().downcast_ref(),
        Some(ResourceObservationError::NotReady(
            ResourceFreshness::Dirty(_)
        ))
    ));
    assert!(absent.body().notes.is_none());
    let error = registry
        .refresh(
            &source,
            || Err(io::Error::from(io::ErrorKind::PermissionDenied).into()),
            || -> Result<MutationReservation> { panic!("failed reads must not acquire the owner") },
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        registry.freshness().unwrap(),
        ResourceFreshness::RefreshFailed
    );
    assert!(registry.capture(&source).is_err());
    let present = refresh(&registry, &source, "new notes");
    assert!(present.epoch() > absent.epoch());
    assert!(present.generation() > absent.generation());
    assert_eq!(present.body().notes.as_deref(), Some("new notes"));
    assert!(absent.body().notes.is_none());
    registry.close().unwrap();
    assert!(registry.capture(&source).is_err());
    assert_eq!(present.body().notes.as_deref(), Some("new notes"));
}

#[test]
fn late_refresh_cannot_overwrite_new_source_generation_or_newer_read() {
    for variant in 0..5 {
        let (registry, source) = fixture();
        refresh(&registry, &source, "initial");
        let late = ticket(&registry, &source);
        let mut current = source.clone();
        match variant {
            0 => current.scope.identity_incarnation += 1,
            1 => {
                current.workspace_id = Some("workspace-b".into());
                current.execution_root_id = Some("root-b".into());
            }
            2 => current.config_generation += 1,
            3 => registry
                .invalidate(ResourceInvalidation::ExternalEdit)
                .unwrap(),
            _ => {}
        }
        let newest = refresh(&registry, &current, "newest");
        let (old_body, phase) = raw_candidate(&registry, &source, "late");
        assert!(matches!(
            registry
                .install(late, SealedResourceBody::new(old_body, &phase).unwrap())
                .unwrap_err()
                .downcast_ref(),
            Some(ResourceObservationError::ObsoleteRefresh)
        ));
        assert!(Arc::ptr_eq(&newest, &registry.capture(&current).unwrap()));
        // A superseded read failure must not overwrite the newest Ready state.
        let failed = ticket(&registry, &current);
        let latest = refresh(&registry, &current, "latest");
        registry.fail(failed).unwrap();
        assert_eq!(registry.freshness().unwrap(), ResourceFreshness::Ready);
        assert!(Arc::ptr_eq(&latest, &registry.capture(&current).unwrap()));
    }
}

#[test]
fn buffered_resource_digest_matches_exact_json_bytes() -> Result<()> {
    for repeats in [0, 1, 127, 128, 129, 4096] {
        let text = "\"\\\n中文".repeat(repeats);
        let value = ContextResourceBody {
            notes: Some(text.clone()),
            memory: LoadedAgentMemory {
                self_source: Some(AgentMemorySource {
                    path: "self.md".into(),
                    total_chars: text.chars().count(),
                    content: text,
                    truncated: false,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let serialized = serde_json::to_vec(&(
            &value.guidance,
            value
                .guidance
                .user_global_source
                .as_ref()
                .map(|s| &s.content),
            value.guidance.agent_source.as_ref().map(|s| &s.content),
            value.guidance.workspace_source.as_ref().map(|s| &s.content),
            &value.memory,
            value.memory.operator_source.as_ref().map(|s| &s.content),
            value.memory.self_source.as_ref().map(|s| &s.content),
            &value.skills,
            &value.notes,
            &value.plans,
        ))?;
        let expected: [u8; 32] = Sha256::digest(&serialized).into();
        assert_eq!(value.digest()?, expected, "repeats={repeats}");
    }
    Ok(())
}

#[test]
fn content_digest_and_final_pins_release_cover_private_guidance_bytes() {
    let (registry, mut source) = fixture();
    let ledger = registry.ledger.clone();
    let dir = tempfile::tempdir().unwrap();
    source.agent_home = dir.path().to_owned();
    let path = dir.path().join("AGENTS.md");
    let read = || -> Result<ContextResourceBody> {
        let guidance = crate::agents_md::load_agents_md(None, dir.path(), None)?;
        // This synthetic memory source obeys the same before-allocation gate.
        resource_io::charge_owned_metadata((14 * 1024 + 1024) as u64, 8)?;
        let content = "private-memory".repeat(1024);
        Ok(ContextResourceBody {
            guidance,
            memory: LoadedAgentMemory {
                self_source: Some(AgentMemorySource {
                    path: "self.md".into(),
                    content,
                    truncated: false,
                    total_chars: 14 * 1024,
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    };
    std::fs::write(&path, "private-guidance-a".repeat(4096)).unwrap();
    let first = registry
        .refresh(&source, read, || reserve(&source), |_, _| Ok(()))
        .unwrap();
    let first_pin = first.clone();
    let first_digest = *first.digest();
    let first_public = serde_json::to_value(&first.body().guidance).unwrap();
    std::fs::write(&path, "private-guidance-b".repeat(4096)).unwrap();
    let second = registry
        .refresh(&source, read, || reserve(&source), |_, _| Ok(()))
        .unwrap();
    assert_ne!(&first_digest, second.digest());
    assert_eq!(
        first_public,
        serde_json::to_value(&second.body().guidance).unwrap()
    );
    assert!(
        ledger.report().unwrap().observed_bytes
            > (first
                .body()
                .guidance
                .agent_source
                .as_ref()
                .unwrap()
                .content
                .len()
                + second
                    .body()
                    .guidance
                    .agent_source
                    .as_ref()
                    .unwrap()
                    .content
                    .len()) as u64
    );
    let peak = ledger.report().unwrap().observed_bytes;
    drop(first);
    assert_eq!(ledger.report().unwrap().observed_bytes, peak);
    registry.close().unwrap();
    drop(registry);
    assert!(ledger.report().unwrap().observed_bytes > 0);
    drop(second);
    assert!(ledger.report().unwrap().observed_bytes > 0);
    drop(first_pin);
    assert_eq!(ledger.report().unwrap().observed_bytes, 0);
}

#[test]
fn every_private_guidance_and_memory_record_participates_in_digest_and_material() {
    use crate::types::{AgentsMdKind, AgentsMdScope, AgentsMdSource};
    fn hidden_body(changed: Option<usize>) -> Result<ContextResourceBody> {
        resource_io::charge_owned_metadata(6 * 4096, 32)?;
        let text = |index| if changed == Some(index) { "b" } else { "a" }.repeat(4096);
        let guidance = |index, scope| AgentsMdSource {
            scope,
            kind: AgentsMdKind::AgentsMd,
            path: "AGENTS.md".into(),
            content: text(index),
        };
        let memory = |index| AgentMemorySource {
            path: "memory.md".into(),
            content: text(index),
            truncated: false,
            total_chars: 4096,
        };
        Ok(ContextResourceBody {
            guidance: LoadedAgentsMd {
                user_global_source: Some(guidance(0, AgentsMdScope::UserGlobal)),
                agent_source: Some(guidance(1, AgentsMdScope::Agent)),
                workspace_source: Some(guidance(2, AgentsMdScope::Workspace)),
                ..Default::default()
            },
            memory: LoadedAgentMemory {
                operator_source: Some(memory(3)),
                self_source: Some(memory(4)),
                ..Default::default()
            },
            ..Default::default()
        })
    }
    let (registry, source) = fixture();
    let original = registry
        .refresh(
            &source,
            || hidden_body(None),
            || reserve(&source),
            |_, _| Ok(()),
        )
        .unwrap();
    let (bytes, records) = original.body().retained_material().unwrap();
    assert!(bytes >= 5 * 4096 && records >= 26);
    let public =
        serde_json::to_value((&original.body().guidance, &original.body().memory)).unwrap();
    for slot in 0..5 {
        let changed = registry
            .refresh(
                &source,
                || hidden_body(Some(slot)),
                || reserve(&source),
                |_, _| Ok(()),
            )
            .unwrap();
        assert_ne!(original.digest(), changed.digest(), "private slot {slot}");
        assert_eq!(
            public,
            serde_json::to_value((&changed.body().guidance, &changed.body().memory)).unwrap()
        );
    }
}

#[test]
fn exhausted_invalidation_cannot_reinstall_the_preexisting_last_generation_ticket() {
    let (registry, source) = fixture();
    let old = refresh(&registry, &source, "stable");
    registry.lock().unwrap().generation = u64::MAX;
    let last_ticket = ticket(&registry, &source);
    assert!(matches!(
        registry
            .invalidate(ResourceInvalidation::WatchOverflow)
            .unwrap_err()
            .downcast_ref(),
        Some(ResourceObservationError::GenerationExhausted)
    ));
    let (body, phase) = raw_candidate(&registry, &source, "stale");
    assert!(registry
        .install(last_ticket, SealedResourceBody::new(body, &phase).unwrap())
        .is_err());
    assert!(registry.capture(&source).is_err());
    assert!(registry
        .begin_refresh(PreparedSource::new(&source, &registry.ledger).unwrap())
        .is_err());
    assert!(matches!(
        registry.freshness().unwrap(),
        ResourceFreshness::Dirty(_)
    ));
    assert_eq!(old.body().notes.as_deref(), Some("stable"));
}

#[test]
fn epoch_and_nonce_exhaustion_permanently_fence_older_tickets() {
    for nonce_overflow in [true, false] {
        let (registry, source) = fixture();
        let old = refresh(&registry, &source, "stable");
        let late = ticket(&registry, &source);
        if nonce_overflow {
            registry.lock().unwrap().nonce = u64::MAX;
            assert!(registry
                .begin_refresh(PreparedSource::new(&source, &registry.ledger).unwrap())
                .is_err());
        } else {
            registry.lock().unwrap().epoch = u64::MAX;
            assert!(registry
                .refresh(
                    &source,
                    || body("overflow"),
                    || reserve(&source),
                    |_, _| Ok(())
                )
                .is_err());
        }
        let (body, phase) = raw_candidate(&registry, &source, "late");
        assert!(registry
            .install(late, SealedResourceBody::new(body, &phase).unwrap())
            .is_err());
        assert!(registry.capture(&source).is_err());
        assert_eq!(old.body().notes.as_deref(), Some("stable"));
    }
}

#[test]
fn oversized_invalid_utf8_and_directory_count_reject_before_owned_decode() {
    let dir = tempfile::tempdir().unwrap();
    let oversized = dir.path().join("large");
    std::fs::write(&oversized, vec![0xff; 1024 * 1024]).unwrap();
    let small_invalid = dir.path().join("invalid");
    std::fs::write(&small_invalid, [0xff]).unwrap();
    let limits = StateMemoryLimits {
        history_per_agent_bytes: 128 * 1024,
        pending_per_agent_bytes: 1,
        record_limit: 1600,
        ..Default::default()
    };
    let ledger = InputEvidenceBudgetLedger::new(limits).unwrap();
    let registry = ContextResourceRegistry::new(ledger);
    let source = source();
    let probe = IoProbe::start();
    let error = registry
        .refresh(
            &source,
            || {
                Ok(ContextResourceBody {
                    notes: read_optional_text(&oversized)?,
                    ..Default::default()
                })
            },
            || -> Result<MutationReservation> { panic!("quota failure precedes owner") },
            |_, _| Ok(()),
        )
        .unwrap_err();
    let described = crate::runtime_error::describe_runtime_error(&error);
    assert_eq!(described.code, "context_resource_exhausted");
    assert_eq!(probe.snapshot().counters.file_read_bytes, 0);
    assert_eq!(
        registry.freshness().unwrap(),
        ResourceFreshness::RefreshFailed
    );
    let error = registry
        .refresh(
            &source,
            || {
                Ok(ContextResourceBody {
                    notes: read_optional_text(&small_invalid)?,
                    ..Default::default()
                })
            },
            || reserve(&source),
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::InvalidData
    );
    let registry = ContextResourceRegistry::new(
        InputEvidenceBudgetLedger::new(StateMemoryLimits {
            record_limit: 1600,
            ..Default::default()
        })
        .unwrap(),
    );
    for index in 0..8 {
        std::fs::write(dir.path().join(format!("entry-{index}")), "").unwrap();
    }
    let decoded = Cell::new(0usize);
    let admitted = Cell::new(0usize);
    let error = registry
        .refresh(
            &source,
            || {
                // Iterator admission must fail before this collected inventory is decoded.
                let entries = io_probe::read_dir(dir.path())?
                    .inspect(|entry| {
                        if entry.is_ok() {
                            admitted.set(admitted.get() + 1);
                        }
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                decoded.set(entries.len());
                Ok(ContextResourceBody::default())
            },
            || reserve(&source),
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert_eq!(
        crate::runtime_error::describe_runtime_error(&error).code,
        "context_resource_exhausted"
    );
    assert_eq!(decoded.get(), 0);
    assert_eq!(admitted.get(), 2);
}

#[test]
fn ordinary_resource_capture_retains_bytes_when_reader_capability_is_denied() {
    let (registry, source) = fixture();
    let allowed = Cell::new(true);
    let attempts = Cell::new(0);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes");
    std::fs::write(&path, "already observed").unwrap();
    let read = || -> Result<ContextResourceBody> {
        attempts.set(attempts.get() + 1);
        if !allowed.get() {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
        Ok(ContextResourceBody {
            notes: read_optional_text(&path)?,
            ..Default::default()
        })
    };
    let old = registry
        .refresh(&source, read, || reserve(&source), |_, _| Ok(()))
        .unwrap();
    allowed.set(false);
    let probe = IoProbe::start();
    let captured = registry.capture(&source).unwrap();
    assert!(Arc::ptr_eq(&old, &captured));
    assert_eq!(captured.body().notes.as_deref(), Some("already observed"));
    assert_eq!(attempts.get(), 1);
    let counters = probe.snapshot().counters;
    assert_eq!(counters.file_read_attempts, 0);
    assert_eq!(counters.file_open_attempts, 0);
    assert_eq!(counters.directory_read_attempts, 0);
    let error = registry
        .refresh(&source, read, || reserve(&source), |_, _| Ok(()))
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        registry.freshness().unwrap(),
        ResourceFreshness::RefreshFailed
    );
    assert_eq!(captured.body().notes.as_deref(), Some("already observed"));
}

#[test]
fn refresh_ends_io_before_owner_and_busy_preserves_ready_or_dirty() {
    let (registry, source) = fixture();
    let owner =
        MutationOwner::standalone(&source.scope.agent_id, source.scope.identity_incarnation);
    let order = RefCell::new(Vec::new());
    let old = registry
        .refresh(
            &source,
            || {
                order.borrow_mut().push("read");
                assert!(owner.try_reserve().is_ok());
                body("stable")
            },
            || {
                order.borrow_mut().push("owner");
                assert!(resource_io::read_to_end(&mut io::empty(), &mut Vec::new()).is_none());
                Ok(owner.try_reserve()?)
            },
            |reservation, current| {
                order.borrow_mut().push("validate");
                assert_eq!(current, &source);
                assert_eq!(
                    reservation.identity_incarnation(),
                    source.scope.identity_incarnation
                );
                assert!(matches!(owner.try_reserve(), Err(MutationOwnerError::Busy)));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(*order.borrow(), ["read", "owner", "validate"]);
    for dirty in [false, true] {
        if dirty {
            registry
                .invalidate(ResourceInvalidation::WatchRestart)
                .unwrap();
        }
        let posture = registry.freshness().unwrap();
        let _held = owner.try_reserve().unwrap();
        let error = registry
            .refresh(
                &source,
                || body("replacement"),
                || Ok(owner.try_reserve()?),
                |_, _| -> Result<()> { panic!("owner is busy") },
            )
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(MutationOwnerError::Busy)
        ));
        assert_eq!(registry.freshness().unwrap(), posture);
        assert_eq!(old.body().notes.as_deref(), Some("stable"));
    }
}

#[test]
fn rejected_old_source_entering_after_new_publication_preserves_ready_snapshot() {
    let (registry, stale) = fixture();
    let mut current = stale.clone();
    current.execution_root_id = Some("root-b".into());
    let published = refresh(&registry, &current, "current");
    let generation = registry.lock().unwrap().generation;
    let error = registry
        .refresh(
            &stale,
            || body("stale"),
            || reserve(&stale),
            |_, candidate| {
                assert_eq!(candidate, &stale);
                Err(ResourceObservationError::SourceChanged.into())
            },
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(ResourceObservationError::SourceChanged)
    ));
    assert_eq!(registry.freshness().unwrap(), ResourceFreshness::Ready);
    assert_eq!(registry.lock().unwrap().generation, generation);
    assert!(Arc::ptr_eq(
        &published,
        &registry.capture(&current).unwrap()
    ));
    assert_eq!(published.body().notes.as_deref(), Some("current"));
}

#[test]
fn inventory_contention_preserves_ready_or_dirty_at_both_publication_stages() {
    for dirty in [false, true] {
        for before_validation in [false, true] {
            let (registry, source) = fixture();
            let published = refresh(&registry, &source, "stable");
            if dirty {
                registry
                    .invalidate(ResourceInvalidation::KnownWrite)
                    .unwrap();
            }
            let posture = registry.freshness().unwrap();
            let error = registry
                .refresh(
                    &source,
                    || body("replacement"),
                    || {
                        if before_validation {
                            Err(ResourceObservationError::InventoryContended.into())
                        } else {
                            reserve(&source)
                        }
                    },
                    |_, _| -> Result<()> {
                        Err(ResourceObservationError::InventoryContended.into())
                    },
                )
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref(),
                Some(ResourceObservationError::InventoryContended)
            ));
            assert_eq!(registry.freshness().unwrap(), posture);
            assert!(Arc::ptr_eq(
                registry.lock().unwrap().value.as_ref().unwrap(),
                &published
            ));
        }
    }
}

#[test]
fn inventory_snapshot_conflict_is_a_real_failed_ticket_not_lock_contention() {
    let (registry, source) = fixture();
    let published = refresh(&registry, &source, "stable");
    let error = registry
        .refresh(
            &source,
            || body("replacement"),
            || reserve(&source),
            |_, _| -> Result<()> { Err(ResourceObservationError::InventorySnapshotChanged.into()) },
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(ResourceObservationError::InventorySnapshotChanged)
    ));
    assert_eq!(
        registry.freshness().unwrap(),
        ResourceFreshness::RefreshFailed
    );
    assert_eq!(published.body().notes.as_deref(), Some("stable"));
}

#[test]
fn different_source_busy_preserves_published_source_posture_and_snapshot() {
    for dirty in [false, true] {
        let (registry, stale) = fixture();
        let mut current = stale.clone();
        current.execution_root_id = Some("root-b".into());
        let published = refresh(&registry, &current, "current");
        if dirty {
            registry
                .invalidate(ResourceInvalidation::KnownWrite)
                .unwrap();
        }
        let posture = registry.freshness().unwrap();
        let generation = registry.lock().unwrap().generation;
        let owner =
            MutationOwner::standalone(&stale.scope.agent_id, stale.scope.identity_incarnation);
        let _held = owner.try_reserve().unwrap();
        let error = registry
            .refresh(
                &stale,
                || body("stale"),
                || Ok(owner.try_reserve()?),
                |_, _| -> Result<()> { panic!("busy owner must not validate") },
            )
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(MutationOwnerError::Busy)
        ));
        assert_eq!(registry.freshness().unwrap(), posture);
        let state = registry.lock().unwrap();
        assert_eq!(state.generation, generation);
        assert_eq!(state.source.as_ref().unwrap().key, current);
        assert!(Arc::ptr_eq(state.value.as_ref().unwrap(), &published));
        drop(state);
        if dirty {
            assert!(matches!(
                registry.capture(&current).unwrap_err().downcast_ref(),
                Some(ResourceObservationError::NotReady(
                    ResourceFreshness::Dirty(ResourceInvalidation::KnownWrite)
                ))
            ));
        } else {
            assert!(Arc::ptr_eq(
                &published,
                &registry.capture(&current).unwrap()
            ));
        }
        assert_eq!(published.body().notes.as_deref(), Some("current"));
    }
}

#[test]
fn only_published_current_source_read_failure_changes_posture() {
    for matching_source in [false, true] {
        let (registry, current) = fixture();
        let published = refresh(&registry, &current, "current");
        let mut candidate = current.clone();
        if !matching_source {
            candidate.execution_root_id = Some("stale-root".into());
        }
        let error = registry
            .refresh(
                &candidate,
                || Err(io::Error::from(io::ErrorKind::PermissionDenied).into()),
                || -> Result<MutationReservation> {
                    panic!("failed reads must not acquire the owner")
                },
                |_, _| -> Result<()> { panic!("failed reads must not validate") },
            )
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            registry.freshness().unwrap(),
            if matching_source {
                ResourceFreshness::RefreshFailed
            } else {
                ResourceFreshness::Ready
            }
        );
        assert_eq!(
            registry.lock().unwrap().source.as_ref().unwrap().key,
            current
        );
        if matching_source {
            assert!(registry.capture(&current).is_err());
        } else {
            assert!(Arc::ptr_eq(
                &published,
                &registry.capture(&current).unwrap()
            ));
        }
        assert_eq!(published.body().notes.as_deref(), Some("current"));
    }
}

#[test]
fn changed_source_is_published_only_after_validation_and_retained_seal() {
    for underaccounted in [false, true] {
        let (registry, current) = fixture();
        let published = refresh(&registry, &current, "current");
        let mut candidate = current.clone();
        candidate.execution_root_id = Some("root-b".into());
        let generation = registry.lock().unwrap().generation;
        let assert_unpublished = || {
            assert_eq!(registry.freshness().unwrap(), ResourceFreshness::Ready);
            assert_eq!(registry.lock().unwrap().generation, generation);
            assert!(Arc::ptr_eq(
                &published,
                &registry.capture(&current).unwrap()
            ));
            assert!(matches!(
                registry.capture(&candidate).unwrap_err().downcast_ref(),
                Some(ResourceObservationError::SourceChanged)
            ));
        };
        let owner_attempted = Cell::new(false);
        let result = registry.refresh(
            &candidate,
            || {
                assert_unpublished();
                if underaccounted {
                    Ok(ContextResourceBody {
                        notes: Some(String::with_capacity(1024 * 1024)),
                        ..Default::default()
                    })
                } else {
                    body("replacement")
                }
            },
            || {
                owner_attempted.set(true);
                assert_unpublished();
                assert!(resource_io::read_to_end(&mut io::empty(), &mut Vec::new()).is_none());
                reserve(&candidate)
            },
            |_, source| {
                assert_eq!(source, &candidate);
                assert_unpublished();
                Ok(())
            },
        );
        assert_eq!(owner_attempted.get(), !underaccounted);
        if underaccounted {
            assert_eq!(
                crate::runtime_error::describe_runtime_error(&result.unwrap_err()).code,
                "context_resource_exhausted"
            );
            assert_unpublished();
        } else {
            let replacement = result.unwrap();
            assert!(replacement.generation() > generation);
            assert!(replacement.epoch() > published.epoch());
            assert!(Arc::ptr_eq(
                &replacement,
                &registry.capture(&candidate).unwrap()
            ));
            assert!(registry.capture(&current).is_err());
        }
        assert_eq!(published.source(), &current);
        assert_eq!(published.body().notes.as_deref(), Some("current"));
    }
}

#[test]
fn read_failure_after_invalidation_or_new_publication_cannot_change_posture() {
    for publish in [false, true] {
        let (registry, source) = fixture();
        let old = refresh(&registry, &source, "old");
        let newer = RefCell::new(None);
        let error = registry
            .refresh(
                &source,
                || {
                    if publish {
                        *newer.borrow_mut() = Some(refresh(&registry, &source, "new"));
                    } else {
                        registry.invalidate(ResourceInvalidation::ExternalEdit)?;
                    }
                    Err(io::Error::from(io::ErrorKind::PermissionDenied).into())
                },
                || -> Result<MutationReservation> {
                    panic!("failed reads must not acquire the owner")
                },
                |_, _| Ok(()),
            )
            .unwrap_err();
        assert!(error.downcast_ref::<io::Error>().is_some());
        if publish {
            assert_eq!(registry.freshness().unwrap(), ResourceFreshness::Ready);
            assert!(Arc::ptr_eq(
                newer.borrow().as_ref().unwrap(),
                &registry.capture(&source).unwrap()
            ));
        } else {
            assert_eq!(
                registry.freshness().unwrap(),
                ResourceFreshness::Dirty(ResourceInvalidation::ExternalEdit)
            );
        }
        assert_eq!(old.body().notes.as_deref(), Some("old"));
    }
}

#[test]
fn changed_source_candidate_cannot_install_or_fail_over_newer_publication() {
    for fail_read in [false, true] {
        let (registry, initial) = fixture();
        let old = refresh(&registry, &initial, "initial");
        let mut candidate = initial.clone();
        candidate.execution_root_id = Some("candidate-root".into());
        let mut current = initial.clone();
        current.execution_root_id = Some("newest-root".into());
        let newer = RefCell::new(None);
        let error = registry
            .refresh(
                &candidate,
                || {
                    *newer.borrow_mut() = Some(refresh(&registry, &current, "newest"));
                    if fail_read {
                        Err(io::Error::from(io::ErrorKind::PermissionDenied).into())
                    } else {
                        body("late candidate")
                    }
                },
                || reserve(&candidate),
                |_, _| Ok(()),
            )
            .unwrap_err();
        if fail_read {
            assert!(error.downcast_ref::<io::Error>().is_some());
        } else {
            assert!(matches!(
                error.downcast_ref(),
                Some(ResourceObservationError::ObsoleteRefresh)
            ));
        }
        assert_eq!(registry.freshness().unwrap(), ResourceFreshness::Ready);
        assert!(Arc::ptr_eq(
            newer.borrow().as_ref().unwrap(),
            &registry.capture(&current).unwrap()
        ));
        assert!(registry.capture(&candidate).is_err());
        assert_eq!(old.body().notes.as_deref(), Some("initial"));
    }
}

#[test]
fn changed_source_generation_overflow_fences_without_publishing_candidate() {
    let (registry, source) = fixture();
    let old = refresh(&registry, &source, "stable");
    registry.lock().unwrap().generation = u64::MAX;
    let stale = ticket(&registry, &source);
    let mut candidate = source.clone();
    candidate.execution_root_id = Some("new-root".into());
    let error = registry
        .refresh(
            &candidate,
            || body("replacement"),
            || reserve(&candidate),
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(ResourceObservationError::GenerationExhausted)
    ));
    let state = registry.lock().unwrap();
    assert!(state.exhausted);
    assert_eq!(state.source.as_ref().unwrap().key, source);
    assert!(Arc::ptr_eq(state.value.as_ref().unwrap(), &old));
    drop(state);
    let (body, phase) = raw_candidate(&registry, &source, "late");
    assert!(matches!(
        registry
            .install(stale, SealedResourceBody::new(body, &phase).unwrap())
            .unwrap_err()
            .downcast_ref(),
        Some(ResourceObservationError::ObsoleteRefresh)
    ));
    assert!(registry.capture(&source).is_err());
    assert!(registry.capture(&candidate).is_err());
    assert_eq!(old.body().notes.as_deref(), Some("stable"));
}

#[test]
fn source_revalidation_failure_and_underaccounted_spare_capacity_fail_closed() {
    let (registry, source) = fixture();
    let old = refresh(&registry, &source, "stable");
    let error = registry
        .refresh(
            &source,
            || body("wrong root"),
            || reserve(&source),
            |_, _| Err(ResourceObservationError::SourceChanged.into()),
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(ResourceObservationError::SourceChanged)
    ));
    assert_eq!(
        registry.freshness().unwrap(),
        ResourceFreshness::RefreshFailed
    );
    let error = registry
        .refresh(
            &source,
            || {
                Ok(ContextResourceBody {
                    notes: Some(String::with_capacity(1024 * 1024)),
                    ..Default::default()
                })
            },
            || panic!("retained admission failure must precede owner acquisition"),
            |_, _| -> Result<()> { panic!("unsealed candidate must not be validated") },
        )
        .unwrap_err();
    assert_eq!(
        crate::runtime_error::describe_runtime_error(&error).code,
        "context_resource_exhausted"
    );
    assert_eq!(
        registry.freshness().unwrap(),
        ResourceFreshness::RefreshFailed
    );
    assert_eq!(old.body().notes.as_deref(), Some("stable"));
}

#[test]
fn sealed_candidate_stays_private_and_releases_its_guard_after_invalidation() {
    let (registry, source) = fixture();
    let old = refresh(&registry, &source, "published");
    let baseline = registry.ledger.report().unwrap().observed_bytes;
    let ticket = ticket(&registry, &source);
    let (body, material) = raw_candidate(&registry, &source, "unpublished");
    let sealed = SealedResourceBody::new(body, &material).unwrap();
    let guard = Arc::downgrade(&sealed.material);
    drop(material);
    assert!(guard.upgrade().is_some());
    assert!(registry.ledger.report().unwrap().observed_bytes > baseline);
    assert!(Arc::ptr_eq(&old, &registry.capture(&source).unwrap()));

    registry
        .invalidate(ResourceInvalidation::ExternalEdit)
        .unwrap();
    assert!(matches!(
        registry.install(ticket, sealed).unwrap_err().downcast_ref(),
        Some(ResourceObservationError::ObsoleteRefresh)
    ));
    assert!(guard.upgrade().is_none());
    assert_eq!(registry.ledger.report().unwrap().observed_bytes, baseline);
    assert_eq!(old.body().notes.as_deref(), Some("published"));
    assert_eq!(
        registry.freshness().unwrap(),
        ResourceFreshness::Dirty(ResourceInvalidation::ExternalEdit)
    );
    assert!(registry.capture(&source).is_err());
}

#[test]
fn refresh_rejects_an_owner_from_another_agent_or_incarnation() {
    for identity in [("other-agent", 1), ("resource-core", 2)] {
        let (registry, source) = fixture();
        let old = refresh(&registry, &source, "stable");
        let owner = MutationOwner::standalone(identity.0, identity.1);
        let error = registry
            .refresh(
                &source,
                || body("wrong owner"),
                || Ok(owner.try_reserve()?),
                |_, _| -> Result<()> {
                    panic!("owner scope must be checked before root/source validation")
                },
            )
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(ResourceObservationError::SourceChanged)
        ));
        assert_eq!(
            registry.freshness().unwrap(),
            ResourceFreshness::RefreshFailed
        );
        assert_eq!(old.body().notes.as_deref(), Some("stable"));
    }
}

#[test]
fn candidate_old_snapshot_and_installed_version_keep_the_actual_original_guards() {
    let (registry, source) = fixture();
    let ledger = registry.ledger.clone();
    let old = refresh(&registry, &source, "old snapshot");
    let old_guard = Arc::downgrade(&old._material);
    let source_guard = Arc::downgrade(&old.source._material);
    let baseline = ledger.report().unwrap().observed_bytes;

    let (abandoned, material) = raw_candidate(&registry, &source, "abandoned");
    let abandoned_guard = Arc::downgrade(&material.guard().unwrap());
    assert!(ledger.report().unwrap().observed_bytes > baseline);
    drop(abandoned);
    assert!(abandoned_guard.upgrade().is_some());
    drop(material);
    assert!(abandoned_guard.upgrade().is_none());
    assert_eq!(ledger.report().unwrap().observed_bytes, baseline);

    let ticket = ticket(&registry, &source);
    let (candidate, material) = raw_candidate(&registry, &source, "installed version");
    let candidate_guard = material.guard().unwrap();
    let installed_guard = Arc::downgrade(&candidate_guard);
    let sealed = SealedResourceBody::new(candidate, &material).unwrap();
    let digest = sealed.digest;
    assert!(Arc::ptr_eq(&candidate_guard, &sealed.material));
    let installed = registry.install(ticket, sealed).unwrap();
    assert_eq!(installed.digest(), &digest);
    assert!(Arc::ptr_eq(&candidate_guard, &installed._material));
    drop(candidate_guard);
    drop(material);
    let snapshot = registry.capture(&source).unwrap();
    assert!(Arc::ptr_eq(&snapshot, &installed));
    assert!(installed_guard.upgrade().is_some());
    assert!(old_guard.upgrade().is_some());
    assert_eq!(old.body().notes.as_deref(), Some("old snapshot"));

    registry.close().unwrap();
    drop(registry);
    drop(installed);
    assert!(installed_guard.upgrade().is_some());
    drop(snapshot);
    assert!(installed_guard.upgrade().is_none());
    assert!(old_guard.upgrade().is_some());
    assert!(source_guard.upgrade().is_some());
    drop(old);
    assert!(old_guard.upgrade().is_none());
    assert!(source_guard.upgrade().is_none());
    assert_eq!(ledger.report().unwrap().observed_bytes, 0);
}

#[test]
fn every_source_identity_component_fences_capture_and_unchanged_refresh_reuses_arc() {
    let (registry, source) = fixture();
    let old = refresh(&registry, &source, "same");
    let same = refresh(&registry, &source, "same");
    assert!(Arc::ptr_eq(&old, &same));
    for index in 0..15 {
        let mut changed = source.clone();
        match index {
            0 => changed.scope.agent_id.push('b'),
            1 => changed.scope.identity_incarnation += 1,
            2 => changed.agent_home.push("other"),
            3 => changed.user_home = None,
            4 => changed.workspace_id = None,
            5 => changed.workspace_anchor = None,
            6 => changed.execution_root_id = None,
            7 => changed.execution_root = None,
            8 => changed.include_user_skills = false,
            9 => changed.config_generation += 1,
            10 => changed.host_generation += 1,
            11 => changed.active_skills_selector[0] = 1,
            12 => changed.work_revision += 1,
            13 => changed.plans.push(ContextPlanSource {
                work_item_id: "work-a".into(),
                recorded: false,
            }),
            _ => changed.plans.push(ContextPlanSource {
                work_item_id: "work-a".into(),
                recorded: true,
            }),
        }
        assert!(matches!(
            registry.capture(&changed).unwrap_err().downcast_ref(),
            Some(ResourceObservationError::SourceChanged)
        ));
        if index < 12 {
            assert!(matches!(
                registry
                    .capture_instructions(&changed)
                    .unwrap_err()
                    .downcast_ref(),
                Some(ResourceObservationError::SourceChanged)
            ));
        } else {
            let instructions = registry.capture_instructions(&changed).unwrap();
            assert!(std::ptr::eq(instructions.guidance(), &old.body().guidance));
            assert!(std::ptr::eq(instructions.skills(), &old.body().skills));
        }
    }
    assert_eq!(old.epoch(), same.epoch());
}

#[test]
fn instruction_read_keeps_original_guard_and_rejects_invalidated_resources() -> Result<()> {
    let (registry, mut source) = fixture();
    let ledger = registry.ledger.clone();
    let original = refresh(&registry, &source, "instructions");
    let guard = Arc::downgrade(&original._material);
    let source_guard = Arc::downgrade(&original.source._material);
    source.work_revision += 1;
    let before = ledger.report()?.observed_bytes;
    let probe = IoProbe::start();
    let instructions = registry.capture_instructions(&source)?;
    assert!(std::ptr::eq(
        instructions.guidance(),
        &original.body().guidance
    ));
    assert!(std::ptr::eq(instructions.skills(), &original.body().skills));
    assert_eq!(ledger.report()?.observed_bytes, before);
    assert_eq!(probe.snapshot().counters.file_read_bytes, 0);
    assert_eq!(probe.snapshot().counters.directory_read_attempts, 0);
    registry.invalidate(ResourceInvalidation::KnownWrite)?;
    assert!(matches!(
        registry
            .capture_instructions(&source)
            .unwrap_err()
            .downcast_ref(),
        Some(ResourceObservationError::NotReady(
            ResourceFreshness::Dirty(_)
        ))
    ));
    registry.close()?;
    drop(original);
    assert!(guard.upgrade().is_some());
    assert!(source_guard.upgrade().is_some());
    drop(instructions);
    assert!(guard.upgrade().is_none());
    assert!(source_guard.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
