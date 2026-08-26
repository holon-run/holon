use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const WEB_ROOT: &str = "web-gui/app";
const WEB_DIST: &str = "web-gui/app/dist";
const MANIFEST_NAME: &str = "holon-web-build.json";
const SOURCE_FILES: &[&str] = &[
    "index.html",
    "package-lock.json",
    "package.json",
    "tsconfig.json",
    "tsconfig.node.json",
    "vite.config.ts",
];

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WebBuildIdentity {
    pub(crate) source_hash: String,
}

#[allow(dead_code)]
pub(crate) fn emit_rerun_directives() {
    println!("cargo:rerun-if-changed={WEB_ROOT}/src");
    for path in SOURCE_FILES {
        println!("cargo:rerun-if-changed={WEB_ROOT}/{path}");
    }
    println!("cargo:rerun-if-changed={WEB_DIST}/{MANIFEST_NAME}");
}

#[allow(dead_code)]
pub(crate) fn validate_embedded_web_dist() -> Result<WebBuildIdentity, String> {
    validate_web_dist(Path::new(WEB_ROOT), Path::new(WEB_DIST))
}

pub(crate) fn validate_web_dist(
    web_root: &Path,
    dist_dir: &Path,
) -> Result<WebBuildIdentity, String> {
    if !dist_dir.is_dir() {
        return Err(format!(
            "embedded Web dist is missing: {}",
            dist_dir.display()
        ));
    }
    let index_path = dist_dir.join("index.html");
    if !index_path.is_file() {
        return Err(format!(
            "embedded Web dist is incomplete: {} is missing",
            index_path.display()
        ));
    }

    let manifest_path = dist_dir.join(MANIFEST_NAME);
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        format!(
            "embedded Web dist identity is missing or unreadable at {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        format!(
            "embedded Web dist identity is invalid at {}: {error}",
            manifest_path.display()
        )
    })?;
    if manifest
        .get("schema_version")
        .and_then(|value| value.as_u64())
        != Some(1)
    {
        return Err(format!(
            "embedded Web dist identity at {} has an unsupported schema",
            manifest_path.display()
        ));
    }
    let built_hash = manifest
        .get("source_hash")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            format!(
                "embedded Web dist identity at {} has no source_hash",
                manifest_path.display()
            )
        })?;
    let current_hash = compute_web_source_hash(web_root)?;
    if built_hash != current_hash {
        return Err(format!(
            "embedded Web dist is stale: manifest has {built_hash}, current Web sources have {current_hash}"
        ));
    }

    Ok(WebBuildIdentity {
        source_hash: current_hash,
    })
}

pub(crate) fn compute_web_source_hash(web_root: &Path) -> Result<String, String> {
    let mut paths = Vec::new();
    collect_files(&web_root.join("src"), &mut paths)?;
    for relative in SOURCE_FILES {
        let path = web_root.join(relative);
        if !path.is_file() {
            return Err(format!("Web source input is missing: {}", path.display()));
        }
        paths.push(path);
    }
    paths.sort_by(|left, right| relative_path(web_root, left).cmp(&relative_path(web_root, right)));

    let mut hasher = Sha256::new();
    for path in paths {
        let relative = relative_path(web_root, &path);
        let bytes = fs::read(&path)
            .map_err(|error| format!("failed to read Web source {}: {error}", path.display()))?;
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn collect_files(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to read Web source directory {}: {error}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read an entry in Web source directory {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_files(&path, paths)?;
        } else if file_type.is_file() {
            paths.push(path);
        }
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("Web source path must be inside Web root")
        .to_string_lossy()
        .replace('\\', "/")
}
