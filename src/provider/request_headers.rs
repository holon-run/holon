use sha2::{Digest, Sha256};

use crate::config::{ProviderHeaders, ProviderRuntimeConfig};

/// Installation- and incarnation-scoped affinity, independent of prompt caching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionId(String);

impl ProviderSessionId {
    pub(crate) fn from_runtime_identity(
        runtime_id: &str,
        agent_id: &str,
        incarnation: u64,
    ) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"holon/provider-session/v1\0");
        for part in [runtime_id.as_bytes(), agent_id.as_bytes()] {
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part);
        }
        hash.update(incarnation.to_be_bytes());
        Self(format!("holon-v1-{:x}", hash.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Route-local headers. Never install these on a shared HTTP client.
#[derive(Debug, Clone, Default)]
pub(super) struct ProviderRequestHeaders {
    configured: ProviderHeaders,
    opencode_go: bool,
}

impl ProviderRequestHeaders {
    pub(super) fn new(config: &ProviderRuntimeConfig) -> Self {
        Self {
            configured: config.headers.clone(),
            opencode_go: config.route_provider.as_str() == "opencode-go",
        }
    }

    pub(super) fn compose<'a>(
        &'a self,
        mut headers: Vec<(&'a str, String)>,
        session: Option<&ProviderSessionId>,
    ) -> anyhow::Result<Vec<(&'a str, String)>> {
        self.require_session(session)?;
        if let (true, Some(session)) = (self.opencode_go, session) {
            headers.push(("x-opencode-session", session.as_str().to_owned()));
            headers.push(("user-agent", format!("holon/{}", env!("CARGO_PKG_VERSION"))));
        }
        for (name, value) in self.configured.iter() {
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            headers.push((name, value.to_owned()));
        }
        Ok(headers)
    }

    pub(super) fn require_session(
        &self,
        session: Option<&ProviderSessionId>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.opencode_go || session.is_some(),
            "opencode-go inference requires a trusted provider session context"
        );
        Ok(())
    }
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_session_identity_is_stable_opaque_and_namespaced() {
        let session = ProviderSessionId::from_runtime_identity("installation", "agent", 1);
        assert_eq!(
            session,
            ProviderSessionId::from_runtime_identity("installation", "agent", 1)
        );
        assert_eq!(session.as_str().len(), 73);
        assert!(reqwest::header::HeaderValue::from_str(session.as_str()).is_ok());
        for other in [
            ProviderSessionId::from_runtime_identity("other", "agent", 1),
            ProviderSessionId::from_runtime_identity("installation", "other", 1),
            ProviderSessionId::from_runtime_identity("installation", "agent", 2),
            ProviderSessionId::from_runtime_identity("installationa", "gent", 1),
        ] {
            assert_ne!(session, other);
        }
        assert_ne!(
            ProviderSessionId::from_runtime_identity("ab", "c", 1),
            ProviderSessionId::from_runtime_identity("a", "bc", 1)
        );
        assert!(!session.as_str().contains("installation"));
        assert!(!session.as_str().contains("agent"));
    }
}
