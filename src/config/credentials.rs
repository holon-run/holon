use super::*;

pub use holon_provider::config::{
    load_credential_store_at, save_credential_store_at, CredentialProfileFile, CredentialStoreFile,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialProfileStatus {
    pub profile: String,
    pub kind: String,
    pub configured: bool,
}

pub fn credential_store_path(home_dir: &Path) -> PathBuf {
    home_dir.join("credentials.json")
}

pub fn set_credential_profile_at(
    path: &Path,
    profile: &str,
    kind: CredentialKind,
    material: String,
) -> Result<CredentialProfileStatus> {
    let profile = normalize_credential_profile_id(profile)?;
    validate_stored_credential_kind(kind)?;
    if material.trim().is_empty() {
        return Err(anyhow!("credential material must not be empty"));
    }
    let mut store = load_credential_store_at(path)?;
    store
        .profiles
        .insert(profile.clone(), CredentialProfileFile { kind, material });
    save_credential_store_at(path, &store)?;
    Ok(CredentialProfileStatus {
        profile,
        kind: kind.as_str().to_string(),
        configured: true,
    })
}

pub fn remove_credential_profile_at(path: &Path, profile: &str) -> Result<CredentialProfileStatus> {
    let profile = normalize_credential_profile_id(profile)?;
    let mut store = load_credential_store_at(path)?;
    let removed = store.profiles.remove(&profile);
    save_credential_store_at(path, &store)?;
    Ok(CredentialProfileStatus {
        profile,
        kind: removed
            .map(|entry| entry.kind.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        configured: false,
    })
}

pub fn list_credential_profiles_at(path: &Path) -> Result<Vec<CredentialProfileStatus>> {
    let store = load_credential_store_at(path)?;
    Ok(store
        .profiles
        .into_iter()
        .map(|(profile, entry)| CredentialProfileStatus {
            profile,
            kind: entry.kind.as_str().to_string(),
            configured: !entry.material.trim().is_empty(),
        })
        .collect())
}

pub(crate) fn normalize_credential_profile_id(profile: &str) -> Result<String> {
    let trimmed = profile.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("credential profile id must not be empty"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(anyhow!(
            "credential profile id must not contain control characters"
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn validate_stored_credential_kind(kind: CredentialKind) -> Result<()> {
    match kind {
        CredentialKind::ApiKey
        | CredentialKind::BearerToken
        | CredentialKind::OAuth
        | CredentialKind::SessionToken => Ok(()),
        CredentialKind::AwsSdk | CredentialKind::None => Err(anyhow!(
            "credential profiles support api_key|bearer_token|oauth|session_token"
        )),
    }
}

pub(crate) fn config_uses_credential_profiles(config: &HolonConfigFile) -> bool {
    config
        .providers
        .values()
        .any(|provider| provider.auth.source == CredentialSource::AuthProfile)
        || config
            .web
            .providers
            .values()
            .any(|p| p.credential_profile.is_some())
        || config
            .agent_templates
            .remote_sources
            .values()
            .any(|source| source.credential_profile.is_some())
        || config
            .runtime
            .observability
            .otlp
            .credential_profile
            .is_some()
}
