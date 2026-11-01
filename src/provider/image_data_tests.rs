use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

#[test]
fn image_encoding_admits_exact_bytes_and_records_before_allocating() -> anyhow::Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "image-data".into(),
        identity_incarnation: 1,
    };
    let bytes = (scope.agent_id.len() as u64 + 256) * 8 + (4 + 512) * 8;
    let records = 64 + 4 * 8;
    for shortage in [None, Some(true), Some(false)] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let material = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            bytes - u64::from(shortage == Some(true)),
            records - usize::from(shortage == Some(false)),
        )?;
        let result = ProviderImageData::encode(b"png", &material);
        if shortage.is_none() {
            let data = result?;
            assert_eq!(&*data, "cG5n");
            assert_eq!(serde_json::to_string(&data)?, "\"cG5n\"");
            assert_eq!(material.finish()?, (bytes, records));
            let last = data.clone();
            assert!(std::ptr::eq(data.as_ptr(), last.as_ptr()));
            assert_eq!(
                material.finish()?,
                (bytes, records),
                "clones share the body"
            );
            let weak = Arc::downgrade(&material.guard()?);
            drop((data, material));
            assert!(weak.upgrade().is_some());
            drop(last);
            assert!(weak.upgrade().is_none());
        } else {
            assert!(result.is_err());
            assert!(material.finish().is_err());
            assert!(ProviderImageData::encode(b"", &material).is_err());
            drop(material);
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
