use super::super::{
    selector_sql::{borrowed_fragment, format_selector, Inventories, Ranks},
    tests::{ledger, request, scope},
};
use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use std::{cell::Cell, fmt};

fn manifest(id: &str) -> SchedulingContinuityCheckpoint {
    SchedulingContinuityCheckpoint {
        policy_version: 1,
        inventory_id: id.into(),
        agent_id: scope().agent_id,
        observed_tables: BTreeSet::new(),
        member_count: 0,
        metadata_bytes: 0,
        content_digest: "0".repeat(64),
    }
}

#[test]
fn empty_accepted_history_keeps_complete_coverage_and_last_material_holder() -> Result<()> {
    let (_home, db) = super::super::tests::legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let snapshot =
        Arc::new(db.transaction(|tx| {
            prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger)
        })?);
    let SchedulingBootstrapSnapshot::Current(current) = snapshot.as_ref() else {
        anyhow::bail!("live scope must retain complete empty Scheduling coverage");
    };
    assert_eq!(current.resident.coverage().history.len(), 3);
    for history in current.resident.coverage().history.values() {
        assert!(history.is_exhaustive);
        assert!(history.keys.is_empty());
        assert!(history.loaded_keys.is_empty());
    }
    let held = Arc::clone(&snapshot);
    drop(snapshot);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(held);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn borrowed_selectors_preserve_escaped_sql_and_manifest_precedence() -> Result<()> {
    let scope = scope();
    let request = request();
    let ledger = ledger()?;
    let mut material = Material::new(&scope, &request, &ledger);
    for manifests in [
        Vec::new(),
        vec![manifest("é'x"), manifest("'two''")],
        (0..12).map(|n| manifest(&format!("m'{n}"))).collect(),
    ] {
        // Independent original replace/join SQL is a compatibility oracle,
        // not a loader run used to discover sufficient budget.
        let inventory = manifests
            .iter()
            .map(|m| format!("'{}'", m.inventory_id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let inventory = if inventory.is_empty() {
            "NULL"
        } else {
            &inventory
        };
        let ranks = manifests
            .iter()
            .enumerate()
            .map(|(i, m)| format!("WHEN '{}' THEN {i}", m.inventory_id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" ");
        let ranks = if ranks.is_empty() {
            "WHEN '' THEN -1"
        } else {
            &ranks
        };
        assert_eq!(Inventories(&manifests).to_string(), inventory);
        assert_eq!(Ranks(&manifests).to_string(), ranks);
        for family in [
            SchedulingFamily::Timer,
            SchedulingFamily::TimerWake,
            SchedulingFamily::ExternalTrigger,
        ] {
            let (from, id, owner, predicate, sort) = stored::source(family);
            let family_name = family_name(family);
            let current_known = format!("({id} IN (SELECT record_id FROM scheduling_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='{family_name}' AND availability!='absent') OR {id} IN (SELECT record_id FROM scheduling_fixed_members WHERE family='{family_name}' AND inventory_id IN ({inventory})))");
            let known_anywhere = format!("({id} IN (SELECT record_id FROM scheduling_domain_records WHERE family='{family_name}' AND availability!='absent') OR {id} IN (SELECT record_id FROM scheduling_fixed_members WHERE family='{family_name}'))");
            for live in [false, true] {
                let expected = if live {
                    format!("({predicate}) AND ({current_known} OR NOT {known_anywhere})")
                } else {
                    format!("NOT ({predicate}) AND {current_known}")
                };
                let before = (material.bytes, material.records);
                let actual = format_selector(
                    &mut material,
                    format_args!("{}", selection_filter(family, live, &manifests)),
                )?;
                assert_eq!(actual, expected);
                assert_eq!(
                    material.bytes - before.0,
                    (expected.len() + std::mem::size_of::<String>() + scope.agent_id.len() + 128)
                        * 8
                );
                assert_eq!(material.records - before.1, 8);
            }
            let current = format!("r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family='{family_name}' AND r.availability!='absent'");
            let rank = |alias: &str| format!("CASE {alias}.inventory_id {ranks} ELSE -1 END");
            let expected = format!("SELECT r.record_id,r.status,r.sort_at,r.created_at,r.availability
        FROM scheduling_domain_records r WHERE {current}
        UNION ALL SELECT f.record_id,f.status,f.sort_at,f.created_at,'present'
        FROM scheduling_fixed_members f WHERE f.family='{family_name}' AND f.inventory_id IN({inventory})
        AND NOT EXISTS(SELECT 1 FROM scheduling_domain_records r WHERE {current} AND r.record_id=f.record_id)
        AND NOT EXISTS(SELECT 1 FROM scheduling_fixed_members newer WHERE newer.family=f.family
         AND newer.record_id=f.record_id AND newer.inventory_id IN({inventory}) AND {}>{})", rank("newer"), rank("f"));
            assert_eq!(
                format_selector(
                    &mut material,
                    format_args!(
                        "{}",
                        super::super::history_selection::accepted_heads(family, &manifests)
                    )
                )?,
                expected
            );
            let live_header = match family {
                SchedulingFamily::Timer => "h.status IN ('active','scheduled')",
                SchedulingFamily::TimerWake => "h.status='pending'",
                SchedulingFamily::ExternalTrigger => "h.status='active'",
            };
            for live in [false, true] {
                for limit in [None, Some(0usize), Some(12)] {
                    let suffix = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
                    let selected = if live {
                        format!("SELECT {id} AS selected_id FROM {from} WHERE {owner}=?1 AND ({predicate}) AND ({current_known} OR NOT {known_anywhere}) ORDER BY {sort} DESC,c.created_at DESC,{id} ASC{suffix}")
                    } else {
                        format!("SELECT record_id AS selected_id FROM ({expected}) h WHERE NOT ({live_header}) ORDER BY sort_at DESC,created_at DESC,record_id ASC{suffix}")
                    };
                    let aggregate = format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(selected_id AS BLOB))+64),0) FROM ({selected})");
                    let before = (material.bytes, material.records);
                    let actual = id_selector(family, live, limit, &manifests, &mut material)?;
                    assert_eq!(actual, aggregate);
                    let borrowed = &actual[IDS_AGGREGATE_PREFIX.len()..actual.len() - 1];
                    assert_eq!(borrowed, selected);
                    assert_eq!(
                        borrowed.as_ptr(),
                        actual.as_bytes()[IDS_AGGREGATE_PREFIX.len()..].as_ptr()
                    );
                    assert_eq!(
                        material.bytes - before.0,
                        (aggregate.len()
                            + std::mem::size_of::<String>()
                            + scope.agent_id.len()
                            + 128)
                            * 8
                    );
                    assert_eq!(material.records - before.1, 8);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn selector_counting_rejects_before_owning_format_and_keeps_last_guard() -> Result<()> {
    struct Probe<'a> {
        text: &'a str,
        calls: &'a Cell<usize>,
    }
    impl fmt::Display for Probe<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.calls.set(self.calls.get() + 1);
            f.write_str(self.text)
        }
    }
    let scope = scope();
    let request = request();
    let ledger = ledger()?;
    let header = ResourceReadMaterial::new(&ledger, &scope)?.finish()?;
    let text = "é'".repeat(1024);
    let cost = (text.len() + std::mem::size_of::<String>() + scope.agent_id.len() + 128) as u64 * 8;
    for bytes_bound in [false, true] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            if bytes_bound {
                header.0 + cost * 2 - 1
            } else {
                1024 * 1024
            },
            if bytes_bound { 4096 } else { header.1 + 15 },
        )?;
        let weak = Arc::downgrade(&phase.guard()?);
        let mut material = Material::new(&scope, &request, &ledger);
        let calls = Cell::new(0);
        let probe = Probe {
            text: &text,
            calls: &calls,
        };
        let fragment = borrowed_fragment(|f| write!(f, "{probe}"));
        let composed = borrowed_fragment(|f| write!(f, "{fragment}"));
        assert_eq!(
            calls.get(),
            0,
            "borrowed composition does not format or allocate"
        );
        assert_eq!(phase.finish()?, header);
        {
            let _entry = phase.enter();
            assert_eq!(
                format_selector(&mut material, format_args!("{composed}"))?,
                text
            );
            assert_eq!(calls.get(), 2);
            assert_eq!(phase.finish()?, (header.0 + cost, header.1 + 8));
            let error = format_selector(&mut material, format_args!("{composed}")).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("ResourceExhausted: context resource read material"),
                "{error:#}"
            );
            assert_eq!(
                calls.get(),
                3,
                "rejection must never enter owning formatting"
            );
        }
        assert!(phase.finish().is_err());
        let holders = material.guards.clone();
        drop(material);
        drop(phase);
        assert!(weak.upgrade().is_some());
        drop(holders);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let calls = Cell::new(0);
    let probe = Probe {
        text: &text,
        calls: &calls,
    };
    let mut small = request.clone();
    small.byte_limit = 1;
    let mut material = Material::new(&scope, &small, &ledger);
    assert!(format_selector(&mut material, format_args!("{probe}")).is_err());
    assert_eq!(
        calls.get(),
        1,
        "local refusal also precedes owning formatting"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn loader_live_and_history_selectors_reject_before_sql_prepare() -> Result<()> {
    let scope = scope();
    let request = request();
    let ledger = ledger()?;
    let manifests = [manifest(&"é'".repeat(1024))];
    let mut connection = Connection::open_in_memory()?;
    let tx = connection.transaction()?;
    for loaded_history in [false, true] {
        for bytes_bound in [false, true] {
            let header = ResourceReadMaterial::new(&ledger, &scope)?.finish()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                if bytes_bound {
                    header.0
                } else {
                    16 * 1024 * 1024
                },
                if bytes_bound { 4096 } else { header.1 },
            )?;
            let mut material = Material::new(&scope, &request, &ledger);
            let error = {
                let _entry = phase.enter();
                if loaded_history {
                    super::super::history_selection::loaded_history_ids(
                        &tx,
                        &scope,
                        SchedulingFamily::Timer,
                        1,
                        &manifests,
                        &mut material,
                    )
                    .unwrap_err()
                } else {
                    ids(
                        &tx,
                        &scope,
                        SchedulingFamily::Timer,
                        true,
                        None,
                        &manifests,
                        &mut material,
                    )
                    .unwrap_err()
                }
            };
            assert!(
                error
                    .to_string()
                    .contains("ResourceExhausted: context resource read material"),
                "{error:#}"
            );
            assert!(phase.finish().is_err());
            drop(material);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        // Identical selectors with sufficient original budget reach SQLite;
        // missing tables prove no SQL work can mask the low-budget refusal.
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
        let mut material = Material::new(&scope, &request, &ledger);
        let error = {
            let _entry = phase.enter();
            if loaded_history {
                super::super::history_selection::loaded_history_ids(
                    &tx,
                    &scope,
                    SchedulingFamily::Timer,
                    1,
                    &manifests,
                    &mut material,
                )
                .unwrap_err()
            } else {
                ids(
                    &tx,
                    &scope,
                    SchedulingFamily::Timer,
                    true,
                    None,
                    &manifests,
                    &mut material,
                )
                .unwrap_err()
            }
        };
        assert!(error.to_string().contains("no such table"), "{error:#}");
        phase.finish()?;
        drop(material);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
