use super::*;

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn wait_posture_honors_original_allowance() -> Result<()> {
    let (_home, host) = active_wait_fixture().await?;
    super::assert_host_projection_limits(&host)
}

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn wait_summary_keeps_original_holder_through_wire_failure_and_last_release() -> Result<()> {
    let (_home, host) = active_wait_fixture().await?;
    for roster in [false, true] {
        let expected = entry(&host, roster)?.0;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(agent.scheduling_posture, expected.scheduling_posture);
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
                    return Err(serde::ser::Error::custom("wait-summary wire failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: total.0,
                fail,
            });
            assert_eq!(result.is_err(), fail);
            if let Ok(body) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&body)?;
                assert_eq!(decoded.scheduling_posture, agent.scheduling_posture);
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
