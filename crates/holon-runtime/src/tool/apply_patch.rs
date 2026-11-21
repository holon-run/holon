//! Application-owned resource scope and file read hooks for patch execution.

use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use anyhow::Result;
pub use holon_domain::ApplyPatchSurface;
use holon_tools::patch::{ApplyPatchOutcome, PatchHost};
use std::path::{Path, PathBuf};

const PATCH_HOST: PatchHost = PatchHost {
    read_text: |path| {
        resource_io::admit_path(path)?;
        Ok(resource_io::read_to_string(path).unwrap_or_else(|| std::fs::read_to_string(path)))
    },
    error_kind: |error| crate::tool::tool_error_from_anyhow(error).kind,
};

/// Owned by the original blocking mutation kernel; its TLS never crosses await.
#[cfg(test)]
pub(crate) fn apply_patch_with_material(
    workspace_root: &Path,
    input: &str,
    surface: ApplyPatchSurface,
    material: &ResourceReadMaterial,
) -> Result<ApplyPatchOutcome> {
    apply_patch_with_material_and_writer(workspace_root, input, surface, material, |_| Ok(()))
}

pub(crate) fn apply_patch_with_material_and_writer<W>(
    workspace_root: &Path,
    input: &str,
    surface: ApplyPatchSurface,
    material: &ResourceReadMaterial,
    begin_write: impl FnOnce(Vec<PathBuf>) -> Result<W>,
) -> Result<ApplyPatchOutcome> {
    let _io = material.enter();
    let result =
        holon_tools::patch::apply_patch(workspace_root, input, surface, &PATCH_HOST, begin_write);
    material.finish()?;
    result
}

#[cfg(test)]
pub(crate) async fn apply_patch(
    workspace_root: &Path,
    input: &str,
    surface: ApplyPatchSurface,
) -> Result<ApplyPatchOutcome> {
    holon_tools::patch::apply_patch(workspace_root, input, surface, &PATCH_HOST, |_| Ok(()))
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod resource_runtime_tests {
    use super::*;
    use crate::runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    };

    fn material(bytes: u64) -> ResourceReadMaterial {
        ResourceReadMaterial::with_test_limits(
            &InputEvidenceBudgetLedger::new(Default::default()).unwrap(),
            &InputEvidenceScope {
                agent_id: "physical-patch".into(),
                identity_incarnation: 1,
            },
            bytes,
            65_536,
        )
        .unwrap()
    }
    const PATCH: &str = "--- a/file.txt\n+++ b/file.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";

    fn assert_resource_exhausted(error: &anyhow::Error) {
        let descriptor = crate::runtime_error::describe_runtime_error(error);
        assert_eq!(descriptor.code, "context_resource_exhausted");
        assert!(descriptor.retryable);
    }

    #[test]
    fn actual_patch_short_input_cannot_read_large_old_file_or_delete_earlier_target() -> Result<()>
    {
        let home = tempfile::tempdir()?;
        let original = "old\n".repeat(256 * 1024);
        std::fs::write(home.path().join("file.txt"), &original)?;
        std::fs::write(home.path().join("delete.txt"), "retain\n")?;
        let patch = format!("--- a/delete.txt\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-retain\n{PATCH}");
        let error = apply_patch_with_material(
            home.path(),
            &patch,
            ApplyPatchSurface::UnifiedDiffJson,
            &material(512 * 1024),
        )
        .unwrap_err();
        assert_resource_exhausted(&error);
        assert_eq!(
            std::fs::read_to_string(home.path().join("file.txt"))?,
            original
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join("delete.txt"))?,
            "retain\n"
        );
        Ok(())
    }

    #[test]
    fn actual_patch_read_growth_is_charged_before_buffer_growth_and_never_written() -> Result<()> {
        use std::io::Write;
        let home = tempfile::tempdir()?;
        let path = home.path().join("file.txt");
        std::fs::write(&path, "old\n")?;
        let phase = material(512 * 1024);
        let growing = path.clone();
        phase.after_file_metadata(move || {
            std::fs::OpenOptions::new()
                .append(true)
                .open(growing)
                .unwrap()
                .write_all(&vec![b'x'; 512 * 1024])
                .unwrap();
        });
        let error = apply_patch_with_material(
            home.path(),
            PATCH,
            ApplyPatchSurface::UnifiedDiffJson,
            &phase,
        )
        .unwrap_err();
        assert_resource_exhausted(&error);
        let content = std::fs::read_to_string(path)?;
        assert!(content.starts_with("old\n"));
        assert_eq!(content.len(), 512 * 1024 + 4);
        Ok(())
    }

    #[test]
    fn actual_patch_copy_and_output_boundary_fails_before_any_write() -> Result<()> {
        let home = tempfile::tempdir()?;
        let path = home.path().join("file.txt");
        std::fs::write(&path, "old\n")?;
        let phase = material(4 * 1024 * 1024);
        let result = apply_patch_with_material(
            home.path(),
            PATCH,
            ApplyPatchSurface::UnifiedDiffJson,
            &phase,
        )?;
        assert_eq!(result.changed_files.len(), 1);
        assert_eq!(std::fs::read_to_string(&path)?, "new\n");
        let usage = phase.finish()?;
        drop(phase);
        std::fs::write(&path, "old\n")?;
        let exact = material(usage.0 - 1);
        let error = apply_patch_with_material(
            home.path(),
            PATCH,
            ApplyPatchSurface::UnifiedDiffJson,
            &exact,
        )
        .unwrap_err();
        assert_resource_exhausted(&error);
        assert_eq!(std::fs::read_to_string(&path)?, "old\n");
        Ok(())
    }
}
