use super::*;

fn measure<T>(read: impl FnOnce() -> Result<T>) -> Result<(T, (u64, usize))> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let value = {
        let _entry = phase.enter();
        read()?
    };
    Ok((value, phase.finish()?))
}

#[tokio::test]
async fn latest_key_rejects_both_axes_on_both_host_paths_without_partial_snapshot() -> Result<()> {
    let (_home, host) = fixture().await?;
    let (_, queue_cost) = measure(|| {
        crate::storage::RuntimeReadModels::borrowed_for_agent(host.runtime_db(), "web")?
            .work_queue()
    })?;
    // Calibrate only the already-admitted SQL prefix before the first key,
    // without invoking the new key helper.
    let (open, sql_prefix) = measure(|| {
        host.runtime_db().agent_states().latest("web")?;
        host.runtime_db().work_items().open_for_agent("web")
    })?;
    assert_eq!(open.len(), 1);
    let key = (
        (open[0].id.len() + std::mem::size_of::<String>()) as u64 * 8,
        8,
    );
    for roster in [false, true] {
        let ((agent, retained), total) = measure(|| entry(&host, roster))?;
        let posture = &agent.scheduling_posture;
        let tail = std::iter::once(posture.reason.as_str())
            .chain(
                [
                    posture.work_item_id.as_deref(),
                    posture.task_id.as_deref(),
                    posture.run_id.as_deref(),
                ]
                .into_iter()
                .flatten(),
            )
            .fold((0, 0), |sum, text| {
                (
                    sum.0 + (text.len() + std::mem::size_of::<String>()) as u64 * 8,
                    sum.1 + 8,
                )
            });
        let prefix = (
            total.0 - queue_cost.0 - tail.0 + sql_prefix.0,
            total.1 - queue_cost.1 - tail.1 + sql_prefix.1,
        );
        drop((agent, retained));
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                prefix.0 + key.0 - u64::from(bytes),
                prefix.1 + key.1 - usize::from(!bytes),
            )?;
            let original = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &original,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                let error = entry(&host, roster).unwrap_err();
                assert_eq!(error.to_string(), "queue latest key admission");
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                let retry = entry(&host, roster).unwrap_err();
                assert_eq!(
                    retry
                        .downcast_ref::<crate::runtime_error::RuntimeError>()
                        .unwrap()
                        .descriptor()
                        .code,
                    "context_resource_exhausted",
                );
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(original);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[tokio::test]
async fn latest_key_keeps_original_host_holder_through_wire_failure_and_last_release() -> Result<()>
{
    let (_home, host) = fixture().await?;
    for roster in [false, true] {
        let expected = serde_json::to_value(entry(&host, roster)?.0)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let total = phase.finish()?;
        let last = retained.as_ref().unwrap().clone();
        drop((phase, original));
        struct Checked<'a> {
            agent: &'a AgentListEntry,
            ledger: &'a InputEvidenceBudgetLedger,
            bytes: u64,
            fail: bool,
        }
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.bytes);
                if self.fail {
                    return Err(serde::ser::Error::custom("latest-key wire failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let encoded = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: total.0,
                fail,
            });
            assert_eq!(encoded.is_err(), fail);
            if let Ok(encoded) = encoded {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&encoded)?,
                    expected
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, total.0);
        }
        drop((agent, retained));
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
