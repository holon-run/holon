use crate::ImageGenerationContract;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaOperation {
    ViewImage,
    GenerateImage,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaToolDefaults {
    pub view_image: Option<String>,
    pub generate_image: Option<String>,
    pub output_format: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaSupport {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaOperationAvailability {
    pub support: MediaSupport,
    pub call_selectable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaModelCapabilities {
    pub image_input: MediaSupport,
    /// Image generation, not a claim that an ordinary text turn returns images.
    pub image_output: MediaSupport,
    pub view_image: MediaOperationAvailability,
    pub generate_image: MediaOperationAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation_contract: Option<ImageGenerationContract>,
}

impl MediaModelCapabilities {
    pub fn operation(&self, operation: MediaOperation) -> &MediaOperationAvailability {
        match operation {
            MediaOperation::ViewImage => &self.view_image,
            MediaOperation::GenerateImage => &self.generate_image,
        }
    }

    pub fn deny_tool_access(&mut self) {
        for operation in [&mut self.view_image, &mut self.generate_image] {
            operation.call_selectable = false;
            operation.unavailable_reason = Some("agent_capability_not_allowed".into());
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaProviderCapabilities {
    pub image_input: MediaSupport,
    pub image_output: MediaSupport,
    pub view_image_selectable_models: usize,
    pub generate_image_selectable_models: usize,
    pub observed_routes: usize,
    /// Catalog/discovery may not enumerate every model the service offers.
    pub inventory_complete: bool,
}

impl MediaProviderCapabilities {
    pub fn selectable_models(&self, operation: MediaOperation) -> usize {
        match operation {
            MediaOperation::ViewImage => self.view_image_selectable_models,
            MediaOperation::GenerateImage => self.generate_image_selectable_models,
        }
    }

    pub fn aggregate<'a>(models: impl IntoIterator<Item = &'a MediaModelCapabilities>) -> Self {
        let models = models.into_iter().collect::<Vec<_>>();
        Self {
            image_input: aggregate_support(models.iter().map(|model| model.image_input)),
            image_output: aggregate_support(models.iter().map(|model| model.image_output)),
            view_image_selectable_models: models
                .iter()
                .filter(|model| model.view_image.call_selectable)
                .count(),
            generate_image_selectable_models: models
                .iter()
                .filter(|model| model.generate_image.call_selectable)
                .count(),
            observed_routes: models.len(),
            inventory_complete: false,
        }
    }
}

fn aggregate_support(values: impl Iterator<Item = MediaSupport>) -> MediaSupport {
    if values
        .into_iter()
        .any(|value| value == MediaSupport::Supported)
    {
        MediaSupport::Supported
    } else {
        // Absence in a partial inventory is not evidence of service-wide absence.
        MediaSupport::Unknown
    }
}
