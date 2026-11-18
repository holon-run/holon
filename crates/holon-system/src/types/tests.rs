use super::*;

#[test]
fn execution_backend_deserializes_legacy_local_value() {
    let backend: ExecutionBackendKind = serde_json::from_str("\"local\"").unwrap();
    assert_eq!(backend, ExecutionBackendKind::HostLocal);
}

#[test]
fn host_local_policy_snapshot_reports_capability_boundary() {
    let profile = ExecutionProfile::default();
    let snapshot = profile.policy_snapshot();

    assert_eq!(snapshot.backend, ExecutionBackendKind::HostLocal);
    assert!(snapshot.process_execution_exposed);
    assert!(snapshot.managed_worktree_supported);
    assert_eq!(
        snapshot.resource_authority.workspace_projection,
        ExecutionGuaranteeLevel::HardEnforced
    );
    assert_eq!(
        snapshot.resource_authority.process_execution,
        ExecutionGuaranteeLevel::RuntimeShaped
    );
    assert_eq!(
        snapshot.process_execution.cwd_rooting,
        ExecutionGuaranteeLevel::RuntimeShaped
    );
    assert_eq!(
        snapshot.process_execution.path_confinement,
        ExecutionGuaranteeLevel::NotEnforced
    );
    assert_eq!(
        snapshot.process_execution.network_confinement,
        ExecutionGuaranteeLevel::NotEnforced
    );
}

#[test]
fn execution_snapshot_derives_missing_policy_from_profile() {
    let snapshot: ExecutionSnapshot = serde_json::from_value(serde_json::json!({
        "profile": {
            "backend": "host_local",
            "process_execution_exposed": false,
            "allow_background_tasks": true,
            "supports_managed_worktrees": false
        },
        "workspace_anchor": "/workspace",
        "execution_root": "/workspace",
        "cwd": "/workspace"
    }))
    .unwrap();

    assert!(!snapshot.policy.process_execution_exposed);
    assert!(!snapshot.policy.managed_worktree_supported);
    assert_eq!(snapshot.policy.backend, ExecutionBackendKind::HostLocal);
}
