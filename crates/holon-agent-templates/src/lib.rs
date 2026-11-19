//! Agent template and skill discovery and installation.

use std::{fs::DirEntry, io, path::Path};

pub mod skills;
pub mod templates;

pub type DirectoryEntries<'a> = dyn Iterator<Item = io::Result<DirEntry>> + 'a;

/// Filesystem operations that the runtime measures and admits against its read budget.
pub trait PackageIo: Sync {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn read_to_string(&self, path: &Path) -> io::Result<String>;

    /// Call `visit` once after opening the directory, with lazy iteration.
    fn read_dir(
        &self,
        path: &Path,
        visit: &mut dyn FnMut(&mut DirectoryEntries<'_>),
    ) -> io::Result<()>;

    fn charge_owned_metadata(&self, bytes: u64, records: usize) -> io::Result<()>;
    fn check_active_budget(&self) -> anyhow::Result<()>;

    /// Call `parse` once after admission, holding the scratch allowance until it returns.
    fn with_template_parse_budget(&self, text: &str, parse: &mut dyn FnMut()) -> io::Result<()>;
}

fn read_dir<T>(
    io: &dyn PackageIo,
    path: &Path,
    visit: impl FnOnce(&mut DirectoryEntries<'_>) -> T,
) -> io::Result<T> {
    let mut visit = Some(visit);
    let mut result = None;
    io.read_dir(path, &mut |entries| {
        let visit = visit.take().expect("directory visitor called once");
        result = Some(visit(entries));
    })?;
    Ok(result.expect("directory visitor ran after a successful open"))
}

fn parse_template<T>(io: &dyn PackageIo, text: &str, parse: impl FnOnce() -> T) -> io::Result<T> {
    let mut parse = Some(parse);
    let mut result = None;
    io.with_template_parse_budget(text, &mut || {
        result = Some(parse.take().expect("template parser called once")());
    })?;
    Ok(result.expect("template parser ran after successful budget admission"))
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
use test_support::TestIo;
