use super::validate_path_selector;

#[test]
fn path_selector_requires_an_existing_directory() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("file");
    std::fs::write(&file, "content").unwrap();
    let missing = temp.path().join("missing");

    assert!(validate_path_selector(temp.path().to_path_buf()).is_ok());
    assert!(validate_path_selector(file).is_err());
    assert!(validate_path_selector(missing).is_err());
}
