use std::{io, path::Path};

use super::{DirectoryEntries, PackageIo};

pub(super) struct TestIo;

impl PackageIo for TestIo {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read_dir(
        &self,
        path: &Path,
        visit: &mut dyn FnMut(&mut DirectoryEntries<'_>),
    ) -> io::Result<()> {
        visit(&mut std::fs::read_dir(path)?);
        Ok(())
    }

    fn charge_owned_metadata(&self, _: u64, _: usize) -> io::Result<()> {
        Ok(())
    }

    fn check_active_budget(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn with_template_parse_budget(&self, _: &str, parse: &mut dyn FnMut()) -> io::Result<()> {
        parse();
        Ok(())
    }
}
