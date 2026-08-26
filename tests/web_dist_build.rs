#[path = "../build_support/web_dist.rs"]
mod web_dist;

use std::fs;
use tempfile::TempDir;

#[test]
fn rejects_missing_web_dist() {
    let fixture = WebFixture::new();

    let error = web_dist::validate_web_dist(&fixture.web_root, &fixture.dist_dir).unwrap_err();

    assert!(error.contains("dist is missing"), "{error}");
}

#[test]
fn rejects_stale_web_dist() {
    let fixture = WebFixture::new();
    fixture.write_dist("sha256:stale");

    let error = web_dist::validate_web_dist(&fixture.web_root, &fixture.dist_dir).unwrap_err();

    assert!(error.contains("dist is stale"), "{error}");
}

#[test]
fn accepts_matching_web_dist() {
    let fixture = WebFixture::new();
    let source_hash = web_dist::compute_web_source_hash(&fixture.web_root).unwrap();
    fixture.write_dist(&source_hash);

    let identity = web_dist::validate_web_dist(&fixture.web_root, &fixture.dist_dir).unwrap();

    assert_eq!(identity.source_hash, source_hash);
}

struct WebFixture {
    _temp: TempDir,
    web_root: std::path::PathBuf,
    dist_dir: std::path::PathBuf,
}

impl WebFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let web_root = temp.path().join("app");
        let dist_dir = web_root.join("dist");
        fs::create_dir_all(web_root.join("src")).unwrap();
        fs::write(web_root.join("src/main.ts"), "export {};\n").unwrap();
        for source_file in [
            "index.html",
            "package-lock.json",
            "package.json",
            "tsconfig.json",
            "tsconfig.node.json",
            "vite.config.ts",
        ] {
            fs::write(web_root.join(source_file), source_file).unwrap();
        }
        Self {
            _temp: temp,
            web_root,
            dist_dir,
        }
    }

    fn write_dist(&self, source_hash: &str) {
        fs::create_dir_all(&self.dist_dir).unwrap();
        fs::write(self.dist_dir.join("index.html"), "<main>Holon</main>").unwrap();
        fs::write(
            self.dist_dir.join("holon-web-build.json"),
            format!(
                "{{\"schema_version\":1,\"source_hash\":\"{source_hash}\",\"web_version\":\"test\"}}\n"
            ),
        )
        .unwrap();
    }
}
