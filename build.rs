use std::process::Command;

fn main() {
    configure_lib_test_shard();
    configure_git_tracking();

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".into());

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let version = match sha {
        Some(ref s) if !s.is_empty() => {
            let dirty = Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            if dirty {
                format!("{} ({}-dirty)", pkg_version, s)
            } else {
                format!("{} ({})", pkg_version, s)
            }
        }
        _ => pkg_version.clone(),
    };

    let version = if std::env::var_os("CARGO_FEATURE_RECOVERY_ACCEPTANCE").is_some() {
        format!("{version} [recovery-acceptance-test]")
    } else {
        version
    };
    println!("cargo:rustc-env=HOLON_VERSION={}", version);
}

fn configure_git_tracking() {
    println!("cargo:rerun-if-changed=build.rs");
    if let Some(path) = git_path("HEAD").filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    // Follow only this checkout's branch, not unrelated shared remote refs.
    let reference = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    if let Some(reference) = reference {
        if let Some(path) = git_path(reference.trim()) {
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            } else {
                if let Some(packed) = git_path("packed-refs").filter(|path| path.exists()) {
                    println!("cargo:rerun-if-changed={}", packed.display());
                }
                // A commit can create a loose ref without changing packed-refs.
                if let Some(parent) = path.ancestors().skip(1).find(|path| path.is_dir()) {
                    println!("cargo:rerun-if-changed={}", parent.display());
                }
            }
        }
    }
}

fn git_path(name: &str) -> Option<std::path::PathBuf> {
    Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| std::path::PathBuf::from(path.trim()))
}

fn configure_lib_test_shard() {
    println!("cargo:rerun-if-env-changed=HOLON_LIB_TEST_SHARD");
    println!("cargo:rustc-check-cfg=cfg(holon_lib_test_shard, values(\"a\", \"b\", \"c\", \"d\"))");

    // Keep dependencies and production code identical between test shards.
    // No selector means the normal, complete local unit-test suite.
    let selected = std::env::var("HOLON_LIB_TEST_SHARD").unwrap_or_else(|_| "all".into());
    let shards: &[&str] = match selected.as_str() {
        "all" => &["a", "b", "c", "d"],
        "a" => &["a"],
        "b" => &["b"],
        "c" => &["c"],
        "d" => &["d"],
        _ => panic!("HOLON_LIB_TEST_SHARD must be all, a, b, c, or d"),
    };
    for shard in shards {
        println!("cargo:rustc-cfg=holon_lib_test_shard=\"{shard}\"");
    }
}
