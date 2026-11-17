use holon_agent_templates::{DirectoryEntries, PackageIo};
use std::{io, path::Path};

use crate::diagnostics::{io_probe, resource_io};

pub(crate) struct RuntimePackageIo;

impl PackageIo for RuntimePackageIo {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        io_probe::read(path)
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        io_probe::read_to_string(path)
    }

    fn read_dir(
        &self,
        path: &Path,
        visit: &mut dyn FnMut(&mut DirectoryEntries<'_>),
    ) -> io::Result<()> {
        visit(&mut io_probe::read_dir(path)?);
        Ok(())
    }

    fn charge_owned_metadata(&self, bytes: u64, records: usize) -> io::Result<()> {
        resource_io::charge_owned_metadata(bytes, records)
    }

    fn check_active_budget(&self) -> anyhow::Result<()> {
        resource_io::check_active_budget()
    }

    fn with_template_parse_budget(&self, text: &str, parse: &mut dyn FnMut()) -> io::Result<()> {
        let _budget = resource_io::template_parse(text)?;
        parse();
        Ok(())
    }
}
