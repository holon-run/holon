use std::{collections::BTreeMap, fs, io::Write, path::Path};

#[cfg(unix)]
use anyhow::anyhow;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::CredentialKind;

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

fn ensure_owner_only_file(path: &Path) -> Result<()> {
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
    let _ = path;
    Ok(())
}
