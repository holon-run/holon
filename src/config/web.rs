use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderBuiltinWebSearchConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub kind: ProviderNativeWebSearchKind,
    pub advertised_tool_type: String,
    pub backend_kind: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebCommandOutputFormatFile {
    #[default]
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCommandResultMappingFile {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

pub(crate) fn web_provider_config_mut<'a>(
    config: &'a mut HolonConfigFile,
    key: &str,
    suffix: &str,
) -> Result<&'a mut WebProviderConfigFile> {
    let rest = key.strip_prefix("web.providers.").unwrap();
    let name = rest.strip_suffix(suffix).unwrap();
    if name.is_empty() {
        return Err(anyhow!(
            "web.providers.<name>{suffix} requires a non-empty provider name"
        ));
    }
    config.web.providers.get_mut(name).ok_or_else(|| {
        anyhow!("web provider {name} not found; set web.providers.{name}.kind first")
    })
}

pub(crate) fn web_provider_output_mapping_key(rest: &str) -> Option<(&str, &str)> {
    let (name, field) = rest.split_once(".output.mapping.")?;
    matches!(field, "title" | "url" | "snippet" | "published_at").then_some((name, field))
}

pub(crate) fn output_mapping_field<'a>(
    mapping: &'a WebCommandResultMappingFile,
    field: &str,
) -> Option<&'a str> {
    match field {
        "title" => (!mapping.title.is_empty()).then_some(mapping.title.as_str()),
        "url" => (!mapping.url.is_empty()).then_some(mapping.url.as_str()),
        "snippet" => mapping.snippet.as_deref(),
        "published_at" => mapping.published_at.as_deref(),
        _ => None,
    }
}

pub(crate) fn set_output_mapping_field(
    mapping: &mut WebCommandResultMappingFile,
    field: &str,
    value: &str,
) {
    match field {
        "title" => mapping.title = value.to_string(),
        "url" => mapping.url = value.to_string(),
        "snippet" => {
            mapping.snippet = (!value.is_empty()).then(|| value.to_string());
        }
        "published_at" => {
            mapping.published_at = (!value.is_empty()).then(|| value.to_string());
        }
        _ => {}
    }
}

pub(crate) fn unset_output_mapping_field(mapping: &mut WebCommandResultMappingFile, field: &str) {
    match field {
        "title" => mapping.title.clear(),
        "url" => mapping.url.clear(),
        "snippet" => mapping.snippet = None,
        "published_at" => mapping.published_at = None,
        _ => {}
    }
}

pub(crate) fn read_only_web_provider_capabilities_key_error(key: &str) -> anyhow::Error {
    anyhow!(
        "{key} is derived read-only capability metadata; configure web.providers.<name>.kind instead"
    )
}
