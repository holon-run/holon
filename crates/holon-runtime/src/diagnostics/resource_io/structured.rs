pub(crate) use holon_system::resource_material::{
    admit_cloned_value, admit_json_buffer, admit_json_payload, admit_serialized_value,
    admit_typed_cloned_value, admit_typed_json_payload, cloned_value_material,
    json_payload_material,
};

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };
    use anyhow::Result;

    #[test]
    fn serialized_and_borrowed_json_share_identical_phase_charges() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        let value = serde_json::json!({"nested": [null, {"quoted": "\\\"{}[]:,"}], "empty": []});
        let payload = serde_json::to_string(&value)?;
        let phase = ResourceReadMaterial::new(&ledger, &scope)?;
        let header = phase.finish()?;
        {
            let _active = phase.enter();
            admit_serialized_value(&value)?;
        }
        let serialized = phase.finish()?;
        let explicit = json_payload_material(&payload)?;
        // The counter returns the raw envelope; ResourceReadMaterial applies
        // its original eightfold capacity allowance in charge().
        assert_eq!(
            (serialized.0 - header.0, serialized.1 - header.1),
            (
                u64::try_from(explicit.0)?.checked_mul(8).unwrap(),
                explicit.1.checked_mul(8).unwrap()
            )
        );
        drop(phase);
        let phase = ResourceReadMaterial::new(&ledger, &scope)?;
        {
            let _active = phase.enter();
            admit_json_payload(&payload)?;
        }
        assert_eq!(phase.finish()?, serialized);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn direct_typed_copies_and_json_buffers_keep_bytes_and_original_remainder() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "direct-copy".into(),
            identity_incarnation: 1,
        };
        // Seven JSON values, including the root and each empty container.
        let value = serde_json::json!({"nested": [null, {}, [], {"body": "\\\"{}[]:,路"}]});
        let bytes = u64::try_from(cloned_value_material(&value)?.0)? * 8;
        let header = ResourceReadMaterial::new(&ledger, &scope)?.finish()?;
        for buffer in [false, true] {
            let records = if buffer { 8 } else { 7 * 8 };
            let admit = || {
                if buffer {
                    admit_json_buffer(&value)
                } else {
                    admit_typed_cloned_value(&value)
                }
            };
            for bytes_bound in [false, true] {
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope,
                    if bytes_bound {
                        header.0 + bytes - 1
                    } else {
                        header.0 + bytes
                    },
                    if bytes_bound {
                        header.1 + records
                    } else {
                        header.1 + records - 1
                    },
                )?;
                {
                    let _entry = phase.enter();
                    assert!(admit().is_err());
                    assert!(admit().is_err());
                }
                assert!(phase.finish().is_err());
            }
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                header.0 + bytes * 2,
                header.1 + records * 2,
            )?;
            let held = phase.guard()?;
            {
                let _entry = phase.enter();
                admit()?;
                admit()?;
            }
            assert_eq!(
                phase.finish()?,
                (header.0 + bytes * 2, header.1 + records * 2)
            );
            {
                let _entry = phase.enter();
                assert!(admit().is_err());
            }
            assert!(phase.finish().is_err());
            drop(phase);
            assert!(ledger.report()?.observed_bytes > 0);
            drop(held);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn no_phase_keeps_non_utf8_current_paths_outside_the_json_gate() -> Result<()> {
        use std::os::unix::ffi::OsStringExt;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));
        assert!(serde_json::to_string(&path).is_err());
        admit_serialized_value(&path)?;
        admit_json_buffer(&path)?;
        admit_typed_cloned_value(&path)?;
        admit_cloned_value(&path)
    }

    #[test]
    fn typed_clone_has_its_own_records_envelope_and_original_bytes() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        let value = serde_json::json!({"nested": [null, {}, [], {"body": "\\\"{}[]:,"}]});
        let slots = cloned_value_material(&value)?.1 - 16;
        let phase = ResourceReadMaterial::new(&ledger, &scope)?;
        let header = phase.finish()?;
        {
            let _active = phase.enter();
            admit_serialized_value(&value)?;
        }
        let decoded = phase.finish()?;
        drop(phase);
        let phase = ResourceReadMaterial::new(&ledger, &scope)?;
        {
            let _active = phase.enter();
            admit_cloned_value(&value)?;
        }
        let cloned = phase.finish()?;
        let explicit = cloned_value_material(&value)?;
        assert_eq!(
            (cloned.0 - header.0, cloned.1 - header.1),
            (u64::try_from(explicit.0)? * 8, explicit.1 * 8)
        );
        assert_eq!(cloned.0, decoded.0);
        assert_eq!(cloned.1 - header.1, (slots + 16) * 8);
        assert_eq!(decoded.1 - header.1, (4 * slots + 16) * 8);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        // Exactly one owning clone fits. A second clone must use the original
        // phase remainder, not reuse the first clone's allowance.
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, cloned.0, cloned.1)?;
        {
            let _active = phase.enter();
            admit_cloned_value(&value)?;
            assert!(admit_cloned_value(&value).is_err());
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        // This profile must never silently replace the decoder's larger gate.
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, decoded.0, cloned.1)?;
        {
            let _active = phase.enter();
            assert!(admit_serialized_value(&value).is_err());
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn direct_decode_counts_cells_without_completion_copies_and_latches_limits() -> Result<()> {
        use serde_json::{json, Value};
        fn cells(value: &Value) -> usize {
            1 + match value {
                Value::Object(fields) => fields.values().map(cells).sum::<usize>(),
                Value::Array(values) => values.iter().map(cells).sum::<usize>(),
                _ => 0,
            }
        }
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "direct-json".into(),
            identity_incarnation: 1,
        };
        for value in [
            json!(null),
            json!(false),
            json!(-1.25e-100),
            json!("\\\"{,:} 汉🦀"),
            json!({"": [], "nested": [null, true, 42, {"key\\\"": "\\\"{,:}"}]}),
            json!({"metadata": {"nested": [[], {}, "", false, -2.0e20]}}),
        ] {
            let payload = serde_json::to_string_pretty(&value)?;
            let (bytes, records) = json_payload_material(&payload)?;
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let header = phase.finish()?;
            {
                let _entry = phase.enter();
                admit_typed_json_payload(&payload)?;
            }
            let exact = phase.finish()?;
            assert_eq!(exact.0 - header.0, u64::try_from(bytes)? * 8);
            assert_eq!(exact.1 - header.1, cells(&value) * 8);
            drop(phase);
            let completion = ResourceReadMaterial::new(&ledger, &scope)?;
            {
                let _entry = completion.enter();
                admit_json_payload(&payload)?;
            }
            assert_eq!(completion.finish()?.0, exact.0);
            assert_eq!(completion.finish()?.1 - header.1, records * 8,);
            drop(completion);
            for (bytes, records, succeeds) in [
                (exact.0, exact.1, true),
                (exact.0 - 1, exact.1, false),
                (exact.0, exact.1 - 1, false),
            ] {
                let phase =
                    ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
                {
                    let _entry = phase.enter();
                    if succeeds {
                        admit_typed_json_payload(&payload)?;
                        assert_eq!(phase.finish()?, exact);
                    } else {
                        assert!(admit_typed_json_payload(&payload).is_err());
                        assert!(admit_typed_json_payload(&payload).is_err());
                        assert!(ResourceReadMaterial::active_guard().is_err());
                        assert!(phase.finish().is_err());
                    }
                }
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
        Ok(())
    }
}
