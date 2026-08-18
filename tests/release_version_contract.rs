use std::path::PathBuf;
use std::process::Command;

fn verifier() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-release-version.sh")
}

fn verify(version: &str, output: &str) -> bool {
    Command::new(verifier())
        .args([version, output])
        .status()
        .expect("run release version verifier")
        .success()
}

#[test]
fn accepts_version_output_with_commit_sha() {
    assert!(verify("0.32.0", "holon 0.32.0 (df561ad)"));
    assert!(verify(
        "0.32.0",
        "holon 0.32.0 (df561adf277c2bfb3db74f69468207a6ef8b9e62)"
    ));
}

#[test]
fn rejects_stale_or_malformed_version_output() {
    for output in [
        "holon 0.32.0",
        "holon 0.31.1 (df561ad)",
        "holon 0.32.0 (df561ad-dirty)",
        "holon 0.32.0 (DF561AD)",
        "holon 0.32.0 (123456)",
    ] {
        assert!(!verify("0.32.0", output), "{output}");
    }
}
