use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageParameterSupport {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenerationApi {
    OpenAiImages,
    XaiImages,
    CodexHostedTool,
    VolcengineImages,
    UnknownImages,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageParameterContract {
    pub support: ImageParameterSupport,
    pub value_type: String,
    pub allowed_values: Vec<String>,
    pub omittable: bool,
    pub default_value: Option<String>,
    pub default_source: String,
    pub default_sent: bool,
}

impl ImageParameterContract {
    pub fn unknown() -> Self {
        Self {
            support: ImageParameterSupport::Unknown,
            value_type: "string".into(),
            allowed_values: Vec::new(),
            omittable: true,
            default_value: None,
            default_source: "unknown".into(),
            default_sent: false,
        }
    }

    pub fn unsupported() -> Self {
        Self {
            support: ImageParameterSupport::Unsupported,
            ..Self::unknown()
        }
    }

    pub fn supported(values: &[&str], default: Option<&str>) -> Self {
        Self {
            support: ImageParameterSupport::Supported,
            allowed_values: values.iter().map(|v| (*v).into()).collect(),
            default_value: default.map(str::to_owned),
            default_source: if default.is_some() {
                "provider"
            } else {
                "unknown"
            }
            .into(),
            ..Self::unknown()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageSizeConstraints {
    pub min_pixels: u64,
    pub max_pixels: u64,
    pub max_edge: u32,
    pub edge_multiple: u32,
    pub max_aspect_ratio: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageGenerationContract {
    pub route: String,
    pub contract_version: String,
    pub api: ImageGenerationApi,
    pub image_generation: bool,
    pub size: ImageParameterContract,
    pub aspect_ratio: ImageParameterContract,
    pub resolution: ImageParameterContract,
    pub background: ImageParameterContract,
    pub native_output_format: ImageParameterContract,
    pub native_output_formats: Vec<String>,
    pub deliverable_formats: Vec<String>,
    pub default_output_format: String,
    pub size_constraints: Option<ImageSizeConstraints>,
    pub mutually_exclusive: Vec<Vec<String>>,
    pub compatibility_mappings: Value,
    pub evidence_status: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageGenerationParameters {
    pub size: Option<String>,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub background: Option<String>,
    /// Final runtime delivery format, not an unconditional wire parameter.
    pub output_format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedImageGenerationParameters {
    pub requested: ImageGenerationParameters,
    pub provider_parameters: ImageGenerationParameters,
    pub effective_parameters: Value,
    pub delivery_format: String,
    pub defaults: Value,
    pub mapping: Value,
}

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{code}: {field} on {route}: {message}")]
pub struct ImageGenerationParameterError {
    pub code: &'static str,
    pub route: String,
    pub field: String,
    pub message: String,
    pub allowed_values: Vec<String>,
    pub recovery_hint: String,
}

impl ImageGenerationContract {
    fn error(
        &self,
        code: &'static str,
        field: &str,
        message: &str,
        allowed: &[String],
    ) -> ImageGenerationParameterError {
        ImageGenerationParameterError {
            code, route: self.route.clone(), field: field.into(), message: message.into(),
            allowed_values: allowed.to_vec(),
            recovery_hint: "omit the optional generation parameter or select a route with confirmed support; do not retry a paid generation to fix local delivery".into(),
        }
    }

    fn check(
        &self,
        field: &str,
        value: &Option<String>,
        c: &ImageParameterContract,
    ) -> Result<(), ImageGenerationParameterError> {
        let Some(value) = value else { return Ok(()) };
        match c.support {
            ImageParameterSupport::Unsupported => {
                return Err(self.error(
                    "unsupported_image_parameter",
                    field,
                    "parameter is unsupported",
                    &c.allowed_values,
                ))
            }
            ImageParameterSupport::Unknown => {
                return Err(self.error(
                    "image_parameter_capability_unknown",
                    field,
                    "parameter support is unconfirmed",
                    &c.allowed_values,
                ))
            }
            ImageParameterSupport::Supported => {}
        }
        if c.allowed_values.iter().any(|v| v == value) {
            return Ok(());
        }
        if field == "size" {
            if let (Some(constraints), Some((w, h))) =
                (&self.size_constraints, parse_image_dimensions(value))
            {
                let pixels = u64::from(w) * u64::from(h);
                if w > 0
                    && h > 0
                    && w <= constraints.max_edge
                    && h <= constraints.max_edge
                    && w % constraints.edge_multiple == 0
                    && h % constraints.edge_multiple == 0
                    && pixels >= constraints.min_pixels
                    && pixels <= constraints.max_pixels
                    && u64::from(w.max(h))
                        <= u64::from(w.min(h)) * u64::from(constraints.max_aspect_ratio)
                {
                    return Ok(());
                }
            }
        }
        Err(self.error(
            "invalid_image_parameter",
            field,
            "value violates the route's allowed values or dimension constraints",
            &c.allowed_values,
        ))
    }

    pub fn resolve_parameters(
        &self,
        p: &ImageGenerationParameters,
    ) -> Result<ResolvedImageGenerationParameters, ImageGenerationParameterError> {
        if !self.image_generation {
            return Err(self.error(
                "media_capability_mismatch",
                "model",
                "route cannot generate images",
                &[],
            ));
        }
        let target = p.output_format.as_deref().unwrap_or("png");
        if !self.deliverable_formats.iter().any(|f| f == target) {
            return Err(self.error(
                "invalid_image_parameter",
                "output_format",
                "expected png, jpeg or webp",
                &self.deliverable_formats,
            ));
        }
        if p.size.is_some() && (p.aspect_ratio.is_some() || p.resolution.is_some()) {
            return Err(self.error(
                "invalid_image_parameter",
                "size",
                "size and aspect_ratio/resolution are mutually exclusive",
                &[],
            ));
        }
        if p.background.as_deref() == Some("transparent") && target == "jpeg" {
            return Err(self.error(
                "invalid_image_parameter",
                "output_format",
                "JPEG cannot preserve a transparent background; use png or webp",
                &self.deliverable_formats,
            ));
        }
        self.check("size", &p.size, &self.size)?;
        self.check("aspect_ratio", &p.aspect_ratio, &self.aspect_ratio)?;
        self.check("resolution", &p.resolution, &self.resolution)?;
        self.check("background", &p.background, &self.background)?;
        let mut wire = p.clone();
        wire.output_format = if self.native_output_formats.iter().any(|f| f == target) {
            Some(target.into())
        } else {
            None
        };
        let mut mapping = json!(null);
        if self.api == ImageGenerationApi::XaiImages {
            if let Some(size) = &p.size {
                let mapped = &self.compatibility_mappings[size];
                wire.size = None;
                wire.aspect_ratio = mapped["aspect_ratio"].as_str().map(str::to_owned);
                wire.resolution = mapped["resolution"].as_str().map(str::to_owned);
                mapping = mapped.clone();
            }
        }
        let effective = |value: &Option<String>, field: &ImageParameterContract, source: &str| {
            json!({
                "value": value.as_ref().or(field.default_value.as_ref()),
                "source": if value.is_some() { source } else { &field.default_source },
                "sent": value.is_some(),
            })
        };
        let effective_parameters = json!({
            "size": effective(&wire.size, &self.size, "caller"),
            "aspect_ratio": effective(&wire.aspect_ratio, &self.aspect_ratio,
                if mapping.is_null() { "caller" } else { "adapter_mapping" }),
            "resolution": effective(&wire.resolution, &self.resolution,
                if mapping.is_null() { "caller" } else { "adapter_mapping" }),
            "background": effective(&wire.background, &self.background, "caller"),
            "native_output_format": effective(&wire.output_format, &self.native_output_format, "adapter"),
        });
        Ok(ResolvedImageGenerationParameters {
            requested: p.clone(),
            provider_parameters: wire,
            effective_parameters,
            delivery_format: target.into(),
            defaults: json!({
                "size": self.size, "aspect_ratio": self.aspect_ratio,
                "resolution": self.resolution, "background": self.background,
                "native_output_format": self.native_output_format,
                "delivery_format": {"value": target, "source": if p.output_format.is_some() { "caller" } else { "runtime" }},
            }),
            mapping,
        })
    }
}

pub fn parse_image_dimensions(value: &str) -> Option<(u32, u32)> {
    let (w, h) = value.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageConversionMetadata {
    pub source_format: String,
    pub source_mime: String,
    pub source_width: u32,
    pub source_height: u32,
    pub delivered_format: String,
    pub delivered_mime: String,
    pub converted: bool,
    pub orientation_applied: bool,
    pub encoding_policy: String,
    pub width: u32,
    pub height: u32,
}
