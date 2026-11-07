use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

#[test]
fn generated_image_model_render_reenters_original_material_and_keeps_wire_clean() -> Result<()> {
    for exhausted in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "image-result".into(),
            identity_incarnation: 1,
        };
        let source = if exhausted {
            ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 64)?
        } else {
            ResourceReadMaterial::new(&ledger, &scope)?
        };
        let mut result = serialize_success(
            NAME,
            &GenerateImageResult {
                images: Vec::new(),
                provider: "test".into(),
                model: "test".into(),
                prompt: "draw".into(),
                parameters: GenerateImageParameters {
                    size: None,
                    aspect_ratio: None,
                    resolution: None,
                    background: None,
                    output_format: None,
                    name: None,
                },
                selection: None,
                generation_succeeded: None,
                requested_parameters: None,
                provider_parameters: None,
                provider_effective_parameters: None,
                parameter_defaults: None,
                parameter_mapping: None,
                contract: None,
                source_images: Vec::new(),
                conversion: Vec::new(),
                summary_text: None,
            },
        )?;
        let wire = serde_json::to_value(&result)?;
        result.resource_material = Some(source.clone());
        assert_eq!(serde_json::to_value(&result)?, wire);
        let rendered = render_for_model(&result);
        if exhausted {
            assert!(rendered
                .unwrap_err()
                .to_string()
                .contains("ResourceExhausted"));
            assert!(source.finish().is_err());
        } else {
            assert_eq!(
                rendered?,
                "GenerateImage result\nProvider/model: test/test\nPrompt: draw"
            );
            let last = result.clone();
            let weak = Arc::downgrade(&source.guard()?);
            drop((result, source));
            assert!(weak.upgrade().is_some());
            drop(last);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, 0);
            continue;
        }
        drop((result, source));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
