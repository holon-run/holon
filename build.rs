use std::process::Command;

#[path = "build_support/web_dist.rs"]
mod web_dist;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
    web_dist::emit_rerun_directives();

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".into());
    let web_identity = web_dist::validate_embedded_web_dist()
        .unwrap_or_else(|error| panic!("{error}\nRun `make web` and retry the Rust build."));

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

    println!("cargo:rustc-env=HOLON_VERSION={}", version);
    println!(
        "cargo:rustc-env=HOLON_WEB_SOURCE_HASH={}",
        web_identity.source_hash
    );
}
