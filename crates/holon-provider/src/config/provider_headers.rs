use holon_domain::model_values::ProviderId;
use std::{collections::BTreeMap, fmt};

use anyhow::{anyhow, Result};
use reqwest::header::{HeaderName, HeaderValue};
use serde::{
    de::{Error, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};

const MAX_HEADERS: usize = 32;
const MAX_VALUE_BYTES: usize = 4096;
const MAX_TOTAL_BYTES: usize = 16384;

// Authentication, protocol negotiation, hop-by-hop, and adapter-owned routing.
const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "cookie",
    "set-cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "upgrade",
    "content-type",
    "accept",
    "accept-encoding",
    "anthropic-version",
    "anthropic-beta",
    "openai-beta",
    "openai-organization",
    "openai-project",
    "chatgpt-account-id",
    "originator",
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-turn-state",
    "x-codex-turn-metadata",
    "x-codex-beta-features",
    "x-codex-routing-hint",
];

/// Validated, lowercase static request headers. Debug never exposes values.
#[derive(Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProviderHeaders(BTreeMap<String, String>);

impl fmt::Debug for ProviderHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderHeaders")
            .field("names", &self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ProviderHeaders {
    pub fn new(headers: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        let mut result = BTreeMap::new();
        for (name, value) in headers {
            let name = Self::normalize_name(&name)?;
            if result.contains_key(&name) {
                return Err(anyhow!("duplicate provider header name '{name}'"));
            }
            if value.len() > MAX_VALUE_BYTES {
                return Err(anyhow!("provider header '{name}' exceeds 4096 value bytes"));
            }
            if value.contains(['\r', '\n']) || HeaderValue::from_bytes(value.as_bytes()).is_err() {
                return Err(anyhow!("invalid value for provider header '{name}'"));
            }
            result.insert(name, value);
            Self::validate_size(&result)?;
        }
        Ok(Self(result))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    pub fn merged(&self, overrides: &Self) -> Result<Self> {
        let mut merged = self.0.clone();
        merged.extend(overrides.0.clone());
        Self::validate_size(&merged)?;
        Ok(Self(merged))
    }

    pub fn normalize_name(name: &str) -> Result<String> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("invalid provider header name"))?
            .as_str()
            .to_string();
        if RESERVED_HEADERS.contains(&name.as_str()) {
            return Err(anyhow!("provider header '{name}' is reserved"));
        }
        Ok(name)
    }

    pub fn validate_for_route(&self, provider: &ProviderId) -> Result<()> {
        if provider.as_str() == "opencode-go" && self.0.contains_key("x-opencode-session") {
            return Err(anyhow!(
                "provider header 'x-opencode-session' is owned by the OpenCode Go adapter"
            ));
        }
        Ok(())
    }

    pub fn without(&self, names: &[String]) -> Self {
        let mut remaining = self.0.clone();
        for name in names {
            remaining.remove(name);
        }
        Self(remaining)
    }

    fn validate_size(headers: &BTreeMap<String, String>) -> Result<()> {
        if headers.len() > MAX_HEADERS {
            return Err(anyhow!("provider headers exceed 32 entries"));
        }
        let bytes: usize = headers
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum();
        if bytes > MAX_TOTAL_BYTES {
            return Err(anyhow!("provider headers exceed 16384 total bytes"));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ProviderHeaders {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HeadersVisitor;

        impl<'de> Visitor<'de> for HeadersVisitor {
            type Value = ProviderHeaders;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of non-sensitive static provider headers")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut headers = ProviderHeaders::default();
                while let Some(name) = map
                    .next_key::<String>()
                    .map_err(|_| A::Error::custom("invalid provider header name"))?
                {
                    let value = map
                        .next_value::<String>()
                        .map_err(|_| A::Error::custom("provider header values must be strings"))?;
                    let entry = ProviderHeaders::new([(name, value)]).map_err(A::Error::custom)?;
                    if entry.0.keys().any(|name| headers.0.contains_key(name)) {
                        return Err(A::Error::custom("duplicate provider header name"));
                    }
                    headers = headers.merged(&entry).map_err(A::Error::custom)?;
                }
                Ok(headers)
            }
        }

        deserializer.deserialize_map(HeadersVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_headers_reserve_transport_owned_headers_but_allow_user_agent() {
        for name in RESERVED_HEADERS {
            assert!(ProviderHeaders::new([(name.to_uppercase(), "not-disclosed".into())]).is_err());
        }
        assert!(ProviderHeaders::new([("User-Agent".into(), "custom-client".into())]).is_ok());
    }
}
