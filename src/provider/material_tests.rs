use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "provider-material".into(),
        identity_incarnation: 1,
    }
}

#[tokio::test]
async fn image_turn_result_and_error_keep_material_without_changing_classification() -> Result<()> {
    for failed in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let source = ResourceReadMaterial::new(&ledger, &scope())?;
        let material = ProviderRequestMaterial::scoped(source.clone());
        material.json_value(&serde_json::json!({"text": "owned"}))?;
        let weak = Arc::downgrade(&source.guard()?);
        let error = crate::provider::retry::invalid_response_error("invalid image turn", "test");
        let text = error.to_string();
        let expected = crate::provider::retry::classify_provider_error(&error);
        let result = material
            .complete_turn(async {
                if failed {
                    Err(error)
                } else {
                    Ok(crate::provider::ProviderTurnResponse {
                        material: Default::default(),
                        blocks: Vec::new(),
                        stop_reason: None,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_usage: None,
                        provider_message_id: None,
                        provider_request_id: None,
                        request_diagnostics: None,
                    })
                }
            })
            .await;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop((material, source));
        assert!(weak.upgrade().is_some());
        if failed {
            let error = result.unwrap_err();
            let actual = crate::provider::retry::classify_provider_error(&error);
            assert_eq!(actual.kind, expected.kind);
            assert_eq!(actual.disposition, expected.disposition);
            assert_eq!(error.to_string(), text);
            assert!(error
                .chain()
                .any(|e| e.is::<crate::provider::retry::ProviderTransportError>()));
            drop(error);
        } else {
            let response = result?;
            let last = response.clone();
            drop(response);
            assert!(weak.upgrade().is_some());
            drop(last);
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn image_turn_refuses_mixed_acquisitions_and_standalone_images() -> Result<()> {
    use crate::provider::{ConversationMessage, ProviderImageData};
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let other_ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope())?;
    let other = ResourceReadMaterial::new(&other_ledger, &scope())?;
    let image = |data_base64| ConversationMessage::UserImage {
        prompt: "test".into(),
        media_type: "image/png".into(),
        data_base64,
        optional_history: false,
    };
    let first = image(ProviderImageData::encode(b"abc", &source)?);
    let material = ProviderRequestMaterial::from_images(std::slice::from_ref(&first))?;
    material.validate_images(std::slice::from_ref(&first))?;
    assert!(ProviderRequestMaterial::default()
        .validate_images(std::slice::from_ref(&first))
        .is_err());
    assert!(ProviderRequestMaterial::from_images(&[
        first.clone(),
        image(ProviderImageData::encode(b"def", &other)?),
    ])
    .is_err());
    assert!(ProviderRequestMaterial::from_images(&[
        first.clone(),
        image(String::from("YWJj").into()),
    ])
    .is_err());
    drop((first, material, source, other));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(other_ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn provider_material_refuses_foreign_ledger_and_incarnation() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let foreign = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let material = ProviderRequestMaterial::scoped(ResourceReadMaterial::new(&ledger, &scope)?);
    assert!(material.matches_runtime(&ledger, &scope.agent_id, 1));
    assert!(!material.matches_runtime(&foreign, &scope.agent_id, 1));
    assert!(!material.matches_runtime(&ledger, &scope.agent_id, 2));
    assert!(!material.matches_runtime(&ledger, "other", 1));
    drop(material);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(foreign.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn provider_wire_and_image_decode_admit_exact_bytes_and_records() -> Result<()> {
    let scope = scope();
    for image in [false, true] {
        let value = serde_json::json!({"prompt":"test"});
        let length = if image {
            3
        } else {
            serde_json::to_vec(&value)?.len()
        };
        let bytes = (scope.agent_id.len() as u64 + 256) * 8 + (length as u64 + 512) * 8;
        let records = 64 + 4 * 8;
        for shortage in [None, Some(true), Some(false)] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let source = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                bytes - u64::from(shortage == Some(true)),
                records - usize::from(shortage == Some(false)),
            )?;
            let material = ProviderRequestMaterial::scoped(source.clone());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let result = if image {
                material.decode_image("YWJj")
            } else {
                material.json_bytes(&value)
            };
            if shortage.is_none() {
                let buffer = result?;
                assert_eq!(source.finish()?, (bytes, records));
                let last = buffer.slice(1..);
                assert_eq!(last.as_ptr(), buffer.as_ptr().wrapping_add(1));
                let weak = Arc::downgrade(&source.guard()?);
                drop((buffer, material, source));
                assert!(weak.upgrade().is_some());
                drop(last);
                assert!(weak.upgrade().is_none());
            } else {
                assert!(result.is_err());
                assert!(source.finish().is_err());
                assert!(material.decode_image("").is_err());
                drop((material, source));
            }
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn generated_image_response_clones_share_body_and_last_holder() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope())?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let response = crate::provider::ProviderGenerateImageResponse {
        provider: "test".into(),
        model: "test".into(),
        images: vec![crate::provider::ProviderGeneratedImage {
            bytes: material.decode_image("YWJj")?,
            mime: None,
        }],
    };
    let cost = source.finish()?;
    let last = response.clone();
    assert_eq!(
        response.images[0].bytes.as_ptr(),
        last.images[0].bytes.as_ptr()
    );
    assert_eq!(source.finish()?, cost);
    let weak = Arc::downgrade(&source.guard()?);
    drop((response, source, material));
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

pub(crate) async fn response(
    body: &[u8],
    content_type: &str,
) -> Result<(String, tokio::task::JoinHandle<Result<()>>)> {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/", listener.local_addr()?);
    let response = [
        format!("HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes(),
        body,
    ].concat();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = [0; 8192];
        socket.read(&mut request).await?;
        socket.write_all(&response).await?;
        Ok(())
    });
    Ok((url, server))
}

#[tokio::test]
async fn provider_response_append_refuses_records_before_owning_copy() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let source = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 64)?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let (url, server) = response(b"abc", "text/plain").await?;
    let response = reqwest::get(url).await?;
    let error = material.response_text(response).await.unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    assert!(source.finish().is_err());
    server.await??;
    drop((material, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn provider_response_retains_lossy_utf8() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope())?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let (url, server) = response(b"a\xffb", "text/plain").await?;
    assert_eq!(
        material.response_text(reqwest::get(url).await?).await?,
        "a\u{fffd}b"
    );
    server.await??;
    drop((material, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
