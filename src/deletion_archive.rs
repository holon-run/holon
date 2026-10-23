//! Explicit, sealed deletion exports. These files are never runtime inputs.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ArchiveBinding {
    pub agent_id: String,
    pub incarnation: u64,
    pub deletion_id: String,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    binding: ArchiveBinding,
    history_sha256: String,
    home_included: bool,
}

pub(super) fn write_archive(
    destination: &Path,
    home: &Path,
    binding: ArchiveBinding,
    history: &serde_json::Value,
) -> Result<()> {
    if destination.try_exists()? {
        return validate_archive(destination, &binding);
    }
    let parent = destination
        .parent()
        .context("archive destination has no parent")?;
    std::fs::create_dir_all(parent)?;
    anyhow::ensure!(
        !std::fs::symlink_metadata(parent)?.file_type().is_symlink(),
        "archive directory must not be a symlink"
    );
    let home_metadata = match std::fs::symlink_metadata(home) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "archive source must be a real AgentHome directory"
            );
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let history = serde_json::to_vec(history)?;
    let manifest = Manifest {
        version: 1,
        binding: binding.clone(),
        history_sha256: format!("{:x}", Sha256::digest(&history)),
        home_included: home_metadata.is_some(),
    };
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let encoder = GzEncoder::new(temporary.reopen()?, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    append_bytes(
        &mut builder,
        "manifest.json",
        &serde_json::to_vec(&manifest)?,
    )?;
    append_bytes(&mut builder, "runtime-history.json", &history)?;
    if home_metadata.is_some() {
        builder.append_dir_all("agent-home", home)?;
    }
    builder.into_inner()?.finish()?.sync_all()?;
    validate_archive(temporary.path(), &binding)?;
    match temporary.persist_noclobber(destination) {
        Ok(file) => file.sync_all()?,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_archive(destination, &binding)?;
        }
        Err(error) => return Err(error.error.into()),
    }
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn append_bytes<W: Write>(builder: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o600);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)?;
    Ok(())
}

fn validate_archive(path: &Path, binding: &ArchiveBinding) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "archive must be a regular file"
    );
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(path)?));
    let mut manifest = None;
    let mut history_hash = None;
    let mut saw_home = false;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        anyhow::ensure!(
            name.components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "unsafe archive entry"
        );
        if name == Path::new("manifest.json") {
            anyhow::ensure!(
                manifest.is_none() && entry.header().entry_type().is_file(),
                "duplicate or invalid archive manifest"
            );
            let mut bytes = Vec::new();
            entry.by_ref().take(64 * 1024 + 1).read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= 64 * 1024, "archive manifest too large");
            manifest = Some(serde_json::from_slice::<Manifest>(&bytes)?);
        } else if name == Path::new("runtime-history.json") {
            anyhow::ensure!(
                history_hash.is_none() && entry.header().entry_type().is_file(),
                "duplicate or invalid archive history"
            );
            let mut hash = Sha256::new();
            std::io::copy(&mut entry, &mut HashWriter(&mut hash))?;
            history_hash = Some(format!("{:x}", hash.finalize()));
        } else {
            anyhow::ensure!(name.starts_with("agent-home"), "unknown archive entry");
            let kind = entry.header().entry_type();
            anyhow::ensure!(
                kind.is_file() || kind.is_dir() || kind.is_symlink(),
                "unsupported archive entry type"
            );
            saw_home = true;
            std::io::copy(&mut entry, &mut std::io::sink())?;
        }
    }
    // Tar's end marker precedes the gzip footer: drain it to verify CRC and
    // detect a truncated export before allowing destructive cleanup.
    std::io::copy(&mut archive.into_inner(), &mut std::io::sink())?;
    let manifest = manifest.context("archive manifest missing")?;
    anyhow::ensure!(
        manifest.version == 1
            && manifest.binding == *binding
            && history_hash.as_deref() == Some(manifest.history_sha256.as_str())
            && manifest.home_included == saw_home,
        "archive seal or identity mismatch"
    );
    Ok(())
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> ArchiveBinding {
        ArchiveBinding {
            agent_id: "archive-agent".into(),
            incarnation: 1,
            deletion_id: "delete-test".into(),
        }
    }

    #[test]
    fn explicit_archive_is_sealed_idempotent_and_generation_bound() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        std::fs::create_dir(&home)?;
        std::fs::write(home.join("memory.md"), "private memory")?;
        let path = dir.path().join("archives/export.tar.gz");
        write_archive(
            &path,
            &home,
            binding(),
            &serde_json::json!({"messages": [1]}),
        )?;
        let original = std::fs::read(&path)?;
        write_archive(
            &path,
            &home,
            binding(),
            &serde_json::json!({"messages": [2]}),
        )?;
        assert_eq!(std::fs::read(&path)?, original);
        let mut replacement = binding();
        replacement.incarnation = 2;
        assert!(write_archive(&path, &home, replacement, &serde_json::json!({})).is_err());
        Ok(())
    }

    #[test]
    fn corrupt_archive_does_not_get_overwritten_or_remove_source() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        std::fs::create_dir(&home)?;
        std::fs::write(home.join("memory.md"), "keep me")?;
        let path = dir.path().join("export.tar.gz");
        write_archive(&path, &home, binding(), &serde_json::json!({}))?;
        let mut bytes = std::fs::read(&path)?;
        bytes.truncate(bytes.len() - 6);
        std::fs::write(&path, &bytes)?;
        assert!(write_archive(&path, &home, binding(), &serde_json::json!({})).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
        assert_eq!(std::fs::read_to_string(home.join("memory.md"))?, "keep me");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn archive_does_not_follow_home_symlinks() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        std::fs::create_dir(&home)?;
        std::os::unix::fs::symlink("/definitely-not-a-readable-source", home.join("link"))?;
        let path = dir.path().join("export.tar.gz");
        write_archive(&path, &home, binding(), &serde_json::json!({}))?;
        validate_archive(&path, &binding())?;
        Ok(())
    }
}
