use anyhow::Result;
use tempfile::TempDir;

use super::RuntimeDb;

pub struct TempRuntimeDb {
    pub db: RuntimeDb,
    _temp_dir: TempDir,
}

impl TempRuntimeDb {
    pub fn new() -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp_dir.path().join("state/runtime.sqlite"),
            temp_dir.path().join("state/runtime.lock"),
        )?;
        Ok(Self {
            db,
            _temp_dir: temp_dir,
        })
    }
}
