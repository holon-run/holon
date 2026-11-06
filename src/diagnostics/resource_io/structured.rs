//! Separate admission envelopes for JSON decoding and existing typed clones.
use anyhow::Result;

/// Allocation-free JSON-shape accounting, independent of punctuation inside
/// report strings. No source body is cloned by this counter.
#[derive(Default)]
struct StructuredMaterialCounter {
    bytes: u64,
    slots: usize,
    cells: usize,
    cells_peak: usize,
    quoted: bool,
    escaped: bool,
    literal: bool,
}

impl std::io::Write for StructuredMaterialCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let exhausted = || std::io::Error::other("ResourceExhausted: completion material");
        self.bytes = self
            .bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| exhausted())?)
            .ok_or_else(exhausted)?;
        for byte in bytes {
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if *byte == b'\\' {
                    self.escaped = true;
                } else if *byte == b'"' {
                    self.quoted = false;
                }
            } else {
                // Count JSON values, not struct field names or punctuation.
                // A colon removes the preceding key string; nested values and
                // array entries each retain their own cell. No parser allocates.
                if matches!(*byte, b'"' | b'{' | b'[')
                    || (!self.literal && matches!(*byte, b'-' | b'0'..=b'9' | b't' | b'f' | b'n'))
                {
                    self.cells = self.cells.checked_add(1).ok_or_else(exhausted)?;
                    self.cells_peak = self.cells_peak.max(self.cells);
                }
                if *byte == b':' {
                    self.cells = self.cells.saturating_sub(1);
                }
                if matches!(*byte, b'"' | b'{' | b'}' | b'[' | b']' | b':' | b',')
                    || byte.is_ascii_whitespace()
                {
                    self.literal = false;
                } else {
                    self.literal = true;
                }
                if matches!(*byte, b'"' | b'{' | b'}' | b'[' | b']' | b':' | b',') {
                    self.slots = self.slots.checked_add(1).ok_or_else(exhausted)?;
                }
                self.quoted = *byte == b'"';
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl StructuredMaterialCounter {
    fn admit(self) -> Result<()> {
        // Completion decoding also covers aggregate/resolver/projection copies.
        let records = self
            .slots
            .checked_mul(4)
            .and_then(|records| records.checked_add(16))
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: completion material"))?;
        self.admit_records(records)
    }

    fn admit_clone(self) -> Result<()> {
        // No parser or resolver is run for this one typed clone. Structural
        // punctuation bounds its owning cells; the phase's original eightfold
        // allowance covers container capacity. Every later clone is gated anew.
        let records = self
            .slots
            .checked_add(16)
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: cloned material"))?;
        self.admit_records(records)
    }

    fn admit_records(self, records: usize) -> Result<()> {
        let bytes = self.material_bytes()?;
        crate::diagnostics::resource_io::charge_owned_metadata(bytes, records)?;
        Ok(())
    }

    fn material_bytes(&self) -> Result<u64> {
        // Keep the original conservative bytes envelope for both paths.
        self.bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(u64::try_from(self.slots).ok()?.checked_mul(128)?))
            .and_then(|bytes| bytes.checked_add(512))
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: completion material"))
    }
}

/// Pure material estimate for callers admitting to an explicit Native phase.
pub(crate) fn json_payload_material(payload: &str) -> Result<(usize, usize)> {
    use std::io::Write;
    let mut counter = StructuredMaterialCounter::default();
    counter.write_all(payload.as_bytes())?;
    let records = counter
        .slots
        .checked_mul(4)
        .and_then(|records| records.checked_add(16))
        .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: completion material"))?;
    Ok((usize::try_from(counter.material_bytes()?)?, records))
}

/// One existing typed clone, without a JSON parser or completion resolver.
pub(crate) fn cloned_value_material(value: &impl serde::Serialize) -> Result<(usize, usize)> {
    let mut counter = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    let records = counter
        .slots
        .checked_add(16)
        .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: cloned material"))?;
    Ok((usize::try_from(counter.material_bytes()?)?, records))
}

/// Only for existing Context types whose owning fields are serialized. This is
/// not the admission gate for parsing JSON or resolving completion aggregates.
pub(crate) fn admit_cloned_value(value: &impl serde::Serialize) -> Result<()> {
    if !super::ACTIVE.with(|slot| slot.borrow().is_some()) {
        return Ok(());
    }
    let mut counter = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    counter.admit_clone()
}

pub(crate) fn admit_serialized_value(value: &impl serde::Serialize) -> Result<()> {
    if !super::ACTIVE.with(|slot| slot.borrow().is_some()) {
        return Ok(());
    }
    let mut counter = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    counter.admit()
}

pub(crate) fn admit_json_payload(payload: &str) -> Result<()> {
    use std::io::Write;
    let mut counter = StructuredMaterialCounter::default();
    counter.write_all(payload.as_bytes())?;
    counter.admit()
}

/// One direct typed decode, with no completion aggregate/resolver copies.
/// Inline scalars still count as cells; the original eightfold allowance covers
/// owning strings, map entries and container capacity. Keep the bytes envelope.
pub(crate) fn admit_typed_json_payload(payload: &str) -> Result<()> {
    use std::io::Write;
    let mut counter = StructuredMaterialCounter::default();
    counter.write_all(payload.as_bytes())?;
    // Malformed suffixes must not erase an already admitted decode prefix.
    let records = counter.cells_peak;
    counter.admit_records(records)
}

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

    #[cfg(unix)]
    #[test]
    fn no_phase_keeps_non_utf8_current_paths_outside_the_json_gate() -> Result<()> {
        use std::os::unix::ffi::OsStringExt;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));
        assert!(serde_json::to_string(&path).is_err());
        admit_serialized_value(&path)?;
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
        let mut counter = StructuredMaterialCounter::default();
        serde_json::to_writer(&mut counter, &value)?;
        let slots = counter.slots;
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
        use std::io::Write;
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
            let mut whole = StructuredMaterialCounter::default();
            whole.write_all(payload.as_bytes())?;
            assert_eq!(whole.cells, cells(&value));
            assert_eq!(whole.cells_peak, whole.cells);
            for size in 1..=17 {
                let mut chunks = StructuredMaterialCounter::default();
                for bytes in payload.as_bytes().chunks(size) {
                    chunks.write_all(bytes)?;
                }
                assert_eq!(
                    (chunks.bytes, chunks.slots, chunks.cells, chunks.cells_peak),
                    (whole.bytes, whole.slots, whole.cells, whole.cells_peak),
                );
            }
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let header = phase.finish()?;
            {
                let _entry = phase.enter();
                admit_typed_json_payload(&payload)?;
            }
            let exact = phase.finish()?;
            assert_eq!(exact.0 - header.0, whole.material_bytes()? * 8);
            assert_eq!(exact.1 - header.1, cells(&value) * 8);
            drop(phase);
            let completion = ResourceReadMaterial::new(&ledger, &scope)?;
            {
                let _entry = completion.enter();
                admit_json_payload(&payload)?;
            }
            assert_eq!(completion.finish()?.0, exact.0);
            assert_eq!(
                completion.finish()?.1 - header.1,
                (whole.slots * 4 + 16) * 8,
            );
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

    #[test]
    fn completion_json_shape_counts_across_writer_chunks_not_body_punctuation() -> Result<()> {
        use serde_json::json;
        use std::io::Write;
        let value = json!({"body":"\\\"[{:}],".repeat(100), "nested":[{}, [], true]});
        let text = serde_json::to_string(&value)?;
        let mut whole = StructuredMaterialCounter::default();
        whole.write_all(text.as_bytes())?;
        let mut chunks = StructuredMaterialCounter::default();
        for byte in text.as_bytes() {
            chunks.write_all(&[*byte])?;
        }
        assert_eq!((whole.bytes, whole.slots), (chunks.bytes, chunks.slots));
        assert!(!chunks.quoted && !chunks.escaped);
        let mut short = StructuredMaterialCounter::default();
        serde_json::to_writer(&mut short, &json!({"body":"", "nested":[{}, [], true]}))?;
        assert_eq!(short.slots, whole.slots);
        Ok(())
    }
}
