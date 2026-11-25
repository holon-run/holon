use std::collections::BTreeMap;

use holon_acp::antigravity::{self, ADC_ENV};
use holon_acp::harness::{LaunchRequest, SandboxMode};

#[test]
fn adc_credentials_are_forwarded_and_other_env_is_dropped() {
    let spec = antigravity::profile();
    let mut source = BTreeMap::new();
    for name in ADC_ENV {
        source.insert((*name).to_string(), "value".to_string());
    }
    source.insert("GEMINI_API_KEY".to_string(), "key".to_string());
    source.insert("AWS_SECRET_ACCESS_KEY".to_string(), "leak".to_string());

    let filtered = spec.filter_env(&source);

    for name in ADC_ENV {
        assert!(
            filtered.contains_key(*name),
            "ADC variable {name} must be forwarded to the harness"
        );
    }
    assert!(filtered.contains_key("GEMINI_API_KEY"));
    assert!(!filtered.contains_key("AWS_SECRET_ACCESS_KEY"));
}

#[test]
fn launch_command_carries_add_dir_sandbox_and_never_skips_permissions() {
    let spec = antigravity::profile();
    let request = LaunchRequest {
        workspace_dir: "/tmp/worktree".to_string(),
        additional_directories: vec!["/tmp/attached".to_string()],
        sandbox: SandboxMode::ReadOnly,
        agent: None,
        log_file: None,
        disable_telemetry: true,
    };

    let command = spec.build_launch(&request);

    assert_eq!(command.program, "agy");
    assert_eq!(command.args.first().map(String::as_str), Some("acp"));
    assert!(command
        .args
        .windows(2)
        .any(|pair| pair[0] == "--add-dir" && pair[1] == "/tmp/attached"));
    assert!(command.args.iter().any(|arg| arg == "--sandbox"));
    assert!(command.args.iter().any(|arg| arg == "--disable-telemetry"));
    assert!(
        !command
            .args
            .iter()
            .any(|arg| arg.contains("dangerously-skip-permissions")),
        "ACP delegation must never pass --dangerously-skip-permissions by default"
    );
}
