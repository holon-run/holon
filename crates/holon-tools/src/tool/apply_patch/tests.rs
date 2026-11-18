use super::*;
use tempfile::tempdir;

fn patch_tool_error(error: &anyhow::Error) -> ToolError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ToolError>())
        .cloned()
        .unwrap_or_else(|| ToolError::new("tool_execution_failed", error.to_string()))
}

const HOST: PatchHost = PatchHost {
    read_text: |path| Ok(std::fs::read_to_string(path)),
    error_kind: |error| patch_tool_error(error).kind,
};

fn apply_sync(path: &Path, input: &str, surface: ApplyPatchSurface) -> Result<ApplyPatchOutcome> {
    super::apply_patch(path, input, surface, &HOST, |_| Ok(()))
}

#[test]
fn apply_patch_surface_is_route_specific_for_deepseek_responses() {
    assert_eq!(
        ApplyPatchSurface::for_model_route_ref("deepseek@responses/deepseek-v4-pro"),
        ApplyPatchSurface::CodexDslFreeform
    );
    assert_eq!(
        ApplyPatchSurface::for_model_route_ref("deepseek@default/deepseek-v4-pro"),
        ApplyPatchSurface::UnifiedDiffJson
    );
    assert_eq!(
        ApplyPatchSurface::for_model_route_ref("deepseek/deepseek-v4-pro"),
        ApplyPatchSurface::UnifiedDiffJson
    );
}

async fn apply_patch(workspace_root: &Path, input: &str) -> Result<ApplyPatchOutcome> {
    apply_sync(workspace_root, input, ApplyPatchSurface::UnifiedDiffJson)
}

fn parse_patch(input: &str) -> Result<Vec<FilePatch>> {
    parse_unified_diff_patch(input)
}

#[tokio::test]
async fn apply_patch_updates_multiple_files_with_unified_diff() {
    let dir = tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    tokio::fs::write(&alpha, "before\nshared\n").await.unwrap();
    tokio::fs::write(&beta, "keep\nold\n").await.unwrap();

    let patch = r#"--- a/alpha.txt
+++ b/alpha.txt
@@ -1,2 +1,2 @@
-before
+after
 shared
--- a/beta.txt
+++ b/beta.txt
@@ -1,2 +1,2 @@
 keep
-old
+new
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&alpha).await.unwrap(),
        "after\nshared\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(&beta).await.unwrap(),
        "keep\nnew\n"
    );
    assert_eq!(outcome.changed_files.len(), 2);
    assert!(outcome.diagnostics.is_empty());
}

#[tokio::test]
async fn apply_patch_unified_diff_allows_codex_marker_as_file_content() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "before\n*** Begin Patch\nafter\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,3 +1,3 @@
 before
 *** Begin Patch
-after
+*** End Patch
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "before\n*** Begin Patch\n*** End Patch\n"
    );
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Modify);
    assert!(outcome.diagnostics.is_empty());
}

#[tokio::test]
async fn apply_patch_unified_diff_ignores_one_trailing_sentinel_at_surface_boundary() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1 +1 @@
-old
+new
*** End Patch
"#;

    assert!(parse_patch(patch).is_err());
    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == "apply_patch_trailing_sentinel_ignored")
        .expect("trailing sentinel diagnostic");
    assert!(diagnostic
        .message
        .contains("parser_mode=trailing_sentinel_compatibility"));
    assert!(diagnostic
        .message
        .contains("trailing_sentinel_ignored=true"));
    assert!(!diagnostic.message.contains(CODEX_END_PATCH_SENTINEL));
}

#[tokio::test]
async fn apply_patch_unified_diff_rejects_non_trailing_or_repeated_sentinel_without_writes() {
    let patches = [
        r#"--- a/sample.txt
+++ b/sample.txt
@@ -1 +1 @@
-old
+new
*** End Patch
--- a/other.txt
+++ b/other.txt
@@ -1 +1 @@
-before
+after
"#,
        r#"--- a/sample.txt
+++ b/sample.txt
@@ -1 +1 @@
-old
+new
*** End Patch
*** End Patch
"#,
        r#"--- a/sample.txt
+++ b/sample.txt
@@ -1 +1 @@
-old
+new
*** End Patch trailing garbage
"#,
    ];

    for patch in patches {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        tokio::fs::write(&file, "old\n").await.unwrap();

        let error = apply_patch(dir.path(), patch).await.unwrap_err();
        let tool_error = patch_tool_error(&error);
        assert_eq!(tool_error.kind, "invalid_patch_syntax");
        assert_eq!(
            tool_error.details.as_ref().unwrap()["rule"],
            "unexpected_patch_sentinel"
        );
        assert!(!tool_error.message.contains(CODEX_END_PATCH_SENTINEL));
        assert!(!tool_error
            .recovery_hint
            .as_deref()
            .unwrap_or_default()
            .contains(CODEX_END_PATCH_SENTINEL));
        assert!(!tool_error
            .details
            .as_ref()
            .unwrap()
            .to_string()
            .contains("trailing garbage"));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "old\n");
    }
}

#[tokio::test]
async fn apply_patch_unified_diff_reports_stripped_body_error_without_writes() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ invalid @@
-old
+new
*** End Patch
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "invalid_patch_syntax");
    assert_eq!(
        tool_error.details.as_ref().unwrap()["rule"],
        "invalid_hunk_header"
    );
    assert!(!tool_error.message.contains(CODEX_END_PATCH_SENTINEL));
    assert!(!tool_error
        .recovery_hint
        .as_deref()
        .unwrap_or_default()
        .contains(CODEX_END_PATCH_SENTINEL));
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "old\n");
}

#[tokio::test]
async fn apply_patch_unified_diff_accepts_bare_hunk_header() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "alpha\nbeta\ngamma\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@
 alpha
-beta
+delta
 gamma
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "alpha\ndelta\ngamma\n"
    );
    assert!(outcome.diagnostics.is_empty());
}

#[tokio::test]
async fn apply_patch_unified_diff_accepts_bare_hunk_header_with_description() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "alpha\nbeta\ngamma\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ tweak the middle line
 alpha
-beta
+delta
 gamma
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "alpha\ndelta\ngamma\n"
    );
    assert!(outcome.diagnostics.is_empty());
}

#[tokio::test]
async fn apply_patch_unified_diff_rejects_bare_pure_add_hunk() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "alpha\nbeta\ngamma\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@
+inserted at top
+inserted below top
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "invalid_patch_syntax");
    assert_eq!(
        tool_error.details.as_ref().unwrap()["rule"],
        "bare_hunk_insertion_position_required"
    );
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "alpha\nbeta\ngamma\n"
    );
}

#[test]
fn parse_unified_diff_bare_headers_derive_counts_and_split_hunks() {
    let patches = parse_patch(
        "--- a/sample.txt\n+++ b/sample.txt\n@@\n alpha\n-beta\n+delta\n@@ tail\n gamma\n",
    )
    .unwrap();
    assert_eq!(patches.len(), 1);
    let hunks = &patches[0].hunks;
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].old_start, 0);
    assert_eq!(hunks[0].old_count, 2);
    assert_eq!(hunks[0].new_start, 0);
    assert_eq!(hunks[0].new_count, 2);
    assert_eq!(hunks[1].old_start, 0);
    assert_eq!(hunks[1].old_count, 1);
    assert_eq!(hunks[1].new_count, 1);
    assert!(patches[0].diagnostics.is_empty());
}

#[test]
fn apply_patch_bare_hunk_unified_diff_uses_cross_format_fallback_on_dsl_surface() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    std::fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@
 alpha
-beta
+delta
 gamma
"#;

    let outcome = apply_sync(dir.path(), patch, ApplyPatchSurface::CodexDslFreeform).unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "alpha\ndelta\ngamma\n"
    );
    assert!(outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "apply_patch_compatibility_fallback"));
}

#[tokio::test]
async fn apply_patch_unified_diff_keeps_full_codex_wrapper_on_cross_format_fallback() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = r#"*** Begin Patch
*** Update File: sample.txt
-old
+new
*** End Patch
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    assert!(outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "apply_patch_compatibility_fallback"));
    assert!(!outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "apply_patch_trailing_sentinel_ignored"));
}

#[test]
fn parse_unified_diff_wrong_format_error_does_not_echo_competing_markers() {
    let malformed = "*** Begin Patch\nnot a complete patch\n";
    let error = parse_patch(malformed).unwrap_err();
    let tool_error = patch_tool_error(&error);

    assert_eq!(tool_error.kind, "invalid_patch_syntax");
    assert_eq!(
        tool_error.details.as_ref().unwrap()["rule"],
        "wrong_patch_format"
    );
    assert!(!tool_error.message.contains("*** Begin Patch"));
    assert!(!tool_error
        .recovery_hint
        .as_deref()
        .unwrap_or_default()
        .contains("*** Begin Patch"));
    assert!(!tool_error
        .details
        .as_ref()
        .unwrap()
        .to_string()
        .contains("*** Begin Patch"));
}

#[tokio::test]
async fn apply_patch_updates_file_with_codex_dsl() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "one\ntwo\nthree\n").await.unwrap();

    let patch = r#"*** Begin Patch
*** Update File: sample.txt
 one
-two
+TWO
 three
*** End Patch
"#;

    let outcome = apply_sync(dir.path(), patch, ApplyPatchSurface::CodexDslFreeform).unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "one\nTWO\nthree\n"
    );
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Modify);
    assert!(outcome.diagnostics.is_empty());
}

#[tokio::test]
async fn apply_patch_codex_dsl_add_delete_and_move_share_changed_shape() {
    let dir = tempdir().unwrap();
    let old = dir.path().join("old.txt");
    let doomed = dir.path().join("doomed.txt");
    tokio::fs::write(&old, "hello\n").await.unwrap();
    tokio::fs::write(&doomed, "bye\n").await.unwrap();

    let patch = r#"*** Begin Patch
*** Add File: created.txt
+hi
*** Delete File: doomed.txt
*** Update File: old.txt
*** Move to: new.txt
 hello
*** End Patch
"#;

    let outcome = apply_sync(dir.path(), patch, ApplyPatchSurface::CodexDslFreeform).unwrap();
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("created.txt"))
            .await
            .unwrap(),
        "hi\n"
    );
    assert!(!tokio::fs::try_exists(&doomed).await.unwrap());
    assert!(!tokio::fs::try_exists(&old).await.unwrap());
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("new.txt"))
            .await
            .unwrap(),
        "hello\n"
    );
    assert_eq!(outcome.changed_files.len(), 3);
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Add);
    assert_eq!(outcome.changed_files[1].action, ApplyPatchAction::Delete);
    assert_eq!(outcome.changed_files[2].action, ApplyPatchAction::Move);
}

#[tokio::test]
async fn apply_patch_codex_dsl_applies_delete_then_add_for_same_relative_path() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = r#"*** Begin Patch
*** Delete File: sample.txt
*** Add File: sample.txt
+new
*** End Patch
"#;

    let outcome = apply_sync(dir.path(), patch, ApplyPatchSurface::CodexDslFreeform).unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    assert_eq!(outcome.changed_files.len(), 2);
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Delete);
    assert_eq!(outcome.changed_files[1].action, ApplyPatchAction::Add);
    assert_eq!(outcome.changed_paths, vec![file.display().to_string()]);
}

#[tokio::test]
async fn apply_patch_codex_dsl_applies_delete_then_add_for_same_absolute_path() {
    let workspace = tempdir().unwrap();
    let external = tempdir().unwrap();
    let file = external.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();
    let path = file.display();
    let patch = format!(
        r#"*** Begin Patch
*** Delete File: {path}
*** Add File: {path}
+new
*** End Patch
"#
    );

    let outcome = apply_sync(
        workspace.path(),
        &patch,
        ApplyPatchSurface::CodexDslFreeform,
    )
    .unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    assert_eq!(outcome.changed_files.len(), 2);
    assert_eq!(outcome.changed_paths, vec![file.display().to_string()]);
}

#[tokio::test]
async fn apply_patch_compatibility_parses_non_active_known_format() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-old
+new
"#;

    let outcome = apply_sync(dir.path(), patch, ApplyPatchSurface::CodexDslFreeform).unwrap();
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == "apply_patch_compatibility_fallback")
        .expect("compatibility diagnostic");
    assert!(diagnostic.message.contains("expected_format=codex_dsl"));
    assert!(diagnostic
        .message
        .contains("compatibility_fallback_used=true"));
}

#[tokio::test]
async fn apply_patch_codex_dsl_accepts_end_of_file_marker() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = r#"*** Begin Patch
*** Update File: sample.txt
-old
+new
*** End of File
*** End Patch
"#;

    apply_sync(dir.path(), patch, ApplyPatchSurface::CodexDslFreeform).unwrap();
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new");
}

#[tokio::test]
async fn apply_patch_adds_and_deletes_files_with_dev_null() {
    let dir = tempdir().unwrap();
    let doomed = dir.path().join("doomed.txt");
    tokio::fs::write(&doomed, "bye\n").await.unwrap();

    let patch = r#"--- /dev/null
+++ b/created.txt
@@ -0,0 +1,1 @@
+hello
--- a/doomed.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("created.txt"))
            .await
            .unwrap(),
        "hello\n"
    );
    assert!(!tokio::fs::try_exists(&doomed).await.unwrap());
}

#[tokio::test]
async fn apply_patch_unified_diff_applies_delete_then_add_for_same_relative_path() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-old
--- /dev/null
+++ b/./sample.txt
@@ -0,0 +1,1 @@
+new
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    assert_eq!(outcome.changed_files.len(), 2);
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Delete);
    assert_eq!(outcome.changed_files[1].action, ApplyPatchAction::Add);
    assert_eq!(outcome.changed_paths, vec![file.display().to_string()]);
}

#[tokio::test]
async fn apply_patch_unified_diff_applies_delete_then_add_for_same_absolute_path() {
    let workspace = tempdir().unwrap();
    let external = tempdir().unwrap();
    let file = external.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();
    let path = file.display();
    let patch = format!(
        r#"--- {path}
+++ /dev/null
@@ -1,1 +0,0 @@
-old
--- /dev/null
+++ {path}
@@ -0,0 +1,1 @@
+new
"#
    );

    let outcome = apply_patch(workspace.path(), &patch).await.unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    assert_eq!(outcome.changed_files.len(), 2);
    assert_eq!(outcome.changed_paths, vec![file.display().to_string()]);
}

#[tokio::test]
async fn apply_patch_add_allows_existing_empty_file() {
    let dir = tempdir().unwrap();
    let existing_empty = dir.path().join("created.txt");
    tokio::fs::write(&existing_empty, "").await.unwrap();

    let patch = r#"--- /dev/null
+++ b/created.txt
@@ -0,0 +1,1 @@
+hello
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&existing_empty).await.unwrap(),
        "hello\n"
    );
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Add);
}

#[tokio::test]
async fn apply_patch_add_still_rejects_existing_non_empty_file() {
    let dir = tempdir().unwrap();
    let existing = dir.path().join("created.txt");
    tokio::fs::write(&existing, "existing\n").await.unwrap();

    let patch = r#"--- /dev/null
+++ b/created.txt
@@ -0,0 +1,1 @@
+hello
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "existing_file");
    assert_eq!(
        tokio::fs::read_to_string(&existing).await.unwrap(),
        "existing\n"
    );
}

#[test]
fn strip_diff_prefix_preserves_only_double_slash_absolute_paths() {
    assert_eq!(strip_diff_prefix("a/src/foo.rs"), "src/foo.rs");
    assert_eq!(strip_diff_prefix("b/src/foo.rs"), "src/foo.rs");
    assert_eq!(strip_diff_prefix("a//home/foo.rs"), "/home/foo.rs");
    assert_eq!(strip_diff_prefix("b//home/foo.rs"), "/home/foo.rs");
    assert_eq!(strip_diff_prefix("a/home/foo.rs"), "home/foo.rs");
    assert_eq!(strip_diff_prefix("b/home/foo.rs"), "home/foo.rs");
}

#[tokio::test]
async fn resolve_patch_path_hints_when_relative_path_is_actually_absolute() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();

    // Create a file at an absolute path that simulates the LLM mistake:
    // LLM writes "a/tmp/holon_hint_test.rs", strip_diff_prefix produces
    // "tmp/holon_hint_test.rs", which resolves to workspace/tmp/... (non-existent).
    // But /tmp/holon_hint_test.rs exists → hint error.
    let abs_file = std::path::PathBuf::from("/tmp/holon_hint_test.rs");
    tokio::fs::write(
        &abs_file, "test
",
    )
    .await
    .unwrap();

    let result = resolve_patch_path(dir.path(), "tmp/holon_hint_test.rs");
    assert!(
        result.is_err(),
        "should return error for misinterpreted absolute path"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("looks like an absolute path"),
        "error should contain hint: {err_msg}"
    );
    assert!(
        err_msg.contains("no `a/` prefix"),
        "error should mention no a/ prefix: {err_msg}"
    );

    // Cleanup
    let _ = tokio::fs::remove_file(&abs_file).await;
}

#[tokio::test]
async fn resolve_patch_path_no_hint_for_genuinely_missing_relative_path() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();

    // A relative path that doesn't exist in workspace and whose absolute
    // form also doesn't exist → should NOT produce a hint, just resolve normally.
    let result = resolve_patch_path(dir.path(), "nonexistent/deeply/nested/file.rs");
    assert!(
        result.is_ok(),
        "should resolve normally for missing relative path without hint"
    );
}

#[tokio::test]
async fn apply_patch_strips_diff_prefix_from_absolute_paths() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();

    let patch = format!(
        "--- a/{}\n+++ b/{}\n@@ -1,1 +1,1 @@\n-old\n+new\n",
        file.display(),
        file.display()
    );

    apply_patch(dir.path(), &patch).await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
}

#[tokio::test]
async fn apply_patch_supports_rename_only() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("old.txt");
    tokio::fs::write(&source, "hello\n").await.unwrap();

    let patch = r#"diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert!(!tokio::fs::try_exists(&source).await.unwrap());
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("new.txt"))
            .await
            .unwrap(),
        "hello\n"
    );
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Move);
    assert_eq!(outcome.ignored_metadata[0].kind, "similarity index");
    assert_eq!(outcome.ignored_metadata[0].path, "new.txt");
}

#[tokio::test]
async fn apply_patch_supports_rename_with_edit() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("old.txt");
    tokio::fs::write(&source, "hello\n").await.unwrap();

    let patch = r#"diff --git a/old.txt b/new.txt
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1,1 +1,1 @@
-hello
+world
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    assert!(!tokio::fs::try_exists(&source).await.unwrap());
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("new.txt"))
            .await
            .unwrap(),
        "world\n"
    );
}

#[tokio::test]
async fn apply_patch_line_number_drift_still_matches_unique_context() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "one\ntwo\nthree\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -42,1 +42,1 @@
-two
+TWO
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "one\nTWO\nthree\n"
    );
}

#[tokio::test]
async fn apply_patch_hunk_count_mismatch_is_diagnostic_not_failure() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "one\ntwo\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,99 +1,99 @@
-one
+ONE
 two
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(outcome.diagnostics[0].kind, "hunk_count_mismatch");
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "ONE\ntwo\n"
    );
}

#[tokio::test]
async fn relaxed_match_trailing_whitespace_preserves_context() {
    let dir = tempdir().unwrap();
    // File has trailing whitespace on context line.
    tokio::fs::write(dir.path().join("f.rs"), "line1\n    let x = 1;   \nline3\n")
        .await
        .unwrap();

    // LLM omits trailing whitespace in context — should still match via relaxed.
    let patch = "--- a/f.rs
+++ b/f.rs
@@ -1,3 +1,3 @@
 line1
    let x = 1;
 line3
";
    let outcome = apply_patch(dir.path(), patch).await.unwrap();
    assert!(outcome.diagnostics.is_empty(), "no diagnostics expected");

    let result = tokio::fs::read_to_string(dir.path().join("f.rs"))
        .await
        .unwrap();
    // Context line should retain original trailing whitespace.
    assert!(
        result.contains("    let x = 1;   \n"),
        "context line should preserve original trailing whitespace: {result:?}"
    );
}

#[tokio::test]
async fn relaxed_match_indent_difference_preserves_context() {
    let dir = tempdir().unwrap();
    // File uses 4-space indent.
    tokio::fs::write(
        dir.path().join("f.py"),
        "def foo():
    return 42
",
    )
    .await
    .unwrap();

    // LLM writes 2-space indent in context — relaxed match should work.
    let patch = "--- a/f.py
+++ b/f.py
@@ -1,2 +1,3 @@
 def foo():
   return 42
+
";
    let _outcome = apply_patch(dir.path(), patch).await.unwrap();

    let result = tokio::fs::read_to_string(dir.path().join("f.py"))
        .await
        .unwrap();
    // Context line should preserve file's actual 4-space indent.
    assert!(
        result.contains(
            "    return 42
"
        ),
        "Python context line must preserve original 4-space indent: {result:?}"
    );
}

#[tokio::test]
async fn relaxed_match_multiple_spaces_preserves_context() {
    let dir = tempdir().unwrap();
    // File has extra spaces between tokens.
    tokio::fs::write(
        dir.path().join("f.rs"),
        "let  x   =     1;
",
    )
    .await
    .unwrap();

    // LLM writes single spaces — level 3 relaxed match should find it.
    let patch = "--- a/f.rs
+++ b/f.rs
@@ -1,1 +1,2 @@
 let x = 1;
+let y = 2;
";
    let _outcome = apply_patch(dir.path(), patch).await.unwrap();

    let result = tokio::fs::read_to_string(dir.path().join("f.rs"))
        .await
        .unwrap();
    // Context line should preserve file's original multi-space formatting.
    assert!(
        result.contains(
            "let  x   =     1;
"
        ),
        "context line should preserve original spacing: {result:?}"
    );
}

#[tokio::test]
async fn relaxed_match_ambiguous_reports_error() {
    let dir = tempdir().unwrap();
    // Two identical lines that differ only in trailing whitespace.
    tokio::fs::write(dir.path().join("f.rs"), "foo   \nbar\nfoo  \n")
        .await
        .unwrap();

    // LLM writes "foo" — should match both (level 1 trim_end normalizes both to "foo").
    // Level 2 and 3 also match both. All levels ambiguous → ambiguous error.
    let patch = "--- a/f.rs
+++ b/f.rs
@@ -1,1 +1,1 @@
-foo
+replaced
";
    let result = apply_patch(dir.path(), patch).await;
    assert!(
        result.is_err(),
        "ambiguous context should produce error, not silent match"
    );
}

#[tokio::test]
async fn apply_patch_rejects_context_not_found_without_partial_writes() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "hello\nworld\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-missing
+present
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    let details = tool_error.details.as_ref().unwrap();
    assert!(details["nearby_lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["text"] == "hello"));
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "hello\nworld\n"
    );
}

#[tokio::test]
async fn apply_patch_handles_markdown_list_context_line() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.md");
    tokio::fs::write(&file, "- keep\nold\n").await.unwrap();

    let patch = r#"--- a/sample.md
+++ b/sample.md
@@ -1,2 +1,2 @@
 - keep
-old
+new
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "- keep\nnew\n"
    );
}

#[tokio::test]
async fn apply_patch_handles_markdown_list_deletion_line() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.md");
    tokio::fs::write(&file, "- remove\nkeep\n").await.unwrap();

    let patch = r#"--- a/sample.md
+++ b/sample.md
@@ -1,2 +1,1 @@
-- remove
 keep
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "keep\n");
}

#[tokio::test]
async fn apply_patch_handles_markdown_list_addition_line() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.md");
    tokio::fs::write(&file, "heading\n").await.unwrap();

    let patch = r#"--- a/sample.md
+++ b/sample.md
@@ -1,1 +1,2 @@
 heading
+- added
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "heading\n- added\n"
    );
}

#[tokio::test]
async fn apply_patch_context_not_found_suggests_markdown_list_prefixes() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.md");
    tokio::fs::write(&file, "- item\nnext\n").await.unwrap();

    let patch = r#"--- a/sample.md
+++ b/sample.md
@@ -1,1 +1,1 @@
- item
+done
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["suspected_issue"], "markdown_list_diff_prefix");
    let recovery_hint = tool_error.recovery_hint.unwrap();
    assert!(recovery_hint.contains("context ` - item`"));
    assert!(recovery_hint.contains("deletion `-- item`"));
    assert!(recovery_hint.contains("addition `+- item`"));
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "- item\nnext\n"
    );
}

#[tokio::test]
async fn apply_patch_doubled_prefix_remove_is_reinterpreted_after_strict_failure() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("sample.txt"), "alpha\nbeta\n")
        .await
        .unwrap();

    // `-​- alpha` parses as Remove("- alpha"), which does not exist in
    // the file; the doubled-marker alternative removes "alpha" instead.
    let patch = "--- a/sample.txt\n+++ b/sample.txt\n@@ -1,2 +1,2 @@\n-- alpha\n+- gamma\n+delta\n";

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("sample.txt"))
            .await
            .unwrap(),
        "gamma\ndelta\nbeta\n"
    );
    assert!(outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "double_prefix_reinterpreted"));
}

#[tokio::test]
async fn apply_patch_doubled_prefix_marker_initial_content_keeps_strict_priority() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("sample.md"), "- item\nnext\n")
        .await
        .unwrap();

    // `-- item` is the legitimate way to remove the line "- item".
    let patch = "--- a/sample.md\n+++ b/sample.md\n@@ -1,1 +1,1 @@\n-- item\n+done\n";

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("sample.md"))
            .await
            .unwrap(),
        "done\nnext\n"
    );
    assert!(!outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "double_prefix_reinterpreted"));
}

#[tokio::test]
async fn apply_patch_doubled_prefix_add_lines_are_reinterpreted_together() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("sample.txt"), "alpha\n")
        .await
        .unwrap();

    // Strict match on "- alpha" fails, so the whole hunk is retried with
    // doubled markers stripped from Remove and Add lines alike.
    let patch =
        "--- a/sample.txt\n+++ b/sample.txt\n@@ -1,1 +1,2 @@\n-- alpha\n+- gamma\n++ delta\n";

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("sample.txt"))
            .await
            .unwrap(),
        "gamma\ndelta\n"
    );
    assert!(outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "double_prefix_reinterpreted"));
}

#[tokio::test]
async fn apply_patch_rejects_ambiguous_context() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "same\nx\nsame\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-same
+changed
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "ambiguous_context");
}

#[tokio::test]
async fn apply_patch_rejects_path_escape() {
    let dir = tempdir().unwrap();
    let patch = r#"--- /dev/null
+++ b/../escape.txt
@@ -0,0 +1,1 @@
+bad
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "path_escape");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["requested_path"], "../escape.txt");
    assert!(details["resolved_path"]
        .as_str()
        .unwrap()
        .ends_with("escape.txt"));
    assert_eq!(
        details["active_workspace_root"],
        normalize_path(dir.path()).unwrap().display().to_string()
    );
}

#[tokio::test]
async fn apply_patch_supports_absolute_path_headers() {
    let workspace = tempdir().unwrap();
    let external = tempdir().unwrap();
    let file = external.path().join("absolute.txt");
    tokio::fs::write(&file, "old\n").await.unwrap();
    let path = file.display();
    let patch = format!(
        r#"--- {path}
+++ {path}
@@ -1,1 +1,1 @@
-old
+new
"#
    );

    let outcome = apply_patch(workspace.path(), &patch).await.unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    assert_eq!(outcome.changed_files[0].path, file.display().to_string());
    assert_eq!(outcome.changed_paths, vec![file.display().to_string()]);
}

#[tokio::test]
async fn apply_patch_strips_prefixed_absolute_add_paths() {
    let dir = tempdir().unwrap();
    let external = tempdir().unwrap();
    let file = external.path().join("target.txt");
    let path = format!("b/{}", file.display());
    let patch = format!(
        r#"--- /dev/null
+++ {path}
@@ -0,0 +1,1 @@
+safe
"#
    );

    let outcome = apply_patch(dir.path(), &patch).await.unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "safe\n");
    assert_eq!(outcome.changed_files[0].path, file.display().to_string());
    assert_eq!(outcome.changed_paths, vec![file.display().to_string()]);
}

#[tokio::test]
async fn apply_patch_preserves_relative_path_semantics() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("relative.txt"), "old\n")
        .await
        .unwrap();

    let patch = r#"--- a/relative.txt
+++ b/relative.txt
@@ -1,1 +1,1 @@
-old
+new
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("relative.txt"))
            .await
            .unwrap(),
        "new\n"
    );
    assert_eq!(outcome.changed_files[0].path, "relative.txt");
    assert_eq!(
        outcome.changed_paths,
        vec![dir.path().join("relative.txt").display().to_string()]
    );
}

#[tokio::test]
async fn apply_patch_context_not_found_diagnostic_is_bounded() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    let content = (1..=30)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    tokio::fs::write(&file, content).await.unwrap();

    let stale_context = (1..=30)
        .map(|line| format!("-stale {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let patch = format!(
        r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,30 +1,1 @@
{stale_context}
+replacement
"#
    );

    let error = apply_patch(dir.path(), &patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    let details = tool_error.details.as_ref().unwrap();
    let nearby_lines = details["nearby_lines"].as_array().unwrap();
    assert!(nearby_lines.len() <= 11);
    assert_eq!(details["nearby_range"]["start"], 1);
    assert_eq!(details["nearby_range"]["end"], nearby_lines.len());
    assert!(tool_error.recovery_hint.unwrap().len() < 1000);
}

#[tokio::test]
async fn apply_patch_context_not_found_empty_file_has_non_inverted_range() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("empty.txt"), "")
        .await
        .unwrap();

    let patch = r#"--- a/empty.txt
+++ b/empty.txt
@@ -1,1 +1,1 @@
-missing
+present
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["nearby_range"]["start"], 0);
    assert_eq!(details["nearby_range"]["end"], 0);
    assert!(details["nearby_lines"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn apply_patch_context_not_found_reports_first_divergence() {
    let dir = tempdir().unwrap();
    let content = (1..=30)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    tokio::fs::write(dir.path().join("sample.txt"), content)
        .await
        .unwrap();

    let matching_context = (1..=20)
        .map(|line| format!(" line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let patch = format!(
        r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,21 +1,1 @@
{matching_context}
-wrong 21
+replacement
"#
    );

    let error = apply_patch(dir.path(), &patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    let details = tool_error.details.as_ref().unwrap();
    let divergence = &details["first_divergence"];
    assert_eq!(divergence["candidate_line"], 1);
    assert_eq!(divergence["matched_prefix_lines"], 20);
    assert_eq!(divergence["file_line"], 21);
    assert_eq!(divergence["expected"][0], "wrong 21");
    assert_eq!(divergence["actual"][0], "line 21");
    let recovery_hint = tool_error.recovery_hint.unwrap();
    assert!(recovery_hint.contains("matched the first 20 context line(s)"));
    assert!(recovery_hint.contains("diverged at line 21"));
}

#[tokio::test]
async fn apply_patch_context_not_found_unnumbered_hunk_uses_candidate_window() {
    let dir = tempdir().unwrap();
    let content = (1..=50)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    tokio::fs::write(dir.path().join("sample.txt"), content)
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,3 +1,1 @@
 line 40
 line 41
-wrong 42
+replacement
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    let details = tool_error.details.as_ref().unwrap();
    let divergence = &details["first_divergence"];
    assert_eq!(divergence["candidate_line"], 40);
    assert_eq!(divergence["matched_prefix_lines"], 2);
    assert_eq!(divergence["file_line"], 42);
    // nearby window follows the divergence point instead of the file head
    assert_eq!(details["nearby_range"]["start"], 37);
    assert!(details["nearby_lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["text"] == "line 42"));
}

#[tokio::test]
async fn apply_patch_context_not_found_unnumbered_hunk_without_candidate_says_so() {
    let dir = tempdir().unwrap();
    let content = (1..=50)
        .map(|line| format!("entry {line}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    tokio::fs::write(dir.path().join("sample.txt"), content)
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,2 +1,1 @@
 missing-first
-missing-second
+replacement
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["candidate_search"], "none");
    assert_eq!(details["first_divergence"], serde_json::Value::Null);
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .unwrap()
        .contains("no usable line number"));
}

#[tokio::test]
async fn apply_patch_doubled_prefix_context_still_fails_when_no_match() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("sample.txt"), "other\ncontent\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-- alpha
+replacement
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("sample.txt"))
            .await
            .unwrap(),
        "other\ncontent\n"
    );
}

#[tokio::test]
async fn apply_patch_missing_file_reports_resolution_context() {
    let dir = tempdir().unwrap();
    let patch = r#"--- a/missing.txt
+++ b/missing.txt
@@ -1,1 +1,1 @@
-old
+new
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "missing_file");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["requested_path"], "missing.txt");
    assert!(details["resolved_path"]
        .as_str()
        .unwrap()
        .ends_with("missing.txt"));
    assert_eq!(
        details["active_workspace_root"],
        normalize_path(dir.path()).unwrap().display().to_string()
    );
}

#[tokio::test]
async fn apply_patch_modify_missing_target_with_pure_add_hunks_creates_file() {
    let dir = tempdir().unwrap();
    let patch = r#"--- a/created.txt
+++ b/created.txt
@@ -0,0 +1,2 @@
+first
+second
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("created.txt"))
            .await
            .unwrap(),
        "first\nsecond\n"
    );
    assert_eq!(outcome.changed_files.len(), 1);
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Add);
    assert!(outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "modify_to_missing_treated_as_add"));
}

#[tokio::test]
async fn apply_patch_modify_missing_target_with_context_hunks_still_fails() {
    let dir = tempdir().unwrap();
    let patch = r#"--- a/missing.txt
+++ b/missing.txt
@@ -1,2 +1,2 @@
 context
-old
+new
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "missing_file");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["action"], "update");
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .unwrap()
        .contains("/dev/null"));
    assert!(!dir.path().join("missing.txt").try_exists().unwrap());
}

#[tokio::test]
async fn apply_patch_modify_missing_target_with_remove_hunks_still_fails() {
    let dir = tempdir().unwrap();
    let patch = r#"--- a/missing.txt
+++ b/missing.txt
@@ -1,1 +1,1 @@
-old
+new
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "missing_file");
    assert!(!dir.path().join("missing.txt").try_exists().unwrap());
}

#[tokio::test]
async fn apply_patch_modify_missing_target_mixed_hunks_still_fails() {
    let dir = tempdir().unwrap();
    let patch = r#"--- a/missing.txt
+++ b/missing.txt
@@ -0,0 +1,1 @@
+first
@@ -1,1 +1,1 @@
-old
+new
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "missing_file");
    assert!(!dir.path().join("missing.txt").try_exists().unwrap());
}

#[tokio::test]
async fn apply_patch_applies_same_path_modify_patches_in_order() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "one\ntwo\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-one
+ONE
--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-ONE
+won
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "won\ntwo\n"
    );
    assert_eq!(outcome.changed_files.len(), 2);
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Modify);
    assert_eq!(outcome.changed_files[1].action, ApplyPatchAction::Modify);
}

#[tokio::test]
async fn apply_patch_applies_normalized_same_path_modify_patches_in_order() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "one\ntwo\nthree\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-one
+ONE
--- a/./sample.txt
+++ b/./sample.txt
@@ -3,1 +3,1 @@
-three
+THREE
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "ONE\ntwo\nTHREE\n"
    );
    assert_eq!(outcome.changed_files.len(), 2);
    assert_eq!(outcome.changed_paths.len(), 1);
}

#[tokio::test]
async fn apply_patch_rejects_stale_second_modify_without_partial_writes() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "one\ntwo\n").await.unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-one
+ONE
--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-one
+won
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "context_not_found");
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "one\ntwo\n"
    );
}

#[tokio::test]
async fn apply_patch_rejects_second_add_from_current_state_without_partial_writes() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");

    let patch = r#"--- /dev/null
+++ b/sample.txt
@@ -0,0 +1,1 @@
+one
--- /dev/null
+++ b/./sample.txt
@@ -0,0 +1,1 @@
+two
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "existing_file");
    assert!(!tokio::fs::try_exists(&file).await.unwrap());
}

#[tokio::test]
async fn apply_patch_applies_modify_after_rename_target_in_order() {
    let dir = tempdir().unwrap();
    let old = dir.path().join("old.txt");
    let new = dir.path().join("new.txt");
    tokio::fs::write(&old, "one\n").await.unwrap();

    let patch = r#"diff --git a/old.txt b/new.txt
rename from old.txt
rename to new.txt
diff --git a/new.txt b/new.txt
--- a/new.txt
+++ b/new.txt
@@ -1,1 +1,1 @@
-one
+two
"#;

    let outcome = apply_patch(dir.path(), patch).await.unwrap();

    assert!(!tokio::fs::try_exists(&old).await.unwrap());
    assert_eq!(tokio::fs::read_to_string(&new).await.unwrap(), "two\n");
    assert_eq!(outcome.changed_files.len(), 2);
    assert_eq!(outcome.changed_files[0].action, ApplyPatchAction::Move);
    assert_eq!(outcome.changed_files[1].action, ApplyPatchAction::Modify);
    assert_eq!(outcome.changed_paths.len(), 2);
}

#[test]
fn parse_patch_rejects_legacy_dsl() {
    let error = parse_patch("*** Begin Patch\n*** End Patch\n").unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "invalid_patch_syntax");
    assert_eq!(
        tool_error.details.as_ref().unwrap()["rule"],
        "wrong_patch_format"
    );
}

#[test]
fn parse_patch_rejects_invalid_hunk_empty_line_with_position() {
    let error = parse_patch("--- a/file.txt\n+++ b/file.txt\n@@ -1,1 +1,1 @@\n\n").unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "invalid_patch_syntax");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["line"], 4);
    assert_eq!(details["rule"], "invalid_hunk_empty_line");
}

#[test]
fn parse_patch_rejects_unsupported_binary_patch() {
    let error = parse_patch(
        "diff --git a/image.png b/image.png\nBinary files a/image.png and b/image.png differ\n",
    )
    .unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "unsupported_git_patch_feature");
}

#[test]
fn parse_patch_rejects_unsupported_copy_and_submodule_patch() {
    for patch in [
        "diff --git a/old.txt b/new.txt\ncopy from old.txt\ncopy to new.txt\n",
        "diff --git a/sub b/sub\nSubproject commit abc123\n",
    ] {
        let error = parse_patch(patch).unwrap_err();
        let tool_error = patch_tool_error(&error);
        assert_eq!(tool_error.kind, "unsupported_git_patch_feature");
    }
}

#[test]
fn parse_patch_rejects_rename_path_mismatch() {
    let error =
        parse_patch("diff --git a/old.txt b/new.txt\nrename from other.txt\nrename to new.txt\n")
            .unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "rename_path_mismatch");
}

#[test]
fn parse_patch_rejects_rename_without_git_header() {
    let error = parse_patch(
            "rename from old.txt\nrename to new.txt\n--- a/old.txt\n+++ b/new.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n",
        )
        .unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "missing_git_header");
}

mod proptests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::OnceLock;

    fn runtime() -> &'static tokio::runtime::Runtime {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn prop_add_file_creates_normalized_workspace_path(
            segments in prop::collection::vec("[a-zA-Z0-9_-]{1,16}", 1..4),
            content in "[a-zA-Z0-9 _-]{1,80}"
        ) {
            let dir = tempdir().unwrap();
            let path = format!("{}.txt", segments.join("/"));
            let expected_content = format!("{content}\n");

            let patch = format!(
                r#"--- /dev/null
+++ b/{path}
@@ -0,0 +1,1 @@
+{content}
"#);

            runtime()
                .block_on(apply_patch(dir.path(), &patch))
                .unwrap();

            prop_assert_eq!(
                std::fs::read_to_string(dir.path().join(path)).unwrap(),
                expected_content
            );
        }

        #[test]
        fn prop_path_escape_rejects_generated_escape_paths(
            file_name in "[a-zA-Z0-9_-]{1,20}"
        ) {
            let dir = tempdir().unwrap();
            let patch = format!(
                r#"--- /dev/null
+++ b/../{file_name}.txt
@@ -0,0 +1,1 @@
+bad
"#
            );

            let error = runtime()
                .block_on(apply_patch(dir.path(), &patch))
                .unwrap_err();
            let tool_error = patch_tool_error(&error);
            prop_assert_eq!(tool_error.kind, "path_escape");
        }

        #[test]
        fn prop_single_line_modify_replaces_only_target_content(
            original in "[a-zA-Z0-9 _-]{1,80}",
            replacement in "[a-zA-Z0-9 _-]{1,80}"
        ) {
            prop_assume!(original != replacement);

            let dir = tempdir().unwrap();
            let file = dir.path().join("test.txt");
            let original_content = format!("{original}\n");
            let replacement_content = format!("{replacement}\n");
            std::fs::write(&file, &original_content).unwrap();

            let patch = format!(
                r#"--- a/test.txt
+++ b/test.txt
@@ -1,1 +1,1 @@
-{original}
+{replacement}
"#
            );

            runtime()
                .block_on(apply_patch(dir.path(), &patch))
                .unwrap();

            prop_assert_eq!(std::fs::read_to_string(&file).unwrap(), replacement_content);
        }

        #[test]
        fn prop_patch_roundtrip_returns_to_original(
            initial_lines in prop::collection::vec("[a-zA-Z0-9_]{1,30}", 1..10),
            modify_line_index in 0usize..10,
            old_line in "[a-zA-Z0-9_]{1,30}",
            new_line in "[a-zA-Z0-9_]{1,30}",
        ) {
            prop_assume!(modify_line_index < initial_lines.len());
            prop_assume!(old_line != new_line);

            let dir = tempdir().unwrap();
            let file_path = dir.path().join("test.txt");

            let content_vec = initial_lines.clone();

            // Ensure old_line is not already in the content (to avoid ambiguous matches)
            let old_line_count = content_vec.iter().filter(|&line| line == &old_line).count();
            prop_assume!(old_line_count == 0);

            // Ensure new_line is not already in the content (to avoid ambiguous reverse matches)
            let new_line_count = content_vec.iter().filter(|&line| line == &new_line).count();
            prop_assume!(new_line_count == 0);

            let mut content_lines = content_vec;
            content_lines[modify_line_index] = old_line.clone();
            let original_content = content_lines.join("\n") + "\n";
            std::fs::write(&file_path, &original_content).unwrap();

            let old_start = modify_line_index + 1;
            let forward_patch = format!(
                r#"--- a/test.txt
+++ b/test.txt
@@ -{old_start},1 +{old_start},1 @@
-{old_line}
+{new_line}
"#
            );

            runtime()
                .block_on(apply_patch(dir.path(), &forward_patch))
                .unwrap();

            let reverse_patch = format!(
                r#"--- a/test.txt
+++ b/test.txt
@@ -{old_start},1 +{old_start},1 @@
-{new_line}
+{old_line}
"#
            );

            runtime()
                .block_on(apply_patch(dir.path(), &reverse_patch))
                .unwrap();

            let final_content = std::fs::read_to_string(&file_path).unwrap();
            prop_assert_eq!(final_content, original_content);
        }

        #[test]
        fn prop_context_matching_robustness(
            base_lines in prop::collection::vec("[a-zA-Z0-9_]{20,40}", 15..30),
            target_line in 5usize..15,
            replacement in "[a-zA-Z0-9_]{5,20}",
        ) {
            prop_assume!(base_lines[target_line] != replacement);

            let dir = tempdir().unwrap();
            let file_path = dir.path().join("test.txt");

            let original_content = base_lines.join("\n") + "\n";
            std::fs::write(&file_path, &original_content).unwrap();

            let context_start = target_line.saturating_sub(2);
            let context_end = (target_line + 3).min(base_lines.len());
            let old_start = context_start + 1;
            let old_count = context_end - context_start;

            // Ensure the old block occurs exactly once to avoid ambiguous_context errors
            let old_block = &base_lines[context_start..context_end];
            let old_block_occurrences = base_lines
                .windows(old_block.len())
                .filter(|window| *window == old_block)
                .count();
            prop_assume!(old_block_occurrences == 1);

            let mut hunk_lines = Vec::new();
            for i in context_start..target_line {
                hunk_lines.push(format!(" {}", &base_lines[i]));
            }
            hunk_lines.push(format!("-{}", &base_lines[target_line]));
            hunk_lines.push(format!("+{}", &replacement));
            for i in (target_line + 1)..context_end {
                if i < base_lines.len() {
                    hunk_lines.push(format!(" {}", &base_lines[i]));
                }
            }

            let patch = format!(
                r#"--- a/test.txt
+++ b/test.txt
@@ -{old_start},{old_count} +{old_start},{old_count} @@
{}"#,
                hunk_lines.join("\n")
            );

            let result = runtime()
                .block_on(apply_patch(dir.path(), &patch));

            prop_assert!(result.is_ok());
            let final_content = std::fs::read_to_string(&file_path).unwrap();
            let final_lines: Vec<&str> = final_content.lines().collect();
            prop_assert_eq!(final_lines[target_line], replacement);
        }
    }
}

#[tokio::test]
async fn apply_patch_rejects_ambiguous_context_with_repeated_blocks() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "same\nx\nsame\ny\nsame\nz\nsame\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,1 +1,1 @@
-same
+changed
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "ambiguous_context");

    // candidates must exist and have correct count ("same" occurs 4 times)
    let details = tool_error.details.as_ref().unwrap();
    let candidates = details["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 4);
    // each candidate must have line_number and distance_from_hint fields
    for c in candidates {
        assert!(c["line_number"].is_number());
        assert!(c["distance_from_hint"].is_number());
    }
}

#[tokio::test]
async fn apply_patch_uses_line_hint_when_window_has_unique_match() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    let mut lines = vec!["target".to_string()];
    lines.extend((1..=30).map(|index| format!("filler {index}")));
    lines.push("target".to_string());
    tokio::fs::write(&file, lines.join("\n") + "\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -32,1 +32,1 @@
-target
+changed
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    let content = tokio::fs::read_to_string(&file).await.unwrap();
    assert!(content.starts_with("target\n"));
    assert!(content.ends_with("changed\n"));
    assert!(!content.ends_with("target\n"));
}

#[tokio::test]
async fn apply_patch_preserves_ambiguous_context_when_hint_window_is_not_unique() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "target\nnear\ntarget\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -2,1 +2,1 @@
-target
+changed
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "ambiguous_context");
}

#[tokio::test]
async fn apply_patch_reports_candidates_near_hint() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "repeat\nline\nrepeat\nline\nrepeat\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -2,1 +2,1 @@
-repeat
+changed
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "ambiguous_context");

    // "repeat" occurs 3 times in the file
    let details = tool_error.details.as_ref().unwrap();
    let candidates = details["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3);

    // hint is line 2 (which is "line"), but "repeat" is at lines 1, 3, 5
    // So the closest candidate to line 2 is line 1 (index 0), which should have distance 1
    // Actually, the distance calculation is: |candidate_index - hint_index|
    // hint_index = 2 - 1 = 1, candidates are at indices 0, 2, 4
    // distances are |0-1|=1, |2-1|=1, |4-1|=3
    // We should find a candidate with distance 1, not 0
    assert!(candidates.iter().any(|c| c["distance_from_hint"] == 1));
}

#[tokio::test]
async fn apply_patch_handles_stale_line_hint() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "pattern\na\nb\nc\npattern\nx\ny\nz\n")
        .await
        .unwrap();

    // hint points to line 42 (far beyond file range), but should still return all candidates
    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -42,1 +42,1 @@
-pattern
+changed
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);
    assert_eq!(tool_error.kind, "ambiguous_context");

    // "pattern" occurs 2 times in the file
    let details = tool_error.details.as_ref().unwrap();
    let candidates = details["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
async fn apply_patch_succeeds_with_unique_context() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "unique_before\nunique_target\nunique_after\n")
        .await
        .unwrap();

    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,3 +1,3 @@
 unique_before
-unique_target
+CHANGED
 unique_after
"#;

    apply_patch(dir.path(), patch).await.unwrap();
    let content = tokio::fs::read_to_string(&file).await.unwrap();
    assert_eq!(content, "unique_before\nCHANGED\nunique_after\n");
}

#[tokio::test]
async fn apply_patch_rejects_empty_hunk_line_without_prefix() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "line1\nline2\nline3\n")
        .await
        .unwrap();

    // Patch with an empty line that lacks the required space prefix
    let patch = r#"--- a/sample.txt
+++ b/sample.txt
@@ -1,3 +1,3 @@
 line1

+line2_modified
 line3
"#;

    let error = apply_patch(dir.path(), patch).await.unwrap_err();
    let tool_error = patch_tool_error(&error);

    // Verify the syntax error category and specific rule
    assert_eq!(tool_error.kind, "invalid_patch_syntax");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["rule"], "invalid_hunk_empty_line");

    // Verify the error message contains helpful context
    let error_message = tool_error.message.to_lowercase();
    assert!(
        error_message.contains("empty"),
        "error should mention empty line"
    );
    assert!(
        error_message.contains("prefix"),
        "error should mention prefix requirement"
    );
    assert!(
        error_message.contains("space"),
        "error should suggest space character"
    );
}

#[tokio::test]
async fn apply_patch_multiple_hunks_same_file_first_hunk_inserts_lines() {
    // First hunk adds 2 lines, shifting the file content down.
    // Second hunk must locate its context on the modified (longer) file,
    // verifying offset accumulation in apply_hunks.
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "header\nmiddle\nfooter\n")
        .await
        .unwrap();

    let patch = "--- a/sample.txt\n+++ b/sample.txt\n@@ -1,1 +1,3 @@\n header\n+inserted_a\n+inserted_b\n middle\n@@ -3,1 +5,1 @@\n-footer\n+FOOTER\n";

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "header\ninserted_a\ninserted_b\nmiddle\nFOOTER\n"
    );
}

#[tokio::test]
async fn apply_patch_multiple_hunks_same_file_first_hunk_removes_lines() {
    // First hunk removes 2 lines, shifting the file content up.
    // Second hunk must locate its context on the modified (shorter) file,
    // verifying offset accumulation in the negative direction.
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "header\nline_a\nline_b\nmiddle\nfooter\n")
        .await
        .unwrap();

    let patch = "--- a/sample.txt\n+++ b/sample.txt\n@@ -1,3 +1,1 @@\n header\n-line_a\n-line_b\n middle\n@@ -5,1 +3,1 @@\n-footer\n+FOOTER\n";

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "header\nmiddle\nFOOTER\n"
    );
}

#[tokio::test]
async fn apply_patch_multiple_hunks_same_file_replace_with_different_count() {
    // First hunk replaces 1 line with 3 (net +2), second hunk replaces
    // 2 lines with 1 (net -1). Verifies that offset from the first hunk
    // does not corrupt the second hunk's context matching.
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "alpha\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();

    let patch = "--- a/sample.txt\n+++ b/sample.txt\n@@ -1,1 +1,3 @@\n-alpha\n+alpha_a\n+alpha_b\n+alpha_c\n beta\n@@ -3,2 +5,1 @@\n-gamma\n-delta\n+GAMMA\n";

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "alpha_a\nalpha_b\nalpha_c\nbeta\nGAMMA\n"
    );
}

#[tokio::test]
async fn apply_patch_multiple_hunks_same_file_with_stale_line_numbers() {
    // Two hunks on the same file where old line numbers in the second
    // hunk reflect the ORIGINAL file (not the post-first-hunk state).
    // The matcher must still locate unique context by content search,
    // not by the (now-drifted) line hint. This hunk pair has no net
    // line count change from hunk 1, testing the normal sequential path.
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    tokio::fs::write(&file, "one\ntwo\nthree\nfour\nfive\n")
        .await
        .unwrap();

    let patch = "--- a/sample.txt\n+++ b/sample.txt\n@@ -1,1 +1,1 @@\n-one\n+ONE\n two\n@@ -5,1 +5,1 @@\n-five\n+FIVE\n";

    apply_patch(dir.path(), patch).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "ONE\ntwo\nthree\nfour\nFIVE\n"
    );
}
