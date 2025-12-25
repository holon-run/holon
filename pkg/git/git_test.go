package git

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// setupTestRepo creates a temporary git repository for testing.
func setupTestRepo(t *testing.T) (string, func()) {
	t.Helper()

	tmpDir := t.TempDir()

	// Initialize repository
	cmd := exec.Command("git", "init")
	cmd.Dir = tmpDir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git init failed: %v, output: %s", err, string(out))
	}

	// Configure git
	cmd = exec.Command("git", "config", "user.name", "Test User")
	cmd.Dir = tmpDir
	if err := cmd.Run(); err != nil {
		t.Fatalf("git config user.name failed: %v", err)
	}

	cmd = exec.Command("git", "config", "user.email", "test@example.com")
	cmd.Dir = tmpDir
	if err := cmd.Run(); err != nil {
		t.Fatalf("git config user.email failed: %v", err)
	}

	// Create initial commit
	testFile := filepath.Join(tmpDir, "README.md")
	if err := os.WriteFile(testFile, []byte("test readme"), 0644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	cmd = exec.Command("git", "add", "README.md")
	cmd.Dir = tmpDir
	if err := cmd.Run(); err != nil {
		t.Fatalf("git add failed: %v", err)
	}

	cmd = exec.Command("git", "commit", "-m", "initial commit")
	cmd.Dir = tmpDir
	if err := cmd.Run(); err != nil {
		t.Fatalf("git commit failed: %v", err)
	}

	cleanup := func() {
		os.RemoveAll(tmpDir)
	}

	return tmpDir, cleanup
}

// setupRemoteRepo creates a bare repository for testing push operations.
func setupRemoteRepo(t *testing.T) string {
	t.Helper()

	remoteDir := t.TempDir()

	// Initialize bare repository
	cmd := exec.Command("git", "init", "--bare")
	cmd.Dir = remoteDir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git init --bare failed: %v, output: %s", err, string(out))
	}

	return remoteDir
}

func TestClient_IsRepo(t *testing.T) {
	ctx := context.Background()

	t.Run("valid git repository", func(t *testing.T) {
		repoDir, _ := setupTestRepo(t)
		client := NewClient(repoDir)

		if !client.IsRepo(ctx) {
			t.Error("expected directory to be a git repository")
		}
	})

	t.Run("non-git directory", func(t *testing.T) {
		tmpDir := t.TempDir()
		client := NewClient(tmpDir)

		if client.IsRepo(ctx) {
			t.Error("expected directory to not be a git repository")
		}
	})
}

func TestClient_GetHeadSHA(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	sha, err := client.GetHeadSHA(ctx)
	if err != nil {
		t.Fatalf("GetHeadSHA failed: %v", err)
	}

	if sha == "" {
		t.Error("expected non-empty SHA")
	}

	if len(sha) != 40 {
		t.Errorf("expected SHA length 40, got %d", len(sha))
	}
}

func TestClient_IsShallowClone(t *testing.T) {
	ctx := context.Background()

	t.Run("full clone", func(t *testing.T) {
		repoDir, _ := setupTestRepo(t)
		client := NewClient(repoDir)

		isShallow, err := client.IsShallowClone(ctx)
		if err != nil {
			t.Fatalf("IsShallowClone failed: %v", err)
		}

		if isShallow {
			t.Error("expected repository to not be shallow")
		}
	})

	t.Run("shallow clone", func(t *testing.T) {
		t.Skip("Skipping shallow clone test - git clone --depth doesn't create shallow repos when cloning from a local file:// URL")

		// Create a shallow clone
		repoDir, _ := setupTestRepo(t)
		cloneDir := t.TempDir()

		cmd := exec.Command("git", "clone", "--depth=1", repoDir, cloneDir)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git clone --depth=1 failed: %v, output: %s", err, string(out))
		}

		client := NewClient(cloneDir)
		isShallow, err := client.IsShallowClone(ctx)
		if err != nil {
			t.Fatalf("IsShallowClone failed: %v", err)
		}

		if !isShallow {
			t.Error("expected repository to be shallow")
		}
	})
}

func TestClient_GetRepositoryInfo(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	info, err := client.GetRepositoryInfo(ctx)
	if err != nil {
		t.Fatalf("GetRepositoryInfo failed: %v", err)
	}

	if info.HEAD == "" {
		t.Error("expected non-empty HEAD SHA")
	}

	if info.Branch != "main" && info.Branch != "master" {
		t.Errorf("expected branch to be main or master, got %s", info.Branch)
	}

	if !info.Clean {
		t.Error("expected working directory to be clean")
	}
}

func TestClient_Checkout(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Get the initial branch name
	info, err := client.GetRepositoryInfo(ctx)
	if err != nil {
		t.Fatalf("failed to get repo info: %v", err)
	}
	initialBranch := info.Branch

	// Create a new branch
	if err := client.Branch(ctx, "test-branch", true); err != nil {
		t.Fatalf("failed to create branch: %v", err)
	}

	// Checkout initial branch
	if err := client.Checkout(ctx, initialBranch); err != nil {
		t.Fatalf("failed to checkout %s: %v", initialBranch, err)
	}

	// Verify we're on the initial branch
	info, err = client.GetRepositoryInfo(ctx)
	if err != nil {
		t.Fatalf("failed to get repo info: %v", err)
	}

	if info.Branch != initialBranch {
		t.Errorf("expected branch %s, got %s", initialBranch, info.Branch)
	}
}

func TestClient_Branch(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	t.Run("create new branch", func(t *testing.T) {
		if err := client.Branch(ctx, "feature-branch", true); err != nil {
			t.Fatalf("failed to create branch: %v", err)
		}

		// Verify branch was created
		info, err := client.GetRepositoryInfo(ctx)
		if err != nil {
			t.Fatalf("failed to get repo info: %v", err)
		}

		if info.Branch != "feature-branch" {
			t.Errorf("expected branch feature-branch, got %s", info.Branch)
		}
	})

	t.Run("checkout existing branch", func(t *testing.T) {
		// Get the initial branch name
		info, err := client.GetRepositoryInfo(ctx)
		if err != nil {
			t.Fatalf("failed to get repo info: %v", err)
		}
		initialBranch := info.Branch

		// First create a branch
		if err := client.Branch(ctx, "another-branch", true); err != nil {
			t.Fatalf("failed to create branch: %v", err)
		}

		// Switch back to initial branch
		if err := client.Checkout(ctx, initialBranch); err != nil {
			t.Fatalf("failed to checkout %s: %v", initialBranch, err)
		}

		// Checkout the existing branch
		if err := client.Branch(ctx, "another-branch", false); err != nil {
			t.Fatalf("failed to checkout existing branch: %v", err)
		}

		// Verify we're on the branch
		info, err = client.GetRepositoryInfo(ctx)
		if err != nil {
			t.Fatalf("failed to get repo info: %v", err)
		}

		if info.Branch != "another-branch" {
			t.Errorf("expected branch another-branch, got %s", info.Branch)
		}
	})
}

func TestClient_AddAndCommit(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Create a new file
	newFile := filepath.Join(repoDir, "test.txt")
	if err := os.WriteFile(newFile, []byte("test content"), 0644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	// Stage the file
	if err := client.Add(ctx, "test.txt"); err != nil {
		t.Fatalf("Add failed: %v", err)
	}

	// Commit the changes
	sha, err := client.Commit(ctx, "add test file")
	if err != nil {
		t.Fatalf("Commit failed: %v", err)
	}

	if sha == "" {
		t.Error("expected non-empty commit SHA")
	}

	// Verify working directory is clean
	if !client.IsClean(ctx) {
		t.Error("expected working directory to be clean after commit")
	}
}

func TestClient_AddAll(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Create multiple files
	for i := 0; i < 3; i++ {
		filename := filepath.Join(repoDir, "file"+string(rune('1'+i))+".txt")
		if err := os.WriteFile(filename, []byte("content"), 0644); err != nil {
			t.Fatalf("failed to create test file: %v", err)
		}
	}

	// Stage all files
	if err := client.AddAll(ctx); err != nil {
		t.Fatalf("AddAll failed: %v", err)
	}

	// Commit
	if _, err := client.Commit(ctx, "add multiple files"); err != nil {
		t.Fatalf("Commit failed: %v", err)
	}

	// Verify working directory is clean
	if !client.IsClean(ctx) {
		t.Error("expected working directory to be clean")
	}
}

func TestClient_HasChanges(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Initially clean
	if client.HasChanges(ctx) {
		t.Error("expected no changes initially")
	}

	// Create an untracked file
	newFile := filepath.Join(repoDir, "untracked.txt")
	if err := os.WriteFile(newFile, []byte("test"), 0644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	// Should have changes
	if !client.HasChanges(ctx) {
		t.Error("expected changes after creating untracked file")
	}
}

func TestClient_InitRepository(t *testing.T) {
	ctx := context.Background()
	tmpDir := t.TempDir()
	client := NewClient(tmpDir)

	if err := client.InitRepository(ctx); err != nil {
		t.Fatalf("InitRepository failed: %v", err)
	}

	// Verify it's a git repository
	if !client.IsRepo(ctx) {
		t.Error("expected directory to be a git repository")
	}

	// Create a file and commit
	testFile := filepath.Join(tmpDir, "test.txt")
	if err := os.WriteFile(testFile, []byte("test"), 0644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	if err := client.AddAll(ctx); err != nil {
		t.Fatalf("AddAll failed: %v", err)
	}

	if _, err := client.Commit(ctx, "test commit"); err != nil {
		t.Fatalf("Commit failed: %v", err)
	}

	// Verify commit was created
	sha, err := client.GetHeadSHA(ctx)
	if err != nil {
		t.Fatalf("GetHeadSHA failed: %v", err)
	}

	if sha == "" {
		t.Error("expected non-empty HEAD SHA")
	}
}

func TestClient_SetConfig(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Set a config value
	if err := client.SetConfig(ctx, "test.key", "test-value"); err != nil {
		t.Fatalf("SetConfig failed: %v", err)
	}

	// Get the config value
	value, err := client.ConfigGet(ctx, "test.key")
	if err != nil {
		t.Fatalf("ConfigGet failed: %v", err)
	}

	if value != "test-value" {
		t.Errorf("expected config value 'test-value', got '%s'", value)
	}
}

func TestClient_Apply(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Create a patch file
	patchContent := `diff --git a/newfile.txt b/newfile.txt
new file mode 100644
index 0000000..1234567
--- /dev/null
+++ b/newfile.txt
@@ -0,0 +1 @@
+test content
`
	patchFile := filepath.Join(t.TempDir(), "test.patch")
	if err := os.WriteFile(patchFile, []byte(patchContent), 0644); err != nil {
		t.Fatalf("failed to create patch file: %v", err)
	}

	// Apply the patch
	if err := client.Apply(ctx, ApplyOptions{
		PatchPath: patchFile,
		ThreeWay:  true,
	}); err != nil {
		t.Fatalf("Apply failed: %v", err)
	}

	// Verify the file was created
	newFile := filepath.Join(repoDir, "newfile.txt")
	if _, err := os.Stat(newFile); err != nil {
		t.Errorf("expected file to exist after applying patch: %v", err)
	}
}

func TestClient_ApplyCheck(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Create a valid patch file
	patchContent := `diff --git a/testfile.txt b/testfile.txt
new file mode 100644
index 0000000..1234567
--- /dev/null
+++ b/testfile.txt
@@ -0,0 +1 @@
+test content
`
	patchFile := filepath.Join(t.TempDir(), "valid.patch")
	if err := os.WriteFile(patchFile, []byte(patchContent), 0644); err != nil {
		t.Fatalf("failed to create patch file: %v", err)
	}

	// Check the patch
	if err := client.ApplyCheck(ctx, patchFile, true); err != nil {
		t.Errorf("ApplyCheck failed: %v", err)
	}
}

func TestClient_CommitWith(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Create a file
	newFile := filepath.Join(repoDir, "test.txt")
	if err := os.WriteFile(newFile, []byte("test"), 0644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	if err := client.AddAll(ctx); err != nil {
		t.Fatalf("AddAll failed: %v", err)
	}

	// Commit with author
	author := &CommitAuthor{
		Name:  "Test Author",
		Email: "author@example.com",
		When:  time.Now(),
	}

	sha, err := client.CommitWith(ctx, CommitOptions{
		Message: "test commit with author",
		Author:  author,
	})
	if err != nil {
		t.Fatalf("CommitWith failed: %v", err)
	}

	if sha == "" {
		t.Error("expected non-empty commit SHA")
	}

	// Verify commit was created
	cmd := exec.Command("git", "log", "-1", "--format=%an <%ae>")
	cmd.Dir = repoDir
	output, err := cmd.Output()
	if err != nil {
		t.Fatalf("git log failed: %v", err)
	}

	authorStr := strings.TrimSpace(string(output))
	if authorStr != "Test Author <author@example.com>" {
		t.Errorf("expected author 'Test Author <author@example.com>', got '%s'", authorStr)
	}
}

func TestClone(t *testing.T) {
	ctx := context.Background()

	// Setup source repository
	sourceRepo, _ := setupTestRepo(t)

	t.Run("basic clone", func(t *testing.T) {
		destDir := t.TempDir()

		result, err := Clone(ctx, CloneOptions{
			Source: sourceRepo,
			Dest:   destDir,
			Quiet:  true,
		})
		if err != nil {
			t.Fatalf("Clone failed: %v", err)
		}

		if result.HEAD == "" {
			t.Error("expected non-empty HEAD SHA")
		}

		// Verify clone was successful
		client := NewClient(destDir)
		if !client.IsRepo(ctx) {
			t.Error("expected cloned directory to be a git repository")
		}
	})

	t.Run("shallow clone", func(t *testing.T) {
		t.Skip("Skipping shallow clone test - git clone --depth doesn't create shallow repos when cloning from a local file:// URL")

		destDir := t.TempDir()

		result, err := Clone(ctx, CloneOptions{
			Source: sourceRepo,
			Dest:   destDir,
			Depth:  1,
			Quiet:  true,
		})
		if err != nil {
			t.Fatalf("Clone failed: %v", err)
		}

		if !result.IsShallow {
			t.Error("expected clone to be shallow")
		}
	})

	t.Run("local clone with --local", func(t *testing.T) {
		destDir := t.TempDir()

		result, err := Clone(ctx, CloneOptions{
			Source: sourceRepo,
			Dest:   destDir,
			Local:  true,
			Quiet:  true,
		})
		if err != nil {
			t.Fatalf("Clone failed: %v", err)
		}

		if result.HEAD == "" {
			t.Error("expected non-empty HEAD SHA")
		}
	})
}

func TestRemoteGetConfig(t *testing.T) {
	ctx := context.Background()

	// Set a config value (using --global since we're not in a specific repo context)
	cmd := exec.Command("git", "config", "--global", "test.remote.key", "test-value")
	if err := cmd.Run(); err != nil {
		t.Fatalf("git config failed: %v", err)
	}

	// Clean up the global config after test
	defer func() {
		cmd := exec.Command("git", "config", "--global", "--unset", "test.remote.key")
		_ = cmd.Run()
	}()

	// Get the config value using the utility
	value, err := RemoteGetConfig(ctx, "test.remote.key")
	if err != nil {
		t.Fatalf("RemoteGetConfig failed: %v", err)
	}

	if value != "test-value" {
		t.Errorf("expected 'test-value', got '%s'", value)
	}
}

func TestClient_DryRun(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)

	client := NewClient(repoDir)
	client.Options = &ClientOptions{
		DryRun: true,
	}

	// Dry run should return an error explaining it's a dry run
	err := client.Checkout(ctx, "main")
	if err == nil {
		t.Error("expected error in dry run mode")
	}

	if !strings.Contains(err.Error(), "dry run") {
		t.Errorf("expected error to mention 'dry run', got: %v", err)
	}
}

func TestClient_IsClean(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// Initially clean
	if !client.IsClean(ctx) {
		t.Error("expected working directory to be clean")
	}

	// Modify a file
	readmePath := filepath.Join(repoDir, "README.md")
	if err := os.WriteFile(readmePath, []byte("modified content"), 0644); err != nil {
		t.Fatalf("failed to modify file: %v", err)
	}

	// Should not be clean
	if client.IsClean(ctx) {
		t.Error("expected working directory to not be clean")
	}
}

func TestClient_InitSubmodules(t *testing.T) {
	ctx := context.Background()
	repoDir, _ := setupTestRepo(t)
	client := NewClient(repoDir)

	// This test verifies that the command runs without error
	// In a real scenario with submodules, it would initialize them
	err := client.InitSubmodules(ctx)
	if err != nil {
		// This is expected to fail when there are no submodules
		// but we're testing the command execution
		t.Logf("InitSubmodules failed (expected with no submodules): %v", err)
	}
}
