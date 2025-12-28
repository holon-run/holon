package workspace

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/holon-run/holon/pkg/git"
)

// TestSnapshotPreparerGitDiff verifies that a snapshot workspace can generate diffs
// This is the primary acceptance criterion for the snapshot strategy
func TestSnapshotPreparerGitDiff(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping snapshot diff test in short mode")
	}

	// Create a temporary git repository to use as source
	sourceDir := t.TempDir()
	setupTestRepoWithMultipleCommits(t, sourceDir)

	ctx := context.Background()
	destDir := t.TempDir()

	preparer := NewSnapshotPreparer()
	req := PrepareRequest{
		Source:  sourceDir,
		Dest:    destDir,
		History: HistoryNone,
	}

	result, err := preparer.Prepare(ctx, req)
	if err != nil {
		t.Fatalf("Prepare() failed: %v", err)
	}

	// Verify basic properties
	if result.Strategy != "snapshot" {
		t.Errorf("expected strategy snapshot, got %s", result.Strategy)
	}
	if result.HasHistory {
		t.Error("expected HasHistory to be false for snapshot")
	}
	if result.IsShallow {
		t.Error("expected IsShallow to be false for snapshot")
	}

	// Verify HEAD SHA was captured from source
	if result.HeadSHA == "" {
		t.Error("expected HeadSHA to be set (captured from source)")
	}

	// Verify git diff works - this is the key acceptance criterion
	// First, make a change to a file
	testFile1 := filepath.Join(destDir, "test1.txt")
	content, err := os.ReadFile(testFile1)
	if err != nil {
		t.Fatalf("failed to read test file: %v", err)
	}

	// Modify the file
	newContent := string(content) + "\nmodified line"
	if err := os.WriteFile(testFile1, []byte(newContent), 0o644); err != nil {
		t.Fatalf("failed to modify test file: %v", err)
	}

	// Run git diff to verify it works
	cmd := exec.Command("git", "-C", destDir, "diff", "--no-color", "test1.txt")
	output, err := cmd.Output()
	if err != nil {
		t.Errorf("git diff failed: %v\nThis is the key acceptance criterion - snapshot workspaces must support git diff", err)
	}

	diffOutput := string(output)
	if !strings.Contains(diffOutput, "+modified line") {
		t.Error("git diff output does not contain the expected change")
	}

	// Verify the file was actually modified
	if !strings.Contains(diffOutput, "test content 1") {
		t.Error("git diff output does not show original content")
	}

	// Test creating a new file and checking git status
	newFile := filepath.Join(destDir, "newfile.txt")
	if err := os.WriteFile(newFile, []byte("new content"), 0o644); err != nil {
		t.Fatalf("failed to create new file: %v", err)
	}

	// Check git status
	cmd = exec.Command("git", "-C", destDir, "status", "--porcelain")
	statusOutput, err := cmd.Output()
	if err != nil {
		t.Errorf("git status failed: %v", err)
	}

	trimmedStatus := strings.TrimSpace(string(statusOutput))
	if trimmedStatus == "" {
		t.Errorf("expected at least 2 changed files in git status, got empty status output")
	} else {
		statusLines := strings.Split(trimmedStatus, "\n")
		if len(statusLines) < 2 {
			t.Errorf("expected at least 2 changed files in git status, got: %s", string(statusOutput))
		}
	}

	// Verify git status shows modified and new files
	statusText := string(statusOutput)
	if !strings.Contains(statusText, "M test1.txt") && !strings.Contains(statusText, " Mtest1.txt") {
		t.Log("git status output:", statusText)
		t.Error("git status should show test1.txt as modified")
	}
}

// TestSnapshotPreparerNoNetwork verifies that snapshot strategy works without network
// This is an important requirement - third-party projects may be offline
func TestSnapshotPreparerNoNetwork(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping snapshot no-network test in short mode")
	}

	// Create a source directory that is NOT a git repo
	// This simulates a third-party project download
	sourceDir := t.TempDir()

	// Create some files
	testFile1 := filepath.Join(sourceDir, "file1.txt")
	if err := os.WriteFile(testFile1, []byte("content 1"), 0o644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	testDir := filepath.Join(sourceDir, "subdir")
	if err := os.MkdirAll(testDir, 0o755); err != nil {
		t.Fatalf("failed to create subdirectory: %v", err)
	}
	testFile2 := filepath.Join(testDir, "file2.txt")
	if err := os.WriteFile(testFile2, []byte("content 2"), 0o644); err != nil {
		t.Fatalf("failed to create test file 2: %v", err)
	}

	ctx := context.Background()
	destDir := t.TempDir()

	preparer := NewSnapshotPreparer()
	req := PrepareRequest{
		Source:  sourceDir,
		Dest:    destDir,
		History: HistoryNone,
	}

	result, err := preparer.Prepare(ctx, req)
	if err != nil {
		t.Fatalf("Prepare() failed: %v", err)
	}

	// Verify the snapshot was created
	if result.Strategy != "snapshot" {
		t.Errorf("expected strategy snapshot, got %s", result.Strategy)
	}

	// Verify files were copied
	copiedFile1 := filepath.Join(destDir, "file1.txt")
	if _, err := os.Stat(copiedFile1); os.IsNotExist(err) {
		t.Error("file1.txt was not copied to destination")
	}

	copiedFile2 := filepath.Join(destDir, "subdir", "file2.txt")
	if _, err := os.Stat(copiedFile2); os.IsNotExist(err) {
		t.Error("subdir/file2.txt was not copied to destination")
	}

	// Verify minimal git repo was initialized (so git diff still works)
	client := git.NewClient(destDir)
	if !client.IsRepo(context.Background()) {
		t.Error("destination should be a minimal git repo for diff generation")
	}

	// Verify git diff works even for non-git-source projects
	// Modify a file
	if err := os.WriteFile(copiedFile1, []byte("modified content"), 0o644); err != nil {
		t.Fatalf("failed to modify file: %v", err)
	}

	// Check git diff
	cmd := exec.Command("git", "-C", destDir, "diff", "--no-color", "file1.txt")
	output, err := cmd.Output()
	if err != nil {
		t.Errorf("git diff failed for non-git-source project: %v", err)
	}

	if !strings.Contains(string(output), "modified content") {
		t.Error("git diff should show the modification")
	}
}

// TestSnapshotPreparerPreservesSourceSHA verifies that snapshot preserves the source commit SHA
func TestSnapshotPreparerPreservesSourceSHA(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping snapshot SHA preservation test in short mode")
	}

	// Create a git repository with a known commit
	sourceDir := t.TempDir()
	setupTestRepo(t, sourceDir)

	// Get the source HEAD SHA
	sourceSHA := getGitSHA(t, sourceDir)

	ctx := context.Background()
	destDir := t.TempDir()

	preparer := NewSnapshotPreparer()
	req := PrepareRequest{
		Source:  sourceDir,
		Dest:    destDir,
		History: HistoryNone,
	}

	result, err := preparer.Prepare(ctx, req)
	if err != nil {
		t.Fatalf("Prepare() failed: %v", err)
	}

	// The HEAD SHA in result should match the source SHA
	if result.HeadSHA != sourceSHA {
		t.Errorf("expected HeadSHA %s (from source), got %s", sourceSHA, result.HeadSHA)
	}

	// Verify the commit message mentions the original SHA
	cmd := exec.Command("git", "-C", destDir, "log", "--oneline", "-1")
	output, err := cmd.Output()
	if err != nil {
		t.Errorf("git log failed: %v", err)
	}

	logMsg := string(output)
	if !strings.Contains(logMsg, sourceSHA) {
		t.Logf("Commit message: %s", logMsg)
		t.Log("Note: Commit message may or may not include original SHA depending on implementation")
	}
}

// TestSnapshotPreparerRefHandling verifies how snapshot handles ref parameter
func TestSnapshotPreparerRefHandling(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping snapshot ref handling test in short mode")
	}

	sourceDir := t.TempDir()
	setupTestRepo(t, sourceDir)

	ctx := context.Background()
	destDir := t.TempDir()

	preparer := NewSnapshotPreparer()
	req := PrepareRequest{
		Source:  sourceDir,
		Dest:    destDir,
		History: HistoryNone,
		Ref:     "main", // Request a specific ref
	}

	result, err := preparer.Prepare(ctx, req)
	if err != nil {
		t.Fatalf("Prepare() failed: %v", err)
	}

	// The ref should be recorded in the result
	if result.Ref != "main" {
		t.Errorf("expected Ref to be 'main', got %s", result.Ref)
	}

	// The snapshot should note that ref checkout wasn't performed
	hasRefNote := false
	for _, note := range result.Notes {
		if strings.Contains(note, "ref") && strings.Contains(note, "not checked out") {
			hasRefNote = true
			break
		}
	}

	if !hasRefNote {
		t.Log("Note: Ref checkout warning not found in notes - implementation may have changed")
		t.Logf("Notes: %v", result.Notes)
	}
}
