use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use super::{
    DirEntry, EditOptions, EffectiveExecution, FileContent, FileRead, FileStat, RemoveOptions,
    WriteOptions,
};

#[async_trait]
pub trait FileHost: Send + Sync {
    async fn read(&self, execution: &EffectiveExecution, path: &str) -> Result<FileRead>;
    async fn write(
        &self,
        execution: &EffectiveExecution,
        path: &str,
        content: FileContent,
        opts: WriteOptions,
    ) -> Result<()>;
    async fn edit(
        &self,
        execution: &EffectiveExecution,
        path: &str,
        old_string: &str,
        new_string: &str,
        opts: EditOptions,
    ) -> Result<()>;
    async fn list(
        &self,
        execution: &EffectiveExecution,
        path: Option<&str>,
    ) -> Result<Vec<DirEntry>>;
    async fn create_dir_all(&self, execution: &EffectiveExecution, path: &Path) -> Result<()>;
    async fn remove(
        &self,
        execution: &EffectiveExecution,
        path: &str,
        opts: RemoveOptions,
    ) -> Result<()>;
    async fn rename(&self, execution: &EffectiveExecution, from: &str, to: &str) -> Result<()>;
    async fn stat(&self, execution: &EffectiveExecution, path: &str) -> Result<FileStat>;
}
