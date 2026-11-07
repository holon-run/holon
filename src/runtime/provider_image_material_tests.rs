use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope, tests::support::StubProvider,
    },
};
use std::sync::Arc;

fn image_prompt() -> anyhow::Result<(tempfile::TempDir, EffectivePrompt)> {
    let workspace = tempfile::tempdir()?;
    std::fs::write(workspace.path().join("image.png"), tests::fixture_png())?;
    let mut prompt = tests::fixture_prompt();
    prompt.execution.workspace_id = Some("ws_image".into());
    prompt.execution.workspace_anchor = workspace.path().into();
    prompt.execution.execution_root = workspace.path().into();
    prompt.execution.cwd = workspace.path().into();
    prompt.context_sections = vec![PromptSection {
        name: "current_input".into(),
        id: "current_input".into(),
        content: "Inspect ![Chart](workspace://ws_image/image.png).".into(),
        stability: PromptStability::TurnScoped,
    }];
    Ok((workspace, prompt))
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: 1,
    }
}

#[test]
fn markdown_image_request_carries_source_through_last_request_clone() -> anyhow::Result<()> {
    let (workspace, prompt) = image_prompt()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let material = ResourceReadMaterial::new(&ledger, &scope())?;
    let request = build_initial_provider_turn_request_with_material(
        &StubProvider::new("unused"),
        &prompt,
        Vec::new(),
        None,
        &material,
    )?;
    let ConversationMessage::UserImage { data_base64, .. } = &request.conversation[1] else {
        anyhow::bail!("missing admitted image")
    };
    assert_eq!(
        base64::engine::general_purpose::STANDARD.decode(data_base64.as_bytes())?,
        tests::fixture_png(),
    );
    let pointer = data_base64.as_ptr();
    let usage = material.finish()?;
    let last = request.clone();
    let ConversationMessage::UserImage { data_base64, .. } = &last.conversation[1] else {
        anyhow::bail!("missing cloned image")
    };
    assert_eq!(
        pointer,
        data_base64.as_ptr(),
        "request clones share encoded bytes"
    );
    assert_eq!(usage, material.finish()?);
    let weak = Arc::downgrade(&material.guard()?);
    drop((request, material, prompt, workspace));
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn markdown_image_growth_is_a_latched_failure_not_an_omitted_attachment() -> anyhow::Result<()> {
    let (workspace, prompt) = image_prompt()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let material = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 32 * 1024, 4096)?;
    let path = workspace.path().join("image.png");
    material.after_file_metadata(move || {
        std::fs::write(path, vec![0; 64 * 1024]).unwrap();
    });
    let result = build_initial_provider_turn_request_with_material(
        &StubProvider::new("unused"),
        &prompt,
        Vec::new(),
        None,
        &material,
    );
    assert!(
        result.is_err(),
        "resource exhaustion cannot silently omit an input"
    );
    assert!(material.finish().is_err());
    drop(material);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn markdown_image_refuses_foreign_material_and_preserves_path_filtering() -> anyhow::Result<()> {
    let (_workspace, mut prompt) = image_prompt()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let mut foreign = scope();
    foreign.identity_incarnation += 1;
    let material = ResourceReadMaterial::new(&ledger, &foreign)?;
    assert!(build_initial_provider_turn_request_with_material(
        &StubProvider::new("unused"),
        &prompt,
        Vec::new(),
        None,
        &material,
    )
    .is_err());
    drop(material);
    let material = ResourceReadMaterial::new(&ledger, &scope())?;
    prompt.context_sections[0].content =
        "![remote](https://example.com/image.png) ![escape](workspace://ws_image/../image.png)"
            .into();
    let request = build_initial_provider_turn_request_with_material(
        &StubProvider::new("unused"),
        &prompt,
        Vec::new(),
        None,
        &material,
    )?;
    assert_eq!(request.conversation.len(), 1);
    drop((request, material));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
