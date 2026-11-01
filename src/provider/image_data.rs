//! Shared immutable image input; cloning a request does not copy its base64 body.
use std::{fmt, ops::Deref, sync::Arc};

use base64::Engine as _;
use serde::{Serialize, Serializer};

use crate::diagnostics::resource_io::{self, ResourceReadMaterial};

#[derive(Clone)]
pub struct ProviderImageData(Arc<ImageData>);

struct ImageData {
    encoded: String,
    _material: Option<ResourceReadMaterial>,
}

impl ProviderImageData {
    pub(crate) fn material(&self) -> Option<&ResourceReadMaterial> {
        self.0._material.as_ref()
    }

    pub(crate) fn data_url(&self, media_type: &str) -> anyhow::Result<String> {
        let material = self
            .material()
            .cloned()
            .map(super::ProviderRequestMaterial::scoped)
            .unwrap_or_default();
        material.read(|| {
            let length = self
                .len()
                .checked_add(media_type.len())
                .and_then(|n| n.checked_add(13))
                .ok_or_else(|| anyhow::anyhow!("image data URL size overflow"))?;
            // format!'s String and json!'s owned string coexist during lowering.
            resource_io::charge_owned_payload(
                u64::try_from(length)?
                    .checked_mul(2)
                    .ok_or_else(|| anyhow::anyhow!("image data URL size overflow"))?,
            )?;
            resource_io::charge_owned_metadata(512, 4)?;
            Ok(format!("data:{media_type};base64,{self}"))
        })
    }

    pub(crate) fn encode(bytes: &[u8], material: &ResourceReadMaterial) -> anyhow::Result<Self> {
        material.read(|| {
            let length = base64::encoded_len(bytes.len(), true)
                .ok_or_else(|| anyhow::anyhow!("image base64 size overflow"))?;
            resource_io::charge_owned_payload(u64::try_from(length)?)?;
            resource_io::charge_owned_metadata(512, 4)?;
            Ok(Self(Arc::new(ImageData {
                encoded: base64::engine::general_purpose::STANDARD.encode(bytes),
                _material: Some(material.clone()),
            })))
        })
    }
}

/// Standalone provider callers have no Runtime publication or budget authority.
impl From<String> for ProviderImageData {
    fn from(encoded: String) -> Self {
        Self(Arc::new(ImageData {
            encoded,
            _material: None,
        }))
    }
}

impl Deref for ProviderImageData {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0.encoded
    }
}

impl fmt::Display for ProviderImageData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self)
    }
}

impl fmt::Debug for ProviderImageData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderImageData")
            .field("encoded_bytes", &self.len())
            .finish_non_exhaustive()
    }
}

impl Serialize for ProviderImageData {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self)
    }
}

#[cfg(test)]
#[path = "image_data_tests.rs"]
mod tests;
