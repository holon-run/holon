use super::*;

pub fn persisted_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join("config.json")
}

pub fn load_persisted_config_at(path: &Path) -> Result<HolonConfigFile> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(HolonConfigFile::default())
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Unconditionally replace the config. Read/modify/write callers must instead
/// hold one PersistedConfigTransaction from load through save.
pub fn save_persisted_config_at(path: &Path, config: &HolonConfigFile) -> Result<()> {
    PersistedConfigTransaction::begin(path)?.save(config)
}

pub struct PersistedConfigTransaction {
    path: PathBuf,
    _lock: fs::File,
}

impl PersistedConfigTransaction {
    pub fn begin(path: &Path) -> Result<Self> {
        let parent = config_parent(path);
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        // Never unlink this sidecar: saving replaces config.json's inode.
        let lock_path = path.with_extension("json.lock");
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&lock_path)
            .with_context(|| format!("failed to open {}", lock_path.display()))?;
        fs2::FileExt::lock_exclusive(&lock)
            .with_context(|| format!("failed to lock {}", lock_path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            _lock: lock,
        })
    }

    pub fn load(&self) -> Result<HolonConfigFile> {
        load_persisted_config_at(&self.path)
    }

    pub fn save(&self, config: &HolonConfigFile) -> Result<()> {
        let content = serde_json::to_vec_pretty(config).context("failed to serialize config")?;
        let parent = config_parent(&self.path);
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&content)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|error| {
            anyhow!("failed to replace {}: {}", self.path.display(), error.error)
        })?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

fn config_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests;
