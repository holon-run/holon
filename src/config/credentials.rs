use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialStoreFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, CredentialProfileFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialProfileFile {
    pub kind: CredentialKind,
    pub material: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialProfileStatus {
    pub profile: String,
    pub kind: String,
    pub configured: bool,
}

pub fn persisted_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join("config.json")
}

pub fn credential_store_path(home_dir: &Path) -> PathBuf {
    home_dir.join("credentials.json")
}

pub fn load_persisted_config_at(path: &Path) -> Result<HolonConfigFile> {
    if !path.exists() {
        return Ok(HolonConfigFile::default());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_persisted_config_at(path: &Path, config: &HolonConfigFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(config).context("failed to serialize config")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
}

pub fn load_credential_store_at(path: &Path) -> Result<CredentialStoreFile> {
    if !path.exists() {
        return Ok(CredentialStoreFile::default());
    }
    ensure_owner_only_file(path)?;
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_credential_store_at(path: &Path, store: &CredentialStoreFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(store).context("failed to serialize credential store")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
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

pub(crate) fn ensure_owner_only_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata =
            fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(anyhow!(
                "credential store {} must be owner-only; found mode {:o}. Fix it with: chmod 600 {}",
                path.display(),
                mode,
                path.display()
            ));
        }
    }
    Ok(())
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
}
