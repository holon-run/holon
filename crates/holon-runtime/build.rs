#[path = "../../build.rs"]
#[allow(dead_code)]
mod application_build;

fn main() {
    println!("cargo:rerun-if-changed=../../build.rs");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    let manifest: toml::Table =
        toml::from_str(include_str!("../../Cargo.toml")).expect("valid application Cargo.toml");
    let version = manifest["package"]["version"]
        .as_str()
        .expect("application package version");
    let crate_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = crate_dir
        .join("../..")
        .canonicalize()
        .expect("repository directory");
    println!("cargo:rustc-env=HOLON_REPO_ROOT={}", repo.display());
    println!("cargo:rustc-env=HOLON_PRODUCT_VERSION={version}");
    application_build::configure_build(version);
}
