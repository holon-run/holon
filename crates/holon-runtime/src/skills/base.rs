use crate::{
    agent_package_io::RuntimePackageIo,
    types::{
        ActiveSkillRecord, SkillCatalogEntry, SkillInstallMode, SkillRootRegistration, SkillScope,
        SkillsRuntimeView,
    },
};
use anyhow::Result;
use holon_agent_templates::skills as package;
use std::path::Path;

#[cfg(test)]
pub(crate) use package::skill_root_id_for_scope;

pub use package::{
    disable_agent_skill, effective_skill_root_registrations, find_skill_by_entrypoint,
    find_skill_by_script_path, skill_root_registration, uninstall_skill, GithubSkillInstallRequest,
    InstalledSkillView, RemoteSkillInstallFailed, RemoteSkillInstallTimedOut, SkillInstallConflict,
    SkillLibraryCheckResult, SkillLibraryReconcileResult, SkillLibraryUpdateProgress,
    SkillLibraryUpdateResult, SkillLibraryUpdateState, SkillLibraryUpdateStatus, SkillVisibility,
    REMOTE_SKILL_INSTALL_TIMEOUT_SECONDS,
};
pub use package::{
    existing_skill_roots, user_global_skill_root_candidates, RemoteSkillArchiveFetchFailed,
    COMPAT_SKILL_ROOT_SUFFIXES,
};
pub(crate) use package::{skill_root_id, skill_wins_catalog_selection, SKILL_ROOT_SUFFIXES};

pub fn load_skills_runtime_view(
    visibility: SkillVisibility,
    user_home: Option<&Path>,
    agent_home: &Path,
    workspace_anchor: Option<&Path>,
    active_skills: &[ActiveSkillRecord],
) -> Result<SkillsRuntimeView> {
    package::load_skills_runtime_view(
        &RuntimePackageIo,
        visibility,
        user_home,
        agent_home,
        workspace_anchor,
        active_skills,
    )
}

pub fn load_catalog_for_scope(scope: SkillScope, root: &Path) -> Result<Vec<SkillCatalogEntry>> {
    package::load_catalog_for_scope(&RuntimePackageIo, scope, root)
}

pub(crate) fn load_catalog_for_root(
    scope: SkillScope,
    root: &Path,
    root_id: String,
) -> Result<Vec<SkillCatalogEntry>> {
    package::load_catalog_for_root(&RuntimePackageIo, scope, root, root_id)
}

pub fn skills_runtime_view_from_catalog(
    catalog: Vec<SkillCatalogEntry>,
    roots: &[SkillRootRegistration],
    active_skills: &[ActiveSkillRecord],
) -> Result<SkillsRuntimeView> {
    package::skills_runtime_view_from_catalog(&RuntimePackageIo, catalog, roots, active_skills)
}

pub fn add_github_remote_skills_to_library(
    user_home: &Path,
    requests: &[GithubSkillInstallRequest],
) -> Result<Vec<String>> {
    package::add_github_remote_skills_to_library(&RuntimePackageIo, user_home, requests)
}

pub fn install_skill(agent_home: &Path, kind: &crate::types::SkillInstallKind) -> Result<String> {
    package::install_skill(&RuntimePackageIo, agent_home, kind)
}

pub fn install_skill_with_user_home(
    agent_home: &Path,
    user_home: Option<&Path>,
    kind: &crate::types::SkillInstallKind,
) -> Result<String> {
    package::install_skill_with_user_home(&RuntimePackageIo, agent_home, user_home, kind)
}

pub fn add_library_skill(
    user_home: &Path,
    kind: &crate::types::SkillInstallKind,
) -> Result<String> {
    package::add_library_skill(&RuntimePackageIo, user_home, kind)
}

pub fn enable_agent_skill(
    agent_home: &Path,
    user_home: Option<&Path>,
    name: &str,
    mode: &SkillInstallMode,
) -> Result<String> {
    package::enable_agent_skill(&RuntimePackageIo, agent_home, user_home, name, mode)
}

pub fn remove_library_skill(user_home: &Path, name: &str) -> Result<()> {
    package::remove_library_skill(&RuntimePackageIo, user_home, name)
}

pub fn check_library_skills(
    user_home: &Path,
    name_filter: Option<&str>,
) -> Result<SkillLibraryCheckResult> {
    package::check_library_skills(&RuntimePackageIo, user_home, name_filter)
}

pub fn reconcile_library_skills(
    user_home: &Path,
    name_filter: Option<&str>,
) -> Result<SkillLibraryReconcileResult> {
    package::reconcile_library_skills(&RuntimePackageIo, user_home, name_filter)
}

pub fn update_library_skills(
    user_home: &Path,
    name_filter: Option<&str>,
) -> Result<SkillLibraryUpdateResult> {
    package::update_library_skills(&RuntimePackageIo, user_home, name_filter)
}

pub fn update_library_skills_with_progress(
    user_home: &Path,
    name_filter: Option<&str>,
    mut on_progress: impl FnMut(SkillLibraryUpdateProgress),
) -> Result<SkillLibraryUpdateResult> {
    package::update_library_skills_with_progress(
        &RuntimePackageIo,
        user_home,
        name_filter,
        &mut on_progress,
    )
}

pub fn list_installed_skills(agent_home: &Path) -> Result<Vec<InstalledSkillView>> {
    package::list_installed_skills(&RuntimePackageIo, agent_home)
}

#[cfg(all(test, holon_lib_test_shard = "c"))]
mod tests {
    use super::*;
    use crate::types::{SkillActivationSource, SkillActivationState, SkillRootSourceKind};
    use std::time::Duration;
    use tempfile::tempdir;

    fn collect_discovered_roots_from_registrations(
        roots: &[SkillRootRegistration],
    ) -> Result<Vec<crate::types::SkillRootView>> {
        package::test_support::collect_discovered_roots_from_registrations(&RuntimePackageIo, roots)
    }

    fn retain_active_skills_for_catalog(
        catalog: &[SkillCatalogEntry],
        active: &[ActiveSkillRecord],
    ) -> Result<Vec<ActiveSkillRecord>> {
        package::test_support::retain_active_skills_for_catalog(&RuntimePackageIo, catalog, active)
    }

    #[test]
    fn archive_fetch_classification_does_not_guess_from_io_or_error_text() {
        for error in [
            anyhow::anyhow!("connection timed out"),
            anyhow::anyhow!("GitHub archive exceeds the compressed size limit"),
            anyhow::anyhow!("selected archive directory contains unsupported entries"),
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::TimedOut)),
        ] {
            let context = RemoteSkillArchiveFetchFailed::new("owner/repo", &error);
            assert!(!context.retryable());
            let wrapped = error.context(context);
            assert!(wrapped
                .downcast_ref::<RemoteSkillArchiveFetchFailed>()
                .is_some());
            assert!(!crate::runtime_error::describe_runtime_error(&wrapped).retryable);
        }
    }

    #[test]
    fn archive_fetch_typed_timeout_preserves_retryable_source() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(25))
            .build()
            .unwrap();
        // A listening socket with no response times out locally, without DNS or external I/O.
        let source = client.get(url).send().unwrap_err();
        assert!(source.is_timeout());
        let source = anyhow::Error::new(source);
        let context = RemoteSkillArchiveFetchFailed::new("owner/repo", &source);
        assert!(context.retryable());
        let error = source.context(context);
        assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(descriptor.code, "remote_skill_archive_fetch_failed");
        assert!(descriptor.retryable);
        assert!(descriptor.recovery_hint.unwrap().contains("network"));
    }

    #[test]
    fn catalog_runtime_view_admits_root_and_active_projections_before_owning() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let home = tempdir()?;
        let roots = vec![skill_root_registration(
            SkillRootSourceKind::AgentHome,
            Some("default".into()),
            home.path().to_path_buf(),
        )];
        let catalog = vec![SkillCatalogEntry {
            skill_id: "visible".into(),
            root_id: "root".into(),
            skill_dir: "demo".into(),
            name: "canonical".into(),
            description: "description".into(),
            path: home.path().join("SKILL.md"),
            scope: SkillScope::Agent,
        }];
        let active = vec![ActiveSkillRecord {
            skill_id: "visible".into(),
            name: "stale".repeat(8192),
            path: home.path().join("stale"),
            scope: SkillScope::Workspace,
            agent_id: "default".into(),
            activation_source: SkillActivationSource::Explicit,
            activation_state: SkillActivationState::SessionActive,
            activated_at_turn: 7,
        }];
        let project = |root_projection: bool| -> Result<()> {
            if root_projection {
                let output = collect_discovered_roots_from_registrations(&roots)?;
                assert_eq!(output.len(), 1);
                assert_eq!(output[0].path, roots[0].root_path);
            } else {
                let output = retain_active_skills_for_catalog(&catalog, &active)?;
                assert_eq!(output.len(), 1);
                assert_eq!(output[0].name, catalog[0].name);
                assert_eq!(output[0].path, catalog[0].path);
                assert_eq!(output[0].scope, SkillScope::Agent);
                assert_eq!(output[0].activated_at_turn, 7);
            }
            Ok(())
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        for root_projection in [true, false] {
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1 << 20, 8192)?;
            {
                let _active = phase.enter();
                project(root_projection)?;
            }
            let (bytes, records) = phase.finish()?;
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            for (byte_limit, record_limit) in [(bytes - 1, 8192), (1 << 20, records - 1)] {
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope,
                    byte_limit,
                    record_limit,
                )?;
                {
                    let _active = phase.enter();
                    let error = project(root_projection).unwrap_err();
                    assert!(format!("{error:#}").contains("ResourceExhausted"));
                    assert!(project(root_projection).is_err(), "rejection stays latched");
                }
                assert!(phase.finish().is_err());
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
        let view = skills_runtime_view_from_catalog(catalog, &roots, &active)?;
        assert!(std::sync::Arc::ptr_eq(
            &view.discoverable_skills,
            &view.attached_skills,
        ));
        let wire = serde_json::to_value(view)?;
        assert!(wire["discoverable_skills"].is_array());
        assert_eq!(wire["discoverable_skills"], wire["attached_skills"]);
        Ok(())
    }
}
