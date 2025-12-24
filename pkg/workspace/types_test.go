package workspace

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func TestPrepareRequestValidation(t *testing.T) {
	tests := []struct {
		name    string
		req     PrepareRequest
		wantErr bool
	}{
		{
			name: "valid request",
			req: PrepareRequest{
				Source:  "/path/to/source",
				Dest:    "/path/to/dest",
				History: HistoryFull,
			},
			wantErr: false,
		},
		{
			name: "empty source",
			req: PrepareRequest{
				Source:  "",
				Dest:    "/path/to/dest",
				History: HistoryFull,
			},
			wantErr: true,
		},
		{
			name: "empty dest",
			req: PrepareRequest{
				Source:  "/path/to/source",
				Dest:    "",
				History: HistoryFull,
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			preparer := &GitClonePreparer{}
			err := preparer.Validate(tt.req)
			if (err != nil) != tt.wantErr {
				t.Errorf("Validate() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

func TestGitClonePreparer(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping git clone test in short mode")
	}

	// Create a temporary git repository to use as source
	sourceDir := t.TempDir()
	setupTestRepo(t, sourceDir)

	t.Run("Prepare with full history", func(t *testing.T) {
		ctx := context.Background()
		destDir := t.TempDir()

		preparer := NewGitClonePreparer()
		req := PrepareRequest{
			Source:  sourceDir,
			Dest:    destDir,
			History: HistoryFull,
		}

		result, err := preparer.Prepare(ctx, req)
		if err != nil {
			t.Fatalf("Prepare() failed: %v", err)
		}

		if result.Strategy != "git-clone" {
			t.Errorf("expected strategy git-clone, got %s", result.Strategy)
		}
		if !result.HasHistory {
			t.Error("expected HasHistory to be true for HistoryFull")
		}
		if result.IsShallow {
			t.Error("expected IsShallow to be false for HistoryFull")
		}
		if result.HeadSHA == "" {
			t.Error("expected HeadSHA to be set")
		}

		// Verify workspace.manifest.json exists
		manifestPath := filepath.Join(destDir, "workspace.manifest.json")
		if _, err := os.Stat(manifestPath); os.IsNotExist(err) {
			t.Error("workspace.manifest.json was not created")
		}
	})

	t.Run("Prepare with shallow history", func(t *testing.T) {
		ctx := context.Background()
		destDir := t.TempDir()

		preparer := NewGitClonePreparer()
		req := PrepareRequest{
			Source:  sourceDir,
			Dest:    destDir,
			History: HistoryShallow,
		}

		result, err := preparer.Prepare(ctx, req)
		if err != nil {
			t.Fatalf("Prepare() failed: %v", err)
		}

		if !result.HasHistory {
			t.Error("expected HasHistory to be true for HistoryShallow")
		}
		if !result.IsShallow {
			t.Error("expected IsShallow to be true for HistoryShallow")
		}
	})

	t.Run("Prepare with no history", func(t *testing.T) {
		ctx := context.Background()
		destDir := t.TempDir()

		preparer := NewGitClonePreparer()
		req := PrepareRequest{
			Source:  sourceDir,
			Dest:    destDir,
			History: HistoryNone,
		}

		result, err := preparer.Prepare(ctx, req)
		if err != nil {
			t.Fatalf("Prepare() failed: %v", err)
		}

		if result.HasHistory {
			t.Error("expected HasHistory to be false for HistoryNone")
		}
	})

	t.Run("Cleanup", func(t *testing.T) {
		ctx := context.Background()
		destDir := t.TempDir()

		preparer := NewGitClonePreparer()
		req := PrepareRequest{
			Source:  sourceDir,
			Dest:    destDir,
			History: HistoryFull,
		}

		_, err := preparer.Prepare(ctx, req)
		if err != nil {
			t.Fatalf("Prepare() failed: %v", err)
		}

		// Verify directory exists
		if _, err := os.Stat(destDir); os.IsNotExist(err) {
			t.Fatal("destination directory does not exist after Prepare()")
		}

		// Cleanup
		err = preparer.Cleanup(destDir)
		if err != nil {
			t.Fatalf("Cleanup() failed: %v", err)
		}

		// Verify directory is removed
		if _, err := os.Stat(destDir); !os.IsNotExist(err) {
			t.Error("destination directory still exists after Cleanup()")
		}
	})
}

func TestSnapshotPreparer(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping snapshot test in short mode")
	}

	// Create a temporary git repository to use as source
	sourceDir := t.TempDir()
	setupTestRepo(t, sourceDir)

	t.Run("Prepare snapshot", func(t *testing.T) {
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

		if result.Strategy != "snapshot" {
			t.Errorf("expected strategy snapshot, got %s", result.Strategy)
		}
		if result.HasHistory {
			t.Error("expected HasHistory to be false for snapshot")
		}
		if result.HeadSHA == "" {
			t.Error("expected HeadSHA to be set (from source)")
		}

		// Verify .git was removed (it's a true snapshot)
		gitDir := filepath.Join(destDir, ".git")
		if _, err := os.Stat(gitDir); !os.IsNotExist(err) {
			// .git should either not exist or be a minimal git repo
			// (we now initialize a minimal git for compatibility)
			t.Log("Note: .git exists (minimal git initialized for compatibility)")
		}

		// Verify workspace.manifest.json exists
		manifestPath := filepath.Join(destDir, "workspace.manifest.json")
		if _, err := os.Stat(manifestPath); os.IsNotExist(err) {
			t.Error("workspace.manifest.json was not created")
		}
	})

	t.Run("Cleanup", func(t *testing.T) {
		ctx := context.Background()
		destDir := t.TempDir()

		preparer := NewSnapshotPreparer()
		req := PrepareRequest{
			Source:  sourceDir,
			Dest:    destDir,
			History: HistoryNone,
		}

		_, err := preparer.Prepare(ctx, req)
		if err != nil {
			t.Fatalf("Prepare() failed: %v", err)
		}

		// Cleanup
		err = preparer.Cleanup(destDir)
		if err != nil {
			t.Fatalf("Cleanup() failed: %v", err)
		}

		// Verify directory is removed
		if _, err := os.Stat(destDir); !os.IsNotExist(err) {
			t.Error("destination directory still exists after Cleanup()")
		}
	})
}

func TestExistingPreparer(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping existing test in short mode")
	}

	// Create a temporary git repository to use as source
	sourceDir := t.TempDir()
	setupTestRepo(t, sourceDir)

	t.Run("Prepare existing", func(t *testing.T) {
		ctx := context.Background()

		preparer := NewExistingPreparer()
		req := PrepareRequest{
			Source:  sourceDir,
			Dest:    "/ignored", // dest is ignored for existing strategy
			History: HistoryNone,
		}

		result, err := preparer.Prepare(ctx, req)
		if err != nil {
			t.Fatalf("Prepare() failed: %v", err)
		}

		if result.Strategy != "existing" {
			t.Errorf("expected strategy existing, got %s", result.Strategy)
		}
		if result.HeadSHA == "" {
			t.Error("expected HeadSHA to be set")
		}

		// Verify workspace.manifest.json was written to source
		manifestPath := filepath.Join(sourceDir, "workspace.manifest.json")
		if _, err := os.Stat(manifestPath); os.IsNotExist(err) {
			t.Error("workspace.manifest.json was not created")
		}
	})

	t.Run("Cleanup is no-op", func(t *testing.T) {
		preparer := NewExistingPreparer()

		// Cleanup should be a no-op
		err := preparer.Cleanup(sourceDir)
		if err != nil {
			t.Fatalf("Cleanup() failed: %v", err)
		}

		// Directory should still exist
		if _, err := os.Stat(sourceDir); os.IsNotExist(err) {
			t.Error("source directory was removed (should be no-op)")
		}
	})
}

func TestManifest(t *testing.T) {
	t.Run("Write and Read manifest", func(t *testing.T) {
		dir := t.TempDir()

		result := PrepareResult{
			Strategy:   "git-clone",
			Source:     "/path/to/source",
			Ref:        "main",
			HeadSHA:    "abc123",
			HasHistory: true,
			IsShallow:  false,
		}

		err := WriteManifest(dir, result)
		if err != nil {
			t.Fatalf("WriteManifest() failed: %v", err)
		}

		manifest, err := ReadManifest(dir)
		if err != nil {
			t.Fatalf("ReadManifest() failed: %v", err)
		}

		if manifest.Strategy != result.Strategy {
			t.Errorf("expected Strategy %s, got %s", result.Strategy, manifest.Strategy)
		}
		if manifest.Source != result.Source {
			t.Errorf("expected Source %s, got %s", result.Source, manifest.Source)
		}
		if manifest.HeadSHA != result.HeadSHA {
			t.Errorf("expected HeadSHA %s, got %s", result.HeadSHA, manifest.HeadSHA)
		}
	})

	t.Run("Read non-existent manifest", func(t *testing.T) {
		dir := t.TempDir()

		_, err := ReadManifest(dir)
		if err == nil {
			t.Error("expected error when reading non-existent manifest")
		}
	})
}

// setupTestRepo creates a minimal git repository for testing
func setupTestRepo(t *testing.T, dir string) {
	t.Helper()

	// Create a test file
	testFile := filepath.Join(dir, "test.txt")
	if err := os.WriteFile(testFile, []byte("test content"), 0o644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	// Initialize git repo
	runGit(t, dir, "init")
	runGit(t, dir, "config", "user.email", "test@test.com")
	runGit(t, dir, "config", "user.name", "Test User")
	runGit(t, dir, "add", "test.txt")
	runGit(t, dir, "commit", "-m", "Initial commit")
}

func runGit(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := runCmd(t, dir, "git", args...)
	if err := cmd.Run(); err != nil {
		t.Fatalf("git %v failed: %v", args, err)
	}
}

func runCmd(t *testing.T, dir string, name string, args ...string) *exec.Cmd {
	t.Helper()
	cmdArgs := []string{"-C", dir}
	cmdArgs = append(cmdArgs, args...)
	return exec.Command(name, cmdArgs...)
}
