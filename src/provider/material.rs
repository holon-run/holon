//! Explicit Runtime acquisition for provider-owned buffers. Default is an
//! unscoped standalone provider call, not a Runtime budget capability.
use std::{fmt, io::Write};

use anyhow::{Context, Result};
use base64::Engine as _;
use serde::Serialize;
use tokio_util::bytes::Bytes;

use crate::diagnostics::resource_io::{self, ResourceReadMaterial};

#[derive(Clone, Default)]
pub struct ProviderRequestMaterial(Option<ResourceReadMaterial>);

impl fmt::Debug for ProviderRequestMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRequestMaterial")
            .field("scoped", &self.0.is_some())
            .finish()
    }
}

impl fmt::Write for SerializedLength {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        Write::write_all(self, text.as_bytes()).map_err(|_| fmt::Error)
    }
}

#[derive(Debug)]
struct MaterialError {
    _material: ProviderRequestMaterial,
    display: Option<String>,
}

impl fmt::Display for MaterialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            self.display
                .as_deref()
                .unwrap_or("ResourceExhausted: provider request material"),
        )
    }
}

#[cfg(test)]
#[path = "material_tests.rs"]
pub(crate) mod tests;

struct OwnedBuffer {
    bytes: Vec<u8>,
    _material: ProviderRequestMaterial,
}

impl AsRef<[u8]> for OwnedBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Default)]
struct SerializedLength(usize);

impl Write for SerializedLength {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("provider JSON size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl ProviderRequestMaterial {
    pub(crate) fn from_images(
        conversation: &[crate::provider::ConversationMessage],
    ) -> Result<Self> {
        let material = conversation
            .iter()
            .filter_map(|message| match message {
                crate::provider::ConversationMessage::UserImage { data_base64, .. } => {
                    data_base64.material()
                }
                _ => None,
            })
            .next()
            .cloned()
            .map(Self::scoped)
            .unwrap_or_default();
        material.validate_images(conversation)?;
        Ok(material)
    }

    pub(crate) fn validate_images(
        &self,
        conversation: &[crate::provider::ConversationMessage],
    ) -> Result<()> {
        for message in conversation {
            if let crate::provider::ConversationMessage::UserImage { data_base64, .. } = message {
                anyhow::ensure!(
                    match (&self.0, data_base64.material()) {
                        (Some(owner), Some(source)) => owner.shares_allowance(source),
                        (None, None) => true,
                        _ => false,
                    },
                    "provider image differs from the explicit request acquisition"
                );
            }
        }
        Ok(())
    }

    pub(crate) fn cloned<T: Serialize + Clone>(&self, value: &T) -> Result<T> {
        self.read(|| resource_io::admit_cloned_value(value))?;
        Ok(value.clone())
    }

    pub(crate) fn cloned_slice<T: Serialize + Clone>(&self, value: &[T]) -> Result<Vec<T>> {
        self.read(|| resource_io::admit_cloned_value(&value))?;
        Ok(value.to_vec())
    }

    pub(crate) fn json_value(&self, value: &impl Serialize) -> Result<serde_json::Value> {
        self.read(|| resource_io::admit_cloned_value(value))?;
        Ok(serde_json::to_value(value)?)
    }

    pub(crate) fn hold_error(&self, error: anyhow::Error) -> anyhow::Error {
        if !self.is_scoped() {
            return error;
        }
        let display = self.read(|| {
            let mut length = SerializedLength::default();
            fmt::write(&mut length, format_args!("{error}"))?;
            resource_io::charge_owned_payload(u64::try_from(length.0)?)?;
            resource_io::charge_owned_metadata(512, 4)?;
            let mut text = String::with_capacity(length.0);
            fmt::write(&mut text, format_args!("{error}"))?;
            Ok(text)
        });
        // anyhow's Context preserves typed downcasts; a generic Error wrapper
        // does not. The display copy is admitted before formatting.
        match display {
            Ok(display) => error.context(MaterialError {
                _material: self.clone(),
                display: Some(display),
            }),
            Err(error) => error.context(MaterialError {
                _material: self.clone(),
                display: None,
            }),
        }
    }

    pub(crate) async fn complete_turn(
        &self,
        future: impl std::future::Future<Output = Result<crate::provider::ProviderTurnResponse>>,
    ) -> Result<crate::provider::ProviderTurnResponse> {
        let result = future.await;
        self.read(|| Ok(()))
            .map_err(|error| self.hold_error(error))?;
        match result {
            Ok(mut response) => {
                response.material = self.clone();
                Ok(response)
            }
            Err(error) => Err(self.hold_error(error)),
        }
    }

    pub(crate) async fn complete_image(
        &self,
        future: impl std::future::Future<
            Output = Result<crate::provider::ProviderGenerateImageResponse>,
        >,
    ) -> Result<crate::provider::ProviderGenerateImageResponse> {
        let result = future.await;
        self.read(|| Ok(()))
            .map_err(|error| self.hold_error(error))?;
        result.map_err(|error| self.hold_error(error))
    }

    pub(crate) fn is_scoped(&self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn scoped(material: ResourceReadMaterial) -> Self {
        Self(Some(material))
    }

    pub(crate) fn matches_runtime(
        &self,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        agent_id: &str,
        incarnation: u64,
    ) -> bool {
        self.0
            .as_ref()
            .is_some_and(|m| m.matches_owner_identity(ledger, agent_id, incarnation))
    }

    pub(crate) fn read<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        match &self.0 {
            Some(material) => material.read(action),
            None => action(),
        }
    }

    pub(crate) fn admit_value(&self, value: &impl Serialize) -> Result<()> {
        self.read(|| resource_io::admit_serialized_value(value))
    }

    /// The HTTP body itself retains the acquisition through retries and slices.
    pub(crate) fn json_bytes(&self, value: &impl Serialize) -> Result<Bytes> {
        self.read(|| {
            let mut length = SerializedLength::default();
            serde_json::to_writer(&mut length, value)?;
            resource_io::charge_owned_payload(u64::try_from(length.0)?)?;
            resource_io::charge_owned_metadata(512, 4)?;
            let mut bytes = Vec::with_capacity(length.0);
            serde_json::to_writer(&mut bytes, value)?;
            Ok(self.hold_bytes(bytes))
        })
    }

    fn hold_bytes(&self, bytes: Vec<u8>) -> Bytes {
        Bytes::from_owner(OwnedBuffer {
            bytes,
            _material: self.clone(),
        })
    }

    pub(crate) fn decode_image(&self, encoded: &str) -> Result<Bytes> {
        self.read(|| {
            let length = base64::decoded_len_estimate(encoded.len());
            resource_io::charge_owned_payload(u64::try_from(length)?)?;
            resource_io::charge_owned_metadata(512, 4)?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
            Ok(self.hold_bytes(bytes))
        })
    }

    /// Network chunks are borrowed transport buffers. Admit each owning append
    /// before copying; do not let Content-Length decide admission or capacity.
    pub(crate) async fn response_text(&self, mut response: reqwest::Response) -> Result<String> {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            self.read(|| {
                resource_io::charge_owned_payload(u64::try_from(chunk.len())?)?;
                resource_io::charge_owned_metadata(128, 1)?;
                bytes.extend_from_slice(&chunk);
                Ok(())
            })?;
        }
        // Reqwest's text() performs lossy UTF-8 decoding. Preserve that contract.
        self.read(|| match String::from_utf8(bytes) {
            Ok(text) => Ok(text),
            Err(error) => {
                let bytes = error.as_bytes();
                resource_io::charge_owned_payload(
                    u64::try_from(bytes.len())?
                        .checked_mul(3)
                        .context("provider text size overflow")?,
                )?;
                resource_io::charge_owned_metadata(128, 1)?;
                Ok(String::from_utf8_lossy(bytes).into_owned())
            }
        })
    }
}
